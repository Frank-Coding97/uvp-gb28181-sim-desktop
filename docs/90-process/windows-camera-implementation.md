---
title: Windows 摄像头接入规格与实施任务
status: implemented-partially-verified
updated: 2026-09-14
---

# 本批规格

老板已确认补齐 Windows 设备枚举、支持规格探测和实际采集。本批目标是 Windows 52 的
Logitech Webcam C925e 能在软件中选择、测试采集，并用真实视频帧驱动现有预览。
摄像头音频按用户原有选择处理；未选择音频时不打开麦克风。

验收标准：设备列表来自系统真实枚举；目标分辨率与帧率有设备能力依据；不支持的规格明确报错；
主路使用实际编码帧与真实时间戳；停止后释放摄像头；预览隐藏/恢复不重启主采集。
不静默降低配置的分辨率、帧率或码率。不宣称所有画质指标或最低配置矩阵已验收。

非目标：本批不实现 Windows 屏幕采集、GPU 自动选型、安装包发布或平台生产联调。

# 技术方案

- DirectShow 通过项目独立 FFmpeg 枚举设备与支持模式；保留唯一设备名，避免同名设备混淆。
- 模式选择优先满足配置的精确尺寸与帧率；C925e 的 1080p/30 使用 MJPEG 输入，
  不把 YUY2 的 1080p/5 当成 30fps，再用重复帧掩盖采集不足。
- 摄像头音视频由同一 FFmpeg 输入会话采集，复用已有编码 AU、音频解析器、主路事件队列和预览旁路。
- Windows 编码 PTS 用仅监听 127.0.0.1 随机端口的统计旁路传递，使用已有统一时间零点。
  不沿用 Unix fd 3/4，不退回序号合成时间戳。Windows 视频使用 DirectShow graph capture clock，
  避免远程摄像头设备时钟与麦克风时钟偏移；编码后 PTS 仍来自 FFmpeg。旁路读等待可取消。
  音频输入缓冲显式设为 100ms；按输入时间戳补齐间歇对应的静音。G.711 在编码前按 160 个采样
  分包，使统计时间戳和现有原始音频包一一对应。音频输出也逐包 flush。
- H.264 使用已有软件编码配置；H.265 使用 Windows 可用的软件编码器并保持所选规格，
  不将 GPU 枚举结果当作编码可用证明。硬件路径另批评测。
- 保持既有前端设备目录/URI 接口，必要错误提示改为跨平台摄像头表述。

# 任务及验证

| 任务 | 输入/操作 | 期望输出 | 结果 |
|---|---|---|---|
| W1 设备枚举 | 多摄像头、中文麦克风、alternative name、none 设备 | 分类准确，无重复，唯一名称可用于打开 | 通过：Windows 12 项相关单测；桌面目录已显示 C925e 和 RDP 摄像头 |
| W2 规格探测 | C925e YUY2/MJPEG 720p/1080p 模式；不支持的尺寸/帧率 | 精确选中可用模式；不支持时拒绝降档 | 通过：精确模式选择及拒绝降档测试；C925e 720p/1080p 能力可读，实际打开待排障 |
| W3 真实时间戳 | TCP 统计多行、未连接取消、停读取消、真实 EOF | 保留 PTS 行；取消有界；不伪造时间戳 | 通过：TCP 取消/读取/EOF；RDP 实机视频与音频各自单调 |
| W4 采集参数 | H.264/H.265、无音频/有音频的配置 | 正确 DirectShow 输入、编码参数与统计输出；不产生第二采集 | 通过：H.264/H.265 命令及 Windows 编译；G.711 分包与 graph clock 修正 |
| W5 实际媒体 | C925e 采集、读取视频/音频事件 | 新鲜编码帧，目标尺寸/帧率，时间戳单调，停止释放 | 部分通过：RDP 摄像头真实采集通过；C925e DirectShow 打开失败 |
| W6 预览生命周期 | 采集中隐藏、恢复、停止 | 隐藏释放 decoder；恢复新画面；主采集代次保持 | 通过：RDP 实机隐藏/恢复/reopen；桌面最小化只回收预览 FFmpeg，主采集 PID 保持 |
| W7 桌面验收 | Windows 重新构建、目录和采集测试、真实预览 | 摄像头可选择，测试成功，真实 Canvas 帧；日志与运行版本一致 | 部分通过：列表、采集按钮和 RDP 1280x720 Canvas 通过；C925e 实际视频仍未通过 |

构建和最终测试在 Windows 执行。两侧工作区保留此前 P1 未提交改动，按文件哈希核对同步；
本批不提交、不推送，不修改系统 FFmpeg、默认 Node 或全局网络配置。

# 2026-09-14 实机证据与边界

- Windows 52 / FFmpeg 8.1.2 / 同一开发分支与未提交工作区；不发布安装包。
- 软件目录已显示 `[0] Logitech Webcam C925e` 和 `[1] MacBook-Air-3 - MacBook Air相机`。
  麦克风目录与会话有关：交互 RDP 会话返回远程音频，SSH 后台会话可枚举 C925e 麦克风。
- C925e 的 720p/25 与 1080p/30 模式探测已通过；实际打开仍返回
  `Could not RenderStream to connect pins`。独立 FFmpeg 默认规格、640x480 原始输入、720p MJPEG
  以及桌面交互会话均复现；PnP 当前设备为 OK，系统摄像头访问为 Allow。
  这些证据只定位到设备打开链路，尚不能区分占用、驱动、USB 或会话限制。
- RDP 摄像头完成真实 H.264/H.265、G.711/AAC 采集、真实 JPEG 尺寸、预览暂停恢复和释放后重开测试。
  G.711 曾因包粒度不一致和输入间歇失败，现 20ms 包间隔连续；H.265 音频曾落后约 0.9s，
  设置 100ms 输入缓冲后，G.711 尾部差 160ms、AAC 368ms，均通过本批小于 500ms 的传输检查。
- 桌面交互验收：RDP 摄像头的“测试采集”成功；Canvas 为 1280x720，页面显示 25 FPS。
  最小化时预览子进程 2920 退出，主采集 21008 保持，恢复后预览子进程变为 16604。
  这里只对本机临时 SIP 测试端注册，未进行真实上级平台点播。

设备测试通过不等于物理声画同步、PS/RTP 播放或所有最低配置机器验收：仍需 C925e 恢复可打开后
验证 1080p/30、真实声画对齐、上级平台接收和不同硬件矩阵。测试中的 500ms 阈值仅检查队列交付
是否明显滞后，不能替代声画对齐测试。视频与音频事件各自单调，不宣称跨流发送顺序已抓包验证。
异常统计通道的启动错误目前仍可能等到原有 15s 启动期限；完整“测试采集”包含枚举、模式查询和启动，
不是总耗时固定 5s。

参数依据：[FFmpeg DirectShow 文档](https://ffmpeg.org/ffmpeg-devices.html#dshow)。其中 graph timestamp
选项只影响视频，不能写成所有 DirectShow 音频设备都使用 graph clock。

证据位于 Windows `G:\devtools\uvp-sim\logs`，主要文件：
`windows-camera-workspace.log`、`windows-camera-build.log`、`windows-camera-ui.jsonl`、
`windows-camera-c925e-720-final.log`、`windows-camera-c925e-1080-final.log`、
`windows-camera-hevc-g711-buffer100.log`、`windows-camera-hevc-aac-buffer100.log`。

最终 Windows 回归：前端 32/32、Rust 299 通过/0 失败/11 跳过；新增硬件测试默认忽略，
已在交互会话单独执行上述摄像头矩阵。最终桌面调试构建成功，SHA-256：
`6b38455bdcb416d505fe74276e2b86f7297c3b926b00cb5bd4dffa23ecee13bb`。
最终日志为 `windows-camera-final-workspace.log`、`windows-camera-final-build.log`；
H.264 最终矩阵为 `windows-camera-final-h264-{none,g711,aac}.log`。
