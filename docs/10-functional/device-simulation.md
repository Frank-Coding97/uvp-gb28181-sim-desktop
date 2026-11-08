# 功能规格:设备模拟

**状态:已实现(核心真机验证)** · 覆盖 FR-1~FR-11 及后续设备控制/订阅/校时/媒体扩展 · 实现见 `30-crates/gb28181-simulator.md`、`sip-core.md`、`gb28181-protocol.md`。

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
- UDP(默认)与 TCP。压测下两种传输都**共享承载**:UDP 复用少量 socket,TCP 复用少量连接,
  靠 Call-ID/事务路由分发,不为每设备开独立 socket/连接 —— FD 占用与设备数无关。
  TCP 连接池默认 4 条(可配 1–64),设备按 ID 哈希固定落到某条连接,平台的"注册连接 → 下发连接"
  绑定关系不会被打破。见 `40-protocol/sip.md#传输` 与 `30-crates/sip-core.md`。

### 3.5 目录查询(FR-5)
- 收到 Catalog 查询(MESSAGE) → 回 200 OK + 异步 MESSAGE 携 Catalog 响应 XML,列出本设备的通道。
- 支持 GB-2022 全字段。见 `40-protocol/manscdp.md#catalog`。
- 桌面模拟器提供「下级平台业务目录（215/216）」模板(`business-platform`)：设备根下挂 1 个业务分组、
  2 个顶层虚拟组织与 1 个嵌套虚拟组织，通道分别归属各虚拟组织，用于联调业务目录解析与通道归属过滤。
- 该模板只用于验证 Catalog 树结构：真实下级平台应以 200 类型联网系统 ID 作为 Catalog 根，
  而单设备模拟器的 `register_one` 示例以设备 ID 为根，不代表完整的平台级联网系统身份。

### 3.6 设备信息/状态(FR-6)
- DeviceInfo 查询 → 回设备厂商/型号/固件/通道数等。
- DeviceStatus 查询 → 回在线/时间/工作状态等。

### 3.7 实时点播(FR-7、FR-8)
- 收到 INVITE(带 SDP,含平台接收 IP/端口/SSRC) → 回 200 OK(带本端 SDP) → **发完 200 即启动推流**(TCP 模式短暂延时等平台建监听);ACK 到达仅记录、不作为推流触发。BYE/CANCEL 停流。
- 推流:读视频源 → PS 封装 → RTP 打包 → 按 SDP 目标发送。
- 收到 BYE → 停流,回 200 OK。
- 媒体细节见 `40-protocol/media-ps-rtp.md`。

### 3.8 版本切换(FR-11)
- GB-2016 / GB-2022 影响 XML 字段集与部分命令,配置项切换。

### 3.9 设备控制(平台 → 设备,DeviceControl)
- PTZ 云台方向/变倍控制(8 字节码,位映射见 `40-protocol/manscdp.md`)。
- 预置位设置/调用/删除(有状态,PresetQuery 反映);看守位 HomePosition;拉框 DragZoom。
- 强制关键帧、录像控制、布防/撤防、报警复位、远程启动。

### 3.10 查询扩展
- 设备配置查询 ConfigDownload(BasicParam:名称/有效期/心跳)、预置位查询 PresetQuery。

### 3.11 订阅与通知
- 平台 SUBSCRIBE:Catalog / Alarm → 设备在订阅对话内回 SIP NOTIFY;MobilePosition → 按 Interval 周期上报位置。
- 设备主动上报:报警(report_alarm)、GPS 位置(report_position)。

### 3.12 网络校时
- 解析平台 REGISTER 200 OK 的 `Date` 头,记录时钟偏移,后续 MANSCDP 时间戳(心跳/报警/位置)对齐平台时间(见 `common::clock`)。
- 校时来源两档:`SipDate`(默认)与 `Ntp`(用户显式启用)。**NTP 生效期间 SIP Date 只记录、不覆盖偏移**;NTP 查询失败时回退到最近一次 SIP Date 偏移,没有回退值时保持原偏移(不清零)。
- NTP 采用最小 SNTP(RFC 5905)查询:校验应答 mode(4/5)、stratum(1~15)、leap≠3 与发起时间戳;偏移 = `((T2−T1)+(T3−T4))/2`。周期刷新仅在设备运行期执行,间隔可配(默认 600s)。
- 本模块**不修改操作系统时钟**,只维护逻辑协议时钟;`设备校时`页面展示平台基准时间、本机时间、偏移、来源凭证、SIP 回退值与最近失败原因(FR-46)。

### 3.13 回放控制与媒体扩展
- 会话内 SIP INFO(MANSRTSP):PLAY/PAUSE + Scale 倍速、暂停/恢复。
- 录像下载:INVITE `s=Download` + `downloadspeed` → 按倍速推流。
- 音视频复合流:视频源含音频轨时 PS 复用 G.711A 音频;支持 MP4/FLV/MKV/MOV 容器(ffmpeg 转封装,见 `40-protocol/media-ps-rtp.md`)。

## 4. 单设备配置项

见 `30-crates/gb28181-simulator.md` 的 `DeviceConfig`:device_id、username、password、server_host/port/domain、transport、**audio_transport**(语音广播/对讲下行的媒体传输:UDP / TCP 主动 / TCP 被动,与信令传输独立)、heartbeat_interval_secs、channels(通道列表)、device_info、video_source、video_fps、light_bitrate_kbps(B 档伪流码率)、gb_version。

### 4.1 语音广播 / 对讲下行(平台 → 设备)

平台下发 `Broadcast` 通知后,设备以 UAC 身份反向 INVITE,`s=Broadcast`、`a=recvonly`、PCMA/PCMU;平台 200 OK 给出媒体地址后回 ACK,并按 `audio_transport` 收流:

| 对讲流传输模式 | offer | 收流方式 |
|---|---|---|
| UDP | `m=audio <port> RTP/AVP 8 0` | 绑定 UDP 端口收 RTP |
| TCP 主动 | `m=audio <port> TCP/RTP/AVP 8 0` + `a=setup:active` | 收到 answer 后由设备 connect 平台 |
| TCP 被动 | 同上 + `a=setup:passive` | 设备监听,等平台连入 |

默认选择 **TCP 主动**(设备连平台),适合设备位于 NAT 后、平台侧可被动监听的环境;平台与设备直连时可改用 UDP 或 TCP 被动。

RTP 按 RFC 4571 解帧(TCP),G.711A/U 解码为 8kHz 单声道 PCM;桌面端经 `broadcast_audio` 事件交给前端 Web Audio 播放,状态(通道/传输/包数)经 `broadcast_state` 显示在首页。平台 BYE 或设备下线时释放端口并尽力回 BYE。

## 5. 验收

- WVP-Pro 控制台单设备显示**在线**(FR-1/2/4)。
- 平台目录树看到本设备通道(FR-5)。
- 平台点播看到**实时画面**(FR-7/8)。

### 5.1 M1 联调运行方式

用 `gb28181-simulator` 的示例 `register_one` 对真实 WVP 验证单设备上线:

```bash
# 1) 起 WVP-Pro(见 40-protocol/gb28181-联调.md)
# 2) 运行单设备示例(参数经环境变量传入)
SERVER_HOST=<WVP主机IP> SERVER_PORT=5060 \
SERVER_DOMAIN=34020000002000000001 \
DEVICE_ID=34020000001320000001 PASSWORD=<与WVP一致> \
cargo run -p gb28181-simulator --example register_one
```

预期:控制台日志出现"注册成功",WVP「国标设备」列表中该设备变为**在线**,并周期收到心跳。

业务目录模板可通过 `CATALOG_TEMPLATE=business-platform` 加载；查看两版本报文时分别设置 `GB_VERSION=2016` 或 `GB_VERSION=2022`。
