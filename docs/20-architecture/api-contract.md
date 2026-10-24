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
| `start_stress` | `{ toml, count, profile_id, position_interval, alarm_interval }` | `string` | 启动压测。**TOML 里不含密码**,由 `profile_id` 从钥匙串取;position/alarm_interval>0 时批量周期主动上报定位/报警(0=关) | ✅ |
| `stop_stress` | — | `string` | 停止当前压测(无入参,返回提示文案) | ✅ |
| `get_metrics` | — | `string`(JSON) | 按需查询当前 `MetricsSnapshot`(实时更新走 `metrics_tick` 事件) | ✅ |
| `get_stress_status` | — | `{ running: bool, device_count: number }` | 查压测运行态(页面切换后与引擎对账,防组件重建丢状态) | ✅ |
| `export_report` | — | `string`(文件路径) | 导出报告：脱敏场景 TOML + 设备数 + 最终快照 + **全量时序及完整性 metadata**，原子写入 app 数据目录的应用托管 `uvp-reports/` 并返回当前路径；默认按 30 天/100 文件/1 GiB 自动清理，不承诺永久保存 | ✅ |
| `save_platform_secret` | `{ profileId, password }` | `{ backend, message }` | 把平台密码写入 OS 钥匙串(见 `SECURITY.md`) | ✅ |
| `delete_platform_secret` | `{ profileId }` | — | 删除钥匙串条目 | ✅ |
| `platform_secret_status` | `{ profileIds: string[] }` | `Record<string, boolean>` | 批量查询哪些档案已配置密码(**不返回明文**) | ✅ |
| `pick_video_source` | — | `string \| null` | **由 Rust 侧弹出系统文件对话框**选视频源,并同时授权它的 asset 预览。取消返回 `null`。对话框在 Rust 侧而不是前端:授权必须绑定在"用户真的选了这个文件"这件事上 | ✅ |
| `allow_media_preview` | `{ path }` | — | 为**已授权过**的媒体文件重新放行 asset 预览(应用重启后恢复上次选的文件)。只接受 `pick_video_source` 记下的路径 —— 之前只检查 `is_file()` 就放行,等于把收窄后的 asset scope 又整体打开(任何能执行 JS 的途径都能读任意文件) | ✅ |
| `export_diagnostics` | — | `string`(zip 路径) | 导出诊断包：滚动日志 + 运行环境快照（版本 / CPU / **fd 限额** / ffmpeg 可用性 / 产物保留策略）+ 隐私说明 README。同目录 staging 完成后原子发布到应用托管 `uvp-reports/`，返回当前路径并受同一自动清理策略管理；与压测报告分开：报告答“结果如何”，诊断包答“这台机器上发生了什么” | ✅ |
| `get_run_status`(规划,未实现) | `{ run_id: string }` | `RunStatus` | 查询某次运行状态 | ⬜ |
| `list_runs`(规划,未实现) | — | `RunSummary[]` | 历史运行列表 | ⬜ |
| `save_scenario`(规划,未实现) | `{ name: string, toml: string }` | `{ id: string }` | 保存场景 | ⬜ |
| `list_scenarios`(规划,未实现) | — | `ScenarioMeta[]` | 场景列表 | ⬜ |

### 单设备联调命令(桌面版核心)

| 命令 | 入参 | 返回 | 说明 | 里程碑 |
|---|---|---|---|---|
| `start_device` | `{ config: DeviceCfg }` | `string` | 启动一台设备(注册+心跳+应答+入站),后台常驻。`DeviceCfg` **无 password 字段**,只带 `profile_id`;`transport` 支持 UDP / TCP；`inbound_identity_mode` 支持 `compatible` / `strict` | ✅ |
| `get_inbound_security_capabilities` | — | `InboundSecurityCapabilities` | 报告可信来源 IP、From 平台 ID、设备注册 Digest 与平台反向 Digest 的真实能力边界 | ✅ |
| `stop_device` | — | `string` | 停止当前设备 | ✅ |
| `get_device_status` | — | `{ running: bool }` | 查设备运行态(页面切换后与引擎对账) | ✅ |
| `fire_alarm` | `{ description: string }` | `string` | 主动上报一条报警 | ✅ |
| `fire_position` | `{ longitude: number, latitude: number }` | `string` | 主动上报一条 GPS 位置 | ✅ |
| `get_broadcast_status` | — | `BroadcastSnapshot \| null` | 查当前语音广播会话(§9.12.1)。**设备未启动或无会话返回 `null` 而不是报错** —— 界面就该显示"空闲" | ✅ |
| `hangup_broadcast` | — | `string` | 设备侧主动挂断语音广播,按附录 K a) 发会话内 BYE | ✅ |
| `get_media_telemetry` | — | `MediaTelemetrySnapshot \| null` | 查当前推流会话的发送统计 + 平台回的 RTCP 接收报告(丢包率/累计丢包/抖动/RTT)+ **本会话采集侧丢弃增量**(`capture_dropped_frames` / `capture_resync_bytes`,用于区分"本机供不上帧"与"网络丢包")。**无会话或发送通道未就绪返回 `null`**;`feedback.reports == 0` 表示平台一条 RR 都没回,与"回报 0% 丢包"是两种不同状态 | ✅ |

> 当前设备状态**只经 `device_state` 事件**推送(见 §3),没有同名查询命令。
>
> 广播状态反过来**只经命令轮询**(前端 500 ms 一次),不推事件:收流统计每 20 ms 变一次,
> 推成事件等于给 webview 灌洪水。会话的建立/结束本身有 `platform_command` 事件。

`DeviceCfg` JSON 字段:`server_host`/`server_port`/`server_domain`/`device_id`/`profile_id`/`transport`("UDP"/"TCP")/`inbound_identity_mode`("compatible"/"strict",缺省 compatible)/`gb_version`("2016"/"2022")/`channel_name`/`video_source`(可空,C 档文件路径或 `live:` URI)/`video_fps`(1–120,默认 25)。

`InboundSecurityCapabilities` 固定显式报告:`source_ip_filter=true`、`from_platform_id=true`、
`digest_client_registration=true`、`digest_server_validation=false`、`literal_platform_ip_only=true`。
严格模式不得在 UI 或诊断信息中表述为双向 Digest。

`BroadcastSnapshot` JSON 字段:

```jsonc
{
  "source_id": "34020000002000000001",  // 语音流发送者 ID
  "local_port": 41234,                  // 本端收流端口
  "call_id": "…",                       // 会话 Call-ID
  "recv": {
    "packets": 250, "payload_bytes": 40000,
    "lost": 0, "reordered": 0, "malformed": 0,
    "payload_type": 8,                  // 8=PCMA、0=PCMU;尚未收到为 null
    "level_pct": 37,                    // 音量百分比(G.711 解码后的峰值)
    "idle_ms": 20                       // 距上次收包毫秒数;尚未收到为 null
  }
}
```

### 2.1 统一错误契约

所有可能失败的 command 在 Tauri `Err` 通道序列化为 JSON 对象，不再返回裸字符串：

```ts
interface IpcError {
  code: string;       // 稳定机器码；UI 分支只能依赖此字段
  message: string;    // 已清洗、可安全展示的公开原文
  details?: string;   // 可选补充；与 message 重复时省略
  retryable?: boolean;
  action?: string;    // 后端掌握明确处置动作时提供
}
```

可选字段为 `None` 时不进入 JSON。UI 的建议优先级为 `action` → `code` 映射 → 一个版本兼容的旧文本规则；未知 code/字符串不得编造建议。主要稳定 code：

- 领域错误：`config.invalid`、`sip.protocol`、`sip.timeout`、`sip.rejected.auth`、`sip.rejected.busy`、`sip.rejected`、`gb28181.protocol`、`media.error`。
- I/O：`io.address_in_use`、`io.permission_denied`、`io.not_found`、`io.storage_full`、`io.error`。
- 生命周期：`device.not_running`、`device.already_running`、`stress.not_running`、`stress.already_running`。
- 凭据与产物：`credential.missing`、`credential.invalid`、`credential.store`、`report.no_data`、`report.export_failed`、`diagnostics.export_failed`。
- 媒体授权：`media.access`、`media.preview_denied`、`media.dialog_failed`、`media.path_invalid`。
- 无法稳定分类的兼容边界：`internal.error`；该 code 默认没有 `action`。

`message/details` 禁止包含密码、Authorization/Proxy-Authorization/Digest 原始内容或 FFmpeg stderr；这些内容只可在经过既有日志安全策略处理后进入受控诊断链路。`Debug` 输出不得直接作为 IPC `details`。

前端 `normalizeIpcError` 在一个版本内兼容旧裸字符串、`Error`、JSON-string DTO 与普通对象；兼容层不改变 command 成功 DTO。

---

## 3. 事件(Event)

引擎主动推送,UI 订阅。

| 事件名 | 负载 | 频率 | 说明 |
|---|---|---|---|
| `metrics_tick` | `MetricsSnapshot`(JSON 字符串) | 1 Hz | 实时指标,喂 ECharts |
| `device_state` | `string`(Disconnected/Registering/Registered/InCall/Failed) | 状态变更时 | 单设备联调状态灯 |
| `device_error` | `{ scope: string, error: IpcError, ts_ms: number }` | 运行时错误 | 单设备运行时错误。前端兼容旧 `{ scope, message, ts_ms }` 一个版本 |
| `stress_state` | `{ run_id, running, stopping, reason?, error?: IpcError, device_count?, metrics? }` | 压测生命周期变更时 | `reason=failed` 时 error 为对象；前端兼容旧 string 一个版本 |
| `sip_trace` | `SipTraceEntry` | 可开关 | 结构化 SIP 报文(大规模默认关) |
| `ptz_action` | `{ up, down, left, right, zoom_in, zoom_out, pan_speed, tilt_speed, zoom_speed }` | PTZ 控制到达时 | 前端云台动画 |
| `ptz_preset` | 预置位号(number) | 预置位调用时 | 前端预置位调用演示 |
| `log_batch` | `{ lines: LogLine[], dropped: number }` | 每 100ms(有日志时) | 日志页。**批量聚合**,取代原来每条一个 `log_line` 事件 |
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
