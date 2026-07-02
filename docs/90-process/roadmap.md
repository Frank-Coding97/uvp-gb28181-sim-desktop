# 开发路线图与并发分工

**状态:已评审** · 里程碑与需求(`00-product/requirements.md`)、验收用例(`00-product/use-cases.md`)对应。

## 里程碑

| 阶段 | 目标 | 覆盖需求 | 验收 | 状态 |
|---|---|---|---|---|
| **M0 骨架** | Cargo workspace + 各 crate 骨架 + Tauri+Vue 壳 + 全套文档 | FR-45 | `cargo build/test/clippy` 全绿;`tauri dev` 起窗口 | ✅ 完成 |
| **M1 单设备注册** | sip-core 收发 + Digest;单设备 REGISTER/心跳/OPTIONS | FR-1~4 | WVP 控制台 1 台设备**在线**(UC-1) | 进行中 |
| **M2 查询与点播** | 目录/设备信息应答;INVITE→RTP 推流(真实 H.264) | FR-5~8, FR-11 | WVP 看到目录 + **实时画面**(UC-1) | 计划 |
| **M3 压测引擎** | scenario 调度器 + 指标 + 事件推送;空媒体档万级并发 | FR-20~27 | UI 曲线显示成功率(UC-2) | 计划 |
| **M4 UI 完善** | 配置向导、场景编排、监控大盘、报告导出 | FR-28, FR-40~44 | 非技术用户独立跑一次压测 | 计划 |
| **M5 打包分发** | 三平台安装包 + 签名/公证 | NFR-1/2 | Win/macOS/Linux 下载即用 | 计划 |

---

## 并发分工(git worktree)

M0 合入 `develop` 后三条线并行。每条线一个 worktree + feature 分支,独立提交,完成合回 `develop`。

| worktree 目录 | 分支 | 负责范围 | crate 规格 |
|---|---|---|---|
| `../uvp-gb-protocol` | `feat/protocol` | sip-core + gb28181-protocol + gb28181-simulator + scenario | `30-crates/{sip-core,gb28181-protocol,gb28181-simulator,scenario}.md` |
| `../uvp-gb-media` | `feat/media` | media-rtp:RTP/PS/H264 推流 | `30-crates/media-rtp.md` |
| `../uvp-gb-ui` | `feat/desktop-ui` | apps/desktop 前端 + Tauri 命令 + 监控大盘 | `20-architecture/api-contract.md` |

`stress-engine`(调度+服务)在协议线与媒体线收敛后集成。

### worktree 操作

```bash
git worktree add ../uvp-gb-protocol -b feat/protocol develop   # 建(已建)
git worktree list                                              # 查看
git worktree remove ../uvp-gb-media                            # 合并后清理
```

### 接口契约先行

跨线依赖靠文档契约解耦:UI 线按 `20-architecture/api-contract.md` 的命令/事件 JSON 结构用 mock 开发,不等引擎;媒体线按 `30-crates/media-rtp.md` 的 `VideoSource` trait 开发。**契约变更必须先改文档并知会相关线**(见 `docs/README.md#核心约定`)。

---

## 提交约定

见 `coding-standards.md`。`<type>(<scope>): <中文描述>`,协议/领域改动需配单测。
