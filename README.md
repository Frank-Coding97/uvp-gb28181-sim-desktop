# UVP GB28181 Desktop v2

> **状态: 重构中** · v2 分支完全重写，v1 代码已清零

GB/T 28181-2022 国标设备模拟器桌面端（Tauri 2 + Vue 3）。

---

## v2 重构路线（2026-07-27 启动）

基于 Codex 联合评审结论，v2 按以下顺序重建：

1. **媒体 IPC 重构** — localhost WebSocket 二进制 H.264 Annex-B（替换 v1 逐帧 JSON）
2. **SIP 事务层** — ezk-sip-core 兼容性验证或从零实现完整 RFC 3261 事务状态机
3. **压测 daemon** — 独立进程 gRPC/UDS 通信，Tokio 分片调度器

**当前进度**: 清场完成，前端只剩 Simulator.vue 单页 + App.vue 壳，后端 Rust crates 全部删除。

---

## 开发

### 前端（apps/desktop/）

```bash
npm run tauri dev    # 开发模式
npx vue-tsc --noEmit # 类型检查
npm run build        # 生产构建
```

### 后端（apps/desktop/src-tauri/）

```bash
cargo check          # 快速检查
cargo fmt            # 格式化
cargo clippy         # Lint
cargo test           # 单元测试
```

---

## 架构（v2 规划）

```
apps/desktop/
├── src-tauri/       # Tauri 后端（Rust，从零重写）
└── src/             # Vue 3 前端（保留 v1 UI 样式）
    ├── App.vue      # 应用壳（frost-blue 磨砂侧边栏）
    └── views/
        └── Simulator.vue  # 设备模拟单页（840 行）
```

**后端 crates 待重建**（按需、按 v2 路线图顺序引入）：
- [ ] 媒体推流（H.264 采集 + WebSocket 二进制推送）
- [ ] SIP 栈（ezk-sip-core 或自研完整事务层）
- [ ] GB28181 协议（MANSCDP / id_codec）
- [ ] 设备状态机（注册 / 心跳 / INVITE / PTZ）
- [ ] 压测 daemon（独立进程，gRPC）

---

## v1 vs v2

| | v1 (develop 分支) | v2 (v2 分支) |
|---|---|---|
| 状态 | 已封版，仅 bug 修复 | 重构中 |
| 后端 | 7 个 crates (12k 行) | 空壳（从零重写）|
| 前端 | 3 页（Dashboard / Channels / Simulator）| 1 页（Simulator）|
| 依赖 | 自研 sip-core + gb28181-simulator + stress-engine | 待重建 |

v1 代码可用 `git show develop:<path>` 查看，但 v2 **严禁直接拷贝** v1 代码（避免隐雷：事务层不完整 / broadcast Lagged / 逐帧 JSON IPC）。

---

## License

MIT


> 压测报告当前导出为最终指标快照 JSON;时序指标落 SQLite 为规划中(见 `docs/20-architecture/data-model.md`)。

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
