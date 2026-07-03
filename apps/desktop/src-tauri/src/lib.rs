//! Tauri 桌面端后端库。
//!
//! 按 docs/20-architecture/api-contract.md 注册 IPC 命令。
//! 第一阶段内嵌 stress-engine；后续可拆为独立进程。

use std::sync::Arc;

use stress_engine::{Metrics, Orchestrator};
use tauri::Emitter; // Tauri 2 emit 需要此 trait
use tokio::sync::{broadcast, Mutex};

/// 应用全局状态（托管在 Tauri managed state）。
struct AppState {
    /// 当前压测的停止发射端；None 表示空闲。
    stop_tx: Mutex<Option<broadcast::Sender<()>>>,
    /// 当前压测的实时指标；None 表示空闲。
    metrics: Mutex<Option<Arc<Metrics>>>,
}

impl AppState {
    fn new() -> Self {
        AppState {
            stop_tx: Mutex::new(None),
            metrics: Mutex::new(None),
        }
    }
}

// ── IPC 命令 ─────────────────────────────────────────────

/// 引擎连通性自检（M0 已有，保留）。
#[tauri::command]
fn engine_version() -> String {
    format!("stress-engine v{} 就绪", env!("CARGO_PKG_VERSION"))
}

/// 校验场景 YAML/TOML 并返回摘要，不启动压测。
#[tauri::command]
async fn validate_scenario(toml: String) -> Result<ScenarioSummary, String> {
    let sc = scenario::LinearScenario::from_toml_str(&toml)
        .map_err(|e| e.to_string())?;
    Ok(ScenarioSummary {
        name: sc.device_info.device_name.clone(),
        device_count: 0, // 由前端传 count 参数
        server_host: sc.server_host.clone(),
        server_port: sc.server_port,
    })
}

/// 启动压测。`toml` 为场景内容，`count` 为设备数。
/// 成功后每秒向前端发送 `metrics_tick` 事件。
#[tauri::command]
async fn start_stress(
    toml: String,
    count: usize,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    let mut stop_guard = state.stop_tx.lock().await;
    if stop_guard.is_some() {
        return Err("已有压测在运行，请先停止".into());
    }

    let sc = scenario::LinearScenario::from_toml_str(&toml)
        .map_err(|e| e.to_string())?;
    let orch = Orchestrator::new(&sc, count)
        .map_err(|e| e.to_string())?;

    let (tx, _) = broadcast::channel::<()>(1);
    *stop_guard = Some(tx.clone());

    let metrics = Arc::clone(&orch.metrics);
    *state.metrics.lock().await = Some(Arc::clone(&metrics));
    drop(stop_guard);

    // 压测任务。
    let tx2 = tx.clone();
    tokio::spawn(async move {
        let _ = orch.run(tx2).await;
    });

    // 每秒推送指标快照给前端。
    let app2 = app.clone();
    let m2 = Arc::clone(&metrics);
    let mut tick_stop = tx.subscribe();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = tick_stop.recv() => break,
                _ = interval.tick() => {
                    let snap = m2.snapshot();
                    if let Ok(json) = serde_json::to_string(&snap) {
                        let _ = app2.emit("metrics_tick", json);
                    }
                }
            }
        }
    });

    Ok(format!("已启动 {count} 台设备压测"))
}

/// 停止当前压测。
#[tauri::command]
async fn stop_stress(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let mut stop_guard = state.stop_tx.lock().await;
    match stop_guard.take() {
        Some(tx) => {
            let _ = tx.send(());
            *state.metrics.lock().await = None;
            Ok("压测已停止".into())
        }
        None => Err("没有正在运行的压测".into()),
    }
}

/// 查询当前指标快照（供按需轮询；实时更新通过 metrics_tick 事件）。
#[tauri::command]
async fn get_metrics(state: tauri::State<'_, AppState>) -> Result<String, String> {
    match &*state.metrics.lock().await {
        Some(m) => serde_json::to_string(&m.snapshot()).map_err(|e| e.to_string()),
        None => Err("无正在运行的压测".into()),
    }
}

// ── 数据传输对象 ──────────────────────────────────────────

#[derive(serde::Serialize)]
struct ScenarioSummary {
    name: String,
    device_count: usize,
    server_host: String,
    server_port: u16,
}

// ── 应用入口 ──────────────────────────────────────────────

/// 初始化日志、注册命令、启动 Tauri 事件循环。
pub fn run() {
    common::logging::init();
    tauri::Builder::default()
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            engine_version,
            validate_scenario,
            start_stress,
            stop_stress,
            get_metrics,
        ])
        .run(tauri::generate_context!())
        .expect("Tauri 应用启动失败");
}
