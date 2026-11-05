# crate: sip-core

**状态:核心完成** · 与 GB28181 无关的通用 SIP 协议栈。

> 进度:`message` ✅ · `auth`(Digest MD5)✅ · `transport`(UDP 共享 socket / TCP 共享连接池 + Call-ID 路由)✅ · `transaction`(客户端事务 + T1 退避重传)✅ · INVITE/ACK/BYE 会话事务已实现并验证 ✅ · `sdp` 模块 ✅。

## 职责

提供纯 SIP 协议能力,GB28181 语义不在此。四层:消息、事务、传输、认证。

## 模块与公开 API(规划)

```rust
pub mod message;      // SIP 消息解析/构造
pub mod transaction;  // 事务状态机 + 重传
pub mod transport;    // UDP/TCP 传输
pub mod auth;         // Digest MD5
pub mod sdp;          // SDP 协商(点播/回放)
```

### message
- `SipMessage`(Request/Response)、`Method`(REGISTER/INVITE/ACK/BYE/MESSAGE/OPTIONS/CANCEL/SUBSCRIBE/NOTIFY/INFO)。
- 头字段:`Via`/`From`/`To`/`CSeq`/`Call-ID`/`Contact`/`Expires`/`WWW-Authenticate`/`Authorization`。
- `parse(&[u8]) -> Result<SipMessage>` 与 `to_bytes(&self) -> Vec<u8>`。

### transaction
- 客户端/服务端事务状态机;UDP 超时重传(T1 起,指数退避到 T2);事务与响应按 branch/CSeq 匹配。

### transport
- `UdpTransport`:共享 socket,后台接收循环解析后分发。
- **响应**按 `Call-ID` 路由到发起事务的设备队列(`register(call_id)`)。
- **入站请求**(平台主动发来的 OPTIONS / MESSAGE 查询)按目标设备 AOR 路由:取 Request-URI 的
  user 部分(即 device_id),投递到该设备的入站队列(`register_inbound(device_id)`)。
- **压测关键**:所有虚拟设备复用少量 socket,避免每设备一 socket 耗尽端口(见 `10-functional/stress-testing.md#42`)。

### transport::tcp(SIP over TCP,RFC 3261 §18)

- `TcpTransport`:**共享连接池**。所有虚拟设备复用 N 条 TCP 连接(默认 4,可配 1–64),
  设备按 ID 哈希固定映射到某条连接 —— FD 占用等于连接池大小,与设备数量解耦。
- `FrameDecoder`:TCP 无报文边界,按 `Content-Length` 切帧;支持粘包/半包/逐字节到达,
  缺省 `Content-Length` 按 0 处理,头部与消息体各有上限防止缓冲膨胀。
- 响应沿入站连接回发(RFC 3261 §18.2.2):收到请求时登记 `Call-ID → 连接`,
  设备回 200 时命中登记,多连接下不会串线。
- 断线自动重连(200ms 起指数退避到 5s),重连后补发断线期间积压的报文。
- 建连失败在 3 秒内返回错误,不让配置错误伪装成事务超时。
- 事务层在 TCP 下**不做应用层重传**(重传由 TCP 负责),只保留一次发送 + 总预算等待。
- 注意:RFC 4571 是 **RTP over TCP** 的分帧规则,与 SIP 信令 TCP 无关;媒体面分帧见 `media-rtp`。

### auth
- 解析 401/407 挑战,计算 `response = MD5(HA1:nonce:HA2)`,构造 `Authorization`。GB28181 用 MD5。

## 错误
统一用 `common::Error::Sip(...)`;超时单独可辨识(供 `timeout` 归因)。

## 依赖
common、tokio、bytes、tracing、md-5。

## 里程碑
- M1:REGISTER/心跳/OPTIONS 所需的消息、UDP 传输、事务、Digest。
- M2:INVITE/ACK/BYE 事务(点播)。

## 测试
消息解析/序列化往返;Digest 响应对拍已知向量;事务超时重传时序(用模拟时钟)。协议逻辑必配单测(NFR-7)。
