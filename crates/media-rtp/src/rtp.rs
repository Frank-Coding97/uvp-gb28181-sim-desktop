//! RTP 打包与发送。
//!
//! 实现 docs/40-protocol/media-ps-rtp.md 的 RTP 层:RFC 3550 的 12 字节头 +
//! 载荷,SSRC/序号/时间戳管理,UDP 发送与发送量统计(供压测指标)。
//! PS 流负载类型固定 96,时钟 90kHz。TCP(RFC 4571)在后续补充。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpStream, UdpSocket};

use common::{Error, Result};

/// PS 流的 RTP 负载类型(GB28181 惯例)。
pub const PT_PS: u8 = 96;
/// 视频 RTP 时钟频率(Hz)。
pub const CLOCK_HZ: u32 = 90_000;

/// 写入 12 字节 RTP 头(V=2,无扩展/CSRC)。`marker` 表示一帧最后一包。
pub fn rtp_header(payload_type: u8, marker: bool, seq: u16, timestamp: u32, ssrc: u32) -> [u8; 12] {
    let mut h = [0u8; 12];
    // 字节0:V=2(10),P=0,X=0,CC=0 → 0x80
    h[0] = 0x80;
    // 字节1:M(1bit) + PT(7bit)
    h[1] = ((marker as u8) << 7) | (payload_type & 0x7f);
    h[2..4].copy_from_slice(&seq.to_be_bytes());
    h[4..8].copy_from_slice(&timestamp.to_be_bytes());
    h[8..12].copy_from_slice(&ssrc.to_be_bytes());
    h
}

/// 构造一个 RTP 包(便于测试与一次性使用;热路径请用 [`rtp_header`] + 复用缓冲)。
pub fn build_rtp_packet(
    payload_type: u8,
    marker: bool,
    seq: u16,
    timestamp: u32,
    ssrc: u32,
    payload: &[u8],
) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(12 + payload.len());
    pkt.extend_from_slice(&rtp_header(payload_type, marker, seq, timestamp, ssrc));
    pkt.extend_from_slice(payload);
    pkt
}

/// 发送统计(供压测指标聚合与现场诊断)。
#[derive(Debug, Default)]
pub struct SendStats {
    /// 已发送 RTP 包数。
    pub packets: AtomicU64,
    /// 已发送字节数(含 RTP 头)。
    pub bytes: AtomicU64,
    /// 已发送 RTCP SR 包数。
    pub rtcp_packets: AtomicU64,
    /// 发送失败次数(本地瓶颈:ENOBUFS / 对端关闭等)。
    pub send_errors: AtomicU64,
}

impl SendStats {
    /// 读取 (包数, 字节数) 快照。
    pub fn snapshot(&self) -> (u64, u64) {
        (
            self.packets.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
        )
    }

    /// 读取完整快照 (包数, 字节数, RTCP 包数, 发送失败数)。
    pub fn full_snapshot(&self) -> (u64, u64, u64, u64) {
        (
            self.packets.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
            self.rtcp_packets.load(Ordering::Relaxed),
            self.send_errors.load(Ordering::Relaxed),
        )
    }
}

/// 构造一个 RTCP Sender Report(SR)包(RFC 3550 §6.4.1,无接收报告块 RC=0)。
///
/// 字段:V=2,P=0,RC=0,PT=200,length=6(以 32 位字计 −1);SSRC;
/// NTP 时间戳(64 位,自 1900 纪元)、RTP 时间戳、发送包数、发送字节数。
/// 供设备侧向平台反馈发送统计(FR-32);当前作为独立可测组件,周期发送待接入 RTCP 端口。
pub fn build_rtcp_sr(
    ssrc: u32,
    ntp_secs: u32,
    ntp_frac: u32,
    rtp_ts: u32,
    packet_count: u32,
    octet_count: u32,
) -> [u8; 28] {
    let mut b = [0u8; 28];
    b[0] = 0x80; // V=2, P=0, RC=0
    b[1] = 200; // PT=SR
    b[2..4].copy_from_slice(&6u16.to_be_bytes()); // length = 28/4 - 1 = 6
    b[4..8].copy_from_slice(&ssrc.to_be_bytes());
    b[8..12].copy_from_slice(&ntp_secs.to_be_bytes());
    b[12..16].copy_from_slice(&ntp_frac.to_be_bytes());
    b[16..20].copy_from_slice(&rtp_ts.to_be_bytes());
    b[20..24].copy_from_slice(&packet_count.to_be_bytes());
    b[24..28].copy_from_slice(&octet_count.to_be_bytes());
    b
}

/// 尝试绑定一对相邻端口:RTP 用 `p`,RTCP 用 `p+1`(RFC 3550 §11 的惯例)。
///
/// `local` 里端口写 0 时由内核挑,于是 `p+1` 可能已被占用 —— 那就整对重来,
/// 试满 [`RTP_PAIR_ATTEMPTS`] 次仍不成就只返回 RTP socket:**收不到 RR 也
/// 不能因此推不了流**,媒体通道本身比反馈统计重要得多。
///
/// `local` 指定了非 0 端口时不做重试(调用方明确要那个端口),
/// `p+1` 占用就退化成无 RTCP 接收。
async fn bind_rtp_pair(local: &str) -> Result<(UdpSocket, Option<Arc<UdpSocket>>)> {
    let fixed_port = local
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .is_some_and(|p| p != 0);
    let host = local.rsplit_once(':').map(|(h, _)| h).unwrap_or("0.0.0.0");
    let attempts = if fixed_port { 1 } else { RTP_PAIR_ATTEMPTS };
    let mut last_rtp = None;
    for _ in 0..attempts {
        let rtp = UdpSocket::bind(local).await.map_err(Error::Io)?;
        let port = rtp.local_addr().map_err(Error::Io)?.port();
        // 端口 65535 没有 +1 可用。
        if let Some(next) = port.checked_add(1) {
            if let Ok(rtcp) = UdpSocket::bind(format!("{host}:{next}")).await {
                return Ok((rtp, Some(Arc::new(rtcp))));
            }
        }
        last_rtp = Some(rtp);
    }
    let rtp = match last_rtp {
        Some(sock) => sock,
        None => UdpSocket::bind(local).await.map_err(Error::Io)?,
    };
    tracing::debug!(
        port = rtp.local_addr().ok().map(|a| a.port()),
        "未能绑定 RTP 端口 +1,本次会话只发 RTCP SR 不收 RR"
    );
    Ok((rtp, None))
}

/// 抢相邻端口对的重试次数。8 次足够:单次冲突概率本身就低,
/// 连续 8 次都撞上说明本机端口紧张,这时退化成无 RR 比一直重试更合理。
const RTP_PAIR_ATTEMPTS: usize = 8;

fn udp_channel(socket: UdpSocket, rtcp_socket: Option<Arc<UdpSocket>>, dst: SocketAddr) -> Channel {
    Channel::Udp {
        socket,
        dst,
        rtcp_dst: SocketAddr::new(dst.ip(), dst.port().saturating_add(1)),
        rtcp_socket,
    }
}

/// 发送通道:UDP 数据报,或 TCP(RFC 4571,每包前置 2 字节大端长度)。
enum Channel {
    Udp {
        socket: UdpSocket,
        dst: SocketAddr,
        /// RTCP 目标端口(惯例为 RTP 端口 + 1)。
        rtcp_dst: SocketAddr,
        /// 本端 RTCP socket(绑在本端 RTP 端口 +1),既用于发 SR 也用于收 RR。
        ///
        /// 之前 SR 是从 RTP socket 发出去的,源端口等于 RTP 端口 —— 严格实现的
        /// 平台会按 RFC 3550 期望 RTCP 来自 RTP 端口 +1,更要紧的是平台的 RR
        /// 会回到那个端口而我们没在听。为 None 表示端口对没抢到,退化成只发不收。
        rtcp_socket: Option<Arc<UdpSocket>>,
    },
    Tcp {
        /// 只留写端:读端交给后台任务收 RTCP(RFC 4571 下 RTP/RTCP 共用一条连接)。
        writer: tokio::net::tcp::OwnedWriteHalf,
    },
}

/// RTP 发送器。持有 SSRC 与自增序号,按 MTU 切片发送 PS 包。
/// 支持 UDP(`RTP/AVP`)与 TCP-ACTIVE(`TCP/RTP/AVP`,主动连平台)两种。
pub struct RtpSender {
    channel: Channel,
    ssrc: u32,
    payload_type: u8,
    seq: u16,
    /// 单个 RTP 载荷最大字节数(留出 IP/UDP/RTP 头余量)。
    max_payload: usize,
    /// 发送缓冲:每包复用,避免每个 RTP 包一次堆分配 + 一次载荷拷贝。
    /// TCP 模式下前 2 字节留给 RFC 4571 长度前缀,从而整包一次 write_all 写出。
    buf: Vec<u8>,
    /// 本次会话起始的 RTP 时间戳偏移(随机初值,RFC 3550 §5.1)。
    ts_offset: u32,
    stats: Arc<SendStats>,
    /// 平台回报的收流质量(RTCP RR)。
    feedback: Arc<crate::rtcp::RtcpFeedback>,
    /// 后台 RTCP 收报任务。随发送器一同销毁(见 `Drop`),
    /// 否则会话结束后任务还抱着 socket 不放。
    rtcp_reader: Option<tokio::task::JoinHandle<()>>,
    /// 会话起点,用于把"最近一次报告"记成相对毫秒(免去每次取系统时间)。
    started: std::time::Instant,
}

impl Drop for RtpSender {
    fn drop(&mut self) {
        if let Some(task) = self.rtcp_reader.take() {
            task.abort();
        }
    }
}

impl RtpSender {
    fn make(channel: Channel, ssrc: u32) -> Self {
        RtpSender {
            channel,
            ssrc,
            payload_type: PT_PS,
            // 序号与时间戳用随机初值:RFC 3550 §5.1 的建议(避免跨会话可预测)。
            seq: rand::random(),
            max_payload: 1400,
            buf: Vec::with_capacity(1500),
            ts_offset: rand::random(),
            stats: Arc::new(SendStats::default()),
            feedback: Arc::new(crate::rtcp::RtcpFeedback::default()),
            rtcp_reader: None,
            started: std::time::Instant::now(),
        }
    }

    /// 构造后立刻拉起 RTCP 收报任务(UDP 用独立 socket,TCP 复用同一条连接的读端)。
    fn with_rtcp_reader(mut self, tcp_reader: Option<tokio::net::tcp::OwnedReadHalf>) -> Self {
        let feedback = Arc::clone(&self.feedback);
        let started = self.started;
        let task = match (&self.channel, tcp_reader) {
            (Channel::Udp { rtcp_socket, .. }, _) => rtcp_socket.as_ref().map(|sock| {
                let sock = Arc::clone(sock);
                tokio::spawn(async move {
                    let mut buf = [0u8; 1500];
                    loop {
                        match sock.recv_from(&mut buf).await {
                            Ok((n, from)) => {
                                Self::ingest_rtcp(&feedback, &buf[..n], started, Some(from));
                            }
                            Err(error) => {
                                tracing::debug!(%error, "RTCP 收报 socket 出错,停止收报");
                                return;
                            }
                        }
                    }
                })
            }),
            (Channel::Tcp { .. }, Some(mut reader)) => Some(tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut len = [0u8; 2];
                let mut buf = vec![0u8; 4096];
                loop {
                    // RFC 4571:2 字节大端长度 + 包体。这条连接上本端只发不收 RTP,
                    // 所以收到的帧要么是 RTCP,要么是平台实现有问题。
                    if reader.read_exact(&mut len).await.is_err() {
                        return;
                    }
                    let n = usize::from(u16::from_be_bytes(len));
                    if n > buf.len() {
                        buf.resize(n, 0);
                    }
                    if reader.read_exact(&mut buf[..n]).await.is_err() {
                        return;
                    }
                    if crate::rtcp::looks_like_rtcp(&buf[..n]) {
                        Self::ingest_rtcp(&feedback, &buf[..n], started, None);
                    }
                }
            })),
            (Channel::Tcp { .. }, None) => None,
        };
        self.rtcp_reader = task;
        self
    }

    fn ingest_rtcp(
        feedback: &crate::rtcp::RtcpFeedback,
        datagram: &[u8],
        started: std::time::Instant,
        from: Option<SocketAddr>,
    ) {
        let now_mid = crate::rtcp::ntp_mid32();
        let elapsed = started.elapsed().as_millis() as u64;
        if feedback.ingest(datagram, now_mid, elapsed) {
            tracing::trace!(?from, bytes = datagram.len(), "收到 RTCP 反馈");
        }
    }

    /// UDP 模式:绑定本地随机端口,面向 `dst` 发送。
    pub async fn new(dst: SocketAddr, ssrc: u32) -> Result<Self> {
        Self::new_bound("0.0.0.0:0", dst, ssrc).await
    }

    /// UDP 模式,指定本地绑定地址(便于把真实端口回填进 SDP)。
    pub async fn new_bound(local: &str, dst: SocketAddr, ssrc: u32) -> Result<Self> {
        let (socket, rtcp_socket) = bind_rtp_pair(local).await?;
        Ok(Self::make(udp_channel(socket, rtcp_socket, dst), ssrc).with_rtcp_reader(None))
    }

    /// TCP-ACTIVE 模式:主动连接平台(WVP 的 TCP-PASSIVE 监听端),RFC 4571 分帧。
    pub async fn new_tcp(dst: SocketAddr, ssrc: u32) -> Result<Self> {
        let stream = TcpStream::connect(dst).await.map_err(|e| {
            tracing::warn!(%dst, error = %e, "TCP-ACTIVE 连接平台失败");
            Error::Io(e)
        })?;
        tracing::info!(%dst, "TCP-ACTIVE 已连接平台收流端口");
        stream.set_nodelay(true).ok();
        Ok(Self::from_tcp(stream, ssrc))
    }

    /// TCP-PASSIVE 模式:本端监听,等平台来连(平台 SDP 声明 `a=setup:active` 时需要)。
    ///
    /// 之前只实现了 TCP-ACTIVE,平台要求设备被动时直接收不到流。
    pub async fn accept_tcp(listener: tokio::net::TcpListener, ssrc: u32) -> Result<Self> {
        let (stream, peer) = listener.accept().await.map_err(Error::Io)?;
        tracing::info!(%peer, "TCP-PASSIVE 平台已连入设备收流端口");
        stream.set_nodelay(true).ok();
        Ok(Self::from_tcp(stream, ssrc))
    }

    /// 由一条已建立的 TCP 连接构造发送器:读端交给 RTCP 收报任务。
    fn from_tcp(stream: TcpStream, ssrc: u32) -> Self {
        let (reader, writer) = stream.into_split();
        Self::make(Channel::Tcp { writer }, ssrc).with_rtcp_reader(Some(reader))
    }

    /// 本端实际绑定地址(UDP 模式)。用于把真实 RTP 端口写进 SDP 的 `m=` 行。
    pub fn local_addr(&self) -> Option<SocketAddr> {
        match &self.channel {
            Channel::Udp { socket, .. } => socket.local_addr().ok(),
            Channel::Tcp { writer } => writer.local_addr().ok(),
        }
    }

    /// 共享的发送统计句柄。
    pub fn stats(&self) -> Arc<SendStats> {
        self.stats.clone()
    }

    /// 共享的 RTCP 反馈句柄(平台回报的丢包/抖动/RTT)。
    pub fn feedback(&self) -> Arc<crate::rtcp::RtcpFeedback> {
        Arc::clone(&self.feedback)
    }

    /// 发送通道建立时刻(RTCP 反馈的"最近报告时间"以此为原点)。
    pub fn started(&self) -> std::time::Instant {
        self.started
    }

    /// 本端 RTCP 端口。为 None 表示没抢到 RTP 端口 +1,此时只发 SR 不收 RR。
    pub fn local_rtcp_port(&self) -> Option<u16> {
        match &self.channel {
            Channel::Udp { rtcp_socket, .. } => rtcp_socket
                .as_ref()
                .and_then(|s| s.local_addr().ok())
                .map(|a| a.port()),
            // TCP 下 RTCP 与 RTP 共用连接,没有独立端口。
            Channel::Tcp { .. } => None,
        }
    }

    /// 发送一条 RTCP Sender Report,携带当前发送包数/字节数。
    ///
    /// UDP 走 RTP 端口 +1;TCP 复用同一条 RFC 4571 连接(国标 TCP 下的常见做法)。
    pub async fn send_rtcp_sr(&mut self, rtp_ts: u32) -> Result<()> {
        let (packets, bytes) = self.stats.snapshot();
        let (ntp_secs, ntp_frac) = ntp_now();
        let sr = build_rtcp_sr(
            self.ssrc,
            ntp_secs,
            ntp_frac,
            rtp_ts.wrapping_add(self.ts_offset),
            packets as u32,
            bytes as u32,
        );
        let result = match &mut self.channel {
            Channel::Udp {
                socket,
                rtcp_dst,
                rtcp_socket,
                ..
            } => match rtcp_socket {
                // 有专用 RTCP socket 就用它发:源端口 = 本端 RTP 端口 +1,
                // 平台按 RFC 3550 的期望回 RR 时才能落回我们在听的那个端口。
                Some(sock) => sock.send_to(&sr, *rtcp_dst).await.map(|_| ()),
                None => socket.send_to(&sr, *rtcp_dst).await.map(|_| ()),
            },
            Channel::Tcp { writer } => {
                let mut framed = [0u8; 30];
                framed[0..2].copy_from_slice(&(sr.len() as u16).to_be_bytes());
                framed[2..].copy_from_slice(&sr);
                writer.write_all(&framed).await
            }
        };
        match result {
            Ok(()) => {
                self.stats.rtcp_packets.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(e) => Err(Error::Io(e)),
        }
    }

    /// 设置 RTP 负载类型。
    ///
    /// 默认 96(PS 封装,`PT_PS`)。语音广播/对讲的音频流按附录 C.2.4 走
    /// **裸 G.711 而非 PS**,负载类型固定 8(PCMA)。
    pub fn set_payload_type(&mut self, pt: u8) {
        self.payload_type = pt;
    }

    /// 组一个 RTP 包并发出(不切片)。`send_frame` 与 `send_audio` 共用。
    async fn send_packet(&mut self, payload: &[u8], ts: u32, marker: bool) -> Result<()> {
        let is_tcp = matches!(self.channel, Channel::Tcp { .. });
        self.buf.clear();
        if is_tcp {
            self.buf.extend_from_slice(&[0, 0]); // RFC 4571 长度前缀占位
        }
        self.buf.extend_from_slice(&rtp_header(
            self.payload_type,
            marker,
            self.seq,
            ts,
            self.ssrc,
        ));
        self.buf.extend_from_slice(payload);
        if is_tcp {
            let rtp_len = (self.buf.len() - 2) as u16;
            self.buf[0..2].copy_from_slice(&rtp_len.to_be_bytes());
        }

        let sent = match &mut self.channel {
            Channel::Udp { socket, dst, .. } => socket.send_to(&self.buf, *dst).await.map(|_| ()),
            Channel::Tcp { writer } => writer.write_all(&self.buf).await,
        };
        if let Err(e) = sent {
            self.stats.send_errors.fetch_add(1, Ordering::Relaxed);
            return Err(Error::Io(e));
        }

        self.seq = self.seq.wrapping_add(1);
        self.stats.packets.fetch_add(1, Ordering::Relaxed);
        let counted = if is_tcp {
            self.buf.len() as u64 - 2
        } else {
            self.buf.len() as u64
        };
        self.stats.bytes.fetch_add(counted, Ordering::Relaxed);
        Ok(())
    }

    /// 发送一个音频访问单元(一个 RTP 包,**不切片**)。
    ///
    /// 附录 C.2.4:一个 RTP 包的音频载荷应为整数个音频编码帧、时长 20~180 ms,
    /// 所以音频从不跨包切分 —— 切了对端就解不出帧。
    /// marker 按 RFC 3551 §4.1 只在 talkspurt 首包置 1。
    pub async fn send_audio(&mut self, au: &[u8], timestamp: u32, marker: bool) -> Result<()> {
        if au.is_empty() {
            return Ok(());
        }
        let ts = timestamp.wrapping_add(self.ts_offset);
        self.send_packet(au, ts, marker).await
    }

    /// 发送一个 PS 访问单元(一帧):按 max_payload 切片成多个 RTP 包,
    /// 同一 timestamp,最后一包置 marker。
    pub async fn send_frame(&mut self, ps: &[u8], timestamp: u32) -> Result<()> {
        if ps.is_empty() {
            return Ok(());
        }
        let ts = timestamp.wrapping_add(self.ts_offset);
        let total = ps.len();
        let step = self.max_payload.max(1);
        let mut offset = 0usize;
        while offset < total {
            let end = (offset + step).min(total);
            let marker = end == total;
            // chunk 借的是入参 ps(不是 self),与 &mut self 不冲突。
            self.send_packet(&ps[offset..end], ts, marker).await?;
            offset = end;
        }
        Ok(())
    }
}

/// 预先准备好的媒体通道。
///
/// 为什么需要"预先":设备侧 200 OK 的 SDP 必须写**真实的本端 RTP 端口**
/// (SDP 里端口 0 表示该媒体被禁用),所以端口要在发 200 之前就确定下来;
/// 而实际连接/收流要等 200 之后再做(TCP-PASSIVE 下平台是收到 200 才来连的)。
pub enum PreparedMedia {
    /// UDP:socket 已绑定,端口已确定。
    Udp {
        /// 已绑定的 RTP socket。
        socket: UdpSocket,
        /// 平台收流地址。
        dst: SocketAddr,
        /// 已绑定的本端 RTCP socket(RTP 端口 +1);抢不到时为 None。
        rtcp_socket: Option<Arc<UdpSocket>>,
    },
    /// TCP-ACTIVE:本端 socket 已绑定(端口已确定),稍后主动连平台。
    TcpActive {
        socket: tokio::net::TcpSocket,
        dst: SocketAddr,
    },
    /// TCP-PASSIVE:本端已监听,稍后 accept 平台连入。
    TcpPassive { listener: tokio::net::TcpListener },
}

impl PreparedMedia {
    /// 绑定本端媒体端口。`tcp`=是否 RTP over TCP;`passive`=本端是否被动等连。
    pub async fn prepare(dst: SocketAddr, tcp: bool, passive: bool) -> Result<Self> {
        if !tcp {
            let (socket, rtcp_socket) = bind_rtp_pair("0.0.0.0:0").await?;
            return Ok(PreparedMedia::Udp {
                socket,
                dst,
                rtcp_socket,
            });
        }
        if passive {
            let listener = tokio::net::TcpListener::bind("0.0.0.0:0")
                .await
                .map_err(Error::Io)?;
            return Ok(PreparedMedia::TcpPassive { listener });
        }
        let socket = tokio::net::TcpSocket::new_v4().map_err(Error::Io)?;
        socket
            .bind("0.0.0.0:0".parse().expect("常量地址可解析"))
            .map_err(Error::Io)?;
        Ok(PreparedMedia::TcpActive { socket, dst })
    }

    /// 已确定的本端媒体端口(写进 SDP 的 `m=` 行)。
    pub fn local_port(&self) -> u16 {
        let addr = match self {
            PreparedMedia::Udp { socket, .. } => socket.local_addr().ok(),
            PreparedMedia::TcpActive { socket, .. } => socket.local_addr().ok(),
            PreparedMedia::TcpPassive { listener } => listener.local_addr().ok(),
        };
        addr.map(|a| a.port()).unwrap_or(0)
    }

    /// 是否走 TCP。
    pub fn is_tcp(&self) -> bool {
        !matches!(self, PreparedMedia::Udp { .. })
    }

    /// 完成连接/接受,得到可用的发送器。
    pub async fn into_sender(self, ssrc: u32) -> Result<RtpSender> {
        match self {
            PreparedMedia::Udp {
                socket,
                dst,
                rtcp_socket,
            } => {
                Ok(RtpSender::make(udp_channel(socket, rtcp_socket, dst), ssrc)
                    .with_rtcp_reader(None))
            }
            PreparedMedia::TcpActive { socket, dst } => {
                let stream = socket.connect(dst).await.map_err(|e| {
                    tracing::warn!(%dst, error = %e, "TCP-ACTIVE 连接平台失败");
                    Error::Io(e)
                })?;
                tracing::info!(%dst, "TCP-ACTIVE 已连接平台收流端口");
                stream.set_nodelay(true).ok();
                Ok(RtpSender::from_tcp(stream, ssrc))
            }
            PreparedMedia::TcpPassive { listener } => RtpSender::accept_tcp(listener, ssrc).await,
        }
    }
}

/// 当前时刻的 NTP 时间戳(自 1900 纪元的秒 + 小数部分)。
fn ntp_now() -> (u32, u32) {
    /// 1900-01-01 到 1970-01-01 的秒数。
    const NTP_EPOCH_OFFSET: u64 = 2_208_988_800;
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => {
            let secs = (d.as_secs() + NTP_EPOCH_OFFSET) as u32;
            let frac = ((d.subsec_nanos() as u64) << 32) / 1_000_000_000;
            (secs, frac as u32)
        }
        Err(_) => (0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtp_头字段正确() {
        let pkt = build_rtp_packet(PT_PS, true, 0x1234, 0xdeadbeef, 0xcafebabe, &[1, 2, 3]);
        assert_eq!(pkt[0], 0x80); // V=2
        assert_eq!(pkt[1], 0x80 | 96); // marker + PT96
        assert_eq!(&pkt[2..4], &[0x12, 0x34]); // seq
        assert_eq!(&pkt[4..8], &[0xde, 0xad, 0xbe, 0xef]); // timestamp
        assert_eq!(&pkt[8..12], &[0xca, 0xfe, 0xba, 0xbe]); // ssrc
        assert_eq!(&pkt[12..], &[1, 2, 3]); // payload
    }

    #[tokio::test]
    async fn udp_下绑定相邻端口对且sr从rtcp端口发出() {
        // 扮演平台:RTP 收流端口 + RTCP 端口(RTP+1)。
        let plat_rtp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let plat_rtp_port = plat_rtp.local_addr().unwrap().port();
        let plat_rtcp = UdpSocket::bind(format!("127.0.0.1:{}", plat_rtp_port + 1))
            .await
            .expect("测试环境应能绑到 +1 端口");
        let dst: SocketAddr = format!("127.0.0.1:{plat_rtp_port}").parse().unwrap();

        let mut sender = RtpSender::new_bound("127.0.0.1:0", dst, 0x1234)
            .await
            .unwrap();
        let rtp_port = sender.local_addr().unwrap().port();
        assert_eq!(
            sender.local_rtcp_port(),
            Some(rtp_port + 1),
            "RTCP 应绑在 RTP 端口 +1"
        );

        sender.send_rtcp_sr(0).await.unwrap();
        let mut buf = [0u8; 256];
        let (n, from) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            plat_rtcp.recv_from(&mut buf),
        )
        .await
        .expect("应在 2s 内收到 SR")
        .unwrap();
        assert!(crate::rtcp::looks_like_rtcp(&buf[..n]));
        assert_eq!(
            from.port(),
            rtp_port + 1,
            "SR 的源端口应是本端 RTCP 端口,平台才知道把 RR 回到哪"
        );
    }

    #[tokio::test]
    async fn udp_下收到平台rr后反馈可见() {
        let plat = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let dst = plat.local_addr().unwrap();
        let sender = RtpSender::new_bound("127.0.0.1:0", dst, 0x0BEB_C201)
            .await
            .unwrap();
        let feedback = sender.feedback();
        let rtcp_port = sender.local_rtcp_port().expect("应拿到 RTCP 端口");
        assert!(!feedback.has_feedback());

        // 平台回一份 RR:10% 丢包、90kHz 下 15ms 抖动。
        let report = crate::rtcp::ReceptionReport {
            ssrc: 0x0BEB_C201,
            fraction_lost: 26,
            cumulative_lost: 42,
            extended_highest_seq: 1000,
            jitter: 1350,
            last_sr: 0,
            delay_since_last_sr: 0,
        };
        plat.send_to(
            &crate::rtcp::build_rtcp_rr(0xABCD, &report),
            format!("127.0.0.1:{rtcp_port}"),
        )
        .await
        .unwrap();

        // 收报是后台任务,给它一点时间。
        for _ in 0..100 {
            if feedback.has_feedback() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let snap = feedback.snapshot(CLOCK_HZ, 0);
        assert_eq!(snap.reports, 1, "应收到并计入一份接收报告");
        assert_eq!(snap.cumulative_lost, 42);
        assert!((snap.loss_percent - 26.0 * 100.0 / 256.0).abs() < 1e-9);
        assert!((snap.jitter_ms - 15.0).abs() < 1e-9);
        assert_eq!(snap.reporter_ssrc, 0xABCD);

        // 发送器销毁后收报任务应随之结束,不再抱着 socket。
        drop(sender);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            UdpSocket::bind(format!("127.0.0.1:{rtcp_port}"))
                .await
                .is_ok(),
            "RTCP 端口应已释放"
        );
    }

    #[tokio::test]
    async fn tcp_下从同一连接收rr() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dst = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move { listener.accept().await.unwrap().0 });
        let sender = RtpSender::new_tcp(dst, 0x0BEB_C201).await.unwrap();
        let feedback = sender.feedback();
        let mut plat = accept.await.unwrap();
        assert_eq!(
            sender.local_rtcp_port(),
            None,
            "TCP 下 RTCP 与 RTP 共用连接,没有独立端口"
        );

        let report = crate::rtcp::ReceptionReport {
            ssrc: 0x0BEB_C201,
            fraction_lost: 128,
            cumulative_lost: -2,
            extended_highest_seq: 77,
            jitter: 90,
            last_sr: 0,
            delay_since_last_sr: 0,
        };
        let rr = crate::rtcp::build_rtcp_rr(0x5555, &report);
        // RFC 4571 分帧。
        let mut framed = Vec::new();
        framed.extend_from_slice(&(rr.len() as u16).to_be_bytes());
        framed.extend_from_slice(&rr);
        plat.write_all(&framed).await.unwrap();

        for _ in 0..100 {
            if feedback.has_feedback() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let snap = feedback.snapshot(CLOCK_HZ, 0);
        assert_eq!(snap.reports, 1);
        assert_eq!(snap.cumulative_lost, -2);
        assert_eq!(snap.reporter_ssrc, 0x5555);
        assert!((snap.jitter_ms - 1.0).abs() < 1e-9);
    }

    #[test]
    fn rtcp_sr_字段正确() {
        let sr = build_rtcp_sr(0xcafebabe, 0x11111111, 0x22222222, 0x33333333, 100, 5000);
        assert_eq!(sr[0], 0x80); // V=2 RC=0
        assert_eq!(sr[1], 200); // PT=SR
        assert_eq!(&sr[2..4], &6u16.to_be_bytes()); // length=6
        assert_eq!(&sr[4..8], &0xcafebabeu32.to_be_bytes()); // SSRC
        assert_eq!(&sr[20..24], &100u32.to_be_bytes()); // packet count
        assert_eq!(&sr[24..28], &5000u32.to_be_bytes()); // octet count
    }

    #[tokio::test]
    async fn udp_模式暴露真实本地端口() {
        let rx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sender = RtpSender::new(rx.local_addr().unwrap(), 1).await.unwrap();
        let local = sender.local_addr().expect("应能取到本地地址");
        assert_ne!(local.port(), 0, "SDP 里必须回填真实端口,0 表示媒体被禁用");
    }

    #[tokio::test]
    async fn rtcp_sr_发到_rtp_端口加一() {
        // RTP 收在 rtp_sock,RTCP 应落到端口 +1。找一对相邻可用端口。
        let mut rtp_sock = None;
        let mut rtcp_sock = None;
        for _ in 0..50 {
            let a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let port = a.local_addr().unwrap().port();
            if let Ok(b) = UdpSocket::bind(("127.0.0.1", port + 1)).await {
                rtp_sock = Some(a);
                rtcp_sock = Some(b);
                break;
            }
        }
        let (rtp_sock, rtcp_sock) = match (rtp_sock, rtcp_sock) {
            (Some(a), Some(b)) => (a, b),
            _ => return, // 环境里抢不到相邻端口,跳过而不是假失败
        };

        let mut sender = RtpSender::new(rtp_sock.local_addr().unwrap(), 0xCAFEBABE)
            .await
            .unwrap();
        sender.send_frame(&[0u8; 32], 900).await.unwrap();
        sender.send_rtcp_sr(900).await.unwrap();

        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), rtcp_sock.recv(&mut buf))
            .await
            .expect("超时未收到 RTCP")
            .unwrap();
        assert_eq!(n, 28);
        assert_eq!(buf[1], 200, "应为 RTCP SR");
        assert_eq!(&buf[4..8], &0xCAFEBABEu32.to_be_bytes());
        // SR 里的包数应反映已发的 RTP 包。
        assert_eq!(u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]), 1);
        assert_eq!(sender.stats().full_snapshot().2, 1, "应记一条 RTCP");
    }

    #[tokio::test]
    async fn tcp_模式按_rfc4571_加长度前缀且一次写出() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move { listener.accept().await.unwrap().0 });

        let mut sender = RtpSender::new_tcp(addr, 0x11223344).await.unwrap();
        let mut stream = accept.await.unwrap();
        sender.max_payload = 8;
        sender.send_frame(&[0xAAu8; 20], 4500).await.unwrap();

        use tokio::io::AsyncReadExt;
        // 20 字节载荷 @8 → 3 包(8+8+4),每包 = 2 字节长度 + 12 字节头 + 载荷。
        let mut buf = vec![0u8; 2 * 3 + 3 * 12 + 20];
        stream.read_exact(&mut buf).await.unwrap();
        let mut off = 0;
        let expect_payloads = [8usize, 8, 4];
        for (i, &plen) in expect_payloads.iter().enumerate() {
            let len = u16::from_be_bytes([buf[off], buf[off + 1]]) as usize;
            assert_eq!(len, 12 + plen, "长度前缀应是 RTP 包长(不含前缀本身)");
            let marker = buf[off + 3] & 0x80 != 0;
            assert_eq!(marker, i == expect_payloads.len() - 1);
            off += 2 + len;
        }
    }

    #[tokio::test]
    async fn tcp_passive_接受平台连入() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accepting = tokio::spawn(async move { RtpSender::accept_tcp(listener, 0x99).await });
        let mut client = TcpStream::connect(addr).await.unwrap();
        let mut sender = accepting.await.unwrap().expect("应接受连接");
        sender.send_frame(&[1u8, 2, 3], 900).await.unwrap();

        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 2 + 12 + 3];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(u16::from_be_bytes([buf[0], buf[1]]) as usize, 15);
        assert_eq!(&buf[14..], &[1, 2, 3]);
    }

    #[tokio::test]
    async fn 发送分片_seq递增_marker置尾() {
        // 接收端。
        let rx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let rx_addr = rx.local_addr().unwrap();

        let mut sender = RtpSender::new(rx_addr, 0x11223344).await.unwrap();
        sender.max_payload = 4; // 强制切片

        // 10 字节 → 3 片(4+4+2)。
        sender.send_frame(&[0u8; 10], 900).await.unwrap();

        let mut markers = Vec::new();
        let mut seqs = Vec::new();
        for _ in 0..3 {
            let mut buf = [0u8; 64];
            let n = rx.recv(&mut buf).await.unwrap();
            assert!(n >= 12);
            markers.push(buf[1] & 0x80 != 0);
            seqs.push(u16::from_be_bytes([buf[2], buf[3]]));
        }
        assert_eq!(markers, vec![false, false, true]); // 仅最后一片 marker
                                                       // seq 起始值是随机的(RFC 3550 §5.1),只断言逐包 +1。
        assert_eq!(seqs[1], seqs[0].wrapping_add(1));
        assert_eq!(seqs[2], seqs[1].wrapping_add(1));
        let (pkts, bytes) = sender.stats().snapshot();
        assert_eq!(pkts, 3);
        assert!(bytes >= 10 + 3 * 12);
    }
}
