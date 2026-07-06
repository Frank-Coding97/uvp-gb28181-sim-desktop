# 协议规格:MANSCDP XML

**状态:已实现** · GB28181 应用层消息(承载在 SIP MESSAGE/INVITE 体内)。实现见 `30-crates/gb28181-protocol.md`。参考 GB/T 28181-2016 与 GB/T 28181-2022 附录标准。

## 载体

`Content-Type: Application/MANSCDP+xml`,XML 根元素按类型分 `<Query>` / `<Response>` / `<Notify>` / `<Control>`,含 `<CmdType>` 与 `<SN>`(序列号)。

## 主要消息

### <a id="catalog"></a>目录 Catalog
- 查询:`<Query><CmdType>Catalog</CmdType><SN>..</SN><DeviceID>..</DeviceID></Query>`
- 应答:`<Response><CmdType>Catalog</CmdType>...<DeviceList Num="N"><Item>..通道..</Item></DeviceList></Response>`
- 通道 Item(与 `CatalogItem` 结构一致):DeviceID、Name、Manufacturer、Model、CivilCode、Parental、ParentID、Status;GB-2022 新增 SecurityLevelCode、IPAddress、Port(仅 2022 版输出)。

### 设备信息 DeviceInfo
- 应答字段:DeviceName、Manufacturer、Model、Firmware、Channel(通道数)。

### 设备状态 DeviceStatus
- 应答字段:Online、Status、Encode、Record、DeviceTime、Alarmstatus 等。

### 心跳 Keepalive(Notify)
- `<Notify><CmdType>Keepalive</CmdType><SN>..</SN><DeviceID>..</DeviceID><Status>OK</Status></Notify>`

### 报警 Alarm(Notify) / 录像 RecordInfo
- M3+ 实现,字段按 GB 标准。

### 设备配置查询 ConfigDownload
- 查询:`<Query><CmdType>ConfigDownload</CmdType><SN>..</SN><DeviceID>..</DeviceID><ConfigType>BasicParam</ConfigType></Query>`。
- 应答:`<Response><CmdType>ConfigDownload</CmdType><SN>..</SN><DeviceID>..</DeviceID><Result>OK</Result><BasicParam><Name/><Expiration/><HeartBeatInterval/><HeartBeatCount/></BasicParam></Response>`。
- 设备实现:先回 200 OK,再以独立 MESSAGE 发回上述应答(与目录查询同路径)。目前仅实现最常用的 `BasicParam`(注册有效期/心跳间隔/心跳超时次数),其它 ConfigType 按需扩展。

### 移动位置订阅 MobilePosition(SUBSCRIBE 携 Query)
- 平台经 SIP `SUBSCRIBE` 下发,体为 `<Query><CmdType>MobilePosition</CmdType><SN>..</SN><DeviceID>..</DeviceID><Interval>N</Interval></Query>`。
- `Interval`(秒):设备据此**周期**上报移动位置 NOTIFY(体为 `MobilePosition` Notify,含经纬度/时间);缺省或 0 时兜底 5 秒。`Interval` 为 `Query` 的可选字段,普通查询不含。
- 设备实现:收到 SUBSCRIBE 先回 200 OK,再启动周期任务复用移动位置 NOTIFY 上报路径;设备下线或重复订阅时清理旧任务。见 `30-crates/gb28181-simulator.md`。

### 预置位查询 PresetQuery
- 查询:`<Query><CmdType>PresetQuery</CmdType><SN>..</SN><DeviceID>..</DeviceID></Query>`。
- 应答:`<Response><CmdType>PresetQuery</CmdType><SN>..</SN><DeviceID>..</DeviceID><PresetList Num="N"><Item><PresetID/><PresetName/></Item>...</PresetList></Response>`。
- 设备实现:先回 200,再以独立 MESSAGE 发回应答(与目录查询同路径);模拟器返回两个内置预置位。

### 目录订阅变更通知 Catalog Notify(✅ WVP 验证)
- 结构:`<Notify><CmdType>Catalog</CmdType><SN>..</SN><DeviceID>..</DeviceID><SumNum>N</SumNum><DeviceList Num="N"><Item><DeviceID/><Name/><Event>ON|OFF|ADD|DEL|UPDATE</Event><Status/></Item></DeviceList></Notify>`(`CatalogNotify` 类型)。
- **关键**:目录/报警订阅的变更通知须在 SUBSCRIBE 建立的 SIP **NOTIFY 对话内**发送 —— 不能用独立 MESSAGE(平台不 200,实测超时)。正确做法:①对 SUBSCRIBE 回 200 时给 To 补一个 tag;②发 NOTIFY 请求沿用订阅 Call-ID,From/To 相对 SUBSCRIBE 反转(本端 From 带同一 tag,平台为 To 带其 tag),并带 `Event`(回显订阅事件)与 `Subscription-State: active;expires=N` 头。见 `builder::notify_in_dialog`。
- 设备实现:收到 Catalog SUBSCRIBE → 在对话内回发一条 NOTIFY(全通道 Event=ON)使平台同步目录。模拟器目录静态,发一次即可(真实设备按变更推送)。

### 录像下载 SDP(INVITE s=Download)
- 平台点录像下载时 INVITE 的 SDP 会话名 `s=Download`,并带 `a=downloadspeed:N`(N 倍速)。设备据此按 N 倍速推流(倍速映射到 `PlaybackControl`,复用回放推流路径)。历史回放为 `s=Playback`(1×)。`SessionDescription` 提供 `is_download()`/`is_playback()`/`download_speed` 解析。
- 实测:`s=Download` + `downloadspeed:4` → 设备以 4×25fps=100 包/s 推流(1× 约 29/s)。

### 回放控制 MANSRTSP(会话内 SIP INFO)
- 平台在回放会话(INVITE 建立)内经 SIP **INFO** 下发 MANSRTSP 控制命令,体形如:
  - 倍速/恢复:`PLAY MANSRTSP/1.0\r\nCSeq: n\r\nScale: 2.0\r\n\r\n`(Scale 为倍速,缺省视为 1.0)
  - 暂停:`PAUSE MANSRTSP/1.0\r\nCSeq: n\r\nPauseTime: now\r\n\r\n`
- 设备实现:`Method::Info` 入站 → 解析体首行 PLAY/PAUSE + Scale → 调整推流的 `PlaybackControl`(倍速改取帧间隔、暂停保持会话不发包)→ 回 200 OK。无活跃会话也回 200(避免平台重传)。见 `media-rtp::PlaybackControl` 与 `push_stream_controlled`。

### 设备控制 DeviceControl(平台 → 设备)
`<Control>` 消息,按出现的子元素判类型:PTZCmd / IFameCmd(强制关键帧)/ RecordCmd(录像)/ GuardCmd(布防撤防)/ AlarmCmd(报警复位)/ TeleBoot(远程启动)/ HomePosition(看守位)/ DragZoomIn·DragZoomOut(拉框)。设备解析后回 `<Response><CmdType>DeviceControl</CmdType>…<Result>OK</Result></Response>`。

**PTZCmd 8 字节码(GB/T 28181-2016 附录 A.3.1,字节以十六进制串给出)**:
`字节0=A5 | 字节1=版本+校验(0F) | 字节2=地址 | 字节3=指令码 | 字节4=水平速度 | 字节5=垂直速度 | 字节6=变倍速度(高4位) | 字节7=校验和`

- **方向/变倍指令码(字节3)位定义**(实测 WVP 抓包核对,与标准一致):
  `右=0x01 · 左=0x02 · 下=0x04 · 上=0x08 · 放大=0x10 · 缩小=0x20`;全 0 为停止。
  速度:字节4=水平(0-255)、字节5=垂直(0-255)、字节6 高 4 位=变倍(0-15)。
- **预置位指令(字节3 高位置 1)**:`0x81 设置 · 0x82 调用 · 0x83 删除`;**预置位编号在字节5**(字节4 恒为 0x01)。
  例:调用预置位 7 = `A50F0182 01 07 00 3F`。⚠️ 编号在字节5 不是字节4 —— 曾因读错字节导致所有预置位识别成 1,已按真实 WVP 码修正。
- **看守位 HomePosition**:子元素 `Enabled`(1/0)、`ResetTime`(秒)、`PresetIndex`(归位预置位号)。
- **拉框 DragZoom**:子元素 Length/Width/MidPointX/MidPointY/LengthX/LengthY。
- 解析见 `Control::ptz_motion()` / `preset_op()`(`gb28181-protocol/src/manscdp.rs`)。设备维护有状态预置位表(设置/删除即时生效,PresetQuery 返回);预置位调用与方向命令经 `DeviceEvent::Ptz`/`PtzPresetCall` 驱动桌面云台可视化。

> **教训**:PTZ 8 字节码的位映射与字节位不要凭标准文档"推断",不同平台/文档表述有出入 —— 本项目的方向位、预置位号字节都是**在真实 WVP 上逐命令抓包核对**后才定的(修过两个方向/字节位 bug)。

### 扩展查询应答(FR-17,M6·P1)

以下查询均**先回 200 OK,再以独立 MESSAGE 发回 `<Response>`**(与 Catalog/ConfigDownload 同路径)。骨架据上游 uvp-gb28181-sim 真实实现核对。

- **报警状态 AlarmStatus**(查询与应答同 CmdType 字符串):
  - GB-2022:`<Response><CmdType>AlarmStatus</CmdType><SN/><DeviceID/><Result>OK</Result><Num>1</Num><Item><DeviceID>{报警通道ID}</DeviceID><DutyStatus>ALARM|OFFDUTY</DutyStatus></Item></Response>`。
  - GB-2016:用 `<NotNumber>0|1</NotNumber>` 替换 `Num/Item` 块。
  - `DutyStatus=ALARM` 当设备处布防/报警态,否则 `OFFDUTY`。
- **看守位查询 HomePositionQuery**:`<Response><CmdType>HomePositionQuery</CmdType><SN/><DeviceID/><Enabled>0|1</Enabled><ResetTime>30</ResetTime><PresetIndex>0|1</PresetIndex></Response>`。ResetTime 固定 30(平台下发的 ResetTime 不落存);PresetIndex 表"有无看守位"(1/0),非真实预置位号。
- **存储卡状态 StorageCardStatusQuery**:`<Response><CmdType>StorageCardStatusQuery</CmdType><SN/><DeviceID/><SumNum>1</SumNum><StorageList Num="1"><Item><CardNum>0</CardNum><Status>Normal</Status><TotalCapacity>32768</TotalCapacity><RemainingSpace>24576</RemainingSpace></Item></StorageList></Response>`。容量单位 MB(32G 总 / 24G 余),模拟固定值。
- **巡航轨迹列表 CruiseTrackListQuery**:`<Response>...<SumNum>{n}</SumNum><TrackList Num="{n}"><Item><GroupID>{轨迹号}</GroupID><Name>巡航 {轨迹号}</Name></Item>...</TrackList></Response>`。无轨迹时 `<TrackList Num="0"/>`。
- **巡航轨迹详情 CruiseTrackQuery**:读请求 `<GroupID>`(退回 `<TrackNum>`,默认 1)。`<Response>...<GroupID>{t}</GroupID><SumNum>{n}</SumNum><PresetList Num="{n}"><Item><PresetID>{p}</PresetID><Speed>5</Speed><DwellTime>3</DwellTime></Item>...</PresetList></Response>`。Speed/DwellTime 固定 5/3。无点时 `<PresetList Num="0"/>`。
- **PTZ 精准状态 PTZPreciseStatusQuery**(GB-2022):`<Response>...<Pan>123.45</Pan><Tilt>-15.00</Tilt><Zoom>3.50</Zoom></Response>`。取最近一次 PTZPreciseCtrl 值,格式 %.2f。
- **移动位置单次查询 MobilePosition**:与订阅周期上报的 `MobilePosition` Notify 体一致,但单发一条(不建周期任务)。

### ConfigDownload 扩展 VideoParamOpt(FR-17)
`<ConfigType>` 支持斜杠分隔组合;新增 `VideoParamOpt` 块:`<VideoParamOpt><DownloadSpeed>1/2/4</DownloadSpeed><Resolution>{分辨率标签}</Resolution></VideoParamOpt>`。只输出被请求的块(BasicParam/VideoParamOpt),Result 恒 OK。

### 扩展设备控制(FR-18,M6·P2)

`<Control>` 内新增子命令(响应均为 SIP 200 OK,无 MANSCDP 响应体,除 PTZPreciseCtrl 状态供 PTZPreciseStatusQuery 读回):

- **精确云台 PTZPreciseCtrl**(GB-2022):子元素 `<Pan>`(0-360.00) `<Tilt>`(-30~90) `<Zoom>`(≥1.00),Float。设备存为最近姿态。
- **巡航控制(PTZCmd 8 字节,byte3 指令码)**:`0x84` 增点(byte4=轨迹# byte5=预置#)、`0x85` 删点、`0x86` 速度(byte5=speed)、`0x87` 停留(byte5=秒)、`0x88` 启动(byte4=轨迹#,0=停止巡航)。
- **辅助控制(PTZCmd)**:byte3 `0x89`(开)/`0x8A`(关),byte4=辅助号:`1=雨刷 2=红外灯 3=加热 4=除雾 5=制冷`(海康/大华事实标准)。
- **Focus 聚焦/光圈**:PTZCmd byte3 的 bit6/bit7(现仅实现变倍 bit4/bit5)。
- **目标跟踪 TargetTrack**:`<TargetTrack><Mode>Auto|Manual|Stop</Mode><ObjectID/><Speed/></TargetTrack>`(Mode 白名单,其它忽略仍 200;Speed 1-255);旧式 `<TargetTrack>Auto</TargetTrack>` 亦作 Mode。纯 XML 无字节码。
- **格式化 SD 卡 FormatSDCard**:`<FormatSDCard>1</FormatSDCard><DiskNum>N</DiskNum>`(或旧式值即卡号)。模拟设备无实际存储,仅应答记录。

### 平台下发抓拍(FR-19,M6·P3)
- **SnapShotCmd**(7.4 旧路径):`<Control>...<SnapShotCmd>..</SnapShotCmd>`。触发后经 **Alarm Notify** 上报(priority=4/method=5/type=5,DeviceID=报警通道),无图片上传。
- **SnapShotConfig**(GB-2022 §9.5 真上传):`<Control>...<SnapShotConfig><SessionID/><UploadURL/><SnapNum>1-10</SnapNum><Interval>秒</Interval></SnapShotConfig>`。SessionID+UploadURL 必填。串行拍 SnapNum 张,每张:采 JPEG → **HTTP PUT**(Content-Type image/jpeg,body 裸 JPEG;UploadURL 末尾为 `/` 则拼 `{SnapShotID}.jpg`;2xx 成功,失败退避 [1000,2000,4000]ms)→ 发完成 NOTIFY;张间隔 Interval。**SSRF 白名单**:仅 http/https,host 须在上传白名单(空名单=全拒),拒环回/链路本地/组播/元数据地址。
  - 完成 NOTIFY:`<Notify><CmdType>Notify</CmdType><SubCmd>SnapShot</SubCmd><SN/><DeviceID/><SessionID/><SnapShotID>{YYYYMMDDThhmmss_序号}</SnapShotID><Time/><StoragePath/></Notify>`(旧式 buildLegacy 用 `<CmdType>SnapShot</CmdType>` 且无 SubCmd)。

### 在线升级 DeviceUpgrade(FR-30,M6·P3)
- 平台:`<Control>...<DeviceUpgrade><Firmware/><SessionID/><FileURL/></DeviceUpgrade>`。
- 设备回 200 后启动 **4 步进度**:percent [0,30,60,100],步间 1500ms,100% 后 5000ms 清理。每步发 `DeviceUpgradeResult` NOTIFY:`<Notify><CmdType>DeviceUpgradeResult</CmdType><SN/><DeviceID/><SessionID/><Firmware/><Result>0|1</Result><Percent>0-100</Percent></Notify>`。percent<100→Result=0(进行中),==100→Result=1(成功);2=失败(定义但模拟不发)。SN=cseq&0xFFFF。

### 主动通知 MediaStatus(FR-31,M6·P4)
- `<Notify><CmdType>MediaStatus</CmdType><SN/><DeviceID/><NotifyType>{code}</NotifyType></Notify>`。code:`121`=历史媒体文件发送结束(回放/下载完成)、`122`=录像异常、`123`=存储满。121 在回放/下载会话结尾发;122/123 主动触发且 fan-out 给 Alarm 订阅者。

### 报警订阅 Alarm(✅ WVP 验证)
- 平台 `SUBSCRIBE + Alarm` → 设备回带 tag 的 200 并**记录订阅对话**(Call-ID/tags/Event/Expires + 平台地址)。
- 此后 `report_alarm` 上报的报警(Alarm Notify XML)走**对话内 SIP NOTIFY**(复用 `notify_in_dialog`),WVP code=200;若无订阅(如手动触发、平台未订阅)则退化为独立 MESSAGE(WVP 同样接受)。设备下线时清理对话。

### 实时视音频回传通知 VideoUploadNotify(FR-37,GB28181-2022 A.2.5.8)
- 设备主动上报"实时视音频已开始回传"通知,供平台感知回传状态。
- 结构:`<Notify><CmdType>VideoUploadNotify</CmdType><SN/><DeviceID/><Time/><Longitude/><Latitude/></Notify>`。CmdType/SN/DeviceID/Time 必选,经纬度可选。
- 设备实现:`VideoUploadNotify` 类型 + `report_video_upload` 主动发独立 MESSAGE(与 report_alarm 同路径)。

### 强制关键帧拼写兼容(GB28181-2022 A.2.3.1.7)
- 2022 标准元素名为 `IFrameCmd`,2016 版/多数设备沿用 `IFameCmd`(少个 r)。设备侧 Control **同时接受两种拼写**(serde alias),避免严格 2022 平台的关键帧命令解析不到。

### RecordInfo 应答补必选 Name(GB28181-2022 A.2.6.7)
- A.2.6.7 规定录像检索应答含必选 `<Name>`(设备/区域名称),位于 SumNum 前。`RecordInfoResponse` 补该字段(取设备名)。

### 标准合规说明(结构偏差取舍)
以下应答的元素名/结构与 2022 XSD 字面存在差异,但均**据 WVP/上游 uvp-gb28181-sim 真实实现核对并真机验证**,为保证互通**保持现状**(不盲目改标准字面):
- SDCardStatus 应答(StorageList/CardNum vs 标准 SDCardStatusInfo/ID)
- CruiseTrackList/CruiseTrackQuery 应答(TrackList/Item/GroupID vs 标准 CruiseTrackList/CruiseTrack/Number)
- HomePositionQuery 应答(顶层平铺 vs 标准 HomePosition 子元素)
- MobilePosition 通知(2016 平铺式 vs 2022 DeviceList 列表式)
- 抓拍完成通知(Notify+SubCmd=SnapShot vs 标准 UploadSnapShotFinished)
- DeviceUpgradeResult(Result/Percent 进度语义 vs 标准 UpgradeResult 成败语义)

## 版本差异
GB-2016 与 GB-2022 在 Catalog 字段集、部分命令上有差异;实现用可选字段 + 版本开关处理(FR-11)。

## <a id="id-编码"></a>ID 编码
20 位:`中心编码(8) + 行业(2) + 类型(3) + 序号(7)`。类型码如 132=视频通道、200=设备。批量生成按序号递增(FR-20),见 `30-crates/gb28181-protocol.md` 的 `id_codec`。

## 编解码策略
用 quick-xml + serde 派生;以真实平台(WVP 等)抓包样例做往返测试,保证兼容。
