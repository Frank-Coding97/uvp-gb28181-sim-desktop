# crate: media-rtp

**状态:M2 完成** · RTP/PS 封装与推流引擎。

> 进度:rtp 包头+切片+统计、PS 封装 H.264、FileSource(C 档 Annex B 切帧循环)、NoneSource(A 档)、LightSource(B 档合成伪流)、push_stream 驱动 ✅ · **PlaybackControl + push_stream_controlled**(回放倍速/暂停/恢复,运行时可调)✅ · **容器解封装**(MP4/FLV/MKV/MOV 经 ffmpeg 转 Annex B,带安全原子缓存)✅ · **音视频复合流(G.711A)**(`mux_frame_av` + PSM 声明音频 0x90 + `source.next_audio()`)✅ · **TCP(RFC 4571)推流**(ACTIVE 主动连 / PASSIVE 被动等连)✅ · **音频收发**(裸 G.711 上行 + `AudioReceiver` 下行收流与电平)🟢 · **RTCP 双向**(周期发 SR、接收平台 RR;设备作为接收方按 RFC 3550 回报 RR)🟢 · 单测 87 全绿。

## 容器格式支持(FileSource)

引擎推流只吃 **H.264/H.265 Annex B 裸流**(按起始码切帧)。为支持常见容器(MP4/FLV/MKV/MOV):
`FileSource::from_path_av` 检测文件非 Annex B 时,调用系统 **ffmpeg** **归一化**为 Annex B 裸流,
结果按 canonical path + size + 高精度 mtime + **目标帧率**缓存。ffmpeg 为**可选**外部依赖:
仅容器格式需要;裸流(.h264/.h265)不依赖它。ffmpeg 缺失时对容器文件报明确错误。

**为什么不再用 `-c:v copy` 只做转封装**:副本会保留容器里的原始帧率与 B 帧解码顺序,
而推流端是固定帧率取帧、按发送顺序直接打 PTS。源 30fps 而推流 25fps 时速度被拖慢 17%;
有 B 帧时解码序 ≠ 显示序,帧序错乱会表现为抖动,严重时解码花屏。归一化参数:

```
-vf fps=<推流帧率>,format=yuv420p -c:v libx264 -preset veryfast -crf 20 -bf 0 -g <2×fps>
```

代价是首次加载一次转码(缓存后瞬时返回);收益是本地视频的每一帧显示时刻严格等于
`发送序号 × 1/fps`,与推流节拍一致。

## 时间基与采集帧率

三条实时源(屏幕 / 摄像头 / 本地视频)必须在**同一时间基**上产出:

- **时间戳按真实媒体时钟推进**(`pusher`):由 `Instant` 累积"非暂停的经过时间 × 倍速",
  RTP 时间戳与 PS PTS 都由它推导,而不是"已发帧数 × 帧间隔"。后者一旦供帧抖动跳过几拍、
  或缓冲快进丢掉一段帧,时间线就永久落后墙钟,平台表现为延迟单调增长、音画渐移。
- **摄像头强制 CFR**:AVFoundation 输入按 30/60 采集,而推流按设备配置帧率消费。
  编码侧 `-vf ...,fps=<目标帧率>` 做帧率归一化;否则缓冲每秒净增 5 帧,触顶后快进丢到
  最近关键帧,表现为每几秒瞬跳一次(与机器性能无关)。
- **屏幕采集**由 `SCStreamConfiguration::with_fps()` 直接按目标帧率出帧,不存在上述错配。
- **编码质量**:摄像头 x264 用 `veryfast` + `high` profile + `nal-hrd=cbr:force-cfr=1`,
  固定 GOP、关闭场景切换。`ultrafast` 关闭 CABAC/亚像素搜索,运动场景同码率下块效应明显。

## 采集丢弃可观测性

`capture_drop_stats()` 返回 (重同步丢弃字节数, 通道满/编码器跳帧数),进程级累计。
`SessionTelemetry` 在会话建立时记录基线,`capture_drops()` 返回本会话增量,供设备控制台
区分"本机供不上帧"与"网络丢包"。计数覆盖三处此前静默的丢弃:屏幕 `raw_tx` 通道满、
OpenH264 `enable_skip_frame` 返回空码流、Annex B 切帧缓冲超限重同步。

### 临时文件与外部进程安全边界

- 固定 `$TEMP/uvp-gb28181` 只用于兼容 Tauri asset scope；任何写入前都用
  `symlink_metadata` 验证它不是符号链接且确为目录，Unix 强制并复核 `0700`。
  初始化失败会中止媒体操作，不能再吞错后继续写未知目标。
- FFmpeg **永不直接写最终缓存名**。Rust 在私有目录中用 128-bit 随机名 +
  `create_new` 占位 staging(Unix `0600`)；成功后验证普通非空文件，视频还必须有
  Annex-B 起始码，最后同目录原子发布。错误/超时由 RAII 删除 staging。
  缓存命中同样拒绝 symlink、非普通文件、空文件或无效 Annex-B，避免 TOCTOU/半文件。
- 所有短命 FFmpeg 命令走统一有界执行器：`stdin=null`，stdout/stderr 持续排空且各只
  保留 256 KiB，探测 3 秒、转码 120 秒；每个成功 spawn 的进程在所有返回路径都会
  `kill + wait` 收尸。实时采集是长驻进程，不设总时长，但有首帧 deadline。
- 实时 FFmpeg 在 spawn 后立即进入 `StartupChildren` armed guard，后续 pipe、handler、
  编码器初始化任一步失败都会 kill+wait。摄像头 FFREPORT 使用每会话高熵 0600 文件，
  启动失败或 `LiveSource::drop` 在子进程收尾后删除。
- 上层停止等待超时后必须 `JoinHandle::abort()` 再 await；仅丢弃 handle 会 detach 任务，
  使持有 `LiveSource` 的 future 不被 drop，最终留下 FFmpeg 孤儿进程。

## 现成组件评估(调研结论:不引入)

| 组件 | 定位 | 结论 |
|---|---|---|
| **gb28181-rs** | Rust 设备侧 UAC:SIP + MANSCDP + PS/RTP 推流,时间戳取自真实采集时间 | 与本项目覆盖面高度重合,但其 SIP 事务/目录/压测模型与本仓库不同,整体替换等于重写。**已借鉴其"按真实采集时间打时间戳"的做法**,未引依赖 |
| **ffmpeg**(已在用) | 采集 / 编码 / 封装 | 继续作为统一编码后端:摄像头走 avfoundation,屏幕 H.265 走 `hevc_videotoolbox`;本轮补齐 CBR/CFR/高质量 preset |
| **scap / pinray** | Rust 屏幕采集(封装 ScreenCaptureKit) | 项目已直接用 `screencapturekit` crate 并拿到像素缓冲,再套一层只增加依赖与回调抽象,无收益 |
| **nokhwa** | 跨平台摄像头采集 | 只做采集不含硬件编码;现有 ffmpeg avfoundation 已覆盖内置/USB/ Continuity Camera,不换 |
| **Cap(开源录屏)** | 采集/编码/封装分 crate、muxer 独立进程 | 架构可借鉴(编码与封装分离),但其产物是 MP4 录制,不涉及 PS/RTP,不能直接复用 |
| **GStreamer** | 全功能媒体管线(mpegpsmux 等) | 依赖体积与运行时分发成本远超收益;GB28181 的 PS-over-RTP 仍要自己写 payloader |

结论:**采集与封装继续用现有自研 + ffmpeg**,真正缺的不是"轮子",而是时间基与帧率归一化 ——
本轮已在不引入新依赖的前提下补齐。

## 职责

把码流(或伪码流)按 RTP 打包发送给上级平台。支持压测三档媒体强度(见 `10-functional/stress-testing.md#2`)。

## 模块与公开 API(规划)

```rust
pub enum RtpMode { Udp, TcpActive, TcpPassive }  // 已定义
pub mod rtp;      // RTP 打包/发送
pub mod ps;       // PS 封装
pub mod source;   // 视频源抽象
pub mod pusher;   // 视频源→PS→RTP 按帧率推流的驱动(push_stream / push_stream_controlled / PlaybackControl)
                  // 以及 push_audio_stream:纯音频上行(裸 G.711A,附录 C.2.4)
pub mod receiver; // RTP 音频**接收**(语音广播下行,§9.12.1)
pub mod rtcp;     // RTCP 复合包解析 + 接收报告(RR)反馈统计
```

### rtp
- `RtpSender`:RTP 头构造、SSRC/序号/时间戳递增、UDP 发送、TCP(RFC 4571)发送;发送速率统计(供指标 send_bitrate)。
- `set_payload_type()`:默认 96(PS);语音流按附录 C.2.4 用 8(PCMA)。
- `send_frame()` 按 MTU 切片;`send_audio()` **不切片** —— 附录 C.2.4 要求一个 RTP 包装整数个
  音频编码帧,切了对端解不出帧。**TCP 下整帧的 RTP 分片拼成一块缓冲、一次 `write_all` 写出**
  (每包仍带 RFC 4571 两字节长度前缀):把一帧的几十次 syscall 压成一次,平台读得慢时也不容易
  在包之间被调度打断。
- **RTCP 端口对**:UDP 下 `bind_rtp_pair()` 抢 `p` / `p+1` 相邻端口对(最多重试 8 次,
  抢不到就退化成只发 SR 不收 RR)。SR **从 RTCP socket 发出**,源端口 = RTP 端口 +1 ——
  否则严格按 RFC 3550 实现的平台会把 RR 回到一个我们没在听的端口。
- **RR 接收**:UDP 从专用 RTCP socket 收,TCP 从同一条 RFC 4571 连接的读端收
  (`Channel::Tcp` 只留 `OwnedWriteHalf`,读端交给后台任务)。收报任务随 `RtpSender`
  一同销毁(`impl Drop` 里 abort),否则会话结束后任务还抱着 socket 不放。
- `feedback()` / `local_rtcp_port()` / `started()`:把反馈句柄与端口交给上层;
  `pusher::SessionTelemetry` 经 `PlaybackControl` 把它们递给设备侧(发送器是在推流任务
  内部创建的,借已经共享的控制块递出来,免得给推流函数再加出参)。

### rtcp
- `parse_compound()`:解析 RTCP 复合包(RFC 3550 §6.1)。遇到结构不合法就在该处停止,
  把已解析出的部分返回 —— 半个包也比整个丢掉有用,现网确实存在把长度字段写错的平台。
- `ReceptionReport`:报告块(24 字节)。`cumulative_lost` 按**有符号 24 位**解析 ——
  按无符号读会在重复包多于丢包时得到 1600 万这种荒谬数字。
  `rtt_us(now_ntp_mid)` 按 §6.4.1 算法估 RTT(LSR=0 或 DLSR 大于总耗时时返回 None,不猜)。
- `RtcpFeedback`:平台侧收流质量汇总(丢包率、累计丢包、抖动 + 峰值、RTT、BYE 标记),
  全原子量;`snapshot()` 出可序列化快照给界面与压测报表。
- `looks_like_rtcp()`:TCP(RFC 4571)下 RTP 与 RTCP 共用一条连接,按 RFC 5761 §4
  的建议只认 PT 200~213 —— RTP 带 marker 位时第 2 字节也能到 255,不能只看数值大小。
- `build_rtcp_rr()`:构造 RR。设备在语音广播/对讲里是接收方,RFC 3550 要求周期回报。
- **为什么手写解析而不引 webrtc-rs 的 `rtcp` crate**:需要的只是几个定长大端字段的读取,
  而那个 crate 会带进 `webrtc-util` 等一串传递依赖。这个"轮子"本身就是 40 行位移运算,
  引依赖的成本高于收益。

### receiver
- `AudioReceiver`:绑定本端 UDP 端口收 RTP(语音广播里设备是接收方),解 RTP 头(含 CSRC 与
  扩展头偏移)、按序号估算丢包/乱序、把 G.711 A/μ-law 解成线性 PCM 算**音量电平**。
- `RecvStats::idle_for()`:距上次收包多久 —— 附录 K a) 的媒体流丢失监测依据。
- 为什么要算电平:平台推静音流时包数照样涨,只有电平能区分"链路通了"与"对面在讲话"。
- **RR 回报**:在 `收流端口+1` 绑 RTCP socket,每 5 秒(`RR_INTERVAL`)向平台回报接收报告
  (区间丢包率、累计丢包、扩展最高序号含回绕计数、抖动按 RFC 3550 A.8 的整数迭代式、DLSR);
  同时接收平台 SR 填出 LSR,平台侧才能算到设备的往返时延。
  **平台 RTCP 地址从首包 RTP 的源地址学**而不是取 SDP 里声明的地址 —— 过 NAT 时后者往往不可达。
  抢不到 `+1` 端口就只收流不回报:媒体链路不能因为反馈通道建不起来而建不起来。

### ps
- `PsMuxer`:PS 包头/系统头/PES 封装,复用 H.264/H.265(+ 音频后续)。

### source(可插拔,对应压测三档)
```rust
pub trait VideoSource: Send {
    fn next_frame(&mut self) -> Option<Frame>;      // 返回一帧(裸码流/伪包)
    fn next_audio(&mut self) -> Vec<Vec<u8>> {}     // 该帧期间的音频分包(默认空;FileSource 覆盖为 G.711A)
    fn request_keyframe(&mut self) -> bool { false } // 强制关键帧(A.2.3.1.7);返回是否真的兑现
}
```
- `request_keyframe()` 默认 false 是刻意的:只有自带编码器的源能立刻插 IDR(屏幕采集走
  OpenH264);ffmpeg 子进程路径(摄像头、H.265)没有运行中改 GOP 的接口,文件源硬标关键帧
  会产出坏流。如实返回让上层能把"未兑现"报给用户,而不是假装成功。
- `NoneSource` — 不产帧(A 档,只维持信令)。
- `LightSource` — 生成低码率伪 PS 包(B 档)。
- `FileSource` — 循环读取 H.264/H.265 文件(C 档 / 单设备联调)。

## 数据类型
`Frame`(码流分片 + 时间戳 + 关键帧标记)、`RtpMode`。

## 错误
`common::Error::Media(...)`;发送 IO 错误归 `network`。

## 依赖
common、tokio、bytes、tracing、rand。

## 里程碑
- M2:`FileSource` + `PsMuxer` + `RtpSender`(UDP),打通真实点播推流。
- M3:`NoneSource`/`LightSource`,支撑压测三档。

## 测试
RTP 头字段/序号递增单测;PS 封装对已知帧的字节结构;FileSource 循环边界。发送用 loopback socket 做集成测试。
