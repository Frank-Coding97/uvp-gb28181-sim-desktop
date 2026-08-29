//! RTCP 复合包解析与**接收报告(RR)反馈**统计。
//!
//! GB/T 28181-2022 §7.2 要求媒体流"应支持 RTP/RTCP",附录 C 的实时流传输
//! 也建议采用 RFC 3550。设备侧此前只**发** SR(把自己发了多少包告诉平台),
//! 没有**收** RR —— 于是"平台到底收到了多少、丢了多少、抖动多大"这件事
//! 在设备侧完全不可见,而这恰恰是现场排障最需要的一个数。
//!
//! 这里做两件事:
//! 1. 解析平台回过来的 RTCP 复合包(RR / SR / BYE,其余类型跳过);
//! 2. 把 RR 里的丢包率、累计丢包、抖动、RTT 汇总进 [`RtcpFeedback`],
//!    供压测指标与设备控制台展示。
//!
//! **为什么手写解析而不引 webrtc-rs 的 `rtcp` crate**:需要的只是几个
//! 大端定长字段的读取(报告块固定 24 字节),而那个 crate 会带进
//! `webrtc-util` 等一串传递依赖。这不是"重复造轮子",这个"轮子"本身
//! 就是 40 行位移运算,引依赖的成本高于收益。

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};

/// RTCP 包类型:发送报告(RFC 3550 §6.4.1)。
pub const PT_SR: u8 = 200;
/// RTCP 包类型:接收报告(RFC 3550 §6.4.2)。
pub const PT_RR: u8 = 201;
/// RTCP 包类型:源描述。
pub const PT_SDES: u8 = 202;
/// RTCP 包类型:离开(RFC 3550 §6.6)。
pub const PT_BYE: u8 = 203;

/// RTCP 报告块的固定长度(RFC 3550 §6.4.1)。
const REPORT_BLOCK_LEN: usize = 24;
/// SR 特有的"发送者信息"块长度(NTP 64 + RTP TS 32 + 包数 32 + 字节数 32)。
const SENDER_INFO_LEN: usize = 20;

/// 一个接收报告块(RFC 3550 §6.4.1 的 report block)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceptionReport {
    /// 被报告的数据源 SSRC(即本端发流用的 SSRC)。
    pub ssrc: u32,
    /// 自上次报告以来的丢包比例,分母 256。
    pub fraction_lost: u8,
    /// 累计丢包数。RFC 3550 里是**有符号** 24 位:重复包多于丢包时为负。
    pub cumulative_lost: i32,
    /// 收到的扩展最高序号(高 16 位是循环计数)。
    pub extended_highest_seq: u32,
    /// 到达间隔抖动,单位与 RTP 时间戳相同。
    pub jitter: u32,
    /// 收到的最后一个 SR 的 NTP 时间戳中间 32 位;0 表示还没收到过 SR。
    pub last_sr: u32,
    /// 从收到那个 SR 到发出本报告的延迟,单位 1/65536 秒。
    pub delay_since_last_sr: u32,
}

impl ReceptionReport {
    /// 从 24 字节报告块解析。
    fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < REPORT_BLOCK_LEN {
            return None;
        }
        // 累计丢包是有符号 24 位:先取 24 位,再按最高位做符号扩展。
        let raw_lost = u32::from_be_bytes([0, b[5], b[6], b[7]]);
        let cumulative_lost = if raw_lost & 0x0080_0000 != 0 {
            (raw_lost | 0xFF00_0000) as i32
        } else {
            raw_lost as i32
        };
        Some(ReceptionReport {
            ssrc: u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
            fraction_lost: b[4],
            cumulative_lost,
            extended_highest_seq: u32::from_be_bytes([b[8], b[9], b[10], b[11]]),
            jitter: u32::from_be_bytes([b[12], b[13], b[14], b[15]]),
            last_sr: u32::from_be_bytes([b[16], b[17], b[18], b[19]]),
            delay_since_last_sr: u32::from_be_bytes([b[20], b[21], b[22], b[23]]),
        })
    }

    /// 丢包率(0.0~1.0)。
    pub fn loss_ratio(&self) -> f64 {
        f64::from(self.fraction_lost) / 256.0
    }

    /// 抖动换算成毫秒。`clock_hz` 取该负载的 RTP 时钟(PS/H.264 为 90000,G.711 为 8000)。
    pub fn jitter_ms(&self, clock_hz: u32) -> f64 {
        if clock_hz == 0 {
            return 0.0;
        }
        f64::from(self.jitter) * 1000.0 / f64::from(clock_hz)
    }

    /// 估算往返时延(微秒)。
    ///
    /// RFC 3550 §6.4.1 的算法:RTT = 当前 NTP 中间 32 位 − DLSR − LSR,
    /// 单位 1/65536 秒。`now_ntp_mid` 由调用方在收到报告的瞬间取。
    /// 平台还没收到过我们的 SR(LSR=0)时无法估算,返回 None。
    pub fn rtt_us(&self, now_ntp_mid: u32) -> Option<u64> {
        if self.last_sr == 0 {
            return None;
        }
        let elapsed = now_ntp_mid.wrapping_sub(self.last_sr);
        // 对端处理耗时不该超过往返总耗时;超了说明时钟对不上,不如不报。
        if self.delay_since_last_sr > elapsed {
            return None;
        }
        let rtt_1_65536 = u64::from(elapsed - self.delay_since_last_sr);
        Some(rtt_1_65536 * 1_000_000 / 65_536)
    }
}

/// 解析出的一个 RTCP 包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RtcpPacket {
    /// 接收报告:平台告诉设备"你的流我收得怎么样"。
    ReceiverReport {
        /// 报告发送方(平台)的 SSRC。
        reporter: u32,
        /// 报告块列表。
        reports: Vec<ReceptionReport>,
    },
    /// 发送报告。平台在双向会话(如对讲)里也会发 SR,其中同样可以携带报告块。
    SenderReport {
        /// 报告发送方 SSRC。
        reporter: u32,
        /// 报告块列表(平台同时也在收流时才有)。
        reports: Vec<ReceptionReport>,
    },
    /// 平台离开:可作为"对端主动结束收流"的信号。
    Bye {
        /// 离开的 SSRC 列表。
        sources: Vec<u32>,
    },
    /// 其它类型(SDES/APP/RTPFB 等),只记类型不解析。
    Other {
        /// RTCP 负载类型。
        payload_type: u8,
    },
}

/// 解析一个 RTCP **复合包**(RFC 3550 §6.1:多个 RTCP 包首尾相接)。
///
/// 遇到结构不合法(版本号不对、长度字段越界)就在该处停止,把已解析出的部分返回 ——
/// 半个包也比整个丢掉有用,而且现场确实存在实现把长度写错的平台。
pub fn parse_compound(buf: &[u8]) -> Vec<RtcpPacket> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + 4 <= buf.len() {
        let head = &buf[offset..];
        if head[0] >> 6 != 2 {
            break; // 版本号必须是 2
        }
        let count = usize::from(head[0] & 0x1F);
        let payload_type = head[1];
        let words = usize::from(u16::from_be_bytes([head[2], head[3]]));
        let total = (words + 1) * 4;
        if total > head.len() {
            break; // 长度字段越界
        }
        let body = &head[..total];
        out.push(match payload_type {
            PT_RR => RtcpPacket::ReceiverReport {
                reporter: read_ssrc(body),
                reports: parse_blocks(body, 8, count),
            },
            PT_SR => RtcpPacket::SenderReport {
                reporter: read_ssrc(body),
                reports: parse_blocks(body, 8 + SENDER_INFO_LEN, count),
            },
            PT_BYE => RtcpPacket::Bye {
                sources: (0..count)
                    .filter_map(|i| {
                        let at = 4 + i * 4;
                        body.get(at..at + 4)
                            .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
                    })
                    .collect(),
            },
            other => RtcpPacket::Other {
                payload_type: other,
            },
        });
        offset += total;
    }
    out
}

fn read_ssrc(body: &[u8]) -> u32 {
    body.get(4..8)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .unwrap_or(0)
}

fn parse_blocks(body: &[u8], start: usize, count: usize) -> Vec<ReceptionReport> {
    (0..count)
        .filter_map(|i| {
            let at = start + i * REPORT_BLOCK_LEN;
            body.get(at..at + REPORT_BLOCK_LEN)
                .and_then(ReceptionReport::parse)
        })
        .collect()
}

/// 判断一段数据看起来是否是 RTCP(而不是 RTP)。
///
/// TCP(RFC 4571)模式下 RTP 与 RTCP 共用一条连接,靠负载类型区分:
/// RTCP 的 PT 落在 200~213,而 RTP 的 PT 最高 127 —— 第 2 字节的最高位
/// (RTP 的 marker 位)置 1 时 RTP 的 PT 字节也能到 255,所以这里按
/// RFC 5761 §4 的建议只认 200~213 这一段。
pub fn looks_like_rtcp(buf: &[u8]) -> bool {
    buf.len() >= 4 && buf[0] >> 6 == 2 && (200..=213).contains(&buf[1])
}

/// 平台侧收流质量的反馈汇总(来自 RTCP RR)。
///
/// 与 `SendStats` 分开:那边是"我发了多少",这边是"对端收到了多少"。
#[derive(Debug, Default)]
pub struct RtcpFeedback {
    /// 收到的接收报告块总数。
    pub reports: AtomicU64,
    /// 最近一次报告的丢包比例(分母 256)。
    pub fraction_lost: AtomicU32,
    /// 最近一次报告的累计丢包数(可为负:重复包多于丢包)。
    pub cumulative_lost: AtomicI32,
    /// 最近一次报告的抖动(RTP 时间戳单位)。
    pub jitter: AtomicU32,
    /// 峰值抖动,用于回看"是否抖过"而不只是"现在抖不抖"。
    pub jitter_peak: AtomicU32,
    /// 最近一次估算的往返时延(微秒);0 表示尚未估出。
    pub rtt_us: AtomicU64,
    /// 报告方(平台)的 SSRC。
    pub reporter_ssrc: AtomicU32,
    /// 最近一次收到报告距进程启动的毫秒数;0 表示从未收到。
    last_report_at: AtomicU64,
    /// 平台是否发过 RTCP BYE。
    pub peer_bye: AtomicBool,
    /// 收到但未能解析出报告块的 RTCP 包数(诊断用)。
    pub unparsed: AtomicU64,
}

impl RtcpFeedback {
    /// 处理一个收到的 RTCP 数据报(可能是复合包)。返回是否更新了统计。
    pub fn ingest(&self, datagram: &[u8], now_ntp_mid: u32, since_start_ms: u64) -> bool {
        let packets = parse_compound(datagram);
        if packets.is_empty() {
            self.unparsed.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let mut updated = false;
        for packet in packets {
            let (reporter, reports) = match packet {
                RtcpPacket::ReceiverReport { reporter, reports }
                | RtcpPacket::SenderReport { reporter, reports } => (reporter, reports),
                RtcpPacket::Bye { .. } => {
                    self.peer_bye.store(true, Ordering::Relaxed);
                    updated = true;
                    continue;
                }
                RtcpPacket::Other { .. } => continue,
            };
            if reports.is_empty() {
                continue;
            }
            self.reporter_ssrc.store(reporter, Ordering::Relaxed);
            for report in reports {
                self.reports.fetch_add(1, Ordering::Relaxed);
                self.fraction_lost
                    .store(u32::from(report.fraction_lost), Ordering::Relaxed);
                self.cumulative_lost
                    .store(report.cumulative_lost, Ordering::Relaxed);
                self.jitter.store(report.jitter, Ordering::Relaxed);
                self.jitter_peak.fetch_max(report.jitter, Ordering::Relaxed);
                if let Some(rtt) = report.rtt_us(now_ntp_mid) {
                    self.rtt_us.store(rtt, Ordering::Relaxed);
                }
                updated = true;
            }
            self.last_report_at.store(since_start_ms, Ordering::Relaxed);
        }
        updated
    }

    /// 是否收到过任何接收报告。没收到的原因通常是平台没开 RTCP 端口,
    /// 或者中间设备把 RTP 端口 +1 的那条流拦了。
    pub fn has_feedback(&self) -> bool {
        self.reports.load(Ordering::Relaxed) > 0
    }

    /// 生成一份可序列化快照。`clock_hz` 用于把抖动换算成毫秒;
    /// `now_since_start_ms` 用于算"距上次报告多久"。
    pub fn snapshot(&self, clock_hz: u32, now_since_start_ms: u64) -> RtcpFeedbackSnapshot {
        let last = self.last_report_at.load(Ordering::Relaxed);
        let jitter = self.jitter.load(Ordering::Relaxed);
        let jitter_peak = self.jitter_peak.load(Ordering::Relaxed);
        let to_ms = |t: u32| {
            if clock_hz == 0 {
                0.0
            } else {
                f64::from(t) * 1000.0 / f64::from(clock_hz)
            }
        };
        RtcpFeedbackSnapshot {
            reports: self.reports.load(Ordering::Relaxed),
            loss_percent: f64::from(self.fraction_lost.load(Ordering::Relaxed)) * 100.0 / 256.0,
            cumulative_lost: self.cumulative_lost.load(Ordering::Relaxed),
            jitter_ms: to_ms(jitter),
            jitter_peak_ms: to_ms(jitter_peak),
            rtt_ms: match self.rtt_us.load(Ordering::Relaxed) {
                0 => None,
                us => Some(us as f64 / 1000.0),
            },
            reporter_ssrc: self.reporter_ssrc.load(Ordering::Relaxed),
            age_ms: if last == 0 {
                None
            } else {
                Some(now_since_start_ms.saturating_sub(last))
            },
            peer_bye: self.peer_bye.load(Ordering::Relaxed),
            unparsed: self.unparsed.load(Ordering::Relaxed),
        }
    }
}

/// [`RtcpFeedback`] 的可序列化快照(给 Tauri 命令与压测报表)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RtcpFeedbackSnapshot {
    /// 累计收到的接收报告块数。
    pub reports: u64,
    /// 最近一次报告的丢包率,百分比。
    pub loss_percent: f64,
    /// 累计丢包数(可为负)。
    pub cumulative_lost: i32,
    /// 最近一次报告的抖动,毫秒。
    pub jitter_ms: f64,
    /// 峰值抖动,毫秒。
    pub jitter_peak_ms: f64,
    /// 往返时延估算,毫秒;平台没回过 SR 时为 None。
    pub rtt_ms: Option<f64>,
    /// 报告方(平台)SSRC。
    pub reporter_ssrc: u32,
    /// 距最近一次报告的毫秒数;从未收到为 None。
    pub age_ms: Option<u64>,
    /// 平台是否发过 RTCP BYE。
    pub peer_bye: bool,
    /// 无法解析的 RTCP 包数。
    pub unparsed: u64,
}

impl RtcpFeedbackSnapshot {
    /// 是否收到过反馈。
    pub fn has_feedback(&self) -> bool {
        self.reports > 0
    }
}

/// 构造一个接收报告(RR)包 —— 仅用于测试与本端作为接收方(语音广播下行)时回报。
///
/// 语音广播/对讲时设备是**接收方**,RFC 3550 要求接收方周期发 RR;
/// 平台据此判断下行链路质量。
pub fn build_rtcp_rr(reporter_ssrc: u32, report: &ReceptionReport) -> [u8; 32] {
    let mut b = [0u8; 32];
    b[0] = 0x80 | 1; // V=2, P=0, RC=1
    b[1] = PT_RR;
    b[2..4].copy_from_slice(&7u16.to_be_bytes()); // 32/4 - 1
    b[4..8].copy_from_slice(&reporter_ssrc.to_be_bytes());
    b[8..12].copy_from_slice(&report.ssrc.to_be_bytes());
    b[12] = report.fraction_lost;
    let lost = report.cumulative_lost.clamp(-0x80_0000, 0x7F_FFFF) as u32 & 0x00FF_FFFF;
    b[13..16].copy_from_slice(&lost.to_be_bytes()[1..4]);
    b[16..20].copy_from_slice(&report.extended_highest_seq.to_be_bytes());
    b[20..24].copy_from_slice(&report.jitter.to_be_bytes());
    b[24..28].copy_from_slice(&report.last_sr.to_be_bytes());
    b[28..32].copy_from_slice(&report.delay_since_last_sr.to_be_bytes());
    b
}

/// 当前 NTP 时间戳的中间 32 位(高 16 位取秒的低 16 位,低 16 位取小数高 16 位)。
///
/// RR 的 LSR 字段用的就是这个格式,RTT 估算必须用同一刻度。
pub fn ntp_mid32() -> u32 {
    const NTP_EPOCH_OFFSET: u64 = 2_208_988_800;
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => {
            let secs = d.as_secs() + NTP_EPOCH_OFFSET;
            let frac = ((u64::from(d.subsec_nanos())) << 32) / 1_000_000_000;
            (((secs & 0xFFFF) << 16) | ((frac >> 16) & 0xFFFF)) as u32
        }
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report() -> ReceptionReport {
        ReceptionReport {
            ssrc: 0x0BEB_C201,
            fraction_lost: 26, // 26/256 ≈ 10.2%
            cumulative_lost: 1234,
            extended_highest_seq: 0x0001_2345,
            jitter: 900, // 90kHz 时钟下 = 10ms
            last_sr: 0xAABB_CCDD,
            delay_since_last_sr: 6553, // ≈ 0.1s
        }
    }

    #[test]
    fn rr_往返解析一致() {
        let original = sample_report();
        let pkt = build_rtcp_rr(0x1234_5678, &original);
        let parsed = parse_compound(&pkt);
        assert_eq!(parsed.len(), 1);
        match &parsed[0] {
            RtcpPacket::ReceiverReport { reporter, reports } => {
                assert_eq!(*reporter, 0x1234_5678);
                assert_eq!(reports.len(), 1);
                assert_eq!(reports[0], original);
            }
            other => panic!("应解析为 RR,实际 {other:?}"),
        }
    }

    #[test]
    fn 累计丢包负值按有符号24位解析() {
        // 重复包多于丢包时 RFC 3550 允许负值;按无符号解析会得到 1600 万这种荒谬数字。
        let mut report = sample_report();
        report.cumulative_lost = -3;
        let pkt = build_rtcp_rr(1, &report);
        assert_eq!(&pkt[13..16], &[0xFF, 0xFF, 0xFD], "应是 24 位补码");
        let parsed = parse_compound(&pkt);
        match &parsed[0] {
            RtcpPacket::ReceiverReport { reports, .. } => {
                assert_eq!(reports[0].cumulative_lost, -3);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn 丢包率与抖动换算() {
        let report = sample_report();
        assert!((report.loss_ratio() - 26.0 / 256.0).abs() < 1e-9);
        assert!((report.jitter_ms(90_000) - 10.0).abs() < 1e-9);
        assert!((report.jitter_ms(8_000) - 112.5).abs() < 1e-9);
        assert_eq!(report.jitter_ms(0), 0.0, "时钟为 0 时不该除零");
    }

    #[test]
    fn rtt估算扣掉对端处理时延() {
        let report = ReceptionReport {
            last_sr: 1_000_000,
            delay_since_last_sr: 6553, // 0.1s
            ..sample_report()
        };
        // 现在比 LSR 晚了 0.3s(65536*0.3 ≈ 19661),扣掉对端 0.1s → RTT ≈ 0.2s。
        let rtt = report.rtt_us(1_000_000 + 19_661).expect("应能估出 RTT");
        assert!(
            (190_000..=210_000).contains(&rtt),
            "RTT 应约 200ms,实际 {rtt}us"
        );
        // 平台还没收到过我们的 SR。
        let no_sr = ReceptionReport {
            last_sr: 0,
            ..report
        };
        assert!(no_sr.rtt_us(12345).is_none());
        // 对端处理时延大于总耗时 → 时钟对不上,不猜。
        let bogus = ReceptionReport {
            delay_since_last_sr: 999_999,
            ..report
        };
        assert!(bogus.rtt_us(1_000_000 + 19_661).is_none());
    }

    #[test]
    fn 复合包按长度字段逐个解析() {
        let mut buf = Vec::new();
        // SR(28 字节,RC=0)+ RR(32 字节,RC=1)+ BYE(8 字节,SC=1)。
        buf.extend_from_slice(&crate::rtp::build_rtcp_sr(7, 1, 2, 3, 4, 5));
        buf.extend_from_slice(&build_rtcp_rr(9, &sample_report()));
        let mut bye = [0u8; 8];
        bye[0] = 0x80 | 1;
        bye[1] = PT_BYE;
        bye[2..4].copy_from_slice(&1u16.to_be_bytes());
        bye[4..8].copy_from_slice(&0xDEAD_BEEFu32.to_be_bytes());
        buf.extend_from_slice(&bye);

        let parsed = parse_compound(&buf);
        assert_eq!(parsed.len(), 3);
        assert!(matches!(
            parsed[0],
            RtcpPacket::SenderReport { reporter: 7, .. }
        ));
        assert!(matches!(
            parsed[1],
            RtcpPacket::ReceiverReport { reporter: 9, .. }
        ));
        assert_eq!(
            parsed[2],
            RtcpPacket::Bye {
                sources: vec![0xDEAD_BEEF]
            }
        );
    }

    #[test]
    fn 畸形包不panic且尽力返回() {
        assert!(parse_compound(&[]).is_empty());
        assert!(parse_compound(&[0x80, 201]).is_empty(), "不足 4 字节");
        assert!(
            parse_compound(&[0x00, 201, 0, 1, 0, 0, 0, 0]).is_empty(),
            "版本号不是 2"
        );
        // 长度字段声明的比实际数据长 → 不越界读,直接停。
        assert!(parse_compound(&[0x80, 201, 0xFF, 0xFF, 1, 2, 3, 4]).is_empty());
        // RC 声明 3 个块但只给了 1 个 → 只返回能读出的那个。
        let mut pkt = build_rtcp_rr(1, &sample_report()).to_vec();
        pkt[0] = 0x80 | 3;
        match &parse_compound(&pkt)[0] {
            RtcpPacket::ReceiverReport { reports, .. } => assert_eq!(reports.len(), 1),
            other => panic!("{other:?}"),
        }
        // 前一个包正常、后一个畸形:前一个仍应解析出来。
        let mut mixed = build_rtcp_rr(5, &sample_report()).to_vec();
        mixed.extend_from_slice(&[0x80, 201, 0xFF, 0xFF]);
        assert_eq!(parse_compound(&mixed).len(), 1);
    }

    #[test]
    fn 未知类型只记类型不解析() {
        // PT=206(RTPFB/PSFB 之类),长度 1 个字 → 8 字节。
        let pkt = [0x80, 206, 0, 1, 0, 0, 0, 1];
        assert_eq!(
            parse_compound(&pkt),
            vec![RtcpPacket::Other { payload_type: 206 }]
        );
    }

    #[test]
    fn tcp下按负载类型区分rtcp与rtp() {
        assert!(looks_like_rtcp(&build_rtcp_rr(1, &sample_report())));
        assert!(looks_like_rtcp(&crate::rtp::build_rtcp_sr(
            1, 2, 3, 4, 5, 6
        )));
        // PS 流的 RTP 包:PT=96,带 marker 时第 2 字节是 0xE0 = 224,不能误判成 RTCP。
        let rtp = crate::rtp::build_rtp_packet(96, true, 1, 2, 3, &[0u8; 10]);
        assert!(!looks_like_rtcp(&rtp));
        assert!(!looks_like_rtcp(&[0x80, 8, 0, 1]), "G.711 的 PT=8");
        assert!(!looks_like_rtcp(&[0x80]));
    }

    #[test]
    fn 反馈汇总取最新值并留峰值() {
        let fb = RtcpFeedback::default();
        assert!(!fb.has_feedback());
        assert!(fb.snapshot(90_000, 0).age_ms.is_none());

        let now = ntp_mid32();
        let first = ReceptionReport {
            jitter: 1800, // 20ms
            fraction_lost: 51,
            last_sr: 0,
            ..sample_report()
        };
        assert!(fb.ingest(&build_rtcp_rr(42, &first), now, 1_000));
        let snap = fb.snapshot(90_000, 1_500);
        assert_eq!(snap.reports, 1);
        assert!((snap.loss_percent - 51.0 * 100.0 / 256.0).abs() < 1e-9);
        assert!((snap.jitter_ms - 20.0).abs() < 1e-9);
        assert_eq!(snap.reporter_ssrc, 42);
        assert_eq!(snap.age_ms, Some(500));
        assert!(snap.rtt_ms.is_none(), "LSR=0 时不该给出 RTT");

        // 第二份报告抖动更小:当前值跟着变,峰值保留。
        let second = ReceptionReport {
            jitter: 450, // 5ms
            fraction_lost: 0,
            ..first
        };
        fb.ingest(&build_rtcp_rr(42, &second), now, 2_000);
        let snap = fb.snapshot(90_000, 2_000);
        assert_eq!(snap.reports, 2);
        assert_eq!(snap.loss_percent, 0.0);
        assert!((snap.jitter_ms - 5.0).abs() < 1e-9);
        assert!((snap.jitter_peak_ms - 20.0).abs() < 1e-9);
        assert_eq!(snap.age_ms, Some(0));
    }

    #[test]
    fn bye与畸形包分别计入() {
        let fb = RtcpFeedback::default();
        let mut bye = [0u8; 8];
        bye[0] = 0x80 | 1;
        bye[1] = PT_BYE;
        bye[2..4].copy_from_slice(&1u16.to_be_bytes());
        assert!(fb.ingest(&bye, 0, 0));
        assert!(fb.snapshot(90_000, 0).peer_bye);
        assert!(!fb.has_feedback(), "BYE 不是接收报告");

        assert!(!fb.ingest(&[1, 2, 3], 0, 0));
        assert_eq!(fb.snapshot(90_000, 0).unparsed, 1);
    }
}
