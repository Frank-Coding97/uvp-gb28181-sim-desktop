//! 单设备 GB28181 仿真:把 SIP 栈、GB 协议、媒体引擎组合成一台完整的下级设备。
//!
//! 一个 [`device::DeviceSimulator`] = 一台虚拟 IPC 的状态机。M1 实现注册(含 Digest)
//! 与心跳保活;目录/点播/媒体在 M2 接入。压测调度器批量创建并驱动它们。

pub mod builder;
pub mod device;

pub use device::{DeviceConfig, DeviceSimulator, DeviceState};

