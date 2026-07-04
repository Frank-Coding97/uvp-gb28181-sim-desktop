# 协议规格:SIP

**状态:已实现** · GB28181 信令承载于 SIP。本文定义本项目用到的 SIP 子集,实现见 `30-crates/sip-core.md`。参考 RFC 3261 + GB/T 28181-2022。

## 消息子集

只实现 GB28181 必需的方法:`REGISTER`(注册/注销)、`MESSAGE`(MANSCDP 载体:心跳/查询/应答/通知)、`INVITE`/`ACK`/`BYE`/`CANCEL`(点播会话建立/停止/取消)、`OPTIONS`(探活)、`SUBSCRIBE`/`NOTIFY`(订阅与对话内通知)、`INFO`(会话内回放控制)。

## 头字段

必备:`Via`(branch=z9hG4bK 前缀)、`From`(含 tag)、`To`、`Call-ID`、`CSeq`、`Contact`、`Max-Forwards`、`Content-Length`、`Content-Type`(MANSCDP 用 `Application/MANSCDP+xml`)。注册用 `Expires`。鉴权用 `WWW-Authenticate`/`Authorization`。

## <a id="digest"></a>Digest 鉴权(MD5)

1. 首个 REGISTER 无 `Authorization` → 平台回 `401` 带 `WWW-Authenticate: Digest realm=..., nonce=...`。
2. 计算:
   - `HA1 = MD5(username:realm:password)`
   - `HA2 = MD5(method:uri)`
   - `response = MD5(HA1:nonce:HA2)`
3. 重发 REGISTER 带 `Authorization: Digest username=..., realm=..., nonce=..., uri=..., response=...`。
4. 平台 `200 OK` → 注册成功。

## <a id="传输"></a>传输

- **UDP**(默认):无连接,事务层负责超时重传(T1≈500ms 起指数退避)。
- **TCP**:面向连接;RTP over TCP 用 RFC 4571 解帧(2 字节大端长度 + 包体)。
- **压测共享 socket**:所有虚拟设备复用少量 socket,收到响应按 `Call-ID` + 事务 branch 路由到对应设备,避免每设备一 socket 导致端口/FD 耗尽。见 `10-functional/stress-testing.md#42`。

## 事务与对话

- 事务:请求 + 其响应,按 branch/CSeq 匹配,管理重传与超时。
- 对话(INVITE 建立):由 Call-ID + From-tag + To-tag 标识,ACK/BYE 在对话内。

## 注册/心跳时序

```
设备                         平台
 │──REGISTER(无鉴权)────────►│
 │◄─401 (nonce)──────────────│
 │──REGISTER(Authorization)─►│
 │◄─200 OK───────────────────│   已注册
 │──MESSAGE(Keepalive)──────►│   每 60s
 │◄─200 OK───────────────────│
```

## 网络校时(SIP Date 头)

平台在 REGISTER 200 OK 携带 `Date` 头(如 `2026-07-04T00:23:58.186`,平台本地时间、无时区)。设备据此校时:`common::clock::sync_from_date_header` 解析并记录“平台时间 − 本地时间”偏移(不改操作系统时钟),生成 MANSCDP 时间戳(心跳/报警/位置)时统一叠加,使设备上报时间与平台对齐。实测偏移 +28794s(本机 UTC ↔ 平台 UTC+8)。
