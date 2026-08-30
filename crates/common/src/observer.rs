//! 设备运行事件观察者。
//!
//! 解耦"设备状态机产生事件"与"压测引擎聚合指标":设备层(gb28181-simulator)
//! 只依赖本 trait 上报事件,聚合方(stress-engine::Metrics)实现它。放在 common
//! 最底层,避免下层 crate 反向依赖上层。

/// 设备运行过程中的关键事件。
#[derive(Debug, Clone)]
pub enum DeviceEvent {
    /// 发起一次注册。`retry=true` 表示这是退避重试或掉线重注册,不是首次尝试。
    ///
    /// 区分首次与重试很重要:把重试也算进"尝试"会让成功率变成**尝试维度**而非
    /// **设备维度**,长时间运行会被重试稀释,看不出真实的上线比例。
    RegisterAttempt { retry: bool },
    /// 注册成功,携带整条注册事务的往返耗时(毫秒)。
    RegisterSuccess { rtt_ms: u64 },
    /// 注册失败(带归因与设备 ID)。
    ///
    /// 带 `device_id` 是为了让压测报告能给出**失败样本** —— 排障要挑一台具体设备
    /// 复现,而之前只有一个总计数,拿不到任何一台可复现的设备编号。
    RegisterFailure {
        /// 失败归因。
        kind: FailureKind,
        /// 失败设备的国标编号。
        device_id: String,
    },
    /// 心跳成功,携带往返耗时(毫秒)。
    HeartbeatOk { rtt_ms: u64 },
    /// 心跳失败。
    HeartbeatFail,
    /// 设备在线状态翻转(用于"当前在线设备数"这个 gauge)。
    ///
    /// 只在状态真的变化时上报,因此聚合方可以直接 +1/-1 而不需要按设备去重。
    OnlineChanged { online: bool },
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// 事务超时无响应。
    Timeout,
    /// 平台以非 2xx 最终响应拒绝,携带状态码。
    ///
    /// 状态码必须带上:401/403 是凭据或权限问题(再加并发也没用),
    /// 486/503 才是承载力信号。之前全被压成同一个 `Rejected`,
    /// 报告里看不出该去修配置还是该降并发。
    Rejected {
        /// SIP 最终响应状态码。
        status: u16,
    },
    /// **本地**发送失败(ENOBUFS、socket 错误、队列满)。
    ///
    /// 单独一类是为了压测结论可信:本机瓶颈导致的失败绝不能算成"平台承载不住"。
    Local,
    /// 网络/其它。
    Other,
}

impl FailureKind {
    /// 平台拒绝时按**处置动作**分档,而不是逐个状态码统计。
    ///
    /// 分档的依据是"看到这个数该做什么":凭据问题去改配置,
    /// 过载问题去降并发,其它需要人看一眼原因短语。
    pub fn reject_bucket(&self) -> Option<RejectBucket> {
        match self {
            FailureKind::Rejected { status } => Some(RejectBucket::of(*status)),
            _ => None,
        }
    }
}

/// 平台拒绝的处置分档。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectBucket {
    /// 401 / 403:凭据或权限问题。加并发不会变好,要去核对用户名/密码/域。
    Auth,
    /// 486 / 503 / 480:平台忙、过载或暂时不可用 —— 这才是承载力信号。
    Overload,
    /// 其它 4xx/5xx:需要看原因短语,常见是编号不被平台接受(404)。
    Other,
}

impl RejectBucket {
    /// 按状态码分档。
    pub fn of(status: u16) -> Self {
        match status {
            401 | 403 | 407 => RejectBucket::Auth,
            480 | 486 | 503 | 500 | 504 => RejectBucket::Overload,
            _ => RejectBucket::Other,
        }
    }

    /// 供报告与界面显示的中文名。
    pub fn label(&self) -> &'static str {
        match self {
            RejectBucket::Auth => "凭据/权限",
            RejectBucket::Overload => "平台忙/过载",
            RejectBucket::Other => "其它拒绝",
        }
    }
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
