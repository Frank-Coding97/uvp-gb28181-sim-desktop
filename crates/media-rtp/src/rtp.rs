//! RTP 打包与发送。
//!
//! 实现 docs/40-protocol/media-ps-rtp.md 的 RTP 层:RFC 3550 的 12 字节头 +
//! 载荷,SSRC/序号/时间戳管理,UDP 发送与发送量统计(供压测指标)。
//! PS 流负载类型固定 96,时钟 90kHz。TCP(RFC 4571)在后续补充。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::net::UdpSocket;

use common::{Error, Result};

/// PS 流的 RTP 负载类型(GB28181 惯例)。
pub const PT_PS: u8 = 96;
/// 视频 RTP 时钟频率(Hz)。
pub const CLOCK_HZ: u32 = 90_000;

/// 构造一个 RTP 包(V=2,无扩展/CSRC)。`marker` 表示一帧最后一包。
pub fn build_rtp_packet(
    payload_type: u8,
    marker: bool,
    seq: u16,
    timestamp: u32,
    ssrc: u32,
    payload: &[u8],
) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(12 + payload.len());
    // 字节0:V=2(10),P=0,X=0,CC=0 → 0x80
    pkt.push(0x80);
    // 字节1:M(1bit) + PT(7bit)
    pkt.push(((marker as u8) << 7) | (payload_type & 0x7f));
    pkt.extend_from_slice(&seq.to_be_bytes());
    pkt.extend_from_slice(&timestamp.to_be_bytes());
    pkt.extend_from_slice(&ssrc.to_be_bytes());
    pkt.extend_from_slice(payload);
    pkt
}

/// 发送统计(供压测指标聚合)。
#[derive(Debug, Default)]
pub struct SendStats {
    /// 已发送 RTP 包数。
    pub packets: AtomicU64,
    /// 已发送字节数(含 RTP 头)。
    pub bytes: AtomicU64,
}

impl SendStats {
    /// 读取 (包数, 字节数) 快照。
    pub fn snapshot(&self) -> (u64, u64) {
        (self.packets.load(Ordering::Relaxed), self.bytes.load(Ordering::Relaxed))
    }
}

/// UDP RTP 发送器。持有目标地址、SSRC 与自增的序号,按 MTU 切片发送 PS 包。
pub struct RtpSender {
    socket: UdpSocket,
    dst: SocketAddr,
    ssrc: u32,
    payload_type: u8,
    seq: u16,
    /// 单个 RTP 载荷最大字节数(留出 IP/UDP/RTP 头余量)。
    max_payload: usize,
    stats: Arc<SendStats>,
}

impl RtpSender {
    /// 绑定本地随机端口,面向 `dst` 发送,使用给定 SSRC。
    pub async fn new(dst: SocketAddr, ssrc: u32) -> Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0").await.map_err(Error::Io)?;
        Ok(RtpSender {
            socket,
            dst,
            ssrc,
            payload_type: PT_PS,
            seq: 0,
            max_payload: 1400,
            stats: Arc::new(SendStats::default()),
        })
    }

    /// 本地绑定端口。
    pub fn local_port(&self) -> u16 {
        self.socket.local_addr().map(|a| a.port()).unwrap_or(0)
    }

    /// 共享的发送统计句柄。
    pub fn stats(&self) -> Arc<SendStats> {
        self.stats.clone()
    }

    /// 发送一个 PS 访问单元(一帧):按 max_payload 切片成多个 RTP 包,
    /// 同一 timestamp,最后一包置 marker。
    pub async fn send_frame(&mut self, ps: &[u8], timestamp: u32) -> Result<()> {
        if ps.is_empty() {
            return Ok(());
        }
        let chunks: Vec<&[u8]> = ps.chunks(self.max_payload).collect();
        let last = chunks.len() - 1;
        for (i, chunk) in chunks.iter().enumerate() {
            let pkt = build_rtp_packet(
                self.payload_type,
                i == last,
                self.seq,
                timestamp,
                self.ssrc,
                chunk,
            );
            self.socket.send_to(&pkt, self.dst).await.map_err(Error::Io)?;
            self.seq = self.seq.wrapping_add(1);
            self.stats.packets.fetch_add(1, Ordering::Relaxed);
            self.stats.bytes.fetch_add(pkt.len() as u64, Ordering::Relaxed);
        }
        Ok(())
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
        assert_eq!(seqs, vec![0, 1, 2]); // seq 递增
        let (pkts, bytes) = sender.stats().snapshot();
        assert_eq!(pkts, 3);
        assert!(bytes >= 10 + 3 * 12);
    }
}
