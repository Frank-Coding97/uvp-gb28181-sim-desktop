# 功能规格:订阅运行详情

**状态:已交付(M7)** · 覆盖 FR-49 / FR-50 · 实现见 `apps/desktop`(Dashboard.vue 订阅卡片 / Settings.vue 硬件版本)+ `crates/gb28181-simulator`(运行期元数据)+ `crates/gb28181-protocol`(PTZ 六字段)。

---

## 1. 目标

首页四张订阅卡片过去只有「已订阅 / 未订阅」两态。实际联调中最常见的三类问题都不在这两态里：

1. 平台订阅**到期没续订**，设备早就停止上报，界面却还亮着「已订阅」。
2. 通知**一直在失败**（平台地址错、端口不通），界面只有计数不涨，看不出原因。
3. PTZ 精准位置只报了 `Pan/Tilt/Zoom`，平台按规范要六字段（含视场角与最大可视距离）才算得出覆盖范围。

这一批把订阅从「有个状态灯」补成「能排障的运行详情」，并把 PTZ 报文补到六字段。

## 2. 订阅生命周期

| 取值 | 含义 | 界面表现 |
|---|---|---|
| `not_subscribed` | 从未订阅 | 灰底「未订阅」 |
| `subscribed` | 活跃且有效期未走完 | 绿底「剩余 59分30秒」 |
| `expired` | 仍在活跃态但有效期已走完（平台未续订） | 橙底「已过期」 |
| `cancelled` | 曾订阅，已退订或设备下线 | 灰底「已退订」 |
| `exception` | 最近一次通知失败且当前无活跃对话 | 橙底「上报异常」 |

`expired` 的判定必须同时看「是否活跃」和「剩余秒数」：只看 `active` 会把过期订阅显示成正常。

## 3. 展示字段

| 字段 | 来源 | 说明 |
|---|---|---|
| 剩余秒数 | `min(Expires - 已过时间, 0)` 饱和 | 本地每秒递减，到 0 即时翻成「已过期」 |
| 累计上报 | 各订阅的原子计数 | 卡片副行「已上报 N 次」 |
| 订阅者 | SUBSCRIBE 的来源地址 | 悬停显示 `host:port` |
| 有效期 | SUBSCRIBE 的 `Expires` | 悬停显示秒数 |
| 最近应答 | 最近一次成功的 SIP 状态码 | 悬停显示，如 `200` |
| 最近错误 | 最近一次失败原因 | 悬停显示；下一次成功后**清空** |
| PTZ 六字段 | 当前精确姿态 | 卡片副行显示变焦倍数与水平视场角，悬停显示全部六项 |

## 4. PTZ 六字段（GB/T 28181-2022 A.2.6.15）

规范六项都是 `minOccurs="0"`，但设备有值就必须全报：平台侧靠光学三量换算实际覆盖范围，缺项会导致覆盖面积算不出来。

- 三轴姿态：`Pan` / `Tilt` / `Zoom`。
- 光学三量**不是独立读数**，而是有效变焦的函数：
  - `HorizontalFieldAngle = 60° ÷ zoom`
  - `VerticalFieldAngle = 36° ÷ zoom`
  - `MaxViewDistance = 300 m × zoom`
- 换算只写在 `gb28181_protocol::ptz::PtzOptics`，报文与 UI 快照共用，避免「报 3 倍变焦却带 1 倍视场角」。
- `zoom < 1` 或非有限值一律按 1 倍处理（规范要求 `Zoom >= 1.00`，脏数据不该让视场角膨胀到 120°）。

查询应答与事件通知使用同一组六字段，区别只在根元素：`<Response>` 用于平台单次查询，`<Notify>` 用于姿态变化时的事件上报。

## 5. 事件与快照的分工

`subscription_state` 事件只带 `kind` / `active` / `notify_count` —— 位置订阅每几秒一条，事件里塞全量详情没必要。

因此前端收到事件后：

1. 只合并 `active` 与 `notify_count`（**不能整体覆盖**，否则 `lifecycle` / `remaining_secs` 会被写成 `undefined`）。
2. 300 ms 合并窗口结束后回读一次 `get_device_subscriptions` 取详情。
3. 两次回读之间由本地秒级倒数维持「剩余时间」的观感。

## 6. 硬件版本（FR-50）

DeviceInfo 应答的 `HardwareVersion` 是 GB-2022 新增字段。设置页「DeviceInfo 应答字段」区可编辑：

- 长度 ≤ 64 字符；拒绝 `<` `>` `&`（该值直接拼进 XML，不挡住会破坏报文体）。
- **仅 GB-2022 输出**：选 2016 时后端不填该字段，避免 2016 平台解析异常。
- **留空不输出空节点**：宁可缺标签，也不要 `<HardwareVersion></HardwareVersion>`。

## 7. 验收

- `cargo test -p gb28181-protocol`：六字段换算与报文断言。
- `cargo test -p gb28181-simulator --lib subscription`：剩余秒数饱和、生命周期切换、异常清除。
- `node --test "src/**/*.test.mjs"`：`subscription-detail-contract` / `hardware-version-contract` / `settings-effective-config`。
