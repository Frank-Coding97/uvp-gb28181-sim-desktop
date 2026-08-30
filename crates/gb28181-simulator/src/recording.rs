//! 本地录像文件的版本化索引。

use common::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

const INDEX_VERSION: u32 = 1;
const INDEX_FILE: &str = "index.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordingKind {
    Time,
    Alarm,
    Manual,
}

impl RecordingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Time => "time",
            Self::Alarm => "alarm",
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingSource {
    Local,
    Platform,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingEntry {
    pub id: String,
    pub channel_id: String,
    pub start_time_ms: u64,
    pub end_time_ms: u64,
    pub start_time: String,
    pub end_time: String,
    pub source: RecordingSource,
    pub kind: RecordingKind,
    pub path: PathBuf,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordingQuery {
    pub start_time_ms: Option<u64>,
    pub end_time_ms: Option<u64>,
    pub channel_id: Option<String>,
    pub kind: Option<RecordingKind>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RecordingIndexV1 {
    version: u32,
    recordings: Vec<RecordingEntry>,
}

/// 线程安全的录像索引；只保存已完成且仍存在的文件。
#[derive(Debug)]
pub struct RecordingStore {
    root: PathBuf,
    entries: RwLock<Vec<RecordingEntry>>,
}

impl RecordingStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let index_path = root.join(INDEX_FILE);
        let mut entries = if index_path.exists() {
            let mut bytes = Vec::new();
            File::open(&index_path)?.read_to_end(&mut bytes)?;
            let index: RecordingIndexV1 = serde_json::from_slice(&bytes).map_err(|error| {
                Error::Config(format!(
                    "录像索引 {} 解析失败: {error}",
                    index_path.display()
                ))
            })?;
            if index.version != INDEX_VERSION {
                return Err(Error::Config(format!(
                    "录像索引版本不支持: {}",
                    index.version
                )));
            }
            index.recordings
        } else {
            Vec::new()
        };
        entries.retain(|entry| entry.path.is_file());
        sort_entries(&mut entries);
        Ok(Self {
            root,
            entries: RwLock::new(entries),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn list(&self) -> Vec<RecordingEntry> {
        self.entries
            .read()
            .map(|entries| entries.clone())
            .unwrap_or_default()
    }

    pub fn query(&self, query: &RecordingQuery) -> Vec<RecordingEntry> {
        self.list()
            .into_iter()
            .filter(|entry| {
                entry.path.is_file()
                    && query
                        .start_time_ms
                        .map(|start| entry.end_time_ms >= start)
                        .unwrap_or(true)
                    && query
                        .end_time_ms
                        .map(|end| entry.start_time_ms <= end)
                        .unwrap_or(true)
                    && query
                        .channel_id
                        .as_deref()
                        .map(|channel| entry.channel_id == channel)
                        .unwrap_or(true)
                    && query.kind.map(|kind| entry.kind == kind).unwrap_or(true)
            })
            .collect()
    }

    pub fn add(&self, entry: RecordingEntry) -> Result<()> {
        if !entry.path.is_file() {
            return Err(Error::Config(format!(
                "录像文件不存在: {}",
                entry.path.display()
            )));
        }
        let mut entries = self
            .entries
            .write()
            .map_err(|_| Error::Config("录像索引锁异常".into()))?;
        if entries.iter().any(|current| current.id == entry.id) {
            return Err(Error::Config(format!("录像 ID 已存在: {}", entry.id)));
        }
        entries.push(entry);
        sort_entries(&mut entries);
        self.persist(&entries)
    }

    pub fn delete(&self, id: &str) -> Result<Option<RecordingEntry>> {
        let mut entries = self
            .entries
            .write()
            .map_err(|_| Error::Config("录像索引锁异常".into()))?;
        let Some(position) = entries.iter().position(|entry| entry.id == id) else {
            return Ok(None);
        };
        let entry = entries.remove(position);
        self.persist(&entries)?;
        if entry.path.exists() {
            fs::remove_file(&entry.path)?;
        }
        Ok(Some(entry))
    }

    fn persist(&self, entries: &[RecordingEntry]) -> Result<()> {
        let path = self.root.join(INDEX_FILE);
        let temp_path = self.root.join("index.json.tmp");
        let payload = serde_json::to_vec_pretty(&RecordingIndexV1 {
            version: INDEX_VERSION,
            recordings: entries.to_vec(),
        })
        .map_err(|error| Error::Config(format!("录像索引序列化失败: {error}")))?;
        {
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&temp_path)?;
            file.write_all(&payload)?;
            file.sync_all()?;
        }
        fs::rename(&temp_path, &path)?;
        if let Ok(directory) = File::open(&self.root) {
            let _ = directory.sync_all();
        }
        Ok(())
    }
}

fn sort_entries(entries: &mut [RecordingEntry]) {
    entries.sort_by(|left, right| {
        left.start_time_ms
            .cmp(&right.start_time_ms)
            .then_with(|| left.id.cmp(&right.id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("uvp-recording-{name}-{nonce}"))
    }

    fn entry(
        root: &Path,
        id: &str,
        channel: &str,
        start: u64,
        kind: RecordingKind,
    ) -> RecordingEntry {
        let path = root.join(format!("{id}.mp4"));
        fs::write(&path, b"media").unwrap();
        RecordingEntry {
            id: id.into(),
            channel_id: channel.into(),
            start_time_ms: start,
            end_time_ms: start + 999,
            start_time: format!("2026-08-30T10:00:{:02}", start / 1_000),
            end_time: format!("2026-08-30T10:00:{:02}", start / 1_000 + 1),
            source: RecordingSource::Local,
            kind,
            path,
            size_bytes: 5,
        }
    }

    #[test]
    fn recording_index_round_trip_and_stable_order() {
        let root = temp_root("roundtrip");
        let store = RecordingStore::open(&root).unwrap();
        let second = entry(&root, "b", "ch-1", 2_000, RecordingKind::Manual);
        let first = entry(&root, "a", "ch-1", 1_000, RecordingKind::Time);
        store.add(second).unwrap();
        store.add(first.clone()).unwrap();
        drop(store);

        let loaded = RecordingStore::open(&root).unwrap().list();
        assert_eq!(loaded[0], first);
        assert_eq!(loaded[1].id, "b");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recording_index_corruption_is_reported_without_overwrite() {
        let root = temp_root("corrupt");
        fs::create_dir_all(&root).unwrap();
        let index = root.join(INDEX_FILE);
        fs::write(&index, b"{broken").unwrap();
        let error = RecordingStore::open(&root).unwrap_err().to_string();
        assert!(error.contains("解析失败"));
        assert_eq!(fs::read(&index).unwrap(), b"{broken");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recording_index_drops_missing_files_and_filters_overlap() {
        let root = temp_root("query");
        let store = RecordingStore::open(&root).unwrap();
        let kept = entry(&root, "kept", "ch-1", 2_000, RecordingKind::Manual);
        let missing = entry(&root, "missing", "ch-2", 4_000, RecordingKind::Alarm);
        store.add(kept).unwrap();
        store.add(missing.clone()).unwrap();
        fs::remove_file(&missing.path).unwrap();
        drop(store);

        let loaded = RecordingStore::open(&root).unwrap();
        assert_eq!(loaded.list().len(), 1);
        let matched = loaded.query(&RecordingQuery {
            start_time_ms: Some(2_500),
            end_time_ms: Some(3_500),
            channel_id: Some("ch-1".into()),
            kind: Some(RecordingKind::Manual),
        });
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].id, "kept");
        assert!(loaded
            .query(&RecordingQuery {
                kind: Some(RecordingKind::Alarm),
                ..Default::default()
            })
            .is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
