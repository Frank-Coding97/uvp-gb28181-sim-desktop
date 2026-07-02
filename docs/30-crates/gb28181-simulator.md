# crate: gb28181-simulator

**状态:开发中(M1)** · worktree `feat/protocol` · 单设备完整状态机 = 组合 sip-core + gb28181-protocol + media-rtp。

> 进度:`builder`(REGISTER/MESSAGE/200 OK 构造)✅ · `register()` 注册闭环(REGISTER→401→Digest→200)✅ · `send_keepalive()` 心跳 ✅ · `answer_inbound()` OPTIONS/MESSAGE 回 200 ✅ · `run()` 主循环(注册+入站应答+周期心跳+掉线重注册指数退避)✅ · 目录/点播 🚧 M2。已单测,待对真实 WVP 联调(roadmap M1 验收)。

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
