//! Tauri 桌面端后端库。
//!
//! 职责:注册前端可调用的命令(IPC),第一阶段内嵌 `stress-engine`。
//! 后续可把引擎拆为独立进程,这里改为 HTTP/WS 客户端。

use stress_engine::Metrics;

/// 返回引擎信息 —— 前端"引擎连通性自检"调用,验证 IPC 通路。
#[tauri::command]
fn engine_version() -> String {
    // 触碰一下引擎类型,确保内嵌链路真实可用(而非仅返回字面量)。
    let _ = Metrics::default();
    format!("stress-engine v{} 就绪", env!("CARGO_PKG_VERSION"))
}

/// 应用入口:初始化日志,注册命令,启动 Tauri 事件循环。
pub fn run() {
    common::logging::init();
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![engine_version])
        .run(tauri::generate_context!())
        .expect("Tauri 应用启动失败");
}
