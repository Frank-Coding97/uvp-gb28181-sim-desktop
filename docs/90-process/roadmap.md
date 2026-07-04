# 开发路线图与并发分工

**状态:已评审** · 里程碑与需求(`00-product/requirements.md`)、验收用例(`00-product/use-cases.md`)对应。

## 里程碑

| 阶段 | 目标 | 覆盖需求 | 验收 | 状态 |
|---|---|---|---|---|
| **M0 骨架** | Cargo workspace + 各 crate 骨架 + Tauri+Vue 壳 + 全套文档 | FR-45 | `cargo build/test/clippy` 全绿;`tauri dev` 起窗口 | ✅ 完成 |
| **M1 单设备注册** | sip-core 收发 + Digest;单设备 REGISTER/心跳/OPTIONS/SUBSCRIBE | FR-1~4 | WVP 控制台 1 台设备**在线**(UC-1) | ✅ **WVP 验证通过** |
| **M2 查询与点播** | 目录/设备信息/录像应答;INVITE→RTP/TCP 推流(真实 H.264) | FR-5~8, FR-10 | WVP 看到目录 + **实时画面**(UC-1) | ✅ **WVP 验证通过(出 FLV 流)** |
| **M3 压测引擎** | scenario 场景 + 编排器 + CLI;批量并发注册 + 原子指标 | FR-20~27 | 多设备批量注册(UC-2) | ✅ 核心完成 |
| **M4 UI 完善** | 平台配置、场景编排、监控大盘(ECharts)+ 报告导出 | FR-40~44, FR-28 | 非技术用户独立跑一次压测 | ✅ 核心完成 |
| **M5 打包分发** | 三平台安装包 + 签名/公证 | NFR-1/2 | Win/macOS/Linux 下载即用 | 🚧 macOS dmg 已验证,Win/Linux 靠 CI |
| **M6 协议全覆盖对齐上游** | 扩展查询/控制/抓拍/升级/主动通知/信令健壮性 + H.265/AAC + 多通道·OSD·语音广播 | FR-17~19, FR-30~36 | 对标 uvp-gb28181-sim GB-2022 覆盖矩阵 100%(去除明确不做项);SIP 注入/真机 WVP 验证 | 🚧 进行中 |

### M6 分档推进(每档:文档→代码→单测/注入验证→更新覆盖矩阵→commit)

| 档 | 内容 | 需求 | 验证 |
|---|---|---|---|
| P1 查询 | AlarmStatus / HomePositionQuery / StorageCardStatusQuery / CruiseTrackListQuery / CruiseTrackQuery / PTZPreciseStatusQuery / MobilePosition 单次 / ConfigDownload+VideoParamOpt | FR-17 | SIP 注入 |
| P2 控制 | PTZPreciseCtrl / 巡航控制 / 辅助控制 / Focus / TargetTrack / FormatSDCard | FR-18 | SIP 注入 + WVP |
| P3 抓拍升级 | SnapShotCmd / SnapShotConfig(HTTP PUT) / DeviceUpgrade 4 步 | FR-19, FR-30 | SIP 注入 + 本地 HTTP 收图 |
| P4 通知健壮 | MediaStatus / Expires 续约 / RTCP SR / Catalog 增量 NOTIFY | FR-31, FR-32 | 单测 + WVP |
| P5 媒体 | H.265 / AAC | FR-33 | ffmpeg 转封装实测 |
| P6 多通道+OSD | 虚拟通道 CRUD+模板 / 报警通道节点 / OSD 叠加 | FR-34, FR-35 | headless Chrome + WVP 目录树 |
| P7 语音广播 | Broadcast 应答 + 反向 INVITE UAC + 音频接收 | FR-36 | (本环境无法触发,尽力实现 + 单测) |

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
