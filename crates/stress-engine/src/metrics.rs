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
use std::sync::Mutex;

use common::{DeviceEvent, DeviceObserver};

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
#[derive(ts_rs::TS)]
#[ts(export, export_to = "../../../apps/desktop/src/types/generated/")]
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct LatencySnapshot {
    #[ts(type = "number")]
    pub count: u64,
    pub avg_ms: f64,
    #[ts(type = "number")]
    pub p50_ms: u64,
    #[ts(type = "number")]
    pub p95_ms: u64,
    #[ts(type = "number")]
    pub p99_ms: u64,
    #[ts(type = "number")]
    pub max_ms: u64,
}

/// `first_failure_online` 的哨兵值:还没有任何失败。
///
/// 不能用 0 —— "第一台设备还没上线就失败了"是完全可能的真实情形,
/// 用 0 当哨兵会把这种最值得注意的情况显示成"从未失败"。
pub const NO_FAILURE_YET: u64 = u64::MAX;

/// 压测实时指标(原子计数,可跨设备共享)。
#[derive(Debug)]
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
    /// 失败归因:平台拒绝(4xx/5xx)总数。
    pub fail_rejected: AtomicU64,
    /// 平台拒绝里的**凭据/权限**档(401/403/407)。
    ///
    /// 单独计数是因为处置动作完全不同:这个数大说明要去核对用户名/密码/域,
    /// 而不是"平台承载不住"。以前它和 503 混在同一个计数里,报告读不出结论。
    pub fail_rejected_auth: AtomicU64,
    /// 平台拒绝里的**忙/过载**档(480/486/500/503/504)—— 真正的承载力信号。
    pub fail_rejected_overload: AtomicU64,
    /// 平台拒绝里的其它状态码(常见 404:编号不被平台接受)。
    pub fail_rejected_other: AtomicU64,
    /// 首次失败时的在线设备数(**失败水位**)。
    ///
    /// `u64::MAX` 表示还没有失败过。这个数回答"撑到多少台开始出问题" ——
    /// 之前只有全局累计计数,时序 JSONL 事后差分才能勉强看出拐点。
    pub first_failure_online: AtomicU64,
    /// 爬坡因本地背压而减速的次数(见 scheduler 的闭环背压)。
    pub ramp_throttled: AtomicU64,
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
    /// 失败设备样本(最多 [`MAX_FAILURE_SAMPLES`] 条,先到先留)。
    ///
    /// 排障要挑一台具体设备复现,而计数器给不出任何一台的编号。
    /// 用锁没问题:只在失败时写,且写满就不再加锁修改。
    failure_samples: Mutex<Vec<FailureSample>>,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics {
            register_attempted: AtomicU64::new(0),
            register_first_attempt: AtomicU64::new(0),
            register_retry: AtomicU64::new(0),
            register_succeeded: AtomicU64::new(0),
            register_failed: AtomicU64::new(0),
            heartbeat_succeeded: AtomicU64::new(0),
            heartbeat_failed: AtomicU64::new(0),
            active_streams: AtomicU64::new(0),
            devices_online: AtomicU64::new(0),
            devices_online_peak: AtomicU64::new(0),
            fail_timeout: AtomicU64::new(0),
            fail_rejected: AtomicU64::new(0),
            fail_rejected_auth: AtomicU64::new(0),
            fail_rejected_overload: AtomicU64::new(0),
            fail_rejected_other: AtomicU64::new(0),
            first_failure_online: AtomicU64::new(NO_FAILURE_YET),
            ramp_throttled: AtomicU64::new(0),
            fail_local: AtomicU64::new(0),
            fail_other: AtomicU64::new(0),
            position_reported: AtomicU64::new(0),
            alarm_reported: AtomicU64::new(0),
            register_latency: LatencyHistogram::default(),
            heartbeat_latency: LatencyHistogram::default(),
            failure_samples: Mutex::new(Vec::new()),
        }
    }
}

/// 失败样本上限。留够挑几台复现即可,不做全量记录 ——
/// 大面积失败时全量记录本身就会变成压力源。
pub const MAX_FAILURE_SAMPLES: usize = 20;

/// 一条失败样本。
#[derive(ts_rs::TS)]
#[ts(export, export_to = "../../../apps/desktop/src/types/generated/")]
#[derive(Debug, Clone, serde::Serialize)]
pub struct FailureSample {
    /// 失败设备国标编号。
    pub device_id: String,
    /// 归因分类名(timeout/rejected/local/other)。
    pub kind: String,
    /// 平台状态码(仅拒绝类有)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// 出现该失败时的在线设备数 —— 定位"在什么水位上发生的"。
    #[ts(type = "number")]
    pub online_at_failure: u64,
}

/// 失败归因分类。
///
/// **直接复用 `common::FailureKind`**,不再在本 crate 重复定义一份 ——
/// 两份枚举加一个手写的转换 match,意味着 common 那边加一个变体时
/// 这里只会静默地映射到 `Other`,而失败归因的可信度正建立在这个分类上。
pub use common::FailureKind;

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

    /// 记一次注册失败(带归因与设备 ID)。
    ///
    /// 除了分类计数,还记录**失败水位**(首次失败时的在线数)与**失败样本**
    /// (最多几台设备编号)—— 排障需要"撑到多少台开始出问题"和"挑哪台复现",
    /// 这两个问题总计数都回答不了。
    pub fn on_register_failure(&self, kind: FailureKind, device_id: &str) {
        self.register_failed.fetch_add(1, Ordering::Relaxed);
        let online = self.devices_online.load(Ordering::Relaxed);
        let (label, status) = match kind {
            FailureKind::Timeout => {
                self.fail_timeout.fetch_add(1, Ordering::Relaxed);
                ("timeout", None)
            }
            FailureKind::Rejected { status } => {
                self.fail_rejected.fetch_add(1, Ordering::Relaxed);
                match common::RejectBucket::of(status) {
                    common::RejectBucket::Auth => {
                        self.fail_rejected_auth.fetch_add(1, Ordering::Relaxed)
                    }
                    common::RejectBucket::Overload => {
                        self.fail_rejected_overload.fetch_add(1, Ordering::Relaxed)
                    }
                    common::RejectBucket::Other => {
                        self.fail_rejected_other.fetch_add(1, Ordering::Relaxed)
                    }
                };
                ("rejected", Some(status))
            }
            FailureKind::Local => {
                self.fail_local.fetch_add(1, Ordering::Relaxed);
                ("local", None)
            }
            FailureKind::Other => {
                self.fail_other.fetch_add(1, Ordering::Relaxed);
                ("other", None)
            }
        };
        // 失败水位只记第一次:用 CAS 而不是"读了再写",避免并发下被后来的失败覆盖。
        let _ = self.first_failure_online.compare_exchange(
            NO_FAILURE_YET,
            online,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
        // 样本满了就不再加锁 —— 大面积失败时抢锁本身会变成压力源。
        if let Ok(mut samples) = self.failure_samples.lock() {
            if samples.len() < MAX_FAILURE_SAMPLES {
                samples.push(FailureSample {
                    device_id: device_id.to_string(),
                    kind: label.into(),
                    status,
                    online_at_failure: online,
                });
            }
        }
    }

    /// 记一次爬坡减速(闭环背压触发)。
    pub fn on_ramp_throttled(&self) {
        self.ramp_throttled.fetch_add(1, Ordering::Relaxed);
    }

    /// 当前失败样本快照。
    pub fn failure_samples(&self) -> Vec<FailureSample> {
        self.failure_samples
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
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
            .map(|t| {
                t.dropped_queue_full
                    + t.send_errors
                    + t.parse_errors
                    + t.rate_limited
                    + t.anti_amplification_blocked
            })
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
            fail_rejected_auth: self.fail_rejected_auth.load(Ordering::Relaxed),
            fail_rejected_overload: self.fail_rejected_overload.load(Ordering::Relaxed),
            fail_rejected_other: self.fail_rejected_other.load(Ordering::Relaxed),
            first_failure_online: match self.first_failure_online.load(Ordering::Relaxed) {
                NO_FAILURE_YET => None,
                v => Some(v),
            },
            ramp_throttled: self.ramp_throttled.load(Ordering::Relaxed),
            failure_samples: self.failure_samples(),
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
#[derive(ts_rs::TS)]
#[ts(export, export_to = "../../../apps/desktop/src/types/generated/")]
#[derive(Debug, Clone, serde::Serialize)]
pub struct MetricsSnapshot {
    #[ts(type = "number")]
    pub register_attempted: u64,
    #[ts(type = "number")]
    pub register_first_attempt: u64,
    #[ts(type = "number")]
    pub register_retry: u64,
    #[ts(type = "number")]
    pub register_succeeded: u64,
    #[ts(type = "number")]
    pub register_failed: u64,
    pub register_success_rate: f64,
    pub device_success_rate: f64,
    #[ts(type = "number")]
    pub heartbeat_succeeded: u64,
    #[ts(type = "number")]
    pub heartbeat_failed: u64,
    #[ts(type = "number")]
    pub active_streams: u64,
    #[ts(type = "number")]
    pub devices_online: u64,
    #[ts(type = "number")]
    pub devices_online_peak: u64,
    #[ts(type = "number")]
    pub fail_timeout: u64,
    #[ts(type = "number")]
    pub fail_rejected: u64,
    /// 拒绝里的凭据/权限档(401/403/407):加并发不会变好,去核对账号配置。
    #[ts(type = "number")]
    pub fail_rejected_auth: u64,
    /// 拒绝里的忙/过载档(480/486/500/503/504):真正的承载力信号。
    #[ts(type = "number")]
    pub fail_rejected_overload: u64,
    /// 拒绝里的其它状态码(常见 404:编号不被平台接受)。
    #[ts(type = "number")]
    pub fail_rejected_other: u64,
    #[ts(type = "number")]
    pub fail_local: u64,
    #[ts(type = "number")]
    pub fail_other: u64,
    /// 首次失败时的在线设备数(失败水位);从未失败为 None。
    #[ts(type = "number | null")]
    pub first_failure_online: Option<u64>,
    /// 爬坡因本地背压减速的次数。非 0 说明本机已经跟不上设定的爬坡速率。
    #[ts(type = "number")]
    pub ramp_throttled: u64,
    /// 失败设备样本(最多 20 条),供挑一台复现。
    pub failure_samples: Vec<FailureSample>,
    #[ts(type = "number")]
    pub position_reported: u64,
    #[ts(type = "number")]
    pub alarm_reported: u64,
    pub register_latency: LatencySnapshot,
    pub heartbeat_latency: LatencySnapshot,
    /// 传输层统计(共享 socket 的收发/丢弃计数)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<sip_core::TransportStatsSnapshot>,
    /// 本地压力总量(队列丢弃 + 发送失败 + 解析失败 + 本地归因失败)。
    #[ts(type = "number")]
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
            DeviceEvent::RegisterFailure { kind, device_id } => {
                // 不需要转换:归因枚举就是 common 那一个。
                self.on_register_failure(kind, &device_id);
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
        m.on_register_failure(FailureKind::Timeout, "34020000001320000001");
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

        m.on_register_failure(FailureKind::Local, "34020000001320000001");
        let s = m.snapshot();
        assert_eq!(s.fail_local, 1);
        assert!(!s.results_trustworthy, "出现本地瓶颈后不应再把失败归给平台");
        assert_eq!(s.local_pressure, 1);

        let transport = sip_core::TransportStatsSnapshot {
            rate_limited: 2,
            anti_amplification_blocked: 3,
            ..Default::default()
        };
        let guarded = Metrics::default().snapshot_with_transport(Some(transport));
        assert_eq!(guarded.local_pressure, 5);
        assert!(
            !guarded.results_trustworthy,
            "安全门控丢包也说明设定流量超过本机可接受范围"
        );
    }

    #[test]
    fn 平台拒绝按处置动作分档() {
        // 分档的意义:401 说明去改配置,503 说明去降并发。
        // 之前两者混在同一个 fail_rejected 里,报告读不出该做什么。
        let m = Metrics::default();
        for status in [401u16, 403, 407] {
            m.on_register_failure(FailureKind::Rejected { status }, "34020000001320000001");
        }
        for status in [480u16, 486, 500, 503, 504] {
            m.on_register_failure(FailureKind::Rejected { status }, "34020000001320000002");
        }
        for status in [404u16, 415] {
            m.on_register_failure(FailureKind::Rejected { status }, "34020000001320000003");
        }
        let s = m.snapshot();
        assert_eq!(s.fail_rejected, 10, "总数仍是全部拒绝之和");
        assert_eq!(s.fail_rejected_auth, 3);
        assert_eq!(s.fail_rejected_overload, 5);
        assert_eq!(s.fail_rejected_other, 2);
        // 凭据问题不该被算成本地压力,也不该让结论变"不可信" ——
        // 那是配置错,不是本机瓶颈。
        assert!(s.results_trustworthy);
    }

    #[test]
    fn 失败水位只记第一次() {
        let m = Metrics::default();
        assert!(
            m.snapshot().first_failure_online.is_none(),
            "未失败时应为 None"
        );

        // 先上线 3 台再失败。
        for _ in 0..3 {
            m.on_online_changed(true);
        }
        m.on_register_failure(FailureKind::Timeout, "dev-a");
        assert_eq!(m.snapshot().first_failure_online, Some(3));

        // 之后又上线更多并再次失败:水位保持第一次的值。
        for _ in 0..5 {
            m.on_online_changed(true);
        }
        m.on_register_failure(FailureKind::Timeout, "dev-b");
        assert_eq!(
            m.snapshot().first_failure_online,
            Some(3),
            "水位回答的是「撑到多少台开始出问题」,不该被后来的失败改写"
        );
    }

    #[test]
    fn 零台在线时失败与从未失败可区分() {
        // 这正是不能用 0 当哨兵的理由:"第一台还没上线就失败"是最值得注意的情形。
        let m = Metrics::default();
        m.on_register_failure(FailureKind::Local, "dev-x");
        assert_eq!(m.snapshot().first_failure_online, Some(0));
        assert_ne!(Metrics::default().snapshot().first_failure_online, Some(0));
    }

    #[test]
    fn 失败样本有上限且带可复现信息() {
        let m = Metrics::default();
        m.on_online_changed(true);
        for i in 0..(MAX_FAILURE_SAMPLES + 30) {
            m.on_register_failure(
                FailureKind::Rejected { status: 503 },
                &format!("3402000000132{i:07}"),
            );
        }
        let samples = m.snapshot().failure_samples;
        assert_eq!(
            samples.len(),
            MAX_FAILURE_SAMPLES,
            "大面积失败时全量记录本身会变成压力源"
        );
        assert_eq!(samples[0].kind, "rejected");
        assert_eq!(samples[0].status, Some(503));
        assert_eq!(samples[0].online_at_failure, 1);
        assert!(samples[0].device_id.starts_with("3402000000132"));
    }

    #[test]
    fn 爬坡减速被记录进报告() {
        let m = Metrics::default();
        assert_eq!(m.snapshot().ramp_throttled, 0);
        m.on_ramp_throttled();
        m.on_ramp_throttled();
        assert_eq!(
            m.snapshot().ramp_throttled,
            2,
            "减速次数非 0 是「本机跟不上设定速率」的证据,必须进报告"
        );
    }

    #[test]
    fn 拒绝分档与状态码映射一致() {
        use common::RejectBucket;
        assert_eq!(RejectBucket::of(401), RejectBucket::Auth);
        assert_eq!(RejectBucket::of(403), RejectBucket::Auth);
        assert_eq!(RejectBucket::of(486), RejectBucket::Overload);
        assert_eq!(RejectBucket::of(503), RejectBucket::Overload);
        assert_eq!(RejectBucket::of(404), RejectBucket::Other);
        assert!(!RejectBucket::Auth.label().is_empty());
    }
}
