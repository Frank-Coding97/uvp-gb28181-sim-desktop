# GB28181 功能点覆盖矩阵

**状态:核心已实现并真机验证** · 本项目实现 GB/T 28181-2022 **下级设备(IPC)侧**协议。协议流程参考开源项目 uvp-gb28181-sim(Android 版),桌面端用 Rust 独立重写。

> **对标 uvp-gb28181-sim 结论(2026-08-30 更新)**:上游 Android 版的核心能力(注册/心跳/目录/点播/PTZ 及设备控制/**预置位 CRUD**/**看守位**/GPS 上报/报警)本项目均已覆盖并真机验证,且**额外**具备上游没有的**万级压测引擎**(差异化核心)、**GB-2016/2022 双版本**、**订阅通知全套**(目录/报警对话内 NOTIFY + 位置周期)、**回放倍速/下载**、**音视频复合流(H.264+G.711A)**、**MP4/FLV/MKV 容器源**、**网络校时**、**SIP 信令实时追踪**、**语音广播完整收流链路**、**附录 K 媒体流保活 + 附录 L Subject 头域**。GB35114、附录 A.4 联网系统扩展应用、上级侧为明确不做/不适用。

图例:✅ 已实现并对真实 WVP 验证 · 🟢 已实现(单测/集成测,未逐一真机) · 🚧 部分 · ⬜ 规划 · 🚫 明确不做

> **2026-08 逐条对标标准原文的复核结论**(依据 `GB/T 28181-2022` 与 `GB/T 28181-2016` PDF 正文):
> 命令集覆盖是完整的 —— 2022 附录 A 的控制(A.2.3)、查询(A.2.4)、通知(A.2.5)、
> 应答(A.2.6)全部命令都有实现,2016 是 2022 的子集因此一并覆盖。复核**发现并修正了
> 6 处与标准原文不一致**,详见「与标准原文的差异修正」一节。
>
> **第二轮复核(§9.12 语音广播 / 附录 C.2.4 / 附录 G / 附录 K / 附录 L)又发现 5 处实现缺口,
> 已全部补齐**,详见「第二轮复核:§9.12 与附录 G/K/L」一节。
>
> **第三轮复核(规范性附录 E / I / J / M / N / O + A.2.1.9 目录项)又发现 1 个**编码错误**与
> 5 处未实现的附录级要求,已全部补齐**;同轮把 RTCP 接收报告从"加分项待排期"落地成
> 双向实现。详见「第三轮复核:附录 E/I/J/M/N/O 与目录项字段」一节。

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

## 第二轮复核:§9.12 与附录 G/K/L(2026-08-30)

前一轮只对了附录 A 的命令集。这一轮把**媒体链路的信令与保活**也逐条对了一遍
(§9.12 语音广播/对讲、附录 C.2.4 音频 RTP 封装、附录 G SDP 定义、附录 K 媒体流保活、
附录 L Subject 头域),发现 5 处缺口,全部补齐:

| # | 条款 | 原实现 | 问题 | 现状 |
|---|---|---|---|---|
| 1 | **§9.12.1 图27 信令 5~15** 语音广播 | 只构造 SDP offer 并打日志,**SDP 里端口写死 0** | 反向 INVITE 从未发出,链路从来没建起来过。端口 0 在 SDP 里意味着"该媒体被禁用",即使发出去平台也无处推流 | ✅ 完整实现:**先绑收流端口再发 INVITE** → 收 200 → 学到 To-tag 发 ACK → 收 G.711 音频 → 平台 BYE 或设备挂断。端到端回归见 `tests/voice_broadcast.rs` |
| 2 | **附录 L** Subject 头域 | 完全没有 | §9.2.3 c) 要求"发送给媒体服务器的消息头**应**包括 Subject 字段";没有它就无法做附录 K c) 的同源判断 | ✅ 新增 `sip_core::Subject`(解析/生成/`media_source()`,实时流序列号首位 0、历史流首位 1),出站 INVITE 携带,入站 INVITE 解析 |
| 3 | **附录 K c)** 同源码流替换 | 有会话就回 `486 Busy Here` | **与标准相反**。标准要求"若已经在发送以此媒体源标识的码流,则应释放现有媒体流发送链路并按照请求建立新的媒体流发送链路"。旧行为下平台重启或丢一次 200 OK,这个通道就永久拉不到流 | ✅ 按 Subject 的媒体源标识判定:同源→释放旧链路重建;确为另一路码流→仍回 486 |
| 4 | **附录 K a) / b)** 媒体流丢失通知 | 设备**从不发 BYE**,推流失败只推一个前端事件 | 平台侧那条媒体链路会一直挂着等流,直到它自己的丢流监测超时 | ✅ 推流异常 → 会话内 BYE(`notify_media_loss`);设备下线 → 先拆链路再注销(`release_media_on_shutdown`) |
| 5 | **附录 C.2.4** 音频流 RTP 封装 | 纯音频 INVITE 会走视频路径,把音频塞进 PS | 标准规定语音比特流**直接用 RTP 打包**(PT=8 / PCMA / 8 kHz / 单声道、一包装整数个编码帧且时长 20~180 ms),塞进 PS 对端解不出 | ✅ 新增 `handle_audio_invite` + `push_audio_stream`:裸 G.711A、PT=8、20 ms 一包、时间戳按采样数递增 |

同一轮修正的两处 SDP 字段问题:

| 条款 | 问题 | 现状 |
|---|---|---|
| **附录 G** s 字段 | 广播 offer 写 `s=Broadcast` —— s 字段只有 `Play`/`Playback`/`Download` 三种合法取值 | ✅ 改为 `s=Play`(与 §9.12.1.3.2 示例一致) |
| **附录 G** f 字段 | 完全没有 `f=`(媒体参数描述) | ✅ 新增 `MediaFormat` 解析/渲染;广播 offer 与纯音频应答输出 `f=v/////a/1/8/1` |

---

## 第三轮复核:附录 E/I/J/M/N/O 与目录项字段(2026-08-31)

前两轮对完了附录 A 的命令集与媒体链路信令。这一轮对的是**规范性附录里那些容易被
当成"参考信息"跳过的条款** —— 它们恰恰决定平台能不能正确解析设备报上去的东西。

### 编码错误(会被平台误解释)

| # | 条款 | 原实现 | 问题 | 现状 |
|---|---|---|---|---|
| 1 | **附录 E 表 E.1** 设备类型编码 | 业务分组用 `137`、虚拟组织用 `138` | **类型码错**(沿用了上游 Android 项目的取值)。表 E.1 里 `137` = 语音输出设备、`138` = 移动传输设备,业务分组应是 **`215`**、虚拟组织应是 **`216`**。平台会把一个业务分组节点解析成一台语音输出设备,目录层级随之错位 | ✅ 改为 `215`/`216`,并补 `type_code_name()` 便于诊断 |
| 2 | **附录 E** 20 位 ID 构成 | 第 14 位(网络标识)被并进了序号,按 7 位序号处理 | 序号超过 100 万时会**静默改写网络标识** —— ID 看着合法,实际归属变了。压测生成万级设备时尤其危险 | ✅ 拆出独立的 `network_id`,序号回到 6 位(`MAX_SERIAL = 999_999`),新增 `gen_child_id_on_network()` |

### 未实现的附录级要求

| # | 条款 | 原实现 | 问题 | 现状 |
|---|---|---|---|---|
| 3 | **A.2.1.9** 目录项字段 | 只输出 11 个字段 | 缺 `Address`/`RegisterWay`/`Secrecy` 等**必选**元素与整个 `Info` 子结构;严格校验 schema 的平台会拒收整条目录应答,现象是"平台一直同步不到通道" | ✅ 扩到 25 个字段 + 23 字段的 `CatalogItemInfo`,**字段顺序严格按 XSD sequence**;2016 独有项(Owner/SafetyWay/CertNum/Certifiable/ErrCode/EndTime)插在 2022 顺序的对应位置,同一结构体按版本置 None 后两版输出顺序都合法 |
| 4 | **附录 O** 摄像机采集部位类型 | 完全没有 | `Info/CapturePositionType` 的 7 位层次码(大类 3 + 中类 2 + 小类 2)无从填写 | ✅ 新增 `capture_position` 模块与常用代码常量 + `is_valid()`(**只校验结构不内置整张表** —— 表 O.1 会随行业规定扩充,写死反而会拒掉合法的新代码);界面可选可手填 |
| 5 | **附录 I** 协议版本标识 | 不发也不读 `X-GB-Ver` | 无法与对端协商版本,只能盲发。2022 新增的消息(如 A.2.5.8 实时视音频回传通知)发给 2016 平台会让它报解析错误 | ✅ REGISTER 带 `X-GB-Ver`(2016→`2.0`、2022→`3.0`),从注册响应学习平台声明;**对端未声明 3.0 时不发 2022 独有消息** |
| 6 | **附录 J** 目录查询返回范围 | 一律返回整棵树 | 附录 J 按被查询对象分了 5 种范围(行政区域/系统/设备/业务分组/虚拟组织)。平台查某个业务分组,收到的却是整个设备的目录 | ✅ `catalog_query_scope()` 按查询 ID 分流:本设备→整树、2~8 位区划码→CivilCode 前缀匹配、树内节点→该节点及其后代;**查询对象不在目录里就回空**(配合附录 M 的 SumNum=0),而不是回一棵平台没请求的树 |
| 7 | **附录 M** 多响应消息传输 | 分片并发发出;空结果仍输出空列表元素 | 附录 M 要求多条应答**串行**(前一条收到 200 再发下一条),且"未查询出结果时 SumNum 取 0 且不携带记录列表" | ✅ `send_reply_messages_serial()` 串行发送(**失败也继续发剩余分片** —— 目录残缺比慢更糟);`DeviceList`/`RecordList` 改成 Option,空结果整个列表元素不输出;单条上限 `MAX_ITEMS_PER_MESSAGE = 10_000` |
| 8 | **附录 N** 目录变更批量通知 | 每条变更立刻一条 NOTIFY | 换一次 16 通道模板会瞬间打出十几条 NOTIFY。附录 N 要求"累计一定数量(如 100)条或一定时间间隔(如 1 分钟)后"再通知 | ✅ 变更进缓冲(同一通道只留终态)、攒够 100 条即发;时间维度用**1 秒去抖 + 60 秒硬上限**,见下方偏离说明 |

### 有意偏离标准示例值:附录 N 的时间窗

附录 N 的原文是"一定时间间隔(**如** 1 分钟)"—— `如` 说明这是示例值而非强制值。
本项目是**联调工具**:工程师在界面上改一个通道名,需要立刻在平台侧看到 NOTIFY,
等一分钟没法排障。所以时间维度取 **1 秒去抖**(最后一条变更后静默 1 秒即发),
同时保留 **60 秒硬上限**兜底(变更持续不断、去抖被反复重置时也不会无限期攒着)。
去抖同样满足附录 N 的本意 —— 避免批量变更时逐条推送。常量与理由都写在
`device.rs` 的 `CATALOG_NOTIFY_DEBOUNCE` 文档注释里。

实现上用**事件驱动的延迟任务**而不是全局定时器:压测万级设备时,一个"目录变更
ticker"会带来上万次无谓唤醒,而实际上绝大多数设备的目录从不变动。

### 安全决策:目录项永不输出 Password

A.2.1.9 里 `Password` 是可选字段。**本项目选择永不输出它** —— 目录应答会经过链路上
每一跳(可能还有级联平台),把设备凭据放进去等于主动广播。为了"字段完整"而输出凭据
不是合规,是漏洞。

---

## 媒体流保活与会话标识(附录 K / L)

| 功能 | 条款 | 状态 |
|---|---|---|
| Subject 头域(生成 + 解析) | 附录 L、§9.2.3 c) | 🟢 `媒体流发送者ID:发送方序列号,媒体流接收者ID:接收方序列号`;实时流序列号首位 0、历史流首位 1;设备发起的 INVITE 必带 |
| 同源码流请求 → 释放旧链路重建 | 附录 K c) | 🟢 按媒体源标识判定;平台不带 Subject 时退化为"同通道重新点播即同源"(现网存在不带 Subject 的平台,一次丢包不该把通道永久锁死) |
| 媒体流丢失 → 会话内 BYE | 附录 K a) | 🟢 推流后端异常/发送失败即发 BYE 并释放链路 |
| 设备离线 → 主动释放媒体链路 | 附录 K b) | 🟢 下线序列:停订阅 → 拆媒体链路(点播发反向 BYE、广播发正向 BYE)→ REGISTER Expires=0 |
| 收流侧丢流监测 | 附录 K a) | 🟢 `RecvStats::idle_for()` 给出"多久没收到包",界面在 >3s 时标记"链路可疑"并提供挂断 |

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
| SDP 协商(PS/90000,y=ssrc,TCP/UDP proto,附录 G `f=` 媒体参数) | FR-7 | ✅ `f=` 字段(附录 G)可解析与生成;`s=` 只用 Play/Playback/Download 三种合法取值 |
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
| 强制关键帧(IFrameCmd) | FR-8 | 🟢 见设备控制:实时编码源真的插 IDR,文件源如实不兑现 |

## 语音广播与语音对讲(§9.12)

标准把对讲拆成两个独立流程的组合(§9.12.2):a) 用 §9.2 实时点播拿设备的音频;
b) 用 §9.12.1 语音广播把中心的音频送到设备。所以下表两行合起来就是完整的双向对讲。

| 功能 | 条款 | 状态 |
|---|---|---|
| 语音广播下行(平台 → 设备,设备收音频) | §9.12.1 | 🟢 完整链路,细节见「其它设备能力」表的「语音广播」行。设备是**语音流接收者**,主动发 INVITE |
| 对讲上行(设备 → 平台,纯音频会话) | §9.12.2 a) + 附录 C.2.4 | 🟢 平台只请求 `m=audio` 时回纯音频 SDP(PCMA / `a=sendonly` / `f=v/////a/1/8/1`),媒体走**裸 G.711A RTP(PT=8)而非 PS**;有配置音源就送真实音频,没有则送 A-law 静音并在统计里如实标注"全程静音" |
| 双向对讲 | §9.12.2 | 🟢 上述两条组合即可;两个方向是独立会话,可同时存在 |
| 广播会话可视化 | — | 🟢 前端「语音广播」卡片:状态灯 / 音量条 / 收包与丢包统计 / 距上次收包 / 挂断按钮 |

> 音量条不是装饰:平台推静音流时包数照样涨,只有把 G.711 解成线性 PCM 算电平
> 才能区分"链路通了"与"对面真的在讲话"。

## 设备控制(平台 → 设备,MANSCDP Control)

平台下发 `<Control>` 命令,设备回 200 + 结果 Notify。对标 uvp-gb28181-sim 的关键补齐项。

| 功能 | CmdType/字段 | 优先级 | 状态 |
|---|---|---|---|
| PTZ 云台控制 | DeviceControl / PTZCmd | P0 | ✅ WVP 下发验证(解析+应答) |
| 预置位设置/调用/删除 | DeviceControl / PTZCmd 指令码 81/82/83 | P1 | ✅ 解析 8 字节码预置位操作,有状态增删,实 SIP 注入验证(设置/删除后 PresetQuery 反映) |
| 看守位设置 | DeviceControl / HomePosition | P1 | 🟢 解析 Enabled/ResetTime/PresetIndex + 应答;更新看守位启用/已设标志供 HomePositionQuery 读回(FR-18) |
| 拉框放大/缩小 | DeviceControl / DragZoomIn / DragZoomOut(`Control::kind()`=拉框放大/缩小) | P1 | 🟢 解析 DragZoom 参数 + 应答 |
| 强制关键帧 | DeviceControl / IFrameCmd(2016 拼写 IFameCmd) | P0 | 🟢 **实时采集源真的生效**:屏幕采集(OpenH264)收到命令后让编码器立刻插 IDR 并重置 GOP 计数;摄像头与 H.265 走 ffmpeg 子进程,无运行中改 GOP 的接口,如实返回"未兑现";文件循环源自带周期 IDR(硬标关键帧会产出坏流)。命令结果进平台命令时间线 |
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
| 设备配置(基本参数) | DeviceConfig / BasicParam | P2 | 🟢 **真实生效**:Name / Expiration / HeartBeatInterval / HeartBeatCount 写入运行时参数,心跳主循环每拍重读(周期与超时次数立即变化),DeviceInfo 与 ConfigDownload 读回新值;空串/0 表示"该项不改"。回归见 `tests/media_keepalive.rs` |

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
| 语音广播(完整收流链路) | Broadcast + INVITE/ACK/BYE | P2 | 🟢 §9.12.1 图27 信令 1~5、14、15、17、18 全通:解析广播通知 → 回广播应答 → **设备作主叫**发带 Subject 的 INVITE(`s=Play` / `m=audio` / `a=recvonly` / `f=v/////a/1/8/1`,端口是真实绑定端口)→ 收 200 学 To-tag → 发 ACK → 收 G.711 音频(统计包数/字节/丢包/乱序/电平)→ 平台 BYE 或设备挂断。端到端回归 `tests/voice_broadcast.rs`(含真实收到 RTP 并校验电平)(FR-36) |
| 设备配置查询 | ConfigDownload / BasicParam + VideoParamOpt | P2 | 🟢 回 200 + ConfigDownload Response,支持 BasicParam(Name/Expiration/HeartBeat*)与 VideoParamOpt(DownloadSpeed/Resolution)按 ConfigType 组合输出(FR-17) |
| 设备配置查询扩展 | ConfigDownload / 全 ConfigType | P2 | 🟢 GB28181-2022 A.2.4.7 全部 ConfigType:BasicParam/VideoParamOpt/VideoParamAttribute/VideoRecordPlan/VideoAlarmRecord/PictureMask/FrameMirror/AlarmReport/OSDConfig/SnapShotConfig 完整;SVACEncodeConfig/SVACDecodeConfig 最小合规占位(FR-37,单测) |
| 设备配置修改 | DeviceConfig / 全配置项 | P2 | 🟢 A.2.3.2 全部配置命令接受回 DeviceConfig 应答(A.2.6.8);含 SVAC/VideoParamAttribute。**BasicParam 真实改变设备行为**,其余配置项接受并应答但不改变模拟行为(FR-37) |
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
| RTCP SR 发送 | FR-32 | 🟢 按 RFC 3550 §6.4.1 构造并**每 5 秒周期发送**(UDP 走 RTP 端口+1,TCP 复用同一条 RFC 4571 连接)。满足 §4.3.6"应支持 RTP/RTCP" |
| RTCP RR 接收(读平台反馈的丢包/抖动) | FR-32 | 🟢 已实现。UDP 下绑 `本端RTP端口+1`(抢不到相邻端口对就退化成只发不收,**媒体链路不因反馈通道建不起来而建不起来**),TCP 下从同一条 RFC 4571 连接按负载类型区分 RTCP;解析 RR/SR/BYE,汇总丢包率、累计丢包(按**有符号 24 位**,重复包多于丢包时为负)、抖动与峰值、按 LSR/DLSR 估算的 RTT。设备控制台「推流质量」卡展示,并**明确区分"平台没回 RR"与"平台回报 0% 丢包"**——把前者画成绿灯等于假装链路已经验证过 |
| RTCP RR 回报(设备作为接收方) | RFC 3550 §6.4.2 | 🟢 语音广播/对讲时设备是收流方,按 RFC 3550 每 5 秒回报 RR:区间丢包率、累计丢包、扩展最高序号(含回绕计数)、抖动(A.8 的整数迭代式)、DLSR;并接收平台 SR 以填出 LSR,平台侧才能算到设备的往返时延。**平台 RTCP 地址从首包 RTP 的源地址学**而不是取 SDP 里声明的地址 —— 过 NAT 时后者往往不可达 |
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
| GB35114 国密扩展 | §8 高安全级别 | 🚫 后续评估。数字证书认证、SIP 信令签名、媒体加密都在该标准里,当前只做数字摘要认证 |
| 联网系统扩展应用 | 附录 A **A.4** | 🚫 不适用。停车场/门禁/道闸/车辆抓拍机等扩展目录类型(detectorType / pmsHostType / dmsHostType / barrierType …)不属于视频 IPC 的能力域 |
| SVAC 编码/解码配置 | A.2.3.2.3 / A.2.3.2.4 | 🚧 最小合规占位(接受并回应答,不真的做 SVAC 编码)。SVAC 是国密视频编码,模拟器不实现编码器 |
| 上级平台(SU)侧 | 全篇 | 🚫 本项目只做下级设备(SIP 设备/网关侧) |
| 多路径传输(`X-RoutePath` / `X-PreferredPath`) | 附录 **H** | 🚫 不适用。原文是"**管理平台宜支持**"级联链路的多路径选择与平台信息列表上报,属于平台间级联能力;下级设备既不参与路径选择也不上报平台信息列表 |
| OSD 烧入画面 | A.2.3.2.11 | 🟡 接受并应用配置命令,但不逐帧烧入图像 —— 逐帧重编码会破坏"轻量循环推流"模型,压测不可用 |
| 强制关键帧对**文件源**生成 IDR | A.2.3.1.7 | 🟡 实时采集源已真的插 IDR;文件循环源只应答 —— 它自带周期 IDR,把某个非关键帧硬标成关键帧会产出对端解不开的坏流 |

---

## 关键 ID 编码

GB28181 设备/通道 ID 为 20 位(附录 E):`中心编码(8) + 行业编码(2) + 类型编码(3) + **网络标识(1)** + 序号(6)`。
第 14 位是**独立的网络标识**,不属于序号 —— 早期实现把它并进序号按 7 位处理,序号超过
100 万时会静默改写网络标识。批量生成虚拟设备按序号递增,上限 `MAX_SERIAL = 999_999`。
类型编码按表 E.1 取值(业务分组 `215`、虚拟组织 `216`、视频通道 `132`、报警通道 `134`)。
详见 `40-protocol/manscdp.md#id-编码` 与 `30-crates/gb28181-protocol.md`。

## 联调平台与运行

单设备联调运行方式见 `10-functional/device-simulation.md#51`;真实 WVP 联调参数见 `40-protocol/gb28181-联调.md`。真实 H.264 测试码流用 `assets/media/gen-test-h264.sh` 生成(需 ffmpeg)。
