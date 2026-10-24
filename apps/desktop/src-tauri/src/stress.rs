use ipc_error::{IpcError, IpcResult};

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use stress_engine::Orchestrator;
use tauri::{AppHandle, Emitter, Manager}; // Tauri 2 emit/state 需要对应 trait
use tokio::sync::broadcast;

use super::*;

/// 校验场景 TOML 并返回摘要，不启动压测。
#[tauri::command]
pub(crate) async fn validate_scenario(toml: String) -> IpcResult<ScenarioSummary> {
    let sc = scenario::LinearScenario::from_toml_str(&toml)?;
    // 生成一台设备可触发所有语义校验,避免“语法正确、启动才失败”。
    Orchestrator::new(&sc, 1)?;
    Ok(ScenarioSummary {
        name: sc.device_info.device_name.clone(),
        device_count: 0, // 由前端传 count 参数
        server_host: sc.server_host.clone(),
        server_port: sc.server_port,
    })
}

/// 压测产物目录：优先 app 数据目录，回退系统临时目录。
///
/// 该目录由应用按 [`artifacts::RetentionPolicy`] 自动清理；IPC 返回路径只是当前
/// 托管产物的位置，不承诺永久保存。
pub(crate) fn reports_dir(app: &AppHandle) -> std::path::PathBuf {
    let base = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("uvp-reports")
}

pub(crate) fn cleanup_artifacts(
    dir: &std::path::Path,
    preserve: std::collections::HashSet<std::path::PathBuf>,
) -> artifacts::CleanupHealth {
    artifacts::cleanup(dir, artifacts::RetentionPolicy::default(), &preserve)
}

/// 同时 preserve 活跃 `.inprogress` 及其可能在并发收尾中出现的 final 名称。
pub(crate) fn preserve_series_path(
    preserve: &mut std::collections::HashSet<std::path::PathBuf>,
    path: &std::path::Path,
) {
    preserve.insert(path.to_path_buf());
    if let Some(name) = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".inprogress"))
    {
        preserve.insert(path.with_file_name(name));
    }
}

/// 报告中保留可复现场景。
///
/// 脱敏由类型保证:`LinearScenario.password` 是 `common::Secret` 且带
/// `serialize_with = serialize_redacted`,**序列化时恒为占位符**。
/// 之前这里要先把字段改成 `"***"` 再序列化 —— 那依赖每条导出路径的作者都记得,
/// 现在漏了也不会泄露。
pub(crate) fn serialize_redacted_scenario(
    scenario: &scenario::LinearScenario,
) -> IpcResult<String> {
    toml::to_string(scenario)
        .map_err(|error| IpcError::config(format!("脱敏场景序列化失败: {error}")))
}

/// 启动压测。`toml` 为场景内容,`count` 为设备数。
/// `profile_id` 用于从 OS 钥匙串取 SIP 密码——前端不再把密码拼进 TOML。
/// 成功后每秒向前端发送带 `run_id` 的 `metrics_tick` 事件。
#[tauri::command]
pub(crate) async fn start_stress(
    toml: String,
    count: usize,
    profile_id: String,
    position_interval: u64,
    alarm_interval: u64,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> IpcResult<String> {
    if position_interval > 86_400 || alarm_interval > 86_400 {
        return Err("主动上报间隔不能超过 86400 秒".into());
    }

    let mut sc = scenario::LinearScenario::from_toml_str(&toml)?;
    sc.password = common::Secret::new(secrets::get(&profile_id)?);
    let redacted_scenario = serialize_redacted_scenario(&sc)?;
    let orch = Orchestrator::new(&sc, count)?;

    let orch = Arc::new(orch);
    let metrics = Arc::clone(&orch.metrics);

    // 只在所有关联状态锁都能同步取得时提交本轮；任何等待都发生在未持锁时。
    // 在锁定槽位前绝不创建 SeriesWriter，确保启动被拒绝时不残留空 .inprogress。
    let mut stress_guard = loop {
        let guard = state.stress.lock().await;
        if guard.is_some() {
            return Err(IpcError::new(
                "stress.already_running",
                "已有压测正在运行或停止中，请等待任务完全退出",
            )
            .with_retryable(true)
            .with_action("请等待当前压测完全退出后重试"));
        }
        let (Ok(mut metrics_guard), Ok(mut last_metrics), Ok(mut last_series), Ok(mut scenario)) = (
            state.metrics.try_lock(),
            state.last_metrics.try_lock(),
            state.last_series.try_lock(),
            state.last_scenario.try_lock(),
        ) else {
            drop(guard);
            tokio::task::yield_now().await;
            continue;
        };
        *metrics_guard = Some(Arc::clone(&metrics));
        *last_metrics = None;
        *last_series = None;
        *scenario = Some((redacted_scenario, count));
        drop((metrics_guard, last_metrics, last_series, scenario));
        break guard;
    };

    let run_id = state.next_stress_id.fetch_add(1, Ordering::Relaxed);
    state.last_run_id.store(run_id, Ordering::Relaxed);
    let started_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let reports_dir = reports_dir(&app);
    // 此时生命周期锁确认无活跃 run；清理失败只降级并告警，绝不阻止压测。
    let cleanup_dir = reports_dir.clone();
    match tokio::task::spawn_blocking(move || {
        cleanup_artifacts(&cleanup_dir, std::collections::HashSet::new())
    })
    .await
    {
        Ok(health) if !health.healthy => {
            tracing::warn!(?health, "压测启动前产物清理未完全健康");
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "压测启动前产物清理任务异常"),
    }
    let series_writer = match series::SeriesWriter::new(&reports_dir, run_id) {
        Ok(writer) => Some(Arc::new(writer)),
        Err(error) => {
            tracing::warn!(%error, "创建时序文件失败，本轮压测将降级为无时序");
            None
        }
    };
    let series_path = series_writer
        .as_ref()
        .map(|writer| writer.inprogress_path().to_path_buf());

    let (tx, _) = broadcast::channel::<()>(1);
    let run_stop = tx.subscribe();
    let tick_stop = tx.subscribe();
    let stopping_flag = Arc::new(AtomicBool::new(false));
    let (completion_tx, completion_rx) = tokio::sync::watch::channel(false);

    // 真正的 worker JoinHandle 只归 supervisor 所有；状态槽只保存 AbortHandle 与
    // 可克隆 completion，避免 worker 自己 join 或 graceful stop detach 任务。
    let orch_for_worker = Arc::clone(&orch);
    let tx_for_worker = tx.clone();
    let stopping_for_worker = Arc::clone(&stopping_flag);
    let worker_task = tokio::spawn(async move {
        let result = orch_for_worker
            .run_with_stop_receiver(tx_for_worker, run_stop, position_interval, alarm_interval)
            .await;
        // 这是运行结果的线性化点：不经过任何 await，立即固定停止状态，
        // 防止自然完成后才到达的 stop 把 completed 误标成 stopped。
        let was_stopping = stopping_for_worker.load(Ordering::Acquire);
        (result, was_stopping)
    });
    let worker_abort = worker_task.abort_handle();

    *stress_guard = Some(StressHandle {
        run_id,
        stop_tx: tx.clone(),
        stopping: false,
        stopping_flag,
        worker_abort,
        completion: completion_rx,
        device_count: count,
        series_path,
    });

    let _ = app.emit(
        "stress_state",
        serde_json::json!({
            "run_id": run_id,
            "running": true,
            "stopping": false,
            "device_count": count,
        }),
    );
    drop(stress_guard);

    let app_for_tick = app.clone();
    let orch_for_tick = Arc::clone(&orch);
    let writer_for_tick = series_writer.clone();
    let tick_task = tokio::spawn(async move {
        let mut tick_stop = tick_stop;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = tick_stop.recv() => break,
                _ = interval.tick() => {
                    let snapshot = orch_for_tick.snapshot();
                    if let Some(writer) = writer_for_tick.clone() {
                        let sample = snapshot.clone();
                        match tokio::task::spawn_blocking(move || writer.append(&sample)).await {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => tracing::warn!(%error, "时序指标写入失败"),
                            Err(error) => tracing::warn!(%error, "时序指标写任务异常"),
                        }
                    }
                    let _ = app_for_tick.emit(
                        "metrics_tick",
                        serde_json::json!({
                            "run_id": run_id,
                            "metrics": snapshot,
                        }),
                    );
                }
            }
        }
    });

    let app_for_run = app.clone();
    let orch_for_run = Arc::clone(&orch);
    let tx_for_run = tx.clone();
    tokio::spawn(async move {
        // supervisor 必须 await 真正 worker；panic/abort 也转换为本轮失败并继续状态清理。
        let (result, was_stopping): (IpcResult<()>, bool) = match worker_task.await {
            Ok((result, was_stopping)) => (result.map_err(IpcError::from), was_stopping),
            Err(error) => {
                let was_stopping = app_for_run
                    .state::<AppState>()
                    .stress
                    .lock()
                    .await
                    .as_ref()
                    .is_some_and(|handle| {
                        handle.run_id == run_id && handle.stopping_flag.load(Ordering::Acquire)
                    });
                if error.is_cancelled() && was_stopping {
                    tracing::warn!(run_id, "压测 worker 在主动停止超时后被取消");
                    (Ok(()), true)
                } else {
                    (
                        Err(IpcError::internal(format!("压测 worker 异常结束: {error}"))),
                        was_stopping,
                    )
                }
            }
        };
        let _ = tx_for_run.send(());
        if let Err(error) = tick_task.await {
            tracing::warn!(%error, run_id, "指标 tick 任务异常结束");
        }

        let outcome = if result.is_err() {
            series::RunOutcome::Failed
        } else if was_stopping {
            series::RunOutcome::Stopped
        } else {
            series::RunOutcome::Completed
        };
        let final_metrics = orch_for_run.snapshot();

        let (finalized_series, incomplete_series) = if let Some(writer) = series_writer.as_ref() {
            let final_sample = final_metrics.clone();
            let inprogress_path = writer.inprogress_path().to_path_buf();
            let writer_for_finalize = Arc::clone(writer);
            match tokio::task::spawn_blocking(move || {
                if let Err(error) = writer_for_finalize.append(&final_sample) {
                    tracing::warn!(%error, "最终时序样本写入失败");
                }
                writer_for_finalize.finalize(outcome)
            })
            .await
            {
                Ok(Ok(finalized)) => (Some(finalized), None),
                Ok(Err(error)) => {
                    tracing::warn!(%error, run_id, "时序收尾失败，保留 inprogress 文件");
                    (None, Some(inprogress_path))
                }
                Err(error) => {
                    tracing::warn!(%error, run_id, "时序收尾任务异常，保留 inprogress 文件");
                    (None, Some(inprogress_path))
                }
            }
        } else {
            (None, None)
        };

        let mut preserve = std::collections::HashSet::new();
        if let Some(finalized) = &finalized_series {
            preserve.insert(finalized.path.clone());
            tracing::info!(
                path = %finalized.path.display(),
                sample_count = finalized.sample_count,
                degraded = finalized.degraded,
                outcome = finalized.outcome.as_str(),
                "压测时序已完整发布"
            );
        }
        // 历史库(尽力而为):JSONL/报告仍是事实源,这里把终结快照与秒级样本写进
        // 紧凑索引,供「历史运行对比」跨运行查询。失败只告警,不影响压测结论。
        if let Some(finalized) = &finalized_series {
            let read = series::read_series(&finalized.path);
            let samples = samples_for_history(&read);
            let record = crate::reports_db::RunSummaryInput {
                outcome: outcome.as_str(),
                started_at_ms,
                ended_at_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(started_at_ms),
                device_count: count as i64,
                profile_id: Some(profile_id.clone()),
                register_succeeded: final_metrics.register_succeeded as i64,
                device_success_rate: final_metrics.device_success_rate,
                register_latency_p95_ms: final_metrics.register_latency.p95_ms as i64,
                devices_online_peak: final_metrics.devices_online_peak as i64,
                fail_timeout: final_metrics.fail_timeout as i64,
                fail_rejected_auth: final_metrics.fail_rejected_auth as i64,
                fail_rejected_overload: final_metrics.fail_rejected_overload as i64,
                fail_local: final_metrics.fail_local as i64,
                ramp_throttled: final_metrics.ramp_throttled as i64,
                results_trustworthy: final_metrics.results_trustworthy,
                summary_json: serde_json::to_string(&final_metrics).unwrap_or_default(),
                report_path: None, // export_report 成功后回填(见 set_report_path)
            };
            let db_path = history_db_path(&reports_dir);
            let history_run_id = run_id;
            tokio::task::spawn_blocking(move || {
                if let Err(error) =
                    crate::reports_db::record_run(&db_path, history_run_id, &record, &samples)
                {
                    tracing::warn!(%error, run_id = history_run_id, "压测历史落库失败(不影响报告与结论)");
                }
            });
        }
        if let Some(path) = &incomplete_series {
            preserve.insert(path.clone());
        }
        let cleanup_dir = reports_dir.clone();
        let cleanup_health =
            tokio::task::spawn_blocking(move || cleanup_artifacts(&cleanup_dir, preserve)).await;
        match cleanup_health {
            Ok(health) if !health.healthy => {
                tracing::warn!(?health, "压测收尾后产物清理未完全健康");
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "压测收尾后产物清理任务异常"),
        }

        let payload = match result {
            Ok(()) => serde_json::json!({
                "run_id": run_id,
                "running": false,
                "stopping": false,
                "reason": outcome.as_str(),
                "device_count": count,
                "metrics": final_metrics,
            }),
            Err(error) => {
                tracing::error!(?error, run_id, "压测任务异常结束");
                serde_json::json!({
                    "run_id": run_id,
                    "running": false,
                    "stopping": false,
                    "reason": "failed",
                    "error": error,
                    "device_count": count,
                    "metrics": final_metrics,
                })
            }
        };

        let retained_series_path = finalized_series
            .as_ref()
            .map(|finalized| finalized.path.clone())
            .or_else(|| incomplete_series.clone());
        let state = app_for_run.state::<AppState>();
        loop {
            let mut current = state.stress.lock().await;
            if !current
                .as_ref()
                .is_some_and(|handle| handle.run_id == run_id)
            {
                drop(current);
                drop(series_writer);
                let _ = completion_tx.send(true);
                return;
            }
            let (Ok(mut last_metrics), Ok(mut last_series), Ok(mut metrics)) = (
                state.last_metrics.try_lock(),
                state.last_series.try_lock(),
                state.metrics.try_lock(),
            ) else {
                drop(current);
                tokio::task::yield_now().await;
                continue;
            };
            *last_metrics = Some(final_metrics.clone());
            if let Some(path) = retained_series_path.clone() {
                *last_series = Some(path);
            }
            metrics.take();
            // 最终事件与槽位释放在同一个无 await 临界区内线性化。
            let _ = app_for_run.emit("stress_state", payload.clone());
            current.take();
            drop((last_metrics, last_series, metrics, current));
            break;
        }
        // SeriesWriter 的 RAII 令牌一直存活到 cleanup 与 last_series 状态交接之后。
        drop(series_writer);
        // completion=true 作为 supervisor 任务最后一个无 await 动作发送，
        // 确保当 receiver 观察到完成信号时，所有 supervisor 资源与文件 guard 已完全释放，
        // 避免退出时的 runtime teardown 与 supervisor 尾部资源销毁发生竞态。
        let _ = completion_tx.send(true);
    });

    Ok(format!("已启动 {count} 台设备压测"))
}

/// 停止当前压测。只发送信号并进入 stopping 状态，运行槽位由后台任务退出时释放。
#[tauri::command]
pub(crate) async fn stop_stress(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> IpcResult<String> {
    let mut stress_guard = state.stress.lock().await;
    match stress_guard.as_mut() {
        Some(handle) if handle.stopping => Ok("压测正在停止，请稍候".into()),
        Some(handle) => {
            handle.stopping = true;
            handle.stopping_flag.store(true, Ordering::Release);
            let _ = handle.stop_tx.send(());
            // 在持有生命周期锁时发布，保证后台最终事件一定排在 stopping 事件之后。
            let _ = app.emit(
                "stress_state",
                serde_json::json!({
                    "run_id": handle.run_id,
                    "running": true,
                    "stopping": true,
                    "reason": "stopping",
                    "device_count": handle.device_count,
                }),
            );
            Ok("压测停止信号已发送，正在等待设备任务退出".into())
        }
        None => Err(IpcError::new("stress.not_running", "没有正在运行的压测")),
    }
}

/// 查询当前指标快照（供按需轮询；实时更新通过 metrics_tick 事件）。
#[tauri::command]
pub(crate) async fn get_metrics(state: tauri::State<'_, AppState>) -> IpcResult<String> {
    match &*state.metrics.lock().await {
        Some(m) => Ok(serde_json::to_string(&m.snapshot())
            .map_err(|error| IpcError::internal(format!("指标序列化失败: {error}")))?),
        None => Err(IpcError::new("stress.not_running", "无正在运行的压测")),
    }
}

/// 查询压测状态，供 UI 页面切换或 IPC 返回后与引擎真实状态对账。
#[tauri::command]
pub(crate) async fn get_stress_status(
    state: tauri::State<'_, AppState>,
) -> IpcResult<serde_json::Value> {
    // 生命周期与关联字段在无 await 临界区取同代际快照。
    let (running, stopping, run_id, device_count, has_report) = loop {
        let stress_guard = state.stress.lock().await;
        let (Ok(last_scenario), Ok(metrics), Ok(last_metrics)) = (
            state.last_scenario.try_lock(),
            state.metrics.try_lock(),
            state.last_metrics.try_lock(),
        ) else {
            drop(stress_guard);
            tokio::task::yield_now().await;
            continue;
        };
        let (running, stopping, run_id, device_count) = match stress_guard.as_ref() {
            Some(handle) => (
                true,
                handle.stopping,
                Some(handle.run_id),
                handle.device_count,
            ),
            None => (
                false,
                false,
                None,
                last_scenario.as_ref().map(|(_, count)| *count).unwrap_or(0),
            ),
        };
        break (
            running,
            stopping,
            run_id,
            device_count,
            metrics.is_some() || last_metrics.is_some(),
        );
    };
    Ok(serde_json::json!({
        "running": running,
        "stopping": stopping,
        "run_id": run_id,
        "device_count": device_count,
        "has_report": has_report,
    }))
}

pub(crate) struct ReportV1Input {
    pub(crate) policy: artifacts::RetentionPolicy,
    pub(crate) retention_health: artifacts::CleanupHealth,
    pub(crate) environment: diagnostics::Environment,
    pub(crate) scenario: Option<(String, usize)>,
    pub(crate) metrics: stress_engine::MetricsSnapshot,
    pub(crate) series: series::SeriesRead,
    pub(crate) series_file: Option<String>,
    pub(crate) generated_at_epoch_secs: u64,
}

/// 构造稳定的 schema v1 payload。JSONL envelope 只用于 reader 校验，series 固定为裸 metrics。
pub(crate) fn build_report_v1(input: ReportV1Input) -> serde_json::Value {
    let ReportV1Input {
        policy,
        retention_health,
        environment,
        scenario,
        metrics,
        series,
        series_file,
        generated_at_epoch_secs,
    } = input;
    serde_json::json!({
        "schema_version": 1,
        "producer": "uvp-gb28181-sim-desktop",
        "app_version": env!("CARGO_PKG_VERSION"),
        "managed_artifact": true,
        "generated_at_epoch_secs": generated_at_epoch_secs,
        "retention": policy,
        "retention_policy": policy,
        "artifact_health": retention_health,
        "environment": environment,
        "scenario_toml_redacted": scenario.as_ref().map(|(toml, _)| toml),
        "device_count": scenario.as_ref().map(|(_, count)| count),
        "metrics": metrics,
        "series_sample_count": series.sample_count,
        "sample_count": series.sample_count,
        "series_complete": series.complete,
        "series_outcome": series.outcome,
        "outcome": series.outcome,
        "series_degraded": series.degraded,
        "series_torn_tail": series.torn_tail,
        "series_corrupt_lines": series.corrupt_lines,
        "series": series.samples,
        "series_file": series_file,
    })
}

/// 导出压测报告（FR-28），返回应用托管产物路径。
#[tauri::command]
pub(crate) async fn export_report(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> IpcResult<String> {
    // 同一 run 的关联状态用 try_lock 在无 await 临界区复制，竞争时释放后重试。
    let (snap, scenario, series_path, run_id) = loop {
        let stress_guard = state.stress.lock().await;
        let (Ok(metrics), Ok(last_metrics), Ok(scenario), Ok(last_series)) = (
            state.metrics.try_lock(),
            state.last_metrics.try_lock(),
            state.last_scenario.try_lock(),
            state.last_series.try_lock(),
        ) else {
            drop(stress_guard);
            tokio::task::yield_now().await;
            continue;
        };
        let snap = match &*metrics {
            Some(metrics) => metrics.snapshot(),
            None => match &*last_metrics {
                Some(snapshot) => snapshot.clone(),
                None => return Err("无压测数据可导出(尚未运行过压测)".into()),
            },
        };
        let series_path = stress_guard
            .as_ref()
            .and_then(|handle| handle.series_path.clone())
            .or_else(|| last_series.clone());
        let run_id = state.last_run_id.load(Ordering::Relaxed);
        break (snap, scenario.clone(), series_path, run_id);
    };

    let dir = reports_dir(&app);
    let db_path = history_db_path(&dir);
    let path = tokio::task::spawn_blocking(move || -> IpcResult<std::path::PathBuf> {
        let policy = artifacts::RetentionPolicy::default();
        let mut preserve = std::collections::HashSet::new();
        if let Some(path) = &series_path {
            preserve_series_path(&mut preserve, path);
        }
        let retention_health = cleanup_artifacts(&dir, preserve.clone());
        let series = series_path
            .as_deref()
            .map(series::read_series)
            .unwrap_or_default();
        let generated_at_epoch_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let series_file = series.source_file.clone().or_else(|| {
            series_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
        });
        let report = build_report_v1(ReportV1Input {
            policy,
            retention_health,
            environment: diagnostics::Environment::collect(),
            scenario,
            metrics: snap,
            series,
            series_file,
            generated_at_epoch_secs,
        });
        let content = serde_json::to_vec_pretty(&report).map_err(|error| {
            IpcError::new("report.export_failed", format!("报告序列化失败: {error}"))
        })?;
        let (nanos, counter) = artifacts::unique_timestamp_counter();
        let path = dir.join(format!("report-{nanos}-{counter}.json"));
        let published = artifacts::atomic_write_new(&path, &content).map_err(|error| {
            IpcError::new("report.export_failed", format!("原子写报告失败: {error}"))
        })?;
        if let Some(warning) = published.durability_warning() {
            tracing::warn!(warning, path = %published.path().display(), "报告持久性降级");
        }
        preserve.insert(path.clone());
        let health = cleanup_artifacts(&dir, preserve);
        if !health.healthy {
            tracing::warn!(?health, "报告导出后产物清理未完全健康");
        }
        // published guard 保留到新报告进入 preserve 并完成 cleanup 交接。
        drop(published);
        // 历史库回填报告路径(尽力而为):列表里的「查看报告」直接定位文件。
        if run_id > 0 {
            if let Err(error) =
                crate::reports_db::set_report_path(&db_path, run_id, &path.to_string_lossy())
            {
                tracing::warn!(%error, run_id, "压测历史回填报告路径失败(不影响导出)");
            }
        }
        Ok(path)
    })
    .await
    .map_err(|error| IpcError::new("report.export_failed", format!("导出任务异常: {error}")))??;

    Ok(path.to_string_lossy().to_string())
}

/// 历史库文件:与 uvp-reports 同级。uvp-reports 的清理只识别本应用的严格
/// 文件名(见 data-model.md §1),不会碰这个 .db。
pub(crate) fn history_db_path(reports_dir: &std::path::Path) -> std::path::PathBuf {
    reports_dir
        .parent()
        .unwrap_or(reports_dir)
        .join("uvp-stress-history.db")
}

/// 从已发布 JSONL 的样本数组提取历史库秒级样本。
///
/// tick 是 1s 一拍,样本内的 `metrics` 不带时间戳 —— 用序号 × 1s 作为运行相对
/// 时间,跨运行对比只看形状与相对拐点,不要求绝对墙钟。
fn samples_for_history(read: &series::SeriesRead) -> Vec<crate::reports_db::SampleInput> {
    read.samples
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let int = |k: &str| v.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
            crate::reports_db::SampleInput {
                ts_ms: i as i64 * 1_000,
                register_attempted: int("register_attempted"),
                register_succeeded: int("register_succeeded"),
                register_latency_p95: v
                    .get("register_latency")
                    .and_then(|l| l.get("p95_ms"))
                    .and_then(|x| x.as_i64())
                    .unwrap_or(0),
                heartbeat_failed: int("heartbeat_failed"),
                devices_online: int("devices_online"),
                device_success_rate: v
                    .get("device_success_rate")
                    .and_then(|x| x.as_f64())
                    .unwrap_or(0.0),
                fail_rejected_overload: int("fail_rejected_overload"),
                active_streams: int("active_streams"),
            }
        })
        .collect()
}

/// 查询最近的压测历史运行(新→旧),供工作台「历史运行」列表与跨运行对比。
#[tauri::command]
pub(crate) async fn list_stress_runs(
    app: AppHandle,
    limit: Option<u32>,
) -> IpcResult<Vec<crate::reports_db::StressRunSummaryDto>> {
    let db = history_db_path(&reports_dir(&app));
    tokio::task::spawn_blocking(move || {
        crate::reports_db::list_runs(&db, limit.unwrap_or(20).clamp(1, 100))
            .map_err(|error| IpcError::new("history.query_failed", error))
    })
    .await
    .map_err(|error| IpcError::internal(format!("历史查询任务异常: {error}")))?
}

/// 查询某次历史运行的完整秒级样本(旧→新),供对比曲线。
#[tauri::command]
pub(crate) async fn get_stress_run_series(
    app: AppHandle,
    run_id: u64,
) -> IpcResult<Vec<crate::reports_db::StressSampleDto>> {
    let db = history_db_path(&reports_dir(&app));
    tokio::task::spawn_blocking(move || {
        crate::reports_db::run_series(&db, run_id)
            .map_err(|error| IpcError::new("history.query_failed", error))
    })
    .await
    .map_err(|error| IpcError::internal(format!("历史查询任务异常: {error}")))?
}

// ── 单设备联调命令 ────────────────────────────────────────

/// 等待 supervisor 完成 series/状态交接。返回 false 表示 sender 未发布完成就异常关闭。
pub(crate) async fn wait_stress_completion(
    completion: &mut tokio::sync::watch::Receiver<bool>,
) -> bool {
    loop {
        if *completion.borrow() {
            return true;
        }
        if completion.changed().await.is_err() {
            return false;
        }
    }
}

/// supervisor 异常消失时按 run_id 兜底清理槽位和实时 metrics；等待不持有任何锁。
pub(crate) async fn clear_abandoned_stress(state: &AppState, run_id: u64) {
    loop {
        let mut stress = state.stress.lock().await;
        if !stress
            .as_ref()
            .is_some_and(|handle| handle.run_id == run_id)
        {
            return;
        }
        let Ok(mut metrics) = state.metrics.try_lock() else {
            drop(stress);
            tokio::task::yield_now().await;
            continue;
        };
        metrics.take();
        stress.take();
        return;
    }
}

/// 停压测并等到 supervisor 收尾完成(带超时)。返回是否停了一次压测。
pub(crate) async fn stop_stress_graceful(state: &AppState) -> bool {
    let (run_id, stop_tx, worker_abort, mut completion) = {
        let mut guard = state.stress.lock().await;
        let Some(handle) = guard.as_mut() else {
            return false;
        };
        handle.stopping = true;
        handle.stopping_flag.store(true, Ordering::Release);
        (
            handle.run_id,
            handle.stop_tx.clone(),
            handle.worker_abort.clone(),
            handle.completion.clone(),
        )
    };
    let _ = stop_tx.send(());

    match tokio::time::timeout(
        GRACEFUL_STOP_TIMEOUT,
        wait_stress_completion(&mut completion),
    )
    .await
    {
        Ok(true) => {
            tracing::info!("压测任务及 supervisor 收尾已全部退出");
        }
        Ok(false) => {
            tracing::error!(run_id, "压测 supervisor 未发布 completion 就异常结束");
            worker_abort.abort();
            clear_abandoned_stress(state, run_id).await;
        }
        Err(_) => {
            tracing::warn!(
                secs = GRACEFUL_STOP_TIMEOUT.as_secs(),
                "压测未在超时内收敛，正在强制取消 worker"
            );
            worker_abort.abort();
            match tokio::time::timeout(
                std::time::Duration::from_secs(1),
                wait_stress_completion(&mut completion),
            )
            .await
            {
                Ok(true) => {}
                Ok(false) => {
                    tracing::error!(run_id, "强制取消后 supervisor 异常关闭 completion");
                    clear_abandoned_stress(state, run_id).await;
                }
                Err(_) => {
                    tracing::error!(
                        run_id,
                        "强制取消 worker 后 supervisor 仍未在 1 秒内完成收尾"
                    );
                }
            }
        }
    }
    true
}

#[derive(ts_rs::TS)]
#[ts(export, export_to = "../../src/types/generated/")]
#[derive(serde::Serialize)]
pub(crate) struct ScenarioSummary {
    name: String,
    device_count: usize,
    server_host: String,
    server_port: u16,
}

// ── 系统信息:日志 ────────────────────────────────────────
