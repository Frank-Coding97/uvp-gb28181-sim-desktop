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

/// 发送通道:UDP 数据报,或 TCP(RFC 4571,每包前置 2 字节大端长度)。
enum Channel {
    Udp {
        socket: UdpSocket,
        dst: SocketAddr,
        /// RTCP 目标端口(惯例为 RTP 端口 + 1)。
        rtcp_dst: SocketAddr,
    },
    Tcp {
        stream: TcpStream,
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
        }
    }

    /// UDP 模式:绑定本地随机端口,面向 `dst` 发送。
    pub async fn new(dst: SocketAddr, ssrc: u32) -> Result<Self> {
        Self::new_bound("0.0.0.0:0", dst, ssrc).await
    }

    /// UDP 模式,指定本地绑定地址(便于把真实端口回填进 SDP)。
    pub async fn new_bound(local: &str, dst: SocketAddr, ssrc: u32) -> Result<Self> {
        let socket = UdpSocket::bind(local).await.map_err(Error::Io)?;
        let rtcp_dst = SocketAddr::new(dst.ip(), dst.port().saturating_add(1));
        Ok(Self::make(
            Channel::Udp {
                socket,
                dst,
                rtcp_dst,
            },
            ssrc,
        ))
    }

    /// TCP-ACTIVE 模式:主动连接平台(WVP 的 TCP-PASSIVE 监听端),RFC 4571 分帧。
    pub async fn new_tcp(dst: SocketAddr, ssrc: u32) -> Result<Self> {
        let stream = TcpStream::connect(dst).await.map_err(|e| {
            tracing::warn!(%dst, error = %e, "TCP-ACTIVE 连接平台失败");
            Error::Io(e)
        })?;
        tracing::info!(%dst, "TCP-ACTIVE 已连接平台收流端口");
        stream.set_nodelay(true).ok();
        Ok(Self::make(Channel::Tcp { stream }, ssrc))
    }

    /// TCP-PASSIVE 模式:本端监听,等平台来连(平台 SDP 声明 `a=setup:active` 时需要)。
    ///
    /// 之前只实现了 TCP-ACTIVE,平台要求设备被动时直接收不到流。
    pub async fn accept_tcp(listener: tokio::net::TcpListener, ssrc: u32) -> Result<Self> {
        let (stream, peer) = listener.accept().await.map_err(Error::Io)?;
        tracing::info!(%peer, "TCP-PASSIVE 平台已连入设备收流端口");
        stream.set_nodelay(true).ok();
        Ok(Self::make(Channel::Tcp { stream }, ssrc))
    }

    /// 本端实际绑定地址(UDP 模式)。用于把真实 RTP 端口写进 SDP 的 `m=` 行。
    pub fn local_addr(&self) -> Option<SocketAddr> {
        match &self.channel {
            Channel::Udp { socket, .. } => socket.local_addr().ok(),
            Channel::Tcp { stream } => stream.local_addr().ok(),
        }
    }

    /// 共享的发送统计句柄。
    pub fn stats(&self) -> Arc<SendStats> {
        self.stats.clone()
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
                socket, rtcp_dst, ..
            } => socket.send_to(&sr, *rtcp_dst).await.map(|_| ()),
            Channel::Tcp { stream } => {
                let mut framed = [0u8; 30];
                framed[0..2].copy_from_slice(&(sr.len() as u16).to_be_bytes());
                framed[2..].copy_from_slice(&sr);
                stream.write_all(&framed).await
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

    /// 发送一个 PS 访问单元(一帧):按 max_payload 切片成多个 RTP 包,
    /// 同一 timestamp,最后一包置 marker。
    pub async fn send_frame(&mut self, ps: &[u8], timestamp: u32) -> Result<()> {
        if ps.is_empty() {
            return Ok(());
        }
        let ts = timestamp.wrapping_add(self.ts_offset);
        let is_tcp = matches!(self.channel, Channel::Tcp { .. });
        let total = ps.len();
        let step = self.max_payload.max(1);
        let mut offset = 0usize;
        while offset < total {
            let end = (offset + step).min(total);
            let marker = end == total;
            let payload = &ps[offset..end];

            // 组包进复用缓冲:一次分配都不做,TCP 也只需一次 write_all。
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
                Channel::Udp { socket, dst, .. } => {
                    socket.send_to(&self.buf, *dst).await.map(|_| ())
                }
                Channel::Tcp { stream } => stream.write_all(&self.buf).await,
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
    Udp { socket: UdpSocket, dst: SocketAddr },
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
            let socket = UdpSocket::bind("0.0.0.0:0").await.map_err(Error::Io)?;
            return Ok(PreparedMedia::Udp { socket, dst });
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
            PreparedMedia::Udp { socket, dst } => {
                let rtcp_dst = SocketAddr::new(dst.ip(), dst.port().saturating_add(1));
                Ok(RtpSender::make(
                    Channel::Udp {
                        socket,
                        dst,
                        rtcp_dst,
                    },
                    ssrc,
                ))
            }
            PreparedMedia::TcpActive { socket, dst } => {
                let stream = socket.connect(dst).await.map_err(|e| {
                    tracing::warn!(%dst, error = %e, "TCP-ACTIVE 连接平台失败");
                    Error::Io(e)
                })?;
                tracing::info!(%dst, "TCP-ACTIVE 已连接平台收流端口");
                stream.set_nodelay(true).ok();
                Ok(RtpSender::make(Channel::Tcp { stream }, ssrc))
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
