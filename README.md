# UVP GB28181 Desktop

**多系统桌面端 GB/T 28181-2022 国标设备模拟器 + 压力测试工具**

把一台普通电脑模拟成一台（或成千上万台）GB28181 下级设备（IPC），注册到国标上级平台（WVP-Pro / EasyGBS / LiveGBS / UVP 等）进行联调与**压力测试**，无需真实摄像头、无需 Android 手机。

支持 Windows / macOS / Linux，下载安装即用，无需额外运行时依赖。

---

## 核心能力

- **设备模拟**：注册 / 心跳保活 / 目录查询 / 设备信息 / 实时点播（INVITE）/ 报警上报 / 录像查询
- **压力测试**（差异化能力）：海量虚拟设备并发注册、心跳、目录应答、RTP 推流，测量上级平台的承载能力与成功率
- **可视化**：SIP 信令实时查看、RTP 推流监控、注册/心跳/点播成功率曲线、失败原因统计、压测报告导出
- **视频源**（可插拔）：空媒体 / 轻量 RTP / 真实 H.264·H.265 文件循环推流

---

## 技术栈

| 层 | 技术 |
|---|---|
| 桌面壳 | Tauri 2 |
| 前端 UI | Vue 3 + TypeScript + Naive UI + ECharts |
| 核心引擎 | Rust + Tokio |
| 存储 | SQLite |
| UI↔引擎 | 本地 HTTP + WebSocket |

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
│   ├── scenario/           # 压测场景模型与解析（YAML）
│   └── stress-engine/      # 压测调度 + 指标采集 + 对外服务
├── docs/                   # 架构 / GB28181 / 路线图
├── assets/media/           # 内置测试码流
└── examples/               # 示例场景与配置
```

---

## 快速开始

> 前置：Rust（stable）、Node.js 20+、pnpm/npm。

```bash
# 前端依赖
cd apps/desktop && npm install

# 开发模式（起 Tauri + Vite 热更新）
npm run tauri dev

# 打当前平台安装包
npm run tauri build
```

> **macOS 打包**:`tauri build` 的 dmg 步骤在无图形会话(SSH/CI 无桌面)下会因
> Finder AppleScript 失败。用 `apps/desktop/build-macos-dmg.sh` 出 `.app` + 朴素 `.dmg`
> (hdiutil,可靠)。`.app` 双击即运行,dmg 拖拽安装。

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

MIT。GB28181 协议流程参考了开源项目 [uvp-gb28181-sim](https://gitee.com/Frank-Coding/uvp-gb28181-sim)（Android 版），本项目为独立重写的桌面端实现。
