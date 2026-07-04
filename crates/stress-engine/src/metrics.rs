//! 压测指标采集。
//!
//! 用原子计数,热路径无锁(NFR-4)。所有设备共享一个 `Arc<Metrics>`,
//! 在注册/心跳/推流路径上累加;编排器或 UI 定时 [`Metrics::snapshot`] 读取聚合值。

use std::sync::atomic::{AtomicU64, Ordering};

use common::{DeviceEvent, DeviceObserver, FailureKind as CommonKind};

/// 压测实时指标(原子计数,可跨设备共享)。
#[derive(Debug, Default)]
pub struct Metrics {
    /// 已尝试注册次数。
    pub register_attempted: AtomicU64,
    /// 注册成功次数。
    pub register_succeeded: AtomicU64,
    /// 注册失败次数(按失败归因细分见 failure_* )。
    pub register_failed: AtomicU64,
    /// 心跳成功次数。
    pub heartbeat_succeeded: AtomicU64,
    /// 心跳失败次数。
    pub heartbeat_failed: AtomicU64,
    /// 当前活跃推流路数。
    pub active_streams: AtomicU64,
    /// 失败归因:超时。
    pub fail_timeout: AtomicU64,
    /// 失败归因:平台拒绝(4xx/5xx)。
    pub fail_rejected: AtomicU64,
    /// 失败归因:网络/其它。
    pub fail_other: AtomicU64,
    /// 批量定位上报成功次数(主动上报施压)。
    pub position_reported: AtomicU64,
    /// 批量报警上报成功次数(主动上报施压)。
    pub alarm_reported: AtomicU64,
}

/// 失败归因分类(对应 docs/20-architecture/data-model.md#4)。
#[derive(Debug, Clone, Copy)]
pub enum FailureKind {
    Timeout,
    Rejected,
    Other,
}

impl Metrics {
    /// 记一次注册尝试。
    pub fn on_register_attempt(&self) {
        self.register_attempted.fetch_add(1, Ordering::Relaxed);
    }

    /// 记一次注册成功。
    pub fn on_register_success(&self) {
        self.register_succeeded.fetch_add(1, Ordering::Relaxed);
    }

    /// 记一次注册失败(带归因)。
    pub fn on_register_failure(&self, kind: FailureKind) {
        self.register_failed.fetch_add(1, Ordering::Relaxed);
        match kind {
            FailureKind::Timeout => self.fail_timeout.fetch_add(1, Ordering::Relaxed),
            FailureKind::Rejected => self.fail_rejected.fetch_add(1, Ordering::Relaxed),
            FailureKind::Other => self.fail_other.fetch_add(1, Ordering::Relaxed),
        };
    }

    /// 记一次心跳成功。
    pub fn on_heartbeat_success(&self) {
        self.heartbeat_succeeded.fetch_add(1, Ordering::Relaxed);
    }

    /// 记一次心跳失败。
    pub fn on_heartbeat_failure(&self) {
        self.heartbeat_failed.fetch_add(1, Ordering::Relaxed);
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

    /// 读取聚合快照(供 CLI/UI/落库)。
    pub fn snapshot(&self) -> MetricsSnapshot {
        let attempted = self.register_attempted.load(Ordering::Relaxed);
        let succeeded = self.register_succeeded.load(Ordering::Relaxed);
        MetricsSnapshot {
            register_attempted: attempted,
            register_succeeded: succeeded,
            register_failed: self.register_failed.load(Ordering::Relaxed),
            register_success_rate: if attempted == 0 {
                0.0
            } else {
                succeeded as f64 / attempted as f64
            },
            heartbeat_succeeded: self.heartbeat_succeeded.load(Ordering::Relaxed),
            heartbeat_failed: self.heartbeat_failed.load(Ordering::Relaxed),
            active_streams: self.active_streams.load(Ordering::Relaxed),
            fail_timeout: self.fail_timeout.load(Ordering::Relaxed),
            fail_rejected: self.fail_rejected.load(Ordering::Relaxed),
            fail_other: self.fail_other.load(Ordering::Relaxed),
            position_reported: self.position_reported.load(Ordering::Relaxed),
            alarm_reported: self.alarm_reported.load(Ordering::Relaxed),
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

/// 指标只读快照(可序列化,推 UI / 落库)。
#[derive(Debug, Clone, serde::Serialize)]
pub struct MetricsSnapshot {
    pub register_attempted: u64,
    pub register_succeeded: u64,
    pub register_failed: u64,
    pub register_success_rate: f64,
    pub heartbeat_succeeded: u64,
    pub heartbeat_failed: u64,
    pub active_streams: u64,
    pub fail_timeout: u64,
    pub fail_rejected: u64,
    pub fail_other: u64,
    pub position_reported: u64,
    pub alarm_reported: u64,
}

/// Metrics 实现 DeviceObserver，可直接注入 DeviceSimulator::with_observer。
impl DeviceObserver for Metrics {
    fn on_event(&self, event: DeviceEvent) {
        match event {
            DeviceEvent::RegisterAttempt => self.on_register_attempt(),
            DeviceEvent::RegisterSuccess => self.on_register_success(),
            DeviceEvent::RegisterFailure(k) => {
                let kind = match k {
                    CommonKind::Timeout => FailureKind::Timeout,
                    CommonKind::Rejected => FailureKind::Rejected,
                    CommonKind::Other => FailureKind::Other,
                };
                self.on_register_failure(kind);
            }
            DeviceEvent::HeartbeatOk => self.on_heartbeat_success(),
            DeviceEvent::HeartbeatFail => self.on_heartbeat_failure(),
            DeviceEvent::StreamStart => self.on_stream_start(),
            DeviceEvent::StreamStop => self.on_stream_stop(),
            // 压测不关心云台动作(UI 动画用),忽略。
            DeviceEvent::Ptz { .. } | DeviceEvent::PtzPresetCall { .. } => {}
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
            m.on_register_attempt();
        }
        for _ in 0..150 {
            m.on_register_success();
        }
        m.on_register_failure(FailureKind::Timeout);
        let s = m.snapshot();
        assert_eq!(s.register_attempted, 200);
        assert_eq!(s.register_succeeded, 150);
        assert!((s.register_success_rate - 0.75).abs() < 1e-9);
        assert_eq!(s.fail_timeout, 1);
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
}
