---
title: Windows 屏幕采集规格与实施记录
status: implemented-partially-verified
updated: 2026-11-05
---

# 本批规格

Windows 桌面端模拟器补齐「屏幕」媒体来源：设备目录来自系统真实显示器枚举，选中后
用桌面复制（Desktop Duplication）采集，H.264/H.265 软件编码，主路与本地预览消费同一份
编码 AU，时间戳来自编码器实际输出而不是按帧率合成。

验收标准：显示器列表来自 Win32 枚举；不存在的显示器明确报错；采集输出分辨率等于媒体
设置档位；视频 PTS 单调且来自 `-stats_enc_post`；预览暂停/恢复不重启采集；停止后释放
桌面复制并可立即重开；请求音频时明确拒绝而不是静默丢音。

非目标：本批不做系统声音、麦克风、窗口采集、GPU 编码器自动选型、4K60、跨适配器
多显示器精确映射，也不宣称完成低配机或 60 分钟稳定性验收。

# 技术方案

- 采集后端用 FFmpeg `ddagrab`（Desktop Duplication API），不新写抓屏线程或编码器。
  `ddagrab` 按 `output_idx` 选择输出，通常等于 Windows 显示器编号减一；本实现用
  `EnumDisplayMonitors` + `GetMonitorInfoW` 枚举 `\\.\DISPLAYn`，把 n 作为持久化的
  `display_id`，采集时还原为 `output_idx = n - 1`。
- `ddagrab` 的 `video_size` 是裁剪而不是缩放（见 `vsrc_ddagrab.c` 的
  `CopySubresourceRegion` box），因此输出档位统一用
  `scale=W:H:force_original_aspect_ratio=decrease,pad=W:H:...` 完成。该滤镜始终保留，
  避免进程 DPI 虚拟化时误判「输入已等于输出」而跳过缩放。
- 编码参数与 DirectShow 摄像头共用 `windows_capture::encoder_args`，H.264/H.265、
  GOP、码率、`-fps_mode cfr` 只有一份定义。
- 时间戳复用 Windows 摄像头已打通的旁路：`-stats_enc_post:v tcp://127.0.0.1:<port>` +
  `-stats_enc_post_fmt:v "{pts} {tb}"`，`StatsReader` 在采集线程内读取，`CameraTimingClock`
  归一化到 90k，`TimedFrameSender` 把 stats 行与编码 AU 严格一一配对；缺 stats 时报错，
  不伪造时间戳。
- 预览复用 `spawn_preview_worker_with_codec`，只消费主路编码 AU 副本；预览不可见时停止
  送帧，不产生第二路采集。
- 不静默回退到 `gdigrab`：桌面复制不可用（D3D11、会话锁屏、被独占）时给出真实 FFmpeg
  stderr 和排查提示，避免同一配置在不同机器上悄悄改变采集路径。

# 任务及验证

| 任务 | 输入/操作 | 期望输出 | 结果 |
|---|---|---|---|
| S1 显示器枚举 | Win32 `EnumDisplayMonitors`，单显示器实机 | 返回 `\\.\DISPLAY1`、1920x1080、主显示器标记 | 通过：`显示器 1（主显示器）` 1920x1080 |
| S2 参数构建 | 1280x720@25 H.264 / 1920x1080@30 H.264 / 720p H.265 | ddagrab output_idx、scale/pad、共享编码参数 | 通过：3 组命令单测与实采 |
| S3 真实时间戳 | 采集 3 秒，读取 timed 事件 | PTS 单调，步长 = 1/fps 对应的 90k，duration > 0 | 通过：720p25 步长 3600；1080p30 步长 3000 |
| S4 实际媒体 | 桌面复制采集 H.264/H.265 | 非空 Annex-B、输出尺寸等于 profile、预览 JPEG 尺寸一致 | 通过：720p/1080p H.264、720p H.265 |
| S5 预览生命周期 | 采集时隐藏/恢复预览 | 隐藏不送帧但视频事件继续；恢复后代次不变、序号前进 | 通过 |
| S6 释放与重开 | drop 后立即重开 | teardown < 4s，重开仍有帧 | 通过 |
| S7 音频边界 | `audio=system` | 启动前明确拒绝 | 通过：单测覆盖 |

# 2026-11-05 实机证据

- 主机：`DESKTOP-BK2HVSM`，Windows 10.0.19045；FFmpeg
  `2026-10-22-git-bf0e831606-full_build`（gyan），显示器 `\\.\DISPLAY1` 1920x1080 主屏。
- 单测：`cargo test -p media-rtp --lib -- --test-threads=1`，111 通过 / 0 失败 / 4 跳过。
  新增 `windows_screen` 参数与显示器名解析测试；实机枚举与实时目录测试标记 `#[ignore]`。
- 实采命令（环境变量指向本机 FFmpeg/FFprobe）：

  ```powershell
  $env:UVP_WINDOWS_SCREEN_TEST_URI = "live:screen:1?audio=none"
  cargo test -p media-rtp --test windows_screen_live -- --ignored --nocapture
  ```

  结果：

  | 档位 | 有效视频帧（约 6.3s） | 首/末 PTS | 最小 duration | 预览 JPEG |
  |---|---|---|---|---|
  | 1280x720@25 H.264 | 159 | 0 / 568800 | 3600 | 1280x720 |
  | 1920x1080@30 H.264 | 190 | 0 / 567000 | 3000 | 1920x1080 |
  | 1280x720@25 H.265 | 162 | 0 / 579600 | 3600 | 1280x720 |

  三次均为 `audio=0`，预览暂停期间视频事件继续，恢复后 `generation` 保持 1、`sequence`
  前进，drop 到重开通过。

# 边界与未验证项

- 只验证了单适配器、单显示器、当前交互会话；多显示器与「显示器挂在不同 GPU 适配器」
  时，`DISPLAYn-1` 不一定等于 `output_idx`，失败会报 `Failed to enumerate DXGI output N`，
  后续可用 DXGI 枚举做精确映射。
- 未验证锁屏/切换用户/远程会话断开、显示器热插拔、4K 显示器 CPU 占用、60 分钟稳定性。
- 未通过桌面 UI 做平台注册、点播和预览验收；本批证据是 crate 级真实采集。
- 本机 FFmpeg 不在 PATH，测试需设置 `UVP_FFMPEG_PATH`，否则既有预览单测会因找不到
  FFmpeg 失败。全量测试并行执行时 `preview_worker` 用例曾因 FFmpeg 子进程竞争偶发
  `Dropped`，串行执行稳定通过；该现象在本批改动前已存在。
