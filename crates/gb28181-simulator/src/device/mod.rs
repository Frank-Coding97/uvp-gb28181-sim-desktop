//! 单设备仿真:设备配置 + 注册/心跳状态机。
//!
//! 实现 docs/10-functional/device-simulation.md §3.1(注册+Digest)、§3.2(心跳)。
//! 注册闭环:REGISTER(无鉴权)→ 平台 401 挑战 → REGISTER(带 Authorization)→ 200 OK。
//! 注册成功后按 `heartbeat_interval_secs` 周期发送 Keepalive。
mod broadcast;
mod control;
mod invite;
mod notify;
mod query;
mod register;
mod snapshot;
pub(crate) use control::*;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::{DeviceId, Error, Result, Transport};
use sip_core::SipTransport;

use crate::builder::{self, DialogIds};

#[cfg(test)]
mod tests;

/// 单个虚拟设备的静态配置。压测时由 `scenario` 按序号批量生成。
#[derive(Debug, Clone)]
pub struct DeviceConfig {
    /// 本设备国标 ID。
    pub device_id: DeviceId,
    /// SIP 认证用户名(通常等于 device_id)。
    pub username: String,
    /// SIP 认证密码。
    ///
    /// 用 [`common::Secret`] 而不是 `String`:本结构派生了 `Debug`,
    /// 裸 `String` 的话任何一处 `tracing::debug!(?config)` 都会把密码写进
    /// 标准输出、日志文件、内存环形缓冲以及前端可拉取的 `get_recent_logs`。
    pub password: common::Secret,
    /// 上级平台 SIP 服务地址 host。
    pub server_host: String,
    /// 上级平台 SIP 服务端口。
    pub server_port: u16,
    /// 上级平台域(SIP domain / 服务器 ID 中心编码)。
    pub server_domain: String,
    /// 入站 SIP 请求的平台身份校验模式。
    pub inbound_identity_mode: common::InboundIdentityMode,
    /// 信令传输方式。
    pub transport: Transport,
    /// 心跳间隔(秒)。
    pub heartbeat_interval_secs: u64,
    /// 通道列表(目录查询应答用)。
    pub channels: Vec<ChannelConfig>,
    /// 设备信息(DeviceInfo 查询应答用)。
    pub device_info: DeviceInfo,
    /// 视频源文件路径(H.264 Annex B,C 档);None 且无 light_bitrate 时不推流(A 档)。
    pub video_source: Option<String>,
    /// 推流帧率(fps)。
    pub video_fps: u32,
    /// B 档轻量伪流码率(kbps);Some 且无 video_source 时用 LightSource。
    pub light_bitrate_kbps: Option<u32>,
    /// GB28181 协议版本(影响 MANSCDP 应答字段集,2022 默认)。
    pub gb_version: common::GbVersion,
    /// 注册有效期(秒),写入 REGISTER 的 Expires 头。
    ///
    /// 注意这只是**请求值**:平台可以在 200 OK 里授予更短的时长,续约必须以
    /// 平台授予值为准(见 [`DeviceSimulator::granted_expires`])。
    ///
    /// GB/T 28181-2022 §9.1.1 d):缺省宜为 86400s,且**不应短于 3600s**。
    pub register_expires_secs: u32,
    /// 注册失败后的重试间隔上限(秒)。
    ///
    /// §9.1.1 c) 规定"与上一次注册时间间隔应可调,一般情况下不应短于 60s"。
    /// 单设备联调默认 60s 以符合标准;**压测场景会刻意调小**(否则爬坡毫无意义),
    /// 这是有意识的偏离,不是漏实现。
    pub register_retry_max_secs: u64,
    /// 信令字符集编码(§6.10:GB18030 默认;部分平台用 UTF-8)。
    /// 决定 MANSCDP XML 体的字节编码与声明,修复中文乱码。
    pub signaling_encoding: common::SignalingEncoding,
}

/// 默认注册有效期(秒)。§9.1.1 d) 的下限是 3600s,缺省宜 86400s;
/// 这里取下限 3600s 以便联调时更快观察到续约行为。
pub const DEFAULT_REGISTER_EXPIRES: u32 = 3600;

/// 默认注册重试间隔上限(秒),对齐 §9.1.1 c) 的"不应短于 60s"。
pub const DEFAULT_REGISTER_RETRY_MAX_SECS: u64 = 60;

/// 通道配置(对应目录查询中的一个 Item)。
#[derive(Debug, Clone)]
pub struct ChannelConfig {
    /// 通道国标 ID(设备侧通道 ID 规则:设备 ID + 通道序号后缀)。
    pub channel_id: DeviceId,
    /// 通道名称。
    pub name: String,
    /// 状态:ON / OFF。
    pub status: String,
}

/// 设备信息(DeviceInfo 查询应答字段)。
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub device_name: String,
    pub manufacturer: String,
    pub model: String,
    pub firmware: String,
}

/// 设备状态(与 docs/10-functional/device-simulation.md#2、UI DTO 对齐)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    /// 离线。
    Disconnected,
    /// 注册中。
    Registering,
    /// 已注册。
    Registered,
    /// 推流中(M2)。
    InCall,
    /// 注册失败。
    Failed,
}

/// 设备仿真运行时状态机。持有配置 + 会话标识 + CSeq 计数 + 会话状态 + 事件观察者。
pub struct DeviceSimulator {
    config: DeviceConfig,
    ids: DialogIds,
    cseq: AtomicU32,
    /// 当前活跃的推流会话(INVITE → 推流中,BYE → 停止)。
    session: tokio::sync::Mutex<Option<PushSession>>,
    /// 事件观察者(上报注册/心跳/推流事件给压测指标)。默认 Noop。
    observer: Arc<dyn common::DeviceObserver>,
    /// 本端对外信令地址 (host, port),run() 启动时设置;应答 MESSAGE 填 Via/From 用。
    local_addr: std::sync::OnceLock<(String, u16)>,
    /// 当前位置(经度, 纬度)。移动位置订阅的周期 NOTIFY 上报此坐标;
    /// 由 report_position/set_position 更新。默认取一个内置坐标(北京)。
    position: parking_lot::Mutex<(f64, f64)>,
    /// 活跃订阅表(CmdType → 订阅句柄)。平台 SUBSCRIBE 建立,退订/下线时清理。
    subscriptions: tokio::sync::Mutex<HashMap<String, Subscription>>,
    /// 报警订阅对话(平台 `SUBSCRIBE + Alarm` 时记录:对话标识 + 平台地址 + 筛选条件)。
    /// 存在时,report_alarm 走对话内 NOTIFY;否则退化为独立 MESSAGE(如手动触发)。
    ///
    /// 筛选条件来自 A.2.4.6:平台只订阅一级警情时,四级警情不该被推上去。
    alarm_dialog: tokio::sync::Mutex<Option<AlarmSubscription>>,
    /// 目录订阅对话(平台 `SUBSCRIBE + Catalog` 时记录):CRUD 变更后据此发对话内增量 NOTIFY。
    catalog_dialog: tokio::sync::Mutex<Option<(builder::NotifyDialog, SocketAddr)>>,
    /// 预置位表(编号 → 名称)。PTZ 预置位设置/删除命令更新,PresetQuery 返回。
    presets: parking_lot::Mutex<std::collections::BTreeMap<u8, String>>,
    /// 设备控制态(布防/看守位/巡航/精准姿态)。扩展查询与控制命令共同维护(FR-17/18)。
    control_state: parking_lot::Mutex<ControlState>,
    /// 多通道目录树(FR-34)。为空时用 `config.channels` 扁平列表出目录;
    /// 非空时用此树(含业务分组/虚拟组织/报警通道层级)出目录,支持 CRUD + 增量 NOTIFY。
    catalog_tree: parking_lot::Mutex<Vec<gb28181_protocol::id_codec::CatalogNode>>,
    /// 上次推给平台的目录快照(用于计算增量 NOTIFY 的 diff)。
    catalog_snapshot: parking_lot::Mutex<gb28181_protocol::manscdp::CatalogSnapshot>,
    /// 是否已经发起过一次注册尝试(用于区分首次与重试)。
    attempted_once: std::sync::atomic::AtomicBool,
    /// 平台在注册 200 OK 里授予的有效期(秒)。0 = 尚未注册成功。
    ///
    /// 续约周期必须以此为准:平台完全可以只给 120s,而请求写的是 3600s。
    granted_expires: AtomicU32,
    /// 设备是否处于"平台视角的在线"状态。仅用于在状态**翻转**时上报事件,
    /// 让聚合方能维护"当前在线设备数"这个 gauge 而无需按设备去重。
    online: std::sync::atomic::AtomicBool,
    /// 服务端事务表:入站请求按 (branch, CSeq, method) 去重并缓存最终响应。
    ///
    /// 少了这层会出现一个很具体的现场故障:平台 200 OK 丢包后重传 INVITE,
    /// 设备因"已有会话"回 486 Busy Here,点播直接失败。
    server_txns: sip_core::ServerTransactions,
    /// 严格身份模式拒绝计数，用于抽样日志，避免可信网段内的洪泛灌爆日志。
    rejected_identity: AtomicU32,
    /// 当前语音广播会话(§9.12.1)。设备是主叫,同一时刻只维持一路。
    broadcast: tokio::sync::Mutex<Option<BroadcastSession>>,
    /// 广播/对讲媒体流序列号计数器(附录 L 的"媒体流序列号")。
    media_seq: AtomicU32,
    /// 平台经 DeviceConfig/BasicParam 改过的运行时参数(A.2.3.2.1)。
    runtime: parking_lot::Mutex<RuntimeParams>,
    /// 待发送的目录变更缓冲(附录 N 的批量通知)。
    ///
    /// 附录 N:"被订阅域宜在累计一定数量(如 100)条目录变更或者一定时间间隔
    /// (如 1 分钟)后,应将变更事件通知订阅域。" 逐条变更立刻发 NOTIFY 在批量
    /// CRUD(比如换一次 16 通道模板)时会瞬间打出十几条 NOTIFY。
    pending_catalog_changes: parking_lot::Mutex<PendingCatalogChanges>,
    /// 对端(平台)在注册响应的 `X-GB-Ver` 里声明的协议版本(附录 I)。
    ///
    /// None = 平台没声明。附录 I 要求"协议版本更高一方应避免向对方发送不能识别的消息",
    /// 所以这个值决定要不要发 2022 新增的通知(如 A.2.5.8 实时视音频回传通知)。
    peer_version: parking_lot::Mutex<Option<common::GbVersion>>,
}

/// 从 `Contact` 头值里取出可连接的 `IP:端口`(302 重定向用)。
///
/// 形如 `<sip:34020000002000000001@192.168.1.20:5060>` → `192.168.1.20:5060`。
/// 缺端口按 SIP 默认 5060;域名(非字面 IP)返回 None —— 上层会给出明确报错,
/// 而不是静默连到一个猜出来的地址。
/// 该地址是否几乎不可能被公网平台直连(内网 / 环回 / 链路本地)。
///
/// 只在 TCP-PASSIVE(平台主动连设备)下用于给出明确告警:设备在 NAT 后面时,
/// SDP 里回填的就是这类地址,平台永远连不进来,而现场只看到"播放失败"。
fn is_private_or_loopback(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        std::net::IpAddr::V6(v6) => v6.is_loopback(),
    }
}

fn contact_addr(contact: &str) -> Option<SocketAddr> {
    let inside = contact
        .split_once('<')
        .and_then(|(_, rest)| rest.split_once('>'))
        .map(|(uri, _)| uri)
        .unwrap_or(contact);
    let after_scheme = inside
        .strip_prefix("sip:")
        .or_else(|| inside.strip_prefix("sips:"))
        .unwrap_or(inside);
    // 去掉 userinfo 与 URI 参数。
    let host_port = after_scheme
        .rsplit_once('@')
        .map(|(_, hp)| hp)
        .unwrap_or(after_scheme);
    let host_port = host_port.split([';', '?']).next()?.trim();
    if let Ok(addr) = host_port.parse::<SocketAddr>() {
        return Some(addr);
    }
    // 无端口:补 SIP 默认端口 5060。
    format!("{host_port}:5060").parse().ok()
}

/// 单条 MANSCDP body 的安全上限(字节)。
///
/// 目标是让"SIP 头 + body"整体落在 1300 字节的安全 MTU 内:SIP 头(Via/From/To/
/// Call-ID/CSeq/Contact/Content-Type…)通常 300~400 字节,故 body 留 900。
const MAX_MANSCDP_BODY_BYTES: usize = 900;

/// 由注册有效期推导续约周期:取 80%,并夹在 [10s, 有效期-5s] 之间。
///
/// 平台给很短的 Expires(如 30s)时,80% 仍需留出一次事务往返的余量。
fn renew_interval(expires_secs: u32) -> std::time::Duration {
    let expires = expires_secs.max(1) as u64;
    let eighty = (expires * 80 / 100).max(10);
    let safe_ceiling = expires.saturating_sub(5).max(5);
    std::time::Duration::from_secs(eighty.min(safe_ceiling).max(5))
}

/// 设备控制态。由扩展控制命令(布防/看守位/巡航/精准云台)更新,
/// 供扩展查询(AlarmStatus/HomePositionQuery/CruiseTrack*/PTZPosition)读回。
#[derive(Debug, Clone)]
pub struct ControlState {
    /// 是否处于布防/报警态(AlarmStatus 的 DutyStatus)。
    pub alarming: bool,
    /// 是否正在录像(DeviceControl/RecordCmd 维护,DeviceStatus 的 Record 字段读回)。
    pub recording: bool,
    /// 看守位是否启用。
    pub home_enabled: bool,
    /// 是否已设看守位(HomePositionQuery 的 PresetIndex 标志)。
    pub home_set: bool,
    /// 巡航轨迹:轨迹号 → 该轨迹的预置位号列表(增点/删点维护)。
    pub cruise_tracks: std::collections::BTreeMap<u32, Vec<u32>>,
    /// 巡航参数:轨迹号 → (速度, 停留秒数)。GB/T 28181-2022 表 A.9 的 12 位值。
    pub cruise_params: std::collections::BTreeMap<u32, (u32, u32)>,
    /// 自动扫描:扫描组号 → (是否在扫描, 速度, 已设左边界, 已设右边界)。
    /// GB/T 28181-2022 表 A.10;之前 0x89/0x8A 被误当成辅助开关,扫描完全没实现。
    pub scan_groups: std::collections::BTreeMap<u32, ScanGroupState>,
    /// 最近一次精准云台姿态(pan 度, tilt 度, zoom 倍)。
    pub precise_pose: (f32, f32, f32),
}

/// 单个自动扫描组的状态(GB/T 28181-2022 表 A.10)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanGroupState {
    /// 是否正在自动扫描。
    pub scanning: bool,
    /// 扫描速度(12 位)。
    pub speed: u32,
    /// 是否已设左边界。
    pub left_set: bool,
    /// 是否已设右边界。
    pub right_set: bool,
}

impl Default for ControlState {
    fn default() -> Self {
        ControlState {
            alarming: false,
            recording: false,
            home_enabled: true,
            home_set: false,
            cruise_tracks: std::collections::BTreeMap::new(),
            cruise_params: std::collections::BTreeMap::new(),
            scan_groups: std::collections::BTreeMap::new(),
            precise_pose: (0.0, 0.0, 1.0),
        }
    }
}

/// 子任务停止后等待它真正退出的上限。
///
/// 为什么要等而不是发完停止信号就走:发信号只保证"任务下次检查时会退出",
/// 而推流任务一轮可能还在 send 一帧、收流任务可能正阻塞在 recv。
/// 不等的话"设备已下线"这句日志之后,socket 与子进程可能还活着几百毫秒 ——
/// 压测反复启停时这些残留会叠起来,而且**没有任何办法证明它收敛了**。
const TASK_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// 等一个子任务退出,超时则强制 abort。返回是否在超时前自行退出。
///
/// 超时不是错误:媒体发送可能卡在一个不响应的对端上。但它值得记一条日志 ——
/// 频繁超时说明某条停止路径没有真正响应停止信号。
async fn await_task(what: &str, task: tokio::task::JoinHandle<()>) -> bool {
    await_task_with_timeout(what, task, TASK_JOIN_TIMEOUT).await
}

async fn await_task_with_timeout(
    what: &str,
    mut task: tokio::task::JoinHandle<()>,
    timeout: Duration,
) -> bool {
    match tokio::time::timeout(timeout, &mut task).await {
        Ok(Ok(())) => true,
        Ok(Err(error)) if error.is_cancelled() => true,
        Ok(Err(error)) => {
            tracing::warn!(what, %error, "子任务异常结束");
            true
        }
        Err(_) => {
            tracing::warn!(
                what,
                secs = timeout.as_secs_f64(),
                "子任务未在超时内退出,正在强制取消"
            );
            // 丢弃 JoinHandle 只会 detach,不会取消任务。必须 abort 后再次 await，
            // 让推流 future 被 drop，进而必达 LiveSource::drop -> kill+wait FFmpeg。
            task.abort();
            match tokio::time::timeout(Duration::from_secs(1), &mut task).await {
                Ok(Err(error)) if error.is_cancelled() => {}
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(what, %error, "强制取消后的子任务异常"),
                Err(_) => tracing::error!(what, "强制取消后子任务仍未收敛"),
            }
            false
        }
    }
}

/// 活跃的推流会话(推流任务句柄 + 停止信号)。
struct PushSession {
    /// 唯一会话标记，防止已结束的旧任务清理后续新会话。
    marker: Arc<()>,
    /// push_stream 任务。下线时要 join 到它真的退出(见 `await_task`)。
    task: tokio::task::JoinHandle<()>,
    /// 停止信号发送端(drop 或 send 均可触发停流)。
    stop_tx: tokio::sync::oneshot::Sender<()>,
    /// 回放控制(倍速/暂停),供会话内 INFO 调整推流。
    control: Arc<media_rtp::PlaybackControl>,
    /// 是否为回放/下载会话(s=Playback/Download):结束时发 MediaStatus 121。
    is_history: bool,
    /// 附录 K c) 的媒体源标识(`Subject` 的"发送者 ID:发送方序列号");
    /// 平台不带 Subject 时为 None。
    media_source: Option<String>,
    /// 会话内主动发 BYE 所需的对话标识(附录 K a) 的媒体流丢失通知)。
    dialog: builder::InboundDialog,
}

/// 报警订阅:对话标识 + 平台地址 + 筛选条件(A.2.4.6)。
#[derive(Debug, Clone)]
struct AlarmSubscription {
    dialog: builder::NotifyDialog,
    peer: SocketAddr,
    filter: gb28181_protocol::manscdp::AlarmSubscribeFilter,
}

/// 一条活跃订阅(周期 NOTIFY 任务句柄 + 停止信号)。
/// 丢弃或 send 停止信号即终止周期上报任务。
struct Subscription {
    /// 周期 NOTIFY 任务。下线时要 join 到它真的退出(见 `await_task`)。
    task: tokio::task::JoinHandle<()>,
    /// 停止信号发送端。
    stop_tx: tokio::sync::oneshot::Sender<()>,
}

/// 附录 N 的批量通知条数阈值:累计这么多条变更就立刻通知。
pub const CATALOG_NOTIFY_BATCH_ITEMS: usize = 100;
/// 批量通知的去抖窗口:最后一条变更之后静默这么久就发出。
///
/// **有意偏离标准的示例值**:附录 N 举的例子是"一定时间间隔(如 1 分钟)",
/// 那个量级对生产平台的目录同步是合适的,但本项目是**联调工具** ——
/// 工程师在界面上改一个通道名,要能立刻在平台侧看到 NOTIFY,等一分钟没法排障。
/// 去抖同样满足附录 N 的本意(避免大批量变更时逐条推送),只是窗口取秒级。
pub const CATALOG_NOTIFY_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(1);
/// 批量通知的硬上限:第一条待发变更之后最多攒这么久,与附录 N 的示例值一致。
///
/// 变更持续不断(去抖一直被重置)时靠它兜底,保证不会无限期攒着不发。
pub const CATALOG_NOTIFY_MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(60);

/// 待发送的目录变更缓冲(附录 N)。
#[derive(Debug, Default)]
struct PendingCatalogChanges {
    /// 已累计的变更项(按发生顺序;同一通道的后续变更会覆盖前一条)。
    items: Vec<gb28181_protocol::manscdp::CatalogNotifyItem>,
    /// 第一条待发变更的时刻,用于硬上限判定。
    since: Option<std::time::Instant>,
    /// 最后一条变更的时刻,用于去抖判定。
    last: Option<std::time::Instant>,
    /// 是否已安排了延迟发送任务,避免每条变更都 spawn 一个。
    flush_scheduled: bool,
}

impl PendingCatalogChanges {
    /// 合并新变更。同一 DeviceID 只保留最新一条 —— 平台关心的是终态,
    /// 把"上线→下线→上线"三条都推过去只是噪声。
    fn merge(&mut self, changes: Vec<gb28181_protocol::manscdp::CatalogNotifyItem>) {
        for change in changes {
            if let Some(slot) = self
                .items
                .iter_mut()
                .find(|c| c.device_id == change.device_id)
            {
                *slot = change;
            } else {
                self.items.push(change);
            }
        }
        if self.items.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        if self.since.is_none() {
            self.since = Some(now);
        }
        self.last = Some(now);
    }

    /// 是否到了该发的时候:攒够条数、去抖窗口已过、或已到硬上限。
    fn should_flush(&self) -> bool {
        if self.items.is_empty() {
            return false;
        }
        self.items.len() >= CATALOG_NOTIFY_BATCH_ITEMS
            || self
                .last
                .is_some_and(|t| t.elapsed() >= CATALOG_NOTIFY_DEBOUNCE)
            || self
                .since
                .is_some_and(|t| t.elapsed() >= CATALOG_NOTIFY_MAX_DELAY)
    }

    /// 取走全部待发变更。
    fn take(&mut self) -> Vec<gb28181_protocol::manscdp::CatalogNotifyItem> {
        self.since = None;
        self.last = None;
        self.flush_scheduled = false;
        std::mem::take(&mut self.items)
    }
}

/// 平台可经 `DeviceConfig / BasicParam`(A.2.3.2.1)在运行中修改的参数。
///
/// 之前这些字段只在配置里、命令来了只打一行日志就回 OK —— 平台以为改成功了,
/// 设备行为却完全没变(心跳周期照旧、DeviceInfo 里还是老名字、再查配置也读不回来)。
#[derive(Debug, Clone)]
pub struct RuntimeParams {
    /// 设备名称(DeviceInfo / Catalog / ConfigDownload 都读它)。
    pub device_name: String,
    /// 注册有效期(秒)。
    pub expiration_secs: u32,
    /// 心跳间隔(秒)。
    pub heartbeat_interval_secs: u64,
    /// 心跳超时次数(连续失败多少次判掉线并重注册)。
    pub heartbeat_count: u32,
}

impl RuntimeParams {
    /// 用启动配置初始化。心跳超时次数没有独立配置项,沿用文档里的重注册阈值 3
    /// (见 docs/10-functional/device-simulation.md §3.2)。
    fn from_config(config: &DeviceConfig) -> Self {
        RuntimeParams {
            device_name: config.device_info.device_name.clone(),
            expiration_secs: config.register_expires_secs,
            heartbeat_interval_secs: config.heartbeat_interval_secs,
            heartbeat_count: DEFAULT_HEARTBEAT_COUNT,
        }
    }
}

/// 心跳连续失败多少次判定掉线并重注册。平台可经 BasicParam 的 HeartBeatCount 改。
const DEFAULT_HEARTBEAT_COUNT: u32 = 3;

/// 语音广播会话(GB/T 28181-2022 §9.12.1,设备是**语音流接收者**)。
///
/// 设备作主叫发出 INVITE、收 200 OK、发 ACK,然后在本端端口上收平台推来的
/// G.711 音频 RTP(附录 C.2.4)。平台发 BYE 或设备主动挂断时结束。
struct BroadcastSession {
    /// 出站对话(Call-ID / from-tag / 学到的平台 to-tag)。
    dialog: builder::OutboundDialog,
    /// 平台地址(ACK/BYE 的目的地)。
    peer: SocketAddr,
    /// 语音流发送者 ID(SourceID)。
    source_id: String,
    /// 本端收流端口(诊断/前端展示)。
    local_port: u16,
    /// 收流统计(包数/字节/丢包/电平/空闲时长)。
    stats: Arc<media_rtp::RecvStats>,
    /// 停止信号:发送或丢弃即结束收流任务。
    stop_tx: tokio::sync::oneshot::Sender<()>,
    /// 收流任务。下线时要 join 到它真的退出(见 `await_task`)。
    task: tokio::task::JoinHandle<()>,
}

/// 当前推流会话的媒体遥测快照(发送侧统计 + 平台 RTCP 反馈)。
///
/// 设备侧一直只能看到"我发了多少",看不到"平台收到了多少" ——
/// 现场排"画面卡但设备说在发"这类问题时,平台回的 RTCP RR 才是关键证据。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MediaTelemetrySnapshot {
    /// 会话是回放/下载(true)还是实时(false)。
    pub is_history: bool,
    /// 附录 K c) 的媒体源标识(平台带 Subject 时才有)。
    pub media_source: Option<String>,
    /// 本端 RTCP 端口;TCP 模式或未抢到 RTP+1 时为 None,此时收不到 RR。
    pub local_rtcp_port: Option<u16>,
    /// 已发送 RTP 包数。
    pub sent_packets: u64,
    /// 已发送字节数(含 RTP 头)。
    pub sent_bytes: u64,
    /// 已发送 RTCP SR 包数。
    pub sent_rtcp: u64,
    /// 本地发送失败次数。
    pub send_errors: u64,
    /// 本会话采集侧丢弃的帧数(通道满 / 编码器主动跳帧)。
    ///
    /// 与 `send_errors` 是两种不同故障:"发得出去但没东西可发"。
    pub capture_dropped_frames: u64,
    /// 本会话 Annex B 切帧缓冲超限被整体丢弃的字节数(重同步)。
    pub capture_resync_bytes: u64,
    /// 平台回报的收流质量;从未收到 RR 时字段 `reports` 为 0。
    pub feedback: media_rtp::RtcpFeedbackSnapshot,
}

/// 语音广播会话快照(给前端面板)。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BroadcastSnapshot {
    /// 语音流发送者 ID。
    pub source_id: String,
    /// 本端收流端口。
    pub local_port: u16,
    /// 会话 Call-ID。
    pub call_id: String,
    /// 收流统计。
    pub recv: media_rtp::RecvStatsSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InboundIdentityRejection {
    status: u16,
    reason: &'static str,
    detail: &'static str,
}

impl DeviceSimulator {
    /// 用配置创建一个待运行的设备仿真实例(无观察者)。
    pub fn new(config: DeviceConfig) -> Self {
        let runtime = RuntimeParams::from_config(&config);
        Self {
            config,
            ids: DialogIds::new(),
            cseq: AtomicU32::new(1),
            session: tokio::sync::Mutex::new(None),
            observer: Arc::new(common::NoopObserver),
            local_addr: std::sync::OnceLock::new(),
            position: parking_lot::Mutex::new((116.397_428, 39.909_230)),
            subscriptions: tokio::sync::Mutex::new(HashMap::new()),
            alarm_dialog: tokio::sync::Mutex::new(None),
            catalog_dialog: tokio::sync::Mutex::new(None),
            presets: parking_lot::Mutex::new(default_presets()),
            control_state: parking_lot::Mutex::new(ControlState::default()),
            catalog_tree: parking_lot::Mutex::new(Vec::new()),
            catalog_snapshot: parking_lot::Mutex::new(gb28181_protocol::manscdp::CatalogSnapshot {
                channels: std::collections::BTreeMap::new(),
            }),
            attempted_once: std::sync::atomic::AtomicBool::new(false),
            granted_expires: AtomicU32::new(0),
            online: std::sync::atomic::AtomicBool::new(false),
            server_txns: sip_core::ServerTransactions::default(),
            rejected_identity: AtomicU32::new(0),
            broadcast: tokio::sync::Mutex::new(None),
            media_seq: AtomicU32::new(1),
            runtime: parking_lot::Mutex::new(runtime),
            peer_version: parking_lot::Mutex::new(None),
            pending_catalog_changes: parking_lot::Mutex::new(PendingCatalogChanges::default()),
        }
    }

    /// 带事件观察者创建(压测时由编排器注入共享 Metrics)。
    pub fn with_observer(config: DeviceConfig, observer: Arc<dyn common::DeviceObserver>) -> Self {
        let runtime = RuntimeParams::from_config(&config);
        Self {
            config,
            ids: DialogIds::new(),
            cseq: AtomicU32::new(1),
            session: tokio::sync::Mutex::new(None),
            observer,
            local_addr: std::sync::OnceLock::new(),
            position: parking_lot::Mutex::new((116.397_428, 39.909_230)),
            subscriptions: tokio::sync::Mutex::new(HashMap::new()),
            alarm_dialog: tokio::sync::Mutex::new(None),
            catalog_dialog: tokio::sync::Mutex::new(None),
            presets: parking_lot::Mutex::new(default_presets()),
            control_state: parking_lot::Mutex::new(ControlState::default()),
            catalog_tree: parking_lot::Mutex::new(Vec::new()),
            catalog_snapshot: parking_lot::Mutex::new(gb28181_protocol::manscdp::CatalogSnapshot {
                channels: std::collections::BTreeMap::new(),
            }),
            attempted_once: std::sync::atomic::AtomicBool::new(false),
            granted_expires: AtomicU32::new(0),
            online: std::sync::atomic::AtomicBool::new(false),
            server_txns: sip_core::ServerTransactions::default(),
            rejected_identity: AtomicU32::new(0),
            broadcast: tokio::sync::Mutex::new(None),
            media_seq: AtomicU32::new(1),
            runtime: parking_lot::Mutex::new(runtime),
            peer_version: parking_lot::Mutex::new(None),
            pending_catalog_changes: parking_lot::Mutex::new(PendingCatalogChanges::default()),
        }
    }

    /// 本端信令 host(run 启动后有效,未设时回退 0.0.0.0)。
    fn local_host(&self) -> String {
        self.local_addr
            .get()
            .map(|(h, _)| h.clone())
            .unwrap_or_else(|| "0.0.0.0".into())
    }

    /// 本端信令端口(run 启动后有效,未设时回退 0)。
    fn local_port(&self) -> u16 {
        self.local_addr.get().map(|(_, p)| *p).unwrap_or(0)
    }

    /// 只读访问配置。
    pub fn config(&self) -> &DeviceConfig {
        &self.config
    }

    /// 设置本端信令地址(仅供测试/非 run 场景手动注入应答用;run() 会自动设置)。
    #[doc(hidden)]
    pub fn set_local_addr_for_test(&self, host: &str, port: u16) {
        let _ = self.local_addr.set((host.to_string(), port));
    }

    /// 本设备注册用的 Call-ID(用于在共享传输上注册接收路由)。
    pub fn call_id(&self) -> &str {
        &self.ids.call_id
    }

    fn next_cseq(&self) -> u32 {
        self.cseq.fetch_add(1, Ordering::Relaxed)
    }

    /// 翻转在线状态并在**真的变化时**上报事件。
    fn set_online(&self, online: bool) {
        let previous = self.online.swap(online, Ordering::Relaxed);
        if previous != online {
            self.observer
                .on_event(common::DeviceEvent::OnlineChanged { online });
        }
    }

    /// 发送一条无对话的 MANSCDP MESSAGE(心跳/报警/位置等)并等平台响应。
    ///
    /// 每次事务用**全新** [`DialogIds`](独立 Call-ID):同一设备的心跳、报警、
    /// 位置上报可能并发(周期任务 + 手动触发),若共用注册 Call-ID 会在共享传输上
    /// 互相覆盖接收路由、抢走对方的响应导致误判超时。独立 Call-ID 彻底隔离,
    /// 事务结束即注销路由。
    async fn send_message_xml(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
        xml: &str,
    ) -> Result<u16> {
        let dst: SocketAddr = format!("{}:{}", self.config.server_host, self.config.server_port)
            .parse()
            .map_err(|_| Error::Config("平台地址非法".into()))?;
        let ids = DialogIds::new();
        let cseq = self.next_cseq();
        let req = builder::message_xml(
            &self.config,
            &ids,
            cseq,
            local_host,
            local_port,
            xml,
            transport.protocol(),
        );
        let (mut rx, _route) = sip_core::RouteGuard::register(transport, &ids.call_id);
        let result = sip_core::client_transact(
            transport.as_ref(),
            dst,
            &req,
            &mut rx,
            sip_core::Timing::default(),
        )
        .await;
        Ok(result?.status)
    }

    /// 发送一条通知 MESSAGE 但**不等待响应**(fire-and-forget)。
    ///
    /// 用于纯告知性、平台可能不 ACK 的通知(如 VideoUploadNotify —— 部分平台
    /// 不支持该 CmdType 直接丢弃)。发出即返回,不因平台不响应而阻塞重传。
    async fn send_message_xml_oneshot(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
        xml: &str,
    ) -> Result<()> {
        let dst: SocketAddr = format!("{}:{}", self.config.server_host, self.config.server_port)
            .parse()
            .map_err(|_| Error::Config("平台地址非法".into()))?;
        let ids = DialogIds::new();
        let cseq = self.next_cseq();
        let req = builder::message_xml(
            &self.config,
            &ids,
            cseq,
            local_host,
            local_port,
            xml,
            transport.protocol(),
        );
        transport
            .send_to(&sip_core::SipMessage::Request(req), dst)
            .await
    }

    /// 在任何业务分派前统一校验平台身份。
    ///
    /// Compatible 保持既有互操作；Strict 要求恰好一个可解析的 From，且 URI user
    /// 精确匹配平台域 ID。这里不把 Authorization 的存在当成认证成功：当前只实现
    /// 设备作为 UAC 的 REGISTER Digest，没有平台反向凭据与 nonce/replay verifier。
    fn validate_inbound_identity(
        &self,
        req: &sip_core::Request,
    ) -> std::result::Result<(), InboundIdentityRejection> {
        if self.config.inbound_identity_mode == common::InboundIdentityMode::Compatible {
            return Ok(());
        }
        if req.headers.get_all("From").len() != 1 {
            return Err(InboundIdentityRejection {
                status: 400,
                reason: "Bad Request",
                detail: "From 头缺失或重复",
            });
        }
        let Some(platform_id) = req.headers.from_uri_user() else {
            return Err(InboundIdentityRejection {
                status: 400,
                reason: "Bad Request",
                detail: "From URI 无法解析",
            });
        };
        if platform_id != self.config.server_domain {
            return Err(InboundIdentityRejection {
                status: 403,
                reason: "Forbidden",
                detail: "From URI user 与平台域 ID 不匹配",
            });
        }
        Ok(())
    }

    /// 处理一条入站请求(带服务端事务去重)。
    ///
    /// 去重是必须的:UDP 下平台的请求会重传,而重复执行业务逻辑会产生真实故障
    /// (最典型的是重传 INVITE 被回 486 Busy Here,点播失败)。命中已完成事务时
    /// 直接重发缓存响应;上一轮仍在处理时吸收掉。
    pub async fn answer_inbound(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        incoming: &sip_core::Incoming,
    ) -> Result<bool> {
        let sip_core::SipMessage::Request(req) = &incoming.message else {
            return Ok(false);
        };

        let token = match self.server_txns.admit(req, incoming.from) {
            sip_core::Admit::Retransmit(cached) => {
                tracing::info!(
                    method = %req.method,
                    status = cached.status,
                    "入站请求重传,重发缓存响应(不重跑业务逻辑)"
                );
                transport
                    .send_to(&sip_core::SipMessage::Response(cached), incoming.from)
                    .await?;
                return Ok(true);
            }
            sip_core::Admit::Absorb => {
                tracing::debug!(method = %req.method, "入站请求重传,上一轮仍在处理,吸收");
                return Ok(true);
            }
            sip_core::Admit::Proceed(token) => token,
        };

        if let Err(rejection) = self.validate_inbound_identity(req) {
            let count = self.rejected_identity.fetch_add(1, Ordering::Relaxed) + 1;
            if count <= 5 || count % 1000 == 0 {
                tracing::warn!(
                    method = %req.method,
                    from = %incoming.from,
                    status = rejection.status,
                    reason = rejection.detail,
                    count,
                    "严格入站身份校验拒绝 SIP 请求"
                );
            }
            // ACK 不允许产生响应（RFC 3261）；身份错误时只静默丢弃，且绝不进入业务分派。
            if req.method == sip_core::Method::Ack {
                self.server_txns.abandon(&token);
                return Ok(true);
            }
            let response = builder::response_status(req, rejection.status, rejection.reason);
            self.respond(transport, incoming.from, &token, response)
                .await?;
            return Ok(true);
        }

        let result = self
            .dispatch_inbound(transport, req, incoming.from, &token)
            .await;
        // 没有产生最终响应(出错或未处理)时释放事务槽位,否则后续重传会被永久吸收。
        if !matches!(result, Ok(true)) {
            self.server_txns.abandon(&token);
        }
        result
    }

    /// 发送最终响应并登记到服务端事务表(供重传直接命中)。
    async fn respond(
        &self,
        transport: &Arc<dyn SipTransport>,
        dst: SocketAddr,
        token: &sip_core::TxToken,
        resp: sip_core::Response,
    ) -> Result<()> {
        self.server_txns.complete(token, &resp);
        transport
            .send_to(&sip_core::SipMessage::Response(resp), dst)
            .await
    }

    /// 按方法分派入站请求。OPTIONS 回 200 并带 Allow;MESSAGE 解析 XML 查询并应答;
    /// SUBSCRIBE 回 200 并按需启动周期 NOTIFY;未实现的方法回 405。
    ///
    /// 取 `self: &Arc<Self>`(而非 `&self`):移动位置订阅需要把设备句柄交给后台
    /// 周期上报任务,任务生命周期独立于单次入站处理。
    async fn dispatch_inbound(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        req: &sip_core::Request,
        from: SocketAddr,
        token: &sip_core::TxToken,
    ) -> Result<bool> {
        let incoming_from = from;
        {
            match req.method {
                sip_core::Method::Options => {
                    // OPTIONS 应答带 Allow,便于平台探知设备支持的方法集。
                    let mut resp = builder::response_ok(req);
                    resp.headers.set("Allow", sip_core::Method::ALLOW_HEADER);
                    self.respond(transport, incoming_from, token, resp).await?;
                    Ok(true)
                }
                sip_core::Method::Message => {
                    // GB28181 规范:先对查询 MESSAGE 回空 200 OK(事务应答),
                    // 再由设备发一条独立的 MESSAGE 把应答 XML 送回平台(新事务)。
                    self.respond(transport, incoming_from, token, builder::response_ok(req))
                        .await?;

                    // body 可能是查询(Query)或控制(Control)。按**根元素**区分,
                    // 不能靠 parse 成功与否(quick-xml 忽略根名,Query 会误吞 Control)。
                    // 按信令编码解码(GB18030/UTF-8),修复中文乱码。
                    {
                        let body_str =
                            builder::decode_xml_body(&req.body, self.config.signaling_encoding);
                        let body_str = body_str.as_str();
                        // 按**真正解析出的根元素**分派,而不是 `contains("<Control>")`:
                        // 后者可被注释或文本节点里的假标签欺骗,改变设备的处理路径。
                        let envelope = gb28181_protocol::manscdp::parse_envelope(body_str)
                            .unwrap_or_else(|e| {
                                tracing::warn!(error = %e, "MANSCDP 报文无法解析根元素");
                                gb28181_protocol::manscdp::Envelope {
                                    root: String::new(),
                                    cmd_type: String::new(),
                                }
                            });
                        if envelope.root == "Control" {
                            if let Ok(ctrl) = gb28181_protocol::manscdp::Control::parse(body_str) {
                                let xml = self.handle_control(&ctrl).await?;
                                self.send_reply_message(transport, incoming_from, &xml)
                                    .await?;
                                // 平台命令时间线:上报语义摘要(控制类,含方向/变倍/预置位等细节)。
                                self.observer
                                    .on_event(common::DeviceEvent::PlatformCommand {
                                        kind: "control".into(),
                                        summary: format!(
                                            "平台控制:{} → 已应答 OK",
                                            Self::control_detail(&ctrl)
                                        ),
                                    });
                                // 抓拍/升级等需设备主动发 NOTIFY 的命令:回 200/结果后异步执行。
                                self.spawn_control_side_effects(transport, &ctrl);
                            }
                        } else if envelope.cmd_type == "Broadcast" {
                            // 语音广播:回 Broadcast Response(OK/ERROR),接受则反向发 INVITE。
                            if let Ok(bc) =
                                gb28181_protocol::manscdp::BroadcastNotify::parse(body_str)
                            {
                                self.handle_broadcast(transport, &bc, incoming_from).await?;
                                self.observer
                                    .on_event(common::DeviceEvent::PlatformCommand {
                                        kind: "broadcast".into(),
                                        summary: "平台语音广播请求 → 已应答".into(),
                                    });
                            }
                        } else if envelope.root == "Query" || envelope.root == "Notify" {
                            if let Ok(query) = gb28181_protocol::manscdp::Query::parse(body_str) {
                                let max_body = Self::max_body_for(transport.protocol());
                                if let Ok(replies) = self.handle_query_replies(&query, max_body) {
                                    // 附录 M:多条应答串行发送(收到上一条的响应再发下一条)。
                                    self.send_reply_messages_serial(
                                        transport,
                                        incoming_from,
                                        &replies,
                                    )
                                    .await?;
                                    // 平台命令时间线:上报语义摘要(查询类)。
                                    self.observer
                                        .on_event(common::DeviceEvent::PlatformCommand {
                                            kind: "query".into(),
                                            summary: self.query_summary(&query.cmd_type),
                                        });
                                }
                            }
                        } else {
                            tracing::warn!(
                                root = %envelope.root,
                                cmd_type = %envelope.cmd_type,
                                "无法识别的 MANSCDP 报文,已回 200 但不处理"
                            );
                        }
                    }
                    Ok(true)
                }
                sip_core::Method::Invite => {
                    // 解析 SDP,启动推流,回 200 OK。
                    self.handle_invite(transport, req, incoming_from, token)
                        .await
                }
                sip_core::Method::Bye => {
                    // 停止推流,回 200 OK。
                    self.handle_bye(transport, req, incoming_from, token).await
                }
                sip_core::Method::Cancel => {
                    // 取消点播(RFC 3261 §9.2):对 CANCEL 本身回 200,
                    // 同时对被取消的 INVITE 回 487 Request Terminated。
                    // 之前只回 200,平台会一直等 INVITE 的最终响应。
                    self.handle_cancel(transport, req, incoming_from, token)
                        .await
                }
                sip_core::Method::Ack => {
                    // ACK 无需响应,但标志会话已确认。
                    Ok(true)
                }
                sip_core::Method::Subscribe => {
                    // 平台订阅(Catalog/MobilePosition/Alarm):先回 200 OK 建立订阅。
                    // 用固定 to_tag,使 200 的 To-tag 与后续对话内 NOTIFY 的 From-tag 一致。
                    let local_tag = builder::rand_token("");
                    // Expires: 0 是退订(RFC 3265 §3.2.4):回 200 后终止该订阅。
                    let requested_expires = req.headers.expires().unwrap_or(3600);
                    self.respond(
                        transport,
                        incoming_from,
                        token,
                        builder::response_ok_tagged(req, &local_tag),
                    )
                    .await?;

                    {
                        let body_str =
                            builder::decode_xml_body(&req.body, self.config.signaling_encoding);
                        if let Ok(query) = gb28181_protocol::manscdp::Query::parse(&body_str) {
                            // 从 SUBSCRIBE + 本端 tag 构建订阅对话(供对话内 NOTIFY 用)。
                            let dialog = builder::NotifyDialog {
                                call_id: req.headers.call_id().unwrap_or_default().to_string(),
                                local_tag,
                                remote_from: req
                                    .headers
                                    .get("From")
                                    .unwrap_or_default()
                                    .to_string(),
                                event: req
                                    .headers
                                    .get("Event")
                                    .unwrap_or(&query.cmd_type)
                                    .to_string(),
                                expires: requested_expires.min(u32::MAX as u64) as u32,
                            };
                            // Expires: 0 → 退订。终止周期任务、清对话,并发一条
                            // Subscription-State: terminated 的收尾 NOTIFY。
                            if requested_expires == 0 {
                                self.terminate_subscription(
                                    transport,
                                    &query.cmd_type,
                                    &dialog,
                                    incoming_from,
                                )
                                .await;
                                return Ok(true);
                            }
                            match query.cmd_type.as_str() {
                                // 移动位置订阅:启动周期位置 NOTIFY(独立 MESSAGE 路径)。
                                "MobilePosition" => {
                                    self.start_position_subscription(
                                        transport,
                                        query.interval,
                                        requested_expires,
                                    )
                                    .await;
                                }
                                // 目录订阅:记录对话供 CRUD 增量 NOTIFY,并回发一条全量目录 NOTIFY。
                                "Catalog" => {
                                    *self.catalog_dialog.lock().await =
                                        Some((dialog.clone(), incoming_from));
                                    self.arm_subscription_expiry(
                                        transport,
                                        "Catalog",
                                        dialog.clone(),
                                        incoming_from,
                                        requested_expires,
                                    )
                                    .await;
                                    let sim = Arc::clone(self);
                                    let tp = Arc::clone(transport);
                                    let sn = query.sn;
                                    let dst = incoming_from;
                                    let dialog_for_notify = dialog.clone();
                                    // 独立任务发送:notify 走事务会等待平台响应,不占用入站循环。
                                    tokio::spawn(async move {
                                        if let Err(e) = sim
                                            .notify_catalog(&tp, dst, &dialog_for_notify, sn)
                                            .await
                                        {
                                            tracing::warn!(error=%e, "目录订阅通知失败");
                                        }
                                    });
                                }
                                // 报警订阅:记录对话,后续 report_alarm 走对话内 NOTIFY。
                                "Alarm" => {
                                    let filter =
                                        gb28181_protocol::manscdp::AlarmSubscribeFilter::from_query(
                                            &query,
                                        );
                                    tracing::info!(
                                        ?filter,
                                        "报警订阅已建立(按 A.2.4.6 筛选条件过滤上报)"
                                    );
                                    *self.alarm_dialog.lock().await = Some(AlarmSubscription {
                                        dialog: dialog.clone(),
                                        peer: incoming_from,
                                        filter,
                                    });
                                    self.arm_subscription_expiry(
                                        transport,
                                        "Alarm",
                                        dialog,
                                        incoming_from,
                                        requested_expires,
                                    )
                                    .await;
                                }
                                // PTZ 精准位置事件订阅(GB28181-2022 §9.11.1/9.11.2):
                                // 启动周期 PTZ 精准状态 NOTIFY(独立 MESSAGE 路径,与位置订阅一致)。
                                "PTZPosition" => {
                                    self.start_ptz_position_subscription(
                                        transport,
                                        query.interval,
                                        requested_expires,
                                    )
                                    .await;
                                }
                                _ => {}
                            }
                        }
                    }
                    Ok(true)
                }
                sip_core::Method::Info => {
                    // 会话内回放控制(MANSRTSP):PLAY(Scale=N)调速 / 恢复,PAUSE 暂停。
                    self.handle_info(transport, req, incoming_from, token).await
                }
                // REGISTER 是设备发出的,不该由设备处理;PRACK/UPDATE/REFER/PUBLISH 未实现。
                // 静默丢弃会让平台一直重传,按 RFC 3261 §8.2.1 回 405 + Allow。
                method => {
                    tracing::info!(%method, "未实现的入站方法,回 405 Method Not Allowed");
                    let mut resp = builder::response_status(req, 405, "Method Not Allowed");
                    resp.headers.set("Allow", sip_core::Method::ALLOW_HEADER);
                    self.respond(transport, incoming_from, token, resp).await?;
                    Ok(true)
                }
            }
        }
    }

    /// 向平台发送一条应答/通知 MESSAGE(查询应答、控制应答、位置通知等复用)。
    /// 不等响应 —— 单条应答不值得为它开一条独立 Call-ID 去等 200。
    async fn send_reply_message(
        &self,
        transport: &Arc<dyn SipTransport>,
        to: SocketAddr,
        xml: &str,
    ) -> Result<()> {
        let cseq = self.next_cseq();
        let reply = builder::message_xml(
            &self.config,
            &self.ids,
            cseq,
            &self.local_host(),
            self.local_port(),
            xml,
            transport.protocol(),
        );
        transport
            .send_to(&sip_core::SipMessage::Request(reply), to)
            .await
    }

    /// **串行**发送多条应答 MESSAGE(GB/T 28181-2022 附录 M)。
    ///
    /// 标准原文:"多条响应、通知消息发送时宜采用串行发送方式,记录发送方需收到上一条
    /// SIP Message/Notify 消息的 SIP 响应后再进行后续发送处理。"
    ///
    /// 为什么这条要求是有意义的:UDP 下一次性把十几条目录应答喷出去,平台侧的接收
    /// 缓冲很容易溢出,现象是"目录时多时少"。等上一条的 200 再发下一条,相当于用
    /// 应答做流控。
    ///
    /// 单条应答不值得为它开一条独立 Call-ID 去等响应,直接发。
    async fn send_reply_messages_serial(
        &self,
        transport: &Arc<dyn SipTransport>,
        to: SocketAddr,
        replies: &[String],
    ) -> Result<()> {
        if replies.len() <= 1 {
            if let Some(xml) = replies.first() {
                self.send_reply_message(transport, to, xml).await?;
            }
            return Ok(());
        }
        let (host, port) = (self.local_host(), self.local_port());
        for (index, xml) in replies.iter().enumerate() {
            // 用等响应的发送路径:它每条用独立 Call-ID,事务结束即注销路由。
            match self.send_message_xml(transport, &host, port, xml).await {
                Ok(status) => {
                    if !(200..300).contains(&status) {
                        tracing::warn!(
                            index,
                            total = replies.len(),
                            status,
                            "多响应传输:平台对某条应答回了非 2xx,继续发后续分片"
                        );
                    }
                }
                Err(error) => {
                    // 平台不回 200 时不能就此放弃剩余分片:目录残缺比慢更糟。
                    tracing::warn!(
                        index,
                        total = replies.len(),
                        %error,
                        "多响应传输:等上一条应答的响应失败,继续发后续分片"
                    );
                }
            }
        }
        Ok(())
    }

    /// 运行设备:注册 → 启动入站应答(OPTIONS)→ 周期心跳,直到 `shutdown` 完成。
    ///
    /// 心跳连续失败 `max_hb_fail` 次判定掉线,触发重注册(指数退避 backoff→2×,上限 backoff_max)。
    /// 这是设备"保持在线"的主循环(docs/10-functional/device-simulation.md §3.2/§3.3)。
    pub async fn run(
        self: Arc<Self>,
        transport: Arc<dyn SipTransport>,
        local_host: String,
        local_port: u16,
        shutdown: impl std::future::Future<Output = ()>,
    ) {
        use std::time::Duration;
        tokio::pin!(shutdown);

        // 记录本端信令地址(应答 MESSAGE / NOTIFY 构造 Via/From/Contact 用)。
        let _ = self.local_addr.set((local_host.clone(), local_port));

        // 入站请求应答任务:注册设备 ID + 全部通道 ID(点播 INVITE 的
        // Request-URI 是通道 ID,需一并注册才能收到)。
        let mut aors = vec![self.config.device_id.as_str().to_string()];
        aors.extend(
            self.config
                .channels
                .iter()
                .map(|c| c.channel_id.as_str().to_string()),
        );
        // 多通道目录树的视频通道 ID 也要注册,否则平台点播子通道的 INVITE 收不到(问题1)。
        {
            let tree = self.catalog_tree.lock();
            aors.extend(
                tree.iter()
                    .filter(|n| {
                        n.node_type == gb28181_protocol::id_codec::CatalogNodeType::VideoChannel
                    })
                    .map(|n| n.id.clone()),
            );
        }
        aors.sort();
        aors.dedup();
        // 保留一份注册清单:下线时必须**逐条**注销。
        // 之前只注销了设备 ID,通道条目永久留在路由表里 ——
        // 反复启停一台 16 通道 NVR,每轮多留一批(压测启停时更明显)。
        let registered_aors = aors.clone();
        let mut inbound = transport.register_inbound_many(aors);
        let inbound_sim = self.clone();
        let inbound_tp = transport.clone();
        let inbound_task = tokio::spawn(async move {
            while let Some(inc) = inbound.recv().await {
                if let sip_core::SipMessage::Request(r) = &inc.message {
                    tracing::info!(method=%r.method, uri=%r.uri, "收到入站请求");
                }
                if let Err(e) = inbound_sim.answer_inbound(&inbound_tp, &inc).await {
                    tracing::warn!(error=%e, "入站请求应答失败");
                }
            }
        });

        // 初次注册(失败则退避重试)。
        // §9.1.1 c):注册重试间隔可调,一般不应短于 60s。压测场景会调小。
        let backoff_base = Duration::from_secs(1);
        let backoff_max = Duration::from_secs(self.config.register_retry_max_secs.max(1));
        let mut backoff = backoff_base;
        loop {
            // **注册本身也必须能被停止打断**。之前只有"退避等待"在 select 里,
            // 而 `register()` 内部是一次完整的 SIP 客户端事务:平台不可达时会按
            // Timer A 重传直到 Timer B(约 32 秒)才返回 Err。于是用户点"停止"后
            // 界面要僵住半分钟,压测批量停止更是逐台叠加。
            let registered = tokio::select! {
                _ = &mut shutdown => None,
                r = self.register(&transport, &local_host, local_port) => Some(r),
            };
            match registered {
                // 注册成功,进入心跳主循环。
                Some(Ok(_)) => break,
                Some(Err(_)) => {}
                None => {
                    // 注册进行中被要求停止:这条早退路径以前既不注销路由也不清理,
                    // 于是"启动失败后重试"会一路累积 AOR 条目。
                    inbound_task.abort();
                    transport.unregister_inbound_many(&registered_aors);
                    return;
                }
            }
            tokio::select! {
                _ = &mut shutdown => {
                    inbound_task.abort();
                    transport.unregister_inbound_many(&registered_aors);
                    return;
                }
                _ = tokio::time::sleep(backoff) => {
                    backoff = (backoff * 2).min(backoff_max);
                }
            }
        }
        tracing::info!(device=%self.config.device_id, "注册成功");

        // 心跳主循环 + 注册续约(FR-32)。
        // 续约周期以**平台授予的 Expires** 为准(取其 80%),而不是硬编码 3600×80%。
        // 平台只给 120s 时,按 2880s 续约会被判掉线。
        let mut heartbeat_interval =
            Duration::from_secs(self.config.heartbeat_interval_secs.max(1));
        let mut current_expires = self.granted_expires();
        let mut renew_ticker = tokio::time::interval(renew_interval(current_expires));
        renew_ticker.reset(); // 跳过启动即触发
        let mut max_hb_fail;
        let mut hb_fail = 0u32;
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                _ = renew_ticker.tick() => {
                    // 到期前主动续约,避免掉线后被动重注册。
                    tracing::info!(
                        device=%self.config.device_id,
                        expires = current_expires,
                        "注册续约(平台授予有效期的 80%)"
                    );
                    let _ = self.register(&transport, &local_host, local_port).await;
                    // 平台可能在续约时改授新的有效期,跟随调整续约周期。
                    let granted = self.granted_expires();
                    if granted != current_expires {
                        current_expires = granted;
                        renew_ticker = tokio::time::interval(renew_interval(current_expires));
                        renew_ticker.reset();
                    }
                }
                _ = tokio::time::sleep(heartbeat_interval) => {
                    // 每拍重读:平台经 DeviceConfig/BasicParam 改过的心跳周期与
                    // 超时次数要真的生效,而不是沿用启动时的快照。
                    {
                        let rt = self.runtime.lock();
                        heartbeat_interval = Duration::from_secs(rt.heartbeat_interval_secs.max(1));
                        max_hb_fail = rt.heartbeat_count.max(1);
                    }
                    match self.send_keepalive(&transport, &local_host, local_port).await {
                        Ok(200) => hb_fail = 0,
                        _ => {
                            hb_fail += 1;
                            if hb_fail >= max_hb_fail {
                                tracing::warn!(device=%self.config.device_id, "心跳连续失败,重注册");
                                hb_fail = 0;
                                // 平台此时已把设备当离线,gauge 要跟上。
                                self.set_online(false);
                                let mut b = backoff_base;
                                while self.register(&transport, &local_host, local_port).await.is_err() {
                                    tokio::select! {
                                        _ = &mut shutdown => { inbound_task.abort(); return; }
                                        _ = tokio::time::sleep(b) => { b = (b * 2).min(backoff_max); }
                                    }
                                }
                                let granted = self.granted_expires();
                                if granted != current_expires {
                                    current_expires = granted;
                                    renew_ticker = tokio::time::interval(renew_interval(current_expires));
                                    renew_ticker.reset();
                                }
                            }
                        }
                    }
                }
            }
        }
        inbound_task.abort();
        self.set_online(false);
        self.stop_subscriptions().await;
        // 附录 K b):设备下线前先释放媒体链路并发会话内 BYE,别让平台干等着收流。
        self.release_media_on_shutdown(&transport).await;
        // 主动向平台注销(REGISTER Expires=0,§9.1.2.2):平台立即置离线,不等心跳超时。
        if let Err(e) = self.unregister(&transport, &local_host, local_port).await {
            tracing::warn!(device=%self.config.device_id, error=%e, "注销失败(下线仍继续)");
        } else {
            tracing::info!(device=%self.config.device_id, "已向平台注销");
        }
        transport.unregister_inbound_many(&registered_aors);
        let (txn_routes, aor_routes) = transport.route_counts();
        tracing::debug!(
            device = %self.config.device_id,
            txn_routes,
            aor_routes,
            "设备已下线,路由已注销"
        );
    }
}
