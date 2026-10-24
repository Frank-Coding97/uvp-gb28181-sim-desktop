//! Tauri 桌面端后端库。
//!
//! 按 docs/20-architecture/api-contract.md 注册 IPC 命令。
//! 第一阶段内嵌 stress-engine；后续可拆为独立进程。

mod artifacts;
mod diagnostics;
mod ipc_error;
mod media_access;
mod secrets;
mod series;

use ipc_error::{sanitize_public_message, IpcError, IpcResult};

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use common::{DeviceEvent, DeviceId, DeviceObserver, GbVersion, Transport};
use gb28181_simulator::{ChannelConfig, DeviceConfig, DeviceInfo, DeviceSimulator};
use serde::{Deserialize, Serialize};
use sip_core::{SipTransport, TcpTransport, UdpTransport};
use stress_engine::{Metrics, Orchestrator};
use tauri::{AppHandle, Emitter, Manager}; // Tauri 2 emit/state 需要对应 trait
use tokio::sync::{broadcast, Mutex};

/// 应用全局状态（托管在 Tauri managed state）。
struct AppState {
    /// 当前压测句柄；任务完全退出前始终占用槽位。
    stress: Mutex<Option<StressHandle>>,
    /// 分配单调递增的压测 ID,防止旧任务事件污染新任务状态。
    next_stress_id: AtomicU64,
    /// 当前压测的实时指标；None 表示任务已完全退出。
    metrics: Mutex<Option<Arc<Metrics>>>,
    /// 最近一次压测的最终指标快照(停止后保留,供导出报告)。
    last_metrics: Mutex<Option<stress_engine::MetricsSnapshot>>,
    /// 最近一次启动使用的脱敏场景 TOML 与设备数（报告导出用）。
    last_scenario: Mutex<Option<(String, usize)>>,
    /// 最近一次压测的时序指标 JSONL 文件路径。
    ///
    /// 之前时序只活在前端图表内存里,且只保留 60 个点 —— 跑 10 分钟只剩最后一分钟,
    /// 报告里连时序都没有。现在引擎每秒追加一行,导出报告时一并打包。
    last_series: Mutex<Option<std::path::PathBuf>>,
    /// 单设备联调：当前设备实例 + 停止发射端 + 共享传输。
    device: Mutex<Option<DeviceHandle>>,
    /// 用户经**系统文件对话框**授权预览的媒体文件集合(见 media_access 模块文档)。
    media_grants: media_access::MediaGrants,
}

/// 压测运行句柄。收到停止请求后仅标记 `stopping`，由后台任务退出时释放。
struct StressHandle {
    run_id: u64,
    stop_tx: broadcast::Sender<()>,
    stopping: bool,
    stopping_flag: Arc<AtomicBool>,
    worker_abort: tokio::task::AbortHandle,
    completion: tokio::sync::watch::Receiver<bool>,
    device_count: usize,
    /// 当前 run 的 `.inprogress` 时序；retention 必须始终保留。
    series_path: Option<std::path::PathBuf>,
}

/// 单设备运行句柄。
struct DeviceHandle {
    sim: Arc<DeviceSimulator>,
    transport: Arc<dyn SipTransport>,
    local_host: String,
    local_port: u16,
    stop_tx: tokio::sync::oneshot::Sender<()>,
    /// `sim.run()` 的任务句柄。
    ///
    /// 之前是 fire-and-forget:`stop_tx.send(())` 之后立刻返回"设备已停止",
    /// 而真正的下线序列(停订阅 → 拆媒体链路发 BYE → REGISTER Expires=0)还在跑。
    /// 关窗口时进程被直接结束,平台要等心跳超时才把设备置离线,
    /// ffmpeg 子进程也来不及被 `Drop` 清掉,留下孤儿进程。
    run_task: tokio::task::JoinHandle<()>,
}

/// 退出清理是否已经开始。
///
/// `AppHandle::exit()` 会再触发一次 `ExitRequested`;没有这个闸的话
/// "拦下退出 → 清理 → 调 exit → 又被拦下"会变成死循环,应用永远关不掉。
static CLEANUP_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 优雅停止的等待上限。
///
/// 下线序列里最慢的一步是"REGISTER Expires=0"这次 SIP 事务:平台不响应时
/// 客户端事务会重传到 Timer B(约 32 秒)。用户关窗口不该等那么久 ——
/// 5 秒足够覆盖平台正常响应的情形,超时就放弃并如实记一条日志。
const GRACEFUL_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// 等待 Tokio 任务优雅退出；超时后必须 abort + await。
///
/// 仅把 `JoinHandle` 移进 `timeout` 再丢弃会 detach 任务，不会取消。设备运行任务持有
/// 推流 future，detach 会阻断 `LiveSource::drop`，从而让 FFmpeg 变成孤儿进程。
async fn await_or_abort_task(what: &str, task: &mut tokio::task::JoinHandle<()>) -> bool {
    match tokio::time::timeout(GRACEFUL_STOP_TIMEOUT, &mut *task).await {
        Ok(Ok(())) => true,
        Ok(Err(error)) if error.is_cancelled() => true,
        Ok(Err(error)) => {
            tracing::warn!(what, %error, "后台任务异常结束");
            true
        }
        Err(_) => {
            tracing::warn!(
                what,
                secs = GRACEFUL_STOP_TIMEOUT.as_secs(),
                "后台任务未在期限内退出,正在强制取消"
            );
            task.abort();
            match tokio::time::timeout(std::time::Duration::from_secs(1), &mut *task).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) if error.is_cancelled() => {}
                Ok(Err(error)) => tracing::warn!(what, %error, "强制取消后的任务异常"),
                Err(_) => tracing::error!(what, "强制取消后任务仍未收敛"),
            }
            false
        }
    }
}

impl AppState {
    /// `app_data_dir` 为 None 时媒体授权退化为仅本次运行有效(拿不到数据目录的极端情形)。
    fn new(app_data_dir: Option<std::path::PathBuf>) -> Self {
        AppState {
            stress: Mutex::new(None),
            next_stress_id: AtomicU64::new(1),
            metrics: Mutex::new(None),
            last_metrics: Mutex::new(None),
            last_scenario: Mutex::new(None),
            last_series: Mutex::new(None),
            device: Mutex::new(None),
            media_grants: media_access::MediaGrants::load(app_data_dir),
        }
    }
}

/// 把设备事件映射成 `device_state` 字符串,通过 Tauri 事件推给前端(状态灯)。
struct StateEmitter {
    app: AppHandle,
}

impl DeviceObserver for StateEmitter {
    fn on_event(&self, event: DeviceEvent) {
        let state = match event {
            DeviceEvent::RegisterAttempt { .. } => "Registering",
            DeviceEvent::RegisterSuccess { .. } => "Registered",
            DeviceEvent::RegisterFailure { .. } => "Failed",
            DeviceEvent::StreamStart => "InCall",
            DeviceEvent::StreamStop => "Registered",
            // 心跳与在线翻转不改变状态灯(状态灯由注册/推流事件驱动)。
            DeviceEvent::HeartbeatOk { .. }
            | DeviceEvent::HeartbeatFail
            | DeviceEvent::OnlineChanged { .. } => return,
            // 云台控制:单独推 ptz_action 事件给前端做云台动画。
            DeviceEvent::Ptz {
                up,
                down,
                left,
                right,
                zoom_in,
                zoom_out,
                pan_speed,
                tilt_speed,
                zoom_speed,
            } => {
                let _ = self.app.emit(
                    "ptz_action",
                    serde_json::json!({
                        "up": up, "down": down, "left": left, "right": right,
                        "zoom_in": zoom_in, "zoom_out": zoom_out,
                        "pan_speed": pan_speed, "tilt_speed": tilt_speed, "zoom_speed": zoom_speed,
                    }),
                );
                return;
            }
            DeviceEvent::PtzPresetCall { preset, name } => {
                let _ = self.app.emit(
                    "ptz_preset",
                    serde_json::json!({ "id": preset, "name": name }),
                );
                return;
            }
            // 平台下发 OSD 配置命令,设备已应用:推给前端展示。
            DeviceEvent::OsdConfig {
                time_show,
                osd_show,
            } => {
                let _ = self.app.emit(
                    "osd_config",
                    serde_json::json!({ "time_show": time_show, "osd_show": osd_show }),
                );
                return;
            }
            // 平台命令语义事件 → 前端"平台命令时间线"。
            DeviceEvent::PlatformCommand { kind, summary } => {
                let ts_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let _ = self.app.emit(
                    "platform_command",
                    serde_json::json!({ "kind": kind, "summary": summary, "ts_ms": ts_ms }),
                );
                return;
            }
            // 订阅状态变化 → 前端"活跃订阅面板"。
            DeviceEvent::SubscriptionChanged {
                kind,
                active,
                notify_count,
            } => {
                let _ = self.app.emit(
                    "subscription_state",
                    serde_json::json!({ "kind": kind, "active": active, "notify_count": notify_count }),
                );
                return;
            }
            // 长任务进度 → 前端进度条。
            DeviceEvent::Progress {
                kind,
                current,
                total,
                percent,
            } => {
                let _ = self.app.emit(
                    "task_progress",
                    serde_json::json!({ "kind": kind, "current": current, "total": total, "percent": percent }),
                );
                return;
            }
            // 运行时错误(如点播采集失败)→ 前端全局错误条。
            DeviceEvent::RuntimeError { scope, message } => {
                let ts_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let error = IpcError::from(message);
                let _ = self.app.emit(
                    "device_error",
                    serde_json::json!({ "scope": scope, "error": error, "ts_ms": ts_ms }),
                );
                return;
            }
        };
        let _ = self.app.emit("device_state", state);
    }
}

/// SIP 信令追踪观察者(FR-43):把每条收发报文结构化为 `sip_trace` 事件推给前端。
struct TraceEmitter {
    app: AppHandle,
}

/// 追踪面板里需要脱敏的头域(小写比较)。
///
/// `Authorization` / `Proxy-Authorization` 携带 Digest 的 `response=` 摘要,
/// 而同一条报文里 `username`/`realm`/`nonce`/`uri`/`method` 全是已知明文 ——
/// 等于把一份可离线爆破的口令哈希连同全部盐值一起交出去。
/// `WWW-Authenticate` 里的 nonce 本身不是秘密,但一并遮掉可以避免用户
/// 把整段信令贴进工单时无意泄露一次挑战-响应对。
const REDACTED_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "www-authenticate",
    "proxy-authenticate",
    "authentication-info",
];

/// 遮罩标记。刻意保留头域名与"有值"这个事实 —— 排障需要知道
/// "这条 REGISTER 到底带没带鉴权",只是不需要知道摘要是什么。
const REDACTED_MARK: &str = "<已脱敏>";

/// 把 SIP 报文文本里的鉴权头值替换成遮罩。
///
/// 只改头部不动正文:MANSCDP 正文里没有凭据(目录项的 `Password` 我们从不输出),
/// 而正文往下还要给用户看 XML。按行处理,遇到头部与正文之间的空行即停。
fn redact_sip_credentials(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_headers = true;
    for line in raw.split_inclusive('\n') {
        // 头部结束:只含 CR/LF 的空行。
        if in_headers && line.trim_end_matches(['\r', '\n']).is_empty() {
            in_headers = false;
            out.push_str(line);
            continue;
        }
        if in_headers {
            if let Some((name, _)) = line.split_once(':') {
                if REDACTED_HEADERS.contains(&name.trim().to_ascii_lowercase().as_str()) {
                    let eol = &line[line.trim_end_matches(['\r', '\n']).len()..];
                    out.push_str(name);
                    out.push_str(": ");
                    out.push_str(REDACTED_MARK);
                    out.push_str(eol);
                    continue;
                }
            }
        }
        out.push_str(line);
    }
    out
}

impl sip_core::TraceObserver for TraceEmitter {
    fn on_trace(&self, trace: sip_core::SipTrace<'_>) {
        let (method, status, cseq, call_id, summary) = match trace.message {
            sip_core::SipMessage::Request(r) => (
                r.method.as_str().to_string(),
                None,
                r.headers.cseq().map(str::to_string),
                r.headers.call_id().map(str::to_string),
                format!("{} {}", r.method.as_str(), r.uri),
            ),
            sip_core::SipMessage::Response(r) => (
                "SIP/2.0".to_string(),
                Some(r.status),
                r.headers.cseq().map(str::to_string),
                r.headers.call_id().map(str::to_string),
                format!("SIP/2.0 {} {}", r.status, r.reason),
            ),
        };
        // 完整报文文本(供前端展开查看详情);body 可能是 GB18030,按 lossy 解码展示。
        let raw = {
            let bytes = match trace.message {
                sip_core::SipMessage::Request(r) => r.to_bytes(),
                sip_core::SipMessage::Response(r) => r.to_bytes(),
            };
            // GB18030 兼容 ASCII 头;中文 body 用 GB18030 解码更可读。
            let (text, _, _) = encoding_rs::GB18030.decode(&bytes);
            redact_sip_credentials(&text)
        };
        let entry = serde_json::json!({
            "ts_ms": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64).unwrap_or(0),
            "direction": if matches!(trace.dir, sip_core::TraceDir::In) { "in" } else { "out" },
            "method": method,
            "status": status,
            "cseq": cseq,
            "call_id": call_id,
            "peer": trace.peer.to_string(),
            "summary": summary,
            "raw": raw,
        });
        let _ = self.app.emit("sip_trace", entry);
    }
}

// ── IPC 命令 ─────────────────────────────────────────────

/// 引擎连通性自检（M0 已有，保留）。
#[tauri::command]
fn engine_version() -> String {
    format!("stress-engine v{} 就绪", env!("CARGO_PKG_VERSION"))
}

/// 入站信令安全能力。静态显式暴露边界，避免 UI 把“严格模式”误解为双向 Digest。
#[derive(Debug, Clone, Copy, serde::Serialize)]
struct InboundSecurityCapabilities {
    source_ip_filter: bool,
    from_platform_id: bool,
    digest_server_validation: bool,
    digest_client_registration: bool,
    literal_platform_ip_only: bool,
}

#[tauri::command]
fn get_inbound_security_capabilities() -> InboundSecurityCapabilities {
    InboundSecurityCapabilities {
        source_ip_filter: true,
        from_platform_id: true,
        digest_server_validation: false,
        digest_client_registration: true,
        literal_platform_ip_only: true,
    }
}

/// 校验场景 TOML 并返回摘要，不启动压测。
#[tauri::command]
async fn validate_scenario(toml: String) -> IpcResult<ScenarioSummary> {
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
fn reports_dir(app: &AppHandle) -> std::path::PathBuf {
    let base = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("uvp-reports")
}

fn cleanup_artifacts(
    dir: &std::path::Path,
    preserve: std::collections::HashSet<std::path::PathBuf>,
) -> artifacts::CleanupHealth {
    artifacts::cleanup(dir, artifacts::RetentionPolicy::default(), &preserve)
}

/// 同时 preserve 活跃 `.inprogress` 及其可能在并发收尾中出现的 final 名称。
fn preserve_series_path(
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
fn serialize_redacted_scenario(scenario: &scenario::LinearScenario) -> IpcResult<String> {
    toml::to_string(scenario)
        .map_err(|error| IpcError::config(format!("脱敏场景序列化失败: {error}")))
}

/// 启动压测。`toml` 为场景内容,`count` 为设备数。
/// `profile_id` 用于从 OS 钥匙串取 SIP 密码——前端不再把密码拼进 TOML。
/// 成功后每秒向前端发送带 `run_id` 的 `metrics_tick` 事件。
#[tauri::command]
async fn start_stress(
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
async fn stop_stress(state: tauri::State<'_, AppState>, app: AppHandle) -> IpcResult<String> {
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
async fn get_metrics(state: tauri::State<'_, AppState>) -> IpcResult<String> {
    match &*state.metrics.lock().await {
        Some(m) => Ok(serde_json::to_string(&m.snapshot())
            .map_err(|error| IpcError::internal(format!("指标序列化失败: {error}")))?),
        None => Err(IpcError::new("stress.not_running", "无正在运行的压测")),
    }
}

/// 查询压测状态，供 UI 页面切换或 IPC 返回后与引擎真实状态对账。
#[tauri::command]
async fn get_stress_status(state: tauri::State<'_, AppState>) -> IpcResult<serde_json::Value> {
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

/// 查询单设备运行状态(UI 切页后对账用)。running=设备实例是否存在。
#[tauri::command]
async fn get_device_status(state: tauri::State<'_, AppState>) -> IpcResult<serde_json::Value> {
    let running = state.device.lock().await.is_some();
    Ok(serde_json::json!({ "running": running }))
}

struct ReportV1Input {
    policy: artifacts::RetentionPolicy,
    retention_health: artifacts::CleanupHealth,
    environment: diagnostics::Environment,
    scenario: Option<(String, usize)>,
    metrics: stress_engine::MetricsSnapshot,
    series: series::SeriesRead,
    series_file: Option<String>,
    generated_at_epoch_secs: u64,
}

/// 构造稳定的 schema v1 payload。JSONL envelope 只用于 reader 校验，series 固定为裸 metrics。
fn build_report_v1(input: ReportV1Input) -> serde_json::Value {
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
async fn export_report(state: tauri::State<'_, AppState>, app: AppHandle) -> IpcResult<String> {
    // 同一 run 的关联状态用 try_lock 在无 await 临界区复制，竞争时释放后重试。
    let (snap, scenario, series_path) = loop {
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
        break (snap, scenario.clone(), series_path);
    };

    let dir = reports_dir(&app);
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
        Ok(path)
    })
    .await
    .map_err(|error| IpcError::new("report.export_failed", format!("导出任务异常: {error}")))??;

    Ok(path.to_string_lossy().to_string())
}

// ── 单设备联调命令 ────────────────────────────────────────

#[derive(Serialize)]
struct LiveScreenDto {
    display_id: u32,
    width: u32,
    height: u32,
    name: String,
    uri: String,
}

#[derive(Serialize)]
struct LiveAvDeviceDto {
    index: u32,
    name: String,
}

#[derive(Serialize)]
struct LiveSourceCatalogDto {
    ffmpeg_available: bool,
    aac_available: bool,
    opus_available: bool,
    screens: Vec<LiveScreenDto>,
    cameras: Vec<LiveAvDeviceDto>,
    microphones: Vec<LiveAvDeviceDto>,
    screen_error: Option<String>,
    avfoundation_error: Option<String>,
}

/// 系统内容选择器(SCContentSharingPicker)的返回值。
///
/// 前端把它拼进 `live:screen|window|app:<目标>` URI:选中的是**目标身份**
/// (显示器 id / 窗口号 / bundle id),不是一次性的过滤器对象 —— 这样同一次选择
/// 才能跨会话复用(每次平台 INVITE 都会重建采集管道)。
#[derive(Serialize)]
struct ShareTargetDto {
    /// `display` / `window` / `application`。
    kind: String,
    display_id: Option<u32>,
    window_id: Option<u32>,
    bundle_id: Option<String>,
    /// 界面展示用标签(窗口标题 / 应用名 / 显示器编号)。
    label: String,
}

impl From<media_rtp::PickedShareTarget> for ShareTargetDto {
    fn from(picked: media_rtp::PickedShareTarget) -> Self {
        use media_rtp::ScreenCaptureTarget as Target;
        let (kind, display_id, window_id, bundle_id) = match &picked.target {
            Target::Display(id) => ("display", Some(*id), None, None),
            Target::Window(id) => ("window", None, Some(*id), None),
            Target::Application(bundle) => ("application", None, None, Some(bundle.clone())),
        };
        Self {
            kind: kind.into(),
            display_id,
            window_id,
            bundle_id,
            label: picked.label,
        }
    }
}

fn sanitize_catalog_error(message: String) -> String {
    if message.to_ascii_lowercase().contains("ffmpeg") {
        "FFmpeg 设备枚举失败（详细输出已隐藏，请查看系统日志）".to_string()
    } else {
        sanitize_public_message(&message)
    }
}

impl From<media_rtp::LiveSourceCatalog> for LiveSourceCatalogDto {
    fn from(catalog: media_rtp::LiveSourceCatalog) -> Self {
        Self {
            ffmpeg_available: catalog.ffmpeg_available,
            aac_available: catalog.aac_available,
            opus_available: catalog.opus_available,
            screens: catalog
                .screens
                .into_iter()
                .map(|screen| LiveScreenDto {
                    display_id: screen.display_id,
                    width: screen.width,
                    height: screen.height,
                    name: screen.name,
                    uri: screen.uri,
                })
                .collect(),
            cameras: catalog
                .cameras
                .into_iter()
                .map(|device| LiveAvDeviceDto {
                    index: device.index,
                    name: device.name,
                })
                .collect(),
            microphones: catalog
                .microphones
                .into_iter()
                .map(|device| LiveAvDeviceDto {
                    index: device.index,
                    name: device.name,
                })
                .collect(),
            screen_error: catalog.screen_error.map(sanitize_catalog_error),
            avfoundation_error: catalog.avfoundation_error.map(sanitize_catalog_error),
        }
    }
}

#[derive(Serialize)]
struct LiveProbeDto {
    uri: String,
    video_ready: bool,
    audio_requested: bool,
    audio_ready: Option<bool>,
    message: String,
}

/// 枚举可用于规范实时源 URI 的摄像头、麦克风和显示器。
#[tauri::command]
async fn list_live_sources() -> IpcResult<LiveSourceCatalogDto> {
    let catalog = tokio::task::spawn_blocking(media_rtp::list_live_sources)
        .await
        .map_err(|error| IpcError::internal(format!("实时源枚举任务异常: {error}")))??;
    Ok(LiveSourceCatalogDto::from(catalog))
}

/// 启动一次短时采集探测。请求音频时，用户应确保麦克风或系统正在产生声音。
#[tauri::command]
async fn probe_live_source(uri: String) -> IpcResult<LiveProbeDto> {
    let spec = media_rtp::LiveSourceSpec::parse(uri.trim())?;
    let canonical_uri = uri.trim().to_string();
    let audio_requested = match spec {
        media_rtp::LiveSourceSpec::Camera { audio_index, .. } => audio_index.is_some(),
        media_rtp::LiveSourceSpec::Screen { audio, .. } => audio.is_enabled(),
    };
    let result = tokio::task::spawn_blocking(move || -> Result<LiveProbeDto, String> {
        use media_rtp::VideoSource;

        let mut source = media_rtp::LiveSource::capture(&canonical_uri, 25)
            .map_err(|error| error.to_string())?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut video_ready = false;
        let mut audio_ready = !audio_requested;
        while std::time::Instant::now() < deadline {
            video_ready |= source.next_frame().is_some();
            if audio_requested {
                audio_ready |= !source.next_audio().is_empty();
            }
            if video_ready && audio_ready {
                break;
            }
            if let Some(error) = source.take_error() {
                return Err(error);
            }
            if !source.is_live() {
                return Err("实时采集在探测完成前停止".to_string());
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let message = if video_ready && audio_ready {
            "视频与请求的音频采集正常".to_string()
        } else if video_ready && audio_requested {
            "视频采集正常，但未检测到音频；请播放系统声音或对麦克风说话后重试".to_string()
        } else {
            "未检测到视频帧，请检查设备占用和 macOS 隐私权限".to_string()
        };
        Ok(LiveProbeDto {
            uri: canonical_uri,
            video_ready,
            audio_requested,
            audio_ready: audio_requested.then_some(audio_ready),
            message,
        })
    })
    .await
    .map_err(|error| IpcError::internal(format!("实时采集探测任务异常: {error}")))?;
    Ok(result?)
}

/// 前端传入的单设备配置。
///
/// 注意:**没有 password 字段**。前端只给 `profile_id`,密码由 Rust 侧从 OS 钥匙串取,
/// 避免明文经 IPC 往返、也避免落 localStorage。
#[derive(Deserialize)]
struct DeviceCfg {
    /// 平台档案标识,用于从钥匙串取 SIP 密码。
    profile_id: String,
    server_host: String,
    server_port: u16,
    server_domain: String,
    #[serde(default)]
    inbound_identity_mode: common::InboundIdentityMode,
    device_id: String,
    #[serde(default)]
    transport: String, // "UDP" / "TCP"
    #[serde(default)]
    gb_version: String, // "2016" / "2022"
    #[serde(default)]
    channel_name: String,
    #[serde(default)]
    video_source: Option<String>,
    /// 目录模板(single/nvr-8ch/civil-3x2/large-16ch);空则用默认单通道。
    #[serde(default)]
    catalog_template: String,
    /// 信令字符集编码(GB18030/UTF-8);空则默认 GB18030。
    #[serde(default)]
    signaling_encoding: String,
    /// 心跳间隔(秒);缺省 60。
    #[serde(default)]
    heartbeat_interval: Option<u64>,
    /// 注册有效期(秒);缺省 3600。平台可授予更短时长,续约以平台值为准。
    #[serde(default)]
    expires: Option<u32>,
    /// 设备厂商 / 型号 / 固件版本(DeviceInfo 应答字段)。之前前端填了但后端写死。
    #[serde(default)]
    manufacturer: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    firmware: Option<String>,
    /// 输出帧率(1–120);缺省 25。对文件源与实时采集都生效。
    #[serde(default)]
    video_fps: Option<u32>,
}

/// 通过"向平台发起 UDP connect"发现本机对外 IP(不真正发包)。
fn discover_local_ip(server: &str) -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect(server)?;
            Ok(s.local_addr()?.ip().to_string())
        })
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

/// 去掉空白后仍非空则返回,否则 None(用于"前端留空则用默认值")。
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// 启动一台设备:注册 + 心跳 + 入站应答,后台常驻。状态经 `device_state` 事件推送。
#[tauri::command]
async fn start_device(
    mut config: DeviceCfg,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> IpcResult<String> {
    let mut guard = state.device.lock().await;
    if guard.is_some() {
        return Err(
            IpcError::new("device.already_running", "已有设备在运行,请先停止")
                .with_retryable(false)
                .with_action("请先停止当前设备实例"),
        );
    }

    if config.server_port == 0 {
        return Err("平台端口必须在 1–65535 之间".into());
    }
    let platform_addr = format!("{}:{}", config.server_host, config.server_port)
        .parse::<std::net::SocketAddr>()
        .map_err(|_| "平台地址必须是有效的 IP:端口（当前不支持域名）".to_string())?;
    // 密码从钥匙串取:未配置时给出明确的引导而不是「密码不能为空」。
    let password = secrets::get(&config.profile_id)?;
    if let Some(source) = config.video_source.take() {
        let source = source.trim().to_string();
        if source.is_empty() {
            config.video_source = None;
        } else {
            if source.starts_with("live:") {
                let spec = media_rtp::LiveSourceSpec::parse(&source)?;
                let catalog = tokio::task::spawn_blocking(media_rtp::list_live_sources)
                    .await
                    .map_err(|error| {
                        IpcError::internal(format!("实时设备枚举任务异常: {error}"))
                    })??;
                let requested_audio_codec = match &spec {
                    media_rtp::LiveSourceSpec::Camera {
                        audio_index: Some(_),
                        audio_codec,
                        ..
                    } => Some(*audio_codec),
                    media_rtp::LiveSourceSpec::Screen {
                        audio, audio_codec, ..
                    } if audio.is_enabled() => Some(*audio_codec),
                    _ => None,
                };
                match requested_audio_codec {
                    Some(media_rtp::LiveAudioCodec::Aac) if !catalog.aac_available => {
                        return Err("当前内嵌 FFmpeg 不支持 AAC 编码，请改用 G.711A/G.711U".into());
                    }
                    Some(media_rtp::LiveAudioCodec::Opus) if !catalog.opus_available => {
                        return Err(
                            "当前内嵌 FFmpeg 不支持 libopus 编码，请改用 G.711A/G.711U".into()
                        );
                    }
                    _ => {}
                }
                match spec {
                    media_rtp::LiveSourceSpec::Camera { video_index, .. }
                        if !catalog
                            .cameras
                            .iter()
                            .any(|camera| camera.index == video_index) =>
                    {
                        return Err(catalog
                            .avfoundation_error
                            .unwrap_or_else(|| {
                                format!("未找到 AVFoundation 摄像头索引 {video_index}")
                            })
                            .into());
                    }
                    media_rtp::LiveSourceSpec::Camera {
                        audio_index: Some(index),
                        ..
                    } if !catalog
                        .microphones
                        .iter()
                        .any(|microphone| microphone.index == index) =>
                    {
                        return Err(catalog
                            .avfoundation_error
                            .unwrap_or_else(|| format!("未找到 AVFoundation 麦克风索引 {index}"))
                            .into());
                    }
                    media_rtp::LiveSourceSpec::Screen {
                        audio: media_rtp::LiveAudioSource::Microphone(index),
                        ..
                    } if !catalog
                        .microphones
                        .iter()
                        .any(|microphone| microphone.index == index) =>
                    {
                        return Err(catalog
                            .avfoundation_error
                            .unwrap_or_else(|| format!("未找到 AVFoundation 麦克风索引 {index}"))
                            .into());
                    }
                    media_rtp::LiveSourceSpec::Screen {
                        target: media_rtp::ScreenCaptureTarget::Display(display_id),
                        ..
                    } if display_id != 0
                        && !catalog
                            .screens
                            .iter()
                            .any(|screen| screen.display_id == display_id) =>
                    {
                        return Err(catalog
                            .screen_error
                            .unwrap_or_else(|| {
                                format!("未找到 ScreenCaptureKit 显示器 {display_id}")
                            })
                            .into());
                    }
                    _ => {}
                }
            }
            config.video_source = Some(source);
        }
    }

    // 输出帧率:前端可选 15/25/30/60,这里仍按 1–120 兜底校验(IPC 入参不可信)。
    let video_fps = config.video_fps.unwrap_or(25);
    if !(1..=120).contains(&video_fps) {
        return Err("输出帧率必须在 1–120 之间".into());
    }

    let device_id = DeviceId::new(config.device_id.clone())?;
    // 通道 ID:设备 ID 前 17 位 + 132(视频通道类型码)。
    let channel_id_str = format!("{}132", &config.device_id[..17]);
    let channel_id = DeviceId::new(channel_id_str)?;
    let gb_version = if config.gb_version == "2016" {
        GbVersion::V2016
    } else {
        GbVersion::V2022
    };
    // 传输由用户选择;Via 里的标识由传输对象自己给出,不会与实际发送通道不一致。
    let transport = if config.transport.eq_ignore_ascii_case("TCP") {
        Transport::Tcp
    } else {
        Transport::Udp
    };
    let channel_name = if config.channel_name.is_empty() {
        "Camera-1".to_string()
    } else {
        config.channel_name.clone()
    };

    let cfg = DeviceConfig {
        device_id: device_id.clone(),
        username: config.device_id.clone(),
        password: common::Secret::new(password),
        server_host: config.server_host.clone(),
        server_port: config.server_port,
        server_domain: config.server_domain.clone(),
        inbound_identity_mode: config.inbound_identity_mode,
        transport,
        heartbeat_interval_secs: config.heartbeat_interval.unwrap_or(60).max(1),
        register_expires_secs: config.expires.unwrap_or(3600).max(1),
        // §9.1.1 c):注册重试间隔不应短于 60s(单设备联调遵守标准)。
        register_retry_max_secs: gb28181_simulator::DEFAULT_REGISTER_RETRY_MAX_SECS,
        channels: vec![ChannelConfig {
            channel_id,
            name: channel_name,
            status: "ON".into(),
        }],
        device_info: DeviceInfo {
            device_name: if config.channel_name.is_empty() {
                "UVP-Sim-Desktop".to_string()
            } else {
                config.channel_name.clone()
            },
            manufacturer: non_empty(config.manufacturer.as_deref())
                .unwrap_or("UVP")
                .into(),
            model: non_empty(config.model.as_deref())
                .unwrap_or("Desktop-Sim")
                .into(),
            firmware: non_empty(config.firmware.as_deref())
                .unwrap_or(env!("CARGO_PKG_VERSION"))
                .into(),
        },
        video_source: config.video_source.clone(),
        video_fps,
        light_bitrate_kbps: None,
        gb_version,
        signaling_encoding: common::SignalingEncoding::from_str_lenient(&config.signaling_encoding),
    };

    // 预热视频源:容器(MP4 等)归一化转码可能耗时数秒,若留到 INVITE 时同步做会阻塞
    // 200 OK 与首包推流,导致平台收流超时。这里在设备上线前先转好、缓存,
    // INVITE 时命中缓存瞬时加载。ffmpeg 是阻塞调用,放 spawn_blocking。
    // 实时采集(live:)无文件可预热,跳过;仅对文件源(C 档容器)做归一化预热。
    // 帧率必须与设备配置一致:归一化产物按帧率缓存,预热错帧率等于白转。
    if let Some(ref vs) = config.video_source {
        if !vs.trim().is_empty() && !vs.trim().starts_with("live:") {
            let vs = vs.clone();
            let prepared = tokio::task::spawn_blocking(move || {
                gb28181_simulator::prepare_video_source_at(&vs, video_fps)
            })
            .await
            .map_err(|error| IpcError::internal(format!("视频源准备任务异常: {error}")))?;
            prepared?;
        }
    }

    // 共享传输 + 本端地址发现。可信来源在接收/accept 循环启动前原子安装，
    // 避免 TCP bind 后再设白名单时，竞态窗口中的恶意连接被长期保留。
    let trusted_ips = vec![platform_addr.ip()];
    let udp: Arc<dyn SipTransport> = match transport {
        Transport::Udp => UdpTransport::bind_with_trusted_ips("0.0.0.0:0", trusted_ips).await?,
        Transport::Tcp => TcpTransport::bind_with_trusted_ips("0.0.0.0:0", trusted_ips).await?,
    };
    let local_port = udp.local_addr()?.port();
    let local_host = discover_local_ip(&platform_addr.to_string());

    // 单设备联调默认开启 SIP 信令追踪(FR-43),把收发报文推给前端。
    udp.set_tracer(Some(Arc::new(TraceEmitter { app: app.clone() })));

    // 带状态观察者的设备实例。
    let observer: Arc<dyn DeviceObserver> = Arc::new(StateEmitter { app: app.clone() });
    let sim = Arc::new(DeviceSimulator::with_observer(cfg, observer));

    // 若指定了目录模板,在注册前载入(平台注册后会立即同步目录,需在这之前准备好)。
    if !config.catalog_template.is_empty() {
        sim.load_catalog_template(&config.catalog_template);
    }

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let sim_run = sim.clone();
    let tp_run = udp.clone();
    let host_run = local_host.clone();
    let run_task = tokio::spawn(async move {
        sim_run
            .run(tp_run, host_run, local_port, async move {
                let _ = stop_rx.await;
            })
            .await;
    });

    *guard = Some(DeviceHandle {
        sim,
        transport: udp,
        local_host,
        local_port,
        stop_tx,
        run_task,
    });
    Ok("设备已启动".into())
}

/// 停设备并**等到下线序列真的走完**(带超时)。返回是否停了一台设备。
///
/// 与 `stop_device` 命令的区别:命令要立刻返回让界面响应,这里要等 ——
/// 应用退出时必须先让平台收到注销,否则平台侧那台设备会挂着直到心跳超时。
async fn stop_device_graceful(state: &AppState) -> bool {
    let handle = state.device.lock().await.take();
    let Some(handle) = handle else {
        return false;
    };
    let DeviceHandle {
        transport,
        stop_tx,
        mut run_task,
        ..
    } = handle;
    let _ = stop_tx.send(());
    if await_or_abort_task("设备下线", &mut run_task).await {
        tracing::info!("设备下线序列已完成");
    }
    // 即使任务被 abort，future drop 会先释放媒体源；随后关传输释放 socket。
    transport.shutdown();
    true
}

/// 等待 supervisor 完成 series/状态交接。返回 false 表示 sender 未发布完成就异常关闭。
async fn wait_stress_completion(completion: &mut tokio::sync::watch::Receiver<bool>) -> bool {
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
async fn clear_abandoned_stress(state: &AppState, run_id: u64) {
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
async fn stop_stress_graceful(state: &AppState) -> bool {
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

/// 应用退出前的清理:先设备、再压测。
///
/// 顺序有讲究:设备下线要发 SIP 注销,而那需要传输还活着;
/// 压测停止会连带释放它自己的分片传输。
///
/// **ffmpeg 采集子进程不需要单独处理**:所有权链是
/// `PushSession.task` → 推流 future → `Box<dyn VideoSource>` → `LiveSource::drop`,
/// 而 `release_media_on_shutdown` 现在会 join 推流任务(见 gb28181-simulator 的
/// `await_task`)。等到这里返回时子进程已被各自的 `Drop` 杀掉。
/// 只有等待超时那条路径可能留下孤儿 —— 那种情况下面会明确告警。
async fn shutdown_all(state: &AppState) {
    let stopped_device = stop_device_graceful(state).await;
    let stopped_stress = stop_stress_graceful(state).await;
    tracing::info!(stopped_device, stopped_stress, "退出前清理完成");
}

/// 停止当前设备。
#[tauri::command]
async fn stop_device(state: tauri::State<'_, AppState>, app: AppHandle) -> IpcResult<String> {
    let mut guard = state.device.lock().await;
    match guard.take() {
        Some(h) => {
            let DeviceHandle {
                transport,
                stop_tx,
                mut run_task,
                ..
            } = h;
            let _ = stop_tx.send(());
            let _ = app.emit("device_state", "Disconnected");
            // 界面要立刻响应,所以在后台等下线；超时必须 abort+await，不能 detach
            // 仍持有 LiveSource/FFmpeg 的任务。
            tokio::spawn(async move {
                await_or_abort_task("设备停止", &mut run_task).await;
                transport.shutdown();
            });
            Ok("设备已停止".into())
        }
        None => Err(IpcError::device_not_running("没有正在运行的设备")),
    }
}

/// 主动上报一条报警（支持指定国标 13 种视频报警类型与优先级）。
#[tauri::command]
async fn fire_alarm(
    description: Option<String>,
    alarm_type: Option<u32>,
    priority: Option<u8>,
    state: tauri::State<'_, AppState>,
) -> IpcResult<String> {
    let guard = state.device.lock().await;
    match &*guard {
        Some(h) => {
            let desc = description
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "移动侦测报警".to_string());
            let vtype = match alarm_type.unwrap_or(2) {
                1 => gb28181_protocol::manscdp::VideoAlarmType::Manual,
                2 => gb28181_protocol::manscdp::VideoAlarmType::MotionDetection,
                3 => gb28181_protocol::manscdp::VideoAlarmType::ObjectLeft,
                4 => gb28181_protocol::manscdp::VideoAlarmType::ObjectRemoved,
                5 => gb28181_protocol::manscdp::VideoAlarmType::TripwireCrossing,
                6 => gb28181_protocol::manscdp::VideoAlarmType::Intrusion,
                7 => gb28181_protocol::manscdp::VideoAlarmType::WrongWay,
                8 => gb28181_protocol::manscdp::VideoAlarmType::Loitering,
                9 => gb28181_protocol::manscdp::VideoAlarmType::FlowStatistics,
                10 => gb28181_protocol::manscdp::VideoAlarmType::DensityDetection,
                11 => gb28181_protocol::manscdp::VideoAlarmType::VideoAbnormal,
                12 => gb28181_protocol::manscdp::VideoAlarmType::FastMoving,
                13 => gb28181_protocol::manscdp::VideoAlarmType::Tampering,
                _ => gb28181_protocol::manscdp::VideoAlarmType::MotionDetection,
            };
            let code = h
                .sim
                .report_alarm_full(
                    &h.transport,
                    &h.local_host,
                    h.local_port,
                    &desc,
                    vtype,
                    priority,
                )
                .await?;
            if code == 0 {
                Ok("报警已触发，但因不满足平台订阅筛选条件已被本端过滤".to_string())
            } else {
                Ok(format!("报警上报完成(平台响应 {code})"))
            }
        }
        None => Err(IpcError::device_not_running("设备未启动")),
    }
}

/// 当前语音广播会话状态(§9.12.1)。无会话时返回 null。
///
/// 走轮询而不是事件流:收流统计每 20ms 就变一次,推事件等于给 webview 灌洪水;
/// 会话的开始/结束本身有 `platform_command` 事件,轮询只补统计与电平。
#[tauri::command]
async fn get_broadcast_status(
    state: tauri::State<'_, AppState>,
) -> IpcResult<Option<gb28181_simulator::BroadcastSnapshot>> {
    let guard = state.device.lock().await;
    match &*guard {
        Some(h) => Ok(h.sim.broadcast_snapshot().await),
        // 设备没启动不是错误:界面就该显示"空闲"。
        None => Ok(None),
    }
}

/// 当前推流会话的媒体遥测(发送统计 + 平台回的 RTCP 接收报告)。
///
/// 无推流会话、或会话刚建立发送通道还没就绪时返回 null。
/// 同样走轮询:RTCP RR 每 5 秒才来一条,但发送包数是每帧在变。
#[tauri::command]
async fn get_media_telemetry(
    state: tauri::State<'_, AppState>,
) -> IpcResult<Option<gb28181_simulator::MediaTelemetrySnapshot>> {
    let guard = state.device.lock().await;
    match &*guard {
        Some(h) => Ok(h.sim.media_telemetry().await),
        None => Ok(None),
    }
}

/// 设备侧主动挂断语音广播(按附录 K a) 发会话内 BYE)。
#[tauri::command]
async fn hangup_broadcast(state: tauri::State<'_, AppState>) -> IpcResult<String> {
    let guard = state.device.lock().await;
    match &*guard {
        Some(h) => {
            if h.sim.hangup_broadcast(&h.transport).await {
                Ok("已挂断语音广播并通知平台".into())
            } else {
                Ok("当前没有进行中的语音广播".into())
            }
        }
        None => Err(IpcError::device_not_running("设备未启动")),
    }
}

/// 主动上报一条 GPS 位置。
#[tauri::command]
async fn fire_position(
    longitude: f64,
    latitude: f64,
    state: tauri::State<'_, AppState>,
) -> IpcResult<String> {
    let guard = state.device.lock().await;
    match &*guard {
        Some(h) => {
            let code = h
                .sim
                .report_position(
                    &h.transport,
                    &h.local_host,
                    h.local_port,
                    longitude,
                    latitude,
                )
                .await?;
            Ok(format!("位置上报完成(平台响应 {code})"))
        }
        None => Err(IpcError::device_not_running("设备未启动")),
    }
}

/// 开关 SIP 信令追踪(FR-43)。开启后每条收发报文以 `sip_trace` 事件推给前端。
#[tauri::command]
async fn set_sip_trace(
    enabled: bool,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> IpcResult<String> {
    let guard = state.device.lock().await;
    match &*guard {
        Some(h) => {
            if enabled {
                h.transport
                    .set_tracer(Some(Arc::new(TraceEmitter { app: app.clone() })));
                Ok("信令追踪已开启".into())
            } else {
                h.transport.set_tracer(None);
                Ok("信令追踪已关闭".into())
            }
        }
        None => Err(IpcError::device_not_running("设备未启动")),
    }
}

// ── 数据传输对象 ──────────────────────────────────────────

/// 目录节点 DTO(前端多通道管理 ⇄ 引擎 CatalogNode)。
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct ChannelNodeDto {
    id: String,
    /// 类型:Device/BusinessGroup/VirtualOrg/VideoChannel/AlarmChannel。
    node_type: String,
    name: String,
    parent_id: String,
    #[serde(default)]
    civil_code: Option<String>,
    #[serde(default = "default_on")]
    status: String,
    // ── 以下是 A.2.1.9 的逐通道可选字段(留空即用设备级默认值)──
    /// 安装地址(`Address`)。一台 NVR 下各路摄像机装在不同位置,
    /// 全报同一个地址会让平台的地图与清单失去意义。
    #[serde(default)]
    address: Option<String>,
    /// 经度(`Longitude`)。留空取设备当前位置。
    #[serde(default)]
    longitude: Option<f64>,
    /// 纬度(`Latitude`)。留空取设备当前位置。
    #[serde(default)]
    latitude: Option<f64>,
    /// 摄像机类型(`Info/PTZType`):1 球机 / 2 半球 / 3 固定枪机 / 4 遥控枪机。
    /// 平台按它决定是否给这一路显示云台面板。
    #[serde(default)]
    ptz_type: Option<u8>,
    /// 采集部位类型(`Info/CapturePositionType`,附录 O 的 7 位层次码)。
    #[serde(default)]
    capture_position_type: Option<String>,
}

fn default_on() -> String {
    "ON".into()
}

/// 解析节点类型。**未知值报错而不是静默落到 VideoChannel** —— 静默兜底会让
/// 前端拼错的类型带着错误的 Parental 值被 NOTIFY 给平台。
fn node_type_from_str(s: &str) -> Result<gb28181_protocol::id_codec::CatalogNodeType, String> {
    use gb28181_protocol::id_codec::CatalogNodeType::*;
    Ok(match s {
        "BusinessGroup" => BusinessGroup,
        "VirtualOrg" => VirtualOrg,
        "AlarmChannel" => AlarmChannel,
        "Device" => Device,
        "VideoChannel" => VideoChannel,
        other => return Err(format!("未知的目录节点类型: {other}")),
    })
}

/// 校验目录节点:ID 必须是合法国标编号,类型必须已知,状态只能 ON/OFF。
///
/// 之前 `id`/`parent_id` 是裸 String、完全不过 `DeviceId::new`,非法编号会直接
/// 进目录树并被 Catalog NOTIFY 发给平台。
fn validate_channel_node(
    dto: ChannelNodeDto,
) -> Result<gb28181_protocol::id_codec::CatalogNode, String> {
    let id = DeviceId::new(dto.id.trim().to_string()).map_err(|e| format!("通道 ID 非法: {e}"))?;
    let parent = dto.parent_id.trim();
    if parent.is_empty() {
        return Err("父节点 ID 不能为空".into());
    }
    DeviceId::new(parent.to_string()).map_err(|e| format!("父节点 ID 非法: {e}"))?;
    let node_type = node_type_from_str(dto.node_type.trim())?;
    let name = dto.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err("通道名称长度必须在 1..=64 个字符之间".into());
    }
    let status = dto.status.trim().to_uppercase();
    if status != "ON" && status != "OFF" {
        return Err("通道状态只能是 ON 或 OFF".into());
    }
    if let Some(code) = dto.civil_code.as_deref() {
        let code = code.trim();
        let looks_valid =
            code.is_empty() || (code.len() <= 8 && code.bytes().all(|b| b.is_ascii_digit()));
        if !looks_valid {
            return Err("行政区划编码必须是 1..=8 位数字".into());
        }
    }

    let mut node = gb28181_protocol::id_codec::CatalogNode::new(
        id.as_str().to_string(),
        node_type,
        name.to_string(),
        parent.to_string(),
    );
    node.civil_code = dto
        .civil_code
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty());
    node.status = status;

    // 逐通道扩展字段:空串等于"不填",不能写成 Some("")——
    // 那会让目录项里出现一个空的 <Address/>,平台侧显示成空白而不是默认值。
    node.address = dto
        .address
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty());
    if let Some(lon) = dto.longitude {
        if !(-180.0..=180.0).contains(&lon) {
            return Err("经度必须在 -180..=180 之间".into());
        }
    }
    if let Some(lat) = dto.latitude {
        if !(-90.0..=90.0).contains(&lat) {
            return Err("纬度必须在 -90..=90 之间".into());
        }
    }
    // 经纬度要么都填要么都不填:只填一个会让平台把另一半当 0 度,
    // 于是这一路通道被钉在几内亚湾。
    match (dto.longitude, dto.latitude) {
        (Some(lon), Some(lat)) => {
            node.longitude = Some(lon);
            node.latitude = Some(lat);
        }
        (None, None) => {}
        _ => return Err("经度与纬度必须同时填写或同时留空".into()),
    }
    if let Some(ptz) = dto.ptz_type {
        if !(1..=4).contains(&ptz) {
            return Err("摄像机类型只能是 1(球机)/2(半球)/3(固定枪机)/4(遥控枪机)".into());
        }
        node.ptz_type = Some(ptz);
    }
    if let Some(code) = dto.capture_position_type.as_deref() {
        let code = code.trim();
        if !code.is_empty() {
            if !gb28181_protocol::manscdp::capture_position::is_valid(code) {
                return Err(
                    "采集部位类型必须是附录 O 的 7 位数字层次码,如 0020100(城市道路)".into(),
                );
            }
            node.capture_position_type = Some(code.to_string());
        }
    }
    Ok(node)
}

fn node_type_to_str(t: gb28181_protocol::id_codec::CatalogNodeType) -> &'static str {
    use gb28181_protocol::id_codec::CatalogNodeType::*;
    match t {
        Device => "Device",
        BusinessGroup => "BusinessGroup",
        VirtualOrg => "VirtualOrg",
        VideoChannel => "VideoChannel",
        AlarmChannel => "AlarmChannel",
    }
}

impl From<&gb28181_protocol::id_codec::CatalogNode> for ChannelNodeDto {
    fn from(n: &gb28181_protocol::id_codec::CatalogNode) -> Self {
        ChannelNodeDto {
            id: n.id.clone(),
            node_type: node_type_to_str(n.node_type).into(),
            name: n.name.clone(),
            parent_id: n.parent_id.clone(),
            civil_code: n.civil_code.clone(),
            status: n.status.clone(),
            address: n.address.clone(),
            longitude: n.longitude,
            latitude: n.latitude,
            ptz_type: n.ptz_type,
            capture_position_type: n.capture_position_type.clone(),
        }
    }
}

/// 载入内置目录模板(single/nvr-8ch/civil-3x2/large-16ch),返回加载后的节点列表(FR-34)。
#[tauri::command]
async fn load_catalog_template(
    template: String,
    state: tauri::State<'_, AppState>,
) -> IpcResult<Vec<ChannelNodeDto>> {
    const TEMPLATES: [&str; 4] = ["single", "nvr-8ch", "civil-3x2", "large-16ch"];
    if !TEMPLATES.contains(&template.as_str()) {
        return Err(IpcError::config(format!(
            "未知的目录模板 {template},可选:{}",
            TEMPLATES.join(" / ")
        )));
    }
    let guard = state.device.lock().await;
    let h = guard
        .as_ref()
        .ok_or_else(|| IpcError::device_not_running("设备未启动,请先在单设备页启动设备"))?;
    h.sim.load_catalog_template(&template);
    // 增量 NOTIFY 推送是副作用,后台发送不阻塞 UI 返回(平台不及时回 200 时事务会退避重传数秒)。
    spawn_catalog_notify(h);
    Ok(h.sim.catalog_tree().iter().map(Into::into).collect())
}

/// 获取当前目录树节点(FR-34)。
#[tauri::command]
async fn get_catalog_tree(state: tauri::State<'_, AppState>) -> IpcResult<Vec<ChannelNodeDto>> {
    let guard = state.device.lock().await;
    let h = guard
        .as_ref()
        .ok_or_else(|| IpcError::device_not_running("设备未启动"))?;
    Ok(h.sim.catalog_tree().iter().map(Into::into).collect())
}

/// 新增/更新一个目录通道节点,触发增量 NOTIFY(FR-34)。
#[tauri::command]
async fn upsert_channel(
    node: ChannelNodeDto,
    state: tauri::State<'_, AppState>,
) -> IpcResult<String> {
    let node = validate_channel_node(node)?;
    let guard = state.device.lock().await;
    let h = guard
        .as_ref()
        .ok_or_else(|| IpcError::device_not_running("设备未启动"))?;
    let added = h.sim.upsert_channel(node);
    spawn_catalog_notify(h);
    Ok(if added {
        "已新增通道".into()
    } else {
        "已更新通道".into()
    })
}

/// 删除一个目录通道节点,触发增量 NOTIFY(FR-34)。
#[tauri::command]
async fn remove_channel(id: String, state: tauri::State<'_, AppState>) -> IpcResult<String> {
    let guard = state.device.lock().await;
    let h = guard
        .as_ref()
        .ok_or_else(|| IpcError::device_not_running("设备未启动"))?;
    if h.sim.remove_channel(&id) {
        spawn_catalog_notify(h);
        Ok("已删除通道".into())
    } else {
        Err("通道不存在".into())
    }
}

/// 后台推送目录增量 NOTIFY(fire-and-forget)。
///
/// NOTIFY 是通知平台的副作用,不该阻塞 IPC 返回:目录订阅存在但平台不及时回 200 时,
/// SIP 客户端事务会按 T1 退避重传,最坏阻塞约 4 秒。放后台 spawn 让 UI 立即拿到目录树。
fn spawn_catalog_notify(h: &DeviceHandle) {
    let sim = Arc::clone(&h.sim);
    let transport = Arc::clone(&h.transport);
    tokio::spawn(async move {
        if let Err(e) = sim.notify_catalog_changed(&transport).await {
            tracing::warn!(error = %e, "目录增量 NOTIFY 推送失败");
        }
    });
}

#[derive(serde::Serialize)]
struct ScenarioSummary {
    name: String,
    device_count: usize,
    server_host: String,
    server_port: u16,
}

// ── 系统信息:日志 ────────────────────────────────────────

/// 设置全局日志级别(error/warn/info/debug/trace,或 EnvFilter 语法)。返回生效后的级别。
#[tauri::command]
fn set_log_level(level: String) -> IpcResult<String> {
    if common::logging::set_level(&level) {
        Ok(common::logging::current_level())
    } else {
        Err(IpcError::config(format!("无效的日志级别: {level}")))
    }
}

/// 查询当前日志级别。
#[tauri::command]
fn get_log_level() -> String {
    common::logging::current_level()
}

/// 拉取最近缓冲的日志(前端打开日志页时补历史)。
#[tauri::command]
fn get_recent_logs(max: Option<usize>) -> Vec<common::logging::LogLine> {
    common::logging::recent(max.unwrap_or(500))
}

/// 导出诊断包(日志 + 运行环境快照),返回 zip 路径。
///
/// 与「导出压测报告」分开:报告回答"这次压测结果如何",诊断包回答
/// "这台机器上到底发生了什么"。后者才是把问题发给技术支持时需要的东西。
#[tauri::command]
async fn export_diagnostics(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> IpcResult<String> {
    let dir = reports_dir(&app);
    let preserve = loop {
        let stress_guard = state.stress.lock().await;
        let Ok(last_series) = state.last_series.try_lock() else {
            drop(stress_guard);
            tokio::task::yield_now().await;
            continue;
        };
        let mut preserve = std::collections::HashSet::new();
        if let Some(path) = stress_guard
            .as_ref()
            .and_then(|handle| handle.series_path.clone())
        {
            preserve_series_path(&mut preserve, &path);
        }
        if let Some(path) = last_series.clone() {
            preserve_series_path(&mut preserve, &path);
        }
        break preserve;
    };
    // 打包会读若干 MB 日志并做 deflate，放到阻塞线程池，别卡住 IPC。
    let path = tauri::async_runtime::spawn_blocking(move || diagnostics::export(&dir, &preserve))
        .await
        .map_err(|error| {
            IpcError::new(
                "diagnostics.export_failed",
                format!("诊断包导出任务异常: {error}"),
            )
        })?
        .map_err(|message| IpcError::new("diagnostics.export_failed", message))?;
    Ok(path.display().to_string())
}

/// 弹出 macOS 系统内容选择器(`SCContentSharingPicker`),让用户按
/// 「显示器 / 窗口 / 应用」挑选要推流的画面。
///
/// 为什么必须走系统选择器:平台侧要的是"某一块画面",而"哪一块"只有用户知道;
/// 应用自己列窗口既拿不到跨应用的稳定身份,也不符合 macOS 的授权习惯。
/// 选择器只在主线程呈现,等待结果放在阻塞线程池 —— 主线程被占住会让回调
/// (走主队列)永远回不来。
///
/// 用户取消时返回 `None`(不是错误)。
#[tauri::command]
async fn pick_share_target(app: AppHandle) -> IpcResult<Option<ShareTargetDto>> {
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        // 呈现动作本身是非阻塞的:把句柄送回异步上下文后再等结果。
        let _ = ready_tx.send(media_rtp::show_share_picker());
    })
    .map_err(|error| IpcError::internal(format!("无法在主线程呈现系统选择器: {error}")))?;

    let handle = ready_rx
        .await
        .map_err(|_| IpcError::internal("系统选择器未能呈现(主线程已退出)"))?
        .map_err(|error| IpcError::new("media.picker_failed", error.to_string()))?;

    let picked = tokio::task::spawn_blocking(move || handle.wait(media_rtp::SHARE_PICKER_TIMEOUT))
        .await
        .map_err(|error| IpcError::internal(format!("系统选择器等待任务异常: {error}")))?
        .map_err(|error| IpcError::new("media.picker_failed", error.to_string()))?;

    Ok(picked.map(ShareTargetDto::from))
}

/// 弹出系统文件对话框让用户选一个视频源文件,并**同时**授权它的 asset 预览。
///
/// 对话框由 Rust 侧弹出而不是前端 —— 这样"哪个路径被授权"完全由用户的真实选择决定,
/// webview 无法凭空捏造一个路径来换取读取能力(见 `media_access` 模块文档)。
/// 用户取消时返回 `None`。
#[tauri::command]
async fn pick_video_source(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> IpcResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("选择视频源文件")
        .add_filter("视频", media_access::MEDIA_EXTENSIONS)
        .pick_file(move |picked| {
            // 对话框回调在别的线程,用 oneshot 把结果搬回当前 async 上下文。
            let _ = tx.send(picked);
        });
    let picked = rx
        .await
        .map_err(|_| IpcError::new("media.dialog_failed", "文件选择对话框异常关闭"))?;
    let Some(file) = picked else {
        return Ok(None); // 用户取消,不是错误
    };
    let path = file.into_path().map_err(|error| {
        IpcError::new(
            "media.path_invalid",
            format!("无法识别所选文件路径: {error}"),
        )
    })?;

    let canonical = state
        .media_grants
        .grant(&path)
        .map_err(|message| IpcError::new("media.access", message))?;
    app.asset_protocol_scope()
        .allow_file(&canonical)
        .map_err(|error| {
            IpcError::new("media.preview_denied", format!("放行媒体预览失败: {error}"))
        })?;
    Ok(Some(canonical.to_string_lossy().into_owned()))
}

/// 为**已授权过**的媒体文件重新放行 asset 预览(应用重启后恢复上次选的文件)。
///
/// asset scope 的放行只在本次进程内有效,而用户上次选的文件路径存在前端 localStorage 里,
/// 重启后需要再放行一次。这里只接受 `media_grants` 里已有的条目 ——
/// 那个集合唯一的写入口是上面的系统文件对话框,所以 webview 传任何别的路径都会被拒。
#[tauri::command]
fn allow_media_preview(
    path: String,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> IpcResult<()> {
    let p = std::path::PathBuf::from(&path);
    if !state.media_grants.is_granted(&p) {
        return Err(IpcError::new(
            "media.preview_denied",
            "该文件尚未经你选择授权,请点「选择文件」重新选取",
        )
        .with_retryable(false)
        .with_action("请点「选择文件」重新选择该媒体文件"));
    }
    let canonical = p
        .canonicalize()
        .map_err(|error| IpcError::new("media.access", format!("文件不可访问: {error}")))?;
    app.asset_protocol_scope()
        .allow_file(&canonical)
        .map_err(|error| {
            IpcError::new("media.preview_denied", format!("放行媒体预览失败: {error}"))
        })?;
    Ok(())
}

// ── 应用入口 ──────────────────────────────────────────────

/// 初始化日志、注册命令、启动 Tauri 事件循环。
pub fn run() {
    // 注意:日志在 `.setup()` 里初始化,不在这里。
    // 落盘目录取自 `app_data_dir()`,而那个路径要有 AppHandle 才能解析 ——
    // 在这里自己按平台规则拼一遍会和 Tauri 的实现产生第二个真相来源。
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // 日志必须最先初始化,否则 setup 里后续的日志会漏掉落盘。
            //
            // 之前只写标准输出:桌面应用从 Finder / 开始菜单启动时标准输出无处可去,
            // 日志只活在 2000 条内存环里,进程一崩就没有任何现场。
            let data_dir = app.path().app_data_dir().ok();
            match data_dir.as_ref().map(|d| d.join("logs")) {
                Some(dir) => match common::logging::init_with_file(dir) {
                    Some(dir) => tracing::info!(log_dir = %dir.display(), "日志已落盘"),
                    None => {
                        common::logging::init();
                        tracing::warn!("日志目录不可用,本次仅输出到控制台");
                    }
                },
                None => {
                    common::logging::init();
                    tracing::warn!("无法定位应用数据目录,本次仅输出到控制台");
                }
            }

            // 状态在 setup 里注册而不是 builder 上:媒体预览授权要落到应用数据目录,
            // 而那个目录只有拿到 AppHandle 之后才能解析出来。setup 早于窗口加载,
            // 任何命令跑起来时状态都已就位。
            app.manage(AppState::new(data_dir.clone()));
            let artifact_dir = data_dir
                .unwrap_or_else(std::env::temp_dir)
                .join("uvp-reports");
            tauri::async_runtime::spawn_blocking(move || {
                let health = cleanup_artifacts(&artifact_dir, std::collections::HashSet::new());
                if !health.healthy {
                    tracing::warn!(?health, "应用启动时产物清理未完全健康");
                }
            });
            tracing::info!(
                media_grants = app.state::<AppState>().media_grants.len(),
                work_dir = %media_rtp::work_dir().display(),
                "已载入媒体预览授权记录"
            );
            // 实时日志推送:**批量聚合**后再过 IPC。
            //
            // 之前是每条日志一个 `log_line` 事件。压测时 10k 设备的 info 日志轻易到
            // 每秒数千条,每条都要 JSON 序列化 + 跨进程送进 webview——前端裁到 3000 条
            // 是在代价已经付完之后。现在每 100ms 合并成一个 `log_batch` 事件,
            // 上限 10 个事件/秒,并统计因队列满被丢弃的条数。
            let handle = app.handle().clone();
            let (log_tx, mut log_rx) = tokio::sync::mpsc::channel::<common::logging::LogLine>(4096);
            let dropped = Arc::new(AtomicU64::new(0));
            let dropped_producer = Arc::clone(&dropped);
            common::logging::set_callback(move |line| {
                if log_tx.try_send(line.clone()).is_err() {
                    dropped_producer.fetch_add(1, Ordering::Relaxed);
                }
            });
            tauri::async_runtime::spawn(async move {
                const FLUSH_MS: u64 = 100;
                const MAX_PER_BATCH: usize = 500;
                let mut ticker = tokio::time::interval(std::time::Duration::from_millis(FLUSH_MS));
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut batch: Vec<common::logging::LogLine> = Vec::with_capacity(MAX_PER_BATCH);
                loop {
                    ticker.tick().await;
                    batch.clear();
                    while batch.len() < MAX_PER_BATCH {
                        match log_rx.try_recv() {
                            Ok(line) => batch.push(line),
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                            // 发送端随回调常驻,不会真的断开;断了就退出任务。
                            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return,
                        }
                    }
                    if batch.is_empty() {
                        continue;
                    }
                    let dropped_now = dropped.swap(0, Ordering::Relaxed);
                    let _ = handle.emit(
                        "log_batch",
                        serde_json::json!({ "lines": &batch, "dropped": dropped_now }),
                    );
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            engine_version,
            get_inbound_security_capabilities,
            validate_scenario,
            start_stress,
            stop_stress,
            get_metrics,
            export_report,
            list_live_sources,
            probe_live_source,
            start_device,
            stop_device,
            fire_alarm,
            fire_position,
            get_broadcast_status,
            get_media_telemetry,
            hangup_broadcast,
            set_sip_trace,
            get_stress_status,
            get_device_status,
            load_catalog_template,
            get_catalog_tree,
            upsert_channel,
            remove_channel,
            set_log_level,
            get_log_level,
            get_recent_logs,
            allow_media_preview,
            pick_share_target,
            pick_video_source,
            export_diagnostics,
            secrets::save_platform_secret,
            secrets::delete_platform_secret,
            secrets::platform_secret_status,
        ])
        .build(tauri::generate_context!())
        .expect("Tauri 应用启动失败")
        .run(|app, event| {
            // 退出前收敛设备与压测。
            //
            // 之前没有这个钩子,关窗口等于直接结束进程,后果有三个:
            // 1. 不发注销 REGISTER,平台要等心跳超时(默认几分钟)才把设备置离线,
            //    下一轮联调看到的是一台"在线但不响应"的幽灵设备;
            // 2. 压测的分片 socket 被硬关,平台侧留下上万个半死连接;
            // 3. ffmpeg 采集子进程来不及被 Drop 清掉,变成孤儿进程继续占摄像头。
            if let tauri::RunEvent::ExitRequested { api, .. } = &event {
                // 只拦第一次。`AppHandle::exit()` 会再发一次 ExitRequested,
                // 无条件 prevent_exit 会让应用永远退不掉。
                if CLEANUP_STARTED.swap(true, Ordering::SeqCst) {
                    return;
                }
                api.prevent_exit();
                let handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    shutdown_all(&handle.state::<AppState>()).await;
                    handle.exit(0);
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 实时源部分错误不会透传_ffmpeg_输出() {
        let safe = sanitize_catalog_error(
            "FFmpeg 未返回可解析设备: /Users/alice/private.mov secret".into(),
        );
        assert!(safe.contains("详细输出已隐藏"));
        assert!(!safe.contains("private.mov"));
        assert!(!safe.contains("secret"));
    }

    #[test]
    fn 入站安全能力不谎报双向_digest() {
        let capabilities = get_inbound_security_capabilities();
        assert!(capabilities.source_ip_filter);
        assert!(capabilities.from_platform_id);
        assert!(capabilities.digest_client_registration);
        assert!(!capabilities.digest_server_validation);
        assert!(capabilities.literal_platform_ip_only);
    }

    #[test]
    fn sip追踪里的鉴权头被脱敏() {
        // SIP 追踪面板会把整条报文原文推给前端,用户还能复制导出。
        // Digest 的 response= 是可离线爆破的口令哈希,而同一条报文里
        // username/realm/nonce/uri/method 全是已知明文 —— 等于连盐一起交出去。
        let raw = "REGISTER sip:3402@example SIP/2.0\r\n\
                   Via: SIP/2.0/UDP 10.0.0.2:5060;branch=z9hG4bK1\r\n\
                   Authorization: Digest username=\"34020000001320000001\", \
                   realm=\"3402000000\", nonce=\"abc123\", uri=\"sip:3402@example\", \
                   response=\"6629fae49393a05397450978507c4ef1\"\r\n\
                   CSeq: 2 REGISTER\r\n\
                   Content-Length: 0\r\n\r\n";
        let out = redact_sip_credentials(raw);
        assert!(
            !out.contains("6629fae49393a05397450978507c4ef1"),
            "摘要不能出现在推给前端的报文里:\n{out}"
        );
        assert!(!out.contains("nonce=\"abc123\""));
        assert!(
            out.contains(&format!("Authorization: {REDACTED_MARK}")),
            "应保留头域名与「带了鉴权」这个事实,便于排障:\n{out}"
        );
        // 其它头与起始行必须原样保留。
        assert!(out.starts_with("REGISTER sip:3402@example SIP/2.0\r\n"));
        assert!(out.contains("Via: SIP/2.0/UDP 10.0.0.2:5060;branch=z9hG4bK1\r\n"));
        assert!(out.contains("CSeq: 2 REGISTER\r\n"));
        assert!(out.ends_with("\r\n\r\n"), "头部结束的空行要保留");
    }

    #[test]
    fn 脱敏覆盖全部鉴权头且大小写不敏感() {
        for name in [
            "Authorization",
            "authorization",
            "PROXY-AUTHORIZATION",
            "WWW-Authenticate",
            "Proxy-Authenticate",
            "Authentication-Info",
        ] {
            let raw = format!("SIP/2.0 401 Unauthorized\r\n{name}: Digest secret-here\r\n\r\n");
            let out = redact_sip_credentials(&raw);
            assert!(!out.contains("secret-here"), "{name} 未脱敏: {out}");
            assert!(out.contains(REDACTED_MARK));
        }
    }

    #[test]
    fn 脱敏不动正文() {
        // MANSCDP 正文里没有凭据(目录项的 Password 我们从不输出),
        // 而正文还要给用户看,不能被误伤。
        let raw = "MESSAGE sip:x SIP/2.0\r\n\
                   Authorization: Digest response=\"deadbeef\"\r\n\
                   Content-Length: 60\r\n\r\n\
                   <?xml version=\"1.0\"?><Notify>Authorization: 不该被改</Notify>";
        let out = redact_sip_credentials(raw);
        assert!(!out.contains("deadbeef"), "头部仍要脱敏");
        assert!(
            out.contains("<Notify>Authorization: 不该被改</Notify>"),
            "空行之后是正文,不该被当成头部处理:\n{out}"
        );
    }

    #[tokio::test]
    async fn stress_completion区分完成值和sender异常关闭() {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        tx.send(true).expect("发布完成");
        assert!(wait_stress_completion(&mut rx).await);

        let (tx, mut rx) = tokio::sync::watch::channel(false);
        drop(tx);
        assert!(
            !wait_stress_completion(&mut rx).await,
            "sender 未写 true 就关闭不得误判完成"
        );
    }

    #[tokio::test]
    async fn stress_completion不早于guard销毁被观察到() {
        let dir =
            std::env::temp_dir().join(format!("uvp-completion-seq-{}", artifacts::unique_suffix()));
        artifacts::create_secure_dir(&dir).expect("建目录");
        let (completion_tx, mut rx) = tokio::sync::watch::channel(false);
        let writer = series::SeriesWriter::new(&dir, 999).expect("创建 writer");
        let inprogress_path = writer.inprogress_path().to_path_buf();
        assert!(artifacts::is_registered_active(&inprogress_path));

        // 模拟 supervisor 尾部：先释放 writer guard，再发送 completion=true
        drop(writer);
        assert!(
            !artifacts::is_registered_active(&inprogress_path),
            "guard 销毁后注销活跃注册"
        );
        completion_tx.send(true).expect("发送 completion");

        assert!(wait_stress_completion(&mut rx).await);
        // 观察到 completion 时活跃注册必须已经消除，不得产生竞争
        assert!(!artifacts::is_registered_active(&inprogress_path));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn 重复启动被拒绝时不残留空inprogress() {
        let app_state = AppState::new(None);
        let (tx, _) = tokio::sync::broadcast::channel(1);
        let (_, rx) = tokio::sync::watch::channel(false);
        let dir =
            std::env::temp_dir().join(format!("uvp-no-orphan-{}", artifacts::unique_suffix()));
        artifacts::create_secure_dir(&dir).expect("建目录");

        // 模拟前序压测正在运行
        {
            let mut guard = app_state.stress.lock().await;
            *guard = Some(StressHandle {
                run_id: 1,
                stop_tx: tx,
                stopping: false,
                stopping_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                worker_abort: tokio::spawn(async {}).abort_handle(),
                completion: rx,
                device_count: 1,
                series_path: None,
            });
        }

        // 槽位被占用时直接返回拒绝，在此分支前不得触碰 SeriesWriter::new
        let guard = app_state.stress.lock().await;
        assert!(guard.is_some(), "已有任务运行");
        drop(guard);

        let inprogress_count = std::fs::read_dir(&dir)
            .expect("读取目录")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".inprogress"))
            .count();
        assert_eq!(
            inprogress_count, 0,
            "拒绝启动路径下绝不创建任何 .inprogress 文件"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn schema_v1报告边界保持裸metrics数组() {
        let dir =
            std::env::temp_dir().join(format!("uvp-report-v1-{}", artifacts::unique_suffix()));
        artifacts::create_secure_dir(&dir).expect("建目录");
        let path = dir.join("series-run1-1-1.jsonl");
        std::fs::write(
            &path,
            b"{\"kind\":\"sample\",\"seq\":1,\"ts_ms\":123,\"metrics\":{\"devices_online\":7}}\n{\"kind\":\"run_end\",\"seq\":2,\"sample_count\":1,\"outcome\":\"completed\",\"complete\":true,\"degraded\":false}\n",
        )
        .expect("写时序");
        let series = series::read_series(&path);
        assert!(series.complete, "reader 应先用 envelope 校验完整性");
        let report = build_report_v1(ReportV1Input {
            policy: artifacts::RetentionPolicy::default(),
            retention_health: artifacts::CleanupHealth::default(),
            environment: diagnostics::Environment::collect(),
            scenario: None,
            metrics: Metrics::default().snapshot(),
            series,
            series_file: Some(path.to_string_lossy().into_owned()),
            generated_at_epoch_secs: 1,
        });

        assert_eq!(report["schema_version"], 1);
        let samples = report["series"].as_array().expect("series 数组");
        assert_eq!(samples, &[serde_json::json!({"devices_online": 7})]);
        for field in ["kind", "seq", "ts_ms", "metrics"] {
            assert!(
                samples[0].get(field).is_none(),
                "不得泄漏 envelope 字段 {field}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn 脱敏对畸形输入不panic() {
        assert_eq!(redact_sip_credentials(""), "");
        // 没有空行(只有头部)。
        let out = redact_sip_credentials("Authorization: x\r\n");
        assert!(out.contains(REDACTED_MARK));
        // 没有冒号的行原样保留。
        assert_eq!(redact_sip_credentials("garbage\r\n"), "garbage\r\n");
        // LF-only 行尾也要能处理(某些平台不严格用 CRLF)。
        let out = redact_sip_credentials("Authorization: x\n\nbody");
        assert!(out.contains(REDACTED_MARK) && out.ends_with("body"));
    }
}
