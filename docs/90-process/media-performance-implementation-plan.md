---
title: 视频采集性能改进实施方案与任务
status: phase-1-complete
updated: 2026-09-14
---

# 实施范围

依据 `docs/00-product/media-performance-quality-spec.md` 的 MPQ-03/04/08、AC-08，
以及老板本轮“开始实施开发”的授权，先完成不可见预览的按需工作。
Windows 为构建与测试环境，Mac 为主要编辑位置；不把开发机当作最低配置样机。
需求草案其余画质阈值、最低设备、Windows 原生采集路线仍须逐项落实。

第一批用户行为：切出预览页或最小化应用后停止预览专用处理；回到预览时恢复。
主采集、主码流、音频、推流和录像保持原有所有权与参数。
可见画面仍取自实际编码码流，保持原有预览分辨率与帧率策略。

# 技术方案

现状：前端 manager.detach 只移除 Canvas，后端 Channel 和预览解码子进程持续存在。
Rust PreviewManager 的内部 watch 接收者也一直存在，不能把 watch.receiver_count
当作真实用户需求。编码后的 AU 仍被复制、解码成 JPEG，再转发至无人查看的 Channel。

1. 前端 manager 以“有效 Canvas、文档可见且原生窗口未最小化”为需求，串行管理连接与释放。
   Windows WebView2 实测最小化后仍可能报告文档 visible，因此结合原生窗口事件与初始状态查询。
   用连接代次拒绝迟到回调；处理 attach 尚未完成就 detach、旧页面迟到 detach、
   隐藏时正在解码、快速切页及关闭与重连竞态。
2. Rust PreviewManager 在有效 token 的 attach/detach/Channel 失效处更新总线需求。
   需求与目标替换在同一同步边界内更新，旧 token 不得关闭新目标需求。
3. PreviewSink 新增默认启用的需求查询，保持其他 sink 行为。
   DesktopPreviewBus 显式持有需求；状态订阅和内部转发器不算用户需求。
4. PreviewWorker 无需求时释放自己的 FFmpeg 子进程、清空队列、进入 paused。
   不计为故障，不消耗恢复预算；恢复时等待新的参数集关键帧，并推进预览代次。
   捕获代次保持不变。入口保持非阻塞，主流 AU 先进入主路。
5. 活跃源与文件源在复制预览 AU 之前检查需求，避免隐藏期间复制和 NAL 检查。
   不添加第二条采集或编码路径；不改变传输协议与编码参数。
6. Windows 的匿名管道写入是同步调用，原有基于 WouldBlock 的截止时间不能中断它。
   将 Windows 预览写入与 supervisor 隔离，通过有界请求和已有子进程所有权回收。
   隐藏、超时或故障均能终止自己拥有的 decoder，主采集入口仍不阻塞。

恢复等待时长受源关键帧间隔影响；不得用旧 JPEG 冒充恢复后的实时帧。
未收到新参数集关键帧时显示等待状态，不重启主采集以强制恢复预览。

# 任务与测试清单

| 任务 | 输入/操作 | 期望输出 | 结果 |
|---|---|---|---|
| P1-01 前端释放 | attach → detach；模拟迟到帧 | 释放原 token；不解码、不回画旧帧 | Windows 前端回归通过：原 token 释放，迟到帧不再解码/绘制 |
| P1-02 前端竞态 | 连接未完成即停止；快速重新 attach | 迟到连接释放；新连接保留；旧回调失效 | Windows 通过：pending start 立即解绑、旧回调隔离、串行替换连接 |
| P1-03 可见性 | visible → hidden → visible，含正在解码 | 隐藏时不绘制或传输；恢复按新连接接收 | Windows 用例及真窗口通过：原生最小化 false / 恢复 true；文档仍 visible |
| P1-04 后端需求 | attach A/B → detach A → detach B | 旧 token 不影响 B；最后目标离开后需求为 false | Windows Rust 通过：有效 token 决定需求，旧 token 不误停新目标 |
| P1-05 通道故障 | Raw Channel send 失败/应用关闭 | 只解除对应目标并释放预览需求 | Windows Rust 通过：关闭通道与带缓存的关闭替换目标均释放需求 |
| P1-06 worker 暂停 | 无需求持续投递 AU；活跃时取消需求 | 不启动或释放 decoder；队列有界且不阻塞主路 | Windows 实际 FFmpeg 用例通过：无需求不入队，暂停回收 decoder |
| P1-07 worker 恢复 | 暂停后先 delta 再 config keyframe | delta 不启动 decoder；新关键帧恢复，无旧输出 | Windows 实际 FFmpeg 用例通过：delta 不启动，新关键帧和后续 burst 输出新 JPEG |
| P1-08 主路隔离 | 主流持续投递，预览反复暂停恢复 | 主路帧/时间戳不变；捕获代次不变 | Windows 源事件用例通过：视频/音频字节及 PTS、duration、capture generation 保持 |
| P1-09 质量回归 | 原 1080p H.264 合成素材预览测试 | 输出 JPEG 仍为 1920×1080，原有效帧断言保持 | Windows 真实 FFmpeg 合成素材用例通过：JPEG 1920×1080，质量参数未改 |
| P1-10 Windows 集成 | 前端全测试、Rust workspace、桌面构建 | 明确通过数/跳过数；最终构建可启动 | Windows 前端 32 项通过；Rust 287 通过、10 原有跳过；桌面构建成功 |
| P1-11 Windows 停读 | 写入超过管道容量，子进程不读 stdin，然后隐藏 | 2 秒内暂停并回收子进程；不拖住主流 | Windows 停读用例通过：隐藏 2 秒内回收；单个大 AU 持续停读触发写入超时 |

新增前端释放、后端需求、worker 暂停与 Windows 停读测试均曾复现失败，再完成修复与回归。
原生最小化以真窗口失败为依据补充用例与实现。构建和最终验收在 Windows 执行；
同步前核对 Windows 工作区，按明确文件列表传输并记录 SHA-256。

# 后续批次

- P2：Windows 原生摄像头/屏幕/音频枚举与统一时间轴采集，连接同一主码流及预览旁路。
  先确定帧/音频样本来源与时间戳，不把 Unix fd 或序号时钟直接搬到 Windows。
- P3：硬件编码实际能力探测与同规格选择，覆盖 NVIDIA/Intel/AMD 和软件回退。
  必须同时证明画质、延迟、吞吐及码率约束，不能把枚举到 GPU 等同于可用。
- P4：固定素材与最低配置矩阵、可见预览/推流/录像组合及 60 分钟稳定性验收。
  无合格路径时明确报告能力不足，不静默降档。

本方案第一批验收不代表 Windows 实时采集、硬件加速或最低配置画质承诺已完成。

# 本批验收记录

代码基线仍为 `47679ba8a7403b49fffb7ce2176866c7fb16e39b` 加未提交改动。
Windows 日志目录为 `G:\devtools\uvp-sim\logs`：

- `preview-phase1-rust-final.log`：`cargo test --workspace --locked`，287 通过、0 失败、10 原有跳过。
- `preview-phase1-frontend-final.log`：32 项通过，vue-tsc 与 Vite 构建通过。
- `preview-native-visibility-final.log`：最后补充的重复原生事件/恢复用例通过。
- `preview-windows-pipe-green.log`：23 项预览测试通过，含两个 Windows 停读回收用例。
- `preview-phase1-build-final.log`：Tauri Windows debug/no-bundle 构建成功。
- `preview-native-visibility-runtime-final.log`、`preview-visibility-cdp.jsonl`：原生窗口与预览需求转换证据。

最终程序 SHA-256：`56c3ea8c97df4b889976d288f24b2d8fcc70219b468ea8a89da57287d891c7ca`。
2026-09-14 11:27（UTC+8）Session 2、PID 16376 的预览页实测：
最小化时原生可见性 false、文档仍 visible、预览需求 false；恢复后原生 true、需求 true；
离开预览路由后需求再次为 false。页面无实时媒体源，因此此项只证明窗口、前端与后端需求联动。
实际 JPEG 恢复及 decoder 回收由独立 FFmpeg 行为用例验证。

验收脚本最初使用 Process.MainWindowHandle，最小化后可能选到辅助窗口，导致恢复/关闭检查失真。
已改为按目标进程 PID 枚举并精确匹配主窗口标题、核对句柄所属进程后操作，应用代码未为此增加补丁。

边界：未做摄像头/屏幕真实采集、平台推流/录像组合、低配 CPU/GPU 对比或 60 分钟稳定性测试；
1080p 分辨率回归不是所有画质指标等价的证明。未修改编码参数，未提交或推送代码。
