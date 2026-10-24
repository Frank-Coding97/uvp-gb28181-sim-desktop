//! 压测历史 SQLite 持久化:跨运行对比的数据底座。
//!
//! 设计取舍(见 docs/20-architecture/data-model.md §2):
//! - **JSONL/报告文件仍是事实源**,本库是"紧凑的二次索引":每秒样本只存 9 个对比
//!   关键列,30 天前的运行自动清理 —— 比保留全量 JSONL(1 GiB 上限)便宜得多,
//!   却足够回答"这次比上次好还是差、拐点在不在同一水位"。
//! - **运行终结时一次性落库**(run 行 + 全部秒级样本),不在 tick 热路径上写库;
//!   读取失败的样本行跳过计数,不中断。
//! - **尽力而为**:落库失败只告警,绝不让历史写入影响压测结论本身。
//! - 迁移:`PRAGMA user_version` 轻量迁移(启动开库时执行),不引入迁移框架。

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// 历史保留窗口:与 uvp-reports 的文件保留策略对齐(30 天)。
const RETENTION_DAYS: i64 = 30;
/// schema 版本(user_version)。加表/加列时递增并写增量 DDL。
const SCHEMA_VERSION: i32 = 1;

static DB: OnceLock<Mutex<Connection>> = OnceLock::new();

/// 打开(或复用)历史库。`db_path` 由调用方给出(应用数据目录下),测试可传临时路径。
fn open(db_path: &Path) -> Result<&'static Mutex<Connection>, String> {
    if let Some(db) = DB.get() {
        return Ok(db);
    }
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
    // WAL:压测终结的批量写入与 UI 查询并发时互不阻塞。
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(3))
        .map_err(|e| e.to_string())?;
    migrate(&conn)?;
    // 并发 open(首压测)时后到者直接复用先建好的实例。
    Ok(DB.get_or_init(|| Mutex::new(conn)))
}

/// 轻量迁移:按 user_version 顺序执行增量 DDL。
fn migrate(conn: &Connection) -> Result<(), String> {
    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    if version < 1 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS run (
                run_id                   INTEGER PRIMARY KEY,
                outcome                  TEXT NOT NULL,
                started_at_ms            INTEGER NOT NULL,
                ended_at_ms              INTEGER NOT NULL,
                device_count             INTEGER NOT NULL,
                profile_id               TEXT,
                register_succeeded       INTEGER NOT NULL,
                device_success_rate      REAL NOT NULL,
                register_latency_p95_ms  INTEGER NOT NULL,
                devices_online_peak      INTEGER NOT NULL,
                fail_timeout             INTEGER NOT NULL,
                fail_rejected_auth       INTEGER NOT NULL,
                fail_rejected_overload   INTEGER NOT NULL,
                fail_local               INTEGER NOT NULL,
                ramp_throttled           INTEGER NOT NULL,
                results_trustworthy      INTEGER NOT NULL,
                summary_json             TEXT NOT NULL,
                report_path              TEXT
            );
            CREATE TABLE IF NOT EXISTS metric_sample (
                run_id                INTEGER NOT NULL REFERENCES run(run_id) ON DELETE CASCADE,
                ts_ms                 INTEGER NOT NULL,
                register_attempted    INTEGER NOT NULL,
                register_succeeded    INTEGER NOT NULL,
                register_latency_p95  INTEGER NOT NULL,
                heartbeat_failed      INTEGER NOT NULL,
                devices_online        INTEGER NOT NULL,
                device_success_rate   REAL NOT NULL,
                fail_rejected_overload INTEGER NOT NULL,
                active_streams        INTEGER NOT NULL,
                PRIMARY KEY (run_id, ts_ms)
            ) WITHOUT ROWID;",
        )
        .map_err(|e| e.to_string())?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 落库用的最终快照摘要(从 `MetricsSnapshot` 摘取对比关键项)。
#[derive(Debug, Clone)]
pub struct RunSummaryInput {
    pub outcome: &'static str,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    pub device_count: i64,
    pub profile_id: Option<String>,
    pub register_succeeded: i64,
    pub device_success_rate: f64,
    pub register_latency_p95_ms: i64,
    pub devices_online_peak: i64,
    pub fail_timeout: i64,
    pub fail_rejected_auth: i64,
    pub fail_rejected_overload: i64,
    pub fail_local: i64,
    pub ramp_throttled: i64,
    pub results_trustworthy: bool,
    pub summary_json: String,
    pub report_path: Option<String>,
}

/// 一条秒级样本(tick 序号 × 1s 作为运行相对时间)。
#[derive(Debug, Clone)]
pub struct SampleInput {
    pub ts_ms: i64,
    pub register_attempted: i64,
    pub register_succeeded: i64,
    pub register_latency_p95: i64,
    pub heartbeat_failed: i64,
    pub devices_online: i64,
    pub device_success_rate: f64,
    pub fail_rejected_overload: i64,
    pub active_streams: i64,
}

/// 记录一次运行(覆盖同 id 旧记录,支持重跑)。出错返回 Err,调用方只告警。
pub fn record_run(
    db_path: &Path,
    run_id: u64,
    run: &RunSummaryInput,
    samples: &[SampleInput],
) -> Result<(), String> {
    let db = open(db_path)?;
    let conn = db.lock().map_err(|e| e.to_string())?;
    record_run_on(&conn, run_id, run, samples)
}

/// 测试可注入独立连接的内核(避开全局单例,保证用例隔离)。
fn record_run_on(
    conn: &Connection,
    run_id: u64,
    run: &RunSummaryInput,
    samples: &[SampleInput],
) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT OR REPLACE INTO run (run_id, outcome, started_at_ms, ended_at_ms, device_count, profile_id,
             register_succeeded, device_success_rate, register_latency_p95_ms, devices_online_peak,
             fail_timeout, fail_rejected_auth, fail_rejected_overload, fail_local, ramp_throttled,
             results_trustworthy, summary_json, report_path)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
        rusqlite::params![
            run_id as i64,
            run.outcome,
            run.started_at_ms,
            run.ended_at_ms,
            run.device_count,
            run.profile_id,
            run.register_succeeded,
            run.device_success_rate,
            run.register_latency_p95_ms,
            run.devices_online_peak,
            run.fail_timeout,
            run.fail_rejected_auth,
            run.fail_rejected_overload,
            run.fail_local,
            run.ramp_throttled,
            run.results_trustworthy as i64,
            run.summary_json,
            run.report_path,
        ],
    )
    .map_err(|e| e.to_string())?;
    {
        let mut stmt = tx
            .prepare(
                "INSERT OR REPLACE INTO metric_sample (run_id, ts_ms, register_attempted,
                     register_succeeded, register_latency_p95, heartbeat_failed, devices_online,
                     device_success_rate, fail_rejected_overload, active_streams)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            )
            .map_err(|e| e.to_string())?;
        for s in samples {
            stmt.execute(rusqlite::params![
                run_id as i64,
                s.ts_ms,
                s.register_attempted,
                s.register_succeeded,
                s.register_latency_p95,
                s.heartbeat_failed,
                s.devices_online,
                s.device_success_rate,
                s.fail_rejected_overload,
                s.active_streams,
            ])
            .map_err(|e| e.to_string())?;
        }
    }
    // 保留窗口:清掉过期的 run(样本级联删除)。历史只是索引,清了不影响报告文件。
    tx.execute(
        "DELETE FROM run WHERE ended_at_ms < (strftime('%s','now') - ?1) * 1000",
        rusqlite::params![RETENTION_DAYS * 86_400],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// 历史运行列表项(IPC 返回;ts-rs 生成前端类型)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(ts_rs::TS)]
#[ts(export, export_to = "../../src/types/generated/")]
pub struct StressRunSummaryDto {
    #[ts(type = "number")]
    pub run_id: u64,
    pub outcome: String,
    #[ts(type = "number")]
    pub started_at_ms: i64,
    #[ts(type = "number")]
    pub ended_at_ms: i64,
    #[ts(type = "number")]
    pub device_count: i64,
    pub profile_id: Option<String>,
    #[ts(type = "number")]
    pub register_succeeded: u64,
    pub device_success_rate: f64,
    #[ts(type = "number")]
    pub register_latency_p95_ms: i64,
    #[ts(type = "number")]
    pub devices_online_peak: i64,
    #[ts(type = "number")]
    pub fail_timeout: u64,
    #[ts(type = "number")]
    pub fail_rejected_auth: u64,
    #[ts(type = "number")]
    pub fail_rejected_overload: u64,
    #[ts(type = "number")]
    pub fail_local: u64,
    #[ts(type = "number")]
    pub ramp_throttled: u64,
    pub results_trustworthy: bool,
    pub report_path: Option<String>,
}

/// 查询最近的历史运行(新→旧)。
pub fn list_runs(db_path: &Path, limit: u32) -> Result<Vec<StressRunSummaryDto>, String> {
    let db = open(db_path)?;
    let conn = db.lock().map_err(|e| e.to_string())?;
    list_runs_on(&conn, limit)
}

fn list_runs_on(conn: &Connection, limit: u32) -> Result<Vec<StressRunSummaryDto>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT run_id, outcome, started_at_ms, ended_at_ms, device_count, profile_id,
                    register_succeeded, device_success_rate, register_latency_p95_ms, devices_online_peak,
                    fail_timeout, fail_rejected_auth, fail_rejected_overload, fail_local, ramp_throttled,
                    results_trustworthy, report_path
             FROM run ORDER BY run_id DESC LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![limit], |row| {
            Ok(StressRunSummaryDto {
                run_id: row.get::<_, i64>(0)? as u64,
                outcome: row.get(1)?,
                started_at_ms: row.get(2)?,
                ended_at_ms: row.get(3)?,
                device_count: row.get(4)?,
                profile_id: row.get(5)?,
                register_succeeded: row.get::<_, i64>(6)? as u64,
                device_success_rate: row.get(7)?,
                register_latency_p95_ms: row.get(8)?,
                devices_online_peak: row.get(9)?,
                fail_timeout: row.get::<_, i64>(10)? as u64,
                fail_rejected_auth: row.get::<_, i64>(11)? as u64,
                fail_rejected_overload: row.get::<_, i64>(12)? as u64,
                fail_local: row.get::<_, i64>(13)? as u64,
                ramp_throttled: row.get::<_, i64>(14)? as u64,
                results_trustworthy: row.get::<_, i64>(15)? != 0,
                report_path: row.get(16)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// 历史运行秒级样本(IPC 返回,对比曲线用)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(ts_rs::TS)]
#[ts(export, export_to = "../../src/types/generated/")]
pub struct StressSampleDto {
    #[ts(type = "number")]
    pub ts_ms: i64,
    #[ts(type = "number")]
    pub register_attempted: u64,
    #[ts(type = "number")]
    pub register_succeeded: u64,
    #[ts(type = "number")]
    pub register_latency_p95: i64,
    #[ts(type = "number")]
    pub heartbeat_failed: u64,
    #[ts(type = "number")]
    pub devices_online: u64,
    pub device_success_rate: f64,
    #[ts(type = "number")]
    pub fail_rejected_overload: u64,
    #[ts(type = "number")]
    pub active_streams: u64,
}

/// 查询某次运行的完整秒级样本(旧→新)。
pub fn run_series(db_path: &Path, run_id: u64) -> Result<Vec<StressSampleDto>, String> {
    let db = open(db_path)?;
    let conn = db.lock().map_err(|e| e.to_string())?;
    run_series_on(&conn, run_id)
}

fn run_series_on(conn: &Connection, run_id: u64) -> Result<Vec<StressSampleDto>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT ts_ms, register_attempted, register_succeeded, register_latency_p95,
                    heartbeat_failed, devices_online, device_success_rate, fail_rejected_overload, active_streams
             FROM metric_sample WHERE run_id = ?1 ORDER BY ts_ms",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![run_id as i64], |row| {
            Ok(StressSampleDto {
                ts_ms: row.get(0)?,
                register_attempted: row.get::<_, i64>(1)? as u64,
                register_succeeded: row.get::<_, i64>(2)? as u64,
                register_latency_p95: row.get(3)?,
                heartbeat_failed: row.get::<_, i64>(4)? as u64,
                devices_online: row.get::<_, i64>(5)? as u64,
                device_success_rate: row.get(6)?,
                fail_rejected_overload: row.get::<_, i64>(7)? as u64,
                active_streams: row.get::<_, i64>(8)? as u64,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// 报告导出成功后回填路径(尽力而为)。
pub fn set_report_path(db_path: &Path, run_id: u64, report_path: &str) -> Result<(), String> {
    let db = open(db_path)?;
    let conn = db.lock().map_err(|e| e.to_string())?;
    set_report_path_on(&conn, run_id, report_path)
}

fn set_report_path_on(conn: &Connection, run_id: u64, report_path: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE run SET report_path = ?1 WHERE run_id = ?2",
        rusqlite::params![report_path, run_id as i64],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个用例独立连接 + 独立临时文件:生产入口 `record_run/list_runs` 走进程级
    /// 单例连接,直接复用会让用例互相串库。
    fn fresh_db(name: &str) -> Connection {
        let dir = std::env::temp_dir().join(format!("uvp-hist-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conn = Connection::open(dir.join("history.db")).unwrap();
        migrate(&conn).unwrap();
        conn
    }

    fn sample(ts: i64, succeeded: i64, p95: i64) -> SampleInput {
        SampleInput {
            ts_ms: ts,
            register_attempted: succeeded + 1,
            register_succeeded: succeeded,
            register_latency_p95: p95,
            heartbeat_failed: 0,
            devices_online: succeeded,
            device_success_rate: 0.99,
            fail_rejected_overload: 0,
            active_streams: 0,
        }
    }

    fn summary(outcome: &'static str) -> RunSummaryInput {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        RunSummaryInput {
            outcome,
            started_at_ms: now - 61_000,
            ended_at_ms: now,
            device_count: 10,
            profile_id: Some("lab-wvp".into()),
            register_succeeded: 10,
            device_success_rate: 1.0,
            register_latency_p95_ms: 120,
            devices_online_peak: 10,
            fail_timeout: 0,
            fail_rejected_auth: 0,
            fail_rejected_overload: 0,
            fail_local: 0,
            ramp_throttled: 0,
            results_trustworthy: true,
            summary_json: "{}".into(),
            report_path: Some("/tmp/report.json".into()),
        }
    }

    #[test]
    fn 落库与查询往返() {
        let conn = fresh_db("roundtrip");
        record_run_on(
            &conn,
            7,
            &summary("completed"),
            &[
                sample(0, 5, 80),
                sample(1_000, 9, 110),
                sample(2_000, 10, 120),
            ],
        )
        .unwrap();
        let runs = list_runs_on(&conn, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, 7);
        assert_eq!(runs[0].outcome, "completed");
        assert!(runs[0].results_trustworthy);
        assert_eq!(runs[0].profile_id.as_deref(), Some("lab-wvp"));
        let series = run_series_on(&conn, 7).unwrap();
        assert_eq!(series.len(), 3);
        assert_eq!(series[2].register_succeeded, 10);
        assert_eq!(series[0].ts_ms, 0);
    }

    #[test]
    fn 重跑同号覆盖旧记录() {
        let conn = fresh_db("upsert");
        record_run_on(&conn, 1, &summary("stopped"), &[sample(0, 3, 50)]).unwrap();
        record_run_on(&conn, 1, &summary("completed"), &[sample(0, 9, 90)]).unwrap();
        let runs = list_runs_on(&conn, 10).unwrap();
        assert_eq!(runs.len(), 1, "同 run_id 不得出现两行");
        assert_eq!(runs[0].outcome, "completed");
        assert_eq!(run_series_on(&conn, 1).unwrap().len(), 1);
    }

    #[test]
    fn 保留窗口清理过期运行() {
        let conn = fresh_db("retention");
        let mut old = summary("completed");
        // 40 天前结束:超出 30 天保留窗口。
        old.started_at_ms = 1;
        old.ended_at_ms = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64)
            - 40 * 86_400_000;
        record_run_on(&conn, 1, &old, &[sample(0, 1, 10)]).unwrap();
        record_run_on(&conn, 2, &summary("completed"), &[sample(0, 2, 20)]).unwrap();
        let runs = list_runs_on(&conn, 10).unwrap();
        assert_eq!(runs.len(), 1, "过期的 run 应被清理");
        assert_eq!(runs[0].run_id, 2);
        assert!(
            run_series_on(&conn, 1).unwrap().is_empty(),
            "样本应随 run 级联删除"
        );
    }

    #[test]
    fn schema_迁移幂等且回填报告路径() {
        let conn = fresh_db("idempotent");
        record_run_on(&conn, 3, &summary("completed"), &[sample(0, 1, 1)]).unwrap();
        // 同一连接再跑一次 migrate 不应报错或丢数据。
        migrate(&conn).unwrap();
        record_run_on(&conn, 4, &summary("completed"), &[sample(0, 1, 1)]).unwrap();
        let runs = list_runs_on(&conn, 10).unwrap();
        assert_eq!(runs.len(), 2);
        set_report_path_on(&conn, 4, "/tmp/r.json").unwrap();
        assert_eq!(
            list_runs_on(&conn, 10).unwrap()[0].report_path.as_deref(),
            Some("/tmp/r.json"),
            "run_id 4 最新,应回填成功"
        );
    }
}
