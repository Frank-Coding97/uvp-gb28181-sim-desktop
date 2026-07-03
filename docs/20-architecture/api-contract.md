# API 契约:UI ↔ 引擎

**状态:草案** · **这是三条 worktree 的共享边界,变更必须知会所有线。** · 关联 FR-40~FR-45。

---

## 1. 通信方式

第一阶段引擎内嵌于 Tauri 进程,UI 通过 **Tauri command(IPC)** 调用;实时指标通过 **事件流(Tauri event / 内部 channel)** 推送。接口语义**按跨进程设计**(请求/响应 + 单向推送),以便后续把引擎拆为独立进程时平滑切换到 HTTP + WebSocket。

- **命令(请求/响应)**:UI → 引擎,同步获取结果。对应未来 HTTP。
- **事件(单向推送)**:引擎 → UI,实时指标/日志。对应未来 WebSocket。

所有负载为 **JSON**,字段用 `snake_case`(Rust serde 默认),TS 侧定义等价接口。

---

## 2. 命令(Command)

| 命令 | 入参 | 返回 | 说明 | 里程碑 |
|---|---|---|---|---|
| `engine_version` | — | `string` | 引擎版本自检 | M0 ✅ |
| `validate_scenario` | `{ yaml: string }` | `ScenarioSummary` | 校验并预览场景 | M3 |
| `start_stress` | `{ scenario: Scenario }` | `{ run_id: string }` | 启动压测,返回运行 ID | M3 |
| `stop_stress` | `{ run_id: string }` | `{ ok: bool }` | 停止压测 | M3 |
| `get_run_status` | `{ run_id: string }` | `RunStatus` | 查询某次运行状态 | M3 |
| `list_runs` | — | `RunSummary[]` | 历史运行列表 | M4 |
| `export_report` | `{ run_id: string, format: string }` | `{ path: string }` | 导出报告 | M4 |
| `save_scenario` | `{ name: string, yaml: string }` | `{ id: string }` | 保存场景 | M3 |
| `list_scenarios` | — | `ScenarioMeta[]` | 场景列表 | M4 |

### 单设备联调命令(桌面版核心)

| 命令 | 入参 | 返回 | 说明 | 里程碑 |
|---|---|---|---|---|
| `start_device` | `{ config: DeviceCfg }` | `string` | 启动一台设备(注册+心跳+应答+入站),后台常驻 | M4 |
| `stop_device` | — | `string` | 停止当前设备 | M4 |
| `device_state` | — | `string`(状态枚举) | 查询当前设备状态 | M4 |
| `fire_alarm` | `{ description: string }` | `string` | 主动上报一条报警 | M4 |

`DeviceCfg` JSON 字段:`server_host`/`server_port`/`server_domain`/`device_id`/`password`/`transport`("UDP"/"TCP")/`gb_version`("2016"/"2022")/`channel_name`/`video_source`(可空,C 档文件路径)。

命令失败统一返回错误对象 `{ code: string, message: string }`(code 对应 `common::Error` 变体)。

---

## 3. 事件(Event)

引擎主动推送,UI 订阅。

| 事件名 | 负载 | 频率 | 说明 |
|---|---|---|---|
| `metrics_tick` | `MetricsSnapshot` | 1 Hz | 实时指标,喂 ECharts |
| `device_state` | `string`(Disconnected/Registering/Registered/InCall/Failed) | 状态变更时 | 单设备联调状态灯 |
| `run_state` | `{ run_id, state }` | 状态变更时 | 压测生命周期(启动中/运行/完成/失败) |
| `sip_trace` | `SipTraceEntry` | 可开关 | 结构化 SIP 报文(大规模默认关) |
| `engine_log` | `LogEntry` | 按需 | 引擎日志转发 |

---

## 4. 关键数据结构(与 Rust 类型对齐)

> 完整定义见 `data-model.md` 与各 crate 规格。此处列 UI 契约字段。

```ts
// 压测实时指标快照(对应 stress_engine::Metrics 的可序列化投影)
interface MetricsSnapshot {
  ts_ms: number;
  register_attempted: number;
  register_succeeded: number;
  register_success_rate: number;   // 0.0~1.0
  register_latency_p95_ms: number;
  heartbeat_success_rate: number;
  invite_succeeded: number;
  active_streams: number;
  send_bitrate_kbps: number;
  failures: Record<string, number>; // 失败归因 → 计数
}

interface ScenarioSummary {
  name: string;
  device_count: number;
  ramp_per_second: number;
  media_mode: "none" | "light" | "real";
  estimated_duration_secs: number;
}

interface RunStatus {
  run_id: string;
  state: "starting" | "running" | "completed" | "failed" | "stopped";
  started_at_ms: number;
  metrics: MetricsSnapshot;
}
```

---

## 5. 契约演进规则

1. 新增命令/事件/字段**向后兼容**(加字段不删字段)。
2. 破坏性变更需在本文件标注版本并同步 UI 与引擎双方。
3. UI 线可在引擎实现前**按本契约用 mock 数据开发**;引擎线按本契约实现,双方以本文件为准。
