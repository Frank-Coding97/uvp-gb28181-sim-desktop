//! 单设备 GB28181 仿真:把 SIP 栈、GB 协议、媒体引擎组合成一台完整的下级设备。
//!
//! 一个 [`device::DeviceSimulator`] = 一台虚拟 IPC 的状态机。M1 实现注册(含 Digest)
//! 与心跳保活;目录/点播/媒体在 M2 接入。压测调度器批量创建并驱动它们。

pub mod builder;
pub mod device;

pub use device::{
    BroadcastSnapshot, ChannelConfig, DeviceConfig, DeviceInfo, DeviceSimulator, DeviceState,
    MediaTelemetrySnapshot, RuntimeParams, CATALOG_NOTIFY_BATCH_ITEMS, CATALOG_NOTIFY_DEBOUNCE,
    CATALOG_NOTIFY_MAX_DELAY, DEFAULT_REGISTER_EXPIRES, DEFAULT_REGISTER_RETRY_MAX_SECS,
};

// 转出媒体侧的视频源预处理(桌面在设备上线前预热归一化缓存,避免 INVITE 阻塞)。
pub use media_rtp::{prepare_video_source, prepare_video_source_at};
