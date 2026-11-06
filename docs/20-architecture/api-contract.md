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

### 订阅运行详情(FR-49)

| 命令 | 入参 | 返回 | 说明 |
|---|---|---|---|
| `get_device_subscriptions` | — | `SubscriptionSnapshot[]` | 四类订阅的运行详情基线(进入页面/事件后回读) |

```ts
type SubscriptionLifecycle =
  | "not_subscribed"   // 从未订阅
  | "subscribed"       // 活跃且未过期
  | "expired"          // 活跃但有效期已走完(平台未续订)
  | "cancelled"        // 曾订阅后主动退订或设备下线
  | "exception";       // 通知失败且当前无活跃对话

interface SubscriptionSnapshot {
  kind: "MobilePosition" | "Catalog" | "Alarm" | "PTZPosition";
  active: boolean;
  lifecycle: SubscriptionLifecycle;
  subscriber: string | null;      // 平台地址 host:port
  expires_secs: number;           // 平台 SUBSCRIBE 携带的 Expires
  remaining_secs: number;         // 剩余秒数，饱和到 0
  notify_count: number;
  last_notify_at_ms: number | null;
  last_notify_result: string | null;  // 最近一次成功的 SIP 状态码
  last_error: string | null;          // 最近一次失败原因
  ptz_position: {                     // 仅 PTZPosition 有值
    pan: number; tilt: number; zoom: number;
    horizontal_field_angle: number;
    vertical_field_angle: number;
    max_view_distance: number;
  } | null;
}
```

> `subscription_state` 事件**只带 `kind`/`active`/`notify_count`**。前端收到事件后必须回读一次
> `get_device_subscriptions` 才拿得到详情,不能把事件载荷整体覆盖到本地状态上 —— 否则
> `lifecycle`/`remaining_secs` 会被写成 `undefined`。

> PTZ 六字段中的光学三量(`HorizontalFieldAngle`/`VerticalFieldAngle`/`MaxViewDistance`)
> 不是独立读数,而是变焦倍数的函数(基准 60°/36°/300 m,除以或乘以有效变焦),
> 报文与快照共用 `gb28181_protocol::ptz::PtzOptics` 同一套换算,保证两边一致。


### 设备校时命令(FR-46)

| 命令 | 入参 | 返回 | 说明 |
|---|---|---|---|
| `get_clock_state` | — | `ClockStateDto` | 生效偏移/来源/凭证/SIP 回退值 + NTP 配置 + 任务运行态 + `config_revision` |
| `save_time_sync` | `{ time_sync: TimeSyncSettings, expectedRevision: number }` | `DesktopConfigV2` | 保存 NTP 配置;设备运行中允许修改,后端立即重启刷新任务;关闭时回退 SIP Date |
| `sync_ntp_now` | — | `NtpProbeDto` | 手动执行一次 SNTP 查询并立即生效,不依赖设备是否在运行 |

```ts
interface TimeSyncSettings {
  ntp_enabled: boolean;          // 默认 false
  ntp_server: string;            // 默认 ntp.aliyun.com
  ntp_port: number;              // 默认 123
  refresh_interval_secs: number; // 默认 600
}

interface ClockSnapshot {
  offset_secs: number;        // 平台时间 − 本地时间
  source: "none" | "sip_date" | "ntp";
  detail: string;             // 原始 Date 头 或 "NTP host:port · RTT nms"
  synced_at_ms: number;
  sip_offset_secs: number;    // 最近一次 SIP Date 偏移(回退值)
  sip_synced: boolean;
  ntp_active: boolean;
  last_error: string | null;
}
```

> 语义约束:NTP 生效期间 `sync_from_date_header` 只更新 `sip_offset_secs`,不覆盖 `offset_secs`;NTP 失败时回退到 `sip_offset_secs`,无可回退值时保持原偏移。

### 录像计划命令(FR-48)

| 命令 | 入参 | 返回 | 说明 |
|---|---|---|---|
| `get_record_plan` | — | `RecordPlanStateDto` | 计划 + `config_revision` + 调度运行态 + 当前周几/秒偏移 + 此刻是否在时段内 + 录像状态 |
| `save_record_plan` | `{ plan: RecordPlanSettings, expectedRevision: number }` | `DesktopConfigV2` | 保存并归一化（排序/合并/裁剪）；运行中的设备立即生效，无需重启 |

```ts
interface RecordPlanSettings {
  enabled: boolean;                 // 默认 false
  week: { days: Array<{ segments: Array<{ start_sec: number; end_sec: number }> }> }; // 7 天，周一在前
}
```

> 语义约束:时段左闭右开 `[start_sec, end_sec)`，相邻段自动合并，每天上限 8 段；`end_sec` 最大 86400。调度每 10s 判定一次，只停「计划自己起的」录像，手动录像不受影响；设备停止时先取消调度再收尾录像。`ConfigDownload/VideoRecordPlan` 的 `RecordPlanType` 随实际生效状态在 0（定时）/2（无定时计划）之间切换。

### 报警单命令(FR-9)

| 命令 | 入参 | 返回 | 说明 |
|---|---|---|---|
| `fire_alarm` | `{ description: string }` | `string` | 首页快捷入口，等价于默认视频报警单；同样写入历史 |
| `report_alarm` | `{ report: AlarmReport }` | `AlarmRecord` | 按报警单上报；成功/失败都写入历史，失败时向上返回错误 |
| `preview_alarm_xml` | `{ report: AlarmReport }` | `string` | 与实际发送共用 `to_notify`，SN 固定为 1，便于核对报文 |
| `list_alarm_history` | — | `AlarmRecord[]` | 最近 50 条，最新在前 |
| `clear_alarm_history` | — | `void` | 清空内存历史（不落盘） |
| `reset_alarm` | — | `string` | 只清设备本地 `alarming` 标记，不向平台发报文 |

```ts
interface AlarmReport {
  priority: number;            // 1-4
  method: number;              // 1-7
  alarm_type?: number | null;  // 1-5，null 表示不输出 <Info>
  alarm_type_param?: string | null;
  description?: string | null;
  longitude?: number | null;   // -180~180
  latitude?: number | null;    // -90~90
  minutes_ago?: number;        // 0~10080，只改 AlarmTime 不延迟发送
}

interface AlarmRecord extends Required<AlarmReport> {
  id: number;
  device_id: string;
  alarm_time: string;       // 报文里实际写入的时间
  captured_at_ms: number;   // 落库时刻
  via_notify: boolean;      // true=订阅对话内 NOTIFY，false=独立 MESSAGE
  status: number | null;    // 平台响应码；失败为 null
  error: string | null;
}
```

> 语义约束:`AlarmType` / `AlarmTypeParam` 必须放在 `<Info>` 容器内(A.2.5.1)；`alarm_type` 为空时整段 `<Info>` 不输出，兼容只认 2016 字段的平台。经纬度以字符串进 XML(最多 6 位小数、去尾零)，避免 `1.0→1` 与 `-0` 这类平台解析告警。

### 抓拍与日志导出命令(FR-47 / FR-44)

| 命令 | 入参 | 返回 | 说明 |
|---|---|---|---|
| `take_snapshot` | — | `SnapshotMeta` | 取预览总线**最新一帧** JPEG 落盘;未出帧时返回「预览尚未出帧」错误,不写黑图 |
| `list_snapshots` | `{ limit?: number }` | `SnapshotMeta[]` | 按 `captured_at_ms` 倒序,`limit` 默认 60、上限 500 |
| `read_snapshot` | `{ id: string }` | `string` | 返回 `data:image/jpeg;base64,…`,不依赖 assetProtocol scope |
| `delete_snapshot` | `{ id: string }` | `void` | 删除单张 |
| `export_snapshot` | `{ id: string, targetPath: string }` | `string` | 原文件复制到用户选定路径,不重新编码 |
| `export_logs` | `{ tab: "sip" \| "system" \| "commands", blocks: string[], targetPath: string }` | `LogExportResult` | 渲染 + 脱敏 + 写文件;`blocks` 为空直接拒绝 |

```ts
interface SnapshotMeta {
  id: string;            // 文件名去扩展名,同时是 ID 白名单校验对象
  file_name: string;
  path: string;
  byte_len: number;
  captured_at_ms: number; // 帧被切出的时刻
  saved_at_ms: number;    // 落盘时刻(可能晚于抓拍时刻)
  width: number | null;   // 来自 JPEG SOF 解析,解析失败为 null
  height: number | null;
}

interface LogExportResult {
  path: string;
  byte_len: number;
  redacted: number;   // 实际替换的敏感字段数
  line_count: number; // 导出条目数
}
```

> 脱敏约束:脱敏在 Rust 侧 `common::log_redact` 完成,前端只递原始文本块。覆盖 `Authorization` / `Proxy-Authorization` 头、`<Password>` 标签、`password=` 键值与 `"password"` JSON 字段;`DeviceID`、`Call-ID`、`CSeq` 等排障主键**不脱敏**。导出文件头固定写明「已脱敏、不可恢复」。
>
> 抓拍 ID 只接受 `[A-Za-z0-9_-]`,用于阻断 `../` 越界读取;同毫秒重复抓拍自动追加序号,不覆盖。

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
| `subscription_state` | `{ kind, active, notify_count }` | 订阅建立/退订/每次上报时 | 订阅卡片点亮与计数;详情需另调 `get_device_subscriptions`(§2) |
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
