//! 设备运行事件观察者。
//!
//! 解耦"设备状态机产生事件"与"压测引擎聚合指标":设备层(gb28181-simulator)
//! 只依赖本 trait 上报事件,聚合方(stress-engine::Metrics)实现它。放在 common
//! 最底层,避免下层 crate 反向依赖上层。

/// 设备运行过程中的关键事件。
#[derive(Debug, Clone)]
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
    /// 平台调用预置位(转到某预置位),携带设备当前登记的国标 PresetName。
    PtzPresetCall { preset: u8, name: String },
    /// 平台下发 OSD 配置命令(DeviceConfig + OSDConfig),设备已应用。
    /// 供 UI 展示"平台设置了 OSD、设备已按其设置"(国标 A.2.3.2.11)。
    OsdConfig {
        /// 时间显示开关。
        time_show: bool,
        /// OSD 信息(通道名等)显示开关。
        osd_show: bool,
    },
    /// 平台命令语义事件(供 UI"平台命令时间线"展示):
    /// 平台下发了什么、设备回了什么,人类可读。
    PlatformCommand {
        /// 命令类别(如 Catalog/DeviceInfo/PTZ/Invite/OSD 等,用于图标/分类)。
        kind: String,
        /// 人类可读摘要(如"查询目录 → 回 8 通道"、"PTZ 左转")。
        summary: String,
    },
    /// 订阅状态变化(供 UI"活跃订阅面板"):建立/清理/收到 NOTIFY 计数。
    SubscriptionChanged {
        /// 订阅类别(Catalog/Alarm/MobilePosition/PTZPosition)。
        kind: String,
        /// 是否活跃(false=已清理)。
        active: bool,
        /// 累计已发送 NOTIFY 次数。
        notify_count: u32,
    },
    /// 长任务进度(供 UI 进度条):抓拍上传 / 在线升级。
    Progress {
        /// 任务类别(snapshot/upgrade)。
        kind: String,
        /// 当前步/张。
        current: u32,
        /// 总步/张数。
        total: u32,
        /// 百分比(0-100)。
        percent: u32,
    },
    /// 运行时错误(供 UI 全局错误条):设备上线后异步流程里的失败,
    /// 如点播(INVITE)时视频源采集失败——这类错误发生在后台任务中,
    /// 不经 start_device 的返回值,必须靠事件推给前端,否则用户界面一片安静。
    RuntimeError {
        /// 错误场景(如 invite/stream/capture),供 UI 分类/图标。
        scope: String,
        /// 人类可读的错误详情。
        message: String,
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
