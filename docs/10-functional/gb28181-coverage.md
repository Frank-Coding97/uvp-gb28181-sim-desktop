# GB28181 功能点覆盖矩阵

**状态:核心已实现并真机验证** · 本项目实现 GB/T 28181-2022 **下级设备(IPC)侧**协议。协议流程参考开源项目 uvp-gb28181-sim(Android 版),桌面端用 Rust 独立重写。

> **对标 uvp-gb28181-sim 结论(2026-07-04 更新)**:上游 Android 版的核心能力(注册/心跳/目录/点播/PTZ 及设备控制/**预置位 CRUD**/**看守位**/GPS 上报/报警)本项目均已覆盖并真机验证,且**额外**具备上游没有的**万级压测引擎**(差异化核心)、**GB-2016/2022 双版本**、**订阅通知全套**(目录/报警对话内 NOTIFY + 位置周期)、**回放倍速/下载**、**音视频复合流(H.264+G.711A)**、**MP4/FLV/MKV 容器源**、**网络校时**、**SIP 信令实时追踪**。仍缺:**语音广播/对讲**(需反向 INVITE + 音频上行,本 WVP 环境无法触发验证)、巡航轨迹控制、摄像头实时采集;GB35114/上级侧为明确不做。

图例:✅ 已实现并对真实 WVP 验证 · 🟢 已实现(单测,未逐一真机) · 🚧 部分 · ⬜ 规划 · 🚫 明确不做

> 真机验证平台:WVP-Pro `192.168.10.222:8160`(SIP 域 `3502000000`),设备 `35020000001310000001`。

---

## 注册与信令

| 功能 | 需求 | 状态 |
|---|---|---|
| REGISTER / Digest MD5 / 注销 | FR-1 | ✅ WVP 显示设备在线 |
| 心跳保活 + 超时重注册(指数退避) | FR-2 | ✅ |
| OPTIONS 探活响应 | FR-3 | ✅ |
| SUBSCRIBE 订阅应答(回 200) | FR-3 | ✅ 注册后 SUBSCRIBE 正常应答 |
| SIP over UDP | FR-4 | ✅ |
| SIP over TCP | FR-4 | 🟢 传输层已支持 |

## 设备查询(平台 → 设备)

| 功能 | 需求 | 状态 |
|---|---|---|
| 目录查询(Catalog,独立 MESSAGE 应答) | FR-5 | ✅ WVP 目录同步出通道 |
| 设备信息(DeviceInfo) | FR-6 | ✅ |
| 设备状态(DeviceStatus) | FR-6 | ✅ |
| 录像列表(RecordInfo) | FR-10 | ✅ WVP 显示录像段 |
| GB-2016 / GB-2022 版本切换 | FR-11 | ✅ 两版 WVP 验证(2022 输出 SecurityLevelCode/IPAddress/Port,2016 省略) |

## 实时音视频 / 回放

| 功能 | 需求 | 状态 |
|---|---|---|
| INVITE 点播 / ACK / BYE 停流 / CANCEL 取消 | FR-7 | ✅ WVP 点播成功出流;CANCEL 停未决点播 |
| SDP 协商(PS/90000,y=ssrc,TCP/UDP proto) | FR-7 | ✅ |
| RTP over UDP 推流 | FR-8 | ✅ |
| RTP over TCP(RFC 4571) | FR-8 | ✅ WVP TCP-PASSIVE 收流 |
| PS 封装 H.264(Pack/System/PSM/PES) | FR-8 | ✅ WVP 转 FLV 出画面 |
| H.264 文件循环推流(C 档) | FR-8 | ✅ |
| 历史回放流推送 | FR-10 | 🟢 复用点播 INVITE 推流路径 |
| 回放控制(会话内 INFO/MANSRTSP:PLAY/PAUSE/Scale 倍速) | FR-10 | 🟢 INFO 回 200 + 运行时调速/暂停/恢复(单测+真机 INFO 注入回 200);全链路倍速需活跃回放会话 |
| 下载(INVITE s=Download + a=downloadspeed:N) | FR-10 | ✅ 按 N 倍速推流(实测:4× → 100 包/s = 4×25fps,1× → ~29/s) |
| 强制关键帧(IFrameCmd) | FR-8 | 🚧 见设备控制 |

## 设备控制(平台 → 设备,MANSCDP Control)

平台下发 `<Control>` 命令,设备回 200 + 结果 Notify。对标 uvp-gb28181-sim 的关键补齐项。

| 功能 | CmdType/字段 | 优先级 | 状态 |
|---|---|---|---|
| PTZ 云台控制 | DeviceControl / PTZCmd | P0 | ✅ WVP 下发验证(解析+应答) |
| 预置位设置/调用/删除 | DeviceControl / PTZCmd 指令码 81/82/83 | P1 | ✅ 解析 8 字节码预置位操作,有状态增删,实 SIP 注入验证(设置/删除后 PresetQuery 反映) |
| 看守位设置 | DeviceControl / HomePosition | P1 | 🟢 解析 Enabled/ResetTime/PresetIndex + 应答 |
| 拉框放大/缩小 | DeviceControl / DragZoomIn / DragZoomOut(`Control::kind()`=拉框放大/缩小) | P1 | 🟢 解析 DragZoom 参数 + 应答 |
| 强制关键帧 | DeviceControl / IFameCmd | P0 | ✅ 命令解析+应答;文件回放源自带周期 IDR,无法"强制"再生成一帧(硬标关键帧会产出坏流),故仅应答,符合回放模拟器语义 |
| 录像控制(录制/停止) | DeviceControl / RecordCmd | P1 | ✅ 同一 Control 链路 |
| 布防/撤防 | DeviceControl / GuardCmd | P1 | ✅ 同一 Control 链路 |
| 报警复位 | DeviceControl / AlarmCmd | P1 | ✅ 同一 Control 链路 |
| 远程启动 | DeviceControl / TeleBoot | P2 | ✅ 同一 Control 链路 |
| 设备配置(基本参数) | DeviceConfig / BasicParam | P2 | ⬜ |

## 订阅与通知(平台订阅 → 设备周期 NOTIFY)

| 功能 | 说明 | 优先级 | 状态 |
|---|---|---|---|
| SUBSCRIBE 应答 200 | 建立订阅 | — | ✅ |
| 移动位置主动上报 NOTIFY(GPS) | MobilePosition | P0 | ✅ WVP 接受(code 200) |
| 移动位置**订阅** + 周期 NOTIFY | SUBSCRIBE MobilePosition + Interval | P1 | ✅ WVP 下发订阅(Interval=5)→ 设备每 5s 上报位置 NOTIFY,WVP 逐条 code=200 |
| 目录订阅 + 变更 NOTIFY | Catalog 订阅 | P1 | ✅ 订阅回带 tag 的 200 + 对话内 SIP NOTIFY(全通道 Event=ON,带 Event/Subscription-State),WVP code=200 |
| 报警订阅 | Alarm 订阅 | P1 | ✅ 订阅回带 tag 的 200 + 记录对话;report_alarm 有订阅时走对话内 SIP NOTIFY(WVP code=200),无订阅退化独立 MESSAGE |

## 其它设备能力

| 功能 | CmdType | 优先级 | 状态 |
|---|---|---|---|
| 网络校时 | REGISTER 200 OK 的 Date 头 | P1 | ✅ 解析平台 Date 校准时钟偏移(实测 +28794s=UTC↔Beijing),心跳/报警/位置时间戳随之对齐(修复了原 1970 占位) |
| 语音广播 | Broadcast | P2 | ⬜ 未做:需设备侧反向 INVITE(UAC)+ 音频接收。WVP broadcast API 返回成功但未观察到向设备发 SIP Broadcast Notify(疑需设备声明音频输出能力),本环境无法验证,故不盲写 |
| 设备配置查询 | ConfigDownload / BasicParam + VideoParamOpt | P2 | 🟢 回 200 + ConfigDownload Response,支持 BasicParam(Name/Expiration/HeartBeat*)与 VideoParamOpt(DownloadSpeed/Resolution)按 ConfigType 组合输出(FR-17) |
| 预置位查询 | PresetQuery | P2 | ✅ 返回有状态预置位表(随预置位设置/删除动态变),实 SIP 注入验证 |
| 报警状态查询 | AlarmStatus | P1 | 🟢 GB-2022 每报警通道 DutyStatus(ALARM/OFFDUTY)/ GB-2016 NotNumber,随布防态动态(FR-17,单测) |
| 看守位查询 | HomePositionQuery | P1 | 🟢 Enabled/ResetTime(固定30)/PresetIndex(有无看守位标志)(FR-17,单测) |
| 存储卡状态查询 | StorageCardStatusQuery | P1 | 🟢 单张 32G 卡余 24G(模拟固定值)(FR-17,单测) |
| 巡航轨迹列表查询 | CruiseTrackListQuery | P1 | 🟢 返回有状态巡航轨迹号列表(随巡航增删动态)(FR-17,单测) |
| 巡航轨迹详情查询 | CruiseTrackQuery | P1 | 🟢 按 GroupID 返回该轨迹预置点(Speed/DwellTime 固定 5/3)(FR-17,单测) |
| PTZ 精准状态查询 | PTZPreciseStatusQuery | P1 | 🟢 返回最近精准云台姿态 Pan/Tilt/Zoom(%.2f)(FR-17,单测) |
| 移动位置单次查询 | MobilePosition | P1 | 🟢 复用位置 NOTIFY 骨架单发(FR-17) |
| 设备软件升级 | DeviceUpgrade | P2 | 🚧 M6·P3 计划(4 步进度 NOTIFY) |

## 主动上报(设备 → 平台)

| 功能 | 需求 | 状态 |
|---|---|---|
| 报警上报(Alarm Notify) | FR-9 | ✅ WVP 接受(code 200) |
| 录像完成/异常通知 | FR-10 | ⬜ |

## 压力测试(差异化能力)

| 功能 | 需求 | 状态 |
|---|---|---|
| 批量生成 N 设备(ID 递增) | FR-20 | 🟢 |
| 爬坡速率控制 | FR-21 | 🟢 |
| 万级并发注册 + 心跳 | FR-22 | 🟢 单机架构支持 |
| 媒体三档 A/B/C | FR-23 | 🟢 |
| active_ratio 推流采样 | FR-24 | 🟢 |
| 场景 TOML 保存/加载/复跑 | FR-25 | 🟢 |
| 实时指标 + 失败归因 | FR-26/27 | 🟢 |
| 压测报告导出 | FR-28 | 🟢 |

## 明确不做

| 功能 | 说明 |
|---|---|
| 双向语音对讲 | 🚫 排期外 |
| GB35114 国密扩展 | 🚫 后续评估 |
| 上级平台(SU)侧 | 🚫 本项目只做下级设备 |

---

## 关键 ID 编码

GB28181 设备/通道 ID 为 20 位:`中心编码(8) + 行业(2) + 类型(3) + 序号(7)`。批量生成虚拟设备按序号递增。详见 `40-protocol/manscdp.md#id-编码` 与 `30-crates/gb28181-protocol.md`。

## 联调平台与运行

单设备联调运行方式见 `10-functional/device-simulation.md#51`;真实 WVP 联调参数见 `40-protocol/gb28181-联调.md`。真实 H.264 测试码流用 `assets/media/gen-test-h264.sh` 生成(需 ffmpeg)。
