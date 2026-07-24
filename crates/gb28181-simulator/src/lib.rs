//! 单设备 GB28181 仿真:把 SIP 栈、GB 协议、媒体引擎组合成一台完整的下级设备。
//!
//! 一个 [`device::DeviceSimulator`] = 一台虚拟 IPC 的状态机。M1 实现注册(含 Digest)
//! 与心跳保活;目录/点播/媒体在 M2 接入。压测调度器批量创建并驱动它们。

pub mod builder;
pub mod device;
pub mod home_position;

pub use device::{ChannelConfig, DeviceConfig, DeviceInfo, DeviceSimulator, DeviceState};
pub use home_position::{
    fixed_cases, FaultProfile, MatrixCase, MatrixReport, MatrixTransport, QueryMode,
    HOME_POSITION_SCENARIO_IDS, MATRIX_SCHEMA_VERSION,
};

// 转出媒体侧的视频源预处理(桌面在设备上线前预热容器转封装缓存,避免 INVITE 阻塞)。
pub use media_rtp::prepare_video_source;
