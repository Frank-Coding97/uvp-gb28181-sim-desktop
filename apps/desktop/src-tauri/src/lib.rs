//! Tauri 桌面端后端库(v2 从零重写)。

/// 应用全局状态(暂为空壳,按需扩展)。
struct AppState {}

impl AppState {
    fn new() -> Self {
        AppState {}
    }
}

/// Tauri 应用入口。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![])
        .run(tauri::generate_context!())
        .expect("Tauri 应用启动失败");
}
