use ipc_error::{IpcError, IpcResult};

// Tauri 2 emit/state 需要对应 trait

use super::*;

/// 引擎连通性自检（M0 已有，保留）。
#[tauri::command]
pub(crate) fn engine_version() -> String {
    format!("stress-engine v{} 就绪", env!("CARGO_PKG_VERSION"))
}

/// 入站信令安全能力。静态显式暴露边界，避免 UI 把“严格模式”误解为双向 Digest。
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub(crate) struct InboundSecurityCapabilities {
    pub(crate) source_ip_filter: bool,
    pub(crate) from_platform_id: bool,
    pub(crate) digest_server_validation: bool,
    pub(crate) digest_client_registration: bool,
    pub(crate) literal_platform_ip_only: bool,
}

#[tauri::command]
pub(crate) fn get_inbound_security_capabilities() -> InboundSecurityCapabilities {
    InboundSecurityCapabilities {
        source_ip_filter: true,
        from_platform_id: true,
        digest_server_validation: false,
        digest_client_registration: true,
        literal_platform_ip_only: true,
    }
}

/// 导出诊断包(日志 + 运行环境快照),返回 zip 路径。
///
/// 与「导出压测报告」分开:报告回答"这次压测结果如何",诊断包回答
/// "这台机器上到底发生了什么"。后者才是把问题发给技术支持时需要的东西。
#[tauri::command]
pub(crate) async fn export_diagnostics(
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
