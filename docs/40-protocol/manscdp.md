# 协议规格:MANSCDP XML

**状态:草案** · GB28181 应用层消息(承载在 SIP MESSAGE/INVITE 体内)。实现见 `30-crates/gb28181-protocol.md`。参考 GB/T 28181-2022 附录。

## 载体

`Content-Type: Application/MANSCDP+xml`,XML 根元素按类型分 `<Query>` / `<Response>` / `<Notify>` / `<Control>`,含 `<CmdType>` 与 `<SN>`(序列号)。

## 主要消息

### <a id="catalog"></a>目录 Catalog
- 查询:`<Query><CmdType>Catalog</CmdType><SN>..</SN><DeviceID>..</DeviceID></Query>`
- 应答:`<Response><CmdType>Catalog</CmdType>...<DeviceList Num="N"><Item>..通道..</Item></DeviceList></Response>`
- 通道 Item(GB-2022 全字段):DeviceID、Name、Manufacturer、Model、Owner、CivilCode、Address、Parental、ParentID、Status、Longitude、Latitude 等。

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

### 报警订阅 Alarm(✅ WVP 验证)
- 平台 `SUBSCRIBE + Alarm` → 设备回带 tag 的 200 并**记录订阅对话**(Call-ID/tags/Event/Expires + 平台地址)。
- 此后 `report_alarm` 上报的报警(Alarm Notify XML)走**对话内 SIP NOTIFY**(复用 `notify_in_dialog`),WVP code=200;若无订阅(如手动触发、平台未订阅)则退化为独立 MESSAGE(WVP 同样接受)。设备下线时清理对话。

## 版本差异
GB-2016 与 GB-2022 在 Catalog 字段集、部分命令上有差异;实现用可选字段 + 版本开关处理(FR-11)。

## <a id="id-编码"></a>ID 编码
20 位:`中心编码(8) + 行业(2) + 类型(3) + 序号(7)`。类型码如 132=视频通道、200=设备。批量生成按序号递增(FR-20),见 `30-crates/gb28181-protocol.md` 的 `id_codec`。

## 编解码策略
用 quick-xml + serde 派生;以真实平台(WVP 等)抓包样例做往返测试,保证兼容。
