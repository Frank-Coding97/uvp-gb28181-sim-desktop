//! 压测指标采集。
//!
//! M0 定义计数结构。M3 用原子计数 + 无锁聚合,热路径不加锁;每秒聚合后经 WS 推 UI、落 SQLite。

/// 压测实时指标快照。
#[derive(Debug, Default, Clone)]
pub struct Metrics {
    /// 已尝试注册数。
    pub register_attempted: u64,
    /// 注册成功数。
    pub register_succeeded: u64,
    /// 心跳成功数。
    pub heartbeat_succeeded: u64,
    /// INVITE 成功(建流)数。
    pub invite_succeeded: u64,
    /// 当前活跃推流路数。
    pub active_streams: u64,
}

impl Metrics {
    /// 注册成功率(0.0~1.0)。尝试数为 0 时返回 0。
    pub fn register_success_rate(&self) -> f64 {
        if self.register_attempted == 0 {
            0.0
        } else {
            self.register_succeeded as f64 / self.register_attempted as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 成功率计算() {
        let m = Metrics { register_attempted: 200, register_succeeded: 150, ..Default::default() };
        assert!((m.register_success_rate() - 0.75).abs() < 1e-9);
        assert_eq!(Metrics::default().register_success_rate(), 0.0);
    }
}
