//! UDP SIP 传输(GB28181 最常用)。
//!
//! 共享一个 socket 承载多台虚拟设备,收包后交给 [`Router`] 按 Call-ID / AOR 分发。

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use common::{Error, Result};

use crate::message::SipMessage;

use super::{
    Incoming, Router, SendFuture, Shutdown, SipTransport, TraceDir, TraceObserver, TransportStats,
    DEFAULT_SOCKET_BUFFER, UDP_SAFE_MTU,
};

/// UDP 入站来源令牌桶配置。
///
/// 令牌按 `UDP_SAFE_MTU`(1300 B)等价报文计费,不是简单的每数据报一个令牌:
/// 65 KiB 数据报约消耗 51 个令牌,避免攻击者用最大包绕过 PPS 防护。桶按来源 IP
/// 聚合(不含端口),并有来源数和空闲回收上限,攻击者无法靠轮换源端口/地址把内存撑爆。
#[derive(Debug, Clone)]
pub struct UdpRateLimitConfig {
    /// 每秒补充的 MTU 等价令牌数。
    pub tokens_per_second: u32,
    /// 单来源最大突发令牌数。
    pub burst_tokens: u32,
    /// 同时追踪的来源 IP 上限。
    pub max_sources: usize,
    /// 来源多久无数据后回收桶。
    pub idle_ttl: Duration,
    /// 惰性扫描空闲桶的最小间隔。
    pub cleanup_interval: Duration,
}

impl Default for UdpRateLimitConfig {
    fn default() -> Self {
        Self {
            // 每个压测 socket 默认承载 64 台设备。400 的突发可容纳一次集中
            // REGISTER 鉴权两轮和合理重传,稳定 200/s 又能挡住解析洪泛。
            tokens_per_second: 200,
            burst_tokens: 400,
            max_sources: 4096,
            idle_ttl: Duration::from_secs(60),
            cleanup_interval: Duration::from_secs(10),
        }
    }
}

impl UdpRateLimitConfig {
    fn validate(&self) -> Result<()> {
        if self.tokens_per_second == 0
            || self.burst_tokens == 0
            || self.max_sources == 0
            || self.idle_ttl.is_zero()
            || self.cleanup_interval.is_zero()
        {
            return Err(Error::Sip(
                "UDP 限流配置必须使用非零速率、突发量、来源上限和时间窗口".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct SourceBucket {
    tokens: f64,
    last_refill: Instant,
    last_seen: Instant,
}

/// 单接收任务独占,无需锁；所有时间由调用方传入,单测不需要 sleep。
struct SourceRateLimiter {
    config: UdpRateLimitConfig,
    buckets: HashMap<IpAddr, SourceBucket>,
    next_cleanup: Instant,
}

impl SourceRateLimiter {
    fn new(config: UdpRateLimitConfig, now: Instant) -> Self {
        let next_cleanup = now + config.cleanup_interval;
        Self {
            config,
            buckets: HashMap::new(),
            next_cleanup,
        }
    }

    fn allow(&mut self, source: IpAddr, bytes: usize, now: Instant) -> bool {
        if now >= self.next_cleanup {
            self.cleanup(now);
        }
        let source = normalize_ip(source);
        let cost = bytes.div_ceil(UDP_SAFE_MTU).max(1) as f64;

        if !self.buckets.contains_key(&source) {
            if self.buckets.len() >= self.config.max_sources {
                self.cleanup(now);
                if self.buckets.len() >= self.config.max_sources {
                    return false;
                }
            }
            self.buckets.insert(
                source,
                SourceBucket {
                    tokens: self.config.burst_tokens as f64,
                    last_refill: now,
                    last_seen: now,
                },
            );
        }

        let bucket = self.buckets.get_mut(&source).expect("刚插入的来源桶");
        let elapsed = now.saturating_duration_since(bucket.last_refill);
        bucket.tokens = (bucket.tokens
            + elapsed.as_secs_f64() * self.config.tokens_per_second as f64)
            .min(self.config.burst_tokens as f64);
        bucket.last_refill = now;
        bucket.last_seen = now;
        if bucket.tokens < cost {
            return false;
        }
        bucket.tokens -= cost;
        true
    }

    fn cleanup(&mut self, now: Instant) {
        let idle_ttl = self.config.idle_ttl;
        self.buckets
            .retain(|_, bucket| now.saturating_duration_since(bucket.last_seen) < idle_ttl);
        self.next_cleanup = now + self.config.cleanup_interval;
    }
}

fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

const REPLY_CREDIT_FACTOR: usize = 3;
const MAX_REPLY_CREDIT_PER_PEER: usize = 64 * 1024;
const REPLY_CREDIT_TTL: Duration = Duration::from_secs(32);

#[derive(Debug)]
struct ReplyCredit {
    bytes: usize,
    last_seen: Instant,
}

/// UDP 请求到直接响应的短期字节信用。按完整返回路径(SocketAddr)绑定,不能被另一来源
/// 借用；信用总量不超过入站请求的 3 倍且有单来源上限。只保护 `Response`,设备主动发起的
/// REGISTER/MESSAGE/NOTIFY 是 `Request`,不应因没有入站信用而被误杀。
struct ReplyBudgets {
    credits: HashMap<SocketAddr, ReplyCredit>,
    max_peers: usize,
    last_cleanup: Instant,
}

impl ReplyBudgets {
    fn new(max_peers: usize) -> Self {
        Self {
            credits: HashMap::new(),
            max_peers,
            last_cleanup: Instant::now(),
        }
    }

    fn credit(&mut self, peer: SocketAddr, request_bytes: usize, now: Instant) {
        self.cleanup_if_due(now);
        let earned = request_bytes
            .saturating_mul(REPLY_CREDIT_FACTOR)
            .min(MAX_REPLY_CREDIT_PER_PEER);
        if let Some(credit) = self.credits.get_mut(&peer) {
            credit.bytes = credit
                .bytes
                .saturating_add(earned)
                .min(MAX_REPLY_CREDIT_PER_PEER);
            credit.last_seen = now;
        } else if self.credits.len() < self.max_peers {
            self.credits.insert(
                peer,
                ReplyCredit {
                    bytes: earned,
                    last_seen: now,
                },
            );
        }
    }

    fn consume(&mut self, peer: SocketAddr, response_bytes: usize, now: Instant) -> bool {
        self.cleanup_if_due(now);
        let Some(credit) = self.credits.get_mut(&peer) else {
            return false;
        };
        if credit.bytes < response_bytes {
            return false;
        }
        credit.bytes -= response_bytes;
        credit.last_seen = now;
        true
    }

    fn cleanup_if_due(&mut self, now: Instant) {
        if now.saturating_duration_since(self.last_cleanup) < Duration::from_secs(1) {
            return;
        }
        self.credits
            .retain(|_, credit| now.saturating_duration_since(credit.last_seen) < REPLY_CREDIT_TTL);
        self.last_cleanup = now;
    }
}

/// 共享 UDP 传输。多设备复用一个 socket,按 Call-ID / AOR 分发收到的消息。
///
/// 用法:
/// 1. 生产入口用 [`UdpTransport::bind_with_trusted_ips`] 绑定并原子安装可信来源；
///    [`UdpTransport::bind`] 是明确不设来源边界的低层/测试入口。
/// 2. 每台设备用其 Call-ID 调 [`SipTransport::register`] 拿到接收端 `Receiver`。
/// 3. 发送用 [`SipTransport::send_to`];接收循环把消息按路由投递到对应队列。
pub struct UdpTransport {
    /// socket + 路由 + 关停信号。**接收循环只持有这个 core,不持有 `UdpTransport`** ——
    /// 否则循环会把传输对象钉死在内存里,socket 永不关闭(见 `SipTransport::shutdown` 文档)。
    core: Arc<UdpCore>,
}

/// 接收循环与发送路径共享的内部状态。
struct UdpCore {
    socket: UdpSocket,
    router: Router,
    reply_budgets: Mutex<ReplyBudgets>,
    shutdown: Shutdown,
}

impl Drop for UdpTransport {
    fn drop(&mut self) {
        // 最后一个 Arc<UdpTransport> 消失即关停:调用方忘了显式 shutdown 也不会泄漏。
        self.core.shutdown.trigger();
    }
}

impl UdpTransport {
    /// 绑定本地地址(如 "0.0.0.0:0" 由系统分配端口),启动后台接收循环。
    pub async fn bind(local: &str) -> Result<Arc<Self>> {
        Self::bind_with_buffer(local, DEFAULT_SOCKET_BUFFER).await
    }

    /// 绑定并在接收循环启动前原子安装非空可信来源集合。
    pub async fn bind_with_trusted_ips(local: &str, trusted_ips: Vec<IpAddr>) -> Result<Arc<Self>> {
        Self::bind_inner(
            local,
            DEFAULT_SOCKET_BUFFER,
            UdpRateLimitConfig::default(),
            Some(trusted_ips),
        )
        .await
    }

    /// 绑定并指定期望的收发缓冲大小。内核可能按 `net.core.rmem_max` 等上限截断,
    /// 截断只记日志不算失败——拿到多少用多少。
    pub async fn bind_with_buffer(local: &str, buffer_bytes: usize) -> Result<Arc<Self>> {
        Self::bind_with_rate_limit(local, buffer_bytes, UdpRateLimitConfig::default()).await
    }

    /// 绑定并指定 socket 缓冲与来源限流策略。主要供不同规模部署调优及确定性测试使用。
    /// 白名单与限流是叠加关系:先拒绝白名单外来源,命中白名单的来源仍必须通过令牌桶。
    pub async fn bind_with_rate_limit(
        local: &str,
        buffer_bytes: usize,
        rate_limit: UdpRateLimitConfig,
    ) -> Result<Arc<Self>> {
        Self::bind_inner(local, buffer_bytes, rate_limit, None).await
    }

    async fn bind_inner(
        local: &str,
        buffer_bytes: usize,
        rate_limit: UdpRateLimitConfig,
        trusted_ips: Option<Vec<IpAddr>>,
    ) -> Result<Arc<Self>> {
        rate_limit.validate()?;
        let max_sources = rate_limit.max_sources;
        let socket = UdpSocket::bind(local).await.map_err(Error::Io)?;
        super::udp::apply_socket_buffers(&socket, buffer_bytes);
        let router = match trusted_ips {
            Some(ips) => Router::new_with_trusted_ips(ips)?,
            None => Router::new(),
        };
        let core = Arc::new(UdpCore {
            socket,
            router,
            reply_budgets: Mutex::new(ReplyBudgets::new(max_sources)),
            shutdown: Shutdown::new(),
        });
        spawn_recv_loop(Arc::clone(&core), rate_limit);
        Ok(Arc::new(UdpTransport { core }))
    }

    /// 本次传输是否已关停。
    pub fn is_closed(&self) -> bool {
        self.core.shutdown.is_closed()
    }
}

/// 后台接收循环:收包 → 来源校验 → 解析 → 分发,直到关停或 socket 出错。
///
/// 参数是 `Arc<UdpCore>` 而不是 `Arc<UdpTransport>`:循环退出后这个 Arc 随任务一起
/// 释放,socket 才真的关掉。关停信号是唯一的正常退出路径。
fn spawn_recv_loop(core: Arc<UdpCore>, rate_limit: UdpRateLimitConfig) {
    tokio::spawn(async move {
        let mut stop = core.shutdown.subscribe();
        let mut limiter = SourceRateLimiter::new(rate_limit, Instant::now());
        let mut buf = vec![0u8; 65535];
        loop {
            let received = tokio::select! {
                _ = stop.wait() => {
                    tracing::debug!(
                        local = ?core.socket.local_addr().ok(),
                        "UDP 传输已关停,接收循环退出"
                    );
                    return;
                }
                r = core.socket.recv_from(&mut buf) => r,
            };
            match received {
                Ok((n, from)) => {
                    if !core.router.source_allowed(from) {
                        core.router.note_rejected_source(from);
                        continue;
                    }
                    // 必须在解析和 tracer 之前扣预算:被攻击流量不应消耗 SIP 解析 CPU,
                    // 也不应进入日志/UI。超限一律静默丢弃,绝不回错误响应以免反射放大。
                    if !limiter.allow(from.ip(), n, Instant::now()) {
                        core.router.note_rate_limited(from, n);
                        continue;
                    }
                    match SipMessage::parse(&buf[..n]) {
                        Ok(message) => {
                            if matches!(message, SipMessage::Request(_)) {
                                core.reply_budgets.lock().credit(from, n, Instant::now());
                            }
                            core.router.dispatch(Incoming { message, from });
                        }
                        Err(e) => core.router.note_parse_error(from, &e),
                    }
                }
                Err(error) => {
                    // 收包彻底失败(socket 被关等)时这个 socket 上的所有设备都将失聪,
                    // 所以除了日志还要计数 —— 否则现象是"设备莫名不再响应平台"却无从查证。
                    core.router.note_recv_error();
                    tracing::error!(%error, "UDP 接收错误,接收循环退出");
                    return;
                }
            }
        }
    });
}

impl SipTransport for UdpTransport {
    fn protocol(&self) -> common::Transport {
        common::Transport::Udp
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        self.core.socket.local_addr().map_err(Error::Io)
    }

    fn register(&self, call_id: &str) -> mpsc::Receiver<Incoming> {
        self.core.router.register(call_id)
    }

    fn register_inbound(&self, aor: &str) -> mpsc::Receiver<Incoming> {
        self.core.router.register_inbound(aor)
    }

    fn register_inbound_many(&self, aors: Vec<String>) -> mpsc::Receiver<Incoming> {
        self.core.router.register_inbound_many(aors)
    }

    fn unregister(&self, call_id: &str) {
        self.core.router.unregister(call_id);
    }

    fn unregister_inbound(&self, aor: &str) {
        self.core.router.unregister_inbound(aor);
    }

    fn unregister_inbound_many(&self, aors: &[String]) {
        self.core.router.unregister_inbound_many(aors);
    }

    fn route_counts(&self) -> (usize, usize) {
        self.core.router.route_counts()
    }

    fn set_tracer(&self, tracer: Option<Arc<dyn TraceObserver>>) {
        self.core.router.set_tracer(tracer);
    }

    fn set_trusted_ips(&self, ips: Vec<IpAddr>) {
        self.core.router.set_trusted_ips(ips);
    }

    fn trust_only_host(&self, host: &str) -> bool {
        self.core.router.trust_only_host(host)
    }

    fn stats(&self) -> Arc<TransportStats> {
        self.core.router.stats()
    }

    fn shutdown(&self) {
        self.core.shutdown.trigger();
    }

    fn send_to<'a>(&'a self, msg: &'a SipMessage, dst: SocketAddr) -> SendFuture<'a> {
        Box::pin(async move {
            if self.core.shutdown.is_closed() {
                return Err(Error::Sip("UDP 传输已关停".into()));
            }
            let bytes = msg.to_bytes();
            if matches!(msg, SipMessage::Response(_)) {
                if bytes.len() > UDP_SAFE_MTU {
                    self.core.router.note_anti_amplification_blocked(
                        dst,
                        bytes.len(),
                        "响应超过 UDP 安全 MTU",
                    );
                    return Err(Error::Sip(format!(
                        "UDP 响应 {} 字节超过安全 MTU {},已阻断",
                        bytes.len(),
                        UDP_SAFE_MTU
                    )));
                }
                if !self
                    .core
                    .reply_budgets
                    .lock()
                    .consume(dst, bytes.len(), Instant::now())
                {
                    self.core.router.note_anti_amplification_blocked(
                        dst,
                        bytes.len(),
                        "无对应入站请求字节信用",
                    );
                    return Err(Error::Sip("UDP 响应缺少对应入站请求字节信用,已阻断".into()));
                }
            }
            self.core.router.trace(TraceDir::Out, msg, dst);
            if bytes.len() > UDP_SAFE_MTU {
                self.core.router.note_oversized(bytes.len(), dst);
            }
            match self.core.socket.send_to(&bytes, dst).await {
                Ok(_) => {
                    self.core.router.note_sent();
                    Ok(())
                }
                Err(e) => {
                    self.core.router.note_send_error();
                    Err(Error::Io(e))
                }
            }
        })
    }
}

/// 放大 socket 收发缓冲。失败只告警:降级到内核默认仍可工作,只是高并发下更易丢包。
pub(crate) fn apply_socket_buffers(socket: &UdpSocket, bytes: usize) {
    // socket2 需要所有权语义的 Socket;这里借底层句柄构造后立刻归还,
    // 避免 Drop 关掉 tokio 仍在使用的 socket。
    #[cfg(unix)]
    let sock = {
        use std::os::fd::{AsRawFd, FromRawFd};
        unsafe { socket2::Socket::from_raw_fd(socket.as_raw_fd()) }
    };
    #[cfg(windows)]
    let sock = {
        use std::os::windows::io::{AsRawSocket, FromRawSocket};
        unsafe { socket2::Socket::from_raw_socket(socket.as_raw_socket()) }
    };

    if let Err(error) = sock.set_recv_buffer_size(bytes) {
        tracing::warn!(%error, bytes, "设置 SO_RCVBUF 失败,沿用内核默认");
    }
    if let Err(error) = sock.set_send_buffer_size(bytes) {
        tracing::warn!(%error, bytes, "设置 SO_SNDBUF 失败,沿用内核默认");
    }

    // 归还句柄所有权,阻止 socket2 的 Drop 关闭它。
    #[cfg(unix)]
    {
        use std::os::fd::IntoRawFd;
        let _ = sock.into_raw_fd();
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawSocket;
        let _ = sock.into_raw_socket();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Headers, Response};
    use std::sync::atomic::Ordering;

    fn test_rate_limit(tokens_per_second: u32, burst_tokens: u32) -> UdpRateLimitConfig {
        UdpRateLimitConfig {
            tokens_per_second,
            burst_tokens,
            max_sources: 4,
            idle_ttl: Duration::from_secs(10),
            cleanup_interval: Duration::from_secs(1),
        }
    }

    #[test]
    fn 来源桶按字节计费_可补充但不超过突发上限() {
        let start = Instant::now();
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        let mut limiter = SourceRateLimiter::new(test_rate_limit(2, 3), start);

        assert!(limiter.allow(ip, UDP_SAFE_MTU, start));
        assert!(limiter.allow(ip, UDP_SAFE_MTU + 1, start)); // 消耗 2,桶清空
        assert!(!limiter.allow(ip, 1, start));
        assert!(limiter.allow(ip, 1, start + Duration::from_millis(500)));
        assert!(!limiter.allow(ip, 1, start + Duration::from_millis(500)));
        // 很久后只补到 burst=3;最大 UDP 包需要约 51 token,仍必须被拒。
        assert!(!limiter.allow(ip, 65_535, start + Duration::from_secs(100)));
    }

    #[test]
    fn 来源桶有界_空闲后回收_映射地址归一化() {
        let start = Instant::now();
        let mut config = test_rate_limit(1, 1);
        config.max_sources = 1;
        let mut limiter = SourceRateLimiter::new(config, start);
        let v4: IpAddr = "192.0.2.9".parse().unwrap();
        let mapped: IpAddr = "::ffff:192.0.2.9".parse().unwrap();
        let other: IpAddr = "198.51.100.1".parse().unwrap();

        assert!(limiter.allow(v4, 1, start));
        assert!(
            !limiter.allow(mapped, 1, start),
            "映射 IPv6 必须与 IPv4 共用桶"
        );
        assert!(
            !limiter.allow(other, 1, start),
            "表满时未知来源不得分配内存"
        );
        assert_eq!(limiter.buckets.len(), 1);
        assert!(limiter.allow(other, 1, start + Duration::from_secs(11)));
        assert_eq!(limiter.buckets.len(), 1);
    }

    #[tokio::test]
    async fn udp_超限在解析前静默丢弃且统计字节() {
        use crate::message::{Method, Request};

        let config = test_rate_limit(1, 2);
        let device =
            UdpTransport::bind_with_rate_limit("127.0.0.1:0", DEFAULT_SOCKET_BUFFER, config)
                .await
                .unwrap();
        let sender = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let mut inbound = device.register_inbound("34020000001320000001");
        let mut headers = Headers::new();
        headers.set("Call-ID", "rate-limit");
        headers.set("CSeq", "1 OPTIONS");
        let req = SipMessage::Request(Request {
            method: Method::Options,
            uri: "sip:34020000001320000001@127.0.0.1".into(),
            headers,
            body: Vec::new(),
        });
        let wire_len = req.to_bytes().len() as u64;
        for _ in 0..3 {
            sender
                .send_to(&req, device.local_addr().unwrap())
                .await
                .unwrap();
        }

        for _ in 0..2 {
            tokio::time::timeout(Duration::from_secs(1), inbound.recv())
                .await
                .expect("预算内报文应送达")
                .expect("队列不应关闭");
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(150), inbound.recv())
                .await
                .is_err(),
            "超限报文必须静默丢弃"
        );
        let stats = device.stats().snapshot();
        assert_eq!(stats.rate_limited, 1);
        assert_eq!(stats.rate_limited_bytes, wire_len);
        assert_eq!(stats.parse_errors, 0);
    }

    #[tokio::test]
    async fn udp_无请求信用与超_mtu_响应均被阻断() {
        use crate::message::{Method, Request};

        let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let peer = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = peer.local_addr().unwrap();
        let mut response_headers = Headers::new();
        response_headers.set("Call-ID", "anti-amplification");
        response_headers.set("CSeq", "1 OPTIONS");
        let response = SipMessage::Response(Response {
            status: 200,
            reason: "OK".into(),
            headers: response_headers.clone(),
            body: Vec::new(),
        });
        assert!(device.send_to(&response, peer_addr).await.is_err());
        assert_eq!(device.stats().snapshot().anti_amplification_blocked, 1);

        // 有请求信用后,普通响应可发,但超过安全 MTU 的响应仍必须硬阻断。
        let mut inbound = device.register("anti-amplification");
        let request = SipMessage::Request(Request {
            method: Method::Options,
            uri: "sip:device@127.0.0.1".into(),
            headers: response_headers,
            body: Vec::new(),
        });
        peer.send_to(&request, device.local_addr().unwrap())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), inbound.recv())
            .await
            .expect("设备应收到请求")
            .expect("设备路由不应关闭");

        let mut oversized = match response {
            SipMessage::Response(response) => response,
            _ => unreachable!(),
        };
        oversized.body = vec![b'x'; UDP_SAFE_MTU];
        assert!(device
            .send_to(&SipMessage::Response(oversized), peer_addr)
            .await
            .is_err());
        assert_eq!(device.stats().snapshot().anti_amplification_blocked, 2);
    }

    #[tokio::test]
    async fn 共享_socket_按_callid_路由() {
        // 起两个 transport 模拟"设备端"与"平台端"。
        let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let device_addr = device.local_addr().unwrap();

        // 设备为 Call-ID "call-A" 注册接收队列。
        let mut rx = device.register("call-A");

        // UDP 响应必须对应一条真实入站请求。先让设备向平台发请求并等待它到达,
        // 为平台建立绑定返回路径的字节信用；"凭空"响应会被反放大策略阻断。
        let mut platform_rx = platform.register("call-A");
        let mut request_headers = Headers::new();
        request_headers.set("Call-ID", "call-A");
        request_headers.set("CSeq", "1 OPTIONS");
        let request = SipMessage::Request(crate::message::Request {
            method: crate::message::Method::Options,
            uri: "sip:platform@127.0.0.1".into(),
            headers: request_headers,
            body: Vec::new(),
        });
        device
            .send_to(&request, platform.local_addr().unwrap())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), platform_rx.recv())
            .await
            .expect("平台应先收到请求")
            .expect("平台路由不应关闭");

        // 平台发一条带该 Call-ID 的 200 OK 给设备。
        let mut headers = Headers::new();
        headers.set("Call-ID", "call-A");
        headers.set("CSeq", "1 REGISTER");
        let resp = SipMessage::Response(Response {
            status: 200,
            reason: "OK".into(),
            headers,
            body: Vec::new(),
        });
        platform.send_to(&resp, device_addr).await.unwrap();

        // 设备应在 call-A 队列收到它。
        let got = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("超时未收到")
            .expect("队列关闭");
        match got.message {
            SipMessage::Response(r) => assert_eq!(r.status, 200),
            _ => panic!("应为响应"),
        }
    }

    #[tokio::test]
    async fn 入站请求按_aor_路由() {
        use crate::message::{Method, Request};

        let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let device_addr = device.local_addr().unwrap();

        // 设备按自身 AOR(device_id)注册入站队列。
        let mut inbound = device.register_inbound("34020000001320000001");

        // 平台发来 OPTIONS(新 Call-ID,Request-URI 指向该设备 AOR)。
        let mut headers = Headers::new();
        headers.set("Call-ID", "fresh-call-from-platform");
        headers.set("CSeq", "1 OPTIONS");
        let req = SipMessage::Request(Request {
            method: Method::Options,
            uri: "sip:34020000001320000001@127.0.0.1:5060".into(),
            headers,
            body: Vec::new(),
        });
        platform.send_to(&req, device_addr).await.unwrap();

        let got = tokio::time::timeout(std::time::Duration::from_secs(2), inbound.recv())
            .await
            .expect("超时未收到")
            .expect("队列关闭");
        match got.message {
            SipMessage::Request(r) => assert_eq!(r.method, Method::Options),
            _ => panic!("应为请求"),
        }
    }

    #[tokio::test]
    async fn 对话内请求优先走_aor_不被事务队列吞掉() {
        use crate::message::{Method, Request};

        let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let device_addr = device.local_addr().unwrap();

        // 设备同时持有:同一 Call-ID 的客户端事务队列 + AOR 入站队列。
        // 这正是 NOTIFY 事务在跑时平台发来 BYE 的场景。
        let mut txn_rx = device.register("dialog-1");
        let mut inbound = device.register_inbound("34020000001320000001");

        let mut headers = Headers::new();
        headers.set("Call-ID", "dialog-1");
        headers.set("CSeq", "9 BYE");
        let req = SipMessage::Request(Request {
            method: Method::Bye,
            uri: "sip:34020000001320000001@127.0.0.1:5060".into(),
            headers,
            body: Vec::new(),
        });
        platform.send_to(&req, device_addr).await.unwrap();

        let got = tokio::time::timeout(std::time::Duration::from_secs(2), inbound.recv())
            .await
            .expect("BYE 应投到 AOR 入站队列")
            .expect("队列关闭");
        match got.message {
            SipMessage::Request(r) => assert_eq!(r.method, Method::Bye),
            _ => panic!("应为请求"),
        }
        // 事务队列不应拿到这条请求。
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), txn_rx.recv())
                .await
                .is_err(),
            "请求不应被投进客户端事务队列"
        );
    }

    #[tokio::test]
    async fn 非可信来源被丢弃且计数() {
        use crate::message::{Method, Request};

        // 构造器在接收循环启动前原子安装白名单；不存在配置窗口。
        let device = UdpTransport::bind_with_trusted_ips(
            "127.0.0.1:0",
            vec!["203.0.113.7".parse::<IpAddr>().unwrap()],
        )
        .await
        .unwrap();
        let attacker = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let device_addr = device.local_addr().unwrap();
        let mut inbound = device.register_inbound("34020000001320000001");

        let mut headers = Headers::new();
        headers.set("Call-ID", "evil-1");
        headers.set("CSeq", "1 MESSAGE");
        let req = SipMessage::Request(Request {
            method: Method::Message,
            uri: "sip:34020000001320000001@127.0.0.1:5060".into(),
            headers,
            body: Vec::new(),
        });
        attacker.send_to(&req, device_addr).await.unwrap();

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), inbound.recv())
                .await
                .is_err(),
            "非可信来源的请求不应进入设备队列"
        );
        assert_eq!(device.stats().snapshot().rejected_source, 1);

        // 放开白名单后同一条报文应可送达。
        device.set_trusted_ips(Vec::new());
        attacker.send_to(&req, device_addr).await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), inbound.recv())
                .await
                .is_ok(),
            "关闭过滤后应能收到"
        );
    }

    #[tokio::test]
    async fn 原子可信来源构造器拒绝空集合() {
        assert!(
            UdpTransport::bind_with_trusted_ips("127.0.0.1:0", Vec::new())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn 队列满时丢弃并计数为本地背压() {
        use crate::message::{Method, Request};

        let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let device_addr = device.local_addr().unwrap();
        // 拿到 Receiver 但从不消费,填满后应记 dropped_queue_full 而非无界堆积。
        let _inbound = device.register_inbound("34020000001320000001");

        let mut headers = Headers::new();
        headers.set("Call-ID", "flood");
        headers.set("CSeq", "1 MESSAGE");
        let req = SipMessage::Request(Request {
            method: Method::Message,
            uri: "sip:34020000001320000001@127.0.0.1:5060".into(),
            headers,
            body: Vec::new(),
        });
        for _ in 0..(super::super::QUEUE_CAPACITY + 40) {
            platform.send_to(&req, device_addr).await.unwrap();
        }
        // UDP 本身可能丢包,只断言"出现了本地背压丢弃"而非精确条数。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if device.stats().snapshot().dropped_queue_full > 0 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "未观察到队列满丢弃");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn 追踪观察者捕获收发方向() {
        use crate::message::{Method, Request};
        use crate::transport::{SipTrace, TraceDir};
        use std::sync::atomic::AtomicUsize;

        #[derive(Default)]
        struct Counter {
            ins: AtomicUsize,
            outs: AtomicUsize,
        }
        impl TraceObserver for Counter {
            fn on_trace(&self, t: SipTrace<'_>) {
                match t.dir {
                    TraceDir::In => self.ins.fetch_add(1, Ordering::Relaxed),
                    TraceDir::Out => self.outs.fetch_add(1, Ordering::Relaxed),
                };
            }
        }

        let sender = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let receiver = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let dst = receiver.local_addr().unwrap();

        let sc = Arc::new(Counter::default());
        let rc = Arc::new(Counter::default());
        sender.set_tracer(Some(sc.clone()));
        receiver.set_tracer(Some(rc.clone()));
        let mut rx = receiver.register("call-trace");

        let mut h = Headers::new();
        h.set("Call-ID", "call-trace");
        h.set("CSeq", "1 MESSAGE");
        let req = SipMessage::Request(Request {
            method: Method::Message,
            uri: "sip:x@h".into(),
            headers: h,
            body: Vec::new(),
        });
        sender.send_to(&req, dst).await.unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await;

        assert_eq!(sc.outs.load(Ordering::Relaxed), 1, "发送侧应记一条 out");
        assert_eq!(sc.ins.load(Ordering::Relaxed), 0);
        assert_eq!(rc.ins.load(Ordering::Relaxed), 1, "接收侧应记一条 in");

        // 关闭追踪后不再回调。
        sender.set_tracer(None);
        sender.send_to(&req, dst).await.unwrap();
        assert_eq!(sc.outs.load(Ordering::Relaxed), 1, "关闭后不应再增");
    }

    #[tokio::test]
    async fn 关停后端口被释放且发送快速失败() {
        // 这是本次修复的核心不变量:接收循环不能把传输对象钉死。
        // 之前 spawn_recv_loop 把 Arc<Self> move 进"只在出错时才 break"的循环,
        // 于是 socket 永不关闭 —— 压测按 64 台设备一个 socket 分片,
        // 一万台设备每跑一轮泄漏 150 多个 fd,跑十轮就撞 ulimit -n。
        let t = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let addr = t.local_addr().unwrap();
        assert!(!t.is_closed());

        t.shutdown();
        assert!(t.is_closed());
        // 关停后发送应快速失败,不该挂住调用方。
        let mut h = Headers::new();
        h.set("Call-ID", "x");
        h.set("CSeq", "1 MESSAGE");
        let msg = SipMessage::Response(Response {
            status: 200,
            reason: "OK".into(),
            headers: h,
            body: Vec::new(),
        });
        assert!(t.send_to(&msg, addr).await.is_err(), "关停后不应还能发送");

        // 释放最后一个 Arc 后接收任务应退出并归还端口。
        drop(t);
        let mut rebound = None;
        for _ in 0..100 {
            if let Ok(again) = UdpSocket::bind(addr).await {
                rebound = Some(again);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            rebound.is_some(),
            "关停后原端口应可重新绑定(socket 已真的关闭)"
        );
    }

    #[tokio::test]
    async fn 传输对象被丢弃时也会自动关停() {
        // 调用方忘了显式 shutdown 时靠 Drop 兜底。
        let addr = {
            let t = UdpTransport::bind("127.0.0.1:0").await.unwrap();
            t.local_addr().unwrap()
        };
        let mut ok = false;
        for _ in 0..100 {
            if UdpSocket::bind(addr).await.is_ok() {
                ok = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(ok, "Arc 归零后应自动关停并释放端口");
    }

    #[tokio::test]
    async fn 畸形包解析错误日志被限流但计数完整() {
        // 畸形包是攻击者能以最低成本大量制造的东西。计数必须每条都涨(用于诊断),
        // 但日志必须限流 —— 否则会把 2000 条环形缓冲和推给界面的日志批次挤满,
        // 相当于用日志把自己的可观测性 DoS 掉。
        let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let attacker = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = device.local_addr().unwrap();
        const N: u64 = 50;
        for _ in 0..N {
            attacker
                .send_to(b"not a sip message at all", addr)
                .await
                .unwrap();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if device.stats().snapshot().parse_errors >= N {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "解析错误计数未达预期");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // 没有断言"日志条数",那需要装订阅者;这里锁的是计数不因限流而丢。
        assert!(device.stats().snapshot().parse_errors >= N);
    }

    #[tokio::test]
    async fn 协议标识为_udp() {
        let t = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        assert_eq!(t.protocol(), common::Transport::Udp);
    }
}
