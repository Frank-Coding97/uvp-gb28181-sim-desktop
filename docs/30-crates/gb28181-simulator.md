# crate: gb28181-simulator

**状态:核心完成并 WVP 验证** · 单设备完整状态机 = 组合 sip-core + gb28181-protocol + media-rtp。

> 进度(均对真实 WVP 验证):注册闭环+心跳+OPTIONS+SUBSCRIBE 应答、run 循环 ✅ · Catalog/DeviceInfo/DeviceStatus/RecordInfo/ConfigDownload/PresetQuery 查询(独立 MESSAGE 应答)✅ · INVITE/ACK/BYE 会话 + UDP/TCP 推流(WVP 出 FLV 流)✅ · report_alarm 报警上报 ✅ · report_position 移动位置上报 ✅ · **移动位置订阅**(SUBSCRIBE MobilePosition + Interval → 周期 NOTIFY)✅ WVP 验证(每 5s 上报,逐条 code=200) · **目录订阅**(SUBSCRIBE Catalog → 对话内 SIP NOTIFY)✅ WVP code=200 · **报警订阅**(SUBSCRIBE Alarm → 记录对话,report_alarm 走对话内 NOTIFY)✅ WVP code=200 · **回放控制**(会话内 INFO/MANSRTSP → 倍速/暂停/恢复)🟢 单测+真机 INFO 回 200 · **录像下载**(INVITE s=Download + downloadspeed → N 倍速推流)✅ 实测 4× · **语音广播/对讲**(反向 INVITE 收 G.711 + 纯音频会话)🟢 端到端测试 · **强制关键帧**(实时采集源真的插 IDR)🟢 · **附录 I/J/M/N**(版本协商 / 查询范围 / 串行应答 / 批量通知)🟢 · 单测 33 + 集成 11。

## 职责

把 SIP 栈、GB 协议、媒体引擎组合成一台完整下级设备。一个 `DeviceSimulator` = 一台虚拟 IPC。压测调度器批量创建并驱动。

## 公开 API(规划)

```rust
pub struct DeviceConfig { /* 见下 */ }
pub struct DeviceSimulator { /* 状态机 */ }

impl DeviceSimulator {
    pub fn new(config: DeviceConfig) -> Self;
    pub fn with_observer(config: DeviceConfig, observer: Arc<dyn DeviceObserver>) -> Self;
    // 注册 → 启动入站应答(OPTIONS/查询/控制/订阅)→ 周期心跳,直到 shutdown 完成。
    pub async fn run(
        self: Arc<Self>,
        transport: Arc<UdpTransport>,
        local_host: String,
        local_port: u16,
        shutdown: impl Future<Output = ()>,
    );
    pub async fn register(&self, transport, local_host, local_port) -> Result<DeviceState>;
    pub fn state(&self) -> DeviceState;         // Disconnected/Registering/Registered/InCall/Failed
}
```

除注册/心跳外,已实现:Catalog/DeviceInfo/DeviceStatus/RecordInfo/ConfigDownload/PresetQuery 查询应答、预置位 CRUD(设置/调用/删除,有状态)、PTZ 及设备控制、目录/报警/移动位置订阅 + 周期 NOTIFY、网络校时、回放 INFO 倍速/暂停/恢复、录像下载。`report_alarm`/`report_position` 支持主动上报(有订阅走对话内 NOTIFY,否则独立 MESSAGE)。

与规范性附录相关的几处行为:

| 能力 | 条款 | 实现要点 |
|---|---|---|
| 协议版本协商 | 附录 I | REGISTER 带 `X-GB-Ver`,从注册响应学习平台声明(`peer_protocol_version()`);**对端未声明 3.0 时不发 2022 独有消息**(如 A.2.5.8 实时视音频回传通知) |
| 目录查询返回范围 | 附录 J | `catalog_items_for(query_id)` 按被查询对象分流,不是一律整棵树;未知对象回空并 `warn` |
| 多响应消息串行发送 | 附录 M | `send_reply_messages_serial()` 等前一条 200 再发下一条;**某一片失败仍继续发剩余分片** —— 目录残缺比慢更糟 |
| 目录变更批量通知 | 附录 N | `PendingCatalogChanges` 缓冲(同一通道只留终态)+ `flush_catalog_changes()`;阈值 `CATALOG_NOTIFY_BATCH_ITEMS=100`、`CATALOG_NOTIFY_DEBOUNCE=1s`(**有意偏离**附录 N 的"如 1 分钟"示例值,理由见代码注释)、`CATALOG_NOTIFY_MAX_DELAY=60s` 硬上限。用**事件驱动的延迟任务**而不是全局 ticker:压测万级设备时后者会带来上万次无谓唤醒 |
| 推流质量遥测 | RFC 3550 | `media_telemetry()` 返回 `MediaTelemetrySnapshot`(发送包数/字节/SR 数/本地失败 + 平台 RTCP RR 的丢包率/累计丢包/抖动/RTT),供设备控制台「推流质量」卡 |

## 数据类型

`DeviceConfig`(M0 已定义骨架):device_id、username、password、server_host/port/domain、transport、heartbeat_interval_secs;M2 增视频源配置(媒体档/文件/码率)。

`DeviceState`:与 `10-functional/device-simulation.md#2` 状态机、UI DTO 一致。

## 行为

严格实现 `10-functional/device-simulation.md`:注册+Digest、心跳+重注册(指数退避)、OPTIONS、Catalog/DeviceInfo 应答、INVITE→推流→BYE。

## 错误
向上返回 `common::Result`;单设备异常隔离,不得拖垮同批其它设备(NFR-5)。

## 依赖
common、sip-core、gb28181-protocol、media-rtp、tokio、tracing。

## 里程碑
- M1:注册 + 心跳 + OPTIONS(单设备 WVP 上线)。
- M2:目录/设备信息应答 + INVITE 推流。
- M3+:订阅通知(目录/报警/移动位置周期 NOTIFY)、预置位 CRUD + PTZ/看守位/拉框控制、回放 INFO 倍速控制、录像下载、网络校时。

## 测试
状态机转移单测(mock 传输,注入 401/200/超时);心跳超时→重注册退避时序;点播 INVITE→ACK→推流→BYE 流程。
