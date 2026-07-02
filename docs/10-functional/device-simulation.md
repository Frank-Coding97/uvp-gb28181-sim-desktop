# 功能规格:设备模拟

**状态:草案** · 覆盖 FR-1~FR-11 · 实现见 `30-crates/gb28181-simulator.md`、`sip-core.md`、`gb28181-protocol.md`。

---

## 1. 概述

一台虚拟设备 = 一个完整的 GB28181 下级设备(IPC)状态机。它对上级平台表现得与真实 IPC 一致:注册、保活、应答查询、被点播时推流。

## 2. 设备状态机

```
             connect()
  离线 ──────────────► 注册中 ──401 携鉴权──► 注册中(带 Authorization)
   ▲                     │                          │
   │  注销/失败           │ 200 OK                    │ 200 OK
   │                     ▼                          ▼
   └─────────────────  已注册 ◄──────────────────────┘
                          │
              心跳超时N次   │  收到 INVITE
                          ▼
                  重注册(指数退避)          推流中 ──BYE──► 已注册
```

状态定义(与 `common` / UI DTO 对齐):`离线 Disconnected` / `注册中 Registering` / `已注册 Registered` / `推流中 InCall` / `失败 Failed`。

## 3. 功能点

### 3.1 注册与鉴权(FR-1)
- 首次 REGISTER 无鉴权头 → 平台返回 401 Unauthorized 带 `WWW-Authenticate`(nonce/realm)。
- 计算 Digest MD5 响应,重发带 `Authorization` 的 REGISTER → 200 OK。
- 注销:`Expires: 0` 的 REGISTER。
- 细节见 `40-protocol/sip.md#digest`。

### 3.2 心跳保活(FR-2)
- 已注册后按 `heartbeat_interval`(默认 60s)周期发送 Keepalive(MESSAGE + MANSCDP `Keepalive` XML)。
- 连续 N 次(默认 3)未收到平台响应 → 判定掉线 → 触发重注册,重注册间隔指数退避(如 1s/2s/4s/…上限)。

### 3.3 OPTIONS 探活(FR-3)
- 收到 OPTIONS → 回 200 OK。

### 3.4 传输(FR-4)
- UDP(默认)与 TCP。压测下**共享 socket**,靠 Call-ID/事务路由分发,不为每设备开独立 socket。见 `40-protocol/sip.md#传输`。

### 3.5 目录查询(FR-5)
- 收到 Catalog 查询(MESSAGE) → 回 200 OK + 异步 MESSAGE 携 Catalog 响应 XML,列出本设备的通道。
- 支持 GB-2022 全字段。见 `40-protocol/manscdp.md#catalog`。

### 3.6 设备信息/状态(FR-6)
- DeviceInfo 查询 → 回设备厂商/型号/固件/通道数等。
- DeviceStatus 查询 → 回在线/时间/工作状态等。

### 3.7 实时点播(FR-7、FR-8)
- 收到 INVITE(带 SDP,含平台接收 IP/端口/SSRC) → 回 200 OK(带本端 SDP) → 收到 ACK 后开始推流。
- 推流:读视频源 → PS 封装 → RTP 打包 → 按 SDP 目标发送。
- 收到 BYE → 停流,回 200 OK。
- 媒体细节见 `40-protocol/media-ps-rtp.md`。

### 3.8 版本切换(FR-11)
- GB-2016 / GB-2022 影响 XML 字段集与部分命令,配置项切换。

## 4. 单设备配置项

见 `30-crates/gb28181-simulator.md` 的 `DeviceConfig`:device_id、username、password、server_host/port/domain、transport、heartbeat_interval,以及视频源配置(媒体档、码流文件、码率)。

## 5. 验收

- WVP-Pro 控制台单设备显示**在线**(FR-1/2/4)。
- 平台目录树看到本设备通道(FR-5)。
- 平台点播看到**实时画面**(FR-7/8)。
