# crate: gb28181-protocol

**状态:核心完成** · GB28181 应用层:MANSCDP XML + ID 编码。纯数据编解码,无网络/状态机。

> 进度:`id_codec`(拆解/组装/批量生成/附录 E 类型码/附录 J 查询范围)✅ · `manscdp`:Keepalive / Query / Catalog(A.2.1.9 全字段 + Info 子结构)/ DeviceInfo / DeviceStatus / AlarmNotify / RecordInfo ✅(均对 WVP 验证)· `capture_position`(附录 O)✅ · 单测 56。

## 职责

承载在 SIP 消息体内的 MANSCDP XML(目录/设备信息/报警/录像/心跳等)的序列化与反序列化,以及 20 位国标 ID 编解码。

## 模块与公开 API(规划)

```rust
pub mod manscdp;   // MANSCDP XML 消息
pub mod id_codec;  // 20 位 ID 编解码
```

### manscdp(用 quick-xml + serde)
- 查询:`CatalogQuery`、`DeviceInfoQuery`、`DeviceStatusQuery`、`RecordInfoQuery`、`PresetQuery`、`ConfigDownload`(设备配置查询)。
- 应答:`CatalogResponse`(含通道列表,GB-2022 全字段)、`DeviceInfoResponse`、`DeviceStatusResponse`、`RecordInfoResponse`、`PresetQueryResponse`(有状态预置位表)、`ConfigDownloadResponse`(BasicParam)。
  `CatalogResponse.device_list` 与 `RecordInfoResponse.record_list` 是 `Option`:
  **附录 M** 要求未查询出结果时 `SumNum` 取 0 且不携带记录列表,所以空结果时整个列表元素都不输出。
- 目录项:`CatalogItem`(A.2.1.9 的 25 个字段,**顺序严格按 XSD sequence**;2016 独有项插在
  2022 顺序的对应位置,按版本置 None 后两版输出顺序都合法)+ `CatalogItemInfo`(Info 子结构 23 字段)。
  构造器 `CatalogItem::folder()`(附录 J 的目录节点:区域/业务分组/虚拟组织,不带设备专属字段)与
  `::device()`(补齐必选默认值 Address/RegisterWay=1/Secrecy=0)。
  **`Password` 永不输出** —— 目录应答会经过链路每一跳,把凭据放进去等于主动广播。
- `capture_position`:**附录 O** 的摄像机采集部位类型 7 位层次码(大类 3 + 中类 2 + 小类 2)+
  常用代码常量 + `is_valid()`。只校验结构不内置整张表 —— 表 O.1 会随行业规定扩充,
  写死会拒掉合法的新代码。
- 通知:`Keepalive`、`Alarm`、`MediaStatus`、`CatalogNotify`(目录订阅对话内 NOTIFY)。
- 设备控制(平台 → 设备 `Control`):PTZ 解析 —— `ptz_motion()` 解出方向/速度(`PtzMotion`),`preset_op()` 解出预置位操作 `PresetAction{Set,Call,Delete}`(8 字节指令码 0x81/0x82/0x83)+ 预置位号;`HomePosition`(看守位)、`DragZoom`(拉框放大/缩小)。`Control::kind()` 返回中文命令名。
- `from_xml(&str)` / `to_xml(&self)`,GB-2016/2022 字段差异用版本参数或字段可选处理。

### id_codec
- 拆解/组装 `中心编码(8)+行业编码(2)+类型编码(3)+网络标识(1)+序号(6)`(附录 E)。
  `IdParts` 里 `network_id` 是**独立字段**,序号上限 `MAX_SERIAL = 999_999`。
- `gen_device_ids(prefix, start_index, count) -> Vec<DeviceId>`:批量生成(压测 FR-20)。
- `gen_child_id` / `gen_child_id_on_network`:按类型码生成子节点 ID(可指定网络标识)。
- `type_code_name(code)`:类型码 → 中文名,用于诊断"平台把这个节点认成了什么"。
- `CatalogNode`:目录树节点。除 id/类型/名称/父节点/区划码/状态外,还带 A.2.1.9 的
  逐通道可选字段 `address` / `longitude` / `latitude` / `ptz_type` /
  `capture_position_type`(留空即用设备级默认值)。因含 f64 不能 derive `Eq`。
- `catalog_query_scope(tree, query_id, root_id) -> (CatalogQueryScope, Vec<&CatalogNode>)`:
  **附录 J** 的目录查询返回范围。本设备 ID→整树;2/4/6/8 位区划码→CivilCode 前缀匹配;
  树内节点→该节点及其后代;不在目录里→空集合(配合附录 M 的 SumNum=0)。

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
