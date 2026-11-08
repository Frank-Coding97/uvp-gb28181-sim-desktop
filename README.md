<div align="center">

<img src="apps/desktop/src/assets/app-icon.svg" alt="UVP GB28181 Desktop" width="112"/>

# UVP GB28181 Desktop

**把一台电脑变成一台（或一万台）GB/T 28181 国标摄像机**

**桌面端国标设备模拟器 + 上级平台压力测试工具** · Windows / macOS / Linux

[![Release](https://img.shields.io/github/v/release/Frank-Coding97/uvp-gb28181-sim-desktop?label=release&color=2563EB)](https://github.com/Frank-Coding97/uvp-gb28181-sim-desktop/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Windows%20%C2%B7%20macOS%20%C2%B7%20Linux-brightgreen.svg)]()
[![Tauri](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)](https://tauri.app/)
[![Rust](https://img.shields.io/badge/Rust-engine-DEA584?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Vue](https://img.shields.io/badge/Vue-3-42B883?logo=vuedotjs&logoColor=white)](https://vuejs.org/)
[![GB/T 28181](https://img.shields.io/badge/GB%2FT%2028181-2016%20%2F%202022-1F6FEB)]()

</div>

---

- 官网：<https://www.uvplatform.com/>
- 当前版本：`1.0.0`（真源 `apps/desktop/src-tauri/tauri.conf.json`）
- 许可证：MIT
- 代码托管：[GitHub](https://github.com/Frank-Coding97/uvp-gb28181-sim-desktop) · [Gitee](https://gitee.com/Frank-Coding/uvp-gb28181-sim-desktop)
- 安装包：[GitHub Releases](https://github.com/Frank-Coding97/uvp-gb28181-sim-desktop/releases/latest) · [Gitee Releases](https://gitee.com/Frank-Coding/uvp-gb28181-sim-desktop/releases)

## 目录

- [什么是 UVP](#什么是-uvp)
- [为什么做这个](#为什么做这个)
- [项目亮点](#项目亮点)
- [三个项目怎么配合](#三个项目怎么配合)
- [能力一览](#能力一览)
- [界面一览](#界面一览)
- [快速开始](#快速开始)
- [技术架构](#技术架构)
- [目录结构](#目录结构)
- [版本与发布](#版本与发布)
- [已知限制](#已知限制)
- [致谢](#致谢)
- [许可证](#许可证)
- [法律声明与免责](#法律声明与免责)
- [联系我](#联系我)

---

## 什么是 UVP

**UVP（Unified Video Platform，统一视频接入平台）** 是一套面向公共安全 / 智慧城市场景的国标视频接入平台，负责把海量前端摄像头通过 GB/T 28181 协议统一接入、管理与分发。

**UVP GB28181 Desktop** 是这套生态里的桌面端工具：把一台普通电脑模拟成 GB/T 28181 下级设备（IPC），注册到任意国标上级平台做联调；再往上一步，把这一台扩展成成千上万台虚拟设备，用来测上级平台到底扛不扛得住。

> 与手机端模拟器不同，桌面端面向的是**办公室和机房里的长时间联调**：屏幕大、能抓完整 SIP 报文、能跑万级并发、能把压测结果导出来做报告。协议与压测逻辑全部用 Rust 写在引擎里，Vue 界面只负责展示与操作。

支持 GB/T 28181-2016 / -2022 双版本。Windows / macOS 发布包已内嵌 FFmpeg / ffprobe，用户无需另外安装任何运行时（Linux 包的要求见[已知限制](#已知限制)）。

---

## 为什么做这个

作者常年扎根安防产业一线，一半时间写代码、一半时间做项目运维。国标项目落地时有两个反复出现的卡点：

**第一，没有摄像头，就没法调。**

买一台合格的国标 IPC 动辄数百元，还要接线、配网、架设环境。项目前期想跑通注册、目录、点播、回放链路，往往得等一台真机到手才敢动手。出差在外、居家办公时更是无从下手。

**第二，一台设备调通了，不代表平台扛得住。**

现场要接的是几千、几万台设备。上级平台在 10 台设备时一切正常，到了 5000 台并发注册、心跳、目录应答、RTP 推流时会怎样？注册成功率往下掉的拐点在哪？失败是超时、平台拒绝还是本机端口耗尽？这些很难靠手工开几台真机测出来，而市面上能批量造设备的工具大多闭源、日志薄弱、只做信令不做媒体。

| 现有工具的常见短板 | UVP GB28181 Desktop 的回答 |
| --- | --- |
| 全部闭源，兼容性问题只能干等 | 完全开源，SIP / MANSCDP / RTP 各层可自行改 |
| 只看得到「成功 / 失败」，看不到报文 | SIP 信令实时追踪，每条消息结构化可查 |
| 只能被动等平台拉流 | 主动上报报警、位置、抓拍，订阅通知全支持 |
| 只造信令，不推真实媒体 | 媒体分 A / B / C 三档，可只跑信令也可真推 PS/RTP 码流 |
| 不支持 GB/T 28181-2022 新字段 | 2016 / 2022 双版本可在配置页切换，信令字符集随版本联动 |
| 没有压测能力，或压测结果不可复现 | 场景化编排、爬坡速率、实时指标、报告可导出 |

一句话概括定位：**手机端模拟器解决「有没有设备」，桌面端解决「设备够不够多、链路能不能查、平台撑不撑得住」。**

---

## 项目亮点

- **单机即完整国标设备**：注册（Digest 鉴权）、心跳保活与超时重注册、网络校时、OPTIONS 探活、目录 / 设备信息 / 状态 / 录像 / 配置 / 预置位查询，一条链路全走真实协议栈。
- **媒体能力不打折**：实时点播 INVITE / ACK / BYE / CANCEL、历史回放与回放倍速控制、录像下载、H.264 / H.265 PS 封装 RTP over UDP / TCP（含 TCP 被动模式）、H.264 + G.711A/U 音视频复合流、OSD 水印烧入实时码流。
- **万级并发压测（差异化能力）**：内置 10 / 100 / 500 / 1000 / 10000 台快速预设，可配爬坡速率、媒体档位、真推流设备占比，并支持批量定位与报警主动上报，对上级平台持续施压。
- **指标可解释**：注册 / 心跳 / 点播成功率曲线、TPS 滑动窗口、失败原因分类（超时、平台拒绝等），报告导出自动隐藏认证密码。
- **可视化排障**：SIP 信令实时追踪、日志分页导出、三维云台拟态摇杆（方向 / 变倍 / 预置位巡航）、通知中心。
- **业务数据成体系**：目录管理（含 215 / 216 下级平台模板）、录像周计划、抓拍图库、报警单与报警历史，方便复现真实项目里的业务场景。
- **协议与 UI 彻底分层**：Rust workspace 自下而上分 8 个 crate，引擎可脱离界面独立运行，为后续 CLI / 多 Agent 分布式压测留好接口。
- **装完即用**：Windows / macOS 发布包内嵌 FFmpeg 与平台依赖库，不需要用户再配环境；三平台同一份代码构建。

---

## 三个项目怎么配合

| 项目 | 角色 | 什么时候用 |
| --- | --- | --- |
| [UVP-GB28181](https://github.com/Frank-Coding97/uvp-gb28181) | 国标上级平台（服务端） | 需要一套能接入、管理、分发视频的平台，或需要验证平台侧功能 |
| [UVP GB28181 Sim](https://github.com/Frank-Coding97/uvp-gb28181-sim) | 手机端单设备模拟器（Android / iOS） | 手边只有手机、出差现场，先跑通一条注册 → 点播 → 回放链路 |
| **UVP GB28181 Desktop**（本项目） | 桌面端单设备模拟器 + 万级压测工具 | 办公室 / 机房里长时间联调、抠协议细节、测平台承载上限 |

三者都遵循同一套 GB/T 28181 协议语义，可以互相注册联调：拿本项目的虚拟设备注册到 UVP-GB28181，或直接对接 WVP-Pro、EasyGBS、LiveGBS 等第三方国标平台。

---

## 能力一览

> 以下均为当前版本已实现的能力，按业务域归类；不同上级平台对国标字段的支持程度以对方实现为准。

### 设备模拟与信令

| 能力 | 说明 |
| --- | --- |
| 注册与鉴权 | SIP REGISTER + Digest、注册有效期协商、掉线超时自动重注册 |
| 保活与探活 | Keepalive 心跳、OPTIONS 保活 |
| 网络校时 | SIP Date 校时、SNTP 手动同步、设备校时页面 |
| 协议版本 | GB/T 28181-2016 / -2022 可切换，信令字符集（GB2312 / GB18030 / UTF-8）随版本联动 |
| 传输方式 | UDP、TCP（共享连接池）、媒体 TCP 被动模式 |
| 设备查询 | 目录、设备信息、设备状态、录像查询、设备配置、预置位查询应答 |
| 硬件信息 | 云台六字段、硬件版本可配置（2016 不输出硬件版本） |

### 媒体与点播

| 能力 | 说明 |
| --- | --- |
| 媒体源 | 内置演示视频、本地视频文件（MP4 / FLV / MKV 等，ffmpeg 转封装）、摄像头、屏幕采集 |
| 编码与封装 | H.264 / H.265，音频 G.711A / G.711U / AAC，PS 封装后走 RTP |
| 实时点播 | INVITE / ACK / BYE / CANCEL 全流程，主辅码流、音频可单独关闭 |
| 本地预览 | WebCodecs 硬件解码，默认不自动预览、点按钮开启 |
| 历史回放 | 录像检索、回放、会话内 INFO 倍速 / 暂停控制、录像下载 |
| 码流加工 | OSD 水印烧入实时码流，支持平台下发 OSDConfig / PictureMask |

### 业务与设备控制

| 能力 | 说明 |
| --- | --- |
| 目录管理 | 国标目录树、业务分组 / 虚拟组织、215 / 216 下级平台模板、通道增删改 |
| 云台控制 | 方向 / 变倍 / 聚焦 / 光圈、预置位设置调用删除、看守位、巡航、强制关键帧、三维拟态摇杆 |
| 录像业务 | 定时录像周计划（相邻段自动合并、每天最多 8 段）、录像中心、缩略图与时间筛选 |
| 图像与报警 | 本地抓拍图库、抓拍上传、报警单管理、报警历史、日志导出 |
| 订阅通知 | 目录订阅、报警订阅（对话内 NOTIFY）、移动位置周期上报、订阅运行详情 |

### 压力测试

| 能力 | 说明 |
| --- | --- |
| 批量设备 | 线性场景按连续 20 位 ID 生成 N 台设备，上限 10,000 台，快速预设 5 档 |
| 施压策略 | 爬坡速率（台/秒）避免瞬时注册风暴；媒体 A 空媒体 / B 轻量伪流 / C 真实码流三档；`active_ratio` 控制真推流设备占比 |
| 主动上报 | 可配周期批量上报 GPS 位置与报警，对平台通知链路施压 |
| 实时指标 | 注册成功率、TPS（10 秒滑动窗口）、运行时长、成功 / 失败计数、平台拒绝等失败归因 |
| 报告 | 一键导出最终指标快照 JSON，认证密码自动隐藏 |

### 可视化与排障

- **SIP 信令实时追踪**：每条报文可查，日志按标签分页并支持导出，不用抓包猜。
- **成功率曲线**：注册 / 心跳 / 点播趋势图 + 失败原因统计，拐点一眼可见。
- **模拟器中心**：单设备状态、云台三维朝向、点播与采集来源集中操作。
- **通知中心**：订阅、报警、录像等运行事件统一汇总。

---

## 界面一览

| 菜单 | 用途 |
| --- | --- |
| 首页 | 运行状态总览、平台连接状态、本地预览入口 |
| 模拟器中心 | 单设备启停、采集来源、云台拟态摇杆、点播控制 |
| 目录管理 | 国标目录树与通道配置、模板套用 |
| 录像中心 | 录像文件、缩略图、时间筛选、回放与下载 |
| 抓拍图库 | 本地抓拍与导出 |
| 报警管理 | 报警单与报警历史 |
| 录像计划 | 周时间表涂色配置定时录像 |
| 压力测试 | 场景编排、预设、实时指标、报告导出 |
| 设备配置 | 设备 / 通道 / SIP 接入参数、国标版本、硬件信息 |
| 音视频配置 | 编码、码率、帧率、媒体源、音频开关 |
| OSD 水印 | 文字与位置、平台下发配置 |
| 网络配置 | 本机绑定地址、SIP 信令追踪开关 |
| 设备校时 | SNTP 同步与校时状态 |
| 日志 | 分类日志查看与导出 |
| 关于 | 版本、协议、桌面框架与配置真相源 |

---

## 快速开始

### 下载安装包（推荐）

到 [GitHub Releases](https://github.com/Frank-Coding97/uvp-gb28181-sim-desktop/releases/latest) 或 [Gitee Releases](https://gitee.com/Frank-Coding/uvp-gb28181-sim-desktop/releases) 下载对应平台的安装包：

| 平台 | 安装包（以 1.0.0 为例） | 说明 |
| --- | --- | --- |
| Windows x64 | `UVP GB28181 Desktop_1.0.0_x64-setup.exe` / `..._x64_en-US.msi` | 双击安装，FFmpeg 及依赖 DLL 已内嵌 |
| macOS Apple Silicon | `UVP-GB28181-Desktop_1.0.0_aarch64.dmg` | 双击后把 app 拖进「应用程序」 |
| Linux x86_64 | `.deb` / `.AppImage` | 由 CI 构建，需系统自行提供 FFmpeg |

首次启动会请求摄像头、麦克风、屏幕录制权限，**需要全部允许**，否则对应采集功能不可用。

> ⚠️ 当前安装包**未做代码签名 / Apple 公证**，系统会拦截，这是预期提示：
>
> - **Windows**：SmartScreen 弹「未知发布者」时，点「更多信息」→「仍要运行」。
> - **macOS**：「系统设置」→「隐私与安全性」→ 找到被拦下的应用点「仍要打开」。若提示「已损坏」，在终端执行
>   `xattr -dr com.apple.quarantine "/Applications/UVP GB28181 Desktop.app"` 即可。

### 5 分钟联调（配 WVP-Pro 上级平台）

最快的方式是本地起一个 [WVP-Pro](https://github.com/648540858/wvp-GB28181-pro)：

```bash
docker run -d --name wvp \
  -p 18080:18080 -p 5060:5060/udp -p 30000-30500:30000-30500 \
  648540858/wvp_pro:latest
# 控制台: http://localhost:18080  (admin / admin)
```

然后在应用顶栏新增一个平台档案，填写：

| 项 | 示例值 |
| --- | --- |
| 设备 ID | `34020000001320000001` |
| 平台 ID（SIP 域） | `34020000002000000001` |
| 服务地址 / 端口 | 运行 WVP 的主机 IP / `5060` |
| 认证密码 | 与 WVP 控制台「国标设备」配置一致 |
| 传输 / 国标版本 | UDP / GB/T 28181-2022 |

保存后到「模拟器中心」启动设备，平台侧应能看到设备上线；再点播通道即可走通 INVITE → 推流 → 出画。

### 从源码运行

环境要求：Rust 1.75+、Node.js 20.12+（建议 Node 22，CI 用 20）、各平台 Tauri 2 系统依赖。

```bash
git clone https://github.com/Frank-Coding97/uvp-gb28181-sim-desktop.git
cd uvp-gb28181-sim-desktop/apps/desktop

npm ci
npm run tauri dev        # 起 Tauri + Vite 热更新
npm run build            # 仅构建前端并做 vue-tsc 类型检查
```

开发态媒体转封装会读 PATH 里的 `ffmpeg`，也可用 `FFMPEG_BIN` 指向指定二进制。Rust 侧单独校验：

```bash
cargo check --workspace
cargo test --workspace
```

### 构建发布包

> **发布包必须用平台脚本**，不能把裸 `npm run tauri build` 当最终交付。平台脚本会校验并内嵌 FFmpeg / ffprobe，避免用户装完再补运行时。

```bash
cd apps/desktop

# macOS：产出 .app 与 .dmg（内嵌 FFmpeg + Swift runtime + ad-hoc 或正式签名）
BUNDLE_FFMPEG=1 ./build-macos-dmg.sh

# Windows：PowerShell，产出 x64-setup.exe / .msi（自动带齐 FFmpeg 依赖 DLL）
.\build-windows.ps1

# Linux：CI 走标准 tauri build，产物为 .deb / .AppImage
npm run tauri build -- --target x86_64-unknown-linux-gnu
```

常用环境变量：

| 变量 | 作用 |
| --- | --- |
| `FFMPEG_BIN` / `FFPROBE_BIN` | 指定要内嵌的 ffmpeg / ffprobe 路径 |
| `BUNDLE_FFMPEG` | macOS 脚本是否内嵌 FFmpeg（发布包固定 `1`） |
| `APPLE_SIGNING_IDENTITY` | 正式签名身份，未设置时为 ad-hoc 签名 |
| `APPLE_NOTARY_PROFILE` | 设置后提交 Apple 公证并装订 DMG |

> macOS 脚本用系统底层 `hdiutil` 生成朴素但可靠的 DMG，不依赖访达 + AppleScript 摆窗口，因此在 SSH / CI / 无图形桌面会话下也能跑完。

### 测试与质量门禁

CI（`.github/workflows/ci.yml`）在每个 PR / push 跑 Rust 质量门，通过后并行构建三平台安装包。本地复现：

```bash
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test --workspace

cd apps/desktop
npm run build
node --test
```

前端有 49 个 `*.test.mjs`，覆盖界面契约、布局与预览逻辑；Rust 侧覆盖 SIP / MANSCDP / RTP / 目录 / 录像计划等核心逻辑。

---

## 技术架构

设计原则：**协议与压测逻辑全部在 Rust 引擎，UI 只负责展示与操作**。引擎当前内嵌在 Tauri 进程里，也可作为独立二进制运行，未来可拆成 CLI 与多 Agent 分布式压测。

```text
┌─────────────────────────────────────────────────────────────┐
│  Vue 3 + TypeScript + Naive UI + ECharts（apps/desktop/src）   │
│  首页 / 模拟器中心 / 目录 / 录像 / 报警 / 压测 / 配置 ...         │
└───────────────────────────┬─────────────────────────────────┘
                            │ Tauri IPC 命令 + 事件流
┌───────────────────────────▼─────────────────────────────────┐
│  Tauri 2 后端（apps/desktop/src-tauri）                        │
│  配置持久化 / 抓拍与日志 / 预览管理 / 引擎生命周期               │
└───────────────────────────┬─────────────────────────────────┘
                            │ 直接内嵌调用
┌───────────────────────────▼─────────────────────────────────┐
│  stress-engine   压测调度 + 指标聚合 + 报告                     │
│  scenario        场景模型（TOML）/ 批量设备配置生成              │
│  gb28181-simulator  单设备状态机（一台虚拟 IPC）                │
│  gb28181-protocol   MANSCDP XML + 20 位编码规则                │
│  sip-core           通用 SIP：消息 / 事务 / 传输 / Digest       │
│  media-rtp          RTP / PS 封装 / 码流推送 / 转封装           │
│  record-plan        录像周计划纯逻辑                            │
│  common             公共类型、错误、日志、NTP、观察者             │
└─────────────────────────────────────────────────────────────┘
```

分层依赖自下而上、禁止环依赖：`common → sip-core / gb28181-protocol / media-rtp → gb28181-simulator → scenario → stress-engine`。

**一次实时点播的基本链路**：上级平台发 INVITE（SDP 带接收地址与端口）→ 设备侧解析并应答 200 OK → 平台 ACK → `media-rtp` 从媒体源取帧、PS 封装、按 RTP 打包推流 → 平台 BYE 时停止推流并回收会话。信令成功但没画面时，可以沿这条链路逐层用 SIP 追踪与日志判断卡在哪一步。

| 层 | 技术 |
| --- | --- |
| 桌面壳 | Tauri 2 |
| 前端 UI | Vue 3 + TypeScript + Naive UI + ECharts + Three.js |
| 核心引擎 | Rust + Tokio |
| UI ↔ 引擎 | Tauri IPC 命令 + 事件流 |
| 媒体转封装 | 发布包内置 FFmpeg / ffprobe（开发态可用 PATH 或 `FFMPEG_BIN` 覆盖） |

---

## 目录结构

```text
uvp-gb28181-sim-desktop/
├── apps/desktop/
│   ├── src/                # Vue 3 前端（views / components / 契约测试 *.test.mjs）
│   ├── src-tauri/          # Tauri 2 Rust 后端、打包配置、tauri.conf.json
│   ├── build-macos-dmg.sh  # macOS 发布包脚本（内嵌 FFmpeg + Swift runtime + DMG）
│   └── build-windows.ps1   # Windows 发布包脚本（能力校验 + 内嵌 FFmpeg/DLL）
├── crates/
│   ├── common/             # 公共类型、错误、配置、日志、NTP
│   ├── sip-core/           # 通用 SIP 协议栈（消息 / 事务 / 传输 / Digest / SDP）
│   ├── gb28181-protocol/   # MANSCDP XML + 国标编码规则 + PTZ / 报警
│   ├── gb28181-simulator/  # 单设备状态机（注册 / 心跳 / 目录 / INVITE…）
│   ├── media-rtp/          # RTP / PS 封装 / 码流推送 / 文件转封装 / 预览
│   ├── record-plan/        # 录像周计划模型与判定（纯逻辑）
│   ├── scenario/           # 压测场景模型与解析（TOML）
│   └── stress-engine/      # 压测调度 + 指标采集 + 对外服务
├── scripts/                # FFmpeg 能力检查、macOS 运行时内嵌、版本号单源化
├── assets/media/           # 测试码流
├── examples/scenarios/     # 示例压测场景（linear.toml）
└── .github/workflows/      # CI：Rust 质量门 + 三平台构建 + tag 发布
```

---

## 版本与发布

- **版本号唯一真源**是 `apps/desktop/src-tauri/tauri.conf.json`（Tauri 打包实读），`package.json`、根 `Cargo.toml`、`Cargo.lock` 由脚本同步：

  ```bash
  node scripts/set-version.mjs            # 查看当前版本
  node scripts/set-version.mjs 1.0.0      # 升版，一次同步所有副本
  ```

- CI 在 `main` / `develop` 的 push 与 PR 上跑 `cargo fmt --check`、clippy、workspace 测试，并构建 macOS / Linux / Windows 三平台安装包。
- 推送 `v*` tag 会触发 Release 发布；仓库同时维护 GitHub 与 Gitee 两个远程，发版需两边同步。
- 版本基线为 `1.0.0`，各版本变更见 Releases 页面。

---

## 已知限制

- **macOS 发布包仅提供 arm64**（Apple Silicon）；Intel 芯片的 Mac 需自行从源码构建。
- 两个平台的安装包**未做代码签名 / Apple 公证**，首次运行需按上文步骤放行；对外正式交付应使用 Developer ID 签名并完成公证。
- 压测**单机上限 10,000 台**，实际可承载数量受本机网卡、端口、内存与上级平台性能共同限制。
- 压测报告目前导出的是**最终指标快照 JSON**，时序指标落 SQLite 仍在规划中。
- Linux 安装包由 CI 直接构建，**未内嵌 FFmpeg**，目标机器需自行安装。
- 引擎与界面当前同进程运行，CLI 与多 Agent 分布式压测为后续规划。

---

## 致谢

- 协议实现参考 **GB/T 28181-2016** 与 **GB/T 28181-2022** 标准文本。
- 思路与协议覆盖受益于同生态的 [UVP-GB28181](https://github.com/Frank-Coding97/uvp-gb28181) 平台与 [UVP GB28181 Sim](https://github.com/Frank-Coding97/uvp-gb28181-sim) 移动端模拟器，本项目是 Rust + Tauri 的独立重写，额外提供万级设备压测能力。
- 媒体转封装依赖 [FFmpeg](https://ffmpeg.org/)，桌面壳依赖 [Tauri](https://tauri.app/)，前端使用 Vue 3 / Naive UI / ECharts 等开源组件，各组件遵循其各自许可证。

---

## 许可证

本项目基于 [MIT License](LICENSE) 开源。个人和企业均可在遵守许可证要求的前提下使用、修改、分发或用于商业项目，请保留原始版权与许可声明。第三方组件遵循各自许可证。

---

## 法律声明与免责

> 本项目是**技术工具**，不是使用授权，也不是合规背书。任何违法违规使用均与作者无关，法律责任由使用者自行承担。

**1. 使用边界。** 本项目是**被控端设备模拟器与压测工具**，用于替代真实摄像机做授权环境下的联调与容量评估。仅可在你拥有所有权、或已取得**书面授权**的设备、网络与平台上使用。压测会产生大量注册与媒体流量，**严禁对未获书面授权的第三方平台施压**，也不得用于任何未经授权的设备接入、信令探测、视频调阅或干扰他人网络正常运行的行为。项目本身不提供扫描、爆破、绕过鉴权能力，也不附带任何第三方平台的账号或凭证。

**2. 合规责任。** 使用者须遵守所在地法律法规，包括但不限于《中华人民共和国网络安全法》《数据安全法》《个人信息保护法》以及刑法相关规定。将本项目用于未授权接入、干扰或数据获取可能构成违法或犯罪，由此产生的一切后果由使用者自行承担。

**3. 数据与隐私。** 项目自身不含远程服务器、遥测或后台上报。信令、媒体流、抓拍与位置数据仅发往**使用者在应用内自行配置的国标平台**；压测产生的示例位置数据为固定测试坐标。数据的流向、留存与二次使用由使用者的配置与行为决定。

**4. 无担保。** 软件按「现状」（AS IS）提供，不附带任何明示或默示担保。因使用或无法使用本项目导致的业务中断、数据丢失、平台过载或任何间接损失，作者及贡献者不承担责任。压测前请自行确认目标平台与网络链路的承载能力，并做好隔离与回滚准备。

**5. 名称与商标。** 「GB/T 28181」是国家标准编号，本项目与该标准的制定机构无隶属关系，亦未获得背书或认证。文中出现的 WVP-Pro、EasyGBS、LiveGBS 等名称与商标归各自权利人所有，仅用于说明兼容性，不构成合作或授权关系。

**6. 第三方组件。** 本项目依赖的第三方库遵循各自许可证。MIT 许可仅授予著作权层面的使用自由，并不豁免上述使用边界与合规义务。

如不同意本声明的任何条款，请立即停止使用并删除本项目的全部副本。

---

## 联系我

遇到 Bug、想要新功能、做平台兼容性反馈，欢迎提 [GitHub Issue](https://github.com/Frank-Coding97/uvp-gb28181-sim-desktop/issues)（Gitee 侧也有 [Issues](https://gitee.com/Frank-Coding/uvp-gb28181-sim-desktop/issues)）。把 SIP 报文片段、平台类型与复现步骤一起贴出来，排查会快很多；讨论留在 Issue 区也方便后来的人检索。

产品介绍与各端下载入口见官网：<https://www.uvplatform.com/>

如果这个项目对你有帮助，欢迎点个 Star，或者把它推荐给身边做国标接入的同行。
