# crate: media-rtp

**状态:M2 完成 + v2 P1** · RTP/PS 封装与推流引擎。演进方向见 [`20-architecture/media-pipeline-v2.md`](../20-architecture/media-pipeline-v2.md)。

> 进度:rtp 包头+切片+统计、PS 封装 H.264、FileSource(C 档 Annex B 切帧循环)、NoneSource(A 档)、LightSource(B 档合成伪流)、push_stream 驱动 ✅ · **PlaybackControl + push_stream_controlled**(回放倍速/暂停/恢复,运行时可调)✅ · **容器解封装**(MP4/FLV/MKV/MOV 经 ffmpeg 转 Annex B,带缓存)✅ · **音视频复合流(G.711A)**(`mux_frame_av` + PSM 声明音频 0x90 + `source.next_audio()`)✅ · **Windows 摄像头/屏幕采集**(DirectShow + ddagrab,共享编码与真实 PTS)✅ · **TCP 主动/被动推流**(RFC 4571)✅ · **v2 P1**:`Bytes` 零拷贝、订阅端事件唤醒、PS 音视频交织、按会话关闭音频 ✅。待补:Windows 屏幕音频。

## v2 P1 变更摘要

- **数据**:`TimedVideoAu.data` / `TimedAudioAu.data` 为 `bytes::Bytes`,订阅者之间共享,clone 只增加引用计数。
- **推流入口**:`push_stream_to(source, RtpTarget, PushOptions, control, stop)`。
  - `RtpTarget::{Udp, TcpActive, TcpPassive { listener, accept_timeout }}`:被动模式的监听端口由调用方在 200 OK 前绑定;accept 期间收到 stop 立即返回。
  - `PushOptions { ssrc, fps, with_audio }`:`with_audio=false` 时 PSM 只声明视频并丢弃音频事件。
  - `push_stream` / `push_stream_controlled` 保留为兼容封装。
- **事件接收**:`VideoSource::{has_async_events, recv_event, wait_event_blocking, take_buffered}`。`SharedVideoSource` 直接 await 总线;积压被丢弃时报告一次 `Discontinuity` 并从下一带参数集关键帧恢复。`LiveEventQueue` 用条件变量唤醒采集线程。
- **交织**:`interleave::InterleavedSource` 在 60ms 窗口内按 PTS 排序音视频,推流(协商音频时)与录像都经过它。
- **LightSource**:关键帧前置伪 SPS/PPS,否则共享总线订阅者等不到起播点。
- 旧接口 `next_frame` / `next_audio` 仍在,P3 替换采集层时删除。

## 容器格式支持(FileSource)

引擎推流只吃 **H.264/H.265 Annex B 裸流**(按起始码切帧)。为支持常见容器(MP4/FLV/MKV/MOV):
`FileSource::from_path` 检测文件非 Annex B 时,调用 FFmpeg 转封装为 Annex B 裸流。用户发布包使用应用内置 FFmpeg；开发环境允许通过 `UVP_FFMPEG_PATH`、PATH 或常见系统路径覆盖。
(`-c:v copy -bsf:v h264_mp4toannexb`,失败回退重编码),结果按"源路径+修改时间"缓存到临时目录复用。
FFmpeg 是用户发布包的内置运行时：容器格式、摄像头采集和预览不要求用户另行安装。构建机必须准备对应平台的 FFmpeg 二进制，发布脚本找不到时应直接失败。

桌面单设备模式的实时源由 `SharedMedia` 在注册成功后唯一持有；本地预览与平台点播分别订阅其有界帧总线。BYE 仅结束 RTP 订阅，注销设备才停止采集，避免第二次打开摄像头并保证预览与平台使用同一批编码帧。

## 职责

把码流(或伪码流)按 RTP 打包发送给上级平台。支持压测三档媒体强度(见 `10-functional/stress-testing.md#2`)。

## 模块与公开 API(规划)

```rust
pub enum RtpMode { Udp, TcpActive, TcpPassive }  // 已定义
pub mod rtp;      // RTP 打包/发送
pub mod ps;       // PS 封装
pub mod source;   // 视频源抽象
pub mod pusher;   // 视频源→PS→RTP 按帧率推流的驱动(push_stream / push_stream_controlled / PlaybackControl)
```

### rtp
- `RtpSender`:RTP 头构造、SSRC/序号/时间戳递增、UDP 发送、TCP(RFC 4571)发送;发送速率统计(供指标 send_bitrate)。

### ps
- `PsMuxer`:PS 包头/系统头/PES 封装,复用 H.264/H.265(+ 音频后续)。

### source(可插拔,对应压测三档)
```rust
pub trait VideoSource: Send {
    fn next_frame(&mut self) -> Option<Frame>;   // 返回一帧(裸码流/伪包)
    fn next_audio(&mut self) -> Vec<Vec<u8>> {}  // 该帧期间的音频分包(默认空;FileSource 覆盖为 G.711A)
}
```
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
