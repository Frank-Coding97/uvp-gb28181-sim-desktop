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
//!
//! RFC 3550 要求**接收方周期回报 RTCP RR**,平台据此判断下行链路质量。
//! 本模块在收流端口 +1 上绑 RTCP socket,周期发 RR(丢包率/累计丢包/抖动/DLSR),
//! 同时接收平台的 SR 以填出 LSR —— 平台侧才能算出到设备的往返时延。

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};
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
/// 接收报告(RR)发送周期。RFC 3550 §6.2 建议最小间隔 5 秒。
pub const RR_INTERVAL: Duration = Duration::from_secs(5);
/// G.711 的 RTP 时钟频率(8 kHz),用于抖动计算与换算。
pub const G711_CLOCK_HZ: u32 = 8_000;

/// 收流统计。全部用原子量,收流任务写、UI/指标读,不需要锁。
#[derive(Debug)]
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
    /// 发送方 SSRC(取自最近一包 RTP 头),RR 的报告块要回报它。
    ssrc: AtomicU32,
    /// 首包序号(扩展前),用于算"应收包数"。0xFFFF_FFFF = 尚未收到。
    base_seq: AtomicU32,
    /// 序号回绕次数(扩展序号的高 16 位)。
    cycles: AtomicU32,
    /// 收到过的最大序号(扩展前)。
    max_seq: AtomicU32,
    /// 到达间隔抖动 ×16(RFC 3550 A.8 的整数迭代式,除 16 才是抖动本身)。
    jitter_x16: AtomicU32,
    /// 上一包的传输时延(到达时刻 − RTP 时间戳),抖动迭代用。i64::MIN = 尚无。
    last_transit: AtomicI64,
    /// 上一次 RR 时的"应收包数",用于算区间丢包率。
    expected_prior: AtomicU64,
    /// 上一次 RR 时的"实收包数"。
    received_prior: AtomicU64,
    /// 平台最近一个 SR 的 NTP 中间 32 位;0 = 没收到过。
    last_sr_mid: AtomicU32,
    /// 收到那个 SR 的本地时刻(距进程启动毫秒)。
    last_sr_at_ms: AtomicU64,
    /// 已发出的 RR 包数。
    rr_sent: AtomicU64,
}

impl Default for RecvStats {
    fn default() -> Self {
        Self {
            packets: AtomicU64::new(0),
            payload_bytes: AtomicU64::new(0),
            lost: AtomicU64::new(0),
            reordered: AtomicU64::new(0),
            malformed: AtomicU64::new(0),
            last_payload_type: AtomicU32::new(0xFFFF),
            level_pct: AtomicU32::new(0),
            last_recv_ms: AtomicU64::new(0),
            started_ms: AtomicU64::new(0),
            ssrc: AtomicU32::new(0),
            // 哨兵值:0 是合法首包序号,不能拿来表示"还没收到"。
            base_seq: AtomicU32::new(u32::MAX),
            cycles: AtomicU32::new(0),
            max_seq: AtomicU32::new(0),
            jitter_x16: AtomicU32::new(0),
            last_transit: AtomicI64::new(i64::MIN),
            expected_prior: AtomicU64::new(0),
            received_prior: AtomicU64::new(0),
            last_sr_mid: AtomicU32::new(0),
            last_sr_at_ms: AtomicU64::new(0),
            rr_sent: AtomicU64::new(0),
        }
    }
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
    /// 到达间隔抖动,毫秒(按 G.711 8 kHz 时钟换算)。
    pub jitter_ms: f64,
    /// 已向平台发出的 RTCP RR 包数。
    pub rr_sent: u64,
    /// 是否收到过平台的 RTCP SR(收到才说明平台的 RTCP 也通)。
    pub peer_sr_seen: bool,
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
            jitter_ms: self.jitter() as f64 * 1000.0 / f64::from(G711_CLOCK_HZ),
            rr_sent: self.rr_sent.load(Ordering::Relaxed),
            peer_sr_seen: self.last_sr_mid.load(Ordering::Relaxed) != 0,
        }
    }

    /// 当前到达间隔抖动(RTP 时间戳单位)。
    pub fn jitter(&self) -> u32 {
        self.jitter_x16.load(Ordering::Relaxed) >> 4
    }

    /// 按 RFC 3550 A.8 迭代更新抖动。
    ///
    /// `rtp_ts` 是包头时间戳,`arrival` 是到达时刻(换算到同一 RTP 时钟)。
    /// 两者之差是"传输时延",相邻包时延之差的平滑绝对值就是抖动。
    fn update_jitter(&self, rtp_ts: u32, arrival: u32) {
        let transit = i64::from(arrival.wrapping_sub(rtp_ts) as i32);
        let prev = self.last_transit.swap(transit, Ordering::Relaxed);
        if prev == i64::MIN {
            return; // 第一包没有"上一包"可比
        }
        let d = (transit - prev).abs();
        // RFC 3550 A.8 的整数迭代:jitter_x16 += |D| − (jitter_x16 + 8) / 16,
        // 存的是抖动 ×16(读取时右移 4 位),这样不用浮点也能保留小数位。
        let j = i64::from(self.jitter_x16.load(Ordering::Relaxed));
        let next = (j + d - ((j + 8) >> 4)).clamp(0, i64::from(u32::MAX));
        self.jitter_x16.store(next as u32, Ordering::Relaxed);
    }

    /// 记录平台 SR 的 NTP 中间 32 位与本地收到时刻(RR 的 LSR/DLSR 用)。
    fn note_sender_report(&self, ntp_mid: u32) {
        self.last_sr_mid.store(ntp_mid, Ordering::Relaxed);
        self.last_sr_at_ms.store(now_ms().max(1), Ordering::Relaxed);
    }

    /// 生成一个接收报告块(RFC 3550 §6.4.1)。尚未收到任何包时返回 None ——
    /// 一个包都没收到时报告"应收 0 实收 0"对平台没有信息量。
    fn reception_report(&self) -> Option<crate::rtcp::ReceptionReport> {
        let base = self.base_seq.load(Ordering::Relaxed);
        if base == u32::MAX {
            return None;
        }
        let extended_max =
            self.cycles.load(Ordering::Relaxed) * 65_536 + self.max_seq.load(Ordering::Relaxed);
        let expected = u64::from(extended_max).saturating_sub(u64::from(base)) + 1;
        let received = self.packets.load(Ordering::Relaxed);
        let cumulative_lost = expected.saturating_sub(received).min(0x7F_FFFF) as i32;

        // 区间丢包率:本次与上次报告之间的应收/实收之差。
        let expected_prior = self.expected_prior.swap(expected, Ordering::Relaxed);
        let received_prior = self.received_prior.swap(received, Ordering::Relaxed);
        let expected_interval = expected.saturating_sub(expected_prior);
        let received_interval = received.saturating_sub(received_prior);
        let fraction_lost = if expected_interval == 0 || received_interval >= expected_interval {
            0
        } else {
            (((expected_interval - received_interval) << 8) / expected_interval).min(255) as u8
        };

        // DLSR:从收到平台 SR 到现在,单位 1/65536 秒。
        let last_sr = self.last_sr_mid.load(Ordering::Relaxed);
        let delay_since_last_sr = if last_sr == 0 {
            0
        } else {
            let since = now_ms().saturating_sub(self.last_sr_at_ms.load(Ordering::Relaxed));
            (since.saturating_mul(65_536) / 1_000).min(u64::from(u32::MAX)) as u32
        };

        Some(crate::rtcp::ReceptionReport {
            ssrc: self.ssrc.load(Ordering::Relaxed),
            fraction_lost,
            cumulative_lost,
            extended_highest_seq: extended_max,
            jitter: self.jitter(),
            last_sr,
            delay_since_last_sr,
        })
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
    /// 本端 RTCP socket(收流端口 +1):发 RR、收平台 SR。
    ///
    /// 抢不到 +1 端口时为 None —— 那就只收流不回报,**媒体链路不能因为
    /// 反馈通道建不起来就建不起来**。
    rtcp: Option<UdpSocket>,
    /// 本端作为报告方的 SSRC(RR 的 sender SSRC)。
    reporter_ssrc: u32,
    /// RR 回报周期。
    rr_interval: Duration,
    stats: Arc<RecvStats>,
}

impl AudioReceiver {
    /// 在指定本地 IP 上绑一个系统分配的端口。
    ///
    /// 端口必须**先绑再发 INVITE**:SDP 的 `m=audio <port>` 要填真实端口,
    /// 填 0 等于告诉平台"往 0 端口发",链路必然建不起来。
    ///
    /// 同时尽力在 `端口+1` 上绑 RTCP(RFC 3550 §11 的惯例),用于回报 RR。
    pub async fn bind(local_ip: IpAddr) -> Result<Self> {
        let socket = UdpSocket::bind(SocketAddr::new(local_ip, 0))
            .await
            .map_err(|e| Error::Media(format!("音频收流端口绑定失败: {e}")))?;
        let rtcp = match socket.local_addr() {
            Ok(addr) => match addr.port().checked_add(1) {
                Some(next) => UdpSocket::bind(SocketAddr::new(local_ip, next)).await.ok(),
                None => None,
            },
            Err(_) => None,
        };
        if rtcp.is_none() {
            tracing::debug!("音频收流:未能绑定端口 +1,本次不回报 RTCP RR");
        }
        Ok(AudioReceiver {
            socket,
            rtcp,
            reporter_ssrc: rand::random(),
            rr_interval: RR_INTERVAL,
            stats: Arc::new(RecvStats::default()),
        })
    }

    /// 调整 RR 回报周期。默认 [`RR_INTERVAL`](5 秒,RFC 3550 §6.2 的建议下限);
    /// 测试里需要更快看到回报时用。
    pub fn with_rr_interval(mut self, interval: Duration) -> Self {
        self.rr_interval = interval.max(Duration::from_millis(10));
        self
    }

    /// 本端实际收流端口(填进 SDP)。
    pub fn local_port(&self) -> Result<u16> {
        self.socket
            .local_addr()
            .map(|a| a.port())
            .map_err(|e| Error::Media(format!("读取音频收流端口失败: {e}")))
    }

    /// 本端 RTCP 端口;未能绑定 `收流端口+1` 时为 None。
    pub fn local_rtcp_port(&self) -> Option<u16> {
        self.rtcp
            .as_ref()
            .and_then(|s| s.local_addr().ok())
            .map(|a| a.port())
    }

    /// 统计句柄(可在 `run` 之前克隆出去给 UI / 保活监测)。
    pub fn stats(&self) -> Arc<RecvStats> {
        Arc::clone(&self.stats)
    }

    /// 收流循环,直到 `stop` 完成。
    ///
    /// 只统计不落盘:模拟器的价值是"证明链路通且对面在说话",
    /// 存音频文件是另一个需求,需要时在 `on_payload` 里接。
    ///
    /// 同时按 [`RR_INTERVAL`] 周期向平台回报 RTCP RR。平台地址从**第一包 RTP
    /// 的源地址**学到(而不是取 SDP 里声明的地址):经过 NAT 时 SDP 里那个
    /// 地址往往是不可达的内网地址。
    pub async fn run<F>(self, stop: F) -> Result<()>
    where
        F: std::future::Future<Output = ()> + Send,
    {
        self.stats.mark_started();
        let mut buf = vec![0u8; MAX_DATAGRAM];
        let mut rtcp_buf = [0u8; 1500];
        // 序号连续性跟踪:None = 还没收到第一包。
        let mut expected_seq: Option<u16> = None;
        // 平台 RTCP 地址:从首包源地址推出(源端口 +1)。
        let mut peer_rtcp: Option<SocketAddr> = None;
        let mut rr_ticker = tokio::time::interval(self.rr_interval);
        // 第一拍立刻触发无害(还没收到包、也不知道平台地址,会静默跳过),
        // 之后每拍一条 RR。
        rr_ticker.reset();
        let stop = std::pin::pin!(stop);
        let mut stop = stop;
        loop {
            tokio::select! {
                _ = &mut stop => return Ok(()),
                received = self.socket.recv_from(&mut buf) => {
                    let (n, peer) = match received {
                        Ok(v) => v,
                        // 收流端不因单次 IO 错误退出:平台重启时会有 ICMP 端口不可达。
                        Err(e) => {
                            tracing::debug!(%e, "音频收流 recv 出错,继续");
                            continue;
                        }
                    };
                    if peer_rtcp.is_none() {
                        peer_rtcp = peer
                            .port()
                            .checked_add(1)
                            .map(|p| SocketAddr::new(peer.ip(), p));
                    }
                    self.ingest(&buf[..n], &mut expected_seq);
                }
                // 平台的 SR:记下 NTP 中间 32 位,RR 里带回去平台才能算 RTT。
                received = Self::recv_rtcp(self.rtcp.as_ref(), &mut rtcp_buf) => {
                    if let Some(n) = received {
                        self.ingest_rtcp(&rtcp_buf[..n]);
                    }
                }
                _ = rr_ticker.tick() => {
                    self.send_receiver_report(peer_rtcp).await;
                }
            }
        }
    }

    /// 从 RTCP socket 读一个包;没有 RTCP socket 时永挂(select 分支等价于禁用)。
    async fn recv_rtcp(socket: Option<&UdpSocket>, buf: &mut [u8]) -> Option<usize> {
        match socket {
            Some(sock) => sock.recv_from(buf).await.ok().map(|(n, _)| n),
            None => std::future::pending().await,
        }
    }

    /// 解析平台发来的 RTCP:只关心 SR 的 NTP 时间戳(填 RR 的 LSR)。
    fn ingest_rtcp(&self, datagram: &[u8]) {
        for packet in crate::rtcp::parse_compound(datagram) {
            if let crate::rtcp::RtcpPacket::SenderReport { .. } = packet {
                // SR 的 NTP 中间 32 位在固定偏移:头 8 字节后是 NTP 秒(4)+ 小数(4)。
                if datagram.len() >= 16 {
                    let secs =
                        u32::from_be_bytes([datagram[8], datagram[9], datagram[10], datagram[11]]);
                    let frac = u32::from_be_bytes([
                        datagram[12],
                        datagram[13],
                        datagram[14],
                        datagram[15],
                    ]);
                    let mid = (secs << 16) | (frac >> 16);
                    self.stats.note_sender_report(mid);
                }
            }
        }
    }

    /// 向平台发一条 RTCP RR。没有反馈通道或还没收到任何包时静默跳过。
    async fn send_receiver_report(&self, peer_rtcp: Option<SocketAddr>) {
        let (Some(sock), Some(dst)) = (self.rtcp.as_ref(), peer_rtcp) else {
            return;
        };
        let Some(report) = self.stats.reception_report() else {
            return;
        };
        let rr = crate::rtcp::build_rtcp_rr(self.reporter_ssrc, &report);
        match sock.send_to(&rr, dst).await {
            Ok(_) => {
                self.stats.rr_sent.fetch_add(1, Ordering::Relaxed);
                tracing::trace!(
                    %dst,
                    fraction_lost = report.fraction_lost,
                    jitter = report.jitter,
                    "已回报 RTCP RR"
                );
            }
            Err(error) => tracing::debug!(%error, %dst, "RTCP RR 发送失败(忽略)"),
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
        let rtp_ts = u32::from_be_bytes([datagram[4], datagram[5], datagram[6], datagram[7]]);
        let ssrc = u32::from_be_bytes([datagram[8], datagram[9], datagram[10], datagram[11]]);
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

        // RR 需要的序号跟踪:首包序号(应收包数的基准)+ 回绕次数 + 最大序号。
        if stats.base_seq.load(Ordering::Relaxed) == u32::MAX {
            stats.base_seq.store(u32::from(seq), Ordering::Relaxed);
            stats.max_seq.store(u32::from(seq), Ordering::Relaxed);
        } else {
            let prev_max = stats.max_seq.load(Ordering::Relaxed) as u16;
            if seq.wrapping_sub(prev_max) < 0x8000 {
                // 前进:序号绕回 0 时进位。
                if seq < prev_max {
                    stats.cycles.fetch_add(1, Ordering::Relaxed);
                }
                stats.max_seq.store(u32::from(seq), Ordering::Relaxed);
            }
        }
        stats.ssrc.store(ssrc, Ordering::Relaxed);
        // 到达时刻换算到 RTP 时钟(G.711 为 8 kHz)后参与抖动迭代。
        let arrival = (now_ms().wrapping_mul(u64::from(G711_CLOCK_HZ)) / 1_000) as u32;
        stats.update_jitter(rtp_ts, arrival);

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

    /// 带时间戳与 SSRC 的 RTP 包(RR 的抖动/SSRC 字段需要)。
    fn rtp_full(seq: u16, ts: u32, ssrc: u32, payload: &[u8]) -> Vec<u8> {
        let mut pkt = vec![0x80, PT_PCMA, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        pkt[2..4].copy_from_slice(&seq.to_be_bytes());
        pkt[4..8].copy_from_slice(&ts.to_be_bytes());
        pkt[8..12].copy_from_slice(&ssrc.to_be_bytes());
        pkt.extend_from_slice(payload);
        pkt
    }

    #[tokio::test]
    async fn rr_未收到任何包时不生成报告() {
        let rx = receiver().await;
        assert!(
            rx.stats.reception_report().is_none(),
            "一个包都没收到时报告 0/0 对平台没有信息量"
        );
    }

    #[tokio::test]
    async fn rr_报告块字段按rfc3550计算() {
        let rx = receiver().await;
        let mut expected = None;
        // 收到序号 100、101、102 之后跳到 105:应收 6 实收 4,丢 2。
        for seq in [100u16, 101, 102, 105] {
            rx.ingest(
                &rtp_full(seq, u32::from(seq) * 160, 0xAABB, &[0xD5; 160]),
                &mut expected,
            );
        }
        let report = rx.stats.reception_report().expect("应能生成报告");
        assert_eq!(report.ssrc, 0xAABB, "报告块里是发送方的 SSRC");
        assert_eq!(report.extended_highest_seq, 105);
        assert_eq!(report.cumulative_lost, 2, "应收 6(100..=105)实收 4");
        // 首次报告的区间就是全部:2/6 ≈ 0.333 → 0.333*256 ≈ 85。
        assert_eq!(report.fraction_lost, 85);
        assert_eq!(report.last_sr, 0, "还没收到平台 SR");
        assert_eq!(report.delay_since_last_sr, 0);

        // 第二次报告:区间内没有新包 → 应收/实收都没涨,丢包率归 0。
        let again = rx.stats.reception_report().expect("应能生成报告");
        assert_eq!(again.fraction_lost, 0, "丢包率是区间值而非累计值");
        assert_eq!(again.cumulative_lost, 2, "累计丢包仍是 2");
    }

    #[tokio::test]
    async fn rr_序号回绕后扩展序号进位() {
        let rx = receiver().await;
        let mut expected = None;
        rx.ingest(&rtp_full(65_534, 0, 1, &[0xD5; 160]), &mut expected);
        rx.ingest(&rtp_full(65_535, 160, 1, &[0xD5; 160]), &mut expected);
        rx.ingest(&rtp_full(0, 320, 1, &[0xD5; 160]), &mut expected);
        rx.ingest(&rtp_full(1, 480, 1, &[0xD5; 160]), &mut expected);
        let report = rx.stats.reception_report().expect("报告");
        assert_eq!(
            report.extended_highest_seq, 65_537,
            "回绕后扩展序号应带上 1 个 cycle(65536 + 1)"
        );
        assert_eq!(report.cumulative_lost, 0, "回绕不该被算成丢了 6 万包");
    }

    #[tokio::test]
    async fn rr_抖动随到达间隔波动而增大() {
        let rx = receiver().await;
        let mut expected = None;
        // 时间戳按 20ms(160 采样)递增,而 ingest 的到达时刻几乎相同 →
        // 传输时延逐包变化,抖动应被累积起来。
        for i in 0..10u16 {
            rx.ingest(
                &rtp_full(i, u32::from(i) * 160, 7, &[0xD5; 160]),
                &mut expected,
            );
        }
        assert!(
            rx.stats.jitter() > 0,
            "包按 20ms 步进的时间戳瞬间到达,抖动不该是 0"
        );
        assert!(rx.stats.snapshot().jitter_ms > 0.0);
    }

    #[tokio::test]
    async fn rr_收到平台sr后填入lsr与dlsr() {
        let rx = receiver().await;
        let mut expected = None;
        rx.ingest(&rtp_full(1, 160, 9, &[0xD5; 160]), &mut expected);
        assert!(!rx.stats.snapshot().peer_sr_seen);

        // 平台 SR:NTP 秒 = 0x0000_0003,小数高 16 位 = 0x8000 → 中间 32 位 0x0003_8000。
        let sr = crate::rtp::build_rtcp_sr(0xF00D, 3, 0x8000_0000, 160, 5, 800);
        rx.ingest_rtcp(&sr);
        assert!(rx.stats.snapshot().peer_sr_seen);
        let report = rx.stats.reception_report().expect("报告");
        assert_eq!(report.last_sr, 0x0003_8000, "LSR 取 SR 的 NTP 中间 32 位");
        // 刚收到就生成报告,DLSR 应接近 0(单位 1/65536 秒)。
        assert!(report.delay_since_last_sr < 65_536, "DLSR 应小于 1 秒");
    }

    #[tokio::test]
    async fn rr_端到端回报给平台() {
        // 扮演平台:RTP 发送端口 p 与 RTCP 端口 p+1。
        let plat_rtp = UdpSocket::bind("127.0.0.1:0").await.expect("bind rtp");
        let plat_port = plat_rtp.local_addr().unwrap().port();
        let plat_rtcp = UdpSocket::bind(format!("127.0.0.1:{}", plat_port + 1))
            .await
            .expect("测试环境应能绑 +1 端口");

        let rx = AudioReceiver::bind(IpAddr::from([127, 0, 0, 1]))
            .await
            .expect("bind")
            .with_rr_interval(Duration::from_millis(80));
        let port = rx.local_port().expect("port");
        assert_eq!(rx.local_rtcp_port(), Some(port + 1));
        let stats = rx.stats();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            rx.run(async {
                let _ = stop_rx.await;
            })
            .await
        });

        // 平台推 5 个音频包(缺一个序号,制造丢包)。
        let dst = SocketAddr::from(([127, 0, 0, 1], port));
        for seq in [1u16, 2, 3, 5, 6] {
            plat_rtp
                .send_to(
                    &rtp_full(seq, u32::from(seq) * 160, 0x1234, &[0xAA; 160]),
                    dst,
                )
                .await
                .expect("send");
        }

        // 等设备回报 RR。
        let mut buf = [0u8; 256];
        let (n, from) = tokio::time::timeout(Duration::from_secs(3), plat_rtcp.recv_from(&mut buf))
            .await
            .expect("应在 3s 内收到设备回报的 RR")
            .expect("recv");
        assert_eq!(from.port(), port + 1, "RR 应从设备的 RTCP 端口发出");
        let parsed = crate::rtcp::parse_compound(&buf[..n]);
        match &parsed[0] {
            crate::rtcp::RtcpPacket::ReceiverReport { reports, .. } => {
                assert_eq!(reports.len(), 1);
                assert_eq!(reports[0].ssrc, 0x1234, "报告的是平台那条流");
                assert!(reports[0].cumulative_lost >= 1, "序号 4 缺失应被报出来");
            }
            other => panic!("应是 RR,实际 {other:?}"),
        }
        assert!(stats.snapshot().rr_sent >= 1);

        let _ = stop_tx.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("收流任务应及时退出");
    }
}
