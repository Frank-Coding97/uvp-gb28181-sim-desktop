# 开发路线图与并发分工

## 里程碑

| 阶段 | 目标 | 验收 |
|---|---|---|
| **M0 骨架** | Cargo workspace + 各 crate 空骨架 + Tauri+Vue 壳 | `cargo build` 全绿；`tauri dev` 起窗口 |
| **M1 单设备注册** | sip-core 基本收发 + Digest；单设备 REGISTER/心跳/OPTIONS | WVP 控制台看到 1 台设备**在线** |
| **M2 查询与点播** | 目录/设备信息应答；INVITE→RTP 推流（真实 H.264 文件） | WVP 看到目录 + **实时画面** |
| **M3 压测引擎** | scenario 解析 + 调度器 + 指标 + WS 推送；空媒体档万级并发 | UI 曲线显示注册/心跳成功率 |
| **M4 UI 完善** | 配置向导、场景编排、监控大盘、报告导出 | 非技术用户可独立跑一次压测 |
| **M5 打包分发** | 三平台安装包 + 签名/公证 | Win/macOS/Linux 下载即用 |

---

## 并发分工（git worktree）

M0 骨架合入 `develop` 后，三条线可并行。每条线一个 worktree + feature 分支，各自独立提交，完成后合回 `develop`。

| worktree 目录 | 分支 | 负责范围 | 依赖 |
|---|---|---|---|
| `../uvp-gb-protocol` | `feat/protocol` | `sip-core` + `gb28181-protocol` + `gb28181-simulator` 单设备信令 | common |
| `../uvp-gb-media` | `feat/media` | `media-rtp`：RTP/PS/H264 文件推流 | common |
| `../uvp-gb-ui` | `feat/desktop-ui` | `apps/desktop` 前端 + Tauri 命令 + 监控大盘 | 引擎 API 契约 |

`stress-engine`（调度+服务）在协议线与媒体线收敛后，于 develop 或单独分支集成。

### worktree 操作

```bash
# 建（基于 develop）
git worktree add ../uvp-gb-protocol -b feat/protocol develop
git worktree add ../uvp-gb-media    -b feat/media    develop
git worktree add ../uvp-gb-ui       -b feat/desktop-ui develop

git worktree list          # 查看
git worktree remove ../uvp-gb-media   # 合并后清理
```

### 接口契约先行

跨线依赖靠 `docs/architecture.md` 的类型契约解耦：UI 线按约定的 HTTP/WS JSON 结构开发，不必等引擎实现；媒体线按 `media-rtp` 对 simulator 暴露的 trait 开发。契约变更必须先改文档并知会相关线。

---

## 提交约定

`<type>(<scope>): <描述>`，type ∈ `feat/fix/refactor/docs/test/chore/perf/ci`。scope 用 crate 名或 `desktop`。协议/领域改动需配单测。
