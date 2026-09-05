//! SIP over TCP 传输(RFC 3261 §18.2/§18.3)。
//!
//! 之前这一层完全没有实现,而配置里允许选 TCP —— `builder` 会把 `SIP/2.0/TCP`
//! 写进 Via,报文却从 UDP socket 发出去,平台按 Via 回包就走错路径。现在
//! 传输标识由传输对象自己给出([`SipTransport::protocol`]),不可能再对不上。
//!
//! 与 UDP 的关键差别:
//! - **流式重组**:TCP 没有报文边界,必须按 Content-Length 切分。依赖
//!   [`crate::message::parse_incremental`] 返回 `Incomplete` 才能做到 ——
//!   这也是为什么解析器不能在 body 不足时"静默截断"。
//! - **连接复用**:按对端地址缓存连接;设备侧发出的请求与平台的响应走同一条连接。
//! - **被动接入**:同时监听本地端口,接受平台主动建立的连接(平台下发 INVITE 等)。
//! - **无 MTU 限制**:大 body(整棵目录树)在 TCP 上不需要分片。

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};

use common::{Error, Result};

use crate::message::{parse_incremental, ParseOutcome, SipMessage, MAX_HEAD_LEN};

use super::{
    Incoming, Router, SendFuture, Shutdown, SipTransport, TraceDir, TraceObserver, TransportStats,
};

/// 单条连接的读缓冲上限。超过说明对端在灌垃圾或 Content-Length 撒谎,直接断开。
const MAX_CONNECTION_BUFFER: usize = 4 * 1024 * 1024;

/// 同时保持的**入站**连接数上限。
///
/// accept 循环之前是无限接受的:每条连接占一个 fd、一个 tokio 任务、
/// 一个 16 KiB 读块加一个 8 KiB 缓冲,而 `connections` 表也随之无界增长。
/// 攻击者(白名单开启时是能完成 TCP 握手的同网段主机)反复建连即可耗尽 fd,
/// 让整个进程连不上平台。64 条对"一个平台 + 少量级联"绰绰有余。
const MAX_INBOUND_CONNECTIONS: usize = 64;

/// 建连后必须在这么久内收到**第一条完整报文**,否则断开。
///
/// 这一条专治 slowloris:建连后不发字节(或一个字节一个字节地发),
/// 原先的 `read()` 没有超时,任务会永久挂住并一直占着 fd 与连接名额。
/// 30 秒对任何真实平台的首条报文都够宽裕。
const FIRST_MESSAGE_TIMEOUT: Duration = Duration::from_secs(30);

/// 已经收过报文的连接,空闲这么久后回收。
///
/// 取值刻意宽松:SIP over TCP 连接在两次心跳之间静默是完全正常的,
/// 这条只用来回收"对端已经消失但 FIN 丢了"的死连接,不该干扰正常长连接。
const IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// 共享 TCP 传输:一个监听端口 + 一组到平台的出站连接,多设备复用。
pub struct TcpTransport {
    /// accept 循环与各读循环**只持有这个 core**,不持有 `TcpTransport` ——
    /// 否则那些"只在出错时才 break"的循环会把传输对象钉死,
    /// 监听 socket 与全部连接永不释放(见 `SipTransport::shutdown` 文档)。
    core: Arc<TcpCore>,
    /// 本地监听地址(写进 Via/Contact)。
    local_addr: SocketAddr,
}

/// accept / 读循环 / 发送路径共享的内部状态。
struct TcpCore {
    router: Arc<Router>,
    /// 对端地址 → 该连接的写半边。出站请求与后续响应共用同一条连接。
    connections: Mutex<HashMap<SocketAddr, Arc<Mutex<OwnedWriteHalf>>>>,
    /// 当前保持的入站连接数(与 [`MAX_INBOUND_CONNECTIONS`] 比较)。
    inbound_count: AtomicUsize,
    shutdown: Shutdown,
}

impl Drop for TcpTransport {
    fn drop(&mut self) {
        self.core.shutdown.trigger();
    }
}

impl TcpTransport {
    /// 绑定本地监听地址(如 "0.0.0.0:0" 由系统分配端口),启动 accept 循环。
    pub async fn bind(local: &str) -> Result<Arc<Self>> {
        Self::bind_inner(local, None).await
    }

    /// 绑定并在 accept 循环启动前原子安装非空可信来源集合。
    ///
    /// 这避免 `bind()` 后再调用 setter 时，竞态窗口中的恶意连接被长期保留。
    pub async fn bind_with_trusted_ips(local: &str, trusted_ips: Vec<IpAddr>) -> Result<Arc<Self>> {
        Self::bind_inner(local, Some(trusted_ips)).await
    }

    async fn bind_inner(local: &str, trusted_ips: Option<Vec<IpAddr>>) -> Result<Arc<Self>> {
        let router = match trusted_ips {
            Some(ips) => Router::new_with_trusted_ips(ips)?,
            None => Router::new(),
        };
        let listener = TcpListener::bind(local).await.map_err(Error::Io)?;
        let local_addr = listener.local_addr().map_err(Error::Io)?;
        let core = Arc::new(TcpCore {
            router: Arc::new(router),
            connections: Mutex::new(HashMap::new()),
            inbound_count: AtomicUsize::new(0),
            shutdown: Shutdown::new(),
        });
        spawn_accept_loop(Arc::clone(&core), listener);
        Ok(Arc::new(TcpTransport { core, local_addr }))
    }

    /// 本次传输是否已关停。
    pub fn is_closed(&self) -> bool {
        self.core.shutdown.is_closed()
    }

    /// 当前保持的入站连接数(诊断用)。
    pub fn inbound_connections(&self) -> usize {
        self.core.inbound_count.load(AtomicOrdering::Relaxed)
    }
}

/// 接受平台主动建立的连接(平台侧下发 INVITE / 查询时会这样)。
fn spawn_accept_loop(core: Arc<TcpCore>, listener: TcpListener) {
    tokio::spawn(async move {
        let mut stop = core.shutdown.subscribe();
        loop {
            let accepted = tokio::select! {
                _ = stop.wait() => {
                    tracing::debug!("TCP 传输已关停,监听循环退出");
                    return;
                }
                r = listener.accept() => r,
            };
            match accepted {
                Ok((stream, peer)) => {
                    if !core.router.source_allowed(peer) {
                        core.router.note_rejected_source(peer);
                        continue;
                    }
                    // 先看名额再 adopt:超限时立刻 drop stream(等于回 RST/FIN),
                    // 不给对方留下"连上了但没人理"的挂起连接。
                    if core.inbound_count.load(AtomicOrdering::Relaxed) >= MAX_INBOUND_CONNECTIONS {
                        core.router
                            .note_rejected_connection(peer, MAX_INBOUND_CONNECTIONS);
                        drop(stream);
                        continue;
                    }
                    tracing::info!(%peer, "接受平台 SIP over TCP 连接");
                    adopt(Arc::clone(&core), stream, peer).await;
                }
                Err(error) => {
                    core.router.note_recv_error();
                    tracing::error!(%error, "TCP accept 失败,监听循环退出");
                    return;
                }
            }
        }
    });
}

/// 把一条已建立的连接拆成读写两半:写半边入表复用,读半边起流式重组循环。
async fn adopt(core: Arc<TcpCore>, stream: TcpStream, peer: SocketAddr) {
    stream.set_nodelay(true).ok();
    let (read_half, write_half) = stream.into_split();
    core.connections
        .lock()
        .await
        .insert(peer, Arc::new(Mutex::new(write_half)));
    core.inbound_count.fetch_add(1, AtomicOrdering::Relaxed);
    spawn_read_loop(core, read_half, peer, true);
}

/// 单连接读循环:累积字节 → 增量解析 → 分发 → 推进游标。
///
/// `inbound` 标记这条连接是平台连进来的(需要占用入站名额并在退出时归还)。
/// 入站与出站连接**共用同一份实现** —— 之前是两份近乎重复的代码,结果出站那份
/// 漏掉了退出时清理 `connections`,也漏掉了头部长度兜底,写死的连接会一直留在表里。
fn spawn_read_loop(
    core: Arc<TcpCore>,
    mut read_half: OwnedReadHalf,
    peer: SocketAddr,
    inbound: bool,
) {
    {
        let router = Arc::clone(&core.router);
        let transport = Arc::clone(&core);
        let mut stop = core.shutdown.subscribe();
        tokio::spawn(async move {
            let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
            let mut chunk = [0u8; 16 * 1024];
            // 还没收到过完整报文时用较短的握手超时,收到之后放宽成空闲超时。
            let mut seen_message = false;
            loop {
                let budget = if seen_message {
                    IDLE_TIMEOUT
                } else {
                    FIRST_MESSAGE_TIMEOUT
                };
                let read = match tokio::select! {
                    _ = stop.wait() => None,
                    r = tokio::time::timeout(budget, read_half.read(&mut chunk)) => Some(r),
                } {
                    None => {
                        tracing::debug!(%peer, "TCP 传输已关停,连接读循环退出");
                        break;
                    }
                    Some(Ok(result)) => result,
                    Some(Err(_)) => {
                        router.note_connection_timeout();
                        tracing::info!(
                            %peer,
                            secs = budget.as_secs(),
                            seen_message,
                            "SIP over TCP 连接超时未收到数据,断开"
                        );
                        break;
                    }
                };
                let n = match read {
                    Ok(0) => {
                        tracing::debug!(%peer, "SIP over TCP 连接被对端关闭");
                        break;
                    }
                    Ok(n) => n,
                    Err(error) => {
                        tracing::debug!(%peer, %error, "SIP over TCP 读失败,连接结束");
                        break;
                    }
                };
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > MAX_CONNECTION_BUFFER {
                    tracing::warn!(
                        %peer,
                        bytes = buf.len(),
                        "TCP 连接缓冲超限(对端可能在灌垃圾或 Content-Length 不实),断开"
                    );
                    break;
                }

                // 一次可能到达多条报文(粘包),也可能只到半条(分片)。
                let mut fatal = false;
                loop {
                    match parse_incremental(&buf) {
                        Ok(ParseOutcome::Complete { message, consumed }) => {
                            seen_message = true;
                            router.dispatch(Incoming {
                                message,
                                from: peer,
                            });
                            buf.drain(..consumed);
                            if buf.is_empty() {
                                break;
                            }
                        }
                        Ok(ParseOutcome::Incomplete) => {
                            // 头部长度兜底。解析器在"无空行且超 MAX_HEAD_LEN"时已经报 Err,
                            // 所以这里只会命中"带了空行但正文迟迟不来"的情形:
                            // 缓冲上限那一条会先兜住,这里留作显式的第二道闸。
                            if buf.len() > MAX_HEAD_LEN.saturating_add(MAX_CONNECTION_BUFFER / 2) {
                                tracing::warn!(%peer, bytes = buf.len(), "TCP 报文迟迟不完整,断开");
                                fatal = true;
                            }
                            break;
                        }
                        Err(e) => {
                            router.note_parse_error(peer, &e);
                            // 无法同步到下一条报文边界:TCP 上一旦错位,后续字节全部不可信,
                            // 必须**断开连接**而不只是清缓冲继续读。
                            fatal = true;
                            break;
                        }
                    }
                }
                if fatal {
                    break;
                }
            }
            // 无论从哪个分支退出,都要把写半边从表里摘掉,否则死连接会被后续
            // send_to 复用(表现为"发出去了但对端永远收不到")。
            transport.connections.lock().await.remove(&peer);
            if inbound {
                transport
                    .inbound_count
                    .fetch_sub(1, AtomicOrdering::Relaxed);
            }
        });
    }
}

impl TcpCore {
    /// 取得(或建立)到 `dst` 的连接。
    async fn connection(self: &Arc<Self>, dst: SocketAddr) -> Result<Arc<Mutex<OwnedWriteHalf>>> {
        if self.shutdown.is_closed() {
            return Err(Error::Sip("SIP over TCP 传输已关停".into()));
        }
        if let Some(existing) = self.connections.lock().await.get(&dst).cloned() {
            return Ok(existing);
        }
        let stream = TcpStream::connect(dst).await.map_err(|e| {
            tracing::warn!(%dst, error = %e, "SIP over TCP 连接平台失败");
            Error::Io(e)
        })?;
        stream.set_nodelay(true).ok();
        let (read_half, write_half) = stream.into_split();
        let writer = Arc::new(Mutex::new(write_half));
        self.connections
            .lock()
            .await
            .insert(dst, Arc::clone(&writer));
        // 平台的响应会从同一条连接回来,所以出站连接也要挂读循环。
        // 出站连接不占入站名额(是我们主动建的,数量由设备数决定)。
        spawn_read_loop(Arc::clone(self), read_half, dst, false);
        tracing::info!(%dst, "SIP over TCP 已连接平台");
        Ok(writer)
    }
}

impl SipTransport for TcpTransport {
    fn protocol(&self) -> common::Transport {
        common::Transport::Tcp
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.local_addr)
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
                return Err(Error::Sip("SIP over TCP 传输已关停".into()));
            }
            self.core.router.trace(TraceDir::Out, msg, dst);
            let bytes = msg.to_bytes();
            // TCP 无 MTU 限制:大 body 不需要分片,这也是目录树很大时该选 TCP 的理由。
            for attempt in 0..2 {
                let writer = self.core.connection(dst).await?;
                let result = {
                    let mut guard = writer.lock().await;
                    guard.write_all(&bytes).await
                };
                match result {
                    Ok(()) => {
                        self.core.router.note_sent();
                        return Ok(());
                    }
                    Err(e) => {
                        // 连接可能已被对端半关闭:清掉缓存重连一次再报错。
                        self.core.connections.lock().await.remove(&dst);
                        if attempt == 1 {
                            self.core.router.note_send_error();
                            return Err(Error::Io(e));
                        }
                        tracing::debug!(%dst, error = %e, "TCP 写失败,重建连接重试");
                    }
                }
            }
            self.core.router.note_send_error();
            Err(Error::Sip("SIP over TCP 发送重试后仍失败".into()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Headers, Method, Request, Response};

    fn request(call_id: &str, body: &str) -> SipMessage {
        let mut h = Headers::new();
        h.set("Call-ID", call_id);
        h.set("CSeq", "1 MESSAGE");
        h.set("Via", "SIP/2.0/TCP 127.0.0.1:5060;branch=z9hG4bK-tcp");
        SipMessage::Request(Request {
            method: Method::Message,
            uri: "sip:34020000001320000001@127.0.0.1".into(),
            headers: h,
            body: body.as_bytes().to_vec(),
        })
    }

    #[tokio::test]
    async fn 协议标识为_tcp_且监听端口可回填() {
        let t = TcpTransport::bind("127.0.0.1:0").await.unwrap();
        assert_eq!(t.protocol(), common::Transport::Tcp);
        assert_ne!(t.local_addr().unwrap().port(), 0);
    }

    #[tokio::test]
    async fn tcp_双向收发与响应回同一连接() {
        let device = TcpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform = TcpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_addr = platform.local_addr().unwrap();

        let mut platform_inbound = platform.register_inbound("34020000001320000001");
        let mut device_rx = device.register("tcp-call-1");

        // 设备 → 平台(会自动建连)。
        device
            .send_to(&request("tcp-call-1", "hello"), platform_addr)
            .await
            .unwrap();

        let got = tokio::time::timeout(std::time::Duration::from_secs(2), platform_inbound.recv())
            .await
            .expect("平台应收到请求")
            .expect("队列关闭");
        let peer = got.from;
        match &got.message {
            SipMessage::Request(r) => assert_eq!(r.body, b"hello"),
            other => panic!("应为请求,实际 {other:?}"),
        }

        // 平台在同一条连接上回 200 —— 设备侧的事务队列应收到。
        let mut h = Headers::new();
        h.set("Call-ID", "tcp-call-1");
        h.set("CSeq", "1 MESSAGE");
        let resp = SipMessage::Response(Response {
            status: 200,
            reason: "OK".into(),
            headers: h,
            body: Vec::new(),
        });
        platform.send_to(&resp, peer).await.unwrap();

        let back = tokio::time::timeout(std::time::Duration::from_secs(2), device_rx.recv())
            .await
            .expect("设备应收到响应")
            .expect("队列关闭");
        match back.message {
            SipMessage::Response(r) => assert_eq!(r.status, 200),
            other => panic!("应为响应,实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn tcp_粘包与分片都能正确重组() {
        use tokio::io::AsyncWriteExt;

        let device = TcpTransport::bind("127.0.0.1:0").await.unwrap();
        let device_addr = device.local_addr().unwrap();
        let mut inbound = device.register_inbound("34020000001320000001");

        // 手写两条报文的字节流,故意在中间切断发送(先粘包再分片)。
        let first = request("s1", "AAAA").to_bytes();
        let second = request("s2", "BBBBBB").to_bytes();
        let mut stream = TcpStream::connect(device_addr).await.unwrap();

        // 第一段:第一条完整 + 第二条的一半(粘包 + 不完整)。
        let split_at = second.len() / 2;
        let mut part1 = first.clone();
        part1.extend_from_slice(&second[..split_at]);
        stream.write_all(&part1).await.unwrap();

        let a = tokio::time::timeout(std::time::Duration::from_secs(2), inbound.recv())
            .await
            .expect("第一条应立刻可解析")
            .unwrap();
        match a.message {
            SipMessage::Request(r) => assert_eq!(r.body, b"AAAA"),
            other => panic!("{other:?}"),
        }
        // 第二条还差字节,此时不应产出。
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), inbound.recv())
                .await
                .is_err(),
            "不完整报文不应被截断成一条"
        );

        // 补齐剩余字节。
        stream.write_all(&second[split_at..]).await.unwrap();
        let b = tokio::time::timeout(std::time::Duration::from_secs(2), inbound.recv())
            .await
            .expect("补齐后应产出第二条")
            .unwrap();
        match b.message {
            SipMessage::Request(r) => assert_eq!(r.body, b"BBBBBB"),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn tcp_大_body_不受_mtu_限制() {
        let device = TcpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform = TcpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_addr = platform.local_addr().unwrap();
        let mut inbound = platform.register_inbound("34020000001320000001");

        // 64 KB body —— UDP 下必须分片,TCP 下应原样送达且不记 oversized。
        let big = "x".repeat(64 * 1024);
        device
            .send_to(&request("big", &big), platform_addr)
            .await
            .unwrap();
        let got = tokio::time::timeout(std::time::Duration::from_secs(3), inbound.recv())
            .await
            .expect("大 body 应能送达")
            .unwrap();
        match got.message {
            SipMessage::Request(r) => assert_eq!(r.body.len(), 64 * 1024),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            device.stats().snapshot().oversized_sent,
            0,
            "TCP 不应触发 MTU 告警"
        );
    }

    #[tokio::test]
    async fn tcp_拒绝非可信来源的连接() {
        // 白名单随 Router 一起构造，并在 accept loop 启动前已经生效。
        let device = TcpTransport::bind_with_trusted_ips(
            "127.0.0.1:0",
            vec!["203.0.113.7".parse::<IpAddr>().unwrap()],
        )
        .await
        .unwrap();
        let device_addr = device.local_addr().unwrap();
        let mut inbound = device.register_inbound("34020000001320000001");

        let platform = TcpTransport::bind("127.0.0.1:0").await.unwrap();
        // 连接会被 accept 后立即丢弃,报文送不进队列。
        let _ = platform.send_to(&request("evil", "x"), device_addr).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(400), inbound.recv())
                .await
                .is_err(),
            "非可信来源不应进入设备队列"
        );
        assert!(device.stats().snapshot().rejected_source >= 1);
    }

    #[tokio::test]
    async fn tcp_原子可信来源构造器拒绝空集合() {
        assert!(
            TcpTransport::bind_with_trusted_ips("127.0.0.1:0", Vec::new())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn 入站连接数达上限后拒绝新连接() {
        // accept 循环之前是无限接受的:每条连接一个 fd + 一个任务 + 24 KiB 缓冲,
        // 反复建连即可耗尽 fd 让整个进程连不上平台。
        let device = TcpTransport::bind("127.0.0.1:0").await.unwrap();
        let addr = device.local_addr().unwrap();

        // 占满名额。保留 stream 句柄,否则连接立刻断开、名额马上被归还。
        let mut held = Vec::new();
        for _ in 0..MAX_INBOUND_CONNECTIONS {
            held.push(TcpStream::connect(addr).await.expect("应能连上"));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while device.inbound_connections() < MAX_INBOUND_CONNECTIONS {
            assert!(std::time::Instant::now() < deadline, "连接未被全部 adopt");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        // 第 65 条:TCP 握手仍会成功(内核 backlog),但应用层应立刻拒绝并计数。
        let _extra = TcpStream::connect(addr).await.expect("握手由内核完成");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if device.stats().snapshot().rejected_connections >= 1 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "超限连接未被拒绝");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            device.inbound_connections(),
            MAX_INBOUND_CONNECTIONS,
            "被拒的连接不该占用名额"
        );

        // 释放一条后名额应归还,新连接可以进来。
        held.pop();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while device.inbound_connections() >= MAX_INBOUND_CONNECTIONS {
            assert!(std::time::Instant::now() < deadline, "连接关闭后名额未归还");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn 关停后监听端口被释放且发送快速失败() {
        // 与 UDP 同一个不变量:accept / 读循环不能把传输对象钉死在内存里。
        let device = TcpTransport::bind("127.0.0.1:0").await.unwrap();
        let addr = device.local_addr().unwrap();
        assert!(!device.is_closed());

        device.shutdown();
        assert!(device.is_closed());
        assert!(
            device.send_to(&request("x", ""), addr).await.is_err(),
            "关停后不应还能发送"
        );

        drop(device);
        let mut rebound = false;
        for _ in 0..150 {
            if TcpListener::bind(addr).await.is_ok() {
                rebound = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(rebound, "关停后监听端口应可重新绑定(listener 已真的释放)");
    }

    #[tokio::test]
    async fn 解析错位后断开连接而不是继续读() {
        // TCP 上一旦错位,后续字节全部不可信 —— 之前只清缓冲继续读,
        // 于是攻击者可以用一条连接持续灌垃圾,每条都记一次解析失败。
        use tokio::io::AsyncWriteExt;
        let device = TcpTransport::bind("127.0.0.1:0").await.unwrap();
        let addr = device.local_addr().unwrap();
        let mut attacker = TcpStream::connect(addr).await.unwrap();
        // 一条结构上不可能是 SIP 的报文(起始行没有空格分隔)。
        attacker.write_all(b"@@@@@@@@\r\n\r\n").await.unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if device.stats().snapshot().parse_errors >= 1 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "未记录解析失败");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // 连接应被服务端关闭:读到 EOF。
        let mut buf = [0u8; 1];
        let closed = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::io::AsyncReadExt::read(&mut attacker, &mut buf),
        )
        .await
        .expect("应在 5s 内被关闭");
        assert!(
            matches!(closed, Ok(0)),
            "解析错位后服务端应关闭连接,实际 {closed:?}"
        );
        // 名额也要归还。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while device.inbound_connections() > 0 {
            assert!(std::time::Instant::now() < deadline, "断开后名额未归还");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    #[test]
    fn 超时常量的量级符合意图() {
        // slowloris 防护必须比空闲回收严格得多:前者针对"建连后不说话",
        // 后者只是回收 FIN 丢失的死连接,不能干扰正常长连接。
        assert!(
            FIRST_MESSAGE_TIMEOUT < IDLE_TIMEOUT,
            "握手超时必须短于空闲超时"
        );
        assert!(
            FIRST_MESSAGE_TIMEOUT >= Duration::from_secs(10),
            "太短会误杀网络慢的真实平台"
        );
        assert!(
            IDLE_TIMEOUT >= Duration::from_secs(300),
            "SIP over TCP 在两次心跳之间静默是正常的,空闲超时不能比心跳周期还短"
        );
    }
}
