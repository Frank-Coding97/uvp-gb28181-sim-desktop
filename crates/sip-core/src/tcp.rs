//! SIP over TCP 传输(RFC 3261 §18)。
//!
//! # 为什么仍是共享连接
//!
//! UDP 传输靠"一个 socket 承载全部虚拟设备 + 按 Call-ID 路由"支撑万级压测。TCP 沿用
//! 同一模型:**所有虚拟设备复用少量 TCP 连接**,收到的报文同样按 Call-ID / AOR 分发。
//! 于是 FD 占用等于连接池大小(默认 [`DEFAULT_CONNECTIONS`] 条),与设备数无关 ——
//! 若改成"每设备一条连接",万级设备会直接撞上 FD 上限与平台侧连接数限制。
//!
//! 连接数 >1 时按设备标识哈希固定映射,避免单连接成为吞吐瓶颈与单点;
//! 同一设备始终落在同一条连接上,平台的"注册连接 → 后续下发连接"绑定关系不会被打破。
//!
//! # 与 UDP 的三点差异
//!
//! 1. **流式分帧**:TCP 没有报文边界,必须按 `Content-Length` 切分,见 [`FrameDecoder`]。
//! 2. **不做应用层重传**:可靠传输下重传由 TCP 负责,事务层只保留超时预算
//!    (见 [`crate::transaction::client_transact`] 的 `is_reliable` 分支)。
//! 3. **响应沿用入站连接**:RFC 3261 §18.2.2 要求响应沿收到请求的连接回发。
//!    因此入站请求会登记 `Call-ID → 连接`,回响应时优先命中该登记,而不是按哈希重选。

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use bytes::BytesMut;
use dashmap::DashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};

use common::{Error, Result};

use crate::message::{Headers, SipMessage};
use crate::transport::{dispatch, Incoming, SipTrace, TraceDir, TraceObserver};

/// 默认连接池大小。取 4 以在"单连接队头阻塞"和"平台连接数浪费"之间折中。
pub const DEFAULT_CONNECTIONS: usize = 4;
/// 连接池上限,防止误配置把平台打满。
pub const MAX_CONNECTIONS: usize = 64;
/// 单条连接的待发队列容量。满时发送方阻塞(背压),不丢报文。
const OUTBOUND_CAPACITY: usize = 4096;
/// 断线期间允许积压的报文条数。超过则丢最旧并告警,避免内存无界增长。
const MAX_BACKLOG: usize = 8192;
/// 单帧头部上限。SIP 头正常在数 KB 内,超出视为流错乱。
const MAX_HEAD_BYTES: usize = 64 * 1024;
/// 单帧消息体上限。目录/录像列表应答可达数十 KB,留足余量。
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// 单次写超时。防止对端不读导致写阻塞拖死同连接的读方向。
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// 按 `Content-Length` 从字节流中切出完整 SIP 报文。
///
/// 用法:`extend` 追加任意长度片段,循环 `next_frame` 取报文,返回 `None` 表示
/// 当前片段不完整、需继续读。内部用 `BytesMut` 做零拷贝切片,避免大缓冲反复搬移。
#[derive(Debug)]
pub struct FrameDecoder {
    buf: BytesMut,
    max_head: usize,
    max_body: usize,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self {
            buf: BytesMut::new(),
            max_head: MAX_HEAD_BYTES,
            max_body: MAX_BODY_BYTES,
        }
    }
}

impl FrameDecoder {
    /// 新建分帧器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加从 socket 读到的片段。
    pub fn extend(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// 当前缓存的未消费字节数(诊断用)。
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// 取下一个完整报文。`Ok(None)` 表示数据不足。
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>> {
        let Some(head_end) = self.find_head_end() else {
            if self.buf.len() > self.max_head {
                return Err(Error::Sip(format!(
                    "TCP 分帧失败:{} 字节内未出现报文头结束符",
                    self.max_head
                )));
            }
            return Ok(None);
        };
        let body_start = head_end + 4;
        let body_len = parse_content_length(&self.buf[..head_end])?;
        if body_len > self.max_body {
            return Err(Error::Sip(format!(
                "TCP 分帧失败:Content-Length {body_len} 超过上限 {}",
                self.max_body
            )));
        }
        let total = body_start + body_len;
        if self.buf.len() < total {
            return Ok(None);
        }
        Ok(Some(self.buf.split_to(total).to_vec()))
    }

    /// 定位报文头结束位置(CRLFCRLF 的起始下标)。
    fn find_head_end(&self) -> Option<usize> {
        self.buf.windows(4).position(|w| w == b"\r\n\r\n")
    }
}

/// 从报文中取出 `Content-Length`。缺省按 0 处理(RFC 3261 要求 TCP 必须带,
/// 但部分平台在无体请求上省略;按 0 处理不会误切后续报文)。
fn parse_content_length(head: &[u8]) -> Result<usize> {
    let text = String::from_utf8_lossy(head);
    let mut len = None;
    for line in text.split("\r\n").skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        // `l` 是 Content-Length 的紧凑形式(RFC 3261 §7.3.3)。
        if name.eq_ignore_ascii_case("Content-Length") || name.eq_ignore_ascii_case("l") {
            let raw = value.trim();
            len = Some(raw.parse::<usize>().map_err(|_| {
                Error::Sip(format!("TCP 分帧失败:Content-Length 非法({raw})"))
            })?);
        }
    }
    Ok(len.unwrap_or(0))
}

/// 共享连接式 SIP over TCP 传输。
///
/// 对外接口与 [`crate::transport::UdpTransport`] 保持一致(见
/// [`crate::transport::SipTransport`]),上层设备逻辑无需感知传输差异。
pub struct TcpTransport {
    /// 平台信令地址。TCP 是面向连接的,所有发送都复用既有连接,不再逐包寻址。
    peer: SocketAddr,
    /// 连接池的发送端,下标即连接编号。
    conns: Vec<mpsc::Sender<Vec<u8>>>,
    /// Call-ID → 事务接收队列。
    routes: Arc<DashMap<String, mpsc::UnboundedSender<Incoming>>>,
    /// 设备 AOR → 入站请求队列。
    aor_routes: Arc<DashMap<String, mpsc::UnboundedSender<Incoming>>>,
    /// 入站请求的 Call-ID → 连接编号。用于响应沿原连接回发。
    call_conn: Arc<DashMap<String, usize>>,
    /// 可选 SIP 追踪观察者;None 时零开销。
    tracer: Arc<RwLock<Option<Arc<dyn TraceObserver>>>>,
    /// 首个建立成功的本端地址,用于回填 Via/Contact。
    local: Arc<RwLock<Option<SocketAddr>>>,
    /// 后台连接任务。传输释放时中止,避免生命周期泄漏。
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// 是否已有至少一条连接就绪(仅诊断/测试用)。
    ready: Arc<AtomicBool>,
}

impl TcpTransport {
    /// 连接平台并启动连接池。`peer` 为 `host:port`。
    ///
    /// 返回前会等待**首条连接就绪**(最多 3 秒):TCP 配置错误或平台不可达时立即报错,
    /// 而不是让上层在事务超时里慢慢失败。其余连接由后台任务建立并自动重连。
    pub async fn connect(peer: &str, connections: usize) -> Result<Arc<Self>> {
        let peer_addr = resolve_peer(peer).await?;
        let n = connections.clamp(1, MAX_CONNECTIONS);
        let routes: Arc<DashMap<String, mpsc::UnboundedSender<Incoming>>> = Arc::new(DashMap::new());
        let aor_routes: Arc<DashMap<String, mpsc::UnboundedSender<Incoming>>> =
            Arc::new(DashMap::new());
        let call_conn = Arc::new(DashMap::new());
        let tracer: Arc<RwLock<Option<Arc<dyn TraceObserver>>>> = Arc::new(RwLock::new(None));
        let local: Arc<RwLock<Option<SocketAddr>>> = Arc::new(RwLock::new(None));
        let ready = Arc::new(AtomicBool::new(false));
        let (ready_tx, mut ready_rx) = watch::channel(false);

        let mut conns = Vec::with_capacity(n);
        let mut tasks = Vec::with_capacity(n);
        for idx in 0..n {
            let (tx, rx) = mpsc::channel(OUTBOUND_CAPACITY);
            conns.push(tx);
            let ctx = ConnCtx {
                idx,
                peer: peer_addr,
                routes: Arc::clone(&routes),
                aor_routes: Arc::clone(&aor_routes),
                call_conn: Arc::clone(&call_conn),
                tracer: Arc::clone(&tracer),
                local: Arc::clone(&local),
                ready: Arc::clone(&ready),
                ready_tx: ready_tx.clone(),
            };
            tasks.push(tokio::spawn(supervise(ctx, rx)));
        }
        drop(ready_tx);

        // 等首条连接就绪。用 watch 而非 Notify:后者在等待方注册前发出的通知会丢失。
        let first = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if *ready_rx.borrow() {
                    return;
                }
                if ready_rx.changed().await.is_err() {
                    return;
                }
            }
        })
        .await;
        if first.is_err() {
            for task in &tasks {
                task.abort();
            }
            return Err(Error::Sip(format!(
                "TCP 信令连接失败:{peer_addr} 3 秒内未建立连接"
            )));
        }

        Ok(Arc::new(TcpTransport {
            peer: peer_addr,
            conns,
            routes,
            aor_routes,
            call_conn,
            tracer,
            local,
            tasks: Mutex::new(tasks),
            ready,
        }))
    }

    /// 挂载 SIP 追踪观察者。传 None 关闭。
    pub fn set_tracer(&self, tracer: Option<Arc<dyn TraceObserver>>) {
        if let Ok(mut g) = self.tracer.write() {
            *g = tracer;
        }
    }

    /// 平台信令地址。
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer
    }

    /// 连接池大小。
    pub fn connections(&self) -> usize {
        self.conns.len()
    }

    /// 是否已有连接就绪。
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    /// 本端地址(取首条连接建立后的实际地址,用于回填 Via/Contact)。
    pub fn local_addr(&self) -> Result<SocketAddr> {
        if let Ok(g) = self.local.read() {
            if let Some(addr) = *g {
                return Ok(addr);
            }
        }
        // 尚未建立:返回平台地址族下的占位,端口为 0。
        Ok(if self.peer.is_ipv4() {
            SocketAddr::from(([0, 0, 0, 0], 0))
        } else {
            SocketAddr::from(([0u16; 8], 0))
        })
    }

    /// 为某 Call-ID 注册响应接收队列。
    pub fn register(&self, call_id: impl Into<String>) -> mpsc::UnboundedReceiver<Incoming> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.routes.insert(call_id.into(), tx);
        rx
    }

    /// 为某设备 AOR 注册入站请求队列。
    pub fn register_inbound(&self, aor: impl Into<String>) -> mpsc::UnboundedReceiver<Incoming> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.aor_routes.insert(aor.into(), tx);
        rx
    }

    /// 为多个 AOR(设备 ID + 各通道 ID)注册到同一入站队列。
    pub fn register_inbound_many(
        &self,
        aors: impl IntoIterator<Item = String>,
    ) -> mpsc::UnboundedReceiver<Incoming> {
        let (tx, rx) = mpsc::unbounded_channel();
        for aor in aors {
            self.aor_routes.insert(aor, tx.clone());
        }
        rx
    }

    /// 注销某 Call-ID 的响应路由。
    pub fn unregister(&self, call_id: &str) {
        self.routes.remove(call_id);
    }

    /// 注销某设备 AOR 的入站路由。
    pub fn unregister_inbound(&self, aor: &str) {
        self.aor_routes.remove(aor);
    }

    /// 发送一条 SIP 消息。
    ///
    /// `dst` 被忽略 —— 连接已建立,报文沿池内连接发出。保留该参数是为了与 UDP
    /// 传输共用上层签名。
    pub async fn send_to(&self, msg: &SipMessage, dst: SocketAddr) -> Result<()> {
        let _ = dst;
        self.trace(TraceDir::Out, msg, self.peer);
        let idx = self.pick_conn(msg);
        let bytes = msg.to_bytes();
        self.conns[idx]
            .send(bytes)
            .await
            .map_err(|_| Error::Sip("TCP 信令连接已关闭".into()))
    }

    /// 选择出向连接。
    ///
    /// - 响应:优先沿用收到对应请求的连接(RFC 3261 §18.2.2)。
    /// - 请求:按发起设备(From 的 URI user)哈希,保证同一设备恒走同一连接。
    fn pick_conn(&self, msg: &SipMessage) -> usize {
        let n = self.conns.len();
        if n <= 1 {
            return 0;
        }
        let call_id = message_call_id(msg);
        if matches!(msg, SipMessage::Response(_)) {
            if let Some(id) = call_id {
                if let Some(hit) = self.call_conn.get(id) {
                    return *hit % n;
                }
            }
            return call_id.map_or(0, |id| hash_index(id, n));
        }
        let requester = match msg {
            SipMessage::Request(r) => from_uri_user(&r.headers),
            SipMessage::Response(_) => None,
        };
        match requester.or(call_id) {
            Some(key) => hash_index(key, n),
            None => 0,
        }
    }

    fn trace(&self, dir: TraceDir, message: &SipMessage, peer: SocketAddr) {
        if let Ok(g) = self.tracer.read() {
            if let Some(t) = g.as_ref() {
                t.on_trace(SipTrace { dir, message, peer });
            }
        }
    }
}

impl Drop for TcpTransport {
    fn drop(&mut self) {
        if let Ok(mut tasks) = self.tasks.lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }
}

/// 单条连接监督所需上下文。
struct ConnCtx {
    idx: usize,
    peer: SocketAddr,
    routes: Arc<DashMap<String, mpsc::UnboundedSender<Incoming>>>,
    aor_routes: Arc<DashMap<String, mpsc::UnboundedSender<Incoming>>>,
    call_conn: Arc<DashMap<String, usize>>,
    tracer: Arc<RwLock<Option<Arc<dyn TraceObserver>>>>,
    local: Arc<RwLock<Option<SocketAddr>>>,
    ready: Arc<AtomicBool>,
    ready_tx: watch::Sender<bool>,
}

/// 连接监督循环:建连 → 收发 → 断开 → 退避重连,直到传输被释放。
async fn supervise(ctx: ConnCtx, mut rx: mpsc::Receiver<Vec<u8>>) {
    let mut backlog: VecDeque<Vec<u8>> = VecDeque::new();
    let mut backoff = Duration::from_millis(200);
    loop {
        match TcpStream::connect(ctx.peer).await {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                if let Ok(addr) = stream.local_addr() {
                    if let Ok(mut g) = ctx.local.write() {
                        if g.is_none() {
                            *g = Some(addr);
                        }
                    }
                }
                if !ctx.ready.swap(true, Ordering::Relaxed) {
                    let _ = ctx.ready_tx.send(true);
                }
                backoff = Duration::from_millis(200);
                tracing::debug!(conn = ctx.idx, peer = %ctx.peer, "TCP 信令连接已建立");
                if !run_io(&ctx, stream, &mut rx, &mut backlog).await {
                    return;
                }
                tracing::warn!(conn = ctx.idx, peer = %ctx.peer, "TCP 信令连接断开,准备重连");
            }
            Err(error) => {
                tracing::warn!(conn = ctx.idx, peer = %ctx.peer, error = %error, "TCP 信令连接失败,退避重试");
            }
        }
        // 退避期间仍要排空待发队列,否则发送方会在 channel 上阻塞到超时。
        if !drain_backoff(&mut rx, &mut backlog, backoff).await {
            return;
        }
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

/// 已连接状态下的收发循环。返回 `false` 表示传输已释放,任务应退出。
async fn run_io(
    ctx: &ConnCtx,
    stream: TcpStream,
    rx: &mut mpsc::Receiver<Vec<u8>>,
    backlog: &mut VecDeque<Vec<u8>>,
) -> bool {
    let (mut rd, mut wr) = stream.into_split();
    let mut decoder = FrameDecoder::new();
    let mut chunk = vec![0u8; 16 * 1024];

    // 重连后先补发断线期间积压的报文。
    while let Some(bytes) = backlog.pop_front() {
        if write_frame(&mut wr, &bytes).await.is_err() {
            backlog.push_front(bytes);
            return true;
        }
    }

    loop {
        tokio::select! {
            read = rd.read(&mut chunk) => {
                match read {
                    Ok(0) => return true, // 对端关闭
                    Ok(n) => {
                        decoder.extend(&chunk[..n]);
                        loop {
                            match decoder.next_frame() {
                                Ok(Some(frame)) => match SipMessage::parse(&frame) {
                                    Ok(message) => dispatch(
                                        &ctx.routes,
                                        &ctx.aor_routes,
                                        &ctx.tracer,
                                        Incoming { message, from: ctx.peer },
                                        Some(ctx.idx),
                                        Some(&ctx.call_conn),
                                    ),
                                    Err(error) => {
                                        tracing::warn!(error = %error, "TCP SIP 报文解析失败,丢弃");
                                    }
                                },
                                Ok(None) => break,
                                Err(error) => {
                                    // 流已错乱,无法再定位边界,只能重建连接。
                                    tracing::warn!(error = %error, "TCP 分帧异常,重建连接");
                                    return true;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "TCP 读错误");
                        return true;
                    }
                }
            }
            out = rx.recv() => {
                match out {
                    Some(bytes) => {
                        if write_frame(&mut wr, &bytes).await.is_err() {
                            backlog.push_back(bytes);
                            return true;
                        }
                    }
                    None => return false, // 传输已释放
                }
            }
        }
    }
}

/// 写一帧,带超时防止对端不读时拖死同任务的读方向。
async fn write_frame<W>(wr: &mut W, bytes: &[u8]) -> std::io::Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    match tokio::time::timeout(WRITE_TIMEOUT, wr.write_all(bytes)).await {
        Ok(result) => result,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "TCP 写超时",
        )),
    }
}

/// 退避等待期间继续收集待发报文。返回 `false` 表示传输已释放。
async fn drain_backoff(
    rx: &mut mpsc::Receiver<Vec<u8>>,
    backlog: &mut VecDeque<Vec<u8>>,
    wait: Duration,
) -> bool {
    let timer = tokio::time::sleep(wait);
    tokio::pin!(timer);
    loop {
        tokio::select! {
            _ = &mut timer => return true,
            out = rx.recv() => match out {
                Some(bytes) => {
                    if backlog.len() >= MAX_BACKLOG {
                        backlog.pop_front();
                        tracing::warn!("TCP 待发队列积压超过 {MAX_BACKLOG} 条,丢弃最旧报文");
                    }
                    backlog.push_back(bytes);
                }
                None => return false,
            }
        }
    }
}

/// 解析 `host:port`,支持域名。
async fn resolve_peer(peer: &str) -> Result<SocketAddr> {
    if let Ok(addr) = peer.parse::<SocketAddr>() {
        return Ok(addr);
    }
    tokio::net::lookup_host(peer)
        .await
        .map_err(Error::Io)?
        .next()
        .ok_or_else(|| Error::Sip(format!("无法解析 TCP 信令目标地址:{peer}")))
}

/// 取消息的 Call-ID。
fn message_call_id(msg: &SipMessage) -> Option<&str> {
    match msg {
        SipMessage::Request(r) => r.headers.call_id(),
        SipMessage::Response(r) => r.headers.call_id(),
    }
}

/// 从 From 头提取发起方 URI 的 user 部分(设备 ID)。
///
/// 响应不能用它 —— 响应的 From 是请求发起者(平台),不是设备。调用方已按消息类型分流。
fn from_uri_user(headers: &Headers) -> Option<&str> {
    let raw = headers.get("From").or_else(|| headers.get("f"))?;
    let start = raw.find("sip:").map_or(0, |i| i + 4);
    let rest = &raw[start..];
    let end = rest.find(['@', '>', ';']).unwrap_or(rest.len());
    let user = &rest[..end];
    (!user.is_empty()).then_some(user)
}

/// 稳定哈希到 `[0, n)`。只要求在单次运行内稳定、分布均匀。
fn hash_index(key: &str, n: usize) -> usize {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    (hasher.finish() as usize) % n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Method, Request, Response};

    fn register_bytes(call_id: &str, cseq: u32) -> Vec<u8> {
        let mut headers = Headers::new();
        headers.set("Call-ID", call_id);
        headers.set("CSeq", format!("{cseq} REGISTER"));
        headers.set(
            "From",
            "<sip:34020000001320000001@3402000000>;tag=abc",
        );
        headers.set("Content-Length", "0");
        Request {
            method: Method::Register,
            uri: "sip:34020000002000000001@127.0.0.1:5060".into(),
            headers,
            body: Vec::new(),
        }
        .to_bytes()
    }

    #[test]
    fn 分帧_单帧完整报文() {
        let mut d = FrameDecoder::new();
        d.extend(&register_bytes("call-1", 1));
        let frame = d.next_frame().unwrap().expect("应切出一帧");
        assert!(SipMessage::parse(&frame).is_ok());
        assert!(d.next_frame().unwrap().is_none());
        assert_eq!(d.buffered(), 0);
    }

    #[test]
    fn 分帧_粘包两帧() {
        let mut d = FrameDecoder::new();
        let mut bytes = register_bytes("call-1", 1);
        bytes.extend_from_slice(&register_bytes("call-2", 2));
        d.extend(&bytes);

        let first = d.next_frame().unwrap().unwrap();
        let second = d.next_frame().unwrap().unwrap();
        assert!(d.next_frame().unwrap().is_none());

        match SipMessage::parse(&first).unwrap() {
            SipMessage::Request(r) => assert_eq!(r.headers.call_id(), Some("call-1")),
            _ => panic!("应为请求"),
        }
        match SipMessage::parse(&second).unwrap() {
            SipMessage::Request(r) => assert_eq!(r.headers.call_id(), Some("call-2")),
            _ => panic!("应为请求"),
        }
    }

    #[test]
    fn 分帧_半包按body长度等待() {
        let mut headers = Headers::new();
        headers.set("Call-ID", "call-body");
        headers.set("CSeq", "1 MESSAGE");
        let body = b"<Notify><CmdType>Keepalive</CmdType></Notify>".to_vec();
        let full = Request {
            method: Method::Message,
            uri: "sip:p@h".into(),
            headers,
            body: body.clone(),
        }
        .to_bytes();

        // 头部已完整、body 只到一半:必须返回 None,不能切出不完整报文。
        let cut = full.len() - body.len() / 2;
        let mut d = FrameDecoder::new();
        d.extend(&full[..cut]);
        assert!(d.next_frame().unwrap().is_none());

        d.extend(&full[cut..]);
        let frame = d.next_frame().unwrap().expect("补齐后应可切帧");
        assert_eq!(&frame[frame.len() - body.len()..], body.as_slice());
    }

    #[test]
    fn 分帧_逐字节喂入仍可切帧() {
        let bytes = register_bytes("call-drip", 3);
        let mut d = FrameDecoder::new();
        let mut frames = 0;
        for b in &bytes {
            d.extend(&[*b]);
            while let Some(frame) = d.next_frame().unwrap() {
                assert!(SipMessage::parse(&frame).is_ok());
                frames += 1;
            }
        }
        assert_eq!(frames, 1);
    }

    #[test]
    fn 分帧_缺失content_length按零处理() {
        let raw = b"OPTIONS sip:dev@h SIP/2.0\r\nCall-ID: c1\r\nCSeq: 1 OPTIONS\r\n\r\n";
        let mut d = FrameDecoder::new();
        d.extend(raw);
        let frame = d.next_frame().unwrap().expect("应切出无体请求");
        assert_eq!(frame.len(), raw.len());
    }

    #[test]
    fn 分帧_紧凑形式content_length() {
        let raw = b"SIP/2.0 200 OK\r\nCall-ID: c1\r\nl: 4\r\n\r\nbody";
        let mut d = FrameDecoder::new();
        d.extend(raw);
        let frame = d.next_frame().unwrap().expect("应切出带体响应");
        assert_eq!(frame.len(), raw.len());
    }

    #[test]
    fn 分帧_头部无结束符且超限报错() {
        let mut d = FrameDecoder {
            buf: BytesMut::new(),
            max_head: 32,
            max_body: MAX_BODY_BYTES,
        };
        d.extend(&[b'x'; 64]);
        assert!(d.next_frame().is_err());
    }

    #[test]
    fn 连接选择_响应沿用入站连接() {
        // 构造一个 4 连接的传输骨架,只验证选连接策略,不建真连接。
        let transport = TcpTransport {
            peer: "127.0.0.1:5060".parse().unwrap(),
            conns: (0..4)
                .map(|_| {
                    let (tx, _rx) = mpsc::channel(1);
                    tx
                })
                .collect(),
            routes: Arc::new(DashMap::new()),
            aor_routes: Arc::new(DashMap::new()),
            call_conn: Arc::new(DashMap::new()),
            tracer: Arc::new(RwLock::new(None)),
            local: Arc::new(RwLock::new(None)),
            tasks: Mutex::new(Vec::new()),
            ready: Arc::new(AtomicBool::new(false)),
        };

        let mut headers = Headers::new();
        headers.set("Call-ID", "shared-call");
        headers.set("CSeq", "1 INVITE");
        let response = SipMessage::Response(Response {
            status: 200,
            reason: "OK".into(),
            headers,
            body: Vec::new(),
        });

        // 登记"该 Call-ID 来自 2 号连接",响应必须回到 2 号。
        transport.call_conn.insert("shared-call".into(), 2);
        assert_eq!(transport.pick_conn(&response), 2);
    }

    #[test]
    fn 连接选择_同一设备恒定同一连接() {
        let transport = TcpTransport {
            peer: "127.0.0.1:5060".parse().unwrap(),
            conns: (0..4)
                .map(|_| {
                    let (tx, _rx) = mpsc::channel(1);
                    tx
                })
                .collect(),
            routes: Arc::new(DashMap::new()),
            aor_routes: Arc::new(DashMap::new()),
            call_conn: Arc::new(DashMap::new()),
            tracer: Arc::new(RwLock::new(None)),
            local: Arc::new(RwLock::new(None)),
            tasks: Mutex::new(Vec::new()),
            ready: Arc::new(AtomicBool::new(false)),
        };

        let make = |device: &str, call_id: &str| {
            let mut headers = Headers::new();
            headers.set("Call-ID", call_id);
            headers.set("CSeq", "1 MESSAGE");
            headers.set("From", format!("<sip:{device}@3402000000>;tag=t"));
            SipMessage::Request(Request {
                method: Method::Message,
                uri: "sip:p@h".into(),
                headers,
                body: Vec::new(),
            })
        };

        // 同一设备不同 Call-ID 仍应落在同一连接(按 From 哈希,不按 Call-ID)。
        let a = transport.pick_conn(&make("34020000001320000001", "call-a"));
        let b = transport.pick_conn(&make("34020000001320000001", "call-b"));
        assert_eq!(a, b);
        assert!(a < 4);
    }

    #[test]
    fn 从from头提取设备标识() {
        let mut headers = Headers::new();
        headers.set("From", "<sip:34020000001320000009@3402000000>;tag=x");
        assert_eq!(from_uri_user(&headers), Some("34020000001320000009"));

        let mut compact = Headers::new();
        compact.set("f", "sip:abc@h;tag=y");
        assert_eq!(from_uri_user(&compact), Some("abc"));

        assert_eq!(from_uri_user(&Headers::new()), None);
    }
}
