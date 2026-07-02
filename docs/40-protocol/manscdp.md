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

## 版本差异
GB-2016 与 GB-2022 在 Catalog 字段集、部分命令上有差异;实现用可选字段 + 版本开关处理(FR-11)。

## <a id="id-编码"></a>ID 编码
20 位:`中心编码(8) + 行业(2) + 类型(3) + 序号(7)`。类型码如 132=视频通道、200=设备。批量生成按序号递增(FR-20),见 `30-crates/gb28181-protocol.md` 的 `id_codec`。

## 编解码策略
用 quick-xml + serde 派生;以真实平台(WVP 等)抓包样例做往返测试,保证兼容。
