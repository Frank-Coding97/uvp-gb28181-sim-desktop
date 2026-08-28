//! 压测指标采集。
//!
//! 用原子计数,热路径无锁(NFR-4)。所有设备共享一个 `Arc<Metrics>`,
//! 在注册/心跳/推流路径上累加;编排器或 UI 定时 [`Metrics::snapshot`] 读取聚合值。
//!
//! 相对最初版本补齐了三处会直接影响"压测结论是否可信"的东西:
//! - **延迟分位数**:压测的目的是找平台承载拐点,而拐点通常先表现为**延迟上升**、
//!   再表现为失败。只有成功/失败计数时,只能看到"已经失败了"。
//! - **在线设备数 gauge**:最该有的一个数,之前完全没有。
//! - **失败归因区分本地与平台**:本机丢包/发送失败绝不能算成平台承载不住。

use std::sync::atomic::{AtomicU64, Ordering};

use common::{DeviceEvent, DeviceObserver, FailureKind as CommonKind};

/// 延迟直方图的桶数。桶 i 覆盖 [2^i, 2^(i+1)) 毫秒,i∈[0,BUCKETS)。
/// 24 个桶覆盖到约 16.7 秒,足够容纳 SIP 事务的 Timer B(32s 之前的全部有效样本)。
const BUCKETS: usize = 24;

/// 无锁延迟直方图(对数分桶)。
///
/// 刻意不引入 hdrhistogram:热路径只做一次 `leading_zeros` + 一次 `fetch_add`,
/// 分位数精度是"同一个 2 倍区间内线性插值",对判断拐点足够,而且不带来额外依赖。
#[derive(Debug, Default)]
pub struct LatencyHistogram {
    buckets: [AtomicU64; BUCKETS],
    count: AtomicU64,
    sum_ms: AtomicU64,
    max_ms: AtomicU64,
}

impl LatencyHistogram {
    /// 记一个样本(毫秒)。
    pub fn record(&self, ms: u64) {
        let idx = Self::bucket_of(ms);
        self.buckets[idx].fetch_add(1, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_ms.fetch_add(ms, Ordering::Relaxed);
        let _ = self
            .max_ms
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                (ms > cur).then_some(ms)
            });
    }

    fn bucket_of(ms: u64) -> usize {
        if ms == 0 {
            return 0;
        }
        // floor(log2(ms)) —— 64 位下 63 - leading_zeros。
        let idx = (63 - ms.leading_zeros()) as usize;
        idx.min(BUCKETS - 1)
    }

    /// 桶下界(含)与上界(不含),单位毫秒。
    fn bucket_bounds(idx: usize) -> (u64, u64) {
        let lo = 1u64 << idx;
        // 第 0 桶把 0ms 也算进来。
        let lo = if idx == 0 { 0 } else { lo };
        (lo, 1u64 << (idx + 1))
    }

    /// 计算分位数(0.0~1.0)。无样本时返回 None。
    pub fn percentile(&self, q: f64) -> Option<u64> {
        let total = self.count.load(Ordering::Relaxed);
        if total == 0 {
            return None;
        }
        let q = q.clamp(0.0, 1.0);
        let target = (total as f64 * q).ceil().max(1.0) as u64;
        let mut cumulative = 0u64;
        for idx in 0..BUCKETS {
            let n = self.buckets[idx].load(Ordering::Relaxed);
            if n == 0 {
                continue;
            }
            if cumulative + n >= target {
                let (lo, hi) = Self::bucket_bounds(idx);
                // 桶内线性插值。
                let into = target - cumulative;
                let frac = into as f64 / n as f64;
                let value = lo as f64 + (hi - lo) as f64 * frac;
                return Some(value.round() as u64);
            }
            cumulative += n;
        }
        Some(self.max_ms.load(Ordering::Relaxed))
    }

    /// 读取快照。
    pub fn snapshot(&self) -> LatencySnapshot {
        let count = self.count.load(Ordering::Relaxed);
        LatencySnapshot {
            count,
            avg_ms: if count == 0 {
                0.0
            } else {
                self.sum_ms.load(Ordering::Relaxed) as f64 / count as f64
            },
            p50_ms: self.percentile(0.50).unwrap_or(0),
            p95_ms: self.percentile(0.95).unwrap_or(0),
            p99_ms: self.percentile(0.99).unwrap_or(0),
            max_ms: self.max_ms.load(Ordering::Relaxed),
        }
    }
}

/// 延迟分布快照。
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct LatencySnapshot {
    pub count: u64,
    pub avg_ms: f64,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
    pub max_ms: u64,
}

/// 压测实时指标(原子计数,可跨设备共享)。
#[derive(Debug, Default)]
pub struct Metrics {
    /// 已尝试注册次数(含重试)。
    pub register_attempted: AtomicU64,
    /// 首次注册尝试次数(≈ 参与压测的设备数)。
    pub register_first_attempt: AtomicU64,
    /// 重试性注册尝试次数(退避重试 / 掉线重注册)。
    pub register_retry: AtomicU64,
    /// 注册成功次数。
    pub register_succeeded: AtomicU64,
    /// 注册失败次数(按失败归因细分见 fail_* )。
    pub register_failed: AtomicU64,
    /// 心跳成功次数。
    pub heartbeat_succeeded: AtomicU64,
    /// 心跳失败次数。
    pub heartbeat_failed: AtomicU64,
    /// 当前活跃推流路数。
    pub active_streams: AtomicU64,
    /// 当前在线设备数(gauge)。
    pub devices_online: AtomicU64,
    /// 历史峰值在线设备数。
    pub devices_online_peak: AtomicU64,
    /// 失败归因:超时。
    pub fail_timeout: AtomicU64,
    /// 失败归因:平台拒绝(4xx/5xx)。
    pub fail_rejected: AtomicU64,
    /// 失败归因:**本地**发送失败/背压(不是平台的问题)。
    pub fail_local: AtomicU64,
    /// 失败归因:网络/其它。
    pub fail_other: AtomicU64,
    /// 批量定位上报成功次数(主动上报施压)。
    pub position_reported: AtomicU64,
    /// 批量报警上报成功次数(主动上报施压)。
    pub alarm_reported: AtomicU64,
    /// 注册事务往返延迟分布。
    pub register_latency: LatencyHistogram,
    /// 心跳往返延迟分布。
    pub heartbeat_latency: LatencyHistogram,
}

/// 失败归因分类(对应 docs/20-architecture/data-model.md#4)。
#[derive(Debug, Clone, Copy)]
pub enum FailureKind {
    Timeout,
    Rejected,
    /// 本地瓶颈(发送失败 / 队列满),与平台过载严格区分。
    Local,
    Other,
}

impl Metrics {
    /// 记一次注册尝试。`retry` 区分首次与重试。
    pub fn on_register_attempt(&self, retry: bool) {
        self.register_attempted.fetch_add(1, Ordering::Relaxed);
        if retry {
            self.register_retry.fetch_add(1, Ordering::Relaxed);
        } else {
            self.register_first_attempt.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 记一次注册成功(带事务往返耗时)。
    pub fn on_register_success(&self, rtt_ms: u64) {
        self.register_succeeded.fetch_add(1, Ordering::Relaxed);
        self.register_latency.record(rtt_ms);
    }

    /// 记一次注册失败(带归因)。
    pub fn on_register_failure(&self, kind: FailureKind) {
        self.register_failed.fetch_add(1, Ordering::Relaxed);
        match kind {
            FailureKind::Timeout => self.fail_timeout.fetch_add(1, Ordering::Relaxed),
            FailureKind::Rejected => self.fail_rejected.fetch_add(1, Ordering::Relaxed),
            FailureKind::Local => self.fail_local.fetch_add(1, Ordering::Relaxed),
            FailureKind::Other => self.fail_other.fetch_add(1, Ordering::Relaxed),
        };
    }

    /// 记一次心跳成功(带往返耗时)。
    pub fn on_heartbeat_success(&self, rtt_ms: u64) {
        self.heartbeat_succeeded.fetch_add(1, Ordering::Relaxed);
        self.heartbeat_latency.record(rtt_ms);
    }

    /// 记一次心跳失败。
    pub fn on_heartbeat_failure(&self) {
        self.heartbeat_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// 设备上线/下线(gauge 增减 + 峰值跟踪)。
    pub fn on_online_changed(&self, online: bool) {
        if online {
            let now = self.devices_online.fetch_add(1, Ordering::Relaxed) + 1;
            let _ = self.devices_online_peak.fetch_update(
                Ordering::Relaxed,
                Ordering::Relaxed,
                |peak| (now > peak).then_some(now),
            );
        } else {
            let _ = self
                .devices_online
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                    Some(v.saturating_sub(1))
                });
        }
    }

    /// 推流开始(活跃数 +1)。
    pub fn on_stream_start(&self) {
        self.active_streams.fetch_add(1, Ordering::Relaxed);
    }

    /// 推流结束(活跃数 -1,不低于 0)。
    pub fn on_stream_stop(&self) {
        let _ = self
            .active_streams
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            });
    }

    /// 读取聚合快照(供 CLI/UI/落库)。`transport` 为传输层统计(可选)。
    pub fn snapshot(&self) -> MetricsSnapshot {
        self.snapshot_with_transport(None)
    }

    /// 读取聚合快照并附带传输层统计。
    ///
    /// 传输层统计是判断"结论是否可信"的关键:`dropped_queue_full`/`send_errors`
    /// 一旦非零,说明失败里混进了本机瓶颈,不能直接当成平台承载上限。
    pub fn snapshot_with_transport(
        &self,
        transport: Option<sip_core::TransportStatsSnapshot>,
    ) -> MetricsSnapshot {
        let attempted = self.register_attempted.load(Ordering::Relaxed);
        let first = self.register_first_attempt.load(Ordering::Relaxed);
        let succeeded = self.register_succeeded.load(Ordering::Relaxed);
        let local_pressure = transport
            .map(|t| t.dropped_queue_full + t.send_errors + t.parse_errors)
            .unwrap_or(0)
            + self.fail_local.load(Ordering::Relaxed);
        MetricsSnapshot {
            register_attempted: attempted,
            register_first_attempt: first,
            register_retry: self.register_retry.load(Ordering::Relaxed),
            register_succeeded: succeeded,
            register_failed: self.register_failed.load(Ordering::Relaxed),
            register_success_rate: ratio(succeeded, attempted),
            // 设备维度成功率:不被退避重试稀释,更接近"多少台真的上线了"。
            device_success_rate: ratio(succeeded.min(first), first),
            heartbeat_succeeded: self.heartbeat_succeeded.load(Ordering::Relaxed),
            heartbeat_failed: self.heartbeat_failed.load(Ordering::Relaxed),
            active_streams: self.active_streams.load(Ordering::Relaxed),
            devices_online: self.devices_online.load(Ordering::Relaxed),
            devices_online_peak: self.devices_online_peak.load(Ordering::Relaxed),
            fail_timeout: self.fail_timeout.load(Ordering::Relaxed),
            fail_rejected: self.fail_rejected.load(Ordering::Relaxed),
            fail_local: self.fail_local.load(Ordering::Relaxed),
            fail_other: self.fail_other.load(Ordering::Relaxed),
            position_reported: self.position_reported.load(Ordering::Relaxed),
            alarm_reported: self.alarm_reported.load(Ordering::Relaxed),
            register_latency: self.register_latency.snapshot(),
            heartbeat_latency: self.heartbeat_latency.snapshot(),
            transport,
            local_pressure,
            results_trustworthy: local_pressure == 0,
        }
    }

    /// 记一次定位上报成功。
    pub fn on_position_reported(&self) {
        self.position_reported.fetch_add(1, Ordering::Relaxed);
    }

    /// 记一次报警上报成功。
    pub fn on_alarm_reported(&self) {
        self.alarm_reported.fetch_add(1, Ordering::Relaxed);
    }
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

/// 指标只读快照(可序列化,推 UI / 落库)。
#[derive(Debug, Clone, serde::Serialize)]
pub struct MetricsSnapshot {
    pub register_attempted: u64,
    pub register_first_attempt: u64,
    pub register_retry: u64,
    pub register_succeeded: u64,
    pub register_failed: u64,
    pub register_success_rate: f64,
    pub device_success_rate: f64,
    pub heartbeat_succeeded: u64,
    pub heartbeat_failed: u64,
    pub active_streams: u64,
    pub devices_online: u64,
    pub devices_online_peak: u64,
    pub fail_timeout: u64,
    pub fail_rejected: u64,
    pub fail_local: u64,
    pub fail_other: u64,
    pub position_reported: u64,
    pub alarm_reported: u64,
    pub register_latency: LatencySnapshot,
    pub heartbeat_latency: LatencySnapshot,
    /// 传输层统计(共享 socket 的收发/丢弃计数)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<sip_core::TransportStatsSnapshot>,
    /// 本地压力总量(队列丢弃 + 发送失败 + 解析失败 + 本地归因失败)。
    pub local_pressure: u64,
    /// 结论是否可信:本地压力为 0 时,失败可归因于平台;否则先扩本机再测。
    pub results_trustworthy: bool,
}

/// Metrics 实现 DeviceObserver，可直接注入 DeviceSimulator::with_observer。
impl DeviceObserver for Metrics {
    fn on_event(&self, event: DeviceEvent) {
        match event {
            DeviceEvent::RegisterAttempt { retry } => self.on_register_attempt(retry),
            DeviceEvent::RegisterSuccess { rtt_ms } => self.on_register_success(rtt_ms),
            DeviceEvent::RegisterFailure(k) => {
                let kind = match k {
                    CommonKind::Timeout => FailureKind::Timeout,
                    CommonKind::Rejected => FailureKind::Rejected,
                    CommonKind::Local => FailureKind::Local,
                    CommonKind::Other => FailureKind::Other,
                };
                self.on_register_failure(kind);
            }
            DeviceEvent::HeartbeatOk { rtt_ms } => self.on_heartbeat_success(rtt_ms),
            DeviceEvent::HeartbeatFail => self.on_heartbeat_failure(),
            DeviceEvent::OnlineChanged { online } => self.on_online_changed(online),
            DeviceEvent::StreamStart => self.on_stream_start(),
            DeviceEvent::StreamStop => self.on_stream_stop(),
            // 压测不关心云台动作/OSD/命令时间线/订阅/进度/运行时错误(均 UI 展示用),忽略。
            DeviceEvent::Ptz { .. }
            | DeviceEvent::PtzPresetCall { .. }
            | DeviceEvent::OsdConfig { .. }
            | DeviceEvent::PlatformCommand { .. }
            | DeviceEvent::SubscriptionChanged { .. }
            | DeviceEvent::Progress { .. }
            | DeviceEvent::RuntimeError { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 计数与成功率() {
        let m = Metrics::default();
        for _ in 0..200 {
            m.on_register_attempt(false);
        }
        for _ in 0..150 {
            m.on_register_success(20);
        }
        m.on_register_failure(FailureKind::Timeout);
        let s = m.snapshot();
        assert_eq!(s.register_attempted, 200);
        assert_eq!(s.register_succeeded, 150);
        assert!((s.register_success_rate - 0.75).abs() < 1e-9);
        assert_eq!(s.fail_timeout, 1);
    }

    #[test]
    fn 设备维度成功率不被重试稀释() {
        let m = Metrics::default();
        // 10 台设备首次尝试,其中 8 台成功;另有 40 次退避重试。
        for _ in 0..10 {
            m.on_register_attempt(false);
        }
        for _ in 0..40 {
            m.on_register_attempt(true);
        }
        for _ in 0..8 {
            m.on_register_success(15);
        }
        let s = m.snapshot();
        // 尝试维度被重试严重稀释(8/50)。
        assert!((s.register_success_rate - 0.16).abs() < 1e-9);
        // 设备维度反映真实上线比例(8/10)。
        assert!((s.device_success_rate - 0.8).abs() < 1e-9);
        assert_eq!(s.register_first_attempt, 10);
        assert_eq!(s.register_retry, 40);
    }

    #[test]
    fn 活跃流增减() {
        let m = Metrics::default();
        m.on_stream_start();
        m.on_stream_start();
        m.on_stream_stop();
        assert_eq!(m.snapshot().active_streams, 1);
        m.on_stream_stop();
        m.on_stream_stop(); // 不低于 0
        assert_eq!(m.snapshot().active_streams, 0);
    }

    #[test]
    fn 在线设备数与峰值() {
        let m = Metrics::default();
        for _ in 0..5 {
            m.on_online_changed(true);
        }
        assert_eq!(m.snapshot().devices_online, 5);
        m.on_online_changed(false);
        m.on_online_changed(false);
        let s = m.snapshot();
        assert_eq!(s.devices_online, 3);
        assert_eq!(s.devices_online_peak, 5, "峰值应记住最高水位");
        // 不会下溢。
        for _ in 0..10 {
            m.on_online_changed(false);
        }
        assert_eq!(m.snapshot().devices_online, 0);
    }

    #[test]
    fn 直方图分桶与分位数() {
        let h = LatencyHistogram::default();
        assert!(h.percentile(0.5).is_none(), "无样本时无分位数");
        // 95 个 ~10ms 样本 + 5 个 5000ms 长尾(尾部占 5%,故 p95/p99 都应落在尾部)。
        for _ in 0..95 {
            h.record(10);
        }
        for _ in 0..5 {
            h.record(5000);
        }
        let s = h.snapshot();
        assert_eq!(s.count, 100);
        assert_eq!(s.max_ms, 5000);
        // p50 落在 8..16 桶内。
        assert!((8..16).contains(&s.p50_ms), "p50={}", s.p50_ms);
        // p99 应把长尾暴露出来 —— 这正是只看成功率看不到的东西。
        assert!(s.p99_ms >= 4096, "p99={}", s.p99_ms);
        assert!(s.avg_ms > 200.0 && s.avg_ms < 400.0, "avg={}", s.avg_ms);

        // 尾部只占 1% 以下时 p99 不应被单个离群点带偏(分位数语义)。
        let h2 = LatencyHistogram::default();
        for _ in 0..999 {
            h2.record(10);
        }
        h2.record(9000);
        let s2 = h2.snapshot();
        assert!(
            s2.p99_ms < 100,
            "p99={} 不该被 0.1% 的离群点主导",
            s2.p99_ms
        );
        assert_eq!(s2.max_ms, 9000, "但 max 必须保留离群点");
    }

    #[test]
    fn 直方图桶边界() {
        assert_eq!(LatencyHistogram::bucket_of(0), 0);
        assert_eq!(LatencyHistogram::bucket_of(1), 0);
        assert_eq!(LatencyHistogram::bucket_of(2), 1);
        assert_eq!(LatencyHistogram::bucket_of(3), 1);
        assert_eq!(LatencyHistogram::bucket_of(4), 2);
        assert_eq!(LatencyHistogram::bucket_of(1023), 9);
        assert_eq!(LatencyHistogram::bucket_of(1024), 10);
        // 超上限的样本落到最后一个桶,不越界 panic。
        assert_eq!(LatencyHistogram::bucket_of(u64::MAX), BUCKETS - 1);
    }

    #[test]
    fn 本地压力使结论不可信() {
        let m = Metrics::default();
        m.on_register_attempt(false);
        m.on_register_success(10);
        assert!(m.snapshot().results_trustworthy, "干净跑时结论可信");

        m.on_register_failure(FailureKind::Local);
        let s = m.snapshot();
        assert_eq!(s.fail_local, 1);
        assert!(!s.results_trustworthy, "出现本地瓶颈后不应再把失败归给平台");
        assert_eq!(s.local_pressure, 1);
    }
}
