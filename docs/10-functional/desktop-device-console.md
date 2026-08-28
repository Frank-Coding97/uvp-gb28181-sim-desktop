# 功能规格:单设备联调控制台

**状态:开发中(M4)** · 覆盖 UC-1(单设备联调)· 实现见 `apps/desktop`(前端 `views/Device.vue` + `components/` 面板 + 后端 IPC)。

---

## 1. 目标

桌面版最直观的价值页:填平台与设备参数 → 一键让一台虚拟 IPC 注册到上级平台 → 实时看到"在线"状态 → 可主动上报报警、被平台点播出流。面向"没有真摄像头也要联调国标平台"的开发者(vision 目标用户)。

## 2. 页面元素

平台连接参数(server_host / 端口 / 域 / 传输 / 信令编码)在**全局顶栏**的平台档案里,SIP 密码存 OS 钥匙串、前端不持有明文;注册状态灯与在线时长也在顶栏,所以本页不重复展示这些内容。

| 区域 | 内容 |
|---|---|
| 视频预览 | 本地画面回显:文件走 asset 协议(仅浏览器可解的容器),摄像头/屏幕走 WebView 的 getUserMedia / getDisplayMedia;支持"适应/铺满"切换 |
| 云台控制 | 平台下发 PTZ 时 3D 球机随方向/变倍联动,预置位调用平滑巡航;显示朝向(累计 pan)/俯仰/变倍/速度/状态(订阅 `ptz_action`、`ptz_preset`) |
| 设备身份与国标参数 | device_id、GB 版本(2016/2022)、通道名、目录模板、厂商/型号/固件、Expires、心跳周期与超时次数 |
| 媒体源 | 四种模式:视频文件 / 屏幕(ScreenCaptureKit)/ 摄像头(AVFoundation,含 iPhone 连续互通)/ 无媒体;实时源可「刷新设备」枚举、「测试采集」自检 |
| 主动上报 | 「上报报警」「上报 GPS」按钮(注册后可用) |
| 平台实时状态 | 活跃订阅(目录/报警/位置及其 NOTIFY 计数)、平台下发的 OSD 配置、长任务(抓拍/升级)进度 |
| 平台命令时间线 | 后端每收到一条平台命令推一条语义事件(查询/控制/点播/广播…) |
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
7. 「注销」→ `stop_device()` → 停止 run 循环。

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
| `components/SipTracePanel.vue` | SIP 信令追踪 |
| `composables/useLiveSources.ts` | 媒体源状态与 `live:` URI 组装、设备枚举、采集自检 |
| `composables/useMediaPreview.ts` | 本地预览(asset 协议 / WebView 媒体流) |
| `composables/usePtzPose.ts` | PTZ 姿态积分与预置位巡航 |
| `composables/usePlatformRuntime.ts` | 命令时间线 / 订阅 / 进度 / OSD |
| `composables/useSipTrace.ts` | 信令追踪缓冲与过滤 |

跨面板复用的页头、表单与面板原语放在全局 `src/theme.css`(scoped 样式不跨组件边界),各页面若需微调用 scoped 规则覆盖。

## 5. 与压测页的关系

单设备页 = 压测页 count=1 的联调特化,但强调**实时状态可视 + 主动业务触发**,而非吞吐指标。两页共用 `gb28181-simulator` 的 `DeviceSimulator`。

## 6. 验收

- 填入真实 WVP 参数(见 `40-protocol/gb28181-联调.md`)→ 点注册 → 状态灯变「已注册」→ WVP 控制台显示设备在线。
- 点「上报报警」→ WVP 收到报警(平台侧可见)。
- 配置视频源后,平台点播 → 状态灯变「推流中」→ 平台出画面。
- 改分辨率/码率/编码/GOP/帧率后重新点播,平台侧码流参数随之变化(可用 `ffprobe` 抓平台侧转发流确认)。
