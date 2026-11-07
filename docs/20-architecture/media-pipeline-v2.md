# 媒体链路 v2 重构方案

**状态:已评审** · 适用平台:Windows / macOS · 关联:`30-crates/media-rtp.md`、`40-protocol/`

> 目标只有两个:**资源占用最低、用户体验最好**。本方案不考虑改造成本。
> 资源数字均为估算值,以 P1~P6 各阶段实测为准。

---

## 1. 需求范围

| 编号 | 需求 | 说明 |
|---|---|---|
| R1 | 输出 H.264 + H.265 | 两种编码都要支持,由媒体配置选择 |
| R2 | 有 GPU 就用 GPU | 硬件编码优先,没有可用硬件时软编兜底 |
| R3 | 首页默认不预览 | 提供"开启预览"按钮,用户主动开启 |
| R4 | 音视频混合流 | 视频 + 音频复用进同一路 PS 流(GB28181 复合流) |
| R5 | 三类源 | 摄像头、屏幕、静态文件 |
| R6 | 双平台 | Windows 10+ / macOS 13+ |

非目标:多画面合成(画中画)、Linux 实时采集。

---

## 2. 现状问题(v1)

| 问题 | 位置 | 影响 |
|---|---|---|
| 实时采集和编码依赖 FFmpeg 子进程,全部 libx264 软编 | `media-rtp/src/source_live.rs` | 1080p30 占 1~2 核 |
| 预览再起一个 FFmpeg 做 H.264→MJPEG | `media-rtp/src/preview_worker.rs` | 额外 0.5~1 核,MJPEG 带宽约为原始码流 10 倍 |
| 设备在线即持续采集编码,无人使用也不停 | `gb28181-simulator/src/device.rs:1457` | 待机占用高 |
| PTS 从 FFmpeg stats 管道单独读取,缺失时按 fps 合成 | `EncodedFrameHub::send_frame_at` | 链路复杂,长时间可能音画漂移 |
| 总线数据是 `Vec<u8>`,每个订阅者一次拷贝 | `source.rs` `MediaEvent` | 多路点播时内存带宽浪费 |
| 订阅端 `try_recv` + 1ms 轮询 | `pusher.rs:380` | 每路一个空转定时器 |
| `VideoSource` 新旧两套接口并存 | `source.rs:313` | pusher 有两条主循环,维护成本高 |
| 视频不支持 TCP 被动 | `push_stream_controlled(use_tcp: bool)` | 平台要求设备被动时推不了 |
| 探测命令独立打开采集设备 | `apps/desktop/src-tauri/src/lib.rs:1590` | 可能与运行中设备抢摄像头 |

---

## 3. 设计原则

1. **实时链路不经过 FFmpeg**:各平台原生 API 采集,画面从采集到编码留在 GPU 显存。
2. **编码器自己掌控**:随时可请求 IDR、参数集由封装层保证完整、时间戳来自采集 API。
3. **按需工作**:没有点播、预览、录像时不采集也不编码。
4. **预览就是真实码流**:编码后的 ES 直接送前端,用 WebView 的 WebCodecs 硬件解码。
5. **总线零拷贝**:编码数据用 `bytes::Bytes` 共享。
6. **故障隔离**:媒体引擎是独立子进程,崩溃不影响 SIP 注册。

---

## 4. 总体架构

```
主程序 (Tauri)
├─ SIP 信令 / 设备模拟:只给媒体引擎下指令
├─ 前端:预览按钮、编码器状态、码率/帧率展示
└─ IPC 客户端 ──────────────┐
                            │ 控制:Win 命名管道 / Mac Unix socket
                            ▼
媒体引擎子进程 uvp-media (每个应用一个)
├─ ① 采集层   输出 GPU 画面帧 + PCM 音频,统一主机时钟 PTS
│  ├─ 摄像头   Win: Media Foundation + WASAPI      Mac: AVFoundation
│  ├─ 屏幕     Win: Windows.Graphics.Capture + WASAPI Loopback
│  │           Mac: ScreenCaptureKit
│  └─ 文件     直通 / 导入时一次性转码缓存
├─ ② GPU 处理层  缩放、色彩转换、OSD 叠加(不回 CPU)
├─ ③ 编码层     视频 H.264/H.265 硬件优先;音频 G.711A/U、AAC
├─ ④ 分发总线 SharedMedia   编码后 ES,Bytes 零拷贝,async 订阅
└─ ⑤ 消费端(每个订阅者独立封装)
   ├─ 平台点播  PS 复用(音视频交织) → RTP (UDP / TCP 主动 / TCP 被动)
   ├─ 录像      PS 或 MP4 落盘
   └─ 本地预览  ES → IPC → Tauri Channel → WebCodecs → canvas
```

### 4.1 核心数据类型

```rust
/// 采集层输出,不跨进程。
pub enum RawFrame {
    Video { surface: GpuSurface, pts: HostTime },   // D3D11 纹理 / IOSurface
    Audio { pcm: Bytes, sample_rate: u32, channels: u16, pts: HostTime },
}

/// 总线上传递的编码单元。
pub enum MediaEvent {
    Video(EncodedVideo),
    Audio(EncodedAudio),
    Discontinuity { pts_90k: u64, reason: DiscontinuityReason },
}

pub struct EncodedVideo {
    pub data: Bytes,            // Annex B
    pub codec: VideoCodec,      // H264 / H265
    pub key_frame: bool,
    pub has_param_sets: bool,   // 关键帧必须为 true
    pub pts_90k: u64,
    pub dts_90k: u64,           // 无 B 帧时等于 pts
}

pub struct EncodedAudio {
    pub data: Bytes,
    pub codec: AudioCodec,      // G711A / G711U / Aac
    pub pts_90k: u64,
    pub duration_90k: u32,
}
```

`VideoSource` 旧接口(`next_frame` / `next_audio`)删除,所有源只产出 `MediaEvent`。

---

## 5. 编码层(R1、R2)

### 5.1 编码器选择顺序

| 平台 | H.264 | H.265 |
|---|---|---|
| Windows | NVENC SDK → Intel oneVPL → AMD AMF → MF 硬件 MFT → x264 | NVENC → oneVPL → AMF → MF 硬件 MFT → x265 |
| macOS | VideoToolbox 硬件 → x264 | VideoToolbox 硬件 → x265 |

- Windows 直接调用厂商 SDK,低延迟参数控制比 MF 细;MF 硬件 MFT 作通用兜底。
- 软编(x264/x265)作为最后兜底,保证无 GPU 的虚拟机、云桌面也能用。

### 5.2 能力探测

- 首次启动和显卡驱动变化时,对每个候选编码器**实际试编 10 帧**。只检查驱动是否存在不够,驱动在但编不出来的情况很常见。
- 结果按"GPU 型号 + 驱动版本"缓存到配置目录。
- 对外暴露:

```rust
pub struct EncoderCapability {
    pub codec: VideoCodec,
    pub backend: EncoderBackend,   // Nvenc / OneVpl / Amf / MfHw / VideoToolbox / X264 / X265
    pub hardware: bool,
    pub max_resolution: (u32, u32),
}
```

### 5.3 统一编码参数

| 参数 | 取值 | 原因 |
|---|---|---|
| B 帧 | 0 | 低延迟,DTS = PTS |
| GOP | 固定,默认 2 秒 | 新订阅者另行请求 IDR,GOP 可以拉长以省码率 |
| 码率控制 | CBR,或 VBR + maxrate | GB28181 平台关心码率稳定 |
| 低延迟模式 | 开 | NVENC `ull`,VT `RealTime`,QSV/AMF 各自的低延迟 preset |
| 参数集 | 每个 IDR 前由封装层补齐 SPS/PPS(H.265 加 VPS) | 不依赖各编码器的 repeat-headers 行为 |
| 强制 IDR | `request_keyframe()`,同一编码器最快 1 秒一次 | 新订阅者快速起播,同时防止被刷爆 |

### 5.4 软编提示

- 当前路径为软编时,前端在设备卡片上标注"CPU 编码"。
- 只能软编 H.265 时(x265 1080p30 约 2~4 核),提示推荐 H.264 或 720p。**只提示,不自动修改配置**。

### 5.5 P2a 落地:FFmpeg 硬件编码器(已实现)

P3 原生采集之前没有 GPU 显存帧,原生编码器无从接入。P2a 先在现有 FFmpeg 采集管线里换编码器,`crates/media-rtp/src/encoder.rs`:

- 候选:Windows `h264/hevc_nvenc → _qsv → _amf → _mf → libx264/libx265`;macOS `h264/hevc_videotoolbox → libx264/libx265`。
- 首次使用时用 `testsrc2` 试编 12 帧(GOP=5),要求有输出、≥2 个 IDR、每个 IDR 前带参数集,首个通过者胜出,进程内缓存。启动失败且 stderr 指向编码器时 `mark_failed`,下次换下一个候选。
- 统一参数:`-bf 0`、`-g` + `-force_key_frames`(**AMF H.264 不认 `-g`**,实测 12 帧只出 1 个 IDR)、CBR(`-maxrate`/`-bufsize` = 码率)、`-bsf:v dump_extra=freq=keyframe`。
- macOS 屏幕 H.264 有 VideoToolbox 时改走 FFmpeg 管道,不再用 openh264 + 逐像素 BGRA→RGB;只能软编时仍回退 openh264。
- 调试:`UVP_VIDEO_ENCODER=nvenc|qsv|amf|mf|videotoolbox|software` 强制指定。
- 前端:设置页"视频编码"旁显示 `GPU 编码（NVENC）` / `CPU 编码`,软编给出降级建议。

**实测**(本机 Ryzen 7 5800H + NVIDIA T600,FFmpeg 9.0.2,单位为单核百分比):

| 场景 | libx264 / libx265 | NVENC |
|---|---|---|
| 1080p25 合成高动态画面,纯编码 H.264 | 55% | 4% |
| 1080p25 合成高动态画面,纯编码 H.265 | 136% | 3% |
| 1080p25 屏幕采集整条管线(桌面基本静止) | 51~59% | 38~45% |

屏幕采集收益小,原因是剩下的开销主要在 `hwdownload` + CPU 缩放/色彩转换(约 40% 单核),不在编码;静止桌面 x264 本身也很便宜。这部分要靠 P3 的 GPU 缩放(D3D11 VideoProcessor)消除。QSV 在本机(无 Intel GPU)探测失败被正确跳过。**macOS 部分只做了参数单测,未在真机验证。**

---

## 6. 采集层(R5)

### 6.1 摄像头

| | Windows | macOS |
|---|---|---|
| 视频 | Media Foundation Source Reader,D3D11 显存输出 NV12 | AVCaptureSession,IOSurface 输出 NV12 |
| 音频 | WASAPI 共享模式,麦克风 | AVCaptureAudioDataOutput |
| 时间戳 | MF 采样时间(QPC 时基) | CMSampleBuffer PTS(host time) |
| 采集格式 | 优先原生 NV12/YUY2;只有 MJPEG 时用 MF 硬件 MJPEG 解码 | 优先 420v / 420f |

### 6.2 屏幕

| | Windows | macOS |
|---|---|---|
| 视频 | Windows.Graphics.Capture,D3D11 纹理,只在画面变化时出帧 | ScreenCaptureKit,IOSurface |
| 系统声音 | WASAPI Loopback | ScreenCaptureKit 音频流 |
| 麦克风 | WASAPI | macOS 15+ 走 ScreenCaptureKit,低版本走 AVFoundation |
| 光标 | 可选是否绘制 | 同左 |

- **静止画面补帧**:屏幕不变时采集 API 不出帧,编码层按目标帧率重复上一帧,保证平台收到的是恒定帧率。重复帧的编码代价很低。
- **系统声音 + 麦克风同时选择时**,在 PCM 层混音(相加后限幅),再编码。

### 6.3 文件

```
导入文件
├─ 建立索引:每帧偏移 / PTS / 关键帧,解析一次后缓存
├─ 编码与目标一致,且不叠 OSD → 直通:只做 PS 封装,CPU ≈ 0
└─ 不一致或需要 OSD → 导入时用硬件编码一次性转码,结果缓存
   缓存键 = 文件内容哈希 + 媒体配置哈希;之后每次播放走直通
```

- 容器解析:MP4/MOV 用原生 API(Win MF Source Reader,Mac AVAssetReader)。FLV/MKV 在**导入阶段**用 FFmpeg 转封装。
- **FFmpeg 只保留在离线导入这一步**,改用只含必要组件的 LGPL 精简版,减小安装包。
- 循环播放:每轮结束后时间戳在上一轮末尾基础上继续累加,不归零;平台端看到的是一条连续的流。
- 回放 seek / 倍速:沿用 v1 的 `PlaybackControl` 语义,基于索引定位到最近的关键帧。

### 6.4 GPU 处理层

| 操作 | Windows | macOS |
|---|---|---|
| 缩放 / 色彩转换 | D3D11 VideoProcessor | VTPixelTransferSession / Metal |
| OSD 叠加(时间、通道名) | Direct2D 画到纹理 | Core Image / Metal |

画面从采集、缩放、叠加到编码器输入**全程不回 CPU 内存**。只有软编路径需要一次显存到内存的拷贝。

---

## 7. 音视频混合流(R4)

### 7.1 各源的音频来源

| 源 | 可选音频 |
|---|---|
| 摄像头 | 无 / 麦克风 |
| 屏幕 | 无 / 系统声音 / 麦克风 / 系统声音 + 麦克风 |
| 文件 | 无 / 文件自带音轨 |

### 7.2 音频编码

| 编码 | PS stream_type | 实现 | 默认 |
|---|---|---|---|
| G.711A | 0x90 | Rust 自实现 | ✅ 兼容性最好 |
| G.711U | 0x91 | Rust 自实现 | |
| AAC-LC | 0x0F | Win MF AAC 编码器 / Mac AudioToolbox | |

采集 PCM 统一重采样到 8kHz 单声道(G.711)或 16/48kHz(AAC)。

### 7.3 音画同步

1. **单一时钟**:所有视频帧和音频块使用采集时刻的主机时间(Win QPC,Mac mach host time),换算成 90kHz。
2. **零点**:首个视频帧和首个音频块中**较早**的那个时间作为本次会话零点,两路共用。
3. **音频 PTS 连续性**:`pts = 首采样时间 + 已输出采样数 / 采样率`。与主机时钟偏差超过 20ms 时重新对齐,并记录一次 `Discontinuity`。
4. **PS 交织缓冲**:复用前保留 40~80ms 窗口,音频和视频按 DTS 排序后输出。SCR 由最早的 DTS 推算。

### 7.4 按订阅者决定是否带音频

总线上是未封装的 ES,**PS 封装在各订阅者内完成**。

- 平台 INVITE 的 SDP 未协商音频时,该订阅者只复用视频。
- 其他订阅者(录像、其他平台)不受影响。

---

## 8. 分发总线

| 项 | v1 | v2 |
|---|---|---|
| 数据 | `Vec<u8>`,每个订阅者拷贝一次 | `Bytes`,引用计数共享 |
| 订阅接收 | `try_recv` + 1ms 轮询 | `recv().await` |
| 起播 | 缓存最近的带参数集关键帧 | 缓存关键帧 + 新订阅时 `request_keyframe()` |
| 积压 | Lagged 后等待下一关键帧 | 同左,并对该订阅者发出 `Discontinuity` |
| 源接口 | 新旧两套 | 只有 `MediaEvent` |

慢消费者只影响自己:每个订阅者有独立的有界接收队列,满了就丢到下一个关键帧,不会阻塞采集和其他订阅者。

---

## 9. 消费端

### 9.1 平台点播

- 每个点播会话:`订阅总线 → PsMuxer(按 SDP 决定音频) → RtpSender`。
- RTP 传输支持 **UDP / TCP 主动 / TCP 被动**,由 SDP 的 `a=setup` 决定(补齐 v1 缺失的视频 TCP 被动)。
- RTCP SR 按 5 秒一次发送,NTP 时间取自 `common::clock`。

### 9.2 录像

- 订阅总线后落盘为 PS(兼容 GB28181 回放)或 MP4(便于本地查看)。
- 录像回放走第 6.3 节的文件直通路径。

### 9.3 本地预览(R3)

**交互**

| 场景 | 行为 |
|---|---|
| 首页初始状态 | 设备卡片显示占位图和"开启预览"按钮,不预览 |
| 点击开启 | 订阅总线 + 请求 IDR,目标 0.5 秒内出画面 |
| 预览中 | 角落显示:编码、硬件/CPU 编码、分辨率、实际帧率、码率 |
| 切页面 / 窗口最小化 | 自动暂停,不解码不传输;回来后自动恢复 |
| 点击关闭 | 退订;之后保持关闭直到用户再次开启 |
| 应用重启 | 一律恢复为关闭,不记忆 |

**技术路径**

```
总线 EncodedVideo (Bytes)
  → 媒体引擎 IPC → 主程序 → Tauri Raw Channel(二进制,不转 base64)
  → 前端 VideoDecoder (WebCodecs, hardwareAcceleration: "prefer-hardware")
  → VideoFrame 画到 canvas,立即 close() 释放
```

- 支持情况:WebView2(Chromium)支持 H.264 硬件解码;WKWebView 需 macOS 13.3+(Safari 16.4)。H.265 取决于系统和显卡。
- 打开前用 `VideoDecoder.isConfigSupported()` 检测。**不支持时**回退到媒体引擎内用原生解码器(Win MF / Mac VideoToolbox)解码,缩到 640 宽,编成 JPEG 送前端。
- 预览只传视频,不传音频。
- 预览帧协议沿用 v1 头部格式(`UVPJ` magic),版本号升到 2,载荷类型增加 `annexb-h264` / `annexb-h265`。

**P4 落地(已实现,媒体引擎子进程未做前在主程序内)**

- 后端 `PreviewManager`(`apps/desktop/src-tauri/src/preview_manager.rs`):页面 attach 才 `SharedMedia::subscribe()`,detach 即退订。**预览关闭时没有订阅者、没有 FFmpeg 预览进程**。
- 起播:订阅自带的缓存关键帧先发出(立即有画面);它与之后的直播帧之间有时间轴缺口,缺口/`Discontinuity` 之后丢 delta,等下一个带参数集的关键帧。
- 帧头 `UVPJ` v2 = 36 字节:`kind`(0 jpeg / 1 h264 / 2 h265)、关键帧标志、关键帧附带 codec 字符串(Rust 解析 SPS 生成 `avc1.PPCCLL` / `hvc1.…`,`media_rtp::codec_string`)。
- 前端 `manager.ts`:编码帧按序送 `VideoDecoder`(不走 latest-wins);首个关键帧检测通过后立即解码;解码队列 > 3 丢 delta 等关键帧;解码器出错重建;代际变化(设备重启)关闭旧解码器。
- 回退:前端 `isConfigSupported` 失败(先试 `prefer-hardware` 再试 `no-preference`)→ `set_preview_fallback(true)` → 同一订阅改喂现有 FFmpeg preview worker 出 JPEG。**实际回退实现沿用 FFmpeg worker,不是上面写的 MF/VideoToolbox 原生解码**,原生解码留给 P5 媒体引擎。
- 抓拍改为取共享媒体当前关键帧转 JPEG,不依赖预览开启(此前取预览总线最新 JPEG,预览默认关闭后会失效)。
- 首页与推流预览页默认关闭,点"开启预览"才订阅;选择只在本次运行内保持,重启恢复关闭。
- 已知遗留:采集源内部的旧预览旁路(`LiveSource` 的 `preview` 参数与 `EncodedFrameHub.preview`)已不再接线(恒为 `None`),代码随 P3 替换 `source_live.rs` 时删除。

**实测**(本机 WebView2 同内核 Edge 156,headless):NVENC 输出的 H.264 Annex B(`avc1.64001E`)不带 description 直接解码 25/25 帧;NVENC H.265(`hvc1.1.6.L90.90`)`isConfigSupported` 返回不支持(本机未装 HEVC 扩展),按设计走 JPEG 回退。

---

## 10. 按需采集

### 10.1 状态机

```
         有消费者加入(点播/预览/录像)
  Idle ──────────────────────────────▶ Starting ──首个 IDR──▶ Running
   ▲                                     │                      │
   │                                  启动失败                最后一个消费者离开
   │                                     ▼                      ▼
   │                                   Failed               Lingering(默认 30s)
   │                                                            │
   └──────────────────── 超时无新消费者 ◀───────────────────────┘
                         (期间有新消费者加入则回到 Running)
```

### 10.2 取舍

- 待机占用 ≈ 0,代价是首次点播多等摄像头启动,约 0.5~1.5 秒。GB28181 INVITE 超时通常有数秒,可以接受。
- 设置项"保持热采集":打开后设备在线即采集,给测起播速度的场景用。**默认关闭**。
- 设备独占:同一物理设备在引擎内只打开一次。探测命令改为查询引擎状态,设备已被占用时直接返回当前采集参数,不再自己打开(修复 v1 的抢设备问题)。

---

## 11. 进程模型

| 项 | 设计 |
|---|---|
| 进程 | 主程序 + 一个 `uvp-media` 子进程(不是每路流一个) |
| 控制通道 | Win 命名管道 / Mac Unix socket,消息用 JSON 或 bincode |
| RTP 发送 | 由 `uvp-media` 直接发送,不经过主程序 |
| 预览数据 | `uvp-media` → 共享内存环形缓冲 → 主程序 → Tauri Channel |
| 崩溃恢复 | 主程序检测到子进程退出后 1 秒内拉起,按当前设备状态重建采集和推流;SIP 注册保持不变 |
| 权限 | macOS 摄像头/麦克风/屏幕录制权限归属主程序 bundle,子进程作为 helper 打包在同一 bundle 内并继承 |

---

## 12. 资源占用目标(1080p30,估算值)

| 场景 | v1 现状 | v2 硬件编码 | v2 软编 H.264 |
|---|---|---|---|
| 设备在线、无人使用 | 1~2 核 | ≈ 0 | ≈ 0 |
| 点播推流、不预览 | 1~2 核 | < 5% CPU | 约 1 核 |
| 点播推流 + 预览 | 2~3 核 | < 8% CPU | 约 1 核多 |
| 文件直通推流 | 低 | ≈ 0 | ≈ 0 |

---

## 13. crate 调整

| crate | 变化 |
|---|---|
| `media-rtp` | 保留 `ps`、`rtp`、`pusher`(只剩 timed 主循环);删除 `source_live.rs`、`preview_worker.rs`、`LegacyMediaClock`、旧 `VideoSource` 接口 |
| 新增 `media-capture` | 采集层 + GPU 处理层,按平台分 `windows/`、`macos/` 模块 |
| 新增 `media-codec` | 编码器 trait、能力探测、各后端实现、G.711/AAC |
| 新增 `media-engine` | 子进程入口:总线、按需状态机、IPC 服务端 |
| `gb28181-simulator` | 不再直接构造源,改为通过 IPC 客户端向引擎请求订阅 |
| `apps/desktop` | 预览改为 WebCodecs 播放器 + 预览按钮;新增编码器状态展示 |

依赖方向:`common → media-codec → media-capture → media-rtp → media-engine`;`gb28181-simulator` 只依赖 IPC 协议类型,不依赖 `media-engine` 实现。

---

## 14. 分阶段实施

| 阶段 | 内容 | 验收标准 |
|---|---|---|
| P1 总线与协议 ✅(代码) | `Bytes` + async 订阅;PS 音视频交织;视频 TCP 被动;按订阅者封装。**删除旧接口推迟到 P3**(旧采集路径随 `source_live.rs` 一起替换) | 现有源跑通,真实平台(如 WVP)点播音视频同步 —— 单测已过,WVP 联调待做 |
| P2a 硬件编码器(FFmpeg) ✅ | 现有 FFmpeg 采集换硬件编码器 + 试编探测 + 运行期失败回退 + 前端 GPU/CPU 标识(§5.5) | Windows 实测选中 NVENC,1080p 屏幕采集 H.264/H.265 验收通过;macOS 待真机验收 |
| P2b 原生编码器 | 编码器 trait;NVENC / oneVPL / AMF SDK、MF MFT、VideoToolbox 直接吃 GPU 显存帧。**与 P3 合并实施**(没有原生采集就没有显存帧) | 各编码器出流都能在平台起播,每个 IDR 都带参数集 |
| P3 采集层 | 双平台三类源原生采集;统一时钟;GPU 缩放和 OSD;删除 `VideoSource` 旧接口(`next_frame`/`next_audio`/`LegacyMediaClock`)与 pusher 按帧率打拍的旧主循环 | 连续运行 24 小时,音画偏差 < 40ms |
| P4 预览 ✅(代码) | 预览按钮 + WebCodecs 播放 + 不支持时的回退(§9.3 P4 落地) | 点击后 0.5 秒内出画面;关闭后预览相关 CPU 归零 —— 单测与 Edge 解码验证已过,应用内真机观感待验收 |
| P5 按需采集 + 子进程 | 状态机、空闲保持;媒体引擎独立进程、崩溃拉起 | 杀掉子进程后自动恢复推流,SIP 注册不掉 |
| P6 文件源 | 索引、直通、一次性转码缓存;FFmpeg 精简为导入专用 | 直通 CPU ≈ 0;循环播放时间戳连续 |

P1 不依赖新采集层,可以在 v1 源上先落地并独立上线。

---

## 15. 测试策略

| 类别 | 方法 |
|---|---|
| 音画同步 | "闪光 + 嘀声"测试源,接收端测量偏差 |
| 编码器兼容 | 硬件矩阵:NVIDIA、Intel 核显、AMD、无 GPU 虚拟机、Intel Mac、Apple Silicon |
| 平台兼容 | H.264/H.265 × G.711A/G.711U/AAC,对接至少两个真实 GB28181 平台 |
| 长稳 | 24 小时推流,监控内存、句柄、时间戳连续性 |
| 资源 | 第 12 节各场景实测 CPU/GPU/内存,回填本文 |
| 故障 | 拔摄像头、驱动重置、杀子进程、网络中断 |

---

## 16. 风险

| 风险 | 应对 |
|---|---|
| 各厂商 SDK 和驱动行为差异 | 能力探测 + 自动降级到下一个后端;问题机型记入黑名单 |
| 部分平台不支持 H.265 或 AAC | 按 SDP 协商;默认 H.264 + G.711A |
| WebView 不支持 H.265 解码 | 第 9.3 节的原生解码回退 |
| macOS 权限弹窗归属 | helper 打包在主 bundle 内;首次使用时引导授权 |
| 按需采集导致首帧变慢 | 提供"保持热采集"开关;启动后立即请求 IDR |
| 软编 H.265 占用过高 | 界面提示并推荐改用 H.264 或 720p |

---

## 17. 已确认决策

| 编号 | 问题 | 决策 |
|---|---|---|
| Q1 | 按需采集是否默认开启 | 默认开启,提供"保持热采集"开关(默认关) |
| Q2 | 媒体引擎是否独立成子进程 | 采用 |
| Q3 | macOS 最低版本 | 13(ScreenCaptureKit 音频、WebCodecs 需 13.3+) |
| Q4 | 录像默认格式 | PS(兼容 GB28181 回放) |
