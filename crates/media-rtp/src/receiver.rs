//! RTP 音频**接收**端(语音广播 / 语音对讲的下行方向)。
//!
//! GB/T 28181-2022 §9.12.1:语音广播里**设备是语音流接收者** —— 设备主动发 INVITE,
//! 平台的媒体服务器把 G.711 音频以 RTP 推给设备。本模块负责那条下行链路:
//! 绑定收流端口、按 RFC 3550 解 RTP 头、按序号估算丢包、把 G.711 解成线性 PCM 算电平。
//!
//! 为什么要算电平:模拟器没有喇叭,"收到包"和"收到有声音的包"是两件事。
//! 平台把静音流推过来时包数照涨,只有电平能说明对面到底有没有在讲话。
//!
//! 附录 K a) 的媒体流丢失监测也落在这里:`RecvStats::idle_for` 给出"多久没收到包",
//! 上层据此释放链路并发会话内 BYE。

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::{Error, Result};
use tokio::net::UdpSocket;

/// RTP 固定头长度(无 CSRC / 无扩展)。
const RTP_HEADER_LEN: usize = 12;
/// G.711 A-law 的 RTP 负载类型(静态绑定,RFC 3551)。
pub const PT_PCMA: u8 = 8;
/// G.711 μ-law 的 RTP 负载类型(静态绑定,RFC 3551)。
pub const PT_PCMU: u8 = 0;
/// 单包上限:G.711 8 kHz 单声道即使 1 秒一包也只有 8000 字节,2 KiB 足够,
/// 留 4 KiB 容纳 PS 封装的音频包。
const MAX_DATAGRAM: usize = 4096;

/// 收流统计。全部用原子量,收流任务写、UI/指标读,不需要锁。
#[derive(Debug, Default)]
pub struct RecvStats {
    /// 收到的 RTP 包数。
    packets: AtomicU64,
    /// 收到的负载字节数(不含 RTP 头)。
    payload_bytes: AtomicU64,
    /// 按序号跳变估算的丢包数。
    lost: AtomicU64,
    /// 乱序/重复(序号回退)包数。
    reordered: AtomicU64,
    /// 头部非法被丢弃的包数。
    malformed: AtomicU64,
    /// 最近一包的负载类型(0xFFFF = 尚未收到)。
    last_payload_type: AtomicU32,
    /// 最近一包的音量百分比(0~100)。非 G.711 负载时保持 0。
    level_pct: AtomicU32,
    /// 最近一次收包距进程启动的毫秒数(0 = 尚未收到)。
    last_recv_ms: AtomicU64,
    /// 收流起始时刻距进程启动的毫秒数。
    started_ms: AtomicU64,
}

/// [`RecvStats`] 的可序列化快照。
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct RecvStatsSnapshot {
    pub packets: u64,
    pub payload_bytes: u64,
    pub lost: u64,
    pub reordered: u64,
    pub malformed: u64,
    /// 最近一包负载类型;尚未收到时为 None。
    pub payload_type: Option<u8>,
    /// 最近一包音量百分比(0~100)。
    pub level_pct: u32,
    /// 距最近一次收包的毫秒数;尚未收到时为 None。
    pub idle_ms: Option<u64>,
}

/// 单调时钟的进程内基准。用 `Instant` 而非系统时间:校时会让系统时间跳变。
fn now_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static BASE: OnceLock<Instant> = OnceLock::new();
    BASE.get_or_init(Instant::now).elapsed().as_millis() as u64
}

impl RecvStats {
    fn mark_started(&self) {
        self.started_ms.store(now_ms().max(1), Ordering::Relaxed);
    }

    /// 距最近一次收包多久。**从未收到过包时以收流开始时刻计**,
    /// 否则"平台一个包都没发"会被误判成"链路健康"。
    pub fn idle_for(&self) -> Option<Duration> {
        let last = self.last_recv_ms.load(Ordering::Relaxed);
        let base = if last > 0 {
            last
        } else {
            let started = self.started_ms.load(Ordering::Relaxed);
            if started == 0 {
                return None;
            }
            started
        };
        Some(Duration::from_millis(now_ms().saturating_sub(base)))
    }

    /// 是否收到过任何一包。
    pub fn has_media(&self) -> bool {
        self.packets.load(Ordering::Relaxed) > 0
    }

    pub fn snapshot(&self) -> RecvStatsSnapshot {
        let pt = self.last_payload_type.load(Ordering::Relaxed);
        RecvStatsSnapshot {
            packets: self.packets.load(Ordering::Relaxed),
            payload_bytes: self.payload_bytes.load(Ordering::Relaxed),
            lost: self.lost.load(Ordering::Relaxed),
            reordered: self.reordered.load(Ordering::Relaxed),
            malformed: self.malformed.load(Ordering::Relaxed),
            payload_type: (pt <= 0x7F).then_some(pt as u8),
            level_pct: self.level_pct.load(Ordering::Relaxed),
            idle_ms: self.idle_for().map(|d| d.as_millis() as u64),
        }
    }
}

/// G.711 A-law → 16 位线性 PCM(ITU-T G.711 参考实现)。
pub fn alaw_to_linear(a_val: u8) -> i16 {
    let a_val = a_val ^ 0x55;
    let mut t = ((a_val & 0x0F) as i32) << 4;
    let seg = ((a_val & 0x70) >> 4) as u32;
    match seg {
        0 => t += 8,
        1 => t += 0x108,
        _ => {
            t += 0x108;
            t <<= seg - 1;
        }
    }
    if a_val & 0x80 != 0 {
        t as i16
    } else {
        -t as i16
    }
}

/// G.711 μ-law → 16 位线性 PCM(ITU-T G.711 参考实现)。
pub fn ulaw_to_linear(u_val: u8) -> i16 {
    const BIAS: i32 = 0x84;
    let u_val = !u_val;
    let mut t = (((u_val & 0x0F) as i32) << 3) + BIAS;
    t <<= ((u_val & 0x70) >> 4) as u32;
    if u_val & 0x80 != 0 {
        (BIAS - t) as i16
    } else {
        (t - BIAS) as i16
    }
}

/// 按负载类型算音量百分比(峰值 / i16::MAX)。非 G.711 返回 None。
fn level_percent(payload_type: u8, payload: &[u8]) -> Option<u32> {
    let decode: fn(u8) -> i16 = match payload_type {
        PT_PCMA => alaw_to_linear,
        PT_PCMU => ulaw_to_linear,
        _ => return None,
    };
    if payload.is_empty() {
        return Some(0);
    }
    let peak = payload
        .iter()
        .map(|&b| decode(b).unsigned_abs() as u32)
        .max()
        .unwrap_or(0);
    Some((peak * 100 / i16::MAX as u32).min(100))
}

/// 音频收流端:绑定本地 UDP 端口,持续接收 RTP 并累计统计。
pub struct AudioReceiver {
    socket: UdpSocket,
    stats: Arc<RecvStats>,
}

impl AudioReceiver {
    /// 在指定本地 IP 上绑一个系统分配的端口。
    ///
    /// 端口必须**先绑再发 INVITE**:SDP 的 `m=audio <port>` 要填真实端口,
    /// 填 0 等于告诉平台"往 0 端口发",链路必然建不起来。
    pub async fn bind(local_ip: IpAddr) -> Result<Self> {
        let socket = UdpSocket::bind(SocketAddr::new(local_ip, 0))
            .await
            .map_err(|e| Error::Media(format!("音频收流端口绑定失败: {e}")))?;
        Ok(AudioReceiver {
            socket,
            stats: Arc::new(RecvStats::default()),
        })
    }

    /// 本端实际收流端口(填进 SDP)。
    pub fn local_port(&self) -> Result<u16> {
        self.socket
            .local_addr()
            .map(|a| a.port())
            .map_err(|e| Error::Media(format!("读取音频收流端口失败: {e}")))
    }

    /// 统计句柄(可在 `run` 之前克隆出去给 UI / 保活监测)。
    pub fn stats(&self) -> Arc<RecvStats> {
        Arc::clone(&self.stats)
    }

    /// 收流循环,直到 `stop` 完成。
    ///
    /// 只统计不落盘:模拟器的价值是"证明链路通且对面在说话",
    /// 存音频文件是另一个需求,需要时在 `on_payload` 里接。
    pub async fn run<F>(self, stop: F) -> Result<()>
    where
        F: std::future::Future<Output = ()> + Send,
    {
        self.stats.mark_started();
        let mut buf = vec![0u8; MAX_DATAGRAM];
        // 序号连续性跟踪:None = 还没收到第一包。
        let mut expected_seq: Option<u16> = None;
        let stop = std::pin::pin!(stop);
        let mut stop = stop;
        loop {
            tokio::select! {
                _ = &mut stop => return Ok(()),
                received = self.socket.recv_from(&mut buf) => {
                    let (n, _peer) = match received {
                        Ok(v) => v,
                        // 收流端不因单次 IO 错误退出:平台重启时会有 ICMP 端口不可达。
                        Err(e) => {
                            tracing::debug!(%e, "音频收流 recv 出错,继续");
                            continue;
                        }
                    };
                    self.ingest(&buf[..n], &mut expected_seq);
                }
            }
        }
    }

    /// 解析一个数据报并更新统计。抽出来是为了能直接单测,不用起 socket。
    fn ingest(&self, datagram: &[u8], expected_seq: &mut Option<u16>) {
        let stats = &self.stats;
        if datagram.len() < RTP_HEADER_LEN || datagram[0] >> 6 != 2 {
            stats.malformed.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let csrc_count = (datagram[0] & 0x0F) as usize;
        let has_extension = datagram[0] & 0x10 != 0;
        let mut offset = RTP_HEADER_LEN + csrc_count * 4;
        if has_extension {
            // 扩展头:2 字节 profile + 2 字节长度(以 4 字节为单位)。
            if datagram.len() < offset + 4 {
                stats.malformed.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let words = u16::from_be_bytes([datagram[offset + 2], datagram[offset + 3]]) as usize;
            offset += 4 + words * 4;
        }
        if datagram.len() < offset {
            stats.malformed.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let payload_type = datagram[1] & 0x7F;
        let seq = u16::from_be_bytes([datagram[2], datagram[3]]);
        let payload = &datagram[offset..];

        match *expected_seq {
            None => {}
            Some(want) => {
                // 16 位序号回绕:用 wrapping 差值判断前进还是回退。
                let gap = seq.wrapping_sub(want);
                if gap == 0 {
                    // 正好是期望的下一个包。
                } else if gap < 0x8000 {
                    stats.lost.fetch_add(gap as u64, Ordering::Relaxed);
                } else {
                    stats.reordered.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        *expected_seq = Some(seq.wrapping_add(1));

        stats.packets.fetch_add(1, Ordering::Relaxed);
        stats
            .payload_bytes
            .fetch_add(payload.len() as u64, Ordering::Relaxed);
        stats
            .last_payload_type
            .store(payload_type as u32, Ordering::Relaxed);
        stats.last_recv_ms.store(now_ms().max(1), Ordering::Relaxed);
        if let Some(level) = level_percent(payload_type, payload) {
            stats.level_pct.store(level, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rtp(seq: u16, pt: u8, payload: &[u8]) -> Vec<u8> {
        let mut pkt = vec![0x80, pt, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        pkt[2..4].copy_from_slice(&seq.to_be_bytes());
        pkt.extend_from_slice(payload);
        pkt
    }

    async fn receiver() -> AudioReceiver {
        AudioReceiver::bind(IpAddr::from([127, 0, 0, 1]))
            .await
            .expect("bind")
    }

    #[test]
    fn g711a_解码关键值() {
        // A-law 0xD5 ^ 0x55 = 0x80 → seg 0、量化 0 → +8;0x55 → -8。
        assert_eq!(alaw_to_linear(0xD5), 8);
        assert_eq!(alaw_to_linear(0x55), -8);
        // 幅度随段号单调增大。
        assert!(alaw_to_linear(0xFF).abs() > alaw_to_linear(0xD5).abs());
    }

    #[test]
    fn g711u_解码关键值() {
        // μ-law 0xFF 是 0 电平,0x7F 是负零点。
        assert_eq!(ulaw_to_linear(0xFF), 0);
        assert_eq!(ulaw_to_linear(0x7F), 0);
        assert!(ulaw_to_linear(0x80).abs() > 30000);
    }

    #[test]
    fn 静音与满幅的电平区分() {
        // A-law 的"静音"码字是 0xD5(+8),电平应接近 0。
        assert_eq!(level_percent(PT_PCMA, &[0xD5; 160]), Some(0));
        // 满幅码字电平应接近 100。
        let loud = level_percent(PT_PCMA, &[0xAA; 160]).expect("pcma");
        assert!(loud > 50, "满幅电平应显著大于静音,实际 {loud}");
        // 非 G.711(PS 封装音频)不算电平。
        assert_eq!(level_percent(96, &[0x00; 10]), None);
    }

    #[tokio::test]
    async fn 收包统计_序号连续时无丢包() {
        let rx = receiver().await;
        let mut expected = None;
        for seq in 100..110u16 {
            rx.ingest(&rtp(seq, PT_PCMA, &[0xD5; 160]), &mut expected);
        }
        let s = rx.stats().snapshot();
        assert_eq!(s.packets, 10);
        assert_eq!(s.payload_bytes, 1600);
        assert_eq!(s.lost, 0);
        assert_eq!(s.payload_type, Some(PT_PCMA));
    }

    #[tokio::test]
    async fn 收包统计_序号跳变计丢包_回退计乱序() {
        let rx = receiver().await;
        let mut expected = None;
        rx.ingest(&rtp(10, PT_PCMA, &[0xD5; 8]), &mut expected);
        // 跳过 11、12 → 丢 2 包。
        rx.ingest(&rtp(13, PT_PCMA, &[0xD5; 8]), &mut expected);
        // 迟到的 11 → 乱序。
        rx.ingest(&rtp(11, PT_PCMA, &[0xD5; 8]), &mut expected);
        let s = rx.stats().snapshot();
        assert_eq!(s.lost, 2);
        assert_eq!(s.reordered, 1);
        assert_eq!(s.packets, 3);
    }

    #[tokio::test]
    async fn 序号回绕不误报丢包() {
        let rx = receiver().await;
        let mut expected = None;
        rx.ingest(&rtp(65534, PT_PCMA, &[0xD5; 8]), &mut expected);
        rx.ingest(&rtp(65535, PT_PCMA, &[0xD5; 8]), &mut expected);
        rx.ingest(&rtp(0, PT_PCMA, &[0xD5; 8]), &mut expected);
        rx.ingest(&rtp(1, PT_PCMA, &[0xD5; 8]), &mut expected);
        assert_eq!(rx.stats().snapshot().lost, 0);
    }

    #[tokio::test]
    async fn 畸形包被计数而不是panic() {
        let rx = receiver().await;
        let mut expected = None;
        rx.ingest(&[], &mut expected); // 空包
        rx.ingest(&[0x80, 8, 0], &mut expected); // 短于 RTP 头
        rx.ingest(&[0x40; 12], &mut expected); // 版本号不是 2
        let s = rx.stats().snapshot();
        assert_eq!(s.malformed, 3);
        assert_eq!(s.packets, 0);
    }

    #[tokio::test]
    async fn 带csrc与扩展头的包能正确定位负载() {
        let rx = receiver().await;
        let mut expected = None;
        // V=2, X=1, CC=1 → 头 12 + 4(CSRC) + 4+4(扩展 1 word)
        let mut pkt = vec![0xB1, PT_PCMA, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        pkt.extend_from_slice(&[1, 2, 3, 4]); // CSRC
        pkt.extend_from_slice(&[0xBE, 0xDE, 0, 1]); // 扩展头:1 word
        pkt.extend_from_slice(&[9, 9, 9, 9]); // 扩展内容
        pkt.extend_from_slice(&[0xD5; 160]); // 负载
        rx.ingest(&pkt, &mut expected);
        let s = rx.stats().snapshot();
        assert_eq!(s.packets, 1);
        assert_eq!(s.payload_bytes, 160, "负载偏移应跳过 CSRC 与扩展头");
    }

    #[tokio::test]
    async fn 未收到任何包时idle以收流开始时刻计() {
        let rx = receiver().await;
        let stats = rx.stats();
        // 尚未 mark_started:无法判断。
        assert!(stats.idle_for().is_none());
        stats.mark_started();
        assert!(stats.idle_for().is_some(), "开始收流后应能给出空闲时长");
        assert!(!stats.has_media());
    }

    #[tokio::test]
    async fn 收流循环可被停止信号终止() {
        let rx = receiver().await;
        let port = rx.local_port().expect("port");
        let stats = rx.stats();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            rx.run(async {
                let _ = stop_rx.await;
            })
            .await
        });
        // 发一包真实 RTP 进去。
        let tx = UdpSocket::bind("127.0.0.1:0").await.expect("tx bind");
        tx.send_to(
            &rtp(1, PT_PCMA, &[0xAA; 160]),
            SocketAddr::from(([127, 0, 0, 1], port)),
        )
        .await
        .expect("send");
        for _ in 0..50 {
            if stats.has_media() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(stats.has_media(), "应收到测试包");
        assert!(stats.snapshot().level_pct > 0, "满幅包电平应非零");
        let _ = stop_tx.send(());
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("收流任务应及时退出");
        assert!(result.expect("join").is_ok());
    }
}
