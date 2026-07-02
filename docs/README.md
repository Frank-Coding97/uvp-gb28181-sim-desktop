# 文档中心

本目录是项目的**唯一事实来源(Single Source of Truth)**。所有功能、协议、接口在实现前先在此定义。

---

## 核心约定:文档先行(Doc-First）

> **文档早于代码。代码是文档的完美实现。**

1. **任何功能开工前**,先在对应文档写清"是什么 / 为什么 / 怎么表现 / 对外接口",经确认后再编码。
2. **代码若与文档不符**,以文档为准 —— 要么改代码对齐文档,要么先改文档再改代码,不允许静默偏离。
3. **接口契约变更**(crate 公开 API、UI↔引擎协议、数据模型)必须先改文档,并知会依赖方(尤其跨 worktree 的另两条线)。
4. 每篇规格顶部标注**状态**:`草案` / `已评审` / `已实现` / `已过时`。代码 PR 应引用其实现的规格编号(如 `实现 FR-3 / crate-sip-core §4`)。

---

## 目录导航

| 层 | 目录 | 内容 |
|---|---|---|
| 产品 | [`00-product/`](00-product/) | 愿景、需求清单(FR/NFR)、用例 |
| 功能 | [`10-functional/`](10-functional/) | 设备模拟、压力测试、GB28181 覆盖矩阵 |
| 架构 | [`20-architecture/`](20-architecture/) | 分层总览、UI↔引擎 API 契约、数据模型 |
| 模块 | [`30-crates/`](30-crates/) | 每个 Rust crate 的详细规格 |
| 协议 | [`40-protocol/`](40-protocol/) | SIP、MANSCDP XML、PS/RTP 媒体 |
| 流程 | [`90-process/`](90-process/) | 路线图、编码规范、测试策略 |

---

## 编号与命名规范

- **功能需求** `FR-<序号>`,**非功能需求** `NFR-<序号>`(见 `00-product/requirements.md`)。
- **用例** `UC-<序号>`。
- crate 规格用统一模板(职责 / 公开 API / 数据类型 / 错误 / 依赖 / 里程碑 / 测试)。
- 章节引用格式:`<文档>#<锚点>` 或 `crate-<名> §<节号>`。

---

## 阅读顺序建议

新成员:`00-product/vision.md` → `requirements.md` → `20-architecture/overview.md` → 自己负责的 `30-crates/*.md`。

跨线协作者:重点读 `20-architecture/api-contract.md` 与 `data-model.md`(这是三条 worktree 的共享边界)。
