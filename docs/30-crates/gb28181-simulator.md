# crate: gb28181-simulator

**状态:核心完成并 WVP 验证** · 单设备完整状态机 = 组合 sip-core + gb28181-protocol + media-rtp。

> 进度(均对真实 WVP 验证):注册闭环+心跳+OPTIONS+SUBSCRIBE 应答、run 循环 ✅ · Catalog/DeviceInfo/DeviceStatus/RecordInfo/ConfigDownload/PresetQuery 查询(独立 MESSAGE 应答)✅ · INVITE/ACK/BYE 会话 + UDP/TCP 推流(WVP 出 FLV 流)✅ · report_alarm 报警上报 ✅ · report_position 移动位置上报 ✅ · **移动位置订阅**(SUBSCRIBE MobilePosition + Interval → 周期 NOTIFY)✅ WVP 验证(每 5s 上报,逐条 code=200) · **目录订阅**(SUBSCRIBE Catalog → 对话内 SIP NOTIFY)✅ WVP code=200 · **报警订阅**(SUBSCRIBE Alarm → 记录对话,report_alarm 走对话内 NOTIFY)✅ WVP code=200 · **回放控制**(会话内 INFO/MANSRTSP → 倍速/暂停/恢复)🟢 单测+真机 INFO 回 200 · **录像下载**(INVITE s=Download + downloadspeed → N 倍速推流)✅ 实测 4× · 单测 12。待补:强制关键帧。

## 职责

把 SIP 栈、GB 协议、媒体引擎组合成一台完整下级设备。一个 `DeviceSimulator` = 一台虚拟 IPC。压测调度器批量创建并驱动。

## 公开 API(规划)

```rust
pub struct DeviceConfig { /* 见下 */ }
pub struct DeviceSimulator { /* 状态机 */ }

impl DeviceSimulator {
    pub fn new(config: DeviceConfig) -> Self;
    pub async fn run(&self, transport: SharedTransport) -> Result<()>; // 注册→心跳→应答
    pub fn state(&self) -> DeviceState;         // Disconnected/Registering/Registered/InCall/Failed
    pub async fn shutdown(&self);
}
```

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

## 测试
状态机转移单测(mock 传输,注入 401/200/超时);心跳超时→重注册退避时序;点播 INVITE→ACK→推流→BYE 流程。
