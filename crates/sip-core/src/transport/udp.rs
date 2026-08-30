//! UDP SIP 传输(GB28181 最常用)。
//!
//! 共享一个 socket 承载多台虚拟设备,收包后交给 [`Router`] 按 Call-ID / AOR 分发。

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use common::{Error, Result};

use crate::message::SipMessage;

use super::{
    Incoming, Router, SendFuture, Shutdown, SipTransport, TraceDir, TraceObserver, TransportStats,
    DEFAULT_SOCKET_BUFFER, UDP_SAFE_MTU,
};

/// 共享 UDP 传输。多设备复用一个 socket,按 Call-ID / AOR 分发收到的消息。
///
/// 用法:
/// 1. [`UdpTransport::bind`] 绑定本地端口并启动接收循环。
/// 2. [`SipTransport::trust_only_host`] 限定可信来源(强烈建议)。
/// 3. 每台设备用其 Call-ID 调 [`SipTransport::register`] 拿到接收端 `Receiver`。
/// 4. 发送用 [`SipTransport::send_to`];接收循环把消息按路由投递到对应队列。
pub struct UdpTransport {
    /// socket + 路由 + 关停信号。**接收循环只持有这个 core,不持有 `UdpTransport`** ——
    /// 否则循环会把传输对象钉死在内存里,socket 永不关闭(见 `SipTransport::shutdown` 文档)。
    core: Arc<UdpCore>,
}

/// 接收循环与发送路径共享的内部状态。
struct UdpCore {
    socket: UdpSocket,
    router: Router,
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

    /// 绑定并指定期望的收发缓冲大小。内核可能按 `net.core.rmem_max` 等上限截断,
    /// 截断只记日志不算失败——拿到多少用多少。
    pub async fn bind_with_buffer(local: &str, buffer_bytes: usize) -> Result<Arc<Self>> {
        let socket = UdpSocket::bind(local).await.map_err(Error::Io)?;
        super::udp::apply_socket_buffers(&socket, buffer_bytes);
        let core = Arc::new(UdpCore {
            socket,
            router: Router::new(),
            shutdown: Shutdown::new(),
        });
        spawn_recv_loop(Arc::clone(&core));
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
fn spawn_recv_loop(core: Arc<UdpCore>) {
    tokio::spawn(async move {
        let mut stop = core.shutdown.subscribe();
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
                    match SipMessage::parse(&buf[..n]) {
                        Ok(message) => core.router.dispatch(Incoming { message, from }),
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
            self.core.router.trace(TraceDir::Out, msg, dst);
            let bytes = msg.to_bytes();
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

    #[tokio::test]
    async fn 共享_socket_按_callid_路由() {
        // 起两个 transport 模拟"设备端"与"平台端"。
        let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let device_addr = device.local_addr().unwrap();

        // 设备为 Call-ID "call-A" 注册接收队列。
        let mut rx = device.register("call-A");

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

        let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let attacker = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let device_addr = device.local_addr().unwrap();
        let mut inbound = device.register_inbound("34020000001320000001");

        // 只信任一个不存在的平台地址,于是本机回环报文都应被拒。
        device.set_trusted_ips(vec!["203.0.113.7".parse::<IpAddr>().unwrap()]);

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
