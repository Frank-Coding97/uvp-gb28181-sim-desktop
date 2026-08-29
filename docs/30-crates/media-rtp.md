# crate: media-rtp

**状态:M2 完成** · RTP/PS 封装与推流引擎。

> 进度:rtp 包头+切片+统计、PS 封装 H.264、FileSource(C 档 Annex B 切帧循环)、NoneSource(A 档)、LightSource(B 档合成伪流)、push_stream 驱动 ✅ · **PlaybackControl + push_stream_controlled**(回放倍速/暂停/恢复,运行时可调)✅ · **容器解封装**(MP4/FLV/MKV/MOV 经 ffmpeg 转 Annex B,带缓存)✅ · **音视频复合流(G.711A)**(`mux_frame_av` + PSM 声明音频 0x90 + `source.next_audio()`)✅ · 单测 14 全绿。待补:TCP(RFC 4571)推流。

## 容器格式支持(FileSource)

引擎推流只吃 **H.264/H.265 Annex B 裸流**(按起始码切帧)。为支持常见容器(MP4/FLV/MKV/MOV):
`FileSource::from_path` 检测文件非 Annex B 时,调用系统 **ffmpeg** 转封装为 Annex B 裸流
(`-c:v copy -bsf:v h264_mp4toannexb`,失败回退重编码),结果按"源路径+修改时间"缓存到临时目录复用。
ffmpeg 为**可选**外部依赖:仅容器格式需要;裸流(.h264/.h265)不依赖它。ffmpeg 缺失时对容器文件报明确错误。

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
```

### rtp
- `RtpSender`:RTP 头构造、SSRC/序号/时间戳递增、UDP 发送、TCP(RFC 4571)发送;发送速率统计(供指标 send_bitrate)。
- `set_payload_type()`:默认 96(PS);语音流按附录 C.2.4 用 8(PCMA)。
- `send_frame()` 按 MTU 切片;`send_audio()` **不切片** —— 附录 C.2.4 要求一个 RTP 包装整数个
  音频编码帧,切了对端解不出帧。

### receiver
- `AudioReceiver`:绑定本端 UDP 端口收 RTP(语音广播里设备是接收方),解 RTP 头(含 CSRC 与
  扩展头偏移)、按序号估算丢包/乱序、把 G.711 A/μ-law 解成线性 PCM 算**音量电平**。
- `RecvStats::idle_for()`:距上次收包多久 —— 附录 K a) 的媒体流丢失监测依据。
- 为什么要算电平:平台推静音流时包数照样涨,只有电平能区分"链路通了"与"对面在讲话"。

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
