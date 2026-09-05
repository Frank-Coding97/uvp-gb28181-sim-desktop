//! SIP 传输层。
//!
//! 实现 docs/30-crates/sip-core.md §transport。两种传输共用一套
//! **共享连接 + 路由分发** 模型:一条 socket 承载多台虚拟设备的收发,
//! 收到的响应按 Call-ID 投递到对应设备的接收队列,入站请求按 Request-URI 的 AOR 投递。
//! 这是压测万级设备的关键(避免每设备一个 socket 耗尽端口/FD,
//! 见 docs/10-functional/stress-testing.md#42)。
//!
//! - [`UdpTransport`]:GB28181 最常用的 UDP 传输。
//! - [`TcpTransport`]:SIP over TCP(RFC 3261 §18.3 的流式重组)。
//!
//! 上层通过 [`SipTransport`] trait 使用,因此 **Via 里的传输标识可以由传输对象自己
//! 给出**,不再可能出现"配置写 TCP、实际走 UDP"这种协议撒谎。
//!
//! 安全与可信度要点:
//! - **来源校验**:通过 [`SipTransport::trust_only_host`] 限定只接受来自上级平台的报文。
//!   不设限时任何主机都能注入 PTZ/DeviceConfig/INVITE,而 INVITE 的 `c=`/`m=` 决定推流
//!   目标,构成媒体流量放大。
//! - **有界队列**:每设备队列有上限,满了记 `dropped_queue_full` 而不是无界堆积。
//! - **收发缓冲**:显式放大 `SO_RCVBUF`,避免注册风暴时内核丢包被误判成"平台超时"。
//! - **统计**:[`TransportStats`] 把本地丢弃与平台失败分开,保证压测结论可信。

mod tcp;
mod udp;

use std::collections::HashSet;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::mpsc;

use common::{Error, Result};

use crate::message::SipMessage;

pub use tcp::TcpTransport;
pub use udp::{UdpRateLimitConfig, UdpTransport};

/// 每设备入站队列容量。SIP 是请求-应答模型,正常情况下队列深度个位数;
/// 128 足够吸收突发,又能在平台洪泛时快速形成可观测的背压而不是吃光内存。
pub(crate) const QUEUE_CAPACITY: usize = 128;

/// 期望的 socket 收发缓冲(字节)。万级设备注册风暴时默认几百 KB 会溢出丢包,
/// 而丢包会被上层统计成"事务超时",让人误判为平台过载。
pub(crate) const DEFAULT_SOCKET_BUFFER: usize = 8 * 1024 * 1024;

/// 单条 UDP 数据报的安全上限(RFC 3261 §18.1.1 建议 MTU-200)。超过即告警:
/// 目录应答等大 body 应分片,否则中间设备静默截断,现象是"平台收不到目录"。
pub(crate) const UDP_SAFE_MTU: usize = 1300;

/// 一条收到的消息 + 来源地址。
#[derive(Debug)]
pub struct Incoming {
    /// 解析后的 SIP 消息。
    pub message: SipMessage,
    /// 来源地址。
    pub from: SocketAddr,
}

/// SIP 报文追踪方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceDir {
    /// 收到平台报文。
    In,
    /// 发往平台。
    Out,
}

/// 一条 SIP 追踪:方向 + 报文 + 对端地址。观察者负责结构化/转发(如推给 UI)。
pub struct SipTrace<'a> {
    pub dir: TraceDir,
    pub message: &'a SipMessage,
    pub peer: SocketAddr,
}

/// SIP 追踪观察者(FR-43)。挂上后每条收发报文回调一次;默认无(零开销)。
/// 压测大规模时不挂,避免热路径回调开销。
pub trait TraceObserver: Send + Sync {
    fn on_trace(&self, trace: SipTrace<'_>);
}

/// 传输层计数器。压测时用来区分"本地瓶颈"与"平台真的失败"。
#[derive(Debug, Default)]
pub struct TransportStats {
    /// 成功解析并分发的报文数。
    pub received: AtomicU64,
    /// 解析失败被丢弃的报文数(畸形包 / 截断 / 非 SIP)。
    pub parse_errors: AtomicU64,
    /// 来源 IP 不在白名单被拒的报文数。
    pub rejected_source: AtomicU64,
    /// UDP 来源超过令牌桶预算而被静默丢弃的数据报数。
    ///
    /// 与白名单叠加:白名单先筛掉不可信来源,可信来源仍受限流保护,避免平台故障
    /// 或可信主机失陷后耗尽解析 CPU。拒绝时绝不回 429/503,避免把限流器变成反射器。
    pub rate_limited: AtomicU64,
    /// 被限流数据报的总字节数,用于区分小包洪泛与大包 CPU/带宽攻击。
    pub rate_limited_bytes: AtomicU64,
    /// UDP 直接响应因无入站请求字节信用或超过安全 MTU 而被阻断的次数。
    pub anti_amplification_blocked: AtomicU64,
    /// 目标设备队列已满被丢弃的报文数——**本地背压**,不是平台失败。
    pub dropped_queue_full: AtomicU64,
    /// 找不到路由被丢弃的报文数。
    pub unrouted: AtomicU64,
    /// 成功发出的报文数。
    pub sent: AtomicU64,
    /// 本地发送失败数(ENOBUFS / EPERM / 连接断开等)——**本地瓶颈**。
    pub send_errors: AtomicU64,
    /// 超过安全 MTU 仍按 UDP 发出的报文数(有被中间设备截断的风险)。
    pub oversized_sent: AtomicU64,
    /// 因入站连接数已达上限而被拒的 TCP 连接数(仅 TCP)。
    pub rejected_connections: AtomicU64,
    /// 因空闲/握手超时被主动断开的 TCP 连接数(仅 TCP)。
    pub timed_out_connections: AtomicU64,
    /// 接收循环因 socket 错误异常退出的次数。
    ///
    /// 非 0 意味着该 socket 上的设备已经**彻底失聪**;之前这种情况只有一行日志,
    /// 现象是"设备莫名不再响应平台"而无从查证。
    pub recv_errors: AtomicU64,
}

/// [`TransportStats`] 的可序列化快照。
///
/// 有 `Default` + [`TransportStatsSnapshot::merge`]:压测把多个 socket 分片的统计
/// 汇总成一份,而调用方**不该在加字段时被迫改动累加代码** ——
/// 之前那里是手写的逐字段初始化 + 逐字段相加,每加一个计数器就编译不过一次,
/// 更糟的是漏加一个字段编译器不会报错。
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct TransportStatsSnapshot {
    pub received: u64,
    pub parse_errors: u64,
    pub rejected_source: u64,
    pub rate_limited: u64,
    pub rate_limited_bytes: u64,
    pub anti_amplification_blocked: u64,
    pub dropped_queue_full: u64,
    pub unrouted: u64,
    pub sent: u64,
    pub send_errors: u64,
    pub oversized_sent: u64,
    pub rejected_connections: u64,
    pub timed_out_connections: u64,
    pub recv_errors: u64,
}

impl TransportStatsSnapshot {
    /// 把另一份快照累加进来(汇总多个 socket 分片)。
    pub fn merge(&mut self, other: &TransportStatsSnapshot) {
        let TransportStatsSnapshot {
            received,
            parse_errors,
            rejected_source,
            rate_limited,
            rate_limited_bytes,
            anti_amplification_blocked,
            dropped_queue_full,
            unrouted,
            sent,
            send_errors,
            oversized_sent,
            rejected_connections,
            timed_out_connections,
            recv_errors,
        } = other;
        // 逐字段解构而不是 `other.x`:将来加字段时这里会编译不过,
        // 强制作者决定"这个新计数器要不要参与汇总",而不是被静默漏掉。
        self.received += received;
        self.parse_errors += parse_errors;
        self.rejected_source += rejected_source;
        self.rate_limited += rate_limited;
        self.rate_limited_bytes += rate_limited_bytes;
        self.anti_amplification_blocked += anti_amplification_blocked;
        self.dropped_queue_full += dropped_queue_full;
        self.unrouted += unrouted;
        self.sent += sent;
        self.send_errors += send_errors;
        self.oversized_sent += oversized_sent;
        self.rejected_connections += rejected_connections;
        self.timed_out_connections += timed_out_connections;
        self.recv_errors += recv_errors;
    }
}

impl TransportStats {
    pub fn snapshot(&self) -> TransportStatsSnapshot {
        TransportStatsSnapshot {
            received: self.received.load(Ordering::Relaxed),
            parse_errors: self.parse_errors.load(Ordering::Relaxed),
            rejected_source: self.rejected_source.load(Ordering::Relaxed),
            rate_limited: self.rate_limited.load(Ordering::Relaxed),
            rate_limited_bytes: self.rate_limited_bytes.load(Ordering::Relaxed),
            anti_amplification_blocked: self.anti_amplification_blocked.load(Ordering::Relaxed),
            dropped_queue_full: self.dropped_queue_full.load(Ordering::Relaxed),
            unrouted: self.unrouted.load(Ordering::Relaxed),
            sent: self.sent.load(Ordering::Relaxed),
            send_errors: self.send_errors.load(Ordering::Relaxed),
            oversized_sent: self.oversized_sent.load(Ordering::Relaxed),
            rejected_connections: self.rejected_connections.load(Ordering::Relaxed),
            timed_out_connections: self.timed_out_connections.load(Ordering::Relaxed),
            recv_errors: self.recv_errors.load(Ordering::Relaxed),
        }
    }
}

/// `send_to` 的返回类型(trait object 友好,免 async-trait 依赖)。
pub type SendFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// 上层统一使用的 SIP 传输接口。
///
/// 之所以做成 trait:让 **Via 里的传输标识由传输对象给出**([`Self::protocol`]),
/// 而不是由配置字段单独声明 —— 后者曾经导致"配置写 TCP、Via 写 TCP、实际用 UDP 发"
/// 这种平台按 Via 回包就走错路径的问题。
pub trait SipTransport: Send + Sync + 'static {
    /// 实际使用的传输协议(用于填 Via 的 `SIP/2.0/<proto>`)。
    fn protocol(&self) -> common::Transport;

    /// 本地实际绑定地址(端口由系统分配时用于回填 Contact/Via)。
    fn local_addr(&self) -> Result<SocketAddr>;

    /// 为某 Call-ID 注册响应接收队列(客户端事务用)。同一 Call-ID 重复注册覆盖。
    fn register(&self, call_id: &str) -> mpsc::Receiver<Incoming>;

    /// 为某设备 AOR(device_id)注册入站请求队列。
    fn register_inbound(&self, aor: &str) -> mpsc::Receiver<Incoming>;

    /// 为多个 AOR(设备 ID + 各通道 ID)注册到同一入站队列。
    fn register_inbound_many(&self, aors: Vec<String>) -> mpsc::Receiver<Incoming>;

    /// 注销某 Call-ID 的响应路由(事务结束时可调)。
    fn unregister(&self, call_id: &str);

    /// 注销某设备 AOR 的入站路由(设备下线时调)。
    fn unregister_inbound(&self, aor: &str);

    /// 批量注销入站路由。与 [`SipTransport::register_inbound_many`] 配对。
    ///
    /// 之前只有单个版本,而注册是批量的(设备 ID + 全部通道 ID + 目录树里的视频通道),
    /// 下线时却只注销了设备 ID —— 通道条目永久留在路由表里。
    /// 反复启停一台 16 通道 NVR,每轮多留一批。
    fn unregister_inbound_many(&self, aors: &[String]);

    /// 当前路由表规模 `(事务路由数, AOR 路由数)`。
    ///
    /// 用于诊断"停止后是否真的收敛":这两个数在设备全部下线后应当归零。
    fn route_counts(&self) -> (usize, usize);

    /// 挂载 SIP 追踪观察者(FR-43)。传 None 关闭。
    fn set_tracer(&self, tracer: Option<Arc<dyn TraceObserver>>);

    /// 设定可信来源 IP。空集合等价于关闭过滤(便于单测与回环联调)。
    fn set_trusted_ips(&self, ips: Vec<IpAddr>);

    /// 便捷入口:按字面 IP 解析出来源白名单。解析失败(域名)返回 false 并保持原白名单。
    fn trust_only_host(&self, host: &str) -> bool;

    /// 传输层计数器(压测报告与诊断用)。
    fn stats(&self) -> Arc<TransportStats>;

    /// 发送一条 SIP 消息到目标地址。
    fn send_to<'a>(&'a self, msg: &'a SipMessage, dst: SocketAddr) -> SendFuture<'a>;

    /// 主动关停传输:停掉接收/接受循环,释放 socket 与全部连接。
    ///
    /// **为什么必须显式提供而不能只靠 Drop**:接收循环需要持有 socket 才能收包,
    /// 于是它和传输对象之间天然存在一条引用。早期实现把 `Arc<Self>` 直接 move 进
    /// 那个"只在出错时才 break"的循环里,引用计数永远 ≥ 1 —— 传输对象永不 drop、
    /// socket 永不关闭、任务永不退出。压测按 64 台设备一个 socket 分片,
    /// 一万台设备每跑一轮就泄漏 150 多个 fd 和同样多的任务,跑十轮就撞上 `ulimit -n`。
    ///
    /// 实现约定:幂等(重复调用无副作用),且调用后 `send_to` 应快速失败而不是挂住。
    fn shutdown(&self);
}

/// 事务路由的 RAII 守卫:离开作用域时自动注销 Call-ID 路由。
///
/// # 为什么必须是 RAII
///
/// 原来每个发事务的地方都是 `transport.register(&call_id)` … 业务逻辑 …
/// `transport.unregister(&call_id)`。只要中间那段被**取消**(被 `select!` 的另一个
/// 分支抢先、调用方超时、任务被 abort),后面的 `unregister` 就不会执行,
/// 路由表里留下一条永久条目。设备启动期间点"停止"就能稳定复现:
/// REGISTER 事务正在按 Timer A 重传,停止信号让整个 future 被丢弃,
/// 那个 Call-ID 从此留在表里。
///
/// 手写"记得在每条早退路径上注销"是行不通的 —— 取消点是隐式的,
/// 没有任何编译期提示。交给 `Drop` 才能覆盖全部路径。
pub struct RouteGuard {
    transport: Arc<dyn SipTransport>,
    call_id: String,
}

impl RouteGuard {
    /// 注册一个 Call-ID 路由并返回 `(接收端, 守卫)`。守卫存活期间路由有效。
    pub fn register(
        transport: &Arc<dyn SipTransport>,
        call_id: &str,
    ) -> (mpsc::Receiver<Incoming>, RouteGuard) {
        let rx = transport.register(call_id);
        (
            rx,
            RouteGuard {
                transport: Arc::clone(transport),
                call_id: call_id.to_string(),
            },
        )
    }

    /// 被守卫的 Call-ID。
    pub fn call_id(&self) -> &str {
        &self.call_id
    }
}

impl Drop for RouteGuard {
    fn drop(&mut self) {
        self.transport.unregister(&self.call_id);
    }
}

impl std::fmt::Debug for RouteGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteGuard")
            .field("call_id", &self.call_id)
            .finish()
    }
}

/// 关停信号。发一次、所有等待方都能看到,**且晚订阅的一方也立刻看到已关停**。
///
/// 用 `watch` 而不是 `Notify`:`notify_waiters()` 对"还没开始 await 的等待方"是丢失的,
/// 而关停与循环启动之间存在真实竞态(bind 之后立刻 shutdown);
/// `notify_one()` 只存一个许可,喂不饱 TCP 那边的 N 条读循环。
pub(crate) struct Shutdown {
    tx: tokio::sync::watch::Sender<bool>,
}

impl Shutdown {
    pub(crate) fn new() -> Self {
        Shutdown {
            tx: tokio::sync::watch::channel(false).0,
        }
    }

    /// 触发关停。幂等。
    pub(crate) fn trigger(&self) {
        // 必须用 `send_replace` 而不是 `send`:后者在"当前没有任何接收端"时返回 Err
        // **并且不更新值**。而 `Shutdown::new()` 建完就丢掉了初始接收端,
        // 循环里的接收端也要等任务真正被调度起来才存在 —— 用 `send` 的话
        // "bind 之后立刻 shutdown" 这条路径会静默丢掉关停信号。
        self.tx.send_replace(true);
    }

    pub(crate) fn is_closed(&self) -> bool {
        *self.tx.borrow()
    }

    pub(crate) fn subscribe(&self) -> ShutdownRx {
        ShutdownRx {
            rx: self.tx.subscribe(),
        }
    }
}

impl Default for Shutdown {
    fn default() -> Self {
        Self::new()
    }
}

/// [`Shutdown`] 的等待端。
pub(crate) struct ShutdownRx {
    rx: tokio::sync::watch::Receiver<bool>,
}

impl ShutdownRx {
    /// 等到关停被触发;**已经关停时立刻返回**。
    pub(crate) async fn wait(&mut self) {
        if *self.rx.borrow() {
            return;
        }
        // 发送端随 core 常驻;真断开了也视为关停。
        let _ = self.rx.changed().await;
    }
}

/// 两种传输共用的路由表 + 追踪 + 来源白名单 + 统计。
pub(crate) struct Router {
    /// Call-ID → 发起事务的设备的响应投递端。
    routes: DashMap<String, mpsc::Sender<Incoming>>,
    /// 设备 AOR(device_id)→ 该设备的入站请求投递端。
    aor_routes: DashMap<String, mpsc::Sender<Incoming>>,
    /// 可选 SIP 追踪观察者(FR-43);None 时零开销。
    tracer: std::sync::RwLock<Option<Arc<dyn TraceObserver>>>,
    /// 可信来源 IP 白名单。None = 不过滤;Some = 只接受集合内 IP。
    ///
    /// 按 **IP 而非 IP:端口** 匹配:部分平台从与 SIP 监听端口不同的源端口发出站请求,
    /// 按端口严格匹配会误杀正常信令;而"限定到平台主机"已经消除了任意第三方注入。
    trusted_ips: std::sync::RwLock<Option<Arc<HashSet<IpAddr>>>>,
    stats: Arc<TransportStats>,
}

impl Router {
    pub(crate) fn new() -> Self {
        Router {
            routes: DashMap::new(),
            aor_routes: DashMap::new(),
            tracer: std::sync::RwLock::new(None),
            trusted_ips: std::sync::RwLock::new(None),
            stats: Arc::new(TransportStats::default()),
        }
    }

    /// 构造时原子安装非空来源白名单。
    ///
    /// 与 bind 后再调用 setter 不同，这保证 UDP 接收循环/TCP accept 循环第一次被调度时
    /// 策略已经存在；空集合报配置错误，绝不退化成“不设限”。
    pub(crate) fn new_with_trusted_ips(ips: impl IntoIterator<Item = IpAddr>) -> Result<Self> {
        let set: HashSet<IpAddr> = ips.into_iter().map(normalize_transport_ip).collect();
        if set.is_empty() {
            return Err(Error::Config("可信 SIP 来源 IP 集合不能为空".into()));
        }
        Ok(Router {
            routes: DashMap::new(),
            aor_routes: DashMap::new(),
            tracer: std::sync::RwLock::new(None),
            trusted_ips: std::sync::RwLock::new(Some(Arc::new(set))),
            stats: Arc::new(TransportStats::default()),
        })
    }

    pub(crate) fn stats(&self) -> Arc<TransportStats> {
        Arc::clone(&self.stats)
    }

    pub(crate) fn set_trusted_ips(&self, ips: impl IntoIterator<Item = IpAddr>) {
        let set: HashSet<IpAddr> = ips.into_iter().map(normalize_transport_ip).collect();
        if let Ok(mut guard) = self.trusted_ips.write() {
            *guard = if set.is_empty() {
                None
            } else {
                Some(Arc::new(set))
            };
        }
    }

    pub(crate) fn trust_only_host(&self, host: &str) -> bool {
        match host.parse::<IpAddr>() {
            Ok(ip) => {
                self.set_trusted_ips([ip]);
                true
            }
            Err(_) => false,
        }
    }

    pub(crate) fn source_allowed(&self, from: SocketAddr) -> bool {
        match self.trusted_ips.read() {
            Ok(guard) => match guard.as_ref() {
                Some(set) => set.contains(&normalize_transport_ip(from.ip())),
                None => true,
            },
            // 锁中毒时宁可放行也不要静默瘫痪信令;中毒本身会在别处报错。
            Err(_) => true,
        }
    }

    /// 来源被拒时的计数 + 限流日志(洪泛时不放大日志本身)。
    pub(crate) fn note_rejected_source(&self, from: SocketAddr) {
        let count = self.stats.rejected_source.fetch_add(1, Ordering::Relaxed) + 1;
        if count <= 5 || count % 1000 == 0 {
            tracing::warn!(%from, count, "丢弃来自非可信来源的 SIP 报文");
        }
    }

    /// UDP 来源被令牌桶拒绝时的计数 + 限流日志。
    ///
    /// 日志按来源攻击的**全局累计数**抽样,避免洪泛反过来灌爆日志。调用方必须静默丢包,
    /// 不回任何 SIP 错误响应,否则伪造源地址可借设备向受害者反射流量。
    pub(crate) fn note_rate_limited(&self, from: SocketAddr, bytes: usize) {
        let count = self.stats.rate_limited.fetch_add(1, Ordering::Relaxed) + 1;
        self.stats
            .rate_limited_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
        if count <= 5 || count % 1000 == 0 {
            tracing::warn!(%from, bytes, count, "SIP UDP 来源超过速率预算,静默丢弃");
        }
    }

    /// UDP 响应触发反放大保护时的计数 + 抽样日志。
    pub(crate) fn note_anti_amplification_blocked(
        &self,
        dst: SocketAddr,
        bytes: usize,
        reason: &'static str,
    ) {
        let count = self
            .stats
            .anti_amplification_blocked
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        if count <= 5 || count % 1000 == 0 {
            tracing::warn!(%dst, bytes, reason, count, "阻断可能造成 UDP 反射放大的 SIP 响应");
        }
    }

    /// 解析失败时的计数 + **限流**日志。
    ///
    /// 限流不是可选的:畸形包是攻击者能以最低成本大量制造的东西,每条一行 warn
    /// 会灌爆日志环形缓冲(2000 条)与推给界面的日志批次,把真正有用的日志挤掉 ——
    /// 相当于用日志把自己的可观测性 DoS 掉。与 `note_rejected_source` 同一档策略。
    pub(crate) fn note_parse_error(&self, from: SocketAddr, error: &common::Error) {
        let count = self.stats.parse_errors.fetch_add(1, Ordering::Relaxed) + 1;
        if count <= 5 || count % 1000 == 0 {
            tracing::warn!(%from, %error, count, "SIP 报文解析失败,丢弃");
        }
    }

    /// 入站连接数达上限被拒时的计数 + 限流日志。
    pub(crate) fn note_rejected_connection(&self, from: SocketAddr, limit: usize) {
        let count = self
            .stats
            .rejected_connections
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        if count <= 5 || count % 1000 == 0 {
            tracing::warn!(%from, limit, count, "入站 TCP 连接数已达上限,拒绝新连接");
        }
    }

    /// 接收循环异常退出时的计数。
    pub(crate) fn note_recv_error(&self) {
        self.stats.recv_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// 连接因超时被主动断开时的计数。
    pub(crate) fn note_connection_timeout(&self) {
        self.stats
            .timed_out_connections
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn set_tracer(&self, tracer: Option<Arc<dyn TraceObserver>>) {
        if let Ok(mut g) = self.tracer.write() {
            *g = tracer;
        }
    }

    pub(crate) fn trace(&self, dir: TraceDir, message: &SipMessage, peer: SocketAddr) {
        if let Ok(g) = self.tracer.read() {
            if let Some(t) = g.as_ref() {
                t.on_trace(SipTrace { dir, message, peer });
            }
        }
    }

    pub(crate) fn register(&self, call_id: &str) -> mpsc::Receiver<Incoming> {
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        self.routes.insert(call_id.to_string(), tx);
        rx
    }

    pub(crate) fn register_inbound(&self, aor: &str) -> mpsc::Receiver<Incoming> {
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        self.aor_routes.insert(aor.to_string(), tx);
        rx
    }

    pub(crate) fn register_inbound_many(&self, aors: Vec<String>) -> mpsc::Receiver<Incoming> {
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        for aor in aors {
            self.aor_routes.insert(aor, tx.clone());
        }
        rx
    }

    pub(crate) fn unregister(&self, call_id: &str) {
        self.routes.remove(call_id);
    }

    pub(crate) fn unregister_inbound(&self, aor: &str) {
        self.aor_routes.remove(aor);
    }

    pub(crate) fn unregister_inbound_many(&self, aors: &[String]) {
        for aor in aors {
            self.aor_routes.remove(aor);
        }
    }

    pub(crate) fn route_counts(&self) -> (usize, usize) {
        (self.routes.len(), self.aor_routes.len())
    }

    pub(crate) fn note_sent(&self) {
        self.stats.sent.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn note_send_error(&self) {
        self.stats.send_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn note_oversized(&self, size: usize, dst: SocketAddr) {
        self.stats.oversized_sent.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            size,
            limit = UDP_SAFE_MTU,
            %dst,
            "SIP 报文超过安全 MTU,可能被中间设备截断(大 body 应分片)"
        );
    }

    /// 投递到目标队列。队列满时丢弃并计数(本地背压,与平台失败区分开)。
    fn deliver(&self, tx: &mpsc::Sender<Incoming>, incoming: Incoming) {
        match tx.try_send(incoming) {
            Ok(()) => {
                self.stats.received.fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Full(dropped)) => {
                self.stats
                    .dropped_queue_full
                    .fetch_add(1, Ordering::Relaxed);
                tracing::debug!(from = %dropped.from, "设备入站队列已满,丢弃报文(本地背压)");
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.stats.unrouted.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// 分发。
    ///
    /// **请求优先按 AOR 路由**:对话内请求(BYE / re-SUBSCRIBE / 重传 INVITE)的 Call-ID
    /// 可能正好命中某个在跑的客户端事务队列,而事务等待循环会忽略请求 → 报文被静默吞掉。
    /// 因此请求先查 AOR,查不到再退回 Call-ID;响应始终按 Call-ID。
    pub(crate) fn dispatch(&self, incoming: Incoming) {
        self.trace(TraceDir::In, &incoming.message, incoming.from);

        let (call_id, aor) = match &incoming.message {
            SipMessage::Request(r) => (
                r.headers.call_id().map(str::to_string),
                uri_user(&r.uri).map(str::to_string),
            ),
            SipMessage::Response(r) => (r.headers.call_id().map(str::to_string), None),
        };

        if let Some(aor) = aor.as_deref() {
            if let Some(tx) = self.aor_routes.get(aor) {
                let tx = tx.clone();
                self.deliver(&tx, incoming);
                return;
            }
        }

        if let Some(id) = call_id.as_deref() {
            if let Some(tx) = self.routes.get(id) {
                let tx = tx.clone();
                self.deliver(&tx, incoming);
                return;
            }
        }

        self.stats.unrouted.fetch_add(1, Ordering::Relaxed);
        match &incoming.message {
            SipMessage::Request(r) => tracing::warn!(
                method = %r.method.as_str(), uri = %r.uri,
                "无匹配路由,丢弃入站请求"
            ),
            SipMessage::Response(r) => tracing::debug!(
                status = r.status,
                "无匹配 Call-ID 路由,丢弃响应(事务可能已结束)"
            ),
        }
    }
}

/// 将 IPv4-mapped IPv6 归一为 IPv4，避免同一平台因 socket 地址表示不同绕过/误触白名单。
fn normalize_transport_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

/// 从 SIP URI(如 `sip:34020000001320000001@host:5060`)提取 user 部分(AOR)。
///
/// 兼容 `sips:`、`user:password@host` 的 userinfo 形式,以及 user 后带参数的写法。
pub(crate) fn uri_user(uri: &str) -> Option<&str> {
    let after_scheme = uri
        .strip_prefix("sip:")
        .or_else(|| uri.strip_prefix("sips:"))
        .or_else(|| uri.strip_prefix("SIP:"))
        .or_else(|| uri.strip_prefix("SIPS:"))
        .unwrap_or(uri);
    // 没有 '@' 说明 URI 里没有 userinfo,整段是主机名,不能当 AOR。
    let userinfo = after_scheme.split('@').next()?;
    if userinfo.len() == after_scheme.len() {
        return None;
    }
    // userinfo 可能是 `user:password`,也可能被误带参数;取到第一个分隔符为止。
    let user = userinfo
        .split([':', ';', '?'])
        .next()
        .unwrap_or(userinfo)
        .trim();
    (!user.is_empty()).then_some(user)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_user_提取() {
        assert_eq!(
            uri_user("sip:34020000001320000001@h:5060"),
            Some("34020000001320000001")
        );
        assert_eq!(
            uri_user("34020000001320000001@h"),
            Some("34020000001320000001")
        );
        assert_eq!(uri_user("sip:@h"), None);
        // 无 userinfo 的 URI 不能把主机名当 AOR。
        assert_eq!(uri_user("sip:192.168.1.10:5060"), None);
        assert_eq!(uri_user("sips:1234@h"), Some("1234"));
        assert_eq!(uri_user("sip:user:pass@h"), Some("user"));
    }
}
