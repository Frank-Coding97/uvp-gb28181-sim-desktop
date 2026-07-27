//! Tauri 桌面端后端库(v2 M2)。
//!
//! 结构:
//! - `daemon` 模块:管理 `uvp-daemon --stdio` 子进程 + JSON-RPC IPC。
//! - 3 个 command (start_device / stop_device / get_device_status) 转发到 daemon。
//! - `#[tauri::command]` 侧只做参数透传 + 错误映射,业务逻辑全在 daemon (Go)。

mod daemon;

use anyhow::Result;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Manager, RunEvent, State};

use crate::daemon::Daemon;

/// Tauri managed state:daemon 句柄。
///
/// 用 `Arc<Option<Daemon>>` 是因为 spawn 是 async,initialize 前 state 为 None。
/// 实际上 setup 里就 spawn 好了,前端调 command 时基本上不会遇到 None(除非 spawn 失败)。
struct DaemonState(Arc<tokio::sync::RwLock<Option<Arc<Daemon>>>>);

impl DaemonState {
    fn new() -> Self {
        Self(Arc::new(tokio::sync::RwLock::new(None)))
    }

    async fn get(&self) -> Result<Arc<Daemon>, String> {
        let guard = self.0.read().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "daemon 未启动".to_string())
    }
}

// ──────── Tauri Commands ────────

/// 启动设备:把前端的 SipConfig 透传给 daemon。
#[tauri::command]
async fn start_device(config: Value, state: State<'_, DaemonState>) -> Result<Value, String> {
    let daemon = state.get().await?;
    daemon
        .call("start_device", config)
        .await
        .map(|resp| resp.get("result").cloned().unwrap_or(resp))
        .map_err(|e| e.to_string())
}

/// 停止设备。
#[tauri::command]
async fn stop_device(state: State<'_, DaemonState>) -> Result<Value, String> {
    let daemon = state.get().await?;
    daemon
        .call("stop_device", json!({}))
        .await
        .map(|resp| resp.get("result").cloned().unwrap_or(resp))
        .map_err(|e| e.to_string())
}

/// 查询设备当前状态(启动时前端会调一次做对账)。
#[tauri::command]
async fn get_device_status(state: State<'_, DaemonState>) -> Result<Value, String> {
    let daemon = state.get().await?;
    daemon
        .call("get_device_status", json!({}))
        .await
        .map(|resp| resp.get("result").cloned().unwrap_or(resp))
        .map_err(|e| e.to_string())
}

// ──────── App 启动 ────────

/// Tauri 应用入口。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 初始化 tracing,写到 stderr,便于开发时看日志
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,uvp_desktop_lib=debug")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(DaemonState::new())
        .setup(|app| {
            let handle = app.handle().clone();
            // 拉起 daemon 子进程(async)
            tauri::async_runtime::spawn(async move {
                if let Err(err) = init_daemon(handle).await {
                    tracing::error!("failed to init daemon: {err:?}");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_device,
            stop_device,
            get_device_status,
        ])
        .build(tauri::generate_context!())
        .expect("Tauri 应用启动失败")
        .run(|handle, event| {
            if let RunEvent::ExitRequested { .. } | RunEvent::Exit = event {
                let handle = handle.clone();
                // 优雅关闭 daemon
                tauri::async_runtime::block_on(async move {
                    if let Some(state) = handle.try_state::<DaemonState>() {
                        if let Ok(daemon) = state.get().await {
                            daemon.shutdown().await;
                        }
                    }
                });
            }
        });
}

/// 定位 daemon 二进制并 spawn。
async fn init_daemon(app: AppHandle) -> Result<()> {
    let path = locate_daemon_binary(&app)?;
    let daemon = Daemon::spawn(path, app.clone()).await?;
    let state: State<DaemonState> = app.state();
    *state.0.write().await = Some(Arc::new(daemon));
    tracing::info!("daemon ready");
    Ok(())
}

/// 查找 daemon 二进制路径。
///
/// 顺序:
///   1. 环境变量 UVP_DAEMON_PATH(dev / 手工调试用)
///   2. 相对可执行文件所在目录 ./uvp-daemon (打包后 sidecar 位置)
///   3. Cargo workspace 相对 apps/daemon/uvp-daemon(dev 模式)
fn locate_daemon_binary(app: &AppHandle) -> Result<PathBuf> {
    if let Ok(env_path) = std::env::var("UVP_DAEMON_PATH") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return Ok(p);
        }
        anyhow::bail!("UVP_DAEMON_PATH set but file missing: {}", p.display());
    }

    // 打包后:与 Tauri 可执行文件同目录
    if let Ok(exe_dir) = std::env::current_exe().and_then(|p| p.parent().map(|p| p.to_path_buf()).ok_or(std::io::Error::from(std::io::ErrorKind::NotFound))) {
        let candidate = exe_dir.join(daemon_binary_name());
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    // dev 模式:apps/daemon/uvp-daemon (相对项目根)
    // 从 Tauri resource dir 逆推:通常是 apps/desktop/src-tauri/target/debug/uvp-desktop
    // 项目根 = target/../../.. (target/debug -> src-tauri -> desktop -> apps -> root)
    if let Some(resource_dir) = app
        .path()
        .resource_dir()
        .ok()
        .and_then(|p| p.parent().map(|q| q.to_path_buf()))
    {
        // 尝试各种可能的 dev 布局
        let candidates = [
            resource_dir.join("../../../daemon").join(daemon_binary_name()),
            resource_dir.join("../../daemon").join(daemon_binary_name()),
        ];
        for c in candidates.iter() {
            if c.exists() {
                return Ok(c.clone());
            }
        }
    }

    // 最后兜底:从 CARGO_MANIFEST_DIR (仅 dev, build 时嵌入)
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let candidate =
        PathBuf::from(manifest_dir).join("../../daemon").join(daemon_binary_name());
    if candidate.exists() {
        return Ok(candidate);
    }

    anyhow::bail!(
        "找不到 uvp-daemon 可执行文件。请设 UVP_DAEMON_PATH 或先 build apps/daemon (go build -o apps/daemon/uvp-daemon ./cmd/uvp-daemon)"
    )
}

fn daemon_binary_name() -> &'static str {
    if cfg!(windows) {
        "uvp-daemon.exe"
    } else {
        "uvp-daemon"
    }
}
