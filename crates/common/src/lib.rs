//! 公共设施 crate。
//!
//! 存放跨模块共享的基础类型:统一错误、领域标识(设备 ID)、传输类型、日志初始化。
//! 本 crate 不依赖任何其它业务 crate,处于依赖树最底层。

pub mod clock;
pub mod error;
pub mod logging;
pub mod ntp;
pub mod observer;
pub mod process;
pub mod types;

pub use error::{Error, Result};
pub use observer::{DeviceEvent, DeviceObserver, FailureKind, NoopObserver};
pub use process::hidden_command;
pub use types::{AudioTransport, DeviceId, GbVersion, SignalingEncoding, Transport};
