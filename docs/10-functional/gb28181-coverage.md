# GB28181 功能点覆盖矩阵

**状态:核心已实现并真机验证** · 本项目实现 GB/T 28181-2022 **下级设备(IPC)侧**协议。协议流程参考开源项目 uvp-gb28181-sim(Android 版),桌面端用 Rust 独立重写。

> **对标 uvp-gb28181-sim 结论(2026-07-04 更新)**:上游 Android 版的核心能力(注册/心跳/目录/点播/PTZ 及设备控制/**预置位 CRUD**/**看守位**/GPS 上报/报警)本项目均已覆盖并真机验证,且**额外**具备上游没有的**万级压测引擎**(差异化核心)、**GB-2016/2022 双版本**、**订阅通知全套**(目录/报警对话内 NOTIFY + 位置周期)、**回放倍速/下载**、**音视频复合流(H.264+G.711A)**、**MP4/FLV/MKV 容器源**、**网络校时**、**SIP 信令实时追踪**。仍缺:**语音广播的音频收流 / 双向对讲**(需反向 INVITE + 音频上行,本 WVP 环境无法触发验证);GB35114、附录 A.4 联网系统扩展应用、上级侧为明确不做/不适用。巡航轨迹控制与摄像头实时采集已实现。

图例:✅ 已实现并对真实 WVP 验证 · 🟢 已实现(单测,未逐一真机) · 🚧 部分 · ⬜ 规划 · 🚫 明确不做

> **2026-08 逐条对标标准原文的复核结论**(依据 `GB/T 28181-2022` 与 `GB/T 28181-2016` PDF 正文):
> 命令集覆盖是完整的 —— 2022 附录 A 的控制(A.2.3)、查询(A.2.4)、通知(A.2.5)、
> 应答(A.2.6)全部命令都有实现,2016 是 2022 的子集因此一并覆盖。复核**发现并修正了
> 6 处与标准原文不一致**,详见下面「与标准原文的差异修正」一节。

> 真机验证平台:WVP-Pro `<WVP_SERVER_HOST>:<WVP_SERVER_PORT>`(SIP 域 `<WVP_SERVER_DOMAIN>`),设备 `<WVP_DEVICE_ID>`。

---

## 与标准原文的差异修正(2026-08 复核)

逐条比对标准 PDF 后发现的实现偏差,均已修正并加了回归测试:

| # | 条款 | 原实现 | 问题 | 现状 |
|---|---|---|---|---|
| 1 | **A.3.7 表 A.11** 辅助开关 | 用 `0x89`/`0x8A` 当开/关 | **指令码错**。`0x8C`(开)/`0x8D`(关)才是辅助开关;`0x89`/`0x8A` 是 **A.3.6 自动扫描**,等于把扫描指令当成雨刷,且真正的辅助开关命令被丢弃 | ✅ 改为 `0x8C`/`0x8D` |
| 2 | **A.3.6 表 A.10** 自动扫描 | 完全未实现 | 开始扫描 / 设左右边界 / 设扫描速度全部无响应 | ✅ 新增 `Control::scan_op()` + `ScanOp` 四种操作与设备侧状态 |
| 3 | **A.3.5 表 A.9 注2** 巡航速度/停留 | 只取字节6(8 位) | 标准是 **12 位**(字节6 低 8 位 + 字节7 高 4 位),>255 的值被截断 | ✅ 按 12 位解析 |
| 4 | **A.3.5 表 A.9 注2** 删除巡航 | `0x85` 一律当"删单点" | 字节6=`00H` 时应**删除整条巡航** | ✅ 新增 `CruiseOp::DelTrack` |
| 5 | **A.2.1.10** 录像项 `Secrecy` | 不输出 | `Secrecy` 是**必选**字段,严格校验 schema 的平台会拒收整条录像应答 | ✅ 输出 `Secrecy`,并补 FilePath/Address/RecorderID/FileSize/StreamNumber |
| 6 | **A.2.5.2** 心跳 `Info` | 不输出 | `Info`(故障设备列表)是**必选**元素,严格平台会拒收心跳 → 设备一直离线 | ✅ 恒输出 `<Info/>`,支持携带故障通道 |

同一轮复核补齐的字段/流程(不算"错",属于之前没做):

| 条款 | 补齐内容 |
|---|---|
| **A.2.6.6** 设备状态应答 | 补 `DeviceTime`(用校时后的真实时间)、`Encode`、`Record`(随录像控制变)、`Alarmstatus`(随布防态给 ALARM/OFFDUTY) |
| **A.2.5.3** 报警通知 | 补 `Longitude`/`Latitude` 与 `Info/AlarmType`(+`AlarmTypeParam/EventType`)。之前视频报警不带 `AlarmType`,平台只能显示"未知视频报警";新增 `VideoAlarmType` 13 种类型与 `AlarmNotify::gps` |
| **A.2.4.5** 录像检索 | 之前无论平台查哪一天都回同一段硬编码录像;现在**按请求的 StartTime/EndTime 切整点段返回**,并按 `Type`(time/alarm/manual/all)过滤 |
| **A.2.4.6** 报警订阅 | 之前订阅条件被完全忽略(全推)。现在解析 `StartAlarmPriority`/`EndAlarmPriority`/`AlarmMethod`/`AlarmType`/`StartAlarmTime`/`EndAlarmTime`,并用 `AlarmSubscribeFilter` 在上报前过滤 |
| **§9.1.2.3** 注册重定向 | 之前 `302 Moved Temporarily` 被当成"注册被拒",接重定向服务器的平台永远注册不上。现在解析 `Contact` 改向新服务器注册(只重定向一次,防环路) |
| **§9.1.1 c)** 注册重试间隔 | 标准要求"可调,一般不应短于 60s"。单设备默认 60s **遵守标准**;压测场景默认 10s 并在配置里注明是**为压测目的有意偏离** |
| **§9.1.1 f)** NAT 内侧宜用 TCP 注册 | 已实现 SIP over TCP 并保持连接复用 |

---

## 注册与信令

| 功能 | 需求 | 状态 |
|---|---|---|
| REGISTER / Digest MD5 / 注销 | FR-1 | ✅ WVP 显示设备在线;下线发 REGISTER Expires=0 主动注销(§9.1.2.2),WVP 立即置离线(真机验证) |
| 心跳保活 + 超时重注册(指数退避) | FR-2 | ✅ |
| OPTIONS 探活响应 | FR-3 | ✅ |
| SUBSCRIBE 订阅应答(回 200) | FR-3 | ✅ 注册后 SUBSCRIBE 正常应答 |
| SIP over UDP | FR-4 | ✅ |
| SIP over TCP | FR-4 | ✅ `TcpTransport` 按 RFC 3261 §18.3 流式重组,连接复用 + 被动 accept;端到端回归见 `tests/tcp_transport.rs`。对应 §9.1.1 f) 的"NAT 内侧宜用 TCP 注册并保持连接" |
| 注册重定向(302 Moved Temporarily) | §9.1.2.3 | 🟢 解析 302 的 Contact 改向新服务器注册,只重定向一次防环路(单测) |
| 注册重试间隔可调 | §9.1.1 c) | ✅ 单设备默认 60s(遵守"不应短于 60s");压测默认 10s,**有意偏离**以服务爬坡观测 |
| 注册有效期 | §9.1.1 d) | 🚧 缺省用 3600s(标准下限),标准建议缺省 86400s。联调时更容易观察续约行为,可在界面改 |

## 设备查询(平台 → 设备)

| 功能 | 需求 | 状态 |
|---|---|---|
| 目录查询(Catalog,独立 MESSAGE 应答) | FR-5 | ✅ WVP 目录同步出通道 |
| 设备信息(DeviceInfo) | FR-6 | ✅ |
| 设备状态(DeviceStatus) | FR-6 | ✅ 含 A.2.6.6 的可选字段:`DeviceTime`(校时后真实时间)/`Encode`/`Record`(随录像控制)/`Alarmstatus`(随布防态) |
| 录像列表(RecordInfo) | FR-10 | ✅ **按请求的 StartTime/EndTime 切整点段返回**(最多 24 段),按 `Type` 过滤;录像项含必选 `Secrecy` 及 FilePath/Address/RecorderID/FileSize/StreamNumber |
| GB-2016 / GB-2022 版本切换 | FR-11 | ✅ 两版 WVP 验证(2022 输出 SecurityLevelCode/IPAddress/Port,2016 省略) |

## 实时音视频 / 回放

| 功能 | 需求 | 状态 |
|---|---|---|
| INVITE 点播 / ACK / BYE 停流 / CANCEL 取消 | FR-7 | ✅ WVP 点播成功出流;CANCEL 停未决点播 |
| SDP 协商(PS/90000,y=ssrc,TCP/UDP proto) | FR-7 | ✅ |
| RTP over UDP 推流 | FR-8 | ✅ |
| RTP over TCP(RFC 4571) | FR-8 | ✅ WVP TCP-PASSIVE 收流 |
| PS 封装 H.264(Pack/System/PSM/PES) | FR-8 | ✅ WVP 转 FLV 出画面 |
| PS 封装 H.265(PSM stream_type 0x24) | FR-33 | 🟢 从 NAL(VPS type32)自动探测 H.265,PSM 声明 0x24,帧切分按 H.265 VCL/关键帧类型;单测(真机推流待验) |
| 音频复合流 G.711A(PSM 0x90) | FR-8 | ✅ 与视频同步复用 |
| 音频复合流 AAC(PSM stream_type 0x0F) | FR-33 | 🟢 PSM 支持 AAC ES 映射;单测(AAC 音频轨抽取/ADTS 封装待接 ffmpeg) |
| H.264 文件循环推流(C 档) | FR-8 | ✅ |
| 历史回放流推送 | FR-10 | 🟢 复用点播 INVITE 推流路径 |
| 回放控制(会话内 INFO/MANSRTSP:PLAY/PAUSE/Scale 倍速/Range 拖动) | FR-10 | 🟢 INFO 回 200 + 运行时调速/暂停/恢复;§9.8 Range 拖动:解析 npt 起点折算千分比,PlaybackControl.seek + FileSource 跳最近关键帧(单测+真机 INFO 注入回 200) |
| 下载(INVITE s=Download + a=downloadspeed:N) | FR-10 | ✅ 按 N 倍速推流(实测:4× → 100 包/s = 4×25fps,1× → ~29/s) |
| 强制关键帧(IFrameCmd) | FR-8 | 🚧 见设备控制 |

## 设备控制(平台 → 设备,MANSCDP Control)

平台下发 `<Control>` 命令,设备回 200 + 结果 Notify。对标 uvp-gb28181-sim 的关键补齐项。

| 功能 | CmdType/字段 | 优先级 | 状态 |
|---|---|---|---|
| PTZ 云台控制 | DeviceControl / PTZCmd | P0 | ✅ WVP 下发验证(解析+应答) |
| 预置位设置/调用/删除 | DeviceControl / PTZCmd 指令码 81/82/83 | P1 | ✅ 解析 8 字节码预置位操作,有状态增删,实 SIP 注入验证(设置/删除后 PresetQuery 反映) |
| 看守位设置 | DeviceControl / HomePosition | P1 | 🟢 解析 Enabled/ResetTime/PresetIndex + 应答;更新看守位启用/已设标志供 HomePositionQuery 读回(FR-18) |
| 拉框放大/缩小 | DeviceControl / DragZoomIn / DragZoomOut(`Control::kind()`=拉框放大/缩小) | P1 | 🟢 解析 DragZoom 参数 + 应答 |
| 强制关键帧 | DeviceControl / IFameCmd | P0 | ✅ 命令解析+应答;文件回放源自带周期 IDR,无法"强制"再生成一帧(硬标关键帧会产出坏流),故仅应答,符合回放模拟器语义 |
| 录像控制(录制/停止) | DeviceControl / RecordCmd | P1 | ✅ 同一 Control 链路 |
| 布防/撤防 | DeviceControl / GuardCmd | P1 | ✅ 同一 Control 链路;更新报警值守态供 AlarmStatus 读回(FR-18) |
| 报警复位 | DeviceControl / AlarmCmd | P1 | ✅ 同一 Control 链路 |
| 远程启动 | DeviceControl / TeleBoot | P2 | ✅ 同一 Control 链路 |
| 精确云台控制 | DeviceControl / PTZPreciseCtrl(Pan/Tilt/Zoom) | P1 | 🟢 解析浮点姿态并记录,供 PTZPreciseStatusQuery 读回(FR-18,单测) |
| 巡航轨迹控制 | DeviceControl / PTZCmd `0x84`~`0x88`(增点/删点/删整条/速度/停留/启动),表 A.9 | P1 | 🟢 速度/停留按 **12 位**解析(字节6 低 8 位 + 字节7 高 4 位);字节6=`00H` 删整条轨迹;状态供 CruiseTrack* 查询读回(FR-18,单测) |
| 自动扫描控制 | DeviceControl / PTZCmd `0x89`(开始/左边界/右边界)、`0x8A`(扫描速度),表 A.10 | P1 | 🟢 新增;之前这两个码被错当成辅助开关,扫描指令等于被吞掉(单测) |
| 辅助开关控制 | DeviceControl / PTZCmd `0x8C` 开 / `0x8D` 关,表 A.11 | P1 | 🟢 **指令码已修正**(原用 0x89/0x8A);字节5 为辅助号,标准只明确 1=雨刷,2~5(红外/加热/除雾/制冷)按厂商事实约定(单测) |
| 聚焦/光圈 | DeviceControl / PTZCmd byte3 聚焦位 | P2 | 🟢 最小识别(0x40 近/0x80 远)+ 应答;⚠️ 未真机核对字节位(FR-18) |
| 目标跟踪 | DeviceControl / TargetTrack(Mode/ObjectID/Speed) | P2 | 🟢 白名单模式解析+应答,模式外忽略仍 200(FR-18,单测) |
| 格式化 SD 卡 | DeviceControl / FormatSDCard + DiskNum | P2 | 🟢 解析+应答(模拟设备无实际存储)(FR-18,单测) |
| 设备配置(基本参数) | DeviceConfig / BasicParam | P2 | ⬜ 修改暂未做(查询见 ConfigDownload) |

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
| 语音广播 | Broadcast | P2 | 🟡 握手已实现:解析 Broadcast Notify(SourceID/TargetID)→ 回 Broadcast Response(TargetID 属本设备 OK,否则 ERROR+Reason);接受后构造反向 INVITE 的 SDP offer(a=recvonly / PCMA+PCMU / y=ssrc)。**反向 INVITE 事务与音频 RTP 接收/ACK/BYE 链路本 WVP 环境无法触发验证,未实装收流**(FR-36,握手+SDP 单测) |
| 设备配置查询 | ConfigDownload / BasicParam + VideoParamOpt | P2 | 🟢 回 200 + ConfigDownload Response,支持 BasicParam(Name/Expiration/HeartBeat*)与 VideoParamOpt(DownloadSpeed/Resolution)按 ConfigType 组合输出(FR-17) |
| 设备配置查询扩展 | ConfigDownload / 全 ConfigType | P2 | 🟢 GB28181-2022 A.2.4.7 全部 ConfigType:BasicParam/VideoParamOpt/VideoParamAttribute/VideoRecordPlan/VideoAlarmRecord/PictureMask/FrameMirror/AlarmReport/OSDConfig/SnapShotConfig 完整;SVACEncodeConfig/SVACDecodeConfig 最小合规占位(FR-37,单测) |
| 设备配置修改 | DeviceConfig / 全配置项 | P2 | 🟢 A.2.3.2 全部配置命令接受回 DeviceConfig 应答(A.2.6.8);含 SVAC/VideoParamAttribute(FR-37) |
| 预置位查询 | PresetQuery | P2 | ✅ 返回有状态预置位表(随预置位设置/删除动态变),实 SIP 注入验证 |
| 报警状态查询 | AlarmStatus | P1 | 🟢 GB-2022 每报警通道 DutyStatus(ALARM/OFFDUTY)/ GB-2016 NotNumber,随布防态动态(FR-17,单测) |
| 看守位查询 | HomePositionQuery | P1 | 🟢 Enabled/ResetTime(固定30)/PresetIndex(有无看守位标志)(FR-17,单测) |
| 存储卡状态查询 | SDCardStatus | P1 | 🟢 单张 32G 卡余 24G(模拟固定值)(FR-17,单测) |
| 巡航轨迹列表查询 | CruiseTrackListQuery | P1 | 🟢 返回有状态巡航轨迹号列表(随巡航增删动态)(FR-17,单测) |
| 巡航轨迹详情查询 | CruiseTrackQuery | P1 | 🟢 按 Number 返回该轨迹预置点(Speed/DwellTime 固定 5/3)(FR-17,单测) |
| PTZ 精准状态查询 | PTZPosition | P1 | 🟢 返回最近精准云台姿态 Pan/Tilt/Zoom(%.2f)(FR-17,单测) |
| PTZ 精准位置事件订阅+通知 | PTZPosition(SUBSCRIBE) | P1 | 🟢 §9.11.1/9.11.2:平台 SUBSCRIBE+PTZPosition(携 Interval)→ 设备周期发 PTZ 精准状态 NOTIFY(复用 PTZPosition 应答体,独立 MESSAGE);2022 新增(FR-37) |
| 移动位置单次查询 | MobilePosition | P1 | 🟢 复用位置 NOTIFY 骨架单发(FR-17) |
| 设备软件升级 | DeviceUpgrade | P2 | 🟢 回 200 后异步发 4 步进度(0/30/60/100%)DeviceUpgradeResult NOTIFY(percent<100→Result=0,=100→Result=1)(FR-30,单测) |
| 平台下发抓拍(旧) | SnapShotCmd | P2 | 🟢 触发经 Alarm Notify 上报(FR-19) |
| 平台下发抓拍(GB-2022) | SnapShotConfig | P2 | 🟢 SessionID/UploadURL/SnapNum(钳1-10)/Interval;串行拍 N 张,裸 TCP HTTP PUT 上传占位 JPEG(2xx 判定 + SSRF 白名单拒环回/私网/组播)+ 完成 NOTIFY(CmdType=Notify/SubCmd=SnapShot);仅 http(FR-19,单测) |

## 主动上报(设备 → 平台)

| 功能 | 需求 | 状态 |
|---|---|---|
| 报警上报(Alarm Notify) | FR-9 | ✅ WVP 接受(code 200)。补齐 A.2.5.3 的 `Longitude`/`Latitude` 与 `Info/AlarmType`(13 种视频报警类型)+ `AlarmTypeParam/EventType`;新增 `AlarmNotify::gps`(AlarmMethod=4) |
| 报警订阅筛选 | A.2.4.6 | 🟢 解析级别区间/报警方式/报警类型并在上报前过滤(`AlarmSubscribeFilter`,单测)。之前订阅条件被忽略,一律全推 |
| 录像完成/异常通知(MediaStatus) | FR-31 | 🟢 回放/下载会话 BYE 后发 MediaStatus NotifyType=121(历史媒体发送结束);122/123(异常/存储满)类型已备(FR-31) |
| 实时视音频回传通知(VideoUploadNotify) | FR-37 | 🟢 GB28181-2022 A.2.5.8:report_video_upload 主动发独立 MESSAGE(Time+可选经纬度)(FR-37,单测) |
| 注册续约 | FR-32 | 🟢 注册有效期 3600s,到期前 80%(2880s)主动重注册,不等心跳失败被动重注册(FR-32) |
| RTCP SR 反馈 | FR-32 | 🟢 build_rtcp_sr 按 RFC3550 §6.4.1 构造 SR 包(SSRC/NTP/RTP-TS/包数/字节数),单测;周期发送待接入 RTCP 端口(需真机反馈校准,暂不盲发) |
| Catalog 增量 NOTIFY | FR-32 | 🟢 diff 引擎(CatalogSnapshot::diff,ADD/DEL/UPDATE/ON/OFF)+ 设备侧 catalog_dialog 记录订阅,CRUD 后 notify_catalog_changed 发对话内增量 NOTIFY;全量 NOTIFY 刷新快照基线(FR-32/34,单测) |
| 多通道虚拟通道 CRUD + 模板 | FR-34 | 🟢 引擎 CatalogNode 树 + 4 模板(single/nvr-8ch/civil-3x2/large-16ch,据上游核对)+ 载入/增删改;报警通道独立节点 typeCode 134;多通道 Catalog 应答按层级出 ParentID/Parental/CivilCode。前端「多通道目录」页:模板选择器 + 目录树表格 + 通道增删改弹窗(headless Chrome 渲染核对)(FR-34,单测) |
| OSD 叠加 | FR-35 | 🟡 配置面对齐上游(时间戳/通道名/水印开关+位置+字号,本地持久化)。**与上游一致 OSD 为本地渲染,不进 GB28181 协议、不影响平台收流**;我方文件推流模型不逐帧烧入(逐帧重编码破坏轻量循环模型,压测不可用),故仅配置面(FR-35) |

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

## 明确不做 / 不适用

| 功能 | 条款 | 说明 |
|---|---|---|
| 双向语音对讲 | §9.12.2 | 🚫 排期外(需音频上行 + 回声处理) |
| 语音广播的音频收流 | §9.12.1 | 🟡 握手与 SDP offer 已实现,反向 INVITE 的音频 RTP 接收未实装(本 WVP 环境无法触发验证) |
| GB35114 国密扩展 | §8 高安全级别 | 🚫 后续评估。数字证书认证、SIP 信令签名、媒体加密都在该标准里,当前只做数字摘要认证 |
| 联网系统扩展应用 | 附录 A **A.4** | 🚫 不适用。停车场/门禁/道闸/车辆抓拍机等扩展目录类型(detectorType / pmsHostType / dmsHostType / barrierType …)不属于视频 IPC 的能力域 |
| SVAC 编码/解码配置 | A.2.3.2.3 / A.2.3.2.4 | 🚧 最小合规占位(接受并回应答,不真的做 SVAC 编码)。SVAC 是国密视频编码,模拟器不实现编码器 |
| 上级平台(SU)侧 | 全篇 | 🚫 本项目只做下级设备(SIP 设备/网关侧) |
| OSD 烧入画面 | A.2.3.2.11 | 🟡 接受并应用配置命令,但不逐帧烧入图像 —— 逐帧重编码会破坏"轻量循环推流"模型,压测不可用 |
| 强制关键帧真的生成 IDR | A.2.3.1.7 | 🟡 命令解析 + 应答。文件循环源自带周期 IDR,硬标关键帧会产出坏流,故只应答 |

---

## 关键 ID 编码

GB28181 设备/通道 ID 为 20 位:`中心编码(8) + 行业(2) + 类型(3) + 序号(7)`。批量生成虚拟设备按序号递增。详见 `40-protocol/manscdp.md#id-编码` 与 `30-crates/gb28181-protocol.md`。

## 联调平台与运行

单设备联调运行方式见 `10-functional/device-simulation.md#51`;真实 WVP 联调参数见 `40-protocol/gb28181-联调.md`。真实 H.264 测试码流用 `assets/media/gen-test-h264.sh` 生成(需 ffmpeg)。
