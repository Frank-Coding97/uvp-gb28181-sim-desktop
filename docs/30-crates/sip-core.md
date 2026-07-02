# crate: sip-core

**状态:草案(M0 骨架)** · worktree `feat/protocol` · 与 GB28181 无关的通用 SIP 协议栈。

## 职责

提供纯 SIP 协议能力,GB28181 语义不在此。四层:消息、事务、传输、认证。

## 模块与公开 API(规划)

```rust
pub mod message;      // SIP 消息解析/构造
pub mod transaction;  // 事务状态机 + 重传
pub mod transport;    // UDP/TCP 传输
pub mod auth;         // Digest MD5
```

### message
- `SipMessage`(Request/Response)、`Method`(REGISTER/INVITE/ACK/BYE/MESSAGE/OPTIONS)。
- 头字段:`Via`/`From`/`To`/`CSeq`/`Call-ID`/`Contact`/`Expires`/`WWW-Authenticate`/`Authorization`。
- `parse(&[u8]) -> Result<SipMessage>` 与 `to_bytes(&self) -> Vec<u8>`。

### transaction
- 客户端/服务端事务状态机;UDP 超时重传(T1 起,指数退避到 T2);事务与响应按 branch/CSeq 匹配。

### transport
- `trait SipTransport { async fn send(...); fn recv_stream(...) }`。
- **压测关键**:共享 socket + Call-ID/事务路由分发,避免每设备一 socket(见 `10-functional/stress-testing.md#42`)。
- TCP 走 RFC 4571 解帧。

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
