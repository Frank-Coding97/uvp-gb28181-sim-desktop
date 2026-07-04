# crate: stress-engine

**状态:M3 核心完成** · 压测编排 + 指标 + CLI。lib + bin 双形态。

> 进度:`Orchestrator`(批量创建设备 + 共享单 UDP 传输并发拉起 + broadcast 优雅停止)✅ ·
> CLI(TOML 场景 + 设备数,Ctrl-C 停止)✅ · 集成测试 8 设备批量注册 ✅ ·
> `Metrics`(注册/心跳/推流 + 批量定位/报警上报计数)+ 每秒 `MetricsSnapshot` ✅ ·
> **批量主动上报施压**:`run(tx, position_secs, alarm_secs)` 周期让每台设备上报定位/报警,计入 `position_reported`/`alarm_reported` ✅。
> 待补:爬坡速率精细控制。

## 职责

组合 scenario 与 gb28181-simulator,批量拉起虚拟设备、采集指标、对外提供控制接口。既被 Tauri 内嵌调用,也可作独立二进制(CLI / 未来分布式 Agent)运行。

## 公开 API(已实现)

```rust
pub struct Orchestrator { pub metrics: Arc<Metrics>, /* ... */ }
impl Orchestrator {
    pub fn new(scenario: &dyn Scenario, count: usize) -> Result<Self>;
    // 并发拉起 count 台设备;position_secs/alarm_secs>0 时每台周期主动上报定位/报警(0=关);
    // tx 为 broadcast 停止信号。
    pub async fn run(&self, tx: broadcast::Sender<()>, position_secs: u64, alarm_secs: u64) -> Result<()>;
}
pub struct Metrics { /* 原子计数 */ }   // + snapshot() / register_success_rate()
```

## 调度
- 共享单 UDP 传输,并发 spawn `DeviceSimulator`(FR-20/21)。
- 按 `active_ratio` 抽样推流(FR-24)。
- broadcast 通道优雅停止全部设备。
- **批量主动上报施压**:`position_secs`/`alarm_secs` 控制每台设备周期上报定位/报警,计入指标。

## 指标
- `Metrics`:原子计数,热路径无锁;`snapshot()` → `MetricsSnapshot`(见 `20-architecture/api-contract.md#4`),含 `position_reported`/`alarm_reported`。
- **不落库**(无 SQLite,规划中,见 `20-architecture/data-model.md`);实时更新经 `metrics_tick` 事件推给 UI。
- 失败归因分类见 `20-architecture/data-model.md#4`。

## 对外服务
- 内嵌模式:被 Tauri command 直接调用(当前形态)。
- 独立模式暴露本地 HTTP + WebSocket:**规划中,未实现**。

## 二进制
`stress-engine <file.toml> <count>`:加载 TOML 场景 → 跑调度 → 指标输出(NFR-8 / UC-5)。

## 错误
`common::Result`;单设备失败计入指标不影响整体(NFR-5)。

## 依赖
common、scenario、gb28181-simulator、sip-core、tokio、tracing、serde。

## 里程碑
- M0:骨架 + 成功率单测(已实现)。
- M3:真实并发拉起、指标聚合、批量主动上报施压(已实现)。

## 测试
成功率计算(已有);调度爬坡速率(模拟时钟);指标聚合并发正确性;失败归因分类。
