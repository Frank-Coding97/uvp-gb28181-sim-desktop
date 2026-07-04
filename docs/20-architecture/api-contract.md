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
| `validate_scenario` | `{ toml: string }` | `ScenarioSummary` | 校验并预览场景 | ✅ |
| `start_stress` | `{ toml, count, position_interval, alarm_interval }` | `string` | 启动压测;position/alarm_interval>0 时批量周期主动上报定位/报警(0=关) | ✅ |
| `stop_stress` | — | `string` | 停止当前压测(无入参,返回提示文案) | ✅ |
| `get_metrics` | — | `string`(JSON) | 按需查询当前 `MetricsSnapshot`(实时更新走 `metrics_tick` 事件) | ✅ |
| `get_stress_status` | — | `{ running: bool, device_count: number }` | 查压测运行态(页面切换后与引擎对账,防组件重建丢状态) | ✅ |
| `export_report` | — | `string`(文件路径) | 导出报告:把场景 TOML + 设备数 + 最终快照写成 JSON 到系统临时目录,返回路径 | ✅ |
| `get_run_status`(规划,未实现) | `{ run_id: string }` | `RunStatus` | 查询某次运行状态 | ⬜ |
| `list_runs`(规划,未实现) | — | `RunSummary[]` | 历史运行列表 | ⬜ |
| `save_scenario`(规划,未实现) | `{ name: string, toml: string }` | `{ id: string }` | 保存场景 | ⬜ |
| `list_scenarios`(规划,未实现) | — | `ScenarioMeta[]` | 场景列表 | ⬜ |

### 单设备联调命令(桌面版核心)

| 命令 | 入参 | 返回 | 说明 | 里程碑 |
|---|---|---|---|---|
| `start_device` | `{ config: DeviceCfg }` | `string` | 启动一台设备(注册+心跳+应答+入站),后台常驻 | ✅ |
| `stop_device` | — | `string` | 停止当前设备 | ✅ |
| `get_device_status` | — | `{ running: bool }` | 查设备运行态(页面切换后与引擎对账) | ✅ |
| `fire_alarm` | `{ description: string }` | `string` | 主动上报一条报警 | ✅ |
| `fire_position` | `{ longitude: number, latitude: number }` | `string` | 主动上报一条 GPS 位置 | ✅ |

> 当前设备状态**只经 `device_state` 事件**推送(见 §3),没有同名查询命令。

`DeviceCfg` JSON 字段:`server_host`/`server_port`/`server_domain`/`device_id`/`password`/`transport`("UDP"/"TCP")/`gb_version`("2016"/"2022")/`channel_name`/`video_source`(可空,C 档文件路径)。

命令失败统一返回错误对象 `{ code: string, message: string }`(code 对应 `common::Error` 变体)。

---

## 3. 事件(Event)

引擎主动推送,UI 订阅。

| 事件名 | 负载 | 频率 | 说明 |
|---|---|---|---|
| `metrics_tick` | `MetricsSnapshot`(JSON 字符串) | 1 Hz | 实时指标,喂 ECharts |
| `device_state` | `string`(Disconnected/Registering/Registered/InCall/Failed) | 状态变更时 | 单设备联调状态灯 |
| `sip_trace` | `SipTraceEntry` | 可开关 | 结构化 SIP 报文(大规模默认关) |
| `ptz_action` | `{ up, down, left, right, zoom_in, zoom_out, pan_speed, tilt_speed, zoom_speed }` | PTZ 控制到达时 | 前端云台动画 |
| `ptz_preset` | 预置位号(number) | 预置位调用时 | 前端预置位调用演示 |
| `run_state`(规划,未实现) | `{ run_id, state }` | — | 压测生命周期(当前未发出) |
| `engine_log`(规划,未实现) | `LogEntry` | — | 引擎日志转发(当前未发出) |

---

## 4. 关键数据结构(与 Rust 类型对齐)

> 完整定义见 `data-model.md` 与各 crate 规格。此处列 UI 契约字段。

```ts
// 压测实时指标快照(对应 stress_engine::Metrics 的可序列化投影)
// 实际实现(crates/stress-engine/src/metrics.rs::MetricsSnapshot)。
interface MetricsSnapshot {
  register_attempted: number;
  register_succeeded: number;
  register_failed: number;
  register_success_rate: number;   // 0.0~1.0
  heartbeat_succeeded: number;
  heartbeat_failed: number;
  active_streams: number;
  fail_timeout: number;            // 失败归因:超时
  fail_rejected: number;           // 失败归因:被拒
  fail_other: number;              // 失败归因:其它
  position_reported: number;       // 批量定位上报成功数(主动上报施压)
  alarm_reported: number;          // 批量报警上报成功数
}

interface ScenarioSummary {
  name: string;          // 取自场景 device_info.device_name
  device_count: number;  // 由前端传 count 决定,校验时返回 0
  server_host: string;
  server_port: number;
}

interface RunStatus {
  run_id: string;
  state: "starting" | "running" | "completed" | "failed" | "stopped";
  started_at_ms: number;
  metrics: MetricsSnapshot;
}

// SIP 信令追踪条目(FR-43)。单设备联调默认开;压测大规模默认关。
interface SipTraceEntry {
  ts_ms: number;              // 捕获时刻(毫秒)
  direction: "in" | "out";    // in=收到平台报文,out=发往平台
  method: string;             // 请求方法(REGISTER/MESSAGE/INVITE…)或 "SIP/2.0"(响应)
  status?: number;            // 响应状态码(仅响应)
  cseq?: string;              // CSeq 头(如 "1 REGISTER")
  call_id?: string;           // Call-ID
  peer: string;               // 对端地址 host:port
  summary: string;            // 首行摘要(请求行 / 状态行)
}
```

### 单设备信令追踪命令

| 命令 | 入参 | 返回 | 说明 | 里程碑 |
|---|---|---|---|---|
| `set_sip_trace` | `{ enabled: bool }` | `string` | 开关 `sip_trace` 事件推送(默认开;压测不启用) | M4 |

---

## 5. 契约演进规则

1. 新增命令/事件/字段**向后兼容**(加字段不删字段)。
2. 破坏性变更需在本文件标注版本并同步 UI 与引擎双方。
3. UI 线可在引擎实现前**按本契约用 mock 数据开发**;引擎线按本契约实现,双方以本文件为准。
