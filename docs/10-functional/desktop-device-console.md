# 功能规格:单设备联调控制台

**状态:开发中(M4)** · 覆盖 UC-1(单设备联调)· 实现见 `apps/desktop`(前端 Device.vue + 后端 IPC)。

---

## 1. 目标

桌面版最直观的价值页:填平台与设备参数 → 一键让一台虚拟 IPC 注册到上级平台 → 实时看到"在线"状态 → 可主动上报报警、被平台点播出流。面向"没有真摄像头也要联调国标平台"的开发者(vision 目标用户)。

## 2. 页面元素

| 区域 | 内容 |
|---|---|
| 平台配置 | server_host、SIP 端口、SIP 域(domain)、传输(UDP/TCP)、GB 版本(2016/2022) |
| 设备配置 | device_id、密码(眼睛图标可切换明文/掩码)、通道名、视频源文件(可空,填则点播出画面;支持 H.264/H.265 裸流及 MP4/FLV/MKV/MOV 容器,容器需系统 ffmpeg) |
| 操作 | 「注册」「注销」按钮(注销与设备实例存在与否挂钩,注册失败/重试中也可点停止);「上报报警」「上报 GPS」按钮(注册后可用) |
| 状态灯 | 实时显示:未连接/注册中/已注册/推流中/失败(订阅 `device_state` 事件,由常驻 App 统一维护,切页不丢) |
| 云台可视化 | 平台下发 PTZ 时,拟态摇杆随方向/变倍推移、预置位调用平滑巡航到目标位;显示当前朝向/速度/状态(订阅 `ptz_action`/`ptz_preset` 事件) |
| SIP 信令追踪(FR-43) | 实时滚动收发报文(方向/首行摘要/CSeq,默认开,可开关/清空;订阅 `sip_trace` 事件,`set_sip_trace` 开关) |
| 提示 | 注册成功/失败、报警/位置上报结果的 toast |

## 3. 交互流程(对应 IPC,见 `20-architecture/api-contract.md`)

1. 填表 → 点「注册」→ 前端调 `start_device({config})`。
2. 后端构造 `DeviceConfig`,启动设备 run 循环(注册+心跳+入站应答),后台常驻。
3. 后端在设备状态变化时 emit `device_state` 事件 → 前端状态灯实时更新。
4. 「上报报警」→ `fire_alarm({description})` → 设备发 Alarm Notify;「上报 GPS」→ `fire_position({longitude, latitude})` → 发 MobilePosition。
5. 平台下发 PTZ/预置位 → 后端 emit `ptz_action`/`ptz_preset` → 前端云台摇杆动画。
6. 信令追踪开关 → `set_sip_trace({enabled})`;开启时后端 emit `sip_trace` 事件流。
7. 「注销」→ `stop_device()` → 停止 run 循环。

## 4. 与压测页的关系

单设备页 = 压测页 count=1 的联调特化,但强调**实时状态可视 + 主动业务触发**,而非吞吐指标。两页共用 `gb28181-simulator` 的 `DeviceSimulator`。

## 5. 验收

- 填入真实 WVP 参数(见 `40-protocol/gb28181-联调.md`)→ 点注册 → 状态灯变「已注册」→ WVP 控制台显示设备在线。
- 点「上报报警」→ WVP 收到报警(平台侧可见)。
- 配置视频源后,平台点播 → 状态灯变「推流中」→ 平台出画面。
