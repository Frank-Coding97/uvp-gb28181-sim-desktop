//! 设备运行事件观察者。
//!
//! 解耦"设备状态机产生事件"与"压测引擎聚合指标":设备层(gb28181-simulator)
//! 只依赖本 trait 上报事件,聚合方(stress-engine::Metrics)实现它。放在 common
//! 最底层,避免下层 crate 反向依赖上层。

/// 设备运行过程中的关键事件。
#[derive(Debug, Clone, Copy)]
pub enum DeviceEvent {
    /// 发起一次注册。
    RegisterAttempt,
    /// 注册成功。
    RegisterSuccess,
    /// 注册失败(带归因)。
    RegisterFailure(FailureKind),
    /// 心跳成功。
    HeartbeatOk,
    /// 心跳失败。
    HeartbeatFail,
    /// 推流开始。
    StreamStart,
    /// 推流结束。
    StreamStop,
    /// 平台下发云台控制(供 UI 云台动画)。字段为方向/变倍开关 + 速度。
    /// 全 false 表示停止。用原始字段而非 protocol 层类型,避免 common 反向依赖。
    Ptz {
        up: bool,
        down: bool,
        left: bool,
        right: bool,
        zoom_in: bool,
        zoom_out: bool,
        pan_speed: u8,
        tilt_speed: u8,
        zoom_speed: u8,
    },
}

/// 失败归因(与 docs/20-architecture/data-model.md#4 对齐)。
#[derive(Debug, Clone, Copy)]
pub enum FailureKind {
    /// 事务超时无响应。
    Timeout,
    /// 平台拒绝(4xx/5xx)。
    Rejected,
    /// 网络/其它。
    Other,
}

/// 设备事件观察者。设备层持有 `Arc<dyn DeviceObserver>` 并在关键节点回调。
pub trait DeviceObserver: Send + Sync {
    /// 收到一个设备事件。实现方须快速返回(热路径),不做阻塞操作。
    fn on_event(&self, event: DeviceEvent);
}

/// 空观察者:忽略所有事件(单设备/测试用)。
pub struct NoopObserver;

impl DeviceObserver for NoopObserver {
    fn on_event(&self, _event: DeviceEvent) {}
}
