---
title: Windows 52 远程开发工作区
status: development-ready
updated: 2026-09-14
---

# Windows 52 远程开发

## 工作方式

Mac 负责编辑和代码审查，Windows 负责原生编译、测试、打包和桌面运行。
也可以通过 SSH MCP 直接修改 Windows 工作区，但同一阶段只保留一个主要编辑位置。
远程操作统一使用 SSH MCP；文件传输使用其 SFTP 工具。

- SSH MCP 别名：`Windows 测试环境52`。
- 主机：`192.168.10.52:22`；已验证账户：`mengfanlu`。
- Windows 项目：`G:\code\uvp-gb28181-sim-desktop`。
- Windows 分支：`codex/windows-development`。
- 初始来源：Mac 的 `v3`，提交 `47679ba8a7403b49fffb7ce2176866c7fb16e39b`。
- 初次通过 Git bundle 传输完整提交历史，未执行外部 Git push。
- 性能需求草案单独同步，保持未提交状态；未同步 Mac 的依赖目录、编译输出和临时工具目录。

## 已核实环境

- Windows 10 x64，Build 19045。
- Intel i5-12400F，6 核 12 线程；约 64 GB 内存；NVIDIA RTX 3050。
- Git 2.52.0.windows.1。
- 项目独立 Node 24.21.0 / npm 11.19.0：`G:\devtools\uvp-sim\node-v24.21.0-win-x64`。
- 系统 Node 16 和既有 Node 20 未修改。Node 20 可构建，但无法直接运行项目的 TypeScript 测试；Node 24 已通过。
- Visual Studio 2022 Community，MSVC 14.38.33130，Windows SDK 10.0.22621.0。
- WebView2 152.0.4191.66。
- Rustup 1.29.1，rustc 1.98.1，stable-x86_64-pc-windows-msvc。
- Rust 使用 `G:\devtools\uvp-rust`，构建输出使用 `G:\devtools\uvp-sim\target`。
- 项目独立 FFmpeg / ffprobe 8.1.2：`G:\devtools\uvp-sim\ffmpeg-8.1.2-essentials_build\bin`。
- 原 `D:\devtools` 中的 2023 年 FFmpeg 未修改；项目脚本显式设置 `UVP_FFMPEG_PATH`、`UVP_FFPROBE_PATH` 与 `FFPROBE_BIN`，避免混用。

## 项目命令入口

这些脚本位于 Windows `G:\devtools\uvp-sim`，仅配置当前命令窗口，不改系统 PATH。

一键入口：

- `build.cmd`：构建内嵌前端的 Windows 调试程序。
- `verify.cmd`：前端构建、全部 `src/**/*.test.mjs` 和 `cargo test --workspace --locked`。
- `dev.cmd`：开发模式，包含 Vite 热更新。
- `run-built.cmd`：启动已构建的调试程序。

在 cmd 中准备开发终端：

```bat
call G:\devtools\uvp-sim\env.cmd
```

重新安装 Rust 和项目依赖，并验证前端构建及 Rust workspace 检查（需要网络）：

```bat
G:\devtools\uvp-sim\bootstrap.cmd
```

在 Windows 已登录桌面会话中启动带热更新的开发模式：

```bat
G:\devtools\uvp-sim\dev.cmd
```

运行已经构建、内嵌前端页面的调试程序：

```bat
G:\devtools\uvp-sim\run-built.cmd
```

该入口配置项目工具路径后运行 `G:\devtools\uvp-sim\target\debug\uvp-desktop.exe`。
调试程序不是完整发布安装包，不包含 FFmpeg 安装资源。

## 代码同步规则

- 默认以 Mac 工作区为主要编辑位置，Windows 用于构建验证；如改为远程主要编辑，应先同步两侧状态。
- 同步前检查双方 HEAD、分支和未提交修改，拒绝覆盖未知改动。
- 已提交代码可通过每个版本独立的 Git bundle + SFTP 同步，或网络及仓库认证就绪后用 Git fetch。
- `mac-bundle` remote 指向首次传输的静态 bundle，不代表之后的 Mac HEAD 自动更新。
- Windows 的 `origin` / `github` 仅保存不带凭证的仓库地址。`origin` 已通过 `git ls-remote` 读取 HEAD；`github` 尚未验证。尚未执行 fetch/pull/merge/push，仍使用初始 bundle 提交加明确同步的未提交改动。
- 未提交改动必须按文件明确同步并核对内容，不能把旧 bundle 当作完整工作区快照。
- 不在两个位置同时生成冲突提交，不使用强制重置覆盖工作。
- 启动与验证时记录代码提交、未提交差异、工具链版本和实际可执行文件。

## 验证结果与边界

最初 Windows 直连 Rust/npm 超时，路由器代理需认证。已复用现有代理认证，
仅在本次依赖安装进程中配置代理；未修改路由器、全局网络或系统默认 Node。
Rustup 与 Node 安装包已核对官方 SHA-256 后通过 SFTP 上传；不在文档或脚本保存代理凭证。

已验证：

- npm ci 成功；Node 24 下前端构建成功，30/30 测试通过。
- `verify.cmd` 完整运行退出码 0：前端 30 项通过，Rust workspace 277 项通过、0 失败、10 项按原有标记跳过。
- 跳过项包括 WVP 外部平台联调、手动媒体集成矩阵和外部 MP4 素材测试，未宣称这些已验收。
- Windows `npm run tauri -- build --debug --no-bundle` 成功；现有编译 warnings 未在环境迁移中清理。
- 通过仅手动触发的计划任务，在已登录 Session 2 启动程序，已取得真实应用窗口截图。
- Rust 测试编译中的 macOS 专属测试缺少平台条件、临时文件名含 Windows 非法字符，两处仅测试代码已修复。
- 旧 FFmpeg 下媒体时间戳测试首视频偏移 64 ms；切换 8.1.2 后同一测试通过，未修改时间戳逻辑和断言。

FFmpeg 8.1.2 Windows 构建来自 [Gyan 构建页](https://www.gyan.dev/ffmpeg/builds/)，
ZIP 在 Mac 与 Windows 两端均核对提供方 SHA-256：
`db580001caa24ac104c8cb856cd113a87b0a443f7bdf47d8c12b1d740584a2ec`。

构建/测试日志：`G:\devtools\uvp-sim\logs`，最终结果见 `verify-final.log`、
`verify-final.result.json`、`desktop-build.result.json` 与 `desktop-window.json/png`。
应用 SHA-256：`ffb9db32277b418efd02c9bb5eaa6323f1121490d9d085835064d278a42402a0`。

仅手动触发的桌面启动任务：`Codex-UVP-Simulator-Dev-20260914`，无定时触发器。
它用于从 SSH 启动到当前用户交互会话；日常也可在 Windows 桌面直接运行上述脚本。
实际窗口启动不等于摄像头、屏幕采集、平台注册或音视频联调通过。
远程桌面可能改变显示设备与会话状态，不能代替物理桌面验收。

迁移开发环境时 canonical 摄像头/屏幕采集仅实现 macOS；后续 Windows 摄像头补齐结果见下节。
此机器可承担 Windows 开发构建，但不应直接视为低配性能验收样机。

官方环境依据：[Tauri Windows 前置条件](https://v2.tauri.app/start/prerequisites/#windows)、
[Rust 安装说明](https://rust-lang.org/tools/install/)。

## P1 性能改进（2026-09-14）

已同步并构建第一批按需预览改动，详见 [实施方案与任务结果](media-performance-implementation-plan.md)。
本批 Windows Rust 287 项通过、10 项原有跳过，前端 32 项通过，桌面构建与原生最小化/恢复需求联动通过。
程序 SHA-256 为 `56c3ea8c97df4b889976d288f24b2d8fcc70219b468ea8a89da57287d891c7ca`。
日常入口仍为 `run-built.cmd`，启动输出写入 `logs\preview-phase1-runtime.log`。
窗口验收临时使用回环 WebView2 调试端口，验收后恢复正常启动入口，不持久设置系统调试变量。
P1 当时尚未实施 Windows 实时采集与 GPU 自动选型；代码保持未提交、未推送。

## Windows 摄像头接入（2026-09-14）

已补齐 DirectShow 设备枚举、精确规格探测、摄像头采集与真实 PTS 通道。
RDP 摄像头的 H.264/H.265、G.711/AAC、720p 预览暂停恢复与重开已通过；
C925e 已出现在软件目录，但实际打开仍被 DirectShow 拒绝，独立 FFmpeg 同样复现。
Windows 屏幕采集与 GPU 自动选型仍不在本批范围。

详细规格、测试结果和设备边界见 [Windows 摄像头实施记录](windows-camera-implementation.md)。
