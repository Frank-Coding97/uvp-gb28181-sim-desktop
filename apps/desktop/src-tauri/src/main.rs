// Tauri 桌面端二进制入口。发布模式下隐藏 Windows 控制台窗口。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    uvp_desktop_lib::run();
}
