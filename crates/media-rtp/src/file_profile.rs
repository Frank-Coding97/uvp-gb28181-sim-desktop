#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{MediaAudioCodec, MediaProfile, MediaVideoCodec};
    use crate::source::MediaEvent;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::AtomicBool;

    fn ffmpeg() -> Option<String> {
        crate::source::ffmpeg_bin()
    }

    fn temp_input(name: &str) -> PathBuf {
        let nonce = format!(
            "{}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test"),
            name
        );
        std::env::temp_dir().join(format!("uvp-file-profile-{nonce}.mp4"))
    }

    fn make_synthetic_input(path: &Path) {
        let ffmpeg = ffmpeg().expect("测试需要可用的 ffmpeg");
        let output = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=30",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:sample_rate=48000",
                "-t",
                "1",
                "-c:v",
                "libx264",
                "-c:a",
                "aac",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(path)
            .output()
            .expect("启动合成素材 ffmpeg 失败");
        assert!(output.status.success(), "合成素材失败: {:?}", output);
    }

    fn indexed_test_profile(
        video: &[(u64, u64, bool)],
        audio: &[(u64, u64)],
        duration_90k: u64,
    ) -> PreparedFileProfile {
        let nonce = TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "uvp-file-profile-timed-{}-{nonce}",
            std::process::id()
        ));
        let video_file = base.with_extension("video");
        let audio_file = base.with_extension("audio");

        let mut video_bytes = Vec::new();
        let mut video_index = Vec::new();
        for (pts_90k, duration_90k, key_frame) in video {
            let offset = video_bytes.len() as u64;
            video_bytes.push(0x11);
            video_index.push(VideoAccessUnitIndex {
                offset,
                length: 1,
                pts_90k: *pts_90k,
                duration_90k: *duration_90k,
                key_frame: *key_frame,
                codec: MediaVideoCodec::H264,
            });
        }
        let mut audio_bytes = Vec::new();
        let mut audio_index = Vec::new();
        for (pts_90k, duration_90k) in audio {
            let offset = audio_bytes.len() as u64;
            audio_bytes.push(0x22);
            audio_index.push(AudioAccessUnitIndex {
                offset,
                length: 1,
                pts_90k: *pts_90k,
                duration_90k: *duration_90k,
                sample_rate_hz: 8000,
                sample_count: 1,
                codec: MediaAudioCodec::G711A,
            });
        }
        fs::write(&video_file, video_bytes).expect("写入测试视频缓存失败");
        fs::write(&audio_file, audio_bytes).expect("写入测试音频缓存失败");

        PreparedFileProfile {
            cache_key: format!("timed-{nonce}"),
            cache_dir: base.with_extension("cache"),
            profile: MediaProfile::default(),
            video_file,
            audio_file: Some(audio_file),
            video_index,
            audio_index,
            duration_90k,
        }
    }

    fn event_signature(event: &MediaEvent) -> (&'static str, u64) {
        match event {
            MediaEvent::Video(video) => ("video", video.pts_90k),
            MediaEvent::Audio(audio) => ("audio", audio.pts_90k),
            MediaEvent::Discontinuity { pts_90k } => ("discontinuity", *pts_90k),
        }
    }

    fn remove_indexed_test_profile(prepared: &PreparedFileProfile) {
        let _ = fs::remove_file(&prepared.video_file);
        if let Some(audio_file) = &prepared.audio_file {
            let _ = fs::remove_file(audio_file);
        }
    }

    #[test]
    fn file_profile_cache_key_includes_source_and_full_profile() {
        let first = MediaProfile::default();
        let mut second = first.clone();
        second.video_fps = 30;

        let first_key = cache_key_for("source-a", &first, "ffmpeg", "ffprobe");
        let second_key = cache_key_for("source-a", &second, "ffmpeg", "ffprobe");
        let changed_source_key = cache_key_for("source-b", &first, "ffmpeg", "ffprobe");

        assert_ne!(first_key, second_key);
        assert_ne!(first_key, changed_source_key);
        assert_eq!(first_key.len(), 64);
    }

    #[test]
    fn cancelled_preparation_does_not_start_or_publish() {
        let cancel = AtomicBool::new(true);
        let result = prepare_file_profile(
            Path::new("/definitely/missing-source.mp4"),
            &MediaProfile::default(),
            &cancel,
            None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("取消"));
    }

    #[test]
    fn synthetic_file_is_transcoded_indexed_and_read_with_timestamps() {
        assert!(
            ffmpeg().is_some(),
            "依赖测试需要可用的 ffmpeg；请安装 ffmpeg 后显式运行此测试"
        );
        assert!(
            ffprobe_bin().is_some(),
            "依赖测试需要可用的 ffprobe；请安装 ffmpeg 后显式运行此测试"
        );
        let input = temp_input("indexed");
        make_synthetic_input(&input);
        let cancel = AtomicBool::new(false);
        let profile = MediaProfile {
            width: 640,
            height: 480,
            video_fps: 15,
            bitrate_kbps: 600,
            keyframe_interval_seconds: 1,
            video_codec: MediaVideoCodec::H264,
            audio_codec: MediaAudioCodec::Aac,
            audio_sample_rate_hz: 16000,
        };

        let prepared =
            prepare_file_profile(&input, &profile, &cancel, None).expect("合成文件准备失败");
        let prepared_again = prepare_file_profile(&input, &profile, &cancel, None)
            .expect("相同源和 profile 应复用媒体缓存");
        assert_eq!(prepared_again.cache_key(), prepared.cache_key());
        assert_eq!(prepared_again.cache_dir(), prepared.cache_dir());
        assert_eq!(prepared.profile(), &profile);
        assert_eq!(prepared.video_index().first().unwrap().pts_90k, 0);
        assert_eq!(
            prepared.video_index().first().unwrap().codec,
            MediaVideoCodec::H264
        );
        assert!(prepared.video_index().len() >= 10);
        assert_eq!(prepared.audio_codec(), Some(MediaAudioCodec::Aac));
        assert_eq!(prepared.audio_sample_rate_hz(), Some(16000));
        assert!(prepared.duration_90k() > 80_000);
        assert!(prepared
            .video_index()
            .windows(2)
            .all(|pair| { pair[1].pts_90k >= pair[0].pts_90k + pair[0].duration_90k }));

        let mut source = prepared.open_source(false).expect("打开准备结果失败");
        let first = source.next_frame().expect("应有首个视频帧");
        assert!(first.key_frame);
        assert!(!first.data.is_empty());
        assert!(!source.next_audio().is_empty());
        assert!(source.next_frame().is_some());
    }

    #[test]
    fn timed_events_keep_audio_leading_and_trailing_access_units() {
        let prepared = indexed_test_profile(
            &[(100, 100, true), (200, 100, false)],
            &[(0, 100), (100, 100), (300, 100)],
            400,
        );
        let mut source = prepared
            .open_source_unpaced(false)
            .expect("打开测试文件源失败");
        let mut signatures = Vec::new();
        while let Some(event) = source
            .next_timed_media_event()
            .expect("读取 timed 事件失败")
        {
            signatures.push(event_signature(&event));
        }

        assert_eq!(
            signatures,
            vec![
                ("audio", 0),
                ("video", 100),
                ("audio", 100),
                ("video", 200),
                ("audio", 300),
            ]
        );
        remove_indexed_test_profile(&prepared);
    }

    #[test]
    fn timed_events_advance_cycle_only_after_both_tracks_end() {
        let prepared =
            indexed_test_profile(&[(100, 100, true), (200, 100, false)], &[(0, 400)], 400);
        let mut source = prepared
            .open_source_unpaced(true)
            .expect("打开测试文件源失败");
        let signatures = (0..6)
            .map(|_| {
                let event = source
                    .next_timed_media_event()
                    .expect("读取循环 timed 事件失败")
                    .expect("循环源应持续产生事件");
                event_signature(&event)
            })
            .collect::<Vec<_>>();

        assert_eq!(
            signatures,
            vec![
                ("audio", 0),
                ("video", 100),
                ("video", 200),
                ("audio", 400),
                ("video", 500),
                ("video", 600),
            ]
        );
        remove_indexed_test_profile(&prepared);
    }

    #[test]
    fn seek_pairs_audio_with_the_video_keyframe_rollback() {
        let prepared = indexed_test_profile(
            &[
                (0, 100, true),
                (100, 100, false),
                (200, 100, true),
                (300, 100, false),
            ],
            &[(0, 100), (100, 80), (180, 100), (280, 100)],
            400,
        );
        let mut source = prepared
            .open_source_unpaced(false)
            .expect("打开测试文件源失败");
        crate::source::VideoSource::seek(&mut source, 625);
        let first = source
            .next_timed_media_event()
            .expect("seek 后读取首个事件失败")
            .expect("seek 后应有事件");
        let second = source
            .next_timed_media_event()
            .expect("seek 后读取第二个事件失败")
            .expect("seek 后应有第二个事件");

        assert_eq!(
            vec![event_signature(&first), event_signature(&second)],
            vec![("audio", 180), ("video", 200)]
        );
        remove_indexed_test_profile(&prepared);
    }
}
use crate::profile::{MediaAudioCodec, MediaProfile, MediaVideoCodec};
use crate::source::{Frame, VideoSource};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

const CACHE_SCHEMA_VERSION: u32 = 1;
const CACHE_ROOT_ENV: &str = "UVP_MEDIA_PROFILE_CACHE_DIR";
static CACHE_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
static TEMP_NONCE: AtomicU64 = AtomicU64::new(1);

/// 文件准备过程中可以观察到的阶段。阶段事件不代表虚假的百分比。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FilePreparationPhase {
    HashingSource,
    CheckingCache,
    Transcoding,
    Extracting,
    Probing,
    Publishing,
    Complete,
}

/// 文件准备进度。当前实现只报告阶段，`completed`/`total` 预留给可精确测量的步骤。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct FilePreparationProgress {
    pub phase: FilePreparationPhase,
    pub completed: Option<u64>,
    pub total: Option<u64>,
}

/// 准备进度回调。
pub type FilePreparationProgressCallback = dyn Fn(FilePreparationProgress) + Send + Sync;

/// 编码视频访问单元在缓存文件中的位置和媒体时间轴信息。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VideoAccessUnitIndex {
    pub offset: u64,
    pub length: u64,
    pub pts_90k: u64,
    pub duration_90k: u64,
    pub key_frame: bool,
    pub codec: MediaVideoCodec,
}

/// 编码音频访问单元在缓存文件中的位置和媒体时间轴信息。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AudioAccessUnitIndex {
    pub offset: u64,
    pub length: u64,
    pub pts_90k: u64,
    pub duration_90k: u64,
    pub sample_rate_hz: u32,
    pub sample_count: u32,
    pub codec: MediaAudioCodec,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CacheIndex {
    video: Vec<VideoAccessUnitIndex>,
    audio: Vec<AudioAccessUnitIndex>,
    duration_90k: u64,
}

#[derive(Debug, Clone)]
struct RawVideoAccessUnitIndex {
    offset: u64,
    length: u64,
    pts_90k: i64,
    duration_90k: u64,
    key_frame: bool,
    codec: MediaVideoCodec,
}

#[derive(Debug, Clone)]
struct RawAudioAccessUnitIndex {
    offset: u64,
    length: u64,
    pts_90k: i64,
    duration_90k: u64,
    sample_rate_hz: u32,
    sample_count: u32,
    codec: MediaAudioCodec,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CacheManifest {
    cache_schema_version: u32,
    cache_key: String,
    source_sha256: String,
    profile: MediaProfile,
    ffmpeg_identity: String,
    ffprobe_identity: String,
    video_file: String,
    audio_file: Option<String>,
    index_file: String,
}

/// 已完成并通过探测的不可变文件媒体产物。
#[derive(Debug, Clone)]
pub struct PreparedFileProfile {
    cache_key: String,
    cache_dir: PathBuf,
    profile: MediaProfile,
    video_file: PathBuf,
    audio_file: Option<PathBuf>,
    video_index: Vec<VideoAccessUnitIndex>,
    audio_index: Vec<AudioAccessUnitIndex>,
    duration_90k: u64,
}

impl PreparedFileProfile {
    pub fn cache_key(&self) -> &str {
        &self.cache_key
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn profile(&self) -> &MediaProfile {
        &self.profile
    }

    pub fn video_index(&self) -> &[VideoAccessUnitIndex] {
        &self.video_index
    }

    pub fn audio_index(&self) -> &[AudioAccessUnitIndex] {
        &self.audio_index
    }

    pub fn duration_90k(&self) -> u64 {
        self.duration_90k
    }

    pub fn video_codec(&self) -> MediaVideoCodec {
        self.profile.video_codec
    }

    pub fn audio_codec(&self) -> Option<MediaAudioCodec> {
        self.audio_file.as_ref().map(|_| self.profile.audio_codec)
    }

    pub fn audio_sample_rate_hz(&self) -> Option<u32> {
        self.audio_file
            .as_ref()
            .map(|_| self.profile.effective_audio_sample_rate_hz())
    }

    pub fn video_path(&self) -> &Path {
        &self.video_file
    }

    pub fn audio_path(&self) -> Option<&Path> {
        self.audio_file.as_deref()
    }

    /// 打开一个独立的文件源。每个会话持有自己的游标和文件句柄。
    pub fn open_source(&self, looping: bool) -> Result<PreparedFileSource, String> {
        PreparedFileSource::new(self.clone(), looping, true)
    }

    /// 打开不主动等待 PTS 的读取源，供已有外部调度器使用。
    pub fn open_source_unpaced(&self, looping: bool) -> Result<PreparedFileSource, String> {
        PreparedFileSource::new(self.clone(), looping, false)
    }
}

/// 已准备文件的单路读取器。
///
/// `next_media_event` 交给新版共享媒体使用，源自身按文件 PTS 节奏等待；
/// `VideoSource` 旧入口仍保留，便于历史回放和旧调用者逐步迁移。
pub struct PreparedFileSource {
    prepared: PreparedFileProfile,
    video_reader: File,
    audio_reader: Option<File>,
    video_cursor: usize,
    audio_cursor: usize,
    cycle: u64,
    looping: bool,
    paced: bool,
    pace_started: Option<Instant>,
    last_video_window: Option<(u64, u64)>,
    timed_video: Option<PreparedVideoAccessUnit>,
    timed_audio: Option<PreparedAudioAccessUnit>,
    last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PreparedVideoAccessUnit {
    pub data: Vec<u8>,
    pub pts_90k: u64,
    pub duration_90k: u64,
    pub key_frame: bool,
    pub codec: MediaVideoCodec,
}

#[derive(Debug, Clone)]
pub struct PreparedAudioAccessUnit {
    pub data: Vec<u8>,
    pub pts_90k: u64,
    pub duration_90k: u64,
    pub sample_rate_hz: u32,
    pub sample_count: u32,
    pub codec: MediaAudioCodec,
}

impl PreparedFileSource {
    fn new(prepared: PreparedFileProfile, looping: bool, paced: bool) -> Result<Self, String> {
        let video_reader = File::open(&prepared.video_file)
            .map_err(|error| format!("打开缓存视频失败: {error}"))?;
        let audio_reader = prepared
            .audio_file
            .as_ref()
            .map(File::open)
            .transpose()
            .map_err(|error| format!("打开缓存音频失败: {error}"))?;
        Ok(Self {
            prepared,
            video_reader,
            audio_reader,
            video_cursor: 0,
            audio_cursor: 0,
            cycle: 0,
            looping,
            paced,
            pace_started: None,
            last_video_window: None,
            timed_video: None,
            timed_audio: None,
            last_error: None,
        })
    }

    pub fn prepared(&self) -> &PreparedFileProfile {
        &self.prepared
    }

    pub fn set_paced(&mut self, paced: bool) {
        self.paced = paced;
        self.pace_started = None;
    }

    pub fn is_paced(&self) -> bool {
        self.paced
    }

    /// 读取一个带缓存索引的编码视频访问单元。
    pub fn next_video_access_unit(&mut self) -> Result<Option<PreparedVideoAccessUnit>, String> {
        let Some(index) = self.next_video_index() else {
            return Ok(None);
        };
        let cycle_offset = self.cycle.saturating_mul(self.prepared.duration_90k);
        let pts_90k = cycle_offset.saturating_add(index.pts_90k);
        self.wait_for_pts(pts_90k);
        let data = read_range(&mut self.video_reader, index.offset, index.length)?;
        self.last_video_window = Some((pts_90k, index.duration_90k));
        Ok(Some(PreparedVideoAccessUnit {
            data,
            pts_90k,
            duration_90k: index.duration_90k,
            key_frame: index.key_frame,
            codec: index.codec,
        }))
    }

    /// 读取与给定视频时间窗相交的音频访问单元。
    pub fn audio_for_window(
        &mut self,
        video_pts_90k: u64,
        video_duration_90k: u64,
    ) -> Result<Vec<PreparedAudioAccessUnit>, String> {
        let Some(reader) = self.audio_reader.as_mut() else {
            return Ok(Vec::new());
        };
        let window_end = video_pts_90k.saturating_add(video_duration_90k);
        let cycle_offset = self.cycle.saturating_mul(self.prepared.duration_90k);
        let mut output = Vec::new();
        while self.audio_cursor < self.prepared.audio_index.len() {
            let index = &self.prepared.audio_index[self.audio_cursor];
            let pts_90k = cycle_offset.saturating_add(index.pts_90k);
            if pts_90k >= window_end {
                break;
            }
            self.audio_cursor += 1;
            let end_90k = pts_90k.saturating_add(index.duration_90k);
            if end_90k <= video_pts_90k {
                continue;
            }
            let data = read_range(reader, index.offset, index.length)?;
            output.push(PreparedAudioAccessUnit {
                data,
                pts_90k,
                duration_90k: index.duration_90k,
                sample_rate_hz: index.sample_rate_hz,
                sample_count: index.sample_count,
                codec: index.codec,
            });
        }
        Ok(output)
    }

    /// 读取下一个独立媒体事件。
    ///
    /// 视频和音频拥有各自的时间轴。每路只预取一个访问单元，然后按 PTS
    /// 合并；因此音频可以领先视频开始，也可以在最后一个视频访问单元之后
    /// 继续输出。只有两路都读完后，循环源才推进到下一轮。
    pub fn next_timed_media_event(&mut self) -> Result<Option<crate::source::MediaEvent>, String> {
        loop {
            self.fill_timed_video_candidate()?;
            self.fill_timed_audio_candidate()?;

            let choose_audio = match (self.timed_video.as_ref(), self.timed_audio.as_ref()) {
                (Some(video), Some(audio)) => audio.pts_90k < video.pts_90k,
                (None, Some(_)) => true,
                (Some(_), None) => false,
                (None, None) => {
                    if !self.looping {
                        return Ok(None);
                    }
                    self.start_next_timed_cycle();
                    continue;
                }
            };

            if choose_audio {
                let audio = self
                    .timed_audio
                    .take()
                    .expect("audio candidate selected while present");
                self.wait_for_pts(audio.pts_90k);
                return Ok(Some(crate::source::MediaEvent::Audio(
                    crate::source::TimedAudioAu::with_duration(
                        audio.data,
                        audio.codec.into(),
                        audio.sample_rate_hz,
                        audio.sample_count,
                        audio.pts_90k,
                        audio.duration_90k,
                    ),
                )));
            }

            let video = self
                .timed_video
                .take()
                .expect("video candidate selected while present");
            self.wait_for_pts(video.pts_90k);
            self.last_video_window = Some((video.pts_90k, video.duration_90k));
            return Ok(Some(crate::source::MediaEvent::Video(
                crate::source::TimedVideoAu::new(
                    video.data,
                    video.codec.into(),
                    video.key_frame,
                    video.pts_90k,
                    video.duration_90k,
                ),
            )));
        }
    }

    fn fill_timed_video_candidate(&mut self) -> Result<(), String> {
        if self.timed_video.is_some() {
            return Ok(());
        }
        let Some(index) = self.prepared.video_index.get(self.video_cursor).cloned() else {
            return Ok(());
        };
        self.video_cursor += 1;
        let cycle_offset = self.cycle.saturating_mul(self.prepared.duration_90k);
        let pts_90k = cycle_offset.saturating_add(index.pts_90k);
        let data = read_range(&mut self.video_reader, index.offset, index.length)?;
        self.timed_video = Some(PreparedVideoAccessUnit {
            data,
            pts_90k,
            duration_90k: index.duration_90k,
            key_frame: index.key_frame,
            codec: index.codec,
        });
        Ok(())
    }

    fn fill_timed_audio_candidate(&mut self) -> Result<(), String> {
        if self.timed_audio.is_some() {
            return Ok(());
        }
        let Some(index) = self.prepared.audio_index.get(self.audio_cursor).cloned() else {
            return Ok(());
        };
        let Some(reader) = self.audio_reader.as_mut() else {
            return Ok(());
        };
        self.audio_cursor += 1;
        let cycle_offset = self.cycle.saturating_mul(self.prepared.duration_90k);
        let pts_90k = cycle_offset.saturating_add(index.pts_90k);
        let data = read_range(reader, index.offset, index.length)?;
        self.timed_audio = Some(PreparedAudioAccessUnit {
            data,
            pts_90k,
            duration_90k: index.duration_90k,
            sample_rate_hz: index.sample_rate_hz,
            sample_count: index.sample_count,
            codec: index.codec,
        });
        Ok(())
    }

    fn start_next_timed_cycle(&mut self) {
        self.video_cursor = 0;
        self.audio_cursor = 0;
        self.cycle = self.cycle.saturating_add(1);
        self.last_video_window = None;
        self.timed_video = None;
        self.timed_audio = None;
    }

    fn next_video_index(&mut self) -> Option<VideoAccessUnitIndex> {
        if self.prepared.video_index.is_empty() {
            return None;
        }
        if self.video_cursor >= self.prepared.video_index.len() {
            if !self.looping {
                return None;
            }
            self.video_cursor = 0;
            self.audio_cursor = 0;
            self.cycle = self.cycle.saturating_add(1);
            self.last_video_window = None;
        }
        let index = self.prepared.video_index[self.video_cursor].clone();
        self.video_cursor += 1;
        Some(index)
    }

    fn wait_for_pts(&mut self, pts_90k: u64) {
        if !self.paced {
            return;
        }
        let started = *self.pace_started.get_or_insert_with(Instant::now);
        let target = started
            .checked_add(Duration::from_nanos(
                pts_90k.saturating_mul(1_000_000_000) / 90_000,
            ))
            .unwrap_or(started);
        if let Some(remaining) = target.checked_duration_since(Instant::now()) {
            std::thread::sleep(remaining);
        }
    }

    fn seek_to_pts(&mut self, target_90k: u64) {
        self.timed_video = None;
        self.timed_audio = None;
        self.last_video_window = None;
        self.pace_started = None;
        let Some((cursor, _)) = self
            .prepared
            .video_index
            .iter()
            .enumerate()
            .find(|(_, index)| index.pts_90k >= target_90k)
        else {
            self.video_cursor = self.prepared.video_index.len();
            self.audio_cursor = self.prepared.audio_index.len();
            return;
        };
        self.video_cursor = (0..=cursor)
            .rev()
            .find(|&index| self.prepared.video_index[index].key_frame)
            .unwrap_or(cursor);
        let rollback_pts_90k = self.prepared.video_index[self.video_cursor].pts_90k;
        self.audio_cursor = self
            .prepared
            .audio_index
            .iter()
            .position(|index| index.pts_90k.saturating_add(index.duration_90k) > rollback_pts_90k)
            .unwrap_or(self.prepared.audio_index.len());
        self.cycle = 0;
        self.pace_started = if self.paced {
            self.prepared
                .video_index
                .get(self.video_cursor)
                .and_then(|index| {
                    Instant::now().checked_sub(Duration::from_nanos(
                        index.pts_90k.saturating_mul(1_000_000_000) / 90_000,
                    ))
                })
        } else {
            None
        };
    }
}

impl VideoSource for PreparedFileSource {
    fn next_frame(&mut self) -> Option<Frame> {
        match self.next_video_access_unit() {
            Ok(Some(video)) => Some(Frame {
                data: video.data,
                key_frame: video.key_frame,
            }),
            Ok(None) => None,
            Err(error) => {
                self.last_error = Some(error);
                None
            }
        }
    }

    fn next_audio(&mut self) -> Vec<Vec<u8>> {
        let Some((pts_90k, duration_90k)) = self.last_video_window else {
            return Vec::new();
        };
        match self.audio_for_window(pts_90k, duration_90k) {
            Ok(audio) => audio.into_iter().map(|audio| audio.data).collect(),
            Err(error) => {
                self.last_error = Some(error);
                Vec::new()
            }
        }
    }

    fn supports_timed_events(&self) -> bool {
        true
    }

    fn timed_events_are_paced(&self) -> bool {
        self.paced
    }

    fn next_media_event(&mut self) -> Option<crate::source::MediaEvent> {
        match self.next_timed_media_event() {
            Ok(event) => event,
            Err(error) => {
                self.last_error = Some(error);
                None
            }
        }
    }

    fn audio_sample_rate_hz(&self) -> u32 {
        self.prepared
            .audio_sample_rate_hz()
            .unwrap_or_else(|| self.prepared.profile().effective_audio_sample_rate_hz())
    }

    fn has_audio(&self) -> bool {
        self.prepared.audio_file.is_some()
    }

    fn audio_codec(&self) -> crate::ps::AudioCodec {
        self.prepared.profile.audio_codec.into()
    }

    fn codec(&self) -> crate::ps::VideoCodec {
        self.prepared.profile.video_codec.into()
    }

    fn seek(&mut self, permille: u32) {
        let target = self
            .prepared
            .duration_90k
            .saturating_mul(u64::from(permille.min(1000)))
            / 1000;
        self.seek_to_pts(target);
    }

    fn take_error(&mut self) -> Option<String> {
        self.last_error.take()
    }
}

impl From<MediaVideoCodec> for crate::ps::VideoCodec {
    fn from(codec: MediaVideoCodec) -> Self {
        match codec {
            MediaVideoCodec::H264 => Self::H264,
            MediaVideoCodec::H265 => Self::H265,
        }
    }
}

impl From<MediaAudioCodec> for crate::ps::AudioCodec {
    fn from(codec: MediaAudioCodec) -> Self {
        match codec {
            MediaAudioCodec::G711A => Self::G711A,
            MediaAudioCodec::G711U => Self::G711U,
            MediaAudioCodec::Aac => Self::Aac,
        }
    }
}

/// 按完整 profile 准备文件媒体产物。
///
/// 该函数同步执行准备，但每个外部步骤之间以及 ffmpeg 运行期间都会检查
/// `cancel`。调用方应在注册前从独立线程/任务调用它，不能在 INVITE 处理路径同步转码。
pub fn prepare_file_profile(
    path: &Path,
    profile: &MediaProfile,
    cancel: &AtomicBool,
    progress: Option<&FilePreparationProgressCallback>,
) -> Result<PreparedFileProfile, String> {
    check_cancel(cancel)?;
    profile.validate()?;
    if !path.is_file() {
        return Err(format!("文件源不存在或不是普通文件: {}", path.display()));
    }

    report(progress, FilePreparationPhase::HashingSource);
    let source_snapshot = SourceSnapshot::capture(path, cancel)?;
    let ffmpeg = crate::source::ffmpeg_bin()
        .ok_or_else(|| "未找到 ffmpeg，无法准备文件媒体 profile".to_string())?;
    let ffprobe = ffprobe_bin()
        .ok_or_else(|| "未找到 ffprobe，无法生成文件媒体访问单元时间索引".to_string())?;
    let ffmpeg_identity = tool_identity(&ffmpeg)?;
    let ffprobe_identity = tool_identity(&ffprobe)?;
    let cache_key = cache_key_for(
        &source_snapshot.sha256,
        profile,
        &ffmpeg_identity,
        &ffprobe_identity,
    );

    report(progress, FilePreparationPhase::CheckingCache);
    let cache_root = cache_root();
    fs::create_dir_all(&cache_root).map_err(|error| format!("创建媒体缓存目录失败: {error}"))?;
    let key_lock = cache_lock(&cache_key);
    let _key_guard = lock_cache_with_cancel(&key_lock, cancel)?;
    let final_dir = cache_root.join(&cache_key);
    if final_dir.is_dir() {
        check_cancel(cancel)?;
        if let Ok(prepared) =
            load_cached_profile(&final_dir, &cache_key, profile, &source_snapshot.sha256)
        {
            check_cancel(cancel)?;
            report(progress, FilePreparationPhase::Complete);
            return Ok(prepared);
        }
        check_cancel(cancel)?;
        fs::remove_dir_all(&final_dir)
            .map_err(|error| format!("移除损坏的媒体缓存失败: {error}"))?;
    }

    check_cancel(cancel)?;
    let temp_dir = cache_root.join(format!(
        ".{cache_key}.tmp-{}-{}",
        std::process::id(),
        TEMP_NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&temp_dir).map_err(|error| format!("创建媒体缓存临时目录失败: {error}"))?;
    let result = prepare_uncached(
        path,
        profile,
        cancel,
        progress,
        &ffmpeg,
        &ffprobe,
        &source_snapshot,
        &cache_key,
        &ffmpeg_identity,
        &ffprobe_identity,
        &temp_dir,
    );
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(error);
    }

    report(progress, FilePreparationPhase::Publishing);
    check_cancel(cancel).inspect_err(|_| {
        let _ = fs::remove_dir_all(&temp_dir);
    })?;
    match fs::rename(&temp_dir, &final_dir) {
        Ok(()) => {}
        Err(error) if final_dir.is_dir() => {
            let _ = fs::remove_dir_all(&temp_dir);
            if !final_dir.is_dir() {
                return Err(format!("媒体缓存并发发布后目录消失: {error}"));
            }
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(format!("原子发布媒体缓存失败: {error}"));
        }
    }
    let prepared = load_cached_profile(&final_dir, &cache_key, profile, &source_snapshot.sha256)?;
    report(progress, FilePreparationPhase::Complete);
    Ok(prepared)
}

fn prepare_uncached(
    source: &Path,
    profile: &MediaProfile,
    cancel: &AtomicBool,
    progress: Option<&FilePreparationProgressCallback>,
    ffmpeg: &str,
    ffprobe: &str,
    source_snapshot: &SourceSnapshot,
    cache_key: &str,
    ffmpeg_identity: &str,
    ffprobe_identity: &str,
    temp_dir: &Path,
) -> Result<(), String> {
    report(progress, FilePreparationPhase::Transcoding);
    let source_copy = copy_source_snapshot(source, source_snapshot, cancel, temp_dir)?;
    let source_has_audio = has_audio_stream(ffprobe, &source_copy, cancel)?;
    let container = temp_dir.join("media.mkv");
    transcode_source(ffmpeg, &source_copy, profile, &container, cancel)?;
    check_source_unchanged(source, source_snapshot, cancel)?;

    report(progress, FilePreparationPhase::Extracting);
    let video_file = temp_dir.join(video_filename(profile.video_codec));
    extract_video(ffmpeg, &container, profile.video_codec, &video_file, cancel)?;
    let output_has_audio = has_audio_stream(ffprobe, &container, cancel)?;
    if source_has_audio && !output_has_audio {
        return Err("源文件包含音频轨，但转码结果缺少音频轨".into());
    }
    let audio_file = if output_has_audio {
        let audio_file = temp_dir.join(audio_filename(profile.audio_codec));
        extract_audio(ffmpeg, &container, profile.audio_codec, &audio_file, cancel)?;
        Some(audio_file)
    } else {
        None
    };

    report(progress, FilePreparationPhase::Probing);
    let raw_video_index = build_video_index(ffprobe, &container, &video_file, profile, cancel)?;
    let raw_audio_index = match &audio_file {
        Some(audio_file) => build_audio_index(ffprobe, &container, audio_file, profile, cancel)?,
        None => Vec::new(),
    };
    if raw_video_index.is_empty() {
        return Err("转码结果不包含视频访问单元".into());
    }
    let timeline_origin = raw_video_index
        .iter()
        .map(|index| index.pts_90k)
        .chain(raw_audio_index.iter().map(|index| index.pts_90k))
        .min()
        .unwrap_or(0);
    let video_index = raw_video_index
        .into_iter()
        .map(|index| VideoAccessUnitIndex {
            offset: index.offset,
            length: index.length,
            pts_90k: normalize_pts(index.pts_90k, timeline_origin),
            duration_90k: index.duration_90k,
            key_frame: index.key_frame,
            codec: index.codec,
        })
        .collect::<Vec<_>>();
    let audio_index = raw_audio_index
        .into_iter()
        .map(|index| AudioAccessUnitIndex {
            offset: index.offset,
            length: index.length,
            pts_90k: normalize_pts(index.pts_90k, timeline_origin),
            duration_90k: index.duration_90k,
            sample_rate_hz: index.sample_rate_hz,
            sample_count: index.sample_count,
            codec: index.codec,
        })
        .collect::<Vec<_>>();
    let duration_90k = media_duration(&video_index, &audio_index);
    if duration_90k == 0 {
        return Err("转码结果没有有效媒体时长".into());
    }
    verify_output_stream(ffprobe, &container, profile, audio_file.is_some(), cancel)?;
    check_source_unchanged(source, source_snapshot, cancel)?;

    let index = CacheIndex {
        video: video_index,
        audio: audio_index,
        duration_90k,
    };
    let index_file = temp_dir.join("index.json");
    write_json_synced(&index_file, &index)?;
    let manifest = CacheManifest {
        cache_schema_version: CACHE_SCHEMA_VERSION,
        cache_key: cache_key.to_string(),
        source_sha256: source_snapshot.sha256.clone(),
        profile: profile.clone(),
        ffmpeg_identity: ffmpeg_identity.to_string(),
        ffprobe_identity: ffprobe_identity.to_string(),
        video_file: video_file
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "缓存视频文件名非 UTF-8".to_string())?
            .to_string(),
        audio_file: audio_file
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .map(str::to_string),
        index_file: "index.json".into(),
    };
    write_json_synced(&temp_dir.join("manifest.json"), &manifest)?;
    fs::remove_file(&source_copy)
        .map_err(|error| format!("删除媒体缓存临时源文件失败: {error}"))?;
    Ok(())
}

fn load_cached_profile(
    cache_dir: &Path,
    cache_key: &str,
    expected_profile: &MediaProfile,
    expected_source_sha256: &str,
) -> Result<PreparedFileProfile, String> {
    let manifest: CacheManifest = read_json(&cache_dir.join("manifest.json"))?;
    if manifest.cache_schema_version != CACHE_SCHEMA_VERSION {
        return Err("媒体缓存 schema 版本不匹配".into());
    }
    if manifest.cache_key != cache_key
        || manifest.profile != *expected_profile
        || manifest.source_sha256 != expected_source_sha256
    {
        return Err("媒体缓存 profile 或 key 不匹配".into());
    }
    let index: CacheIndex = read_json(&cache_dir.join(&manifest.index_file))?;
    let video_file = cache_dir.join(&manifest.video_file);
    if !video_file.is_file() {
        return Err("媒体缓存缺少视频文件".into());
    }
    let audio_file = manifest.audio_file.map(|file| cache_dir.join(file));
    if audio_file.as_ref().is_some_and(|path| !path.is_file()) {
        return Err("媒体缓存缺少音频文件".into());
    }
    if index.video.is_empty() || index.duration_90k == 0 {
        return Err("媒体缓存索引为空或时长无效".into());
    }
    Ok(PreparedFileProfile {
        cache_key: cache_key.to_string(),
        cache_dir: cache_dir.to_path_buf(),
        profile: manifest.profile,
        video_file,
        audio_file,
        video_index: index.video,
        audio_index: index.audio,
        duration_90k: index.duration_90k,
    })
}

fn transcode_source(
    ffmpeg: &str,
    source: &Path,
    profile: &MediaProfile,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let gop_frames = profile
        .video_fps
        .checked_mul(profile.keyframe_interval_seconds)
        .ok_or_else(|| "GOP 帧数溢出".to_string())?;
    let filter = format!(
        "scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2:color=black,fps={}",
        profile.width, profile.height, profile.width, profile.height, profile.video_fps
    );
    let video_encoder = match profile.video_codec {
        MediaVideoCodec::H264 => "libx264",
        MediaVideoCodec::H265 => "libx265",
    };
    let mut command = Command::new(ffmpeg);
    command.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]);
    command.arg(source);
    command.args([
        "-map",
        "0:v:0",
        "-map",
        "0:a:0?",
        "-vf",
        &filter,
        "-fps_mode",
        "cfr",
        "-c:v",
        video_encoder,
        "-b:v",
    ]);
    command.arg(format!("{}k", profile.bitrate_kbps));
    command.args(["-g", &gop_frames.to_string(), "-keyint_min"]);
    command.arg(gop_frames.to_string());
    command.args(["-sc_threshold", "0", "-bf", "0"]);
    match profile.video_codec {
        MediaVideoCodec::H264 => {
            command.args([
                "-x264-params",
                &format!("keyint={gop_frames}:min-keyint={gop_frames}:scenecut=0:aud=1"),
            ]);
        }
        MediaVideoCodec::H265 => {
            command.args([
                "-x265-params",
                &format!("keyint={gop_frames}:min-keyint={gop_frames}:scenecut=0:aud=1"),
            ]);
        }
    }
    append_audio_args(&mut command, profile);
    command.args(["-avoid_negative_ts", "disabled", "-f", "matroska"]);
    command.arg(output);
    run_cancellable(command, cancel, "ffmpeg 文件转码")
}

fn append_audio_args(command: &mut Command, profile: &MediaProfile) {
    match profile.audio_codec {
        MediaAudioCodec::G711A => command.args(["-c:a", "pcm_alaw", "-ar", "8000", "-ac", "1"]),
        MediaAudioCodec::G711U => command.args(["-c:a", "pcm_mulaw", "-ar", "8000", "-ac", "1"]),
        MediaAudioCodec::Aac => command.args([
            "-c:a",
            "aac",
            "-ar",
            &profile.effective_audio_sample_rate_hz().to_string(),
            "-ac",
            "1",
            "-b:a",
            "32k",
        ]),
    };
}

fn extract_video(
    ffmpeg: &str,
    container: &Path,
    codec: MediaVideoCodec,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let (format, bitstream_filter) = match codec {
        MediaVideoCodec::H264 => ("h264", "h264_mp4toannexb"),
        MediaVideoCodec::H265 => ("hevc", "hevc_mp4toannexb"),
    };
    let mut command = Command::new(ffmpeg);
    command.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]);
    command.arg(container);
    command.args([
        "-map",
        "0:v:0",
        "-c:v",
        "copy",
        "-bsf:v",
        bitstream_filter,
        "-f",
        format,
    ]);
    command.arg(output);
    run_cancellable(command, cancel, "ffmpeg 提取视频访问单元")
}

fn extract_audio(
    ffmpeg: &str,
    container: &Path,
    codec: MediaAudioCodec,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let (format, expected_codec) = match codec {
        MediaAudioCodec::G711A => ("alaw", "pcm_alaw"),
        MediaAudioCodec::G711U => ("mulaw", "pcm_mulaw"),
        MediaAudioCodec::Aac => ("adts", "aac"),
    };
    let mut command = Command::new(ffmpeg);
    command.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]);
    command.arg(container);
    command.args(["-map", "0:a:0", "-c:a", "copy", "-f", format]);
    command.arg(output);
    run_cancellable(command, cancel, "ffmpeg 提取音频访问单元")
        .map_err(|error| format!("提取 {expected_codec} 音频失败: {error}"))
}

fn run_cancellable(
    mut command: Command,
    cancel: &AtomicBool,
    operation: &str,
) -> Result<(), String> {
    check_cancel(cancel)?;
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("{operation}启动失败: {error}"))?;
    loop {
        if cancel.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{operation}已取消"));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child
                    .wait_with_output()
                    .map_err(|error| format!("{operation}读取结果失败: {error}"))?;
                if status.success() {
                    return Ok(());
                }
                return Err(format!("{operation}失败: {}", last_stderr_line(&output)));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{operation}等待失败: {error}"));
            }
        }
    }
}

fn last_stderr_line(output: &Output) -> String {
    last_stderr_line_bytes(&output.stderr)
}

fn last_stderr_line_bytes(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .last()
        .unwrap_or("未知错误")
        .trim()
        .to_string()
}

fn has_audio_stream(ffprobe: &str, container: &Path, cancel: &AtomicBool) -> Result<bool, String> {
    let value = run_probe(
        ffprobe,
        [
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_type",
            "-of",
            "json",
        ],
        container,
        cancel,
    )?;
    Ok(value
        .get("streams")
        .and_then(Value::as_array)
        .is_some_and(|streams| !streams.is_empty()))
}

fn verify_output_stream(
    ffprobe: &str,
    container: &Path,
    profile: &MediaProfile,
    has_audio: bool,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let video_probe = run_probe(
        ffprobe,
        [
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,width,height,r_frame_rate,bit_rate",
            "-of",
            "json",
        ],
        container,
        cancel,
    )?;
    let video = first_stream(&video_probe, "video")?;
    let expected_video_codec = match profile.video_codec {
        MediaVideoCodec::H264 => "h264",
        MediaVideoCodec::H265 => "hevc",
    };
    let actual_video_codec = video
        .get("codec_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if actual_video_codec != expected_video_codec {
        return Err(format!(
            "视频转码结果编码不匹配: 期望 {expected_video_codec}, 实际 {actual_video_codec}"
        ));
    }
    let actual_width = json_u64(video.get("width")).unwrap_or_default();
    let actual_height = json_u64(video.get("height")).unwrap_or_default();
    if (actual_width, actual_height) != (u64::from(profile.width), u64::from(profile.height)) {
        return Err(format!(
            "视频转码结果尺寸不匹配: 期望 {}x{}, 实际 {actual_width}x{actual_height}",
            profile.width, profile.height
        ));
    }
    let actual_frame_rate = video
        .get("r_frame_rate")
        .and_then(Value::as_str)
        .and_then(parse_frame_rate)
        .ok_or_else(|| "视频转码结果缺少可验证的帧率".to_string())?;
    if actual_frame_rate.0 != u64::from(profile.video_fps) * actual_frame_rate.1 {
        return Err(format!(
            "视频转码结果帧率不匹配: 期望 {} fps, 实际 {}/{} fps",
            profile.video_fps, actual_frame_rate.0, actual_frame_rate.1
        ));
    }
    if has_audio {
        let audio_probe = run_probe(
            ffprobe,
            [
                "-select_streams",
                "a:0",
                "-show_entries",
                "stream=codec_name,sample_rate,channels",
                "-of",
                "json",
            ],
            container,
            cancel,
        )?;
        let audio = first_stream(&audio_probe, "audio")?;
        let expected_audio_codec = match profile.audio_codec {
            MediaAudioCodec::G711A => "pcm_alaw",
            MediaAudioCodec::G711U => "pcm_mulaw",
            MediaAudioCodec::Aac => "aac",
        };
        let actual_audio_codec = audio
            .get("codec_name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if actual_audio_codec != expected_audio_codec {
            return Err(format!(
                "音频转码结果编码不匹配: 期望 {expected_audio_codec}, 实际 {actual_audio_codec}"
            ));
        }
        let actual_sample_rate = json_u64(audio.get("sample_rate")).unwrap_or_default();
        let expected_sample_rate = u64::from(profile.effective_audio_sample_rate_hz());
        if actual_sample_rate != expected_sample_rate {
            return Err(format!(
                "音频转码结果采样率不匹配: 期望 {expected_sample_rate}, 实际 {actual_sample_rate}"
            ));
        }
        let channels = json_u64(audio.get("channels")).unwrap_or_default();
        if channels != 1 {
            return Err(format!("音频转码结果不是单声道: {channels}"));
        }
    }
    Ok(())
}

fn build_video_index(
    ffprobe: &str,
    container: &Path,
    elementary: &Path,
    profile: &MediaProfile,
    cancel: &AtomicBool,
) -> Result<Vec<RawVideoAccessUnitIndex>, String> {
    let frames = run_probe(
        ffprobe,
        [
            "-select_streams",
            "v:0",
            "-show_frames",
            "-show_entries",
            "frame=best_effort_timestamp_time,pkt_duration_time,key_frame",
            "-of",
            "json",
        ],
        container,
        cancel,
    )?;
    let packets = run_probe(
        ffprobe,
        [
            "-f",
            raw_video_format(profile.video_codec),
            "-framerate",
            &profile.video_fps.to_string(),
            "-show_packets",
            "-show_entries",
            "packet=pos,size,flags",
            "-of",
            "json",
        ],
        elementary,
        cancel,
    )?;
    let frame_values = array_values(&frames, "frames")?;
    let packet_values = array_values(&packets, "packets")?;
    if frame_values.len() != packet_values.len() {
        return Err(format!(
            "视频 ffprobe 帧索引与裸流包索引数量不一致: {} != {}",
            frame_values.len(),
            packet_values.len()
        ));
    }
    let fallback_duration = duration_for_rate(profile.video_fps);
    let raw_pts: Vec<i64> = frame_values
        .iter()
        .enumerate()
        .map(|(index, frame)| {
            json_time_90k(frame.get("best_effort_timestamp_time")).unwrap_or_else(|| {
                i64::try_from(index).unwrap_or(i64::MAX) * fallback_duration as i64
            })
        })
        .collect();
    frame_values
        .iter()
        .zip(packet_values.iter())
        .enumerate()
        .map(|(index, (frame, packet))| {
            let offset = json_u64(packet.get("pos"))
                .ok_or_else(|| format!("视频包 {index} 缺少裸流偏移"))?;
            let length = json_u64(packet.get("size"))
                .ok_or_else(|| format!("视频包 {index} 缺少裸流长度"))?;
            let duration = frame
                .get("pkt_duration_time")
                .and_then(|value| json_time_90k(Some(value)))
                .filter(|value| *value > 0)
                .or_else(|| raw_pts.get(index + 1).map(|next| next - raw_pts[index]))
                .filter(|value| *value > 0)
                .map(|value| value as u64)
                .unwrap_or(fallback_duration);
            let key_frame = json_bool(frame.get("key_frame"))
                .or_else(|| {
                    packet
                        .get("flags")
                        .and_then(Value::as_str)
                        .map(|flags| flags.starts_with('K'))
                })
                .unwrap_or(false);
            Ok(RawVideoAccessUnitIndex {
                offset,
                length,
                pts_90k: raw_pts[index],
                duration_90k: duration,
                key_frame,
                codec: profile.video_codec,
            })
        })
        .collect()
}

fn build_audio_index(
    ffprobe: &str,
    container: &Path,
    elementary: &Path,
    profile: &MediaProfile,
    cancel: &AtomicBool,
) -> Result<Vec<RawAudioAccessUnitIndex>, String> {
    let frames = run_probe(
        ffprobe,
        [
            "-select_streams",
            "a:0",
            "-show_frames",
            "-show_entries",
            "frame=best_effort_timestamp_time,pkt_duration_time,nb_samples",
            "-of",
            "json",
        ],
        container,
        cancel,
    )?;
    let container_packets = run_probe(
        ffprobe,
        [
            "-select_streams",
            "a:0",
            "-show_packets",
            "-show_entries",
            "packet=pts_time,duration_time",
            "-of",
            "json",
        ],
        container,
        cancel,
    )?;
    let packets = run_probe(
        ffprobe,
        [
            "-f",
            raw_audio_format(profile.audio_codec),
            "-ar",
            &profile.effective_audio_sample_rate_hz().to_string(),
            "-show_packets",
            "-show_entries",
            "packet=pos,size",
            "-of",
            "json",
        ],
        elementary,
        cancel,
    )?;
    let frame_values = array_values(&frames, "frames")?;
    let container_packet_values = array_values(&container_packets, "packets")?;
    let packet_values = array_values(&packets, "packets")?;
    if frame_values.is_empty() != packet_values.is_empty() {
        return Err(format!(
            "音频 ffprobe 帧索引与裸流包索引为空状态不一致: {} != {}",
            frame_values.len(),
            packet_values.len()
        ));
    }
    if frame_values.is_empty() {
        return Err("转码结果音频流没有可读取的访问单元".into());
    }
    if !matches!(
        profile.audio_codec,
        MediaAudioCodec::G711A | MediaAudioCodec::G711U
    ) && frame_values.len() > packet_values.len()
    {
        return Err(format!(
            "音频 ffprobe 帧索引多于裸流包索引: {} > {}",
            frame_values.len(),
            packet_values.len()
        ));
    }
    let sample_rate = profile.effective_audio_sample_rate_hz();
    let raw_pts: Vec<i64> = frame_values
        .iter()
        .enumerate()
        .map(|(index, frame)| {
            json_time_90k(frame.get("best_effort_timestamp_time")).unwrap_or_else(|| {
                i64::try_from(index).unwrap_or(i64::MAX)
                    * i64::from(default_audio_samples(profile.audio_codec))
                    * 90_000
                    / i64::from(sample_rate)
            })
        })
        .collect();
    let first_frame_pts = raw_pts.first().copied().unwrap_or(0);
    if matches!(
        profile.audio_codec,
        MediaAudioCodec::G711A | MediaAudioCodec::G711U
    ) {
        let raw_length = fs::metadata(elementary)
            .map_err(|error| format!("读取裸 G.711 音频长度失败: {error}"))?
            .len();
        let mut timeline_pts = raw_pts.first().copied().unwrap_or(0);
        let mut index = Vec::with_capacity(packet_values.len());
        for (packet_index, packet) in packet_values.iter().enumerate() {
            let offset = json_u64(packet.get("pos"))
                .ok_or_else(|| format!("G.711 包 {packet_index} 缺少裸流偏移"))?;
            let length = json_u64(packet.get("size"))
                .ok_or_else(|| format!("G.711 包 {packet_index} 缺少裸流长度"))?;
            let sample_count = u32::try_from(length)
                .map_err(|_| format!("G.711 包 {packet_index} 样本数超出范围"))?;
            if length == 0 || offset.saturating_add(length) > raw_length {
                return Err(format!(
                    "G.711 裸流包 {packet_index} 越过文件边界: 结束 {}, 文件长度 {raw_length}",
                    offset.saturating_add(length)
                ));
            }
            index.push(RawAudioAccessUnitIndex {
                offset,
                length,
                pts_90k: timeline_pts,
                duration_90k: duration_for_samples(sample_count, sample_rate),
                sample_rate_hz: sample_rate,
                sample_count,
                codec: profile.audio_codec,
            });
            timeline_pts = timeline_pts.saturating_add(
                i64::try_from(duration_for_samples(sample_count, sample_rate)).unwrap_or(i64::MAX),
            );
        }
        return Ok(index);
    }
    let leading_packets = container_packet_values
        .iter()
        .take_while(|packet| {
            json_time_90k(packet.get("pts_time")).is_some_and(|pts| pts < first_frame_pts)
        })
        .count();
    if leading_packets.saturating_add(frame_values.len()) > packet_values.len() {
        return Err(format!(
            "音频裸流包无法覆盖容器帧索引: 起始偏移 {leading_packets}, 帧数 {}, 裸流包数 {}",
            frame_values.len(),
            packet_values.len()
        ));
    }
    frame_values
        .iter()
        .enumerate()
        .map(|(index, frame)| {
            let packet = &packet_values[index + leading_packets];
            let offset = json_u64(packet.get("pos"))
                .ok_or_else(|| format!("音频包 {index} 缺少裸流偏移"))?;
            let length = json_u64(packet.get("size"))
                .ok_or_else(|| format!("音频包 {index} 缺少裸流长度"))?;
            let sample_count = json_u64(frame.get("nb_samples"))
                .map(|samples| u32::try_from(samples).unwrap_or(u32::MAX))
                .filter(|samples| *samples > 0)
                .unwrap_or_else(|| default_audio_samples(profile.audio_codec));
            let duration = duration_for_samples(sample_count, sample_rate);
            Ok(RawAudioAccessUnitIndex {
                offset,
                length,
                pts_90k: raw_pts[index],
                duration_90k: duration,
                sample_rate_hz: sample_rate,
                sample_count,
                codec: profile.audio_codec,
            })
        })
        .collect()
}

fn run_probe<const N: usize>(
    ffprobe: &str,
    args: [&str; N],
    input: &Path,
    cancel: &AtomicBool,
) -> Result<Value, String> {
    check_cancel(cancel)?;
    let mut child = Command::new(ffprobe)
        .args(args)
        .arg(input)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("启动 ffprobe 失败: {error}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ffprobe 标准输出不可用".to_string())?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "ffprobe 标准错误不可用".to_string())?;
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let status = loop {
        if cancel.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err("ffprobe 已取消".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(format!("ffprobe 等待失败: {error}"));
            }
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "读取 ffprobe 标准输出线程异常".to_string())?
        .map_err(|error| format!("读取 ffprobe 标准输出失败: {error}"))?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "读取 ffprobe 标准错误线程异常".to_string())?
        .map_err(|error| format!("读取 ffprobe 标准错误失败: {error}"))?;
    check_cancel(cancel)?;
    if !status.success() {
        return Err(format!("ffprobe 失败: {}", last_stderr_line_bytes(&stderr)));
    }
    serde_json::from_slice(&stdout).map_err(|error| format!("解析 ffprobe JSON 失败: {error}"))
}

fn array_values<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("ffprobe JSON 缺少 {key} 数组"))
}

fn first_stream<'a>(value: &'a Value, kind: &str) -> Result<&'a Value, String> {
    value
        .get("streams")
        .and_then(Value::as_array)
        .and_then(|streams| streams.first())
        .ok_or_else(|| format!("ffprobe 未找到 {kind} 流"))
}

fn json_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
    })
}

fn json_bool(value: Option<&Value>) -> Option<bool> {
    value.and_then(|value| {
        value.as_bool().or_else(|| {
            value.as_u64().map(|number| number != 0).or_else(|| {
                value
                    .as_str()
                    .and_then(|text| text.parse::<u8>().ok())
                    .map(|n| n != 0)
            })
        })
    })
}

fn json_time_90k(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_str).and_then(parse_time_90k)
}

fn parse_time_90k(value: &str) -> Option<i64> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("n/a") {
        return None;
    }
    let (negative, unsigned) = match value.strip_prefix('-') {
        Some(value) => (true, value),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let whole = whole.parse::<i128>().ok()?;
    let fraction_digits = fraction.chars().take(12).collect::<String>();
    let denominator = 10_i128.checked_pow(fraction_digits.len() as u32)?;
    let fraction = if fraction_digits.is_empty() {
        0
    } else {
        fraction_digits.parse::<i128>().ok()?
    };
    let numerator = whole.checked_mul(denominator)?.checked_add(fraction)?;
    let ticks_numerator = numerator.checked_mul(90_000)?;
    let rounded = (ticks_numerator + denominator / 2) / denominator;
    let rounded = i64::try_from(rounded).ok()?;
    Some(if negative { -rounded } else { rounded })
}

fn parse_frame_rate(value: &str) -> Option<(u64, u64)> {
    let (numerator, denominator) = value.trim().split_once('/')?;
    let numerator = numerator.parse::<u64>().ok()?;
    let denominator = denominator.parse::<u64>().ok()?;
    (denominator > 0).then_some((numerator, denominator))
}

fn duration_for_rate(fps: u32) -> u64 {
    (90_000_u64 / u64::from(fps.max(1))).max(1)
}

fn duration_for_samples(sample_count: u32, sample_rate_hz: u32) -> u64 {
    if sample_rate_hz == 0 {
        return 0;
    }
    ((u64::from(sample_count) * 90_000) / u64::from(sample_rate_hz)).max(1)
}

fn default_audio_samples(codec: MediaAudioCodec) -> u32 {
    match codec {
        MediaAudioCodec::Aac => 1024,
        MediaAudioCodec::G711A | MediaAudioCodec::G711U => 160,
    }
}

fn normalize_pts(pts_90k: i64, origin_90k: i64) -> u64 {
    pts_90k
        .saturating_sub(origin_90k)
        .max(0)
        .try_into()
        .unwrap_or(u64::MAX)
}

fn media_duration(video: &[VideoAccessUnitIndex], audio: &[AudioAccessUnitIndex]) -> u64 {
    video
        .iter()
        .map(|index| index.pts_90k.saturating_add(index.duration_90k))
        .chain(
            audio
                .iter()
                .map(|index| index.pts_90k.saturating_add(index.duration_90k)),
        )
        .max()
        .unwrap_or(0)
}

fn raw_video_format(codec: MediaVideoCodec) -> &'static str {
    match codec {
        MediaVideoCodec::H264 => "h264",
        MediaVideoCodec::H265 => "hevc",
    }
}

fn raw_audio_format(codec: MediaAudioCodec) -> &'static str {
    match codec {
        MediaAudioCodec::G711A => "alaw",
        MediaAudioCodec::G711U => "mulaw",
        MediaAudioCodec::Aac => "aac",
    }
}

fn video_filename(codec: MediaVideoCodec) -> &'static str {
    match codec {
        MediaVideoCodec::H264 => "video.h264",
        MediaVideoCodec::H265 => "video.h265",
    }
}

fn audio_filename(codec: MediaAudioCodec) -> &'static str {
    match codec {
        MediaAudioCodec::G711A => "audio.g711a",
        MediaAudioCodec::G711U => "audio.g711u",
        MediaAudioCodec::Aac => "audio.aac",
    }
}

fn read_range(reader: &mut File, offset: u64, length: u64) -> Result<Vec<u8>, String> {
    let length =
        usize::try_from(length).map_err(|_| "媒体访问单元长度超过内存寻址范围".to_string())?;
    reader
        .seek(SeekFrom::Start(offset))
        .map_err(|error| format!("定位媒体缓存失败: {error}"))?;
    let mut data = vec![0_u8; length];
    reader
        .read_exact(&mut data)
        .map_err(|error| format!("读取媒体缓存失败: {error}"))?;
    Ok(data)
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err("文件媒体准备已取消".into())
    } else {
        Ok(())
    }
}

fn report(callback: Option<&FilePreparationProgressCallback>, phase: FilePreparationPhase) {
    if let Some(callback) = callback {
        callback(FilePreparationProgress {
            phase,
            completed: None,
            total: None,
        });
    }
}

#[derive(Debug, Clone)]
struct SourceSnapshot {
    length: u64,
    modified: Option<SystemTime>,
    sha256: String,
}

impl SourceSnapshot {
    fn capture(path: &Path, cancel: &AtomicBool) -> Result<Self, String> {
        let metadata =
            fs::metadata(path).map_err(|error| format!("读取源文件信息失败: {error}"))?;
        let sha256 = sha256_file(path, cancel)?;
        let after_hash =
            fs::metadata(path).map_err(|error| format!("读取源文件信息失败: {error}"))?;
        if metadata.len() != after_hash.len()
            || metadata.modified().ok() != after_hash.modified().ok()
        {
            return Err("源文件在建立快照期间发生变化".into());
        }
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            sha256,
        })
    }
}

fn copy_source_snapshot(
    source: &Path,
    snapshot: &SourceSnapshot,
    cancel: &AtomicBool,
    temp_dir: &Path,
) -> Result<PathBuf, String> {
    let extension = source
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty());
    let copy_name = extension
        .map(|extension| format!("source-input.{extension}"))
        .unwrap_or_else(|| "source-input".to_string());
    let copy_path = temp_dir.join(copy_name);
    let mut input = File::open(source).map_err(|error| format!("打开源文件副本失败: {error}"))?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&copy_path)
        .map_err(|error| format!("创建源文件副本失败: {error}"))?;
    let mut digest = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        check_cancel(cancel)?;
        let count = input
            .read(&mut buffer)
            .map_err(|error| format!("读取源文件副本失败: {error}"))?;
        if count == 0 {
            break;
        }
        copied = copied.saturating_add(count as u64);
        if copied > snapshot.length {
            return Err("源文件在复制快照期间增长，已放弃媒体准备".into());
        }
        output
            .write_all(&buffer[..count])
            .map_err(|error| format!("写入源文件副本失败: {error}"))?;
        digest.update(&buffer[..count]);
    }
    if copied != snapshot.length {
        return Err(format!(
            "源文件在复制快照期间变化: 期望 {} 字节，实际 {copied} 字节",
            snapshot.length
        ));
    }
    let copied_sha256 = hex_encode(&digest.finalize());
    if copied_sha256 != snapshot.sha256 {
        return Err("源文件内容在复制快照期间变化，已放弃媒体准备".into());
    }
    output
        .sync_all()
        .map_err(|error| format!("同步源文件副本失败: {error}"))?;
    Ok(copy_path)
}

fn check_source_unchanged(
    path: &Path,
    snapshot: &SourceSnapshot,
    cancel: &AtomicBool,
) -> Result<(), String> {
    check_cancel(cancel)?;
    let metadata =
        fs::metadata(path).map_err(|error| format!("源文件在准备期间不可读: {error}"))?;
    if metadata.len() != snapshot.length || metadata.modified().ok() != snapshot.modified {
        return Err("源文件在准备期间发生变化，已放弃缓存发布".into());
    }
    let sha256 = sha256_file(path, cancel)?;
    if sha256 != snapshot.sha256 {
        return Err("源文件内容在准备期间发生变化，已放弃缓存发布".into());
    }
    Ok(())
}

fn sha256_file(path: &Path, cancel: &AtomicBool) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| format!("打开源文件失败: {error}"))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        check_cancel(cancel)?;
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("读取源文件失败: {error}"))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex_encode(&digest.finalize()))
}

fn cache_key_for(
    source_sha256: &str,
    profile: &MediaProfile,
    ffmpeg_identity: &str,
    ffprobe_identity: &str,
) -> String {
    let profile_json = serde_json::to_string(profile).expect("MediaProfile 序列化不应失败");
    let mut digest = Sha256::new();
    for value in [
        CACHE_SCHEMA_VERSION.to_string(),
        source_sha256.to_string(),
        profile_json,
        ffmpeg_identity.to_string(),
        ffprobe_identity.to_string(),
    ] {
        digest.update(value.as_bytes());
        digest.update([0]);
    }
    hex_encode(&digest.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn cache_root() -> PathBuf {
    std::env::var_os(CACHE_ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("uvp-media-profile-cache"))
}

fn cache_lock(key: &str) -> Arc<Mutex<()>> {
    let locks = CACHE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks.lock().expect("媒体缓存锁映射已损坏");
    locks
        .entry(key.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn lock_cache_with_cancel<'a>(
    lock: &'a Arc<Mutex<()>>,
    cancel: &AtomicBool,
) -> Result<std::sync::MutexGuard<'a, ()>, String> {
    loop {
        check_cancel(cancel)?;
        match lock.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err("媒体缓存锁已损坏".to_string());
            }
        }
    }
}

fn write_json_synced<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("序列化媒体缓存索引失败: {error}"))?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("创建媒体缓存索引文件失败: {error}"))?;
    file.write_all(&data)
        .map_err(|error| format!("写入媒体缓存索引失败: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("同步媒体缓存索引失败: {error}"))?;
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let data = fs::read(path).map_err(|error| format!("读取媒体缓存元数据失败: {error}"))?;
    serde_json::from_slice(&data).map_err(|error| format!("解析媒体缓存元数据失败: {error}"))
}

fn tool_identity(path: &str) -> Result<String, String> {
    let output = Command::new(path)
        .arg("-version")
        .output()
        .map_err(|error| format!("读取媒体工具版本失败: {error}"))?;
    if !output.status.success() {
        return Err(format!("媒体工具不可用: {path}"));
    }
    let first_line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    Ok(format!("{path}|{first_line}"))
}

fn ffprobe_bin() -> Option<String> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("UVP_FFPROBE_PATH") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(ffmpeg) = crate::source::ffmpeg_bin() {
        let ffmpeg_path = Path::new(&ffmpeg);
        if let Some(parent) = ffmpeg_path.parent() {
            candidates.push(parent.join(if cfg!(target_os = "windows") {
                "ffprobe.exe"
            } else {
                "ffprobe"
            }));
        }
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(parent) = executable.parent() {
            for name in if cfg!(target_os = "windows") {
                ["ffprobe.exe", "ffprobe"]
            } else {
                ["ffprobe", "ffprobe.exe"]
            } {
                candidates.push(parent.join(name));
                candidates.push(parent.join("resources").join(name));
                candidates.push(parent.join("../resources").join(name));
                candidates.push(parent.join("../Resources").join(name));
            }
        }
    }
    candidates.push(PathBuf::from(if cfg!(target_os = "windows") {
        "ffprobe.exe"
    } else {
        "ffprobe"
    }));
    candidates.extend([
        PathBuf::from("/opt/homebrew/bin/ffprobe"),
        PathBuf::from("/usr/local/bin/ffprobe"),
        PathBuf::from("/opt/local/bin/ffprobe"),
        PathBuf::from("/usr/bin/ffprobe"),
    ]);
    candidates.into_iter().find_map(|candidate| {
        let output = Command::new(&candidate).arg("-version").output().ok()?;
        output
            .status
            .success()
            .then(|| candidate.to_string_lossy().into_owned())
    })
}
