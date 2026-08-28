# UVP GB28181 Desktop

**多系统桌面端 GB/T 28181-2022 国标设备模拟器 + 压力测试工具**

把一台普通电脑模拟成一台（或成千上万台）GB28181 下级设备（IPC），注册到国标上级平台（WVP-Pro / EasyGBS / LiveGBS / UVP 等）进行联调与**压力测试**，无需真实摄像头、无需 Android 手机。

支持 Windows / macOS / Linux，下载安装即用，无需额外运行时依赖。

---

## 核心能力

- **设备模拟**：注册（Digest MD5 / MD5-sess，支持 qop=auth·auth-int、opaque、stale 重挑战）/ 心跳保活+超时重注册 / **尊重平台授予的 Expires** / **网络校时**（SIP Date）/ OPTIONS 探活 / 目录（超 MTU 自动分多条）、设备信息、状态、录像、配置、预置位查询 / GB-2016·2022 双版本
- **媒体**：实时点播 INVITE·ACK·BYE·CANCEL / **历史回放 + 倍速播放 + 录像下载** / H.264·H.265 PS 封装 RTP over UDP·TCP / **音视频复合流（H.264 + G.711A）** / **MP4·FLV·MKV 容器**（ffmpeg 转封装）
- **设备控制**：PTZ 云台方向·变倍 / **预置位设置·调用·删除** / 看守位 / 强制关键帧
- **订阅通知**：目录 / 报警（对话内 NOTIFY）/ 移动位置周期上报；主动上报报警 / GPS 位置
- **压力测试**（差异化能力）：海量虚拟设备并发注册、心跳、目录应答、RTP 推流 + **批量定位/报警主动上报**，测量上级平台承载能力与成功率
- **信令传输**：**UDP 与 SIP over TCP 双传输**（TCP 走 RFC 3261 §18.3 流式重组，大目录树不受 MTU 限制）/ RFC 3261 客户端与**服务端事务**（请求重传去重 + 响应缓存重发）
- **可视化**：**SIP 信令实时追踪** / **云台控制拟态摇杆演示**（方向/变倍/预置位巡航）/ 上线成功率·在线设备数·**注册延迟 p95** 曲线 / 失败原因统计（含"本机瓶颈"独立归因）/ 压测报告导出（含每秒时序）
- **外观**：浅色 / 暗色 / 跟随系统

---

## 技术栈

| 层 | 技术 |
|---|---|
| 桌面壳 | Tauri 2 |
| 凭据存储 | OS 钥匙串(Keychain / Credential Manager / libsecret) |
| 前端 UI | Vue 3 + TypeScript + Naive UI + ECharts |
| 核心引擎 | Rust + Tokio |
| UI↔引擎 | Tauri IPC 命令 + 事件流 |
| 媒体转封装 | 系统 ffmpeg(仅 MP4 等容器源 / 音频抽取时需要) |

> 压测报告导出 JSON,包含最终快照 + **每秒一个采样点的时序**(引擎运行期追加写
> `<app 数据目录>/uvp-reports/series-run<id>-<ts>.jsonl`)。落 SQLite 仍为规划中
> (见 `docs/20-architecture/data-model.md`)。

### 压测结论的可信度

指标快照带 `results_trustworthy` 与 `local_pressure`:当本机出现队列丢弃、socket
发送失败或报文解析失败时,失败数里就混进了**本机瓶颈**,不能直接当成平台承载上限。
UI 会在这种情况下显示告警条,建议先降设备数/放宽爬坡再重测。

万级压测的现实边界:**信令档(A/B)可行,媒体档必须靠 `active_ratio` 采样** ——
真推流是每路一个编码器/子进程,单机不可能几千路。同素材在设备间通过 `Arc` 共享
(不再每台一份拷贝),这是万级的内存前提。

设计原则：**协议与压测逻辑全部在 Rust 引擎，UI 只负责展示与操作**。引擎可独立于 UI 运行（未来支持 CLI 与多 Agent 分布式压测）。

---

## 仓库结构

```
uvp-gb28181-desktop/
├── apps/
│   └── desktop/            # Tauri 桌面应用
│       ├── src-tauri/      # Rust 侧（Tauri 后端，拉起/嵌入 stress-engine）
│       └── src/            # Vue 3 前端
├── crates/                 # Rust workspace 各功能库
│   ├── common/             # 公共类型、错误、配置、日志
│   ├── sip-core/           # SIP 协议栈（消息/事务/事务层/传输）
│   ├── gb28181-protocol/   # GB28181 MANSCDP XML + 设备/通道编码规则
│   ├── gb28181-simulator/  # 单设备状态机（注册/心跳/目录/INVITE…）
│   ├── media-rtp/          # RTP / PS 封装 / 码流推送
│   ├── scenario/           # 压测场景模型与解析（TOML）
│   └── stress-engine/      # 压测调度 + 指标采集 + 对外服务
├── docs/                   # 架构 / GB28181 / 路线图
├── SECURITY.md             # 凭据存储、来源校验、SSRF、权限面(改相关代码前必读)
├── assets/media/           # 内置测试码流
└── examples/               # 示例场景与配置
```

---

## 快速开始

> 前置：Rust（stable）、Node.js 20+、pnpm/npm。
> 可选：**ffmpeg**（仅当视频源用 MP4/FLV/MKV 容器或需推送音频时;纯 .h264/.h265 裸流不需要）。

```bash
# 前端依赖
cd apps/desktop && npm install

# 开发模式（起 Tauri + Vite 热更新）
npm run tauri dev

# 打当前平台安装包
npm run tauri build
```

首次启动后需在顶栏「目标平台 → 编辑」里填一次 SIP 密码:密码存进 OS 钥匙串,
之后顶栏会显示 🔒「密码已配置」。**未配置密码时注册与压测会被前置拦住**。

质量门(与 CI 一致):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

cd apps/desktop
npx vue-tsc --noEmit   # 类型检查
npx vitest run         # 前端单测
npx vite build         # 生产构建
```

> **macOS 打包**:用脚本 `apps/desktop/build-macos-dmg.sh` 打包,产出 `.app`(双击即运行)
> 和 `.dmg`(分发用,挂载后把 app 拖进「应用程序」即安装)。
>
> 为什么用脚本而非 `tauri build`:Tauri 官方生成"带背景图/图标布局"的精美 dmg 时,要调 macOS
> 访达(Finder)+ AppleScript 摆窗口,**这一步依赖图形桌面**——在 SSH 远程 / CI / 无桌面会话下会
> 失败,导致整个打包中断。本脚本改用系统底层命令 `hdiutil` 打一个**朴素但可靠**的 dmg(无花哨
> 布局,拖拽安装),绕开该限制。有图形桌面时也可正常用 `npm run tauri build`。

---

## 文档

完整文档见 [`docs/`](docs/)(文档中心 + 文档先行约定)。常用入口:

- [产品愿景](docs/00-product/vision.md) · [需求清单 FR/NFR](docs/00-product/requirements.md)
- [功能规格:设备模拟](docs/10-functional/device-simulation.md) · [压力测试](docs/10-functional/stress-testing.md)
- [架构总览](docs/20-architecture/overview.md) · [UI↔引擎 API 契约](docs/20-architecture/api-contract.md) · [数据模型](docs/20-architecture/data-model.md)
- [各 crate 规格](docs/30-crates/) · [协议规格](docs/40-protocol/)
- [路线图与并发分工](docs/90-process/roadmap.md) · [编码规范](docs/90-process/coding-standards.md) · [测试策略](docs/90-process/testing-strategy.md)

---

## License

MIT。协议实现参考了 **GB/T 28181-2016** 与 **GB/T 28181-2022** 标准文档,以及开源项目 [uvp-gb28181-sim](https://gitee.com/Frank-Coding/uvp-gb28181-sim)(通用 GB28181 模拟程序),本项目为 Rust + Tauri 独立重写的跨平台桌面端实现,并额外提供万级设备压测能力。
