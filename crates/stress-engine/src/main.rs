//! stress-engine 独立二进制入口。
//!
//! M0:初始化日志并打印占位信息。M3 起:读取场景 YAML → 起调度器 →
//! 暴露本地 HTTP/WS 控制接口(供桌面 UI 或 CLI 使用)。

fn main() {
    common::logging::init();
    tracing::info!("UVP GB28181 压测引擎(骨架)。M3 起接入场景调度与服务层。");
}
