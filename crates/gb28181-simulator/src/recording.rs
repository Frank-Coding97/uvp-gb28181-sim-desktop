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
    /// 收尾后文件内的视频 codec（例如 `h264`/`h265`）。旧索引缺少该字段时为空。
    #[serde(default)]
    pub video_codec: Option<String>,
    /// 收尾后文件内的音频 codec（例如 `aac`/`opus`）。旧索引缺少该字段时为空。
    #[serde(default)]
    pub audio_codec: Option<String>,
    /// 收尾后文件内音频的实际采样率；无音轨时为空。
    #[serde(default)]
    pub audio_sample_rate_hz: Option<u32>,
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

    fn finalize_media(
        &self,
        input: &Path,
        output: &Path,
        media: &RecordingMediaSpec,
    ) -> Result<()> {
        self.finalize(input, output, media.fps)
    }
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

    fn finalize_media(
        &self,
        input: &Path,
        output: &Path,
        media: &RecordingMediaSpec,
    ) -> Result<()> {
        let ffmpeg = media_rtp::ffmpeg_bin()
            .ok_or_else(|| Error::Media("未找到 ffmpeg，无法收尾录像".into()))?;
        let mut command = std::process::Command::new(ffmpeg);
        command
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(input)
            .args(["-map", "0:v:0", "-c:v", "copy"]);
        match media.audio_codec {
            None => {
                command.arg("-an");
            }
            Some(media_rtp::AudioCodec::G711A | media_rtp::AudioCodec::G711U) => {
                let sample_rate = media.audio_sample_rate_hz.unwrap_or(8_000).to_string();
                command.args([
                    "-map",
                    "0:a:0",
                    "-c:a",
                    "aac",
                    "-b:a",
                    "32k",
                    "-ar",
                    sample_rate.as_str(),
                    "-ac",
                    "1",
                ]);
            }
            Some(media_rtp::AudioCodec::Aac | media_rtp::AudioCodec::Opus) => {
                command.args(["-map", "0:a:0", "-c:a", "copy"]);
            }
        }
        let result = command
            .args(["-movflags", "+faststart"])
            .arg(output)
            .output()
            .map_err(|error| Error::Media(format!("ffmpeg 录像收尾启动失败: {error}")))?;
        if !result.status.success() || !output.is_file() {
            return Err(Error::Media(format!(
                "ffmpeg 录像收尾失败: {}",
                String::from_utf8_lossy(&result.stderr)
                    .lines()
                    .last()
                    .unwrap_or("未知错误")
            )));
        }
        if let Some(audio_codec) = media.audio_codec {
            let probed = probe_recording_audio(output)?;
            let expected_codec = match audio_codec {
                media_rtp::AudioCodec::G711A
                | media_rtp::AudioCodec::G711U
                | media_rtp::AudioCodec::Aac => "aac",
                media_rtp::AudioCodec::Opus => "opus",
            };
            if probed.codec_name != expected_codec {
                return Err(Error::Media(format!(
                    "录像音轨编码不符: 期望 {expected_codec}, 实际 {}",
                    probed.codec_name
                )));
            }
            if let Some(expected_rate) = media.audio_sample_rate_hz {
                if probed.sample_rate_hz != Some(expected_rate) {
                    return Err(Error::Media(format!(
                        "录像音轨采样率不符: 期望 {expected_rate}, 实际 {:?}",
                        probed.sample_rate_hz
                    )));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProbedRecordingAudio {
    codec_name: String,
    sample_rate_hz: Option<u32>,
}

fn probe_recording_audio(path: &Path) -> Result<ProbedRecordingAudio> {
    let ffprobe = recording_ffprobe_bin()
        .ok_or_else(|| Error::Media("未找到 ffprobe，无法验证录像音轨".into()))?;
    let result = std::process::Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_type,codec_name,sample_rate",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .map_err(|error| Error::Media(format!("ffprobe 录像音轨启动失败: {error}")))?;
    if !result.status.success() {
        return Err(Error::Media(format!(
            "ffprobe 录像音轨失败: {}",
            String::from_utf8_lossy(&result.stderr)
                .lines()
                .last()
                .unwrap_or("未知错误")
        )));
    }
    let report: serde_json::Value = serde_json::from_slice(&result.stdout)
        .map_err(|error| Error::Media(format!("ffprobe 录像音轨结果解析失败: {error}")))?;
    let stream = report
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .and_then(|streams| streams.first())
        .ok_or_else(|| Error::Media("录像收尾成功但未发现音轨".into()))?;
    let codec_name = stream
        .get("codec_name")
        .and_then(serde_json::Value::as_str)
        .filter(|codec| !codec.is_empty())
        .ok_or_else(|| Error::Media("录像音轨缺少实际 codec".into()))?
        .to_string();
    let sample_rate_hz = stream
        .get("sample_rate")
        .and_then(serde_json::Value::as_str)
        .and_then(|rate| rate.parse::<u32>().ok());
    Ok(ProbedRecordingAudio {
        codec_name,
        sample_rate_hz,
    })
}

fn recording_ffprobe_bin() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("FFPROBE_BIN").map(PathBuf::from) {
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(ffmpeg) = media_rtp::ffmpeg_bin() {
        if let Some(sibling) = PathBuf::from(ffmpeg)
            .parent()
            .map(|parent| parent.join("ffprobe"))
            .filter(|path| path.is_file())
        {
            return Some(sibling);
        }
    }
    std::process::Command::new("sh")
        .args(["-c", "command -v ffprobe"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()))
        .filter(|path| path.is_file())
}

#[derive(Debug, Clone, Copy)]
struct RecordingMediaSpec {
    fps: u32,
    video_codec: media_rtp::VideoCodec,
    audio_codec: Option<media_rtp::AudioCodec>,
    audio_sample_rate_hz: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct RecordingCapture {
    bytes_written: u64,
    audio_sample_rate_hz: Option<u32>,
}

struct ActiveRecording {
    id: String,
    channel_id: String,
    start_time_ms: u64,
    start_time: String,
    source: RecordingSource,
    kind: RecordingKind,
    media: RecordingMediaSpec,
    part_path: PathBuf,
    final_path: PathBuf,
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<RecordingCapture>>,
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
        let part_path = self.store.root().join(format!("{id}.ps.part"));
        let final_path = self.store.root().join(format!("{id}.mp4"));
        let channel_id = channel_id.into();
        let (stop, mut stop_rx) = tokio::sync::watch::channel(false);
        let mut stream = media.subscribe();
        use media_rtp::VideoSource;
        let media_spec = RecordingMediaSpec {
            fps: fps.max(1),
            video_codec: stream.codec(),
            audio_codec: stream.has_audio().then(|| stream.audio_codec()),
            audio_sample_rate_hz: stream.has_audio().then(|| stream.audio_sample_rate_hz()),
        };
        // G.711U 的 GB28181 私有 stream_type(0x91) 在本机 FFmpeg PS demux 中会
        // 被误识别为 MP2。录像旁路将其先做一次 PCMU -> 线性 PCM -> PCMA，
        // 仅本地临时 PS 使用 G.711A；共享媒体事件本身仍保留原始 G.711U。
        let recording_audio_codec = media_spec.audio_codec.map(|codec| match codec {
            media_rtp::AudioCodec::G711U => media_rtp::AudioCodec::G711A,
            codec => codec,
        });
        let muxer = match recording_audio_codec {
            Some(audio) => media_rtp::PsMuxer::with_codecs(media_spec.video_codec, audio),
            None => media_rtp::PsMuxer::with_video(media_spec.video_codec),
        };
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
            let mut file: Option<File> = None;
            let mut bytes_written = 0_u64;
            let mut audio_sample_rate_hz = None;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                if *stop_rx.borrow() {
                    break;
                }
                if let Some(event) = stream.next_media_event() {
                    match event {
                        media_rtp::source::MediaEvent::Video(video) => {
                            if video.codec != media_spec.video_codec {
                                return Err(Error::Media(format!(
                                    "录像视频编码变化: {:?} -> {:?}",
                                    media_spec.video_codec, video.codec
                                )));
                            }
                            if file.is_none() {
                                if !video.key_frame
                                    || !media_rtp::contains_codec_config(&video.data, video.codec)
                                {
                                    continue;
                                }
                                file = Some(File::create(&part_for_task)?);
                                if let Ok(mut current) = state.lock() {
                                    current.phase = RecordingPhase::Recording;
                                }
                            }
                            if let Some(output) = file.as_mut() {
                                let packet = muxer.mux_video_au(&video);
                                output.write_all(&packet)?;
                                bytes_written += packet.len() as u64;
                            }
                        }
                        media_rtp::source::MediaEvent::Audio(audio) => {
                            if file.is_none() {
                                continue;
                            }
                            if media_spec.audio_codec != Some(audio.codec) {
                                return Err(Error::Media(format!(
                                    "录像音频编码变化: {:?} -> {:?}",
                                    media_spec.audio_codec, audio.codec
                                )));
                            }
                            if let Some(previous) = audio_sample_rate_hz {
                                if previous != audio.sample_rate_hz {
                                    return Err(Error::Media(format!(
                                        "录像音频采样率变化: {previous} -> {}",
                                        audio.sample_rate_hz
                                    )));
                                }
                            } else {
                                audio_sample_rate_hz = Some(audio.sample_rate_hz);
                            }
                            if let Some(output) = file.as_mut() {
                                let recording_audio = normalize_recording_audio(&audio);
                                let packet = muxer.mux_audio_au(&recording_audio);
                                output.write_all(&packet)?;
                                bytes_written += packet.len() as u64;
                            }
                        }
                        media_rtp::source::MediaEvent::Discontinuity { .. } => {}
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
            Ok(RecordingCapture {
                bytes_written,
                audio_sample_rate_hz,
            })
        });
        *active = Some(ActiveRecording {
            id,
            channel_id,
            start_time_ms,
            start_time: common::clock::synced_iso8601(),
            source,
            kind,
            media: media_spec,
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
        let capture = match active.task.await {
            Ok(Ok(capture)) => capture,
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
        let mut media = active.media;
        if capture.audio_sample_rate_hz.is_some() {
            media.audio_sample_rate_hz = capture.audio_sample_rate_hz;
        }
        if let Err(error) =
            self.finalizer
                .finalize_media(&active.part_path, &active.final_path, &media)
        {
            let _ = fs::remove_file(&active.final_path);
            self.fail(error.to_string());
            return Err(error);
        }
        let size_bytes = fs::metadata(&active.final_path)
            .map(|metadata| metadata.len())
            .unwrap_or(capture.bytes_written);
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
            video_codec: Some(file_video_codec_name(media.video_codec).into()),
            audio_codec: media
                .audio_codec
                .map(|codec| file_audio_codec_name(codec).into()),
            audio_sample_rate_hz: media.audio_codec.and(media.audio_sample_rate_hz),
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

fn file_video_codec_name(codec: media_rtp::VideoCodec) -> &'static str {
    match codec {
        media_rtp::VideoCodec::H264 => "h264",
        media_rtp::VideoCodec::H265 => "h265",
    }
}

fn file_audio_codec_name(codec: media_rtp::AudioCodec) -> &'static str {
    match codec {
        media_rtp::AudioCodec::G711A
        | media_rtp::AudioCodec::G711U
        | media_rtp::AudioCodec::Aac => "aac",
        media_rtp::AudioCodec::Opus => "opus",
    }
}

/// 把录像旁路里的 G.711U(PCMU)访问单元转成 G.711A(PCMA)访问单元。
///
/// 这是为本地 FFmpeg PS demux 兼容性做的压扩转换，重新量化会带来极小的
/// G.711U 录像音质变化，不能当作无损转码；上行媒体事件不经过此函数。
fn normalize_recording_audio(
    audio: &media_rtp::source::TimedAudioAu,
) -> media_rtp::source::TimedAudioAu {
    if audio.codec != media_rtp::AudioCodec::G711U {
        return audio.clone();
    }
    let mut normalized = audio.clone();
    normalized.codec = media_rtp::AudioCodec::G711A;
    normalized.data = normalized
        .data
        .iter()
        .copied()
        .map(g711u_to_g711a)
        .collect();
    normalized
}

fn g711u_to_g711a(encoded: u8) -> u8 {
    let value = !encoded;
    let exponent = i32::from((value >> 4) & 0x07);
    let mantissa = i32::from(value & 0x0f);
    let magnitude = ((mantissa << 3) + 0x84) << exponent;
    let pcm = if value & 0x80 != 0 {
        -(magnitude - 0x84)
    } else {
        magnitude - 0x84
    };
    linear_pcm_to_g711a(pcm)
}

fn linear_pcm_to_g711a(sample: i32) -> u8 {
    const SEGMENT_END: [i32; 8] = [0x1f, 0x3f, 0x7f, 0xff, 0x1ff, 0x3ff, 0x7ff, 0xfff];
    let mut pcm = sample >> 3;
    let mask = if pcm >= 0 {
        0xd5
    } else {
        pcm = (-pcm - 1).max(0);
        0x55
    };
    let segment = SEGMENT_END.iter().position(|&end| pcm <= end).unwrap_or(8);
    if segment >= 8 {
        return 0x7f ^ mask;
    }
    let mantissa = if segment < 2 {
        (pcm >> 1) & 0x0f
    } else {
        (pcm >> segment) & 0x0f
    };
    ((segment as u8) << 4 | mantissa as u8) ^ mask
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
    use std::collections::VecDeque;
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

    struct TimedEventsSource {
        events: VecDeque<media_rtp::source::MediaEvent>,
        first_event: bool,
        audio_codec: media_rtp::AudioCodec,
    }

    impl media_rtp::VideoSource for TimedEventsSource {
        fn next_frame(&mut self) -> Option<media_rtp::Frame> {
            None
        }

        fn supports_timed_events(&self) -> bool {
            true
        }

        fn next_media_event(&mut self) -> Option<media_rtp::source::MediaEvent> {
            if self.first_event {
                self.first_event = false;
                std::thread::sleep(Duration::from_millis(20));
            }
            self.events.pop_front()
        }

        fn has_audio(&self) -> bool {
            true
        }

        fn audio_codec(&self) -> media_rtp::AudioCodec {
            self.audio_codec
        }

        fn audio_sample_rate_hz(&self) -> u32 {
            8_000
        }

        fn codec(&self) -> media_rtp::VideoCodec {
            media_rtp::VideoCodec::H264
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
            video_codec: Some("h264".into()),
            audio_codec: None,
            audio_sample_rate_hz: None,
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
    fn recording_index_without_media_metadata_defaults_to_none() {
        let root = temp_root("legacy-media-metadata");
        let store = RecordingStore::open(&root).unwrap();
        let legacy = entry(&root, "legacy", "ch-1", 1_000, RecordingKind::Manual);
        let mut index = serde_json::to_value(RecordingIndexV1 {
            version: INDEX_VERSION,
            recordings: vec![legacy],
        })
        .unwrap();
        let recording = index
            .get_mut("recordings")
            .and_then(serde_json::Value::as_array_mut)
            .and_then(|recordings| recordings.first_mut())
            .and_then(serde_json::Value::as_object_mut)
            .unwrap();
        recording.remove("video_codec");
        recording.remove("audio_codec");
        recording.remove("audio_sample_rate_hz");
        fs::write(
            root.join(INDEX_FILE),
            serde_json::to_vec_pretty(&index).unwrap(),
        )
        .unwrap();
        drop(store);

        let loaded = RecordingStore::open(&root).unwrap().list();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].video_codec, None);
        assert_eq!(loaded[0].audio_codec, None);
        assert_eq!(loaded[0].audio_sample_rate_hz, None);
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

    fn timed_recording_source() -> TimedEventsSource {
        timed_recording_source_with_audio(media_rtp::AudioCodec::G711A)
    }

    fn timed_recording_source_with_audio(audio_codec: media_rtp::AudioCodec) -> TimedEventsSource {
        use media_rtp::source::{MediaEvent, TimedAudioAu, TimedVideoAu};

        let audio_data = match audio_codec {
            media_rtp::AudioCodec::G711U => vec![0xff; 160],
            _ => vec![0xd5; 160],
        };
        TimedEventsSource {
            events: VecDeque::from([
                MediaEvent::Video(TimedVideoAu::new(
                    sample_h264(),
                    media_rtp::VideoCodec::H264,
                    true,
                    0,
                    3_600,
                )),
                MediaEvent::Audio(TimedAudioAu::from_samples(
                    audio_data.clone(),
                    audio_codec,
                    8_000,
                    160,
                    0,
                )),
                MediaEvent::Audio(TimedAudioAu::from_samples(
                    audio_data,
                    audio_codec,
                    8_000,
                    160,
                    1_800,
                )),
                MediaEvent::Video(TimedVideoAu::new(
                    sample_h264(),
                    media_rtp::VideoCodec::H264,
                    false,
                    31_500,
                    3_600,
                )),
            ]),
            first_event: true,
            audio_codec,
        }
    }

    fn pes_pts(bytes: &[u8], stream_id: u8) -> Vec<u64> {
        let mut pts = Vec::new();
        for index in 0..bytes.len().saturating_sub(14) {
            if bytes[index..index + 4] != [0, 0, 1, stream_id]
                || bytes[index + 8] < 5
                || bytes[index + 7] & 0x80 == 0
            {
                continue;
            }
            let header = &bytes[index + 9..index + 14];
            let value = (u64::from((header[0] >> 1) & 0x07) << 30)
                | (u64::from(header[1]) << 22)
                | (u64::from(header[2] >> 1) & 0x7f) << 15
                | (u64::from(header[3]) << 7)
                | u64::from(header[4] >> 1);
            pts.push(value);
        }
        pts
    }

    fn ps_audio_stream_type(bytes: &[u8]) -> Option<u8> {
        for index in 0..bytes.len().saturating_sub(6) {
            if bytes[index..index + 4] != [0, 0, 1, 0xBC] {
                continue;
            }
            let body_start = index + 6;
            let body_len = usize::from(u16::from_be_bytes([bytes[index + 4], bytes[index + 5]]));
            let body_end = body_start.checked_add(body_len)?;
            if body_end > bytes.len() || body_len < 10 {
                continue;
            }
            let program_info_len = usize::from(u16::from_be_bytes([
                bytes[body_start + 2],
                bytes[body_start + 3],
            ]));
            let map_len_offset = body_start + 4;
            let map_start = body_start
                .checked_add(6)
                .and_then(|start| start.checked_add(program_info_len))?;
            let map_len = usize::from(u16::from_be_bytes([
                bytes[map_len_offset],
                bytes[map_len_offset + 1],
            ]));
            let map_end = map_start.checked_add(map_len)?;
            if map_end > body_end.saturating_sub(4) {
                continue;
            }
            let mut cursor = map_start;
            while cursor + 4 <= map_end {
                let stream_type = bytes[cursor];
                let stream_id = bytes[cursor + 1];
                let info_len =
                    usize::from(u16::from_be_bytes([bytes[cursor + 2], bytes[cursor + 3]]));
                let next = cursor.checked_add(4)?.checked_add(info_len)?;
                if next > map_end {
                    break;
                }
                if stream_id == 0xC0 {
                    return Some(stream_type);
                }
                cursor = next;
            }
        }
        None
    }

    fn ps_video_stream_type(bytes: &[u8]) -> Option<u8> {
        for index in 0..bytes.len().saturating_sub(6) {
            if bytes[index..index + 4] != [0, 0, 1, 0xBC] {
                continue;
            }
            let body_start = index + 6;
            let body_len = usize::from(u16::from_be_bytes([bytes[index + 4], bytes[index + 5]]));
            let body_end = body_start.checked_add(body_len)?;
            if body_end > bytes.len() || body_len < 10 {
                continue;
            }
            let program_info_len = usize::from(u16::from_be_bytes([
                bytes[body_start + 2],
                bytes[body_start + 3],
            ]));
            let map_start = body_start
                .checked_add(6)
                .and_then(|start| start.checked_add(program_info_len))?;
            let map_len = usize::from(u16::from_be_bytes([
                bytes[body_start + 4],
                bytes[body_start + 5],
            ]));
            let map_end = map_start.checked_add(map_len)?;
            if map_end > body_end.saturating_sub(4) {
                continue;
            }
            let mut cursor = map_start;
            while cursor + 4 <= map_end {
                let stream_type = bytes[cursor];
                let stream_id = bytes[cursor + 1];
                let info_len =
                    usize::from(u16::from_be_bytes([bytes[cursor + 2], bytes[cursor + 3]]));
                let next = cursor.checked_add(4)?.checked_add(info_len)?;
                if next > map_end {
                    break;
                }
                if stream_id == 0xE0 {
                    return Some(stream_type);
                }
                cursor = next;
            }
        }
        None
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
        let partials: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.to_string_lossy().ends_with(".ps.part"))
            .collect();
        assert_eq!(partials.len(), 1, "收尾失败应保留唯一临时 PS");
        assert_eq!(service.state().phase, RecordingPhase::Failed);
        media.stop();
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn recording_timed_events_keep_audio_when_video_has_gap() {
        let root = temp_root("timed-av");
        let store = Arc::new(RecordingStore::open(&root).unwrap());
        let service = RecordingService::with_finalizer(store, Arc::new(CopyFinalizer));
        let media = media_rtp::start_shared_media(Box::new(timed_recording_source()), 25);
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
        tokio::time::sleep(Duration::from_millis(100)).await;

        let entry = service.stop().await.unwrap().unwrap();
        let bytes = fs::read(&entry.path).unwrap();
        assert!(bytes.windows(4).any(|window| window == [0, 0, 1, 0xE0]));
        assert!(bytes.windows(4).any(|window| window == [0, 0, 1, 0xC0]));
        let video_pts = pes_pts(&bytes, 0xE0);
        let audio_pts = pes_pts(&bytes, 0xC0);
        assert!(video_pts.contains(&0));
        assert!(
            video_pts.contains(&31_500),
            "视频缺口的 PTS 未保留: {video_pts:?}"
        );
        assert!(audio_pts.contains(&0));
        assert!(audio_pts.contains(&1_800));

        media.stop();
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn recording_timed_events_normalize_g711u_for_local_ps() {
        let root = temp_root("timed-g711u");
        let store = Arc::new(RecordingStore::open(&root).unwrap());
        let service = RecordingService::with_finalizer(store, Arc::new(CopyFinalizer));
        let media = media_rtp::start_shared_media(
            Box::new(timed_recording_source_with_audio(
                media_rtp::AudioCodec::G711U,
            )),
            25,
        );
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
        tokio::time::sleep(Duration::from_millis(100)).await;

        let entry = service.stop().await.unwrap().unwrap();
        let bytes = fs::read(&entry.path).unwrap();
        assert_eq!(
            ps_audio_stream_type(&bytes),
            Some(0x90),
            "本地 G.711U 录制应使用 G.711A PSM"
        );
        assert_eq!(entry.video_codec.as_deref(), Some("h264"));
        assert_eq!(entry.audio_codec.as_deref(), Some("aac"));
        assert_eq!(entry.audio_sample_rate_hz, Some(8_000));

        media.stop();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn g711u_recording_normalization_preserves_timing() {
        let source = media_rtp::source::TimedAudioAu::with_duration(
            vec![0xff, 0x7f, 0x00, 0x80],
            media_rtp::AudioCodec::G711U,
            8_000,
            4,
            12_345,
            45,
        );
        let normalized = normalize_recording_audio(&source);
        assert_eq!(normalized.codec, media_rtp::AudioCodec::G711A);
        assert_eq!(normalized.sample_rate_hz, source.sample_rate_hz);
        assert_eq!(normalized.sample_count, source.sample_count);
        assert_eq!(normalized.pts_90k, source.pts_90k);
        assert_eq!(normalized.duration_90k, source.duration_90k);
        assert_eq!(normalized.data.len(), source.data.len());
        assert_ne!(normalized.data, source.data);
    }

    #[test]
    #[ignore = "需可执行 ffmpeg + ffprobe 的合成媒体环境"]
    fn ffmpeg_finalizer_converts_g711_ps_to_decodable_aac_mp4() {
        let ffmpeg = media_rtp::ffmpeg_bin().expect("T08 真实收尾验证需要 ffmpeg");
        let ffmpeg_path = PathBuf::from(ffmpeg);
        let ffprobe = recording_ffprobe_bin().expect("T08 真实收尾验证需要 ffprobe");

        let root = temp_root("ffmpeg-g711");
        fs::create_dir_all(&root).unwrap();
        let h264_path = root.join("source.h264");
        let alaw_path = root.join("source.alaw");
        let input_path = root.join("capture.ps.part");
        let output_path = root.join("capture.mp4");
        let video = std::process::Command::new(&ffmpeg_path)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x120:rate=25",
                "-frames:v",
                "1",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "h264",
            ])
            .arg(&h264_path)
            .output()
            .unwrap();
        assert!(video.status.success(), "合成 H.264 失败: {:?}", video);
        let audio = std::process::Command::new(&ffmpeg_path)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:sample_rate=8000",
                "-t",
                "0.02",
                "-c:a",
                "pcm_alaw",
                "-ar",
                "8000",
                "-ac",
                "1",
                "-f",
                "alaw",
            ])
            .arg(&alaw_path)
            .output()
            .unwrap();
        assert!(audio.status.success(), "合成 G.711A 失败: {:?}", audio);

        let video_au = media_rtp::source::TimedVideoAu::new(
            fs::read(&h264_path).unwrap(),
            media_rtp::VideoCodec::H264,
            true,
            0,
            3_600,
        );
        let audio_au = media_rtp::source::TimedAudioAu::from_samples(
            fs::read(&alaw_path).unwrap(),
            media_rtp::AudioCodec::G711A,
            8_000,
            160,
            0,
        );
        let muxer = media_rtp::PsMuxer::with_codecs(
            media_rtp::VideoCodec::H264,
            media_rtp::AudioCodec::G711A,
        );
        let mut ps = muxer.mux_video_au(&video_au);
        ps.extend(muxer.mux_audio_au(&audio_au));
        fs::write(&input_path, ps).unwrap();

        let spec = RecordingMediaSpec {
            fps: 25,
            video_codec: media_rtp::VideoCodec::H264,
            audio_codec: Some(media_rtp::AudioCodec::G711A),
            audio_sample_rate_hz: Some(8_000),
        };
        FfmpegRecordingFinalizer
            .finalize_media(&input_path, &output_path, &spec)
            .unwrap();
        let probe = std::process::Command::new(&ffprobe)
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type,codec_name,sample_rate",
                "-of",
                "json",
            ])
            .arg(&output_path)
            .output()
            .unwrap();
        assert!(probe.status.success(), "ffprobe 失败: {:?}", probe);
        let report = String::from_utf8_lossy(&probe.stdout);
        assert!(
            report.contains("\"codec_name\": \"h264\""),
            "缺少 H.264: {report}"
        );
        assert!(
            report.contains("\"codec_name\": \"aac\""),
            "缺少 AAC: {report}"
        );
        assert!(
            report.contains("\"sample_rate\": \"8000\""),
            "采样率不符: {report}"
        );
        let decoded = std::process::Command::new(&ffmpeg_path)
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&output_path)
            .args(["-f", "null", "-"])
            .output()
            .unwrap();
        assert!(decoded.status.success(), "MP4 解码失败: {:?}", decoded);

        let video_only_input = root.join("video-only.ps.part");
        let video_only_output = root.join("video-only.mp4");
        let video_only_muxer = media_rtp::PsMuxer::with_video(media_rtp::VideoCodec::H264);
        fs::write(&video_only_input, video_only_muxer.mux_video_au(&video_au)).unwrap();
        let missing_audio = FfmpegRecordingFinalizer
            .finalize_media(&video_only_input, &video_only_output, &spec)
            .unwrap_err();
        assert!(missing_audio.to_string().contains("ffmpeg 录像收尾失败"));
        assert!(
            !video_only_output.exists(),
            "声明有音频时不能静默产出纯视频"
        );

        let pure_video_output = root.join("pure-video.mp4");
        let pure_video_spec = RecordingMediaSpec {
            audio_codec: None,
            audio_sample_rate_hz: None,
            ..spec
        };
        FfmpegRecordingFinalizer
            .finalize_media(&video_only_input, &pure_video_output, &pure_video_spec)
            .unwrap();
        let pure_video_probe = std::process::Command::new(&ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "a:0",
                "-show_entries",
                "stream=codec_type",
                "-of",
                "json",
            ])
            .arg(&pure_video_output)
            .output()
            .unwrap();
        assert!(pure_video_probe.status.success());
        assert!(
            !String::from_utf8_lossy(&pure_video_probe.stdout).contains("audio"),
            "无音轨输入应产出纯视频"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "需可执行 ffmpeg + ffprobe 的合成媒体环境"]
    fn ffmpeg_finalizer_converts_g711u_recording_to_decodable_aac_mp4() {
        let ffmpeg = media_rtp::ffmpeg_bin().expect("T08 真实收尾验证需要 ffmpeg");
        let ffmpeg_path = PathBuf::from(ffmpeg);
        let ffprobe = recording_ffprobe_bin().expect("T08 真实收尾验证需要 ffprobe");

        let root = temp_root("ffmpeg-g711u");
        fs::create_dir_all(&root).unwrap();
        let h264_path = root.join("source.h264");
        let ulaw_path = root.join("source.mulaw");
        let input_path = root.join("capture.ps.part");
        let output_path = root.join("capture.mp4");
        let video = std::process::Command::new(&ffmpeg_path)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x120:rate=25",
                "-frames:v",
                "1",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "h264",
            ])
            .arg(&h264_path)
            .output()
            .unwrap();
        assert!(video.status.success(), "合成 H.264 失败: {:?}", video);
        let audio = std::process::Command::new(&ffmpeg_path)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:sample_rate=8000",
                "-t",
                "0.08",
                "-c:a",
                "pcm_mulaw",
                "-ar",
                "8000",
                "-ac",
                "1",
                "-f",
                "mulaw",
            ])
            .arg(&ulaw_path)
            .output()
            .unwrap();
        assert!(audio.status.success(), "合成 G.711U 失败: {:?}", audio);

        let video_au = media_rtp::source::TimedVideoAu::new(
            fs::read(&h264_path).unwrap(),
            media_rtp::VideoCodec::H264,
            true,
            0,
            3_600,
        );
        let source_audio_data = fs::read(&ulaw_path).unwrap();
        assert!(!source_audio_data.is_empty(), "合成 G.711U 未产生有效样本");
        let source_audio = media_rtp::source::TimedAudioAu::from_samples(
            source_audio_data,
            media_rtp::AudioCodec::G711U,
            8_000,
            u32::try_from(fs::metadata(&ulaw_path).unwrap().len()).unwrap(),
            0,
        );
        let recording_audio = normalize_recording_audio(&source_audio);
        assert_eq!(source_audio.codec, media_rtp::AudioCodec::G711U);
        assert_eq!(recording_audio.codec, media_rtp::AudioCodec::G711A);
        assert_eq!(recording_audio.sample_rate_hz, 8_000);
        assert_eq!(recording_audio.sample_count, source_audio.sample_count);
        assert_eq!(recording_audio.pts_90k, source_audio.pts_90k);
        assert_eq!(recording_audio.duration_90k, source_audio.duration_90k);

        let muxer = media_rtp::PsMuxer::with_codecs(
            media_rtp::VideoCodec::H264,
            media_rtp::AudioCodec::G711A,
        );
        let mut ps = muxer.mux_video_au(&video_au);
        ps.extend(muxer.mux_audio_au(&recording_audio));
        assert_eq!(ps_audio_stream_type(&ps), Some(0x90));
        fs::write(&input_path, ps).unwrap();

        let spec = RecordingMediaSpec {
            fps: 25,
            video_codec: media_rtp::VideoCodec::H264,
            audio_codec: Some(media_rtp::AudioCodec::G711U),
            audio_sample_rate_hz: Some(8_000),
        };
        FfmpegRecordingFinalizer
            .finalize_media(&input_path, &output_path, &spec)
            .unwrap();
        let probed = probe_recording_audio(&output_path).unwrap();
        assert_eq!(probed.codec_name, "aac");
        assert_eq!(probed.sample_rate_hz, Some(8_000));

        let decoded = std::process::Command::new(&ffmpeg_path)
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&output_path)
            .args(["-map", "0:a:0", "-f", "null", "-"])
            .output()
            .unwrap();
        assert!(
            decoded.status.success(),
            "G.711U 录像音频解码失败: {:?}",
            decoded
        );
        let duration = std::process::Command::new(&ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "a:0",
                "-show_entries",
                "stream=duration",
                "-of",
                "default=nw=1:nk=1",
            ])
            .arg(&output_path)
            .output()
            .unwrap();
        assert!(
            duration.status.success(),
            "ffprobe 音轨时长失败: {:?}",
            duration
        );
        let duration_seconds = String::from_utf8_lossy(&duration.stdout)
            .trim()
            .parse::<f64>()
            .expect("G.711U MP4 音轨缺少有效时长");
        assert!(
            duration_seconds > 0.01,
            "G.711U MP4 音轨时长异常: {duration_seconds}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "需可执行 ffmpeg + ffprobe 的合成媒体环境"]
    fn ffmpeg_finalizer_preserves_aac_8k_and_16k_recordings() {
        let ffmpeg = media_rtp::ffmpeg_bin().expect("T08 真实收尾验证需要 ffmpeg");
        let ffmpeg_path = PathBuf::from(ffmpeg);
        let ffprobe = recording_ffprobe_bin().expect("T08 真实收尾验证需要 ffprobe");
        let root = temp_root("ffmpeg-aac-rates");
        fs::create_dir_all(&root).unwrap();
        let h264_path = root.join("source.h264");
        let video = std::process::Command::new(&ffmpeg_path)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x120:rate=25",
                "-frames:v",
                "1",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "h264",
            ])
            .arg(&h264_path)
            .output()
            .unwrap();
        assert!(video.status.success(), "合成 H.264 失败: {:?}", video);
        let video_au = media_rtp::source::TimedVideoAu::new(
            fs::read(&h264_path).unwrap(),
            media_rtp::VideoCodec::H264,
            true,
            0,
            3_600,
        );

        for sample_rate_hz in [8_000_u32, 16_000_u32] {
            let aac_path = root.join(format!("source-{sample_rate_hz}.adts"));
            let input_path = root.join(format!("capture-{sample_rate_hz}.ps.part"));
            let output_path = root.join(format!("capture-{sample_rate_hz}.mp4"));
            let sample_rate_arg = sample_rate_hz.to_string();
            let audio = std::process::Command::new(&ffmpeg_path)
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=1000",
                    "-t",
                    "0.128",
                    "-c:a",
                    "aac",
                    "-b:a",
                    "32k",
                    "-ar",
                    sample_rate_arg.as_str(),
                    "-ac",
                    "1",
                    "-f",
                    "adts",
                ])
                .arg(&aac_path)
                .output()
                .unwrap();
            assert!(
                audio.status.success(),
                "合成 AAC {sample_rate_hz}Hz 失败: {audio:?}"
            );
            let audio_data = fs::read(&aac_path).unwrap();
            assert!(
                !audio_data.is_empty(),
                "合成 AAC {sample_rate_hz}Hz 未产生样本"
            );
            let audio_au = media_rtp::source::TimedAudioAu::from_samples(
                audio_data,
                media_rtp::AudioCodec::Aac,
                sample_rate_hz,
                1_024,
                0,
            );
            let muxer = media_rtp::PsMuxer::with_codecs(
                media_rtp::VideoCodec::H264,
                media_rtp::AudioCodec::Aac,
            );
            let mut ps = muxer.mux_video_au(&video_au);
            ps.extend(muxer.mux_audio_au(&audio_au));
            fs::write(&input_path, ps).unwrap();

            let spec = RecordingMediaSpec {
                fps: 25,
                video_codec: media_rtp::VideoCodec::H264,
                audio_codec: Some(media_rtp::AudioCodec::Aac),
                audio_sample_rate_hz: Some(sample_rate_hz),
            };
            FfmpegRecordingFinalizer
                .finalize_media(&input_path, &output_path, &spec)
                .unwrap();
            let probed = probe_recording_audio(&output_path).unwrap();
            assert_eq!(probed.codec_name, "aac");
            assert_eq!(probed.sample_rate_hz, Some(sample_rate_hz));

            let decoded = std::process::Command::new(&ffmpeg_path)
                .args(["-hide_banner", "-loglevel", "error", "-i"])
                .arg(&output_path)
                .args(["-map", "0:a:0", "-f", "null", "-"])
                .output()
                .unwrap();
            assert!(
                decoded.status.success(),
                "AAC {sample_rate_hz}Hz 录像音频解码失败: {decoded:?}"
            );
            let duration = std::process::Command::new(&ffprobe)
                .args([
                    "-v",
                    "error",
                    "-select_streams",
                    "a:0",
                    "-show_entries",
                    "stream=duration",
                    "-of",
                    "default=nw=1:nk=1",
                ])
                .arg(&output_path)
                .output()
                .unwrap();
            assert!(
                duration.status.success(),
                "ffprobe AAC {sample_rate_hz}Hz 音轨时长失败: {duration:?}"
            );
            let duration_seconds = String::from_utf8_lossy(&duration.stdout)
                .trim()
                .parse::<f64>()
                .expect("AAC MP4 音轨缺少有效时长");
            assert!(
                duration_seconds > 0.01,
                "AAC {sample_rate_hz}Hz MP4 音轨时长异常: {duration_seconds}"
            );
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "需可执行 ffmpeg + ffprobe 的合成媒体环境"]
    fn ffmpeg_finalizer_records_eight_video_audio_combinations() {
        let ffmpeg = media_rtp::ffmpeg_bin().expect("T08 真实收尾验证需要 ffmpeg");
        let ffmpeg_path = PathBuf::from(ffmpeg);
        let ffprobe = recording_ffprobe_bin().expect("T08 真实收尾验证需要 ffprobe");
        let root = temp_root("ffmpeg-recording-matrix");
        fs::create_dir_all(&root).unwrap();

        let video_cases = [
            (
                "h264",
                media_rtp::VideoCodec::H264,
                "h264",
                "libx264",
                "h264",
            ),
            (
                "h265",
                media_rtp::VideoCodec::H265,
                "hevc",
                "libx265",
                "hevc",
            ),
        ];
        let mut video_data = Vec::new();
        for (label, codec, expected_codec, encoder, output_format) in video_cases {
            let video_path = root.join(format!("source.{output_format}"));
            let mut args = vec![
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x120:rate=25",
                "-frames:v",
                "1",
                "-c:v",
                encoder,
            ];
            if codec == media_rtp::VideoCodec::H265 {
                args.extend(["-preset", "ultrafast", "-x265-params", "log-level=error"]);
            } else {
                args.extend(["-pix_fmt", "yuv420p"]);
            }
            args.extend(["-f", output_format]);
            let result = std::process::Command::new(&ffmpeg_path)
                .args(args)
                .arg(&video_path)
                .output()
                .unwrap();
            assert!(result.status.success(), "合成 {label} 失败: {result:?}");
            let data = fs::read(video_path).unwrap();
            assert!(!data.is_empty(), "合成 {label} 未产生视频样本");
            video_data.push((label, codec, expected_codec, data));
        }

        let audio_cases = [
            ("g711-a-8k", media_rtp::AudioCodec::G711A, 8_000_u32),
            ("g711-u-8k", media_rtp::AudioCodec::G711U, 8_000_u32),
            ("aac-8k", media_rtp::AudioCodec::Aac, 8_000_u32),
            ("aac-16k", media_rtp::AudioCodec::Aac, 16_000_u32),
        ];
        let mut case_count = 0;
        for (audio_label, audio_codec, sample_rate_hz) in audio_cases {
            let audio_path = root.join(format!("source-{audio_label}"));
            let sample_rate_arg = sample_rate_hz.to_string();
            let (encoder, output_format, duration) = match audio_codec {
                media_rtp::AudioCodec::G711A => ("pcm_alaw", "alaw", "0.08"),
                media_rtp::AudioCodec::G711U => ("pcm_mulaw", "mulaw", "0.08"),
                media_rtp::AudioCodec::Aac => ("aac", "adts", "0.128"),
                media_rtp::AudioCodec::Opus => unreachable!("T08矩阵不包含 Opus"),
            };
            let mut audio_args = vec![
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000",
                "-t",
                duration,
                "-c:a",
                encoder,
                "-ar",
                sample_rate_arg.as_str(),
                "-ac",
                "1",
            ];
            if audio_codec == media_rtp::AudioCodec::Aac {
                audio_args.extend(["-b:a", "32k"]);
            }
            audio_args.extend(["-f", output_format]);
            let result = std::process::Command::new(&ffmpeg_path)
                .args(audio_args)
                .arg(&audio_path)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "合成 {audio_label} 失败: {result:?}"
            );
            let audio_data = fs::read(&audio_path).unwrap();
            assert!(!audio_data.is_empty(), "合成 {audio_label} 未产生音频样本");
            let sample_count = match audio_codec {
                media_rtp::AudioCodec::G711A | media_rtp::AudioCodec::G711U => {
                    u32::try_from(audio_data.len()).unwrap()
                }
                media_rtp::AudioCodec::Aac => 1_024,
                media_rtp::AudioCodec::Opus => unreachable!("T08矩阵不包含 Opus"),
            };
            let source_audio = media_rtp::source::TimedAudioAu::from_samples(
                audio_data,
                audio_codec,
                sample_rate_hz,
                sample_count,
                0,
            );
            let recording_audio = normalize_recording_audio(&source_audio);
            assert_eq!(source_audio.codec, audio_codec);
            assert_eq!(recording_audio.sample_rate_hz, sample_rate_hz);
            assert_eq!(recording_audio.sample_count, source_audio.sample_count);
            assert_eq!(recording_audio.pts_90k, source_audio.pts_90k);
            assert_eq!(recording_audio.duration_90k, source_audio.duration_90k);

            for (video_label, video_codec, expected_video_codec, data) in &video_data {
                let video_au = media_rtp::source::TimedVideoAu::new(
                    data.clone(),
                    *video_codec,
                    true,
                    0,
                    3_600,
                );
                let muxer = media_rtp::PsMuxer::with_codecs(*video_codec, recording_audio.codec);
                let mut ps = muxer.mux_video_au(&video_au);
                ps.extend(muxer.mux_audio_au(&recording_audio));
                let expected_audio_stream_type = match audio_codec {
                    media_rtp::AudioCodec::G711A | media_rtp::AudioCodec::G711U => 0x90,
                    media_rtp::AudioCodec::Aac => 0x0F,
                    media_rtp::AudioCodec::Opus => unreachable!("T08矩阵不包含 Opus"),
                };
                assert_eq!(ps_audio_stream_type(&ps), Some(expected_audio_stream_type));
                assert_eq!(
                    ps_video_stream_type(&ps),
                    Some(video_codec.stream_type()),
                    "{video_label} PSM 视频编码声明不符"
                );

                let input_path = root.join(format!("capture-{video_label}-{audio_label}.ps.part"));
                let output_path = root.join(format!("capture-{video_label}-{audio_label}.mp4"));
                fs::write(&input_path, ps).unwrap();
                let spec = RecordingMediaSpec {
                    fps: 25,
                    video_codec: *video_codec,
                    audio_codec: Some(audio_codec),
                    audio_sample_rate_hz: Some(sample_rate_hz),
                };
                FfmpegRecordingFinalizer
                    .finalize_media(&input_path, &output_path, &spec)
                    .unwrap();
                let audio_probe = probe_recording_audio(&output_path).unwrap();
                assert_eq!(audio_probe.codec_name, "aac");
                assert_eq!(audio_probe.sample_rate_hz, Some(sample_rate_hz));

                let video_probe = std::process::Command::new(&ffprobe)
                    .args([
                        "-v",
                        "error",
                        "-select_streams",
                        "v:0",
                        "-show_entries",
                        "stream=codec_name",
                        "-of",
                        "default=nw=1:nk=1",
                    ])
                    .arg(&output_path)
                    .output()
                    .unwrap();
                assert!(
                    video_probe.status.success(),
                    "ffprobe 视频失败: {video_probe:?}"
                );
                assert_eq!(
                    String::from_utf8_lossy(&video_probe.stdout).trim(),
                    *expected_video_codec
                );
                let decoded = std::process::Command::new(&ffmpeg_path)
                    .args(["-hide_banner", "-loglevel", "error", "-i"])
                    .arg(&output_path)
                    .args(["-map", "0:v:0", "-map", "0:a:0", "-f", "null", "-"])
                    .output()
                    .unwrap();
                assert!(
                    decoded.status.success(),
                    "{video_label}/{audio_label} 录像解码失败: {decoded:?}"
                );
                let duration = std::process::Command::new(&ffprobe)
                    .args([
                        "-v",
                        "error",
                        "-select_streams",
                        "a:0",
                        "-show_entries",
                        "stream=duration",
                        "-of",
                        "default=nw=1:nk=1",
                    ])
                    .arg(&output_path)
                    .output()
                    .unwrap();
                assert!(
                    duration.status.success(),
                    "ffprobe {video_label}/{audio_label} 时长失败: {duration:?}"
                );
                let duration_seconds = String::from_utf8_lossy(&duration.stdout)
                    .trim()
                    .parse::<f64>()
                    .expect("录像音轨缺少有效时长");
                assert!(
                    duration_seconds > 0.01,
                    "{video_label}/{audio_label} 音轨时长异常: {duration_seconds}"
                );
                println!(
                    "[recording-matrix] {video_label}/{audio_label}: video={expected_video_codec}, audio=aac@{sample_rate_hz}, duration={duration_seconds:.3}s"
                );
                case_count += 1;
            }
        }
        assert_eq!(case_count, 8, "录像矩阵必须覆盖 H.264/H.265 × 4 音频形态");

        let _ = fs::remove_dir_all(root);
    }
}
