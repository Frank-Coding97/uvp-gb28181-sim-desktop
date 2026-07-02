//! 压测调度器。
//!
//! M0 仅持有场景与占位 `run`。M3 实现:按 `rate_per_second` 爬坡创建 `DeviceSimulator`,
//! 每设备独立 Tokio task;心跳/重注册用时间轮分桶避免惊群;按 `active_ratio` 抽样推流。

use scenario::Scenario;

/// 压测调度器。
pub struct Scheduler {
    scenario: Scenario,
}

impl Scheduler {
    /// 用场景创建调度器。
    pub fn new(scenario: Scenario) -> Self {
        Self { scenario }
    }

    /// 只读访问场景。
    pub fn scenario(&self) -> &Scenario {
        &self.scenario
    }

    /// 运行压测(M0 占位)。M3 实现爬坡拉起与生命周期管理。
    pub async fn run(&self) {
        tracing::info!(name = %self.scenario.name, "压测调度器启动(骨架,尚未实现拉起逻辑)");
    }
}
