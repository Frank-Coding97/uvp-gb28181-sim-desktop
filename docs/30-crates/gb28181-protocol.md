# crate: gb28181-protocol

**状态:核心完成** · GB28181 应用层:MANSCDP XML + ID 编码。纯数据编解码,无网络/状态机。

> 进度:`id_codec`(拆解/组装/批量生成)✅ · `manscdp`:Keepalive / Query / Catalog / DeviceInfo / DeviceStatus / AlarmNotify / RecordInfo ✅(均对 WVP 验证)· 单测 11。

## 职责

承载在 SIP 消息体内的 MANSCDP XML(目录/设备信息/报警/录像/心跳等)的序列化与反序列化,以及 20 位国标 ID 编解码。

## 模块与公开 API(规划)

```rust
pub mod manscdp;   // MANSCDP XML 消息
pub mod id_codec;  // 20 位 ID 编解码
```

### manscdp(用 quick-xml + serde)
- 查询:`CatalogQuery`、`DeviceInfoQuery`、`DeviceStatusQuery`、`RecordInfoQuery`。
- 应答:`CatalogResponse`(含通道列表,GB-2022 全字段)、`DeviceInfoResponse`、`DeviceStatusResponse`、`RecordInfoResponse`。
- 通知:`Keepalive`、`Alarm`、`MediaStatus`。
- `from_xml(&str)` / `to_xml(&self)`,GB-2016/2022 字段差异用版本参数或字段可选处理。

### id_codec
- 拆解/组装 `中心编码(8)+行业(2)+类型(3)+序号(7)`。
- `gen_device_ids(prefix, start_index, count) -> Vec<DeviceId>`:批量生成(压测 FR-20)。

## 数据类型
各 XML 消息对应强类型 struct,`#[derive(Serialize, Deserialize)]`,字段名按 GB 标准 XML 标签映射。

## 错误
解析失败 → `common::Error::Gb28181(...)`。

## 依赖
common、serde、quick-xml、tracing。

## 里程碑
- M1:Keepalive。
- M2:Catalog / DeviceInfo / DeviceStatus / ID 批量生成。
- M3+:Alarm / RecordInfo / MediaStatus。

## 测试
真实平台样例 XML 的解析往返;ID 编解码边界;批量生成序号正确性。
