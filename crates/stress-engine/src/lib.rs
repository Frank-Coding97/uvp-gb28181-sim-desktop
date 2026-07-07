//! 压测引擎:调度 + 指标 + 对外服务。
//!
//! 组合 `scenario` 与 `gb28181-simulator`,批量拉起虚拟设备并采集指标。
//! 既可被 Tauri 桌面端内嵌调用,也可作为独立二进制(CLI / 未来分布式 Agent)运行。
//!
//! - `scheduler`:编排器,批量拉起设备、生命周期管理
//! - `metrics`:实时指标采集与聚合
//! - 场景定义在独立的 `scenario` crate(可复用)
//!
//! M3 实现编排器 + 场景。对外 HTTP/WS 服务层在 M4(见 docs/architecture.md 进程模型)。

pub mod metrics;
pub mod scheduler;

pub use metrics::{Metrics, MetricsSnapshot};
pub use scenario::{LinearScenario, MediaProfile, Scenario};
pub use scheduler::Orchestrator;
