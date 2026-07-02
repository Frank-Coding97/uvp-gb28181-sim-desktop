//! 单设备 GB28181 仿真:把 SIP 栈、GB 协议、媒体引擎组合成一台完整的下级设备。
//!
//! 一个 [`device::DeviceSimulator`] = 一台虚拟 IPC 的完整状态机
//! (注册 / 心跳 / 目录 / 设备信息 / 点播 / 报警)。压测调度器批量创建并驱动它们。
//!
//! M0 为可编译骨架,仅定义配置与状态机外壳。

pub mod device;

pub use device::{DeviceConfig, DeviceSimulator};
