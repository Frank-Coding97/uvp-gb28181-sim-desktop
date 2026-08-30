//! 本地录像文件的版本化索引。

use common::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingPhase {
    Idle,
    Starting,
    Recording,
    Finalizing,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordingState {
    pub phase: RecordingPhase,
    pub active_id: Option<String>,
    pub error: Option<String>,
}

impl Default for RecordingState {
    fn default() -> Self {
        Self {
            phase: RecordingPhase::Idle,
            active_id: None,
            error: None,
        }
    }
}

trait RecordingFinalizer: Send + Sync {
    fn finalize(&self, input: &Path, output: &Path, fps: u32) -> Result<()>;
}

struct FfmpegRecordingFinalizer;

impl RecordingFinalizer for FfmpegRecordingFinalizer {
    fn finalize(&self, input: &Path, output: &Path, fps: u32) -> Result<()> {
        let ffmpeg = media_rtp::ffmpeg_bin()
            .ok_or_else(|| Error::Media("未找到 ffmpeg，无法收尾录像".into()))?;
        let result = std::process::Command::new(ffmpeg)
            .args(["-y", "-framerate"])
            .arg(fps.max(1).to_string())
            .arg("-i")
            .arg(input)
            .args(["-an", "-c:v", "copy", "-movflags", "+faststart"])
            .arg(output)
            .output()
            .map_err(|error| Error::Media(format!("ffmpeg 录像收尾启动失败: {error}")))?;
        if result.status.success() && output.is_file() {
            Ok(())
        } else {
            Err(Error::Media(format!(
                "ffmpeg 录像收尾失败: {}",
                String::from_utf8_lossy(&result.stderr)
                    .lines()
                    .last()
                    .unwrap_or("未知错误")
            )))
        }
    }
}

struct ActiveRecording {
    id: String,
    channel_id: String,
    start_time_ms: u64,
    start_time: String,
    source: RecordingSource,
    kind: RecordingKind,
    fps: u32,
    part_path: PathBuf,
    final_path: PathBuf,
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<u64>>,
}

/// 共享媒体旁路录像状态机。完成文件只在收尾成功后进入索引。
pub struct RecordingService {
    store: Arc<RecordingStore>,
    active: tokio::sync::Mutex<Option<ActiveRecording>>,
    state: Arc<Mutex<RecordingState>>,
    finalizer: Arc<dyn RecordingFinalizer>,
}

impl std::fmt::Debug for RecordingService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RecordingService")
            .field("store", &self.store.root())
            .field("state", &self.state())
            .finish()
    }
}

impl RecordingService {
    pub fn new(store: Arc<RecordingStore>) -> Self {
        Self::with_finalizer(store, Arc::new(FfmpegRecordingFinalizer))
    }

    fn with_finalizer(store: Arc<RecordingStore>, finalizer: Arc<dyn RecordingFinalizer>) -> Self {
        Self {
            store,
            active: tokio::sync::Mutex::new(None),
            state: Arc::new(Mutex::new(RecordingState::default())),
            finalizer,
        }
    }

    pub fn store(&self) -> &Arc<RecordingStore> {
        &self.store
    }

    pub fn state(&self) -> RecordingState {
        self.state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_else(|_| RecordingState {
                phase: RecordingPhase::Failed,
                active_id: None,
                error: Some("录像状态锁异常".into()),
            })
    }

    pub async fn start(
        &self,
        media: Option<Arc<media_rtp::SharedMedia>>,
        channel_id: impl Into<String>,
        kind: RecordingKind,
        source: RecordingSource,
        fps: u32,
    ) -> Result<RecordingState> {
        let media = media.ok_or_else(|| Error::Media("共享媒体未启动，不能录像".into()))?;
        if !media.is_alive() {
            return Err(Error::Media("共享媒体已停止，不能录像".into()));
        }
        let mut active = self.active.lock().await;
        if active.is_some() {
            return Err(Error::Media("当前已有录像任务".into()));
        }
        let start_time_ms = epoch_millis();
        let id = format!("{start_time_ms}-{:08x}", rand::random::<u32>());
        let part_path = self.store.root().join(format!("{id}.h264.part"));
        let final_path = self.store.root().join(format!("{id}.mp4"));
        let channel_id = channel_id.into();
        let (stop, mut stop_rx) = tokio::sync::watch::channel(false);
        let mut stream = media.subscribe();
        let part_for_task = part_path.clone();
        *self
            .state
            .lock()
            .map_err(|_| Error::Media("录像状态锁异常".into()))? = RecordingState {
            phase: RecordingPhase::Starting,
            active_id: Some(id.clone()),
            error: None,
        };
        let state = self.state.clone();
        let task = tokio::spawn(async move {
            use media_rtp::VideoSource;
            let mut file: Option<File> = None;
            let mut bytes_written = 0_u64;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                if *stop_rx.borrow() {
                    break;
                }
                if let Some(frame) = stream.next_frame() {
                    if file.is_none() {
                        if !frame.key_frame
                            || !media_rtp::contains_codec_config(&frame.data, stream.codec())
                        {
                            continue;
                        }
                        file = Some(File::create(&part_for_task)?);
                        if let Ok(mut current) = state.lock() {
                            current.phase = RecordingPhase::Recording;
                        }
                    }
                    if let Some(output) = file.as_mut() {
                        output.write_all(&frame.data)?;
                        bytes_written += frame.data.len() as u64;
                    }
                } else if tokio::time::Instant::now() >= deadline && file.is_none() {
                    return Err(Error::Media("等待可解码关键帧超时".into()));
                }
                tokio::select! {
                    changed = stop_rx.changed() => {
                        if changed.is_ok() && *stop_rx.borrow() {
                            break;
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(10)) => {}
                }
            }
            if let Some(mut output) = file {
                output.flush()?;
                output.sync_all()?;
            }
            if bytes_written == 0 {
                return Err(Error::Media("录像未收到可写入的媒体帧".into()));
            }
            Ok(bytes_written)
        });
        *active = Some(ActiveRecording {
            id,
            channel_id,
            start_time_ms,
            start_time: common::clock::synced_iso8601(),
            source,
            kind,
            fps: fps.max(1),
            part_path,
            final_path,
            stop,
            task,
        });
        Ok(self.state())
    }

    pub async fn stop(&self) -> Result<Option<RecordingEntry>> {
        let Some(active) = self.active.lock().await.take() else {
            return Ok(None);
        };
        if let Ok(mut state) = self.state.lock() {
            state.phase = RecordingPhase::Finalizing;
        }
        let _ = active.stop.send(true);
        let bytes_written = match active.task.await {
            Ok(Ok(bytes_written)) => bytes_written,
            Ok(Err(error)) => {
                self.fail(error.to_string());
                return Err(error);
            }
            Err(error) => {
                let error = Error::Media(format!("录像任务回收失败: {error}"));
                self.fail(error.to_string());
                return Err(error);
            }
        };
        if let Err(error) =
            self.finalizer
                .finalize(&active.part_path, &active.final_path, active.fps)
        {
            self.fail(error.to_string());
            return Err(error);
        }
        let size_bytes = fs::metadata(&active.final_path)
            .map(|metadata| metadata.len())
            .unwrap_or(bytes_written);
        let entry = RecordingEntry {
            id: active.id,
            channel_id: active.channel_id,
            start_time_ms: active.start_time_ms,
            end_time_ms: epoch_millis(),
            start_time: active.start_time,
            end_time: common::clock::synced_iso8601(),
            source: active.source,
            kind: active.kind,
            path: active.final_path,
            size_bytes,
        };
        self.store.add(entry.clone())?;
        let _ = fs::remove_file(active.part_path);
        *self
            .state
            .lock()
            .map_err(|_| Error::Media("录像状态锁异常".into()))? = RecordingState::default();
        Ok(Some(entry))
    }

    fn fail(&self, error: String) {
        if let Ok(mut state) = self.state.lock() {
            state.phase = RecordingPhase::Failed;
            state.active_id = None;
            state.error = Some(error);
        }
    }
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

fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct CopyFinalizer;

    impl RecordingFinalizer for CopyFinalizer {
        fn finalize(&self, input: &Path, output: &Path, _fps: u32) -> Result<()> {
            fs::copy(input, output)?;
            Ok(())
        }
    }

    struct RejectFinalizer;

    impl RecordingFinalizer for RejectFinalizer {
        fn finalize(&self, _input: &Path, _output: &Path, _fps: u32) -> Result<()> {
            Err(Error::Media("测试收尾失败".into()))
        }
    }

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

    fn sample_h264() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0x00]);
        data.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xce]);
        data.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x80, 0x22]);
        data.extend_from_slice(&[0, 0, 0, 1, 0x61, 0x80, 0x33]);
        data
    }

    async fn shared_media() -> Arc<media_rtp::SharedMedia> {
        let source = media_rtp::FileSource::from_bytes(&sample_h264()).unwrap();
        let media = media_rtp::start_shared_media(Box::new(source), 25);
        for _ in 0..100 {
            if media.has_frames() {
                return media;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("共享媒体未产生帧");
    }

    #[tokio::test]
    async fn recording_service_rejects_missing_media_and_duplicate_start() {
        let root = temp_root("service-busy");
        let store = Arc::new(RecordingStore::open(&root).unwrap());
        let service = RecordingService::with_finalizer(store, Arc::new(CopyFinalizer));
        assert!(service
            .start(
                None,
                "ch-1",
                RecordingKind::Manual,
                RecordingSource::Local,
                25,
            )
            .await
            .is_err());
        let media = shared_media().await;
        service
            .start(
                Some(Arc::clone(&media)),
                "ch-1",
                RecordingKind::Manual,
                RecordingSource::Local,
                25,
            )
            .await
            .unwrap();
        assert!(service
            .start(
                Some(Arc::clone(&media)),
                "ch-1",
                RecordingKind::Manual,
                RecordingSource::Local,
                25,
            )
            .await
            .is_err());
        tokio::time::sleep(Duration::from_millis(80)).await;
        let entry = service.stop().await.unwrap().unwrap();
        assert!(entry.path.is_file());
        assert_eq!(service.store().list().len(), 1);
        assert!(service.stop().await.unwrap().is_none());
        media.stop();
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn recording_finalize_failure_never_enters_completed_index() {
        let root = temp_root("service-fail");
        let store = Arc::new(RecordingStore::open(&root).unwrap());
        let service = RecordingService::with_finalizer(store, Arc::new(RejectFinalizer));
        let media = shared_media().await;
        service
            .start(
                Some(Arc::clone(&media)),
                "ch-1",
                RecordingKind::Time,
                RecordingSource::Platform,
                25,
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(service.stop().await.is_err());
        assert!(service.store().list().is_empty());
        assert_eq!(service.state().phase, RecordingPhase::Failed);
        media.stop();
        let _ = fs::remove_dir_all(root);
    }
}
