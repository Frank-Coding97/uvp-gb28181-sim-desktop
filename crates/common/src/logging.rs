//! 日志初始化。
//!
//! 统一用 `tracing`。通过 `RUST_LOG` 环境变量控制级别,缺省 `info`。
//! 引擎与 CLI 在启动时调用一次 [`init`]。

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// 初始化全局日志订阅器。重复调用安全(忽略二次初始化错误)。
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false))
        .try_init();
}
