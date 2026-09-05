# 测试策略

**状态:已评审** · 关联 NFR-7 · "协议/领域逻辑必配单测"。

## 分层

| 层 | 范围 | 工具 | 何时跑 |
|---|---|---|---|
| 单元测试 | crate 内纯逻辑:协议解析、状态机转移、编码、指标聚合 | `cargo test` | 每次提交 |
| 集成测试 | 跨 crate:注册流程(mock 平台)、点播链路、loopback RTP | `cargo test`(tests/) | 每次提交 |
| 端到端 | 对真实 WVP 联调:单设备上线、点播出图 | 手动 / 脚本 | 里程碑验收 |
| 压测自测 | 引擎自身规模/稳定性(本机 loopback 或测试平台) | CLI + 指标 | M3+ |

## 重点单测(必须)

- **sip-core**:消息解析/序列化往返;Digest 响应对拍已知向量;事务超时重传时序(模拟时钟)。
- **gb28181-protocol**:真实平台样例 XML 往返;ID 编解码与批量生成序号。
- **gb28181-simulator**:状态机转移(mock 传输注入 401/200/超时);心跳超时→重注册退避。
- **media-rtp**:RTP 头/序号递增;PS 封装字节结构;FileSource 循环边界。
- **scenario**:合法/非法场景解析;DeviceConfig 生成。
- **stress-engine**:成功率计算;爬坡速率(模拟时钟);指标并发聚合;
  失败归因分档(401/403 与 486/503 必须分开,处置动作完全不同)、失败水位只记第一次、
  失败样本上限、爬坡减速计数。
- **前端**:`ipcError` 的错误映射(原文永不丢失、未知错误不编造建议);
  `device.ts` 的启停守卫(IPC 已发出但未返回这段窗口不可重复触发);
  `versionedStorage` 的 legacy 迁移、future schema 防覆盖、损坏备份去重/上限、quota/security 分类与
  “备份失败不覆盖 primary”；`platform.ts` 的“任何 localStorage（含损坏备份）里永不出现可识别
  password”不变量；设备表单和主题的字段白名单与边界修复。

## Mock 与确定性

- 平台交互用 mock transport,注入固定响应,保证测试确定、不依赖网络。
- 时间相关(重传、心跳、爬坡)用可注入时钟,避免 `sleep` 真等待。

## CI 质量门(已接入)

`.github/workflows/ci.yml` 有六个并行门,全绿方可合入 `develop`:

| Job | 内容 | 拦的是什么 |
|---|---|---|
| `version-check` | `scripts/check-version.sh` + 工具链声明一致 | 版本号漂移(曾出现安装包标 0.1.2、界面显示 0.1.0、上报固件也是 0.1.0) |
| `supply-chain` | `cargo deny --all-features check` + `npm audit --audit-level=high` | 依赖漏洞 / 许可证 / 重复版本 / 非 crates.io 来源 |
| `rust-check` | `cargo fmt --check` + `cargo clippy --workspace --all-targets -D warnings` + `cargo test --workspace` | — |
| `frontend-check` | `eslint .`(含 `vuejs-accessibility`)+ `vue-tsc --noEmit` + `vitest run` + `vite build` | 类型系统看不见的可访问性问题 |
| `secret-scan` | gitleaks(显式指定 `.gitleaks.toml`) | 凭据被提交 |
| `build-desktop` → `release` | 三平台安装包;tag 时发布并附 `SHA256SUMS.txt` | 产物不可验证 |

工具链版本由 `rust-toolchain.toml` 固定(`stable` 每六周变一次,构建不可复现),
workflow 里的 `RUST_TOOLCHAIN` 与它一致由 `version-check` 保证。

### 关于依赖豁免

`deny.toml` 的 `advisories.ignore` 里每条豁免必须写清三件事:公告编号、
为什么本项目不受影响、以及下次复查的触发条件。没有这三样的豁免等于把门禁关掉。

### 前端测试的环境约定

`vitest.config.ts` 用 `environment: "node"`(现有测试都是纯逻辑,不挂载组件)。
需要 `localStorage` 的测试自带 `MemoryStorage` 并 `vi.stubGlobal`
(见 `platform.test.ts` / `device.test.ts`);要测组件时在文件头加
`// @vitest-environment jsdom`(jsdom 与 `@vue/test-utils` 已在 devDeps)。

## 验收测试映射

里程碑验收标准见 `roadmap.md`;用例场景见 `00-product/use-cases.md`。E2E 按 UC-1(单设备)/ UC-2(压测)执行。
