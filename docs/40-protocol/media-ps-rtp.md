# 协议规格:PS 封装与 RTP 推流

**状态:草案** · 实时点播的媒体承载。实现见 `30-crates/media-rtp.md`。参考 GB/T 28181-2022 + ISO/IEC 13818-1(PS)+ RFC 3550(RTP)。

## 总链路

```
视频源(H.264/H.265 帧) → PS 封装 → 切片 → RTP 打包 → UDP/TCP 发送 → 平台
```

## SDP 协商

INVITE 携平台侧 SDP(平台接收 IP/端口、期望的 payload type、SSRC、传输方式)。设备 200 OK 回本端 SDP。GB28181 常用:
- `m=video <port> RTP/AVP 96`,`a=rtpmap:96 PS/90000`(PS 流,时钟 90kHz)。
- `y=` 行携带 SSRC(GB28181 扩展)。
- `a=sendonly`(设备推、平台收)。
- 传输:UDP 或 TCP(`TCP/RTP/AVP`,被动/主动)。

## PS 封装

- PS 包 = Pack Header + System Header(关键帧处)+ 若干 PES。
- 视频 PES 复用 H.264/H.265 NAL;音频(G.711A 等)后续复用。
- 时间戳(PTS/DTS)基于 90kHz 时钟递增。

## RTP 打包

- RTP 头:V=2、PT=96(PS)、序号递增、时间戳(90kHz)、SSRC(与 SDP `y=` 一致)。
- PS 包按 MTU(约 1400B)切片进多个 RTP 包,最后一包置 marker。
- **UDP**:每 RTP 包一个数据报。
- **TCP(RFC 4571)**:每包前置 2 字节大端长度。

## 关键帧
点播开始与收到强制关键帧(IFrameCmd)时,PS 输出关键帧序列头,便于平台快速出图(FR-8)。

## 压测三档的媒体差异
- A 空媒体:不建媒体流(不推 RTP)。
- B 轻量:发固定/伪 PS 包,极低码率,验证平台 RTP 通道。
- C 真实:循环真实文件,码率/帧率受控。
详见 `10-functional/stress-testing.md#2`。
