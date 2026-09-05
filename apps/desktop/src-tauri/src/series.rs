//! 压测 JSONL 时序的单 writer、完整收尾与完整性读取。

use serde::Serialize;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunOutcome {
    Completed,
    Stopped,
    Failed,
}

impl RunOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug)]
pub struct FinalizedSeries {
    pub path: PathBuf,
    pub sample_count: u64,
    pub degraded: bool,
    pub outcome: RunOutcome,
}

pub struct SeriesWriter {
    inprogress_path: PathBuf,
    final_path: PathBuf,
    file: Mutex<Option<File>>,
    active: crate::artifacts::ActiveArtifactGuard,
    seq: AtomicU64,
    sample_count: AtomicU64,
    degraded: AtomicBool,
}

impl SeriesWriter {
    pub fn new(dir: &Path, run_id: u64) -> io::Result<Self> {
        crate::artifacts::create_secure_dir(dir)?;
        let (nanos, counter) = crate::artifacts::unique_timestamp_counter();
        let stem = format!("series-run{run_id}-{nanos}-{counter}.jsonl");
        let final_path = dir.join(&stem);
        let inprogress_path = dir.join(format!("{stem}.inprogress"));
        let (file, active) = crate::artifacts::create_active_series(&inprogress_path)?;
        Ok(Self {
            inprogress_path,
            final_path,
            file: Mutex::new(Some(file)),
            active,
            seq: AtomicU64::new(0),
            sample_count: AtomicU64::new(0),
            degraded: AtomicBool::new(false),
        })
    }

    pub fn inprogress_path(&self) -> &Path {
        &self.inprogress_path
    }

    pub fn append<T: Serialize>(&self, metrics: &T) -> io::Result<()> {
        let mut guard = self
            .file
            .lock()
            .map_err(|_| io::Error::other("series writer mutex poisoned"))?;
        let file = guard
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "series writer 已收尾"))?;
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let mut line = serde_json::to_vec(&serde_json::json!({
            "kind": "sample",
            "seq": seq,
            "ts_ms": now_millis(),
            "metrics": metrics,
        }))
        .map_err(|error| {
            self.degraded.store(true, Ordering::Relaxed);
            io::Error::new(io::ErrorKind::InvalidData, error)
        })?;
        line.push(b'\n');
        if let Err(error) = file
            .write_all(&line)
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_data())
        {
            self.degraded.store(true, Ordering::Relaxed);
            return Err(error);
        }
        self.sample_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub fn finalize(&self, outcome: RunOutcome) -> io::Result<FinalizedSeries> {
        let mut guard = self
            .file
            .lock()
            .map_err(|_| io::Error::other("series writer mutex poisoned"))?;
        let file = guard
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "series writer 已收尾"))?;
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let sample_count = self.sample_count.load(Ordering::Relaxed);
        let degraded = self.degraded.load(Ordering::Relaxed);
        let mut marker = serde_json::to_vec(&serde_json::json!({
            "kind": "run_end",
            "seq": seq,
            "sample_count": sample_count,
            "outcome": outcome,
            "complete": true,
            "degraded": degraded,
            "ts_ms": now_millis(),
        }))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        marker.push(b'\n');
        if let Err(error) = file
            .write_all(&marker)
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_all())
        {
            self.degraded.store(true, Ordering::Relaxed);
            return Err(error);
        }
        let file = guard.take().expect("上方已确认 file 存在");
        drop(file);
        drop(guard);
        let publish_health = self.active.publish_to(&self.final_path)?;
        if let Some(warning) = publish_health.durability_warning {
            tracing::warn!(warning, path = %self.final_path.display(), "时序已发布但持久性降级");
        }
        Ok(FinalizedSeries {
            path: self.final_path.clone(),
            sample_count,
            degraded,
            outcome,
        })
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SeriesRead {
    pub samples: Vec<serde_json::Value>,
    pub complete: bool,
    pub sample_count: u64,
    pub outcome: Option<String>,
    pub degraded: bool,
    pub torn_tail: bool,
    pub corrupt_lines: usize,
    pub source_file: Option<String>,
}

pub fn read_series(path: &Path) -> SeriesRead {
    let fallback = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".inprogress"))
        .map(|name| path.with_file_name(name));
    let (bytes, source_path) = match std::fs::read(path) {
        Ok(bytes) => (bytes, path.to_path_buf()),
        Err(first_error) => match fallback.as_deref().map(std::fs::read) {
            Some(Ok(bytes)) => (bytes, fallback.expect("上方已确认 fallback 存在")),
            _ => {
                tracing::warn!(%first_error, path = %path.display(), "读取压测时序失败");
                return SeriesRead::default();
            }
        },
    };
    let ends_with_newline = bytes.ends_with(b"\n");
    let parts = bytes.split(|byte| *byte == b'\n').collect::<Vec<_>>();
    let last_nonempty = parts.iter().rposition(|part| !part.is_empty());
    let mut result = SeriesRead {
        source_file: Some(source_path.to_string_lossy().into_owned()),
        ..SeriesRead::default()
    };
    let mut end_marker: Option<(u64, String, bool)> = None;
    let mut marker_count = 0usize;
    let mut legacy_records = 0usize;
    let mut expected_seq = 1u64;

    for (index, raw) in parts.iter().enumerate() {
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice::<serde_json::Value>(raw) {
            Ok(value) => match value.get("kind").and_then(serde_json::Value::as_str) {
                Some("sample") => {
                    let valid = end_marker.is_none()
                        && value.get("seq").and_then(serde_json::Value::as_u64)
                            == Some(expected_seq)
                        && value
                            .get("ts_ms")
                            .and_then(serde_json::Value::as_u64)
                            .is_some()
                        && value
                            .get("metrics")
                            .is_some_and(serde_json::Value::is_object);
                    if valid {
                        result
                            .samples
                            .push(value.get("metrics").expect("上方已校验 metrics").clone());
                        expected_seq += 1;
                    } else {
                        result.corrupt_lines += 1;
                        tracing::warn!(
                            path = %source_path.display(),
                            line = index + 1,
                            "压测时序 sample 结构或顺序损坏"
                        );
                    }
                }
                Some("run_end") => {
                    marker_count += 1;
                    let count = value
                        .get("sample_count")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(u64::MAX);
                    let outcome = value
                        .get("outcome")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown")
                        .to_string();
                    let marker_valid = marker_count == 1
                        && legacy_records == 0
                        && value.get("seq").and_then(serde_json::Value::as_u64)
                            == Some(expected_seq)
                        && matches!(outcome.as_str(), "completed" | "stopped" | "failed")
                        && value
                            .get("complete")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                    if !marker_valid {
                        result.corrupt_lines += 1;
                        tracing::warn!(
                            path = %source_path.display(),
                            line = index + 1,
                            "压测时序 run_end 结构、顺序或数量损坏"
                        );
                    }
                    result.degraded = value
                        .get("degraded")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    end_marker = Some((count, outcome, marker_valid));
                }
                None if end_marker.is_none() && value.is_object() => {
                    // 兼容旧版无 kind/seq 的裸 metrics JSONL；因没有 run_end，它永远不会标 complete。
                    legacy_records += 1;
                    result.samples.push(value);
                }
                _ => {
                    result.corrupt_lines += 1;
                    tracing::warn!(
                        path = %source_path.display(),
                        line = index + 1,
                        "压测时序存在未知或越过 run_end 的记录"
                    );
                }
            },
            Err(error) => {
                let is_torn_tail = !ends_with_newline && Some(index) == last_nonempty;
                if is_torn_tail {
                    result.torn_tail = true;
                } else {
                    result.corrupt_lines += 1;
                    tracing::warn!(
                        %error,
                        path = %source_path.display(),
                        line = index + 1,
                        "压测时序存在中部损坏行"
                    );
                }
            }
        }
    }

    result.sample_count = result.samples.len() as u64;
    if let Some((declared_count, outcome, marker_valid)) = end_marker {
        result.outcome = Some(outcome);
        result.complete = marker_valid
            && marker_count == 1
            && declared_count == result.sample_count
            && result.corrupt_lines == 0
            && !result.torn_tail;
    }
    result
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "uvp-series-{label}-{}",
            crate::artifacts::unique_suffix()
        ))
    }

    #[test]
    fn finalized_series_has_end_marker_and_integrity() {
        let dir = test_dir("complete");
        let writer = SeriesWriter::new(&dir, 7).expect("创建 writer");
        writer
            .append(&serde_json::json!({"value": 1}))
            .expect("写样本");
        writer
            .append(&serde_json::json!({"value": 2}))
            .expect("写样本");
        let finalized = writer.finalize(RunOutcome::Completed).expect("收尾");
        assert!(!writer.inprogress_path().exists());
        let read = read_series(&finalized.path);
        assert!(read.complete);
        assert_eq!(read.sample_count, 2);
        assert_eq!(read.outcome.as_deref(), Some("completed"));
        assert_eq!(
            read.samples,
            vec![
                serde_json::json!({"value": 1}),
                serde_json::json!({"value": 2})
            ],
            "schema_version=1 报告必须继续得到裸 metrics 数组"
        );
        assert!(read
            .samples
            .iter()
            .all(|sample| sample.get("kind").is_none()));
        drop(writer);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn writer_drop自动释放inprogress和final活跃注册() {
        let dir = test_dir("raii");
        let policy = crate::artifacts::RetentionPolicy {
            max_age_secs: 0,
            max_files: 0,
            max_total_bytes: 0,
            stale_temp_secs: 0,
        };
        let writer = SeriesWriter::new(&dir, 8).expect("创建 writer");
        let inprogress = writer.inprogress_path().to_path_buf();
        crate::artifacts::cleanup(&dir, policy, &std::collections::HashSet::new());
        assert!(inprogress.exists(), "writer 活跃期间 inprogress 不得删除");
        drop(writer);
        crate::artifacts::cleanup(&dir, policy, &std::collections::HashSet::new());
        assert!(!inprogress.exists(), "writer Drop 后必须自动解除注册");

        let writer = SeriesWriter::new(&dir, 9).expect("创建 writer");
        writer
            .append(&serde_json::json!({"value": 1}))
            .expect("写样本");
        let final_path = writer.finalize(RunOutcome::Completed).expect("发布").path;
        crate::artifacts::cleanup(&dir, policy, &std::collections::HashSet::new());
        assert!(final_path.exists(), "发布后 guard 必须原子转移到 final");
        drop(writer);
        crate::artifacts::cleanup(&dir, policy, &std::collections::HashSet::new());
        assert!(!final_path.exists(), "final guard 随 writer Drop 自动解除");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reader要求sample_metrics为object且报告series不暴露envelope() {
        let dir = test_dir("metrics-object");
        crate::artifacts::create_secure_dir(&dir).expect("建目录");
        let path = dir.join("series-run4-1-1.jsonl");
        std::fs::write(
            &path,
            b"{\"kind\":\"sample\",\"seq\":1,\"ts_ms\":1,\"metrics\":[1]}\n{\"kind\":\"run_end\",\"seq\":2,\"sample_count\":1,\"outcome\":\"completed\",\"complete\":true,\"degraded\":false}\n",
        )
        .expect("写文件");
        let read = read_series(&path);
        assert!(!read.complete);
        assert!(read.corrupt_lines >= 1);
        assert!(read.samples.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reader_rejects_parseable_structural_corruption_and_falls_back_to_final() {
        let dir = test_dir("structure");
        crate::artifacts::create_secure_dir(&dir).expect("建目录");
        let final_path = dir.join("series-run3-1-1.jsonl");
        std::fs::write(
            &final_path,
            b"{}\n{\"kind\":\"run_end\",\"seq\":1,\"sample_count\":1,\"outcome\":\"completed\",\"complete\":true}\n",
        )
        .expect("写损坏文件");
        let read = read_series(&final_path);
        assert!(!read.complete, "可解析但缺字段的记录不能标完整");
        assert!(read.corrupt_lines > 0);

        std::fs::write(
            &final_path,
            b"{\"kind\":\"sample\",\"seq\":1,\"ts_ms\":1,\"metrics\":{}}\n{\"kind\":\"run_end\",\"seq\":2,\"sample_count\":1,\"outcome\":\"completed\",\"complete\":true,\"degraded\":false}\n",
        )
        .expect("写完整 final");
        let stale_inprogress = dir.join("series-run3-1-1.jsonl.inprogress");
        let read = read_series(&stale_inprogress);
        assert!(read.complete, "发布交接窗口应回退读取 final");
        assert_eq!(
            read.source_file.as_deref(),
            Some(final_path.to_string_lossy().as_ref())
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reader_tolerates_only_torn_tail_and_flags_middle_corruption() {
        let dir = test_dir("corruption");
        crate::artifacts::create_secure_dir(&dir).expect("建目录");
        let middle = dir.join("series-run1-1-1.jsonl");
        std::fs::write(
            &middle,
            b"{\"kind\":\"sample\",\"seq\":1,\"ts_ms\":1,\"metrics\":{}}\nnot-json\n{\"kind\":\"run_end\",\"seq\":2,\"sample_count\":1,\"outcome\":\"failed\",\"complete\":true,\"degraded\":false}\n",
        )
        .expect("写文件");
        let read = read_series(&middle);
        assert!(!read.complete);
        assert_eq!(read.corrupt_lines, 1);

        let torn = dir.join("series-run2-1-1.jsonl.inprogress");
        std::fs::write(
            &torn,
            b"{\"kind\":\"sample\",\"seq\":1,\"ts_ms\":1,\"metrics\":{}}\n{\"kind\":\"sample\"",
        )
        .expect("写文件");
        let read = read_series(&torn);
        assert!(read.torn_tail);
        assert_eq!(read.corrupt_lines, 0);
        assert_eq!(read.samples.len(), 1);
        assert!(!read.complete);
        let _ = std::fs::remove_dir_all(dir);
    }
}
