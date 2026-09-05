# crate: sip-core

**状态:核心完成** · 与 GB28181 无关的通用 SIP 协议栈。

> 进度:`message` ✅ · `auth`(Digest MD5)✅ · `transport`(UDP 共享 socket + Call-ID 路由)✅ · `transaction`(客户端事务 + T1 退避重传)✅ · INVITE/ACK/BYE 会话事务已实现并验证 ✅ · `sdp` 模块 ✅。

## 职责

提供纯 SIP 协议能力,GB28181 语义不在此。四层:消息、事务、传输、认证。

## 模块与公开 API(规划)

```rust
pub mod message;      // SIP 消息解析/构造
pub mod transaction;  // 事务状态机 + 重传
pub mod transport;    // UDP/TCP 传输
pub mod auth;         // Digest MD5
pub mod sdp;          // SDP 协商(点播/回放/语音广播)+ 附录 G 的 f= 媒体参数
pub mod subject;      // 附录 L Subject 头域(媒体流标识,附录 K 的同源判断依据)
```

### message
- `SipMessage`(Request/Response)、`Method`(REGISTER/INVITE/ACK/BYE/MESSAGE/OPTIONS/CANCEL/SUBSCRIBE/NOTIFY/INFO)。
- 头字段:`Via`/`From`/`To`/`CSeq`/`Call-ID`/`Contact`/`Expires`/`WWW-Authenticate`/`Authorization`。
- `parse(&[u8]) -> Result<SipMessage>` 与 `to_bytes(&self) -> Vec<u8>`。

### transaction
- 客户端/服务端事务状态机;UDP 超时重传(T1 起,指数退避到 T2);事务与响应按 branch/CSeq 匹配。
- `client_transact()` 也用于**设备作主叫**的场景(语音广播的 INVITE):INVITE 收到 1xx 后停止
  重传、一路等到 Timer B。ACK 不由它发 —— ACK 是独立事务,由调用方在学到对端 To-tag 后自己发。

### sdp
- `SessionDescription`:解析/生成;`is_audio_only()` 区分纯音频会话(语音对讲上行)。
- `MediaFormat`:附录 G 的 `f=v/编码格式/分辨率/帧率/码率类型/码率大小a/编码格式/码率大小/采样率`。
  视频段与音频段之间没有分隔符,音频段直接以 `a` 开头(如 `f=v/////a/1/8/1`)。
- `build_broadcast_offer()`:语音广播的 offer(`s=Play` + `m=audio` + `a=recvonly` + `y=` + `f=`)。
- `new_device_audio_response()`:纯音频会话的应答(PCMA / PT=8 / `a=sendonly`)。

### subject
- 附录 L:`媒体流发送者ID:发送方媒体流序列号,媒体流接收者ID:接收方媒体流序列号`。
- `media_source()` 给出附录 K c) 用来判断"是否已在发送同一路码流"的标识。
- 实时流序列号首位取 0、历史流首位取 1。

### transport
- `UdpTransport`:共享 socket,后台接收循环按「可信来源 → 每 IP 令牌桶 → 解析 → 分发」处理。
- `TcpTransport`:SIP 流式重组,首报文/空闲超时与 64 条入站连接上限;TCP 已完成,不再是规划项。
- **响应**按 `Call-ID` 路由到发起事务的设备队列(`register(call_id)`)。
- **入站请求**(平台主动发来的 OPTIONS / MESSAGE 查询)按目标设备 AOR 路由:取 Request-URI 的
  user 部分(即 device_id),投递到该设备的入站队列(`register_inbound(device_id)`)。
- **UDP 来源限流**:`UdpRateLimitConfig` 默认每来源 IP 200 个 MTU 等价令牌/s、突发 400;
  数据报按 `ceil(bytes/1300)` 计费,所以 65 KiB 大包不能伪装成一个普通包。桶最多 4096 个来源,
  空闲 60s 回收;同 IP 换源端口或 IPv4-mapped IPv6 不能绕过。
- **白名单与限流叠加**:生产入口使用 `bind_with_trusted_ips`，非空白名单与 Router 同时构造，
  在 UDP 接收循环/TCP accept 循环启动前已生效，消除 bind 后 setter 的竞态窗口；空集合明确报错，
  不会退化成放行。先做平台 IP 白名单,未命中者不占桶;命中白名单的来源仍受限流。
  低层测试 API 仍可显式关闭白名单，但桌面单设备与压测生产路径不会使用该降级。
- **反放大**:限流拒绝一律静默丢弃,不回 429/503。每条通过的 UDP Request 仅为同一
  `SocketAddr` 产生最多 3×入站字节的短期响应信用;UDP Response 必须消耗信用且不超过
  1300 B 安全 MTU。服务端事务键也包含来源地址,不同主机不能靠复制 branch/CSeq 命中缓存响应。
- `TransportStats` 的 `rate_limited` / `rate_limited_bytes` / `anti_amplification_blocked`
  会汇入压测 `local_pressure`;安全策略丢包时报告必须标为不可信,不能把它归咎于平台。
- **压测关键**:所有虚拟设备复用少量 socket,避免每设备一 socket 耗尽端口(见 `10-functional/stress-testing.md#42`)。

### auth
- 解析 401/407 挑战,计算 `response = MD5(HA1:nonce:HA2)`,构造 `Authorization`。该能力用于设备
  作为 UAC 完成 REGISTER Digest。
- 当前**不提供平台→设备的 Digest 服务端校验**：尚无平台反向凭据模型、服务端 nonce TTL、
  `nc/cnonce` 重放检测与常量时间 verifier；不能仅凭请求带有 Authorization 就宣称已认证。

## 错误
统一用 `common::Error::Sip(...)`;超时单独可辨识(供 `timeout` 归因)。

## 依赖
common、tokio、bytes、tracing、md-5。

## 里程碑
- M1:REGISTER/心跳/OPTIONS 所需的消息、UDP 传输、事务、Digest。
- M2:INVITE/ACK/BYE 事务(点播)。

## 测试
消息解析/序列化往返;Digest 响应对拍已知向量;事务超时重传时序(用模拟时钟)。协议逻辑必配单测(NFR-7)。
