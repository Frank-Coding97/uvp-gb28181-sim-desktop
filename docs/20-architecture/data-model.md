# 数据模型

**状态:草案** · 定义当前的报告导出与核心内存结构,以及规划中的持久化 · 关联 FR-25、FR-26、FR-28。

---

## 1. 当前存储现实

**当前无数据库**。持久化只有一处:`export_report` 命令把一次压测的场景参数(原始 TOML)+ 设备数 + 结束时的 `MetricsSnapshot` 汇总成**单个 JSON 文件**,写到**系统临时目录**下的 `uvp-reports/report-<epoch 秒>.json`。

压测指标全程为**内存原子计数**(`stress-engine::Metrics`),不落时间序列,不建库;每秒仅通过 `metrics_tick` 事件投影给 UI(见 `20-architecture/api-contract.md#4`)。

## 1.1 规划中(未实现):SQLite 持久化

以下 SQLite 方案为设计草案,**尚未实现**。若后续引入,拟用 **SQLite**(单文件、零部署,符合 NFR-2),文件置于系统应用数据目录,如:
- macOS `~/Library/Application Support/com.uvp.gb28181.desktop/`
- Windows `%APPDATA%/com.uvp.gb28181.desktop/`
- Linux `~/.local/share/com.uvp.gb28181.desktop/`

不引入 PostgreSQL/MySQL(桌面安装负担)。压测高频指标先在内存聚合,按秒/批落库,避免逐包写盘。

---

## 2. SQLite 表结构(规划中,未实现)

```sql
-- 平台连接配置
CREATE TABLE platform (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    host        TEXT NOT NULL,
    port        INTEGER NOT NULL,
    domain      TEXT NOT NULL,
    created_at  INTEGER NOT NULL      -- epoch ms
);

-- 压测场景(保存的 TOML 原文 + 摘要)
CREATE TABLE scenario (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    toml        TEXT NOT NULL,        -- 原始 TOML,复跑用
    device_count INTEGER NOT NULL,
    media_mode  TEXT NOT NULL,        -- none|light|real
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

-- 压测运行记录
CREATE TABLE run (
    id            TEXT PRIMARY KEY,   -- run_id (uuid)
    scenario_name TEXT NOT NULL,
    state         TEXT NOT NULL,      -- starting|running|completed|failed|stopped
    started_at    INTEGER NOT NULL,
    ended_at      INTEGER,
    summary_json  TEXT                -- 结束时的汇总指标
);

-- 指标时间序列(每秒一行,供报告绘曲线)
CREATE TABLE metric_sample (
    run_id                TEXT NOT NULL REFERENCES run(id),
    ts_ms                 INTEGER NOT NULL,
    register_attempted    INTEGER NOT NULL,
    register_succeeded    INTEGER NOT NULL,
    register_latency_p95  INTEGER,
    heartbeat_success_rate REAL,
    invite_succeeded      INTEGER,
    active_streams        INTEGER,
    send_bitrate_kbps     INTEGER,
    PRIMARY KEY (run_id, ts_ms)
);

-- 失败明细归因(按 run + 类别聚合)
CREATE TABLE failure_stat (
    run_id    TEXT NOT NULL REFERENCES run(id),
    category  TEXT NOT NULL,          -- timeout|rejected|other
    count     INTEGER NOT NULL,
    PRIMARY KEY (run_id, category)
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

指标聚合:热路径用原子计数,定时器每秒 snapshot → 通过 `metrics_tick` 事件推送(NFR-4)。**当前不落库**(无 `metric_sample` 时间序列);仅 `export_report` 在导出时把最终快照写入一个 JSON 文件。

---

## 4. 失败归因分类

实际实现的 `common::FailureKind` 只有 3 个桶(见 `crates/common/src/observer.rs`),对应 `MetricsSnapshot` 的 `fail_timeout`/`fail_rejected`/`fail_other`:

| FailureKind | 含义 | 来源 |
|---|---|---|
| `Timeout` | 事务超时无响应 | SIP 事务层 |
| `Rejected` | 平台返回 4xx/5xx 拒绝 | SIP 响应 |
| `Other` | 网络/其它(含 socket/IO 错误) | 兜底 |

---

## 5. 演进

表结构变更走轻量迁移(启动时检测版本 + 增量 DDL)。当前为设计草案,M3 落库前定稿。
