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
    Incoming, Router, SendFuture, SipTransport, TraceDir, TraceObserver, TransportStats,
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
    socket: Arc<UdpSocket>,
    router: Router,
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
        let transport = Arc::new(UdpTransport {
            socket: Arc::new(socket),
            router: Router::new(),
        });
        transport.clone().spawn_recv_loop();
        Ok(transport)
    }

    /// 启动后台接收循环:收包 → 来源校验 → 解析 → 分发。
    fn spawn_recv_loop(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            loop {
                match self.socket.recv_from(&mut buf).await {
                    Ok((n, from)) => {
                        if !self.router.source_allowed(from) {
                            self.router.note_rejected_source(from);
                            continue;
                        }
                        match SipMessage::parse(&buf[..n]) {
                            Ok(message) => self.router.dispatch(Incoming { message, from }),
                            Err(e) => self.router.note_parse_error(from, &e),
                        }
                    }
                    Err(e) => {
                        tracing::error!(error=%e, "UDP 接收错误,接收循环退出");
                        break;
                    }
                }
            }
        });
    }
}

impl SipTransport for UdpTransport {
    fn protocol(&self) -> common::Transport {
        common::Transport::Udp
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        self.socket.local_addr().map_err(Error::Io)
    }

    fn register(&self, call_id: &str) -> mpsc::Receiver<Incoming> {
        self.router.register(call_id)
    }

    fn register_inbound(&self, aor: &str) -> mpsc::Receiver<Incoming> {
        self.router.register_inbound(aor)
    }

    fn register_inbound_many(&self, aors: Vec<String>) -> mpsc::Receiver<Incoming> {
        self.router.register_inbound_many(aors)
    }

    fn unregister(&self, call_id: &str) {
        self.router.unregister(call_id);
    }

    fn unregister_inbound(&self, aor: &str) {
        self.router.unregister_inbound(aor);
    }

    fn set_tracer(&self, tracer: Option<Arc<dyn TraceObserver>>) {
        self.router.set_tracer(tracer);
    }

    fn set_trusted_ips(&self, ips: Vec<IpAddr>) {
        self.router.set_trusted_ips(ips);
    }

    fn trust_only_host(&self, host: &str) -> bool {
        self.router.trust_only_host(host)
    }

    fn stats(&self) -> Arc<TransportStats> {
        self.router.stats()
    }

    fn send_to<'a>(&'a self, msg: &'a SipMessage, dst: SocketAddr) -> SendFuture<'a> {
        Box::pin(async move {
            self.router.trace(TraceDir::Out, msg, dst);
            let bytes = msg.to_bytes();
            if bytes.len() > UDP_SAFE_MTU {
                self.router.note_oversized(bytes.len(), dst);
            }
            match self.socket.send_to(&bytes, dst).await {
                Ok(_) => {
                    self.router.note_sent();
                    Ok(())
                }
                Err(e) => {
                    self.router.note_send_error();
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
    async fn 协议标识为_udp() {
        let t = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        assert_eq!(t.protocol(), common::Transport::Udp);
    }
}
