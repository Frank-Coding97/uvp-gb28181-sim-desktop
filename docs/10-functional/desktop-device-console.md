# 功能规格:单设备联调控制台

**状态:开发中(M4)** · 覆盖 UC-1(单设备联调)· 实现见 `apps/desktop`(前端 `views/Device.vue` + `components/` 面板 + 后端 IPC)。

---

## 1. 目标

桌面版最直观的价值页:填平台与设备参数 → 一键让一台虚拟 IPC 注册到上级平台 → 实时看到"在线"状态 → 可主动上报报警、被平台点播出流。面向"没有真摄像头也要联调国标平台"的开发者(vision 目标用户)。

## 2. 页面元素

平台连接参数(server_host / 端口 / 域 / 传输 / 信令编码 / 入站身份模式)在**全局顶栏**的平台档案里,SIP 密码存 OS 钥匙串、前端不持有明文;注册状态灯与在线时长也在顶栏,所以本页不重复展示这些内容。

平台档案、活动档案、设备表单和主题偏好使用带 `kind/schemaVersion/writtenAt/data` 的版本化
localStorage envelope。旧裸数组/对象/字符串在启动时迁移；未知字段和越界字段按白名单修复；高于当前
schema 的配置只读锁定，旧客户端不得覆盖。损坏配置先写入固定 `.corrupt_backup`（按指纹去重、最多
3 份）再恢复默认；备份失败时保留原 primary。平台备份始终脱敏，任何可识别密码都不得复制到
localStorage。迁移、修复、版本保护和存储失败会在全局显示持久、可关闭且不含原文的提示。

| 区域 | 内容 |
|---|---|
| 视频预览 | 本地画面回显:文件走 asset 协议(仅浏览器可解的容器),摄像头/屏幕走 WebView 的 getUserMedia / getDisplayMedia;支持"适应/铺满"切换 |
| 云台控制 | 平台下发 PTZ 时 3D 球机随方向/变倍联动,预置位调用平滑巡航;显示朝向(累计 pan)/俯仰/变倍/速度/状态(订阅 `ptz_action`、`ptz_preset`) |
| 设备身份与国标参数 | device_id、GB 版本(2016/2022)、通道名、目录模板、厂商/型号/固件、Expires、心跳周期与超时次数 |
| 媒体源 | 四种模式:视频文件 / 屏幕(ScreenCaptureKit)/ 摄像头(AVFoundation,含 iPhone 连续互通)/ 无媒体;实时源可「刷新设备」枚举、「测试采集」自检 |
| 主动上报 | 「上报报警」「上报 GPS」按钮(注册后可用) |
| 平台实时状态 | 活跃订阅(目录/报警/位置及其 NOTIFY 计数)、平台下发的 OSD 配置、长任务(抓拍/升级)进度 |
| 语音广播 | 平台下发语音广播(§9.12.1)后显示会话状态:语音流发送者 ID、本端收流端口、负载类型、收包/字节、丢包/乱序、距上次收包,以及**音量条**;可主动挂断(发会话内 BYE) |
| 推流质量 | 左右分栏:左边本端视角(RTP 包/字节、平均包长、已发 RTCP SR、本地发送失败),右边平台视角(RTCP RR 的丢包率、累计丢包、抖动/峰值、RTT)。**"平台没回 RR"与"平台回报 0% 丢包"显示为两种不同状态** —— 把前者画成绿灯等于假装链路已经验证过。丢包 ≥1% 或抖动 ≥50 ms 给告警;RR 中断超 12 秒单独提示 |
| 平台命令时间线 | 后端每收到一条平台命令推一条语义事件(查询/控制/点播/广播…),含"强制关键帧已下发到编码器""平台改设备配置已生效:HeartBeatInterval=15"这类结果 |
| SIP 信令追踪(FR-43) | 实时滚动收发报文(方向/摘要/CSeq),可按方向与方法过滤、点开看原始报文;默认开,可开关/清空 |

### 2.1 编码参数与实际生效路径

实时采集的编码参数**全部真实下发**到引擎,没有只做展示的控件:

| 参数 | 传递路径 | 生效范围 |
|---|---|---|
| 分辨率 / 码率 / 编码格式(H.264、H.265)/ GOP | 编进 `live:` URI 的 `width`/`height`/`bitrate`/`codec`/`gop`,由 `media_rtp::LiveSourceSpec::parse` 校验 | 摄像头与屏幕采集 |
| 帧率 | `DeviceConfig.video_fps`(不进 URI) | 摄像头、屏幕与**文件源**都生效 |
| 音频编码格式 | `live:` URI 的 `audio_codec` | 摄像头与屏幕采集 |
| 音频采样率 / 声道 / 比特率 | 不可单独设置 | 由编码格式决定:G.711A/U 固定 8 kHz 单声道 20 ms;AAC/Opus 固定 48 kHz 单声道。界面按编码格式只读展示 |

`live:` URI 规范形式(前端 `src/lib/liveSource.ts` 与 Rust `source_live.rs` 保持同一套白名单):

```
live:camera:<video_index>?audio=<none|audio_index>&audio_codec=<g711a|g711u|aac|opus>[&width=&height=&bitrate=&codec=&gop=]
live:screen:<display_id>?audio=<none|system|microphone:N>&audio_codec=<g711a|g711u|aac|opus>[&width=&height=&bitrate=&codec=&gop=]
```

`width/height/bitrate/codec` 必须成组出现(省略则整体取默认档 1280×720 / 2500 kbps / H.264),`gop` 可单独给(默认 50 帧)。GOP 在 OpenH264 屏幕编码路径上通过每 N 帧 `force_intra_frame()` 实现,在 ffmpeg 路径(摄像头、H.265 屏幕)上通过 `-g` 实现。

## 3. 交互流程(对应 IPC,见 `20-architecture/api-contract.md`)

1. 填表 → 点顶栏「注册」→ 前端调 `start_device({config})`(只传 `profile_id`,密码由 Rust 侧从钥匙串取)。
2. 后端构造 `DeviceConfig`,启动设备 run 循环(注册+心跳+入站应答),后台常驻。
3. 后端在设备状态变化时 emit `device_state` 事件 → 顶栏状态灯实时更新。
4. 「上报报警」→ `fire_alarm({description})` → 设备发 Alarm Notify;「上报 GPS」→ `fire_position({longitude, latitude})` → 发 MobilePosition。
5. 平台下发 PTZ/预置位 → 后端 emit `ptz_action`/`ptz_preset` → 前端云台动画。
6. 信令追踪开关 → `set_sip_trace({enabled})`;开启时后端 emit `sip_trace` 事件流。
7. 平台下发语音广播 → 设备回广播应答后**主动发 INVITE** 收音频;前端 500 ms 轮询 `get_broadcast_status` 刷新统计,`hangup_broadcast` 可主动挂断。
8. 平台点播 → 设备推流的同时,前端 500 ms 轮询 `get_media_telemetry`,把本端发送统计与平台回的 RTCP RR 一起显示在「推流质量」卡。
8. 「注销」→ `stop_device()` → 按附录 K b) 先拆媒体链路(发会话内 BYE)再 REGISTER Expires=0。

> 广播状态用轮询而不是事件推送:收流统计每 20 ms 就变一次,推成事件等于给 webview 灌洪水。
> 会话的建立与结束本身有 `platform_command` 事件进命令时间线,轮询只补统计与电平。

## 4. 前端结构

页面本体只做布局与事件订阅编排,状态与事件监听集中在模块级单例 composable(与 `device.ts` 的 store 模式一致):

| 文件 | 职责 |
|---|---|
| `views/Device.vue` | 布局 + 生命周期编排(启停共享事件订阅) |
| `components/DevicePreview.vue` | 视频预览卡片 |
| `components/PtzConsole.vue` | 云台控制卡片(内嵌 `ThreePtzCamera.vue`) |
| `components/DeviceIdentityPanel.vue` | 设备身份与国标参数,内含 `MediaSourcePanel.vue` |
| `components/MediaSourcePanel.vue` | 媒体模式、采集自检、主动上报;视频/音频参数列复用 `MediaVideoFields.vue`、`MediaAudioFields.vue` |
| `components/PlatformRuntimePanel.vue` | 活跃订阅 / OSD / 长任务进度 |
| `components/CommandTimeline.vue` | 平台命令时间线 |
| `components/VoiceBroadcastPanel.vue` | 语音广播会话状态与音量条 |
| `components/MediaTelemetryPanel.vue` | 推流质量:本端发送统计 + 平台 RTCP 反馈 |
| `components/SipTracePanel.vue` | SIP 信令追踪 |
| `composables/useLiveSources.ts` | 媒体源状态与 `live:` URI 组装、设备枚举、采集自检 |
| `composables/useMediaPreview.ts` | 本地预览(asset 协议 / WebView 媒体流) |
| `composables/usePtzPose.ts` | PTZ 姿态积分与预置位巡航 |
| `composables/usePlatformRuntime.ts` | 命令时间线 / 订阅 / 进度 / OSD |
| `composables/useSipTrace.ts` | 信令追踪缓冲与过滤 |
| `composables/useVoiceBroadcast.ts` | 语音广播状态轮询(500 ms)、丢流判定、挂断 |
| `composables/useMediaTelemetry.ts` | 推流遥测轮询(500 ms)、反馈可用性/质量阈值判定、数值格式化 |
| `lib/ipcError.ts` | IPC 错误的统一呈现:把后端原文映射成「出了什么事 + 下一步做什么」 |

跨面板复用的页头、表单与面板原语放在全局 `src/theme.css`(scoped 样式不跨组件边界),各页面若需微调用 scoped 规则覆盖。

### 4.1 设计令牌与可访问性约定

`src/theme.css` 是设计系统的唯一来源。除颜色/圆角/阴影外,还包含:

| 令牌组 | 用途 | 约定 |
|---|---|---|
| `--fs-xs … --fs-2xl` | 字号阶梯(11/12/13/15/18/24) | **下限就是 11px**。需要更小说明层级划分有问题;组件里不再出现裸 px 字号 |
| `--sp-1 … --sp-8` | 间距阶梯(4px 基准) | 替代散落的魔法数字 |
| `--ctl-h-sm/md/lg` | 控件高度(26/30/34) | 输入框与按钮统一走这三档,保证点击区足够 |
| `--z-base … --z-toast` | 层级 | 加浮层时按语义选,不再拍一个裸数字 |
| `--fw-*` / `--lh-*` | 字重与行高 | — |

可访问性基线(由 ESLint 的 `vuejs-accessibility` 规则集守住,见 `apps/desktop/eslint.config.js`):

- 表单控件必须有关联 label。多实例组件用 `useId()` 生成 id ——
  写死 id 在同页出现两个实例时会让屏幕阅读器把字段名读串。
- 分段控件(`.seg`)用 `role="group"` + `aria-labelledby` + 每个按钮的 `aria-pressed`;
  **一组按钮不是表单控件**,所以标题用 `<span class="fg-label">` 而不是 `<label>`。
- 所有 `<button>` 必须写 `type`(`vue/html-button-has-type`)。
- `--text-tertiary` 对背景的对比度过 WCAG AA(浅色 4.60:1、暗色 4.98:1);
  另有 `prefers-contrast: more` 分支把两级次要文字并到接近正文色,并关掉磨砂层。
- 动效尊重 `prefers-reduced-motion`(含 `ThreePtzCamera` 直接停 rAF)。

### 4.2 状态与错误呈现约定

- 每个数据面板要区分**加载中 / 就绪 / 设备未启动 / 出错**四态,并各自给出下一步动作。
  典型实现见 `views/Channels.vue` 的 `LoadState`。
- 云台卡在设备未启动时进入 idle 态(降饱和 + 蒙层说明):
  照常渲染默认读数会让人误以为在运行。
- 长耗时操作必须防重复提交。注册/注销由 `device.ts` 的 `busy` 标志把
  "IPC 已发出但未返回"这段窗口封住(`startDisabled` / `startingUp`),
  回归测试见 `src/device.test.ts`。
- 破坏性操作必须二次确认:删除通道、**删除平台档案**(会连带删钥匙串密码)、
  **载入目录模板**(整体覆盖当前目录)。
- 失败提示走 `lib/ipcError.ts` 的 `formatError()`,且 `duration: 0` 不自动消失 ——
  错误信息往往要照着改配置,一闪而过没法用。

## 5. 与压测页的关系

单设备页 = 压测页 count=1 的联调特化,但强调**实时状态可视 + 主动业务触发**,而非吞吐指标。两页共用 `gb28181-simulator` 的 `DeviceSimulator`。

## 6. 验收

- 填入真实 WVP 参数(见 `40-protocol/gb28181-联调.md`)→ 点注册 → 状态灯变「已注册」→ WVP 控制台显示设备在线。
- 点「上报报警」→ WVP 收到报警(平台侧可见)。
- 配置视频源后,平台点播 → 状态灯变「推流中」→ 平台出画面。
- 改分辨率/码率/编码/GOP/帧率后重新点播,平台侧码流参数随之变化(可用 `ffprobe` 抓平台侧转发流确认)。
- 平台下发语音广播 → 「语音广播」卡片出现会话、收包数持续上涨、说话时音量条跟着动;点「挂断」平台侧会话结束。
- 平台点播 → 「推流质量」卡出现会话、发送包数持续上涨;平台若回 RTCP RR,右栏出现丢包率/抖动/RTT,否则明确显示"平台未回报 RTCP RR"而非 0%。
- 平台改设备配置(心跳周期)→ 命令时间线出现"已生效",且后续心跳间隔按新值。
