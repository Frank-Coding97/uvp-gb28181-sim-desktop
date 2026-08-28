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

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};

use common::{Error, Result};

use crate::message::{parse_incremental, ParseOutcome, SipMessage, MAX_HEAD_LEN};

use super::{Incoming, Router, SendFuture, SipTransport, TraceDir, TraceObserver, TransportStats};

/// 单条连接的读缓冲上限。超过说明对端在灌垃圾或 Content-Length 撒谎,直接断开。
const MAX_CONNECTION_BUFFER: usize = 4 * 1024 * 1024;

/// 共享 TCP 传输:一个监听端口 + 一组到平台的出站连接,多设备复用。
pub struct TcpTransport {
    router: Arc<Router>,
    /// 本地监听地址(写进 Via/Contact)。
    local_addr: SocketAddr,
    /// 对端地址 → 该连接的写半边。出站请求与后续响应共用同一条连接。
    connections: Mutex<HashMap<SocketAddr, Arc<Mutex<OwnedWriteHalf>>>>,
}

impl TcpTransport {
    /// 绑定本地监听地址(如 "0.0.0.0:0" 由系统分配端口),启动 accept 循环。
    pub async fn bind(local: &str) -> Result<Arc<Self>> {
        let listener = TcpListener::bind(local).await.map_err(Error::Io)?;
        let local_addr = listener.local_addr().map_err(Error::Io)?;
        let transport = Arc::new(TcpTransport {
            router: Arc::new(Router::new()),
            local_addr,
            connections: Mutex::new(HashMap::new()),
        });
        transport.clone().spawn_accept_loop(listener);
        Ok(transport)
    }

    /// 接受平台主动建立的连接(平台侧下发 INVITE / 查询时会这样)。
    fn spawn_accept_loop(self: Arc<Self>, listener: TcpListener) {
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer)) => {
                        if !self.router.source_allowed(peer) {
                            self.router.note_rejected_source(peer);
                            continue;
                        }
                        tracing::info!(%peer, "接受平台 SIP over TCP 连接");
                        self.clone().adopt(stream, peer).await;
                    }
                    Err(error) => {
                        tracing::error!(%error, "TCP accept 失败,监听循环退出");
                        break;
                    }
                }
            }
        });
    }

    /// 把一条已建立的连接拆成读写两半:写半边入表复用,读半边起流式重组循环。
    async fn adopt(self: Arc<Self>, stream: TcpStream, peer: SocketAddr) {
        stream.set_nodelay(true).ok();
        let (read_half, write_half) = stream.into_split();
        self.connections
            .lock()
            .await
            .insert(peer, Arc::new(Mutex::new(write_half)));
        self.spawn_read_loop(read_half, peer);
    }

    /// 单连接读循环:累积字节 → 增量解析 → 分发 → 推进游标。
    fn spawn_read_loop(self: Arc<Self>, mut read_half: OwnedReadHalf, peer: SocketAddr) {
        let router = Arc::clone(&self.router);
        let transport = Arc::clone(&self);
        tokio::spawn(async move {
            let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
            let mut chunk = [0u8; 16 * 1024];
            loop {
                let n = match read_half.read(&mut chunk).await {
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
                loop {
                    match parse_incremental(&buf) {
                        Ok(ParseOutcome::Complete { message, consumed }) => {
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
                            // 头部一直等不到空行也要有上界(解析器内部已有 MAX_HEAD_LEN)。
                            if buf.len() > MAX_HEAD_LEN && !buf.windows(2).any(|w| w == b"\n\n") {
                                tracing::warn!(%peer, "TCP 报文头部超限,断开");
                                buf.clear();
                            }
                            break;
                        }
                        Err(e) => {
                            router.note_parse_error(peer, &e);
                            // 无法同步到下一条报文边界,只能丢弃整个缓冲并断开:
                            // TCP 上一旦错位,后续字节全部不可信。
                            buf.clear();
                            break;
                        }
                    }
                }
            }
            transport.connections.lock().await.remove(&peer);
        });
    }

    /// 取得(或建立)到 `dst` 的连接。
    async fn connection(&self, dst: SocketAddr) -> Result<Arc<Mutex<OwnedWriteHalf>>> {
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
        self.spawn_read_loop_for(read_half, dst);
        tracing::info!(%dst, "SIP over TCP 已连接平台");
        Ok(writer)
    }

    /// 出站连接的读循环。与被动连接共用同一份实现,只是无法用 `Arc<Self>`
    /// (`&self` 上下文),因此单独走一层路由句柄。
    fn spawn_read_loop_for(&self, mut read_half: OwnedReadHalf, peer: SocketAddr) {
        let router = Arc::clone(&self.router);
        tokio::spawn(async move {
            let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
            let mut chunk = [0u8; 16 * 1024];
            loop {
                let n = match read_half.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(error) => {
                        tracing::debug!(%peer, %error, "SIP over TCP 出站连接读失败");
                        break;
                    }
                };
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > MAX_CONNECTION_BUFFER {
                    tracing::warn!(%peer, bytes = buf.len(), "TCP 连接缓冲超限,断开");
                    break;
                }
                loop {
                    match parse_incremental(&buf) {
                        Ok(ParseOutcome::Complete { message, consumed }) => {
                            router.dispatch(Incoming {
                                message,
                                from: peer,
                            });
                            buf.drain(..consumed);
                            if buf.is_empty() {
                                break;
                            }
                        }
                        Ok(ParseOutcome::Incomplete) => break,
                        Err(e) => {
                            router.note_parse_error(peer, &e);
                            buf.clear();
                            break;
                        }
                    }
                }
            }
        });
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
            // TCP 无 MTU 限制:大 body 不需要分片,这也是目录树很大时该选 TCP 的理由。
            for attempt in 0..2 {
                let writer = self.connection(dst).await?;
                let result = {
                    let mut guard = writer.lock().await;
                    guard.write_all(&bytes).await
                };
                match result {
                    Ok(()) => {
                        self.router.note_sent();
                        return Ok(());
                    }
                    Err(e) => {
                        // 连接可能已被对端半关闭:清掉缓存重连一次再报错。
                        self.connections.lock().await.remove(&dst);
                        if attempt == 1 {
                            self.router.note_send_error();
                            return Err(Error::Io(e));
                        }
                        tracing::debug!(%dst, error = %e, "TCP 写失败,重建连接重试");
                    }
                }
            }
            self.router.note_send_error();
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
        let device = TcpTransport::bind("127.0.0.1:0").await.unwrap();
        let device_addr = device.local_addr().unwrap();
        let mut inbound = device.register_inbound("34020000001320000001");
        device.set_trusted_ips(vec!["203.0.113.7".parse::<IpAddr>().unwrap()]);

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
}
