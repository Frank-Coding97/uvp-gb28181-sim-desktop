# 数据模型

**状态:已实现(SQLite 历史索引 v1)** · 报告导出、内存结构与 SQLite 历史索引 · 关联 FR-25、FR-26、FR-28。

---

## 1. 当前存储现实

**当前无数据库**。压测时序、JSON 报告与诊断 ZIP 都写入应用数据目录下的
`uvp-reports/`；只有无法解析应用数据目录时才回退系统临时目录。它们是
**应用托管产物**，IPC 当前返回该路径供用户定位，但不表示永久归档。

默认保留策略为：最长 30 天、最多 100 个文件、总计最多 1 GiB；本模块遗留的
`.uvp-*.tmp` staging 超过 24 小时会优先清理。清理只识别本应用的严格文件名，
跳过 symlink、目录和未知文件；所有活跃 staging、发布后尚未完成状态/preserve 交接的
final 及本次刚导出的产物都由 RAII 注册令牌保护。清理逐项告警并降级，不阻止压测
启动或完成；`CleanupHealth.failed_removals`、`retained_temp_files/bytes` 和
`metadata_failures` 分别暴露删除失败、仍保留的 temp 及元数据读取失败，managed 文件
即使修改时间读取失败，也继续计入可测的保留文件数和字节数。

压测指标为**内存原子计数**（`stress-engine::Metrics`，含延迟直方图），每秒通过
`metrics_tick` 事件投影给 UI，同时由单一 writer 追加到
`series-run<run_id>-<nanos>-<counter>.jsonl.inprogress`。运行结束写入带
`sample_count/outcome/complete` 的 `run_end` marker，同步后原子发布为 `.jsonl`。
JSONL 内部的 sample 使用 `{kind,seq,ts_ms,metrics}` envelope 做顺序和完整性校验，
其中 `metrics` 必须是 object；但 `schema_version=1` 报告的 `series` 字段固定输出
**裸 metrics object 数组**，不得把内部 envelope 暴露进报告。`export_report` 将脱敏
场景、最终快照、完整性 metadata 和该兼容数组写入唯一命名的
`report-<nanos>-<counter>.json`。文件 rename 成功即视为已发布；若随后父目录同步失败，
真实 final 路径仍用于 registry/状态交接，同时记录 durability warning。SQLite 历史索引
（跨运行对比）已落地,见 §1.1/§2。

## 1.1 已实现(v1):SQLite 历史索引

2026-10 落地**压测历史索引**(源码 `apps/desktop/src-tauri/src/reports_db.rs`,rusqlite
`bundled` 自带 sqlite3,零系统依赖)。定位是**紧凑二次索引**而非事实源:JSONL/报告文件
仍是一切结论的来源;SQLite 回答的是"这次比上次好还是差、过载拐点在不在同一水位"这类
跨运行问题。库文件置于应用数据目录(与 `uvp-reports/` 同级,`uvp-stress-history.db`),
不会被产物清理碰到(清理只识别本应用的严格文件名)。

- **写入时机**:压测终结时一次性落库(run 行 + 全部秒级样本,tick 热路径不写库),
  尽力而为 —— 失败只告警,绝不影响压测结论;`export_report` 成功后回填报告路径。
- **查询**:`list_stress_runs` / `get_stress_run_series` IPC(见 api-contract.md),
  前端压测工作台「历史运行」面板消费,支持任选两条做关键指标对比。
- **保留**:与文件产物对齐的 30 天窗口,过期 run 连同样本级联删除(报告文件不受影响)。
- **迁移**:`PRAGMA user_version` 轻量迁移,开库时执行。

`platform` / `scenario` 两张表仍在草案中(平台档案当前由前端 localStorage + OS 钥匙串
管理,迁库前需先定义迁移路径)。

---

## 2. SQLite 表结构(v1 已实现 + 草案)

```sql
-- ✅ v1 已实现:压测运行汇总(一 run 一行,重跑同号覆盖)
CREATE TABLE run (
    run_id                   INTEGER PRIMARY KEY,  -- supervisor 分配的单调 run_id
    outcome                  TEXT NOT NULL,        -- completed|stopped|failed
    started_at_ms            INTEGER NOT NULL,
    ended_at_ms              INTEGER NOT NULL,
    device_count             INTEGER NOT NULL,
    profile_id               TEXT,                 -- 目标平台档案 id
    register_succeeded       INTEGER NOT NULL,     -- ↓ 对比关键项(冗余列,免 JSON 反解)
    device_success_rate      REAL NOT NULL,
    register_latency_p95_ms  INTEGER NOT NULL,
    devices_online_peak      INTEGER NOT NULL,
    fail_timeout             INTEGER NOT NULL,
    fail_rejected_auth       INTEGER NOT NULL,
    fail_rejected_overload   INTEGER NOT NULL,
    fail_local               INTEGER NOT NULL,
    ramp_throttled           INTEGER NOT NULL,
    results_trustworthy      INTEGER NOT NULL,
    summary_json             TEXT NOT NULL,        -- 完整 MetricsSnapshot(未来详情页)
    report_path              TEXT                  -- export_report 成功后回填
);

-- ✅ v1 已实现:秒级样本(tick 序号×1s 为运行相对时间;WITHOUT ROWID 省一半空间)
CREATE TABLE metric_sample (
    run_id                 INTEGER NOT NULL REFERENCES run(run_id) ON DELETE CASCADE,
    ts_ms                  INTEGER NOT NULL,
    register_attempted     INTEGER NOT NULL,
    register_succeeded     INTEGER NOT NULL,
    register_latency_p95   INTEGER NOT NULL,
    heartbeat_failed       INTEGER NOT NULL,
    devices_online         INTEGER NOT NULL,
    device_success_rate    REAL NOT NULL,
    fail_rejected_overload INTEGER NOT NULL,
    active_streams         INTEGER NOT NULL,
    PRIMARY KEY (run_id, ts_ms)
) WITHOUT ROWID;

-- ⬜ 以下仍为草案(未实现):平台档案迁库与场景库
CREATE TABLE platform (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    host        TEXT NOT NULL,
    port        INTEGER NOT NULL,
    domain      TEXT NOT NULL,
    created_at  INTEGER NOT NULL      -- epoch ms
);

CREATE TABLE scenario (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    toml        TEXT NOT NULL,        -- 原始 TOML,复跑用
    device_count INTEGER NOT NULL,
    media_mode  TEXT NOT NULL,        -- none|light|real
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
```

---

## 3. 核心内存结构(Rust)

不落库、仅运行时存在的高频结构,定义在各 crate:

| 结构 | crate | 说明 |
|---|---|---|
| `LinearScenario`(实现 `Scenario` trait) | scenario | TOML 解析后的强类型场景 |
| `DeviceConfig` | gb28181-simulator | 单设备静态配置 |
| `DeviceSimulator` | gb28181-simulator | 单设备运行时状态机 |
| `Metrics` | stress-engine | 原子计数聚合,每秒投影成 `MetricsSnapshot` 推 UI |

指标聚合：热路径用原子计数，定时器每秒 snapshot → 通过 `metrics_tick` 事件推送（NFR-4）。
当前仍**不落数据库**（无 SQLite `metric_sample` 表），但同一快照会安全追加到应用托管
JSONL；报告导出时解析该 JSONL，并显式报告 `series_complete`、样本数、运行结果和损坏行数。

---

## 4. 失败归因分类

`common::FailureKind`(见 `crates/common/src/observer.rs`)四个桶,对应 `MetricsSnapshot`
的 `fail_timeout` / `fail_rejected` / `fail_local` / `fail_other`:

| FailureKind | 含义 | 来源 |
|---|---|---|
| `Timeout` | 事务超时无响应 | `Error::Timeout`(SIP 事务层) |
| `Rejected { status }` | 平台以非 2xx 最终响应拒绝,**携带状态码** | `Error::Rejected` |
| `Local` | **本机**发送失败(ENOBUFS / socket 错误 / 队列满) | `Error::Io` |
| `Other` | 网络/其它 | 兜底 |

### 为什么归因按错误变体而不是错误文案

早期实现是 `msg.contains("超时")` / `msg.contains("被拒")`。任何一次文案措辞调整
都会静默地把失败归到 `Other`,而压测结论的可信度恰恰建立在这个分类上。
现在 `common::Error` 有独立的 `Timeout` 与 `Rejected { status, reason }` 变体,
归因是一次 `match`。

### 为什么 `Local` 必须单独一类

本机瓶颈导致的失败绝不能算成"平台承载不住" —— 那会让压测结论直接失真。
`MetricsSnapshot::local_pressure` 汇总本地压力(队列丢弃 + 发送失败 + 解析失败 +
本地归因失败),`results_trustworthy` 在它非零时置 false;压测爬坡还会据此**闭环减速**
(见 `stress-engine::scheduler` 的 `MAX_GAP_MULTIPLIER`),而不是等事后才发现这轮白跑。

### 平台拒绝按处置动作再分档

状态码必须带上,因为该做什么完全不同。`common::RejectBucket`:

| 分档 | 状态码 | 处置动作 | 快照字段 |
|---|---|---|---|
| `Auth` | 401 / 403 / 407 | 凭据或权限问题,**加并发不会变好**;核对用户名/密码/域/设备是否已在平台注册 | `fail_rejected_auth` |
| `Overload` | 480 / 486 / 500 / 503 / 504 | 平台忙或过载 —— 这才是承载力信号,降并发找拐点 | `fail_rejected_overload` |
| `Other` | 其它 4xx/5xx | 需看原因短语,常见 404 = 编号不被平台接受 | `fail_rejected_other` |

### 报告里另外三个用于定位的字段

| 字段 | 回答的问题 |
|---|---|
| `first_failure_online` | **失败水位**:撑到多少台在线时出现首次失败(哨兵 `u64::MAX` → `None`;不能用 0,"第一台还没上线就失败"是真实且最值得注意的情形) |
| `failure_samples` | 挑哪台复现(最多 20 条:device_id + 归因 + 状态码 + 当时在线数) |
| `ramp_throttled` | 爬坡因本机背压减速的次数;非 0 即"本机跟不上设定速率" |

---

## 5. 演进

表结构变更走轻量迁移(`PRAGMA user_version`,开库时执行增量 DDL)。v1 已落 run /
metric_sample 两表;platform / scenario 迁库(平台档案从 localStorage + 钥匙串迁入)
与逐秒曲线叠加视图为后续项。
