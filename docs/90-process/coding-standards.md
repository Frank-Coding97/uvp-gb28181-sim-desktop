# 编码规范

**状态:已评审** · 关联 NFR-7(代码整洁、中文注释、协议逻辑必配单测)。

## 通用

- **文档先行**:见 `docs/README.md#核心约定`。代码 PR 引用其实现的规格编号。
- **中文注释**:模块级 `//!` 与关键函数 `///` 用中文说明"做什么、为什么",而非复述代码。
- **提交信息**:`<type>(<scope>): <中文描述>`,type ∈ `feat/fix/refactor/docs/test/chore/perf/ci`,scope 用 crate 名或 `desktop`。

## Rust

- 版本:stable,edition 2021;统一走 workspace 依赖(`xxx.workspace = true`)。
- **提交前三件套必过**:`cargo fmt`(格式)、`cargo clippy`(0 警告)、`cargo test`(全绿)。
- 错误统一用 `common::Error`/`Result`,不用 `unwrap()`/`expect()` 于可恢复路径(测试与启动期除外)。
- 公开 API 加 `///` 文档注释;模块加 `//!`。
- 依赖方向遵守 `20-architecture/overview.md`,禁止环依赖。
- 并发:优先 Tokio 异步任务;热路径避免锁(用原子/无锁结构),见 NFR-4。

## 前端(Vue/TS)

- TypeScript `strict`;组件用 `<script setup lang="ts">`。
- 与引擎交互只走 `20-architecture/api-contract.md` 定义的命令/事件,类型定义与契约一致。
- UI 不含协议逻辑(产品第一原则)。
- 提交前 `npm run build`(含 vue-tsc 类型检查)通过。

## 文档

- 新功能先更新对应 `10-functional/` 与 `30-crates/` 规格,再写代码。
- 契约类文档(`api-contract.md`、`data-model.md`、crate 公开 API)变更须知会相关 worktree。
