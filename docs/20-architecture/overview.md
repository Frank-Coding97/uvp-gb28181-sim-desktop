# 架构总览

本文档是并行开发的**共同契约**：定义分层、模块边界、crate 依赖方向、关键数据流。改动模块接口前应先更新本文档。

---

## 1. 设计目标与原则

| 目标 | 落地原则 |
|---|---|
| 易用性 | UI 负责"配置向导 + 一键运行 + 可视化"，隐藏协议细节 |
| 安装简单 | Tauri 打包单安装包，无需 JVM/Python/Docker |
| 性能稳定 | 核心用 Rust + Tokio，无 GC 抖动；长时间压测资源可控 |
| 高效并发 | 单设备 = 轻量异步任务，万级设备靠调度器分批拉起 |
| 可扩展 | 引擎与 UI 解耦，为 CLI / 分布式压测预留 |

**第一原则**：协议栈、设备状态机、媒体推送、压测调度**全部在 Rust 引擎**。UI（Vue）只做展示和下发指令，绝不承载协议逻辑。

---

## 2. 分层架构

```
┌──────────────────────────────────────────────┐
│  桌面 UI      Tauri 2 + Vue 3 + Naive UI       │
│  配置向导 / 场景编排 / 实时监控 / 日志 / 报表   │
└───────────────────────┬──────────────────────┘
                        │ Tauri IPC 命令 + 事件流（JSON）
                        ▼
┌──────────────────────────────────────────────┐
│  App Core（stress-engine 对外服务层）           │
│  任务管理 / 配置 / 状态 / 报告导出 / 日志 / 指标 │
└───────────────────────┬──────────────────────┘
                        ▼
┌──────────────────────────────────────────────┐
│  GB28181 仿真引擎（gb28181-simulator）          │
│  设备状态机：注册/心跳/目录/设备信息/INVITE/    │
│  报警/录像查询                                  │
└──────────┬─────────────────────┬──────────────┘
           ▼                     ▼
┌────────────────────┐  ┌────────────────────────┐
│ SIP 栈 (sip-core)  │  │ 媒体引擎 (media-rtp)     │
│ 消息/事务/传输     │  │ RTP/PS/H264·H265/多路   │
└────────────────────┘  └────────────────────────┘
```

---

## 3. Rust crate 划分与依赖方向

依赖只能自下而上，禁止环依赖：

```
common          ← 无依赖（类型/错误/配置/工具）
sip-core        ← common
gb28181-protocol← common
media-rtp       ← common
gb28181-simulator ← common, sip-core, gb28181-protocol, media-rtp
scenario        ← common, gb28181-simulator
stress-engine   ← common, scenario, gb28181-simulator, sip-core
apps/desktop/src-tauri ← stress-engine（或通过子进程 HTTP 调用）
```

| crate | 职责 | 关键类型（规划） |
|---|---|---|
| `common` | 跨模块公共设施 | `Error`, `Result`, `DeviceId`, `Transport`, 日志初始化 |
| `sip-core` | 与 GB 无关的 SIP 协议栈 | `SipMessage`, `Request`, `Response`, `Transaction`, `SipTransport`(UDP/TCP), Digest 认证 |
| `gb28181-protocol` | MANSCDP XML + 编码规则 | `Catalog`, `DeviceInfo`, `Alarm`, `RecordInfo`, `IdCodec` |
| `media-rtp` | RTP 打包与推流 | `RtpSender`, `PsMuxer`, `FileSource`/`NoneSource`/`LightSource`, `RtpMode` |
| `gb28181-simulator` | 单设备完整状态机 | `DeviceSimulator`, `ChannelSimulator`, `PlatformSession` |
| `scenario` | 压测场景模型 | trait `Scenario`, `LinearScenario`, `MediaProfile{A,B,C}` |
| `stress-engine` | 调度 + 指标 + 服务 | `Orchestrator`, `Metrics` |

---

## 4. 进程模型

采用 **UI + 引擎双层**，二者可同进程（Tauri 直接链接 stress-engine）或分进程（引擎独立 + HTTP 控制）。第一阶段先同进程内嵌，接口按"跨进程"设计（HTTP/WS 语义），便于后续拆分：

```
第一阶段：Tauri 进程内直接调用 stress-engine（Rust 函数）
演进：stress-engine 独立二进制 + CLI
终态：桌面 Controller ── HTTP/WS ──► 多台机器上的 engine agent
```

好处：UI 崩溃不影响压测；引擎可脱离 UI 跑；为分布式压测铺路。

---

## 5. 关键数据流

**压测启动**：UI 提交场景 TOML（`from_toml_str`）→ engine 校验 → `Orchestrator` 按 `ramp_per_second` 分批创建 `DeviceSimulator` → 每设备独立 Tokio task 跑注册/心跳 → 指标聚合 → `metrics_tick` 事件推送实时统计给 UI。

**实时监控**：engine 每秒聚合 `Metrics`（注册成功数、心跳成功率、INVITE 成功率、活跃推流路数、带宽）→ `metrics_tick` 事件 → UI ECharts 曲线。

**SIP 信令查看**：可选开启抓包模式，engine 把结构化 SIP 报文经 `sip_trace` 事件推给 UI（压测大规模时默认关闭，避免刷屏）。

---

## 6. 并发与性能要点

- 每个虚拟设备是**轻量异步任务**，不占独立线程；万级设备靠 Tokio 多路复用。
- SIP UDP 收发用共享 socket + `Call-ID`/事务路由分发，避免每设备一个 socket 耗尽端口。
- 心跳、重注册用**时间轮 / 分桶定时器**，避免万级 `tokio::time::sleep` 惊群。
- RTP 推流按 `active_ratio` 只让部分设备真推流，码率/帧率受控。
- 指标用原子计数 + 无锁聚合（`dashmap`/`metrics`），热路径不加锁。

详见 [压测设计](../10-functional/stress-testing.md)。
