# crate: stress-engine

**状态:M3 核心完成** · 压测编排 + 指标 + CLI。lib + bin 双形态。

> 进度:`Orchestrator`(批量创建设备 + 共享单 UDP 传输并发拉起 + broadcast 优雅停止)✅ ·
> CLI(TOML 场景 + 设备数,Ctrl-C 停止)✅ · 集成测试 8 设备批量注册 ✅ ·
> `Metrics`(注册/心跳/推流计数)结构就绪。
> 待补:实时指标聚合上报(接编排器运行态)、爬坡速率控制、HTTP/WS 服务层(M4 接 UI)。

## 职责

组合 scenario 与 gb28181-simulator,批量拉起虚拟设备、采集指标、对外提供控制接口。既被 Tauri 内嵌调用,也可作独立二进制(CLI / 未来分布式 Agent)运行。

## 公开 API(规划,M0 已有骨架)

```rust
pub struct Scheduler { /* 持 Scenario */ }
impl Scheduler {
    pub fn new(scenario: Scenario) -> Self;
    pub async fn run(&self);          // 爬坡拉起 + 生命周期管理(M3 实现)
}
pub struct Metrics { /* 计数 */ }     // 已实现 + register_success_rate()
```

## 调度(M3)
- 按 `rate_per_second` 分批 spawn `DeviceSimulator`(FR-21)。
- 时间轮/分桶定时器管理心跳与重注册,避免万级惊群(FR-22)。
- 按 `active_ratio` 抽样推流(FR-24)。

## 指标(M3)
- `Metrics`:原子计数,热路径无锁;每秒 snapshot → `MetricsSnapshot`(见 `20-architecture/api-contract.md#4`)。
- 落 SQLite `metric_sample`,推 `metrics_tick` 事件。
- 失败归因分类见 `20-architecture/data-model.md#4`。

## 对外服务(M3)
- 内嵌模式:被 Tauri command 直接调用。
- 独立模式:暴露本地 HTTP + WebSocket(命令/事件语义同 `api-contract.md`),供 CLI / 分布式。

## 二进制
`stress-engine --scenario <file.yaml>`:加载场景 → 跑调度 → 指标输出(NFR-8 / UC-5)。

## 错误
`common::Result`;单设备失败计入指标不影响整体(NFR-5)。

## 依赖
common、scenario、gb28181-simulator、tokio、tracing。M3 增:sqlite 驱动、axum/ws(独立服务时)。

## 里程碑
- M0:Scheduler/Metrics 骨架 + 成功率单测(已实现)。
- M3:真实爬坡拉起、指标聚合落库、服务层。

## 测试
成功率计算(已有);调度爬坡速率(模拟时钟);指标聚合并发正确性;失败归因分类。
