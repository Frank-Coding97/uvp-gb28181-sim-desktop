#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{MediaAudioCodec, MediaProfile, MediaVideoCodec};
    use crate::source::MediaEvent;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;

    fn ffmpeg() -> Option<String> {
        crate::source::ffmpeg_bin()
    }

    fn temp_input(name: &str) -> PathBuf {
        let nonce = format!(
            "{}-{}-{}",
            std::process::id(),
            TEMP_NONCE.fetch_add(1, Ordering::Relaxed),
            name
        );
        std::env::temp_dir().join(format!("uvp-file-profile-{nonce}.mp4"))
    }

    fn make_synthetic_input(path: &Path) {
        make_synthetic_input_secs(path, 1);
    }

    /// 合成指定时长的素材。流式分段测试需要比 `SEGMENT_SECONDS` 更长的输入，
    /// 否则永远只会产生一个分段，测不到跨段拼接。
    fn make_synthetic_input_secs(path: &Path, seconds: u32) {
        let ffmpeg = ffmpeg().expect("测试需要可用的 ffmpeg");
        let output = common::hidden_command(ffmpeg)
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
                &seconds.to_string(),
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

        PreparedFileProfile::sealed(
            format!("timed-{nonce}"),
            base.with_extension("cache"),
            MediaProfile::default(),
            video_file,
            Some(audio_file),
            video_index,
            audio_index,
            duration_90k,
        )
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

    #[test]
    fn quick_fingerprint_reuses_full_hash_when_source_is_unchanged() {
        let input = temp_input("fingerprint-stable");
        make_synthetic_input(&input);
        let cancel = AtomicBool::new(false);

        let first = SourceSnapshot::capture(&input, &cancel, None).expect("首次快照失败");
        let second = SourceSnapshot::capture(&input, &cancel, None).expect("二次快照失败");

        assert_eq!(first.sha256, second.sha256);
        assert_eq!(first.sample_sha256, second.sample_sha256);
        assert_eq!(first.length, second.length);
        // 索引必须落盘：只有内存缓存的话，下次启动仍要重算整片。
        assert!(
            source_index_path(&cache_root()).is_file(),
            "快速指纹索引未写入磁盘"
        );
        let _ = fs::remove_file(&input);
    }

    #[test]
    fn quick_fingerprint_changes_when_source_content_changes() {
        let input = temp_input("fingerprint-changed");
        make_synthetic_input(&input);
        let cancel = AtomicBool::new(false);

        let before = SourceSnapshot::capture(&input, &cancel, None).expect("首次快照失败");
        fs::write(&input, b"not a video anymore").expect("覆盖源文件失败");
        let after = SourceSnapshot::capture(&input, &cancel, None).expect("二次快照失败");

        assert_ne!(before.sha256, after.sha256, "内容变了必须重算全量哈希");
        let _ = fs::remove_file(&input);
    }

    #[test]
    fn parses_ffmpeg_progress_out_time() {
        assert_eq!(parse_hhmmss_us("00:00:05.500000"), Some(5_500_000));
        assert_eq!(parse_hhmmss_us("01:02:03.000000"), Some(3_723_000_000));
        assert_eq!(parse_hhmmss_us("N/A"), None);
        assert_eq!(parse_hhmmss_us(""), None);

        let path = std::env::temp_dir().join(format!(
            "uvp-ffprogress-{}-{}",
            std::process::id(),
            TEMP_NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(
            &path,
            "frame=12\nout_time_us=500000\nout_time_ms=500000\nout_time=00:00:00.500000\nprogress=continue\n\
             frame=25\nout_time_us=1000000\nout_time_ms=1000000\nout_time=00:00:01.000000\nprogress=end\n",
        )
        .expect("写入进度文件失败");
        assert_eq!(read_progress_out_time_us(&path), Some(1_000_000));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn preparation_reports_transcode_progress_with_total() {
        assert!(ffmpeg().is_some(), "该测试需要可用的 ffmpeg");
        let input = temp_input("progress-report");
        make_synthetic_input(&input);
        let cancel = AtomicBool::new(false);
        // 用一组没被其它测试用过的参数，确保这次真的走转码而不是命中缓存。
        let profile = MediaProfile {
            width: 640,
            height: 480,
            video_fps: 20,
            bitrate_kbps: 600,
            keyframe_interval_seconds: 1,
            video_codec: MediaVideoCodec::H264,
            audio_codec: MediaAudioCodec::Aac,
            audio_sample_rate_hz: 16000,
        };
        // 回调要求 'static，用 Arc 共享收集器，测试体自己再读一遍。
        let events: Arc<Mutex<Vec<FilePreparationProgress>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let callback = move |progress: FilePreparationProgress| {
            sink.lock().expect("进度收集锁异常").push(progress);
        };

        let prepared = prepare_file_profile(&input, &profile, &cancel, Some(&callback))
            .expect("合成文件准备失败");

        let captured = events.lock().expect("进度收集锁异常").clone();
        let transcoding: Vec<_> = captured
            .iter()
            .filter(|event| event.phase == FilePreparationPhase::Transcoding)
            .collect();
        assert!(!transcoding.is_empty(), "没有收到转码阶段进度");
        assert!(
            transcoding
                .iter()
                .any(|event| event.total.is_some() && event.completed == event.total),
            "转码阶段应给出带总量的进度并收尾到 100%: {transcoding:?}"
        );
        // 阶段顺序不能乱：转码一定在索引之前。
        let transcode_at = captured
            .iter()
            .position(|event| event.phase == FilePreparationPhase::Transcoding)
            .expect("缺少转码阶段");
        let probing_at = captured
            .iter()
            .position(|event| event.phase == FilePreparationPhase::Probing)
            .expect("缺少索引阶段");
        assert!(transcode_at < probing_at);

        remove_indexed_test_profile(&prepared);
        let _ = fs::remove_file(&input);
    }

    /// 流式准备的核心契约：首段就绪就交付，剩余分段在后台补，
    /// 拼出来的索引必须是一条连续、可循环的时间轴。
    #[test]
    fn streaming_preparation_publishes_first_segment_then_appends() {
        assert!(ffmpeg().is_some(), "该测试需要可用的 ffmpeg");
        let input = temp_input("streaming-segments");
        // 必须长于 SEGMENT_SECONDS，否则只会产生一个分段，测不到跨段拼接。
        make_synthetic_input_secs(&input, SEGMENT_SECONDS * 2 + 5);
        let cancel = AtomicBool::new(false);
        let profile = MediaProfile {
            width: 640,
            height: 480,
            video_fps: 20,
            bitrate_kbps: 600,
            keyframe_interval_seconds: 1,
            video_codec: MediaVideoCodec::H264,
            audio_codec: MediaAudioCodec::G711A,
            audio_sample_rate_hz: 8000,
        };

        let published: Arc<Mutex<Vec<(usize, bool)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&published);
        let callback = move |snapshot: PreparedFileProfile| {
            sink.lock()
                .expect("发布记录锁异常")
                .push((snapshot.video_index().len(), snapshot.is_complete()));
        };

        let prepared =
            prepare_file_profile_streaming(&input, &profile, &cancel, None, Some(&callback))
                .expect("流式准备失败");

        let snapshots = published.lock().expect("发布记录锁异常").clone();
        assert_eq!(snapshots.len(), 1, "首段最多发布一次: {snapshots:?}");
        let (published_len, was_complete) = snapshots[0];
        assert!(!was_complete, "发布时全片不应该已经转完");
        assert!(published_len > 0, "发布时索引不能为空");

        let video = prepared.video_index();
        assert!(
            video.len() > published_len,
            "发布之后还要继续追加: 发布 {published_len} 帧, 最终 {} 帧",
            video.len()
        );
        assert!(prepared.is_complete(), "准备返回时应已完成");

        // PTS 必须等步长递进：分段拼接若丢了偏移或重置了时间戳，这里会断。
        let step = 90_000 / u64::from(profile.video_fps);
        for pair in video.windows(2) {
            assert_eq!(
                pair[1].pts_90k,
                pair[0].pts_90k.saturating_add(step),
                "分段拼接后 PTS 不连续: {} -> {}",
                pair[0].pts_90k,
                pair[1].pts_90k
            );
        }
        // 每个 GOP 起点必须是关键帧，否则循环回放会在接缝处花屏。
        let gop = usize::try_from(profile.video_fps * profile.keyframe_interval_seconds)
            .expect("GOP 帧数超出 usize");
        for (index, entry) in video.iter().enumerate() {
            if index % gop == 0 {
                assert!(entry.key_frame, "GOP 起点 {index} 不是关键帧");
            }
        }

        // 索引偏移必须落在产物文件范围内，否则推流时会读到文件尾的空洞。
        let video_size = fs::metadata(prepared.video_path())
            .expect("读取产物信息失败")
            .len();
        for entry in &video {
            assert!(
                entry.offset.saturating_add(entry.length) <= video_size,
                "访问单元越界: offset={} length={} size={video_size}",
                entry.offset,
                entry.length
            );
        }
        let audio = prepared.audio_index();
        assert!(!audio.is_empty(), "G.711 音频索引不应为空");
        for pair in audio.windows(2) {
            assert!(
                pair[1].pts_90k >= pair[0].pts_90k,
                "音频 PTS 回退: {} -> {}",
                pair[0].pts_90k,
                pair[1].pts_90k
            );
        }

        // 全片完成后必须能循环：跨过片尾继续输出，且 PTS 仍然单调。
        let total = video.len();
        let mut source = prepared.open_source_unpaced(true).expect("打开循环源失败");
        let mut frames = 0usize;
        let mut last_pts = 0u64;
        while frames < total + 5 {
            match source.next_timed_media_event().expect("读取事件失败") {
                Some(MediaEvent::Video(access_unit)) => {
                    if frames > 0 {
                        assert!(access_unit.pts_90k > last_pts, "循环后 PTS 必须继续增长");
                    }
                    last_pts = access_unit.pts_90k;
                    frames += 1;
                }
                Some(_) => {}
                None => break,
            }
        }
        assert_eq!(frames, total + 5, "循环源应能越过片尾继续输出");

        // 目录留给下次缓存命中，会让"发布时未完成"的断言失效，直接清掉。
        let _ = fs::remove_dir_all(prepared.cache_dir());
        let _ = fs::remove_file(&input);
    }

    /// 手动基准：验证"首段就绪"确实远早于"全片完成"。
    ///
    /// `cargo test -p media-rtp --lib streaming_first_segment -- --ignored --nocapture`
    #[test]
    #[ignore = "手动基准，不做常规回归"]
    fn streaming_first_segment_arrives_much_earlier() {
        assert!(ffmpeg().is_some(), "该测试需要可用的 ffmpeg");
        let input = temp_input("streaming-bench");
        make_synthetic_input_secs(&input, 60);
        let cancel = AtomicBool::new(false);
        let profile = MediaProfile::default();

        let start = Instant::now();
        let published_at: Arc<Mutex<Option<Duration>>> = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&published_at);
        let callback = move |_snapshot: PreparedFileProfile| {
            let mut slot = sink.lock().expect("基准记录锁异常");
            if slot.is_none() {
                *slot = Some(start.elapsed());
            }
        };

        let prepared =
            prepare_file_profile_streaming(&input, &profile, &cancel, None, Some(&callback))
                .expect("流式准备失败");
        let total = start.elapsed();
        let first = published_at
            .lock()
            .expect("基准记录锁异常")
            .expect("没有发布首段");
        println!("60 秒素材：首段就绪 {first:?}，全片完成 {total:?}");
        assert!(
            first < total / 2,
            "首段应显著早于全片完成: {first:?} vs {total:?}"
        );

        let _ = fs::remove_dir_all(prepared.cache_dir());
        let _ = fs::remove_file(&input);
    }
}
use crate::profile::{MediaAudioCodec, MediaProfile, MediaVideoCodec};
use crate::encoder::{self, probe_source_keyframe_gap, probe_source_video, SourceVideoStream};
use crate::ps::VideoCodec;
use crate::source::{Frame, VideoSource};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime};

const CACHE_SCHEMA_VERSION: u32 = 1;
const CACHE_ROOT_ENV: &str = "UVP_MEDIA_PROFILE_CACHE_DIR";
/// 文件读写缓冲大小。必须堆分配：Windows 阻塞线程栈只有 2MB，
/// 若把 1MB 缓冲放在栈上，叠加同层调用会触发 STATUS_STACK_OVERFLOW。
const COPY_BUFFER_BYTES: usize = 256 * 1024;
static CACHE_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
#[cfg(test)]
static TEMP_NONCE: AtomicU64 = AtomicU64::new(1);

/// 源文件快速指纹索引。
///
/// 缓存键仍然用整片 SHA256（保持与既有缓存目录兼容），但每次注册都把整片重读一遍
/// 只为算出这个键，长素材动辄几十秒。这里额外记一份「路径 → 大小/修改时间/首尾采样」
/// 的索引：命中就直接复用上次算出的 SHA256，未命中才算全量。
///
/// 索引只是加速产物，任何读写失败都退回全量哈希，不能影响媒体准备的正确性。
const SOURCE_INDEX_FILE: &str = "source-index.json";
const SOURCE_INDEX_VERSION: u32 = 1;
/// 首尾各取多少字节参与快速指纹。两次小读即可覆盖「同大小同时间但内容被换掉」的情况。
const QUICK_SAMPLE_BYTES: u64 = 64 * 1024;
/// 索引条目上限。超过就整体重建：它只是加速用的旁路数据，
/// 与其为清理单个失效条目引入时间戳/排序，不如一次性清空换简单可靠。
const SOURCE_INDEX_MAX_ENTRIES: usize = 128;
/// 进度事件最小上报间隔。ffmpeg 本身约每 0.5 秒一行，这里再限流一次，
/// 避免长片转码把 Tauri 事件通道刷爆（前端按帧重绘也会卡）。
const PROGRESS_REPORT_INTERVAL: Duration = Duration::from_millis(500);
/// 流式准备时，推流端追到索引末尾后的轮询周期。
///
/// 一个 10 秒分段在普通机器上约 0.5~1 秒转完，5ms 的轮询粒度足够细，
/// 又不会在长素材上空转烧 CPU。
const INDEX_WAIT_TICK: Duration = Duration::from_millis(5);
/// 等新索引的上限。超过就认为转码侧卡住或已失败，让上层看到"源已结束"，
/// 由设备层决定是重连还是报错——不能把推流线程永久挂死在这里。
const INDEX_WAIT_LIMIT: Duration = Duration::from_secs(3);
/// 分段转码的单个分片时长。首帧延迟≈这段时间的转码耗时，同时它决定
/// 索引追加和磁盘读写的粒度；10 秒在首帧延迟与调度开销之间比较平衡。
const SEGMENT_SECONDS: u32 = 10;

/// 文件转码策略(P6):直通 → 硬件 → 软编。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TranscodeStrategy {
    /// `-c:v copy`:源文件编码/分辨率/帧率与目标完全一致。
    Passthrough,
    /// 硬件编码器(VideoToolbox / NVENC / QSV...)。
    Hardware(encoder::EncoderBackend),
    /// libx264 / libx265,最终兜底。
    Software,
}

impl TranscodeStrategy {
    fn label(&self) -> &'static str {
        match self {
            Self::Passthrough => "直通",
            Self::Hardware(backend) => match backend {
                encoder::EncoderBackend::VideoToolbox => "VideoToolbox",
                encoder::EncoderBackend::Nvenc => "NVENC",
                encoder::EncoderBackend::Qsv => "QSV",
                encoder::EncoderBackend::Amf => "AMF",
                encoder::EncoderBackend::MediaFoundation => "Media Foundation",
                encoder::EncoderBackend::Software => unreachable!(),
            },
            Self::Software => "软编",
        }
    }
}

/// 返回从低代价到高代价的转码策略列表。源探测失败时只返回转码策略。
fn video_strategies(
    ffmpeg: &str,
    ffprobe: &str,
    source_path: &Path,
    profile: &MediaProfile,
    source_video: Option<&SourceVideoStream>,
) -> Vec<TranscodeStrategy> {
    let mut strategies = Vec::new();
    // 直通条件:编码、分辨率、帧率一致 **且** 码率、关键帧间隔不超标。
    //
    // 只看编码/分辨率/帧率是不够的:直通等于把源文件原样送给对端,profile 里配的码率
    // 和 I 帧间隔会被彻底忽略——用户选 2 Mbps,实际可能按源文件的 70 Mbps 在推。
    // 码率探测不到时同样不直通(宁可转码,不给对端一条失控的流)。
    let gop_frames = profile
        .video_fps
        .saturating_mul(profile.keyframe_interval_seconds)
        .max(1);
    if let Some(source) = source_video {
        let codec_match = match (source.codec, profile.video_codec) {
            (VideoCodec::H264, MediaVideoCodec::H264) => true,
            (VideoCodec::H265, MediaVideoCodec::H265) => true,
            _ => false,
        };
        let bitrate_ok = source.bit_rate.is_some_and(|rate| {
            // 允许 30% 余量:源文件码率贴着目标值时,重编码省下的那点带宽没意义。
            rate / 1000 <= u64::from(profile.bitrate_kbps) * 13 / 10
        });
        let shape_ok = codec_match
            && source.width == profile.width
            && source.height == profile.height
            && source.fps_matches(profile.video_fps)
            && bitrate_ok;
        // GOP 探测要读一段源文件,所以放在最后:前面几项已经否掉时就不必花这个钱。
        let gop_ok = shape_ok
            && probe_source_keyframe_gap(
                ffprobe,
                source_path,
                f64::from(profile.keyframe_interval_seconds) + 1.0,
            )
            .is_some_and(|gap| gap <= gop_frames);
        if shape_ok && gop_ok {
            strategies.push(TranscodeStrategy::Passthrough);
        }
    }
    // 硬件编码(P2a 选出的后端)。
    let codec_for_encoder = match profile.video_codec {
        MediaVideoCodec::H264 => VideoCodec::H264,
        MediaVideoCodec::H265 => VideoCodec::H265,
    };
    let selected = encoder::select(ffmpeg, codec_for_encoder);
    if selected.backend.is_hardware() {
        strategies.push(TranscodeStrategy::Hardware(selected.backend));
    }
    // 软编兜底。
    strategies.push(TranscodeStrategy::Software);
    strategies
}

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
    /// 全片是否已转完。流式准备边转边写，只有收尾时才置真。
    ///
    /// 旧版本清单没有这个字段，缺省按"完整"读——旧缓存确实都是整片转好的，
    /// 没必要让它们全部失效重转。
    #[serde(default = "default_complete")]
    complete: bool,
}

fn default_complete() -> bool {
    true
}

/// 文件媒体产物：已完成，或流式准备中正在增长的同一份产物。
///
/// 索引与总时长放在共享句柄里，而不是普通字段：流式准备期间转码线程持续追加，
/// 推流端在同一时刻按游标消费，两边必须看到同一份索引。`complete` 为真表示
/// 全片已经转完、索引不再变化，消费端追到末尾后可以安全回到循环起点。
#[derive(Debug, Clone)]
pub struct PreparedFileProfile {
    cache_key: String,
    cache_dir: PathBuf,
    profile: MediaProfile,
    video_file: PathBuf,
    audio_file: Option<PathBuf>,
    video_index: Arc<RwLock<Vec<VideoAccessUnitIndex>>>,
    audio_index: Arc<RwLock<Vec<AudioAccessUnitIndex>>>,
    duration_90k: Arc<AtomicU64>,
    complete: Arc<AtomicBool>,
}

/// 索引读锁。持锁方 panic 不应让整条推流链路卡死，中毒就直接接管内部数据。
fn read_list<T>(lock: &RwLock<Vec<T>>) -> std::sync::RwLockReadGuard<'_, Vec<T>> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write_list<T>(lock: &RwLock<Vec<T>>) -> std::sync::RwLockWriteGuard<'_, Vec<T>> {
    lock.write().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl PreparedFileProfile {
    /// 由已校验的完整索引构造产物（缓存命中、测试、非流式路径共用）。
    #[allow(clippy::too_many_arguments)]
    fn sealed(
        cache_key: String,
        cache_dir: PathBuf,
        profile: MediaProfile,
        video_file: PathBuf,
        audio_file: Option<PathBuf>,
        video_index: Vec<VideoAccessUnitIndex>,
        audio_index: Vec<AudioAccessUnitIndex>,
        duration_90k: u64,
    ) -> Self {
        Self {
            cache_key,
            cache_dir,
            profile,
            video_file,
            audio_file,
            video_index: Arc::new(RwLock::new(video_index)),
            audio_index: Arc::new(RwLock::new(audio_index)),
            duration_90k: Arc::new(AtomicU64::new(duration_90k)),
            complete: Arc::new(AtomicBool::new(true)),
        }
    }

    /// 构造一个索引为空、尚未完成的产物句柄，供流式准备边生成边发布。
    fn streaming(
        cache_key: String,
        cache_dir: PathBuf,
        profile: MediaProfile,
        video_file: PathBuf,
        audio_file: Option<PathBuf>,
    ) -> Self {
        Self {
            cache_key,
            cache_dir,
            profile,
            video_file,
            audio_file,
            video_index: Arc::new(RwLock::new(Vec::new())),
            audio_index: Arc::new(RwLock::new(Vec::new())),
            duration_90k: Arc::new(AtomicU64::new(0)),
            complete: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cache_key(&self) -> &str {
        &self.cache_key
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn profile(&self) -> &MediaProfile {
        &self.profile
    }

    /// 当前已就绪的视频索引快照。流式准备期间长度会随时间增长。
    pub fn video_index(&self) -> Vec<VideoAccessUnitIndex> {
        read_list(&self.video_index).clone()
    }

    /// 当前已就绪的音频索引快照。
    pub fn audio_index(&self) -> Vec<AudioAccessUnitIndex> {
        read_list(&self.audio_index).clone()
    }

    pub fn duration_90k(&self) -> u64 {
        self.duration_90k.load(Ordering::Acquire)
    }

    /// 全片是否已经准备完成。未完成时消费端追到索引末尾要继续等，不能判定为结束。
    pub fn is_complete(&self) -> bool {
        self.complete.load(Ordering::Acquire)
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

/// 索引分路，用于等待新数据时区分视频/音频两条独立时间轴。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndexLane {
    Video,
    Audio,
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
        let cycle_offset = self.cycle.saturating_mul(self.prepared.duration_90k());
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
        let cycle_offset = self.cycle.saturating_mul(self.prepared.duration_90k());
        let mut output = Vec::new();
        // 这里不做等待：音频暂时落后于视频时不该把视频帧卡住，
        // 缺的音频在下一帧的窗口里会被补上。
        let audio_len = read_list(&self.prepared.audio_index).len();
        while self.audio_cursor < audio_len {
            let index = read_list(&self.prepared.audio_index)[self.audio_cursor].clone();
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
                    // 流式准备还没产出下一段：先等，不能误判成播放结束而回到开头。
                    if !self.prepared.is_complete() {
                        if self.wait_for_growth(self.video_cursor, IndexLane::Video)
                            || self.wait_for_growth(self.audio_cursor, IndexLane::Audio)
                        {
                            continue;
                        }
                        return Ok(None);
                    }
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
        let Some(index) = self.video_index_at(self.video_cursor) else {
            return Ok(());
        };
        self.video_cursor += 1;
        let cycle_offset = self.cycle.saturating_mul(self.prepared.duration_90k());
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
        let Some(index) = self.audio_index_at(self.audio_cursor) else {
            return Ok(());
        };
        let Some(reader) = self.audio_reader.as_mut() else {
            return Ok(());
        };
        self.audio_cursor += 1;
        let cycle_offset = self.cycle.saturating_mul(self.prepared.duration_90k());
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

    /// 视频索引当前长度。
    fn video_len(&self) -> usize {
        read_list(&self.prepared.video_index).len()
    }

    fn audio_len(&self) -> usize {
        read_list(&self.prepared.audio_index).len()
    }

    /// 取视频索引第 `cursor` 项。流式准备还没产出这一项时短暂等待。
    fn video_index_at(&self, cursor: usize) -> Option<VideoAccessUnitIndex> {
        if let Some(index) = read_list(&self.prepared.video_index).get(cursor) {
            return Some(index.clone());
        }
        if self.prepared.is_complete() || !self.wait_for_growth(cursor, IndexLane::Video) {
            return None;
        }
        read_list(&self.prepared.video_index).get(cursor).cloned()
    }

    fn audio_index_at(&self, cursor: usize) -> Option<AudioAccessUnitIndex> {
        if let Some(index) = read_list(&self.prepared.audio_index).get(cursor) {
            return Some(index.clone());
        }
        if self.prepared.is_complete() || !self.wait_for_growth(cursor, IndexLane::Audio) {
            return None;
        }
        read_list(&self.prepared.audio_index).get(cursor).cloned()
    }

    /// 等索引长到 `cursor` 之后。返回 false 表示全片已转完，或等待超时。
    fn wait_for_growth(&self, cursor: usize, lane: IndexLane) -> bool {
        let mut waited = Duration::ZERO;
        loop {
            let len = match lane {
                IndexLane::Video => self.video_len(),
                IndexLane::Audio => self.audio_len(),
            };
            if len > cursor {
                return true;
            }
            if self.prepared.is_complete() || waited >= INDEX_WAIT_LIMIT {
                return false;
            }
            std::thread::sleep(INDEX_WAIT_TICK);
            waited += INDEX_WAIT_TICK;
        }
    }

    fn next_video_index(&mut self) -> Option<VideoAccessUnitIndex> {
        loop {
            let len = self.video_len();
            if self.video_cursor < len {
                let index = read_list(&self.prepared.video_index)[self.video_cursor].clone();
                self.video_cursor += 1;
                return Some(index);
            }
            // 游标追到末尾。流式准备期间这是"还没转出来"，不是"播完了"：
            // 只有全片完成之后才允许回到循环起点。
            if !self.prepared.is_complete() {
                if self.wait_for_growth(self.video_cursor, IndexLane::Video) {
                    continue;
                }
                return None;
            }
            if !self.looping || len == 0 {
                return None;
            }
            self.video_cursor = 0;
            self.audio_cursor = 0;
            self.cycle = self.cycle.saturating_add(1);
            self.last_video_window = None;
        }
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
        let video_index = self.prepared.video_index();
        let audio_index = self.prepared.audio_index();
        let Some((cursor, _)) = video_index
            .iter()
            .enumerate()
            .find(|(_, index)| index.pts_90k >= target_90k)
        else {
            self.video_cursor = video_index.len();
            self.audio_cursor = audio_index.len();
            return;
        };
        self.video_cursor = (0..=cursor)
            .rev()
            .find(|&index| video_index[index].key_frame)
            .unwrap_or(cursor);
        let rollback_pts_90k = video_index[self.video_cursor].pts_90k;
        self.audio_cursor = audio_index
            .iter()
            .position(|index| index.pts_90k.saturating_add(index.duration_90k) > rollback_pts_90k)
            .unwrap_or(audio_index.len());
        self.cycle = 0;
        self.pace_started = if self.paced {
            video_index
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
            .duration_90k()
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
    prepare_file_profile_streaming(path, profile, cancel, progress, None)
}

/// 流式准备：第一段转出并通过校验就交付一个可以立刻推流的产物句柄，
/// 剩余分段在后台继续追加到同一份句柄里。
///
/// 与 [`prepare_file_profile`] 的产物结构、索引格式、缓存布局完全一致，唯一区别是
/// "什么时候可以开始播"：普通版本等全片，流式版本等第一段。`on_playable` 会被调用
/// 至多一次，且一定在返回之前——调用方可以在回调里立刻把句柄交给推流端。
pub fn prepare_file_profile_streaming(
    path: &Path,
    profile: &MediaProfile,
    cancel: &AtomicBool,
    progress: Option<&FilePreparationProgressCallback>,
    on_playable: Option<&(dyn Fn(PreparedFileProfile) + Send + Sync)>,
) -> Result<PreparedFileProfile, String> {
    check_cancel(cancel)?;
    profile.validate()?;
    if !path.is_file() {
        return Err(format!("文件源不存在或不是普通文件: {}", path.display()));
    }

    report(progress, FilePreparationPhase::HashingSource);
    let source_snapshot = SourceSnapshot::capture(path, cancel, progress)?;
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
            if let Some(callback) = on_playable {
                callback(prepared.clone());
            }
            return Ok(prepared);
        }
        check_cancel(cancel)?;
        // 目录在但读不出完整产物：多半是上次流式准备中途退出留下的半成品。
        fs::remove_dir_all(&final_dir)
            .map_err(|error| format!("移除损坏的媒体缓存失败: {error}"))?;
    }

    check_cancel(cancel)?;
    fs::create_dir_all(&final_dir).map_err(|error| format!("创建媒体缓存目录失败: {error}"))?;

    // 流式准备直接写最终目录，不再"临时目录 + rename"：产物必须在转码期间就能被
    // 推流端打开，rename 会让已打开的句柄指向被移走的 inode（Windows 上直接失败）。
    // 完整性改由 manifest 的 `complete` 标记保证，未完成的目录下次会被清掉重来。
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
        &final_dir,
        on_playable,
    );
    if let Err(error) = result {
        // 取消或失败都清掉半成品。忽略删除错误：文件可能仍被推流端打开，
        // 下次启动时 `load_cached_profile` 会把它判成不完整再清一次。
        let _ = fs::remove_dir_all(&final_dir);
        return Err(error);
    }
    report(progress, FilePreparationPhase::Complete);
    result
}

#[allow(clippy::too_many_arguments)]
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
    cache_dir: &Path,
    on_playable: Option<&(dyn Fn(PreparedFileProfile) + Send + Sync)>,
) -> Result<PreparedFileProfile, String> {
    report(progress, FilePreparationPhase::Transcoding);
    // 直接读源文件，不再复制一份快照：转码本来就要把源整片读一遍，复制等于把整片
    // 再读再写一遍——长素材光这一步就要十几秒，正好卡在"首段就绪"之前。
    // 源文件在准备期间被改动的情况由 `SourceSnapshot` 的快照比对兜底。
    let transcode_input = source.to_path_buf();
    let source_has_audio = has_audio_stream(ffprobe, &transcode_input, cancel)?;
    let source_video = probe_source_video(ffprobe, &transcode_input).ok();
    // 进度分母：转码耗时与时长线性相关，拿到时长前端才能给出百分比与剩余时间。
    let source_duration_us = probe_stream_duration_us(ffprobe, &transcode_input, cancel);

    let video_file = cache_dir.join(video_filename(profile.video_codec));
    let audio_file = source_has_audio.then(|| cache_dir.join(audio_filename(profile.audio_codec)));
    // 先建空产物：第一段就绪后推流端会立刻按这两个路径打开文件。
    File::create(&video_file).map_err(|error| format!("创建视频产物失败: {error}"))?;
    if let Some(path) = &audio_file {
        File::create(path).map_err(|error| format!("创建音频产物失败: {error}"))?;
    }

    // 全程只有这一份产物句柄：分段消费时往里追加，第一段就绪后通过 `on_playable`
    // 交给推流端。两边持有同一组 Arc，所以"索引增长"和"完成标记"对推流端立即可见。
    let prepared = PreparedFileProfile::streaming(
        cache_key.to_string(),
        cache_dir.to_path_buf(),
        profile.clone(),
        video_file.clone(),
        audio_file.clone(),
    );

    // 按代价从低到高尝试:直通 → 硬件转码 → 软编。首段校验不过就退到下一档,
    // 软编是最终兜底,它失败才真正报错。
    let mut last_error = String::new();
    let mut published = false;
    let mut finished = false;
    for strategy in video_strategies(ffmpeg, ffprobe, &transcode_input, profile, source_video.as_ref()) {
        check_cancel(cancel)?;
        match run_streaming_transcode(
            ffmpeg,
            ffprobe,
            &transcode_input,
            profile,
            cache_dir,
            strategy,
            &prepared,
            source_has_audio,
            source_duration_us,
            progress,
            cancel,
            &mut published,
            on_playable,
        ) {
            Ok(()) => {
                tracing::info!(strategy = strategy.label(), "文件源视频准备完成");
                finished = true;
                break;
            }
            Err(error) => {
                check_cancel(cancel)?;
                if published {
                    // 索引已经对外发布，消费端游标就落在这份产物上。此刻清空重试会让
                    // 游标指向不存在的项，只能如实报错，由设备层决定重连。
                    return Err(error);
                }
                tracing::warn!(strategy = strategy.label(), %error, "文件源视频准备失败,尝试下一档");
                last_error = error;
                reset_streaming_output(&prepared)?;
            }
        }
    }
    if !finished {
        return Err(format!("文件转码失败: {last_error}"));
    }

    check_source_unchanged(source, source_snapshot, cancel)?;

    let video_index = prepared.video_index();
    let audio_index = prepared.audio_index();
    let duration_90k = media_duration(&video_index, &audio_index);
    if duration_90k == 0 {
        return Err("转码结果没有有效媒体时长".into());
    }
    prepared.duration_90k.store(duration_90k, Ordering::Release);

    report(progress, FilePreparationPhase::Publishing);
    let index = CacheIndex {
        video: video_index,
        audio: audio_index,
        duration_90k,
    };
    write_json_synced(&cache_dir.join("index.json"), &index)?;
    let manifest = CacheManifest {
        cache_schema_version: CACHE_SCHEMA_VERSION,
        cache_key: cache_key.to_string(),
        source_sha256: source_snapshot.sha256.clone(),
        profile: profile.clone(),
        ffmpeg_identity: ffmpeg_identity.to_string(),
        ffprobe_identity: ffprobe_identity.to_string(),
        video_file: file_name_utf8(&video_file)?,
        audio_file: audio_file
            .as_ref()
            .map(|path| file_name_utf8(path))
            .transpose()?,
        index_file: "index.json".into(),
        complete: true,
    };
    write_json_synced(&cache_dir.join("manifest.json"), &manifest)?;
    // 段目录只是中间产物，留着会占掉与成片相当的空间。
    let _ = fs::remove_dir_all(cache_dir.join("segments"));
    // 顺序不能反：先把索引和清单落盘，再宣布完成。消费端看到 complete 时索引一定是全的，
    // 否则循环回开头会读到不存在的访问单元。
    prepared.complete.store(true, Ordering::Release);
    Ok(prepared)
}

fn file_name_utf8(path: &Path) -> Result<String, String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .ok_or_else(|| "缓存产物文件名非 UTF-8".to_string())
}

/// 清空上一档策略留下的半成品，让下一档从零开始写同一份句柄。
fn reset_streaming_output(prepared: &PreparedFileProfile) -> Result<(), String> {
    write_list(&prepared.video_index).clear();
    write_list(&prepared.audio_index).clear();
    prepared.duration_90k.store(0, Ordering::Release);
    fs::write(&prepared.video_file, []).map_err(|error| format!("清空视频产物失败: {error}"))?;
    if let Some(path) = &prepared.audio_file {
        fs::write(path, []).map_err(|error| format!("清空音频产物失败: {error}"))?;
    }
    let _ = fs::remove_dir_all(prepared.cache_dir.join("segments"));
    Ok(())
}

/// 跑一档转码策略：启动分段转码，边跑边把已经落定的段并进产物。
///
/// `published` 跨调用共享：置真表示索引已经交给推流端，调用方不能再清空重试。
#[allow(clippy::too_many_arguments)]
fn run_streaming_transcode(
    ffmpeg: &str,
    ffprobe: &str,
    source: &Path,
    profile: &MediaProfile,
    cache_dir: &Path,
    strategy: TranscodeStrategy,
    prepared: &PreparedFileProfile,
    source_has_audio: bool,
    source_duration_us: Option<u64>,
    progress: Option<&FilePreparationProgressCallback>,
    cancel: &AtomicBool,
    published: &mut bool,
    on_playable: Option<&(dyn Fn(PreparedFileProfile) + Send + Sync)>,
) -> Result<(), String> {
    let segment_dir = cache_dir.join("segments");
    let _ = fs::remove_dir_all(&segment_dir);
    fs::create_dir_all(&segment_dir).map_err(|error| format!("创建分段目录失败: {error}"))?;

    let mut command = build_transcode_command(ffmpeg, source, profile, &segment_dir, strategy)?;
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    let progress_file = progress_file_path(&segment_dir);
    let _ = fs::remove_file(&progress_file);
    let mut child = command
        .spawn()
        .map_err(|error| format!("ffmpeg 文件转码启动失败: {error}"))?;

    // stderr 必须持续读走：管道写满会把 ffmpeg 卡在日志输出上，现场表现就是"转码卡死"。
    let mut stderr_reader = child.stderr.take().map(|stderr| {
        std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(stderr);
            let mut data = Vec::new();
            let _ = reader.read_to_end(&mut data);
            data
        })
    });

    let mut video_writer = OpenOptions::new()
        .append(true)
        .open(&prepared.video_file)
        .map_err(|error| format!("打开视频产物失败: {error}"))?;
    let mut audio_writer = prepared
        .audio_file
        .as_ref()
        .map(|path| OpenOptions::new().append(true).open(path))
        .transpose()
        .map_err(|error| format!("打开音频产物失败: {error}"))?;

    let mut next_segment = 0usize;
    let mut timeline_origin: Option<i64> = None;
    let mut exit_status: Option<std::process::ExitStatus> = None;
    // G.711 要跨段攒半块，这里放一份装配器；AAC 走裸流包索引，不需要。
    let mut g711 = (source_has_audio
        && matches!(
            profile.audio_codec,
            MediaAudioCodec::G711A | MediaAudioCodec::G711U
        ))
    .then(|| G711Assembler::new(profile.audio_codec, profile.effective_audio_sample_rate_hz()));
    let mut last_report = Instant::now()
        .checked_sub(PROGRESS_REPORT_INTERVAL)
        .unwrap_or_else(Instant::now);

    loop {
        if cancel.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(reader) = stderr_reader.take() {
                let _ = reader.join();
            }
            let _ = fs::remove_file(&progress_file);
            return Err("ffmpeg 文件转码已取消".into());
        }
        if exit_status.is_none() {
            exit_status = child
                .try_wait()
                .map_err(|error| format!("ffmpeg 文件转码等待失败: {error}"))?;
        }

        // 把已经落定的段全部并进产物。段边界由 segment muxer 在关键帧处切，
        // 且保留了原始时间戳，所以拼起来天然是一条连续时间轴。
        while let Some(segment) =
            segment_ready(&segment_dir, next_segment, exit_status.is_some())
        {
            let is_first = next_segment == 0;
            // 抽取与校验现在按段进行、和转码交织在一起。这两个阶段名保留给前端
            // 展示"正在建立索引"，只在第一段报一次，避免每段刷屏。
            if is_first {
                report(progress, FilePreparationPhase::Extracting);
            }
            ingest_segment(
                ffmpeg,
                ffprobe,
                &segment,
                profile,
                prepared,
                &mut video_writer,
                audio_writer.as_mut(),
                g711.as_mut(),
                &mut timeline_origin,
                is_first,
                source_has_audio,
                cancel,
            )?;
            if is_first {
                report(progress, FilePreparationPhase::Probing);
            }
            let _ = fs::remove_file(&segment);
            next_segment = next_segment.saturating_add(1);

            if is_first && !*published {
                // 首段校验通过：从这一刻起可以推流，后续段继续追加到同一份句柄。
                *published = true;
                if let Some(callback) = on_playable {
                    callback(prepared.clone());
                }
            }
        }

        if last_report.elapsed() >= PROGRESS_REPORT_INTERVAL {
            if let Some(done_us) = read_progress_out_time_us(&progress_file) {
                report_with_progress(
                    progress,
                    FilePreparationPhase::Transcoding,
                    Some(done_us),
                    source_duration_us,
                );
            }
            last_report = Instant::now();
        }

        if let Some(status) = exit_status {
            let stderr = stderr_reader
                .take()
                .map(|reader| reader.join().unwrap_or_default())
                .unwrap_or_default();
            let _ = fs::remove_file(&progress_file);
            if status.success() {
                if next_segment == 0 {
                    return Err("转码没有产出任何分段".to_string());
                }
                // G.711 的最后一个短块只有整片转完才知道，这里补发。
                if let Some(assembler) = g711.as_mut() {
                    let mut tail = Vec::new();
                    assembler.flush(timeline_origin.unwrap_or_default(), &mut tail);
                    if !tail.is_empty() {
                        write_list(&prepared.audio_index).extend(tail);
                        refresh_duration(prepared);
                    }
                }
                // 收尾补一次满格：短素材可能在第一个上报周期之前就转完了，
                // 不补的话前端进度会停在 0。
                if let (Some(total), Some(callback)) = (source_duration_us, progress) {
                    report_with_progress(
                        Some(callback),
                        FilePreparationPhase::Transcoding,
                        Some(total),
                        Some(total),
                    );
                }
                return Ok(());
            }
            return Err(format!(
                "ffmpeg 文件转码失败: {}",
                last_stderr_line_bytes(&stderr)
            ));
        }

        std::thread::sleep(Duration::from_millis(25));
    }
}

/// 判断第 `index` 个分段是否已经写完。
///
/// segment muxer 关闭当前段之后才创建下一段，所以"下一段存在"就等价于"这一段写完"。
/// 最后一段没有后继，只能等进程退出。
fn segment_ready(segment_dir: &Path, index: usize, process_exited: bool) -> Option<PathBuf> {
    let path = segment_dir.join(format!("seg-{index:05}.mkv"));
    if !path.is_file() {
        return None;
    }
    if process_exited || segment_dir.join(format!("seg-{:05}.mkv", index + 1)).is_file() {
        Some(path)
    } else {
        None
    }
}

/// G.711(PCM)裸流没有帧头，只能按固定块切。512 字节 = 8 kHz 下的 64 ms，也是历史
/// "整片一次转完"产物的包长。切块按全局字节偏移对齐，跨段剩下的半个块攒到下一段
/// 补满——否则每段尾巴都会多出一个短包，段越多包数越对不上。
const G711_BLOCK_BYTES: usize = 512;

/// 跨分段累积 G.711 字节，按固定块产出音频 AU。
struct G711Assembler {
    codec: MediaAudioCodec,
    sample_rate: u32,
    /// 还没凑满一块的尾部字节。
    pending: Vec<u8>,
    /// `pending` 首字节在产物文件里的偏移。
    base: u64,
    /// 下一个 AU 的时间戳；G.711 每个字节就是一个样本，按块长往后推即可。
    next_pts: Option<i64>,
}

impl G711Assembler {
    fn new(codec: MediaAudioCodec, sample_rate: u32) -> Self {
        Self {
            codec,
            sample_rate,
            pending: Vec::new(),
            base: 0,
            next_pts: None,
        }
    }

    /// 追加"刚刚写进产物文件、起始偏移为 `file_offset`"的一段字节。
    fn push(
        &mut self,
        file_offset: u64,
        bytes: &[u8],
        start_pts: Option<i64>,
        origin: i64,
        out: &mut Vec<AudioAccessUnitIndex>,
    ) {
        if self.next_pts.is_none() {
            self.next_pts = start_pts;
        }
        if self.pending.is_empty() {
            self.base = file_offset;
        }
        self.pending.extend_from_slice(bytes);
        self.drain_blocks(origin, out);
    }

    /// 收尾：把最后不满一块的尾巴也当成一个 AU 发出去（整片末尾本来就会有一个短包）。
    fn flush(&mut self, origin: i64, out: &mut Vec<AudioAccessUnitIndex>) {
        let length = self.pending.len();
        if length == 0 {
            return;
        }
        self.emit(0, length, origin, out);
        self.pending.clear();
        self.base = self.base.saturating_add(length as u64);
    }

    fn drain_blocks(&mut self, origin: i64, out: &mut Vec<AudioAccessUnitIndex>) {
        let mut consumed = 0;
        while self.pending.len() - consumed >= G711_BLOCK_BYTES {
            self.emit(consumed, G711_BLOCK_BYTES, origin, out);
            consumed += G711_BLOCK_BYTES;
        }
        if consumed > 0 {
            self.pending.drain(..consumed);
            self.base = self.base.saturating_add(consumed as u64);
        }
    }

    fn emit(
        &mut self,
        relative: usize,
        length: usize,
        origin: i64,
        out: &mut Vec<AudioAccessUnitIndex>,
    ) {
        let sample_count = u32::try_from(length).unwrap_or(u32::MAX);
        let duration_90k = duration_for_samples(sample_count, self.sample_rate);
        let pts_90k = self.next_pts.unwrap_or_default();
        self.next_pts = Some(
            pts_90k.saturating_add(i64::try_from(duration_90k).unwrap_or(i64::MAX)),
        );
        out.push(AudioAccessUnitIndex {
            offset: self.base.saturating_add(relative as u64),
            length: length as u64,
            pts_90k: normalize_pts(pts_90k, origin),
            duration_90k,
            sample_rate_hz: self.sample_rate,
            sample_count,
            codec: self.codec,
        });
    }
}

/// 读某个分段里第一个音频包的时间戳（G.711 每段只能从这里接上时间轴）。
fn first_audio_pts_90k(
    ffprobe: &str,
    container: &Path,
    cancel: &AtomicBool,
) -> Result<Option<i64>, String> {
    let packets = run_probe(
        ffprobe,
        [
            "-select_streams",
            "a:0",
            "-read_intervals",
            "%+#1",
            "-show_packets",
            "-show_entries",
            "packet=pts_time",
            "-of",
            "json",
        ],
        container,
        cancel,
    )?;
    Ok(array_values(&packets, "packets")?
        .first()
        .and_then(|packet| json_time_90k(packet.get("pts_time"))))
}

/// 把一个分段并进产物：抽裸流 → 建索引 → 追加到文件末尾 → 追加全局索引。
///
/// 首段额外做完整校验（编码/尺寸/帧率/关键帧/音频轨），这样"用哪档编码策略"
/// 在推流开始之前就确定了，不会出现"已经推了两秒才发现要换策略"。
#[allow(clippy::too_many_arguments)]
fn ingest_segment(
    ffmpeg: &str,
    ffprobe: &str,
    segment: &Path,
    profile: &MediaProfile,
    prepared: &PreparedFileProfile,
    video_writer: &mut File,
    audio_writer: Option<&mut File>,
    g711: Option<&mut G711Assembler>,
    timeline_origin: &mut Option<i64>,
    is_first: bool,
    source_has_audio: bool,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let seg_video = segment.with_extension("vid.tmp");
    let seg_audio = segment.with_extension("aud.tmp");

    extract_video(ffmpeg, segment, profile.video_codec, &seg_video, cancel)?;
    let audio_extracted = if source_has_audio {
        extract_audio(ffmpeg, segment, profile.audio_codec, &seg_audio, cancel)?;
        true
    } else {
        false
    };
    let audio_bytes = if audio_extracted {
        fs::metadata(&seg_audio).map(|meta| meta.len()).unwrap_or(0)
    } else {
        0
    };

    let raw_video = build_video_index(ffprobe, segment, &seg_video, profile, cancel)?;
    // G.711 的 AU 由 `G711Assembler` 按固定块切，不走裸流包解析。
    let raw_audio = if audio_extracted && g711.is_none() {
        build_audio_index(ffprobe, segment, &seg_audio, profile, is_first, cancel)?
    } else {
        Vec::new()
    };

    if raw_video.is_empty() {
        let _ = fs::remove_file(&seg_video);
        let _ = fs::remove_file(&seg_audio);
        return Err("分段转码结果不包含视频访问单元".to_string());
    }
    if is_first {
        if !raw_video.first().is_some_and(|index| index.key_frame) {
            return Err("视频首帧不是关键帧".to_string());
        }
        if source_has_audio && (audio_bytes == 0 || (g711.is_none() && raw_audio.is_empty())) {
            return Err("源文件包含音频轨，但转码结果缺少音频轨".to_string());
        }
        verify_output_stream(ffprobe, segment, profile, audio_extracted, cancel)?;
    }

    // 全局时间轴原点取第一段视频的最小 PTS。视频是时间轴的锚，音频只能往上对齐：
    // AAC 编码器在片头会吐出一个 PTS 为负的"预热包"（`-avoid_negative_ts disabled`
    // 下负时间戳被保留），它比视频起点还早——如果让它参与取 min，整条视频时间轴会被
    // 往后推一个音频帧（64ms），第一帧就不是 0 了。
    let origin = *timeline_origin.get_or_insert_with(|| {
        raw_video
            .iter()
            .map(|index| index.pts_90k)
            .min()
            .unwrap_or(0)
    });
    // 早于视频起点的音频 AU 属于片头预热，播放器本来也会丢弃；这里直接不索引，
    // 与之对应的字节随后也一并被裁掉，产物和索引才对得上。
    let raw_audio: Vec<_> = raw_audio
        .into_iter()
        .filter(|index| index.pts_90k >= origin)
        .collect();

    let video_base = video_writer
        .metadata()
        .map_err(|error| format!("读取视频产物大小失败: {error}"))?
        .len();
    // 产物里只留索引覆盖得到的字节，保证"文件长度 == 最后一个 AU 的结尾"这条不变式。
    let video_start = raw_video.iter().map(|index| index.offset).min().unwrap_or(0);
    let video_end = raw_video
        .iter()
        .map(|index| index.offset.saturating_add(index.length))
        .max()
        .unwrap_or(0);
    append_file_range(video_writer, &seg_video, video_start, video_end)?;
    let video_entries: Vec<VideoAccessUnitIndex> = raw_video
        .into_iter()
        .map(|index| VideoAccessUnitIndex {
            offset: video_base.saturating_add(index.offset.saturating_sub(video_start)),
            length: index.length,
            pts_90k: normalize_pts(index.pts_90k, origin),
            duration_90k: index.duration_90k,
            key_frame: index.key_frame,
            codec: index.codec,
        })
        .collect();

    let mut audio_entries = Vec::new();
    if let Some(writer) = audio_writer {
        if let Some(assembler) = g711 {
            // G.711：字节原样追加，切块交给装配器（跨段半块会攒着）。
            let audio_base = writer
                .metadata()
                .map_err(|error| format!("读取音频产物大小失败: {error}"))?
                .len();
            let bytes = fs::read(&seg_audio)
                .map_err(|error| format!("读取分段 G.711 裸流失败: {error}"))?;
            append_file_range(writer, &seg_audio, 0, bytes.len() as u64)?;
            let start_pts = if assembler.next_pts.is_none() {
                first_audio_pts_90k(ffprobe, segment, cancel)?
            } else {
                None
            };
            assembler.push(audio_base, &bytes, start_pts, origin, &mut audio_entries);
        } else if !raw_audio.is_empty() {
            // AAC 每段开头的包会被 mkv 标成"编码器预热"，早先按解码帧建索引时它进不了
            // 索引，字节却照样写进产物——分段越多，孤儿字节越多。现在索引按包建，理论上
            // 覆盖整段；这里仍按索引覆盖范围追加，把"文件长度 == 最后一个 AU 的结尾"
            // 这条不变式钉死。
            let audio_start = raw_audio
                .iter()
                .map(|index| index.offset)
                .min()
                .unwrap_or(0);
            let audio_end = raw_audio
                .iter()
                .map(|index| index.offset.saturating_add(index.length))
                .max()
                .unwrap_or(0);
            let audio_base = writer
                .metadata()
                .map_err(|error| format!("读取音频产物大小失败: {error}"))?
                .len();
            append_file_range(writer, &seg_audio, audio_start, audio_end)?;
            audio_entries = raw_audio
                .into_iter()
                .map(|index| AudioAccessUnitIndex {
                    offset: audio_base.saturating_add(index.offset.saturating_sub(audio_start)),
                    length: index.length,
                    pts_90k: normalize_pts(index.pts_90k, origin),
                    duration_90k: index.duration_90k,
                    sample_rate_hz: index.sample_rate_hz,
                    sample_count: index.sample_count,
                    codec: index.codec,
                })
                .collect();
        }
    }

    write_list(&prepared.video_index).extend(video_entries);
    write_list(&prepared.audio_index).extend(audio_entries);
    refresh_duration(prepared);

    let _ = fs::remove_file(&seg_video);
    let _ = fs::remove_file(&seg_audio);
    Ok(())
}

/// 时长随索引增长重算。循环播放只在全片完成后才用它，那时数值已经稳定。
fn refresh_duration(prepared: &PreparedFileProfile) {
    let duration = {
        let video = read_list(&prepared.video_index);
        let audio = read_list(&prepared.audio_index);
        media_duration(&video, &audio)
    };
    prepared.duration_90k.store(duration, Ordering::Release);
}

/// 把 `source` 里 `[start, end)` 这段字节追加到 `writer` 末尾。
fn append_file_range(
    writer: &mut File,
    source: &Path,
    start: u64,
    end: u64,
) -> Result<(), String> {
    let mut reader =
        File::open(source).map_err(|error| format!("打开分段裸流失败: {error}"))?;
    if start > 0 {
        reader
            .seek(SeekFrom::Start(start))
            .map_err(|error| format!("定位分段裸流失败: {error}"))?;
    }
    let mut remaining = end.saturating_sub(start);
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    while remaining > 0 {
        let chunk = usize::try_from(remaining.min(COPY_BUFFER_BYTES as u64))
            .unwrap_or(COPY_BUFFER_BYTES);
        let count = reader
            .read(&mut buffer[..chunk])
            .map_err(|error| format!("读取分段裸流失败: {error}"))?;
        if count == 0 {
            return Err("分段裸流在预期长度之前结束".to_string());
        }
        writer
            .write_all(&buffer[..count])
            .map_err(|error| format!("写入产物失败: {error}"))?;
        remaining = remaining.saturating_sub(count as u64);
    }
    writer.flush().map_err(|error| format!("刷新产物失败: {error}"))
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
    if !manifest.complete {
        return Err("媒体缓存尚未准备完成".into());
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
    Ok(PreparedFileProfile::sealed(
        cache_key.to_string(),
        cache_dir.to_path_buf(),
        manifest.profile,
        video_file,
        audio_file,
        index.video,
        index.audio,
        index.duration_90k,
    ))
}

/// 解析 `HH:MM:SS.ffffff` 为微秒。ffmpeg 各版本的进度输出都带这一行。
fn parse_hhmmss_us(text: &str) -> Option<u64> {
    let mut parts = text.split(':');
    let hours = parts.next()?.parse::<u64>().ok()?;
    let minutes = parts.next()?.parse::<f64>().ok()?;
    let seconds = parts.next()?.parse::<f64>().ok()?;
    let total = hours as f64 * 3600.0 + minutes * 60.0 + seconds;
    (total.is_finite() && total >= 0.0).then(|| (total * 1_000_000.0) as u64)
}

/// 读取 `-progress` 文件里最后一个 `out_time` 值（微秒）。
///
/// 优先用 `out_time=` 字符串形式：`out_time_us` 是较新版本才有的字段，
/// 而 `out_time_ms` 在旧版本里名不副实（值其实是微秒），两者都只作兜底。
fn read_progress_out_time_us(path: &Path) -> Option<u64> {
    let text = fs::read_to_string(path).ok()?;
    let mut latest = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("out_time_us=") {
            if let Ok(us) = value.trim().parse::<u64>() {
                latest = Some(us);
            }
        } else if let Some(value) = line.strip_prefix("out_time_ms=") {
            if let Ok(us) = value.trim().parse::<u64>() {
                latest = Some(us);
            }
        } else if let Some(value) = line.strip_prefix("out_time=") {
            if let Some(us) = parse_hhmmss_us(value.trim()) {
                latest = Some(us);
            }
        }
    }
    latest
}

/// 取源视频流时长（微秒），作为转码进度的分母。
///
/// 流级时长在部分容器里是 `N/A`，此时退回容器总时长；都拿不到就不显示百分比，
/// 不影响准备流程本身。
fn probe_stream_duration_us(ffprobe: &str, path: &Path, cancel: &AtomicBool) -> Option<u64> {
    let seconds_from = |value: &Value, scope: &str| {
        value
            .get(scope)
            .and_then(|node| match scope {
                "streams" => node.as_array().and_then(|streams| streams.first()),
                _ => Some(node),
            })
            .and_then(|node| node.get("duration"))
            .and_then(Value::as_str)
            .and_then(|text| text.trim().parse::<f64>().ok())
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
    };
    let stream = run_probe(
        ffprobe,
        [
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=duration",
            "-of",
            "json",
        ],
        path,
        cancel,
    )
    .ok()
    .and_then(|value| seconds_from(&value, "streams"));
    let seconds = stream.or_else(|| {
        run_probe(
            ffprobe,
            ["-show_entries", "format=duration", "-of", "json"],
            path,
            cancel,
        )
        .ok()
        .and_then(|value| seconds_from(&value, "format"))
    })?;
    Some((seconds * 1_000_000.0) as u64)
}

/// 构造分段转码命令。
///
/// 输出按 [`SEGMENT_SECONDS`] 切成独立 mkv 段，每段都从关键帧开始，并用
/// `-reset_timestamps 0` 保留原始时间戳——段与段拼起来就是一条连续时间轴，
/// 不需要在消费侧人为加偏移。这是"边转边推"能成立的前提。
///
/// 返回 `Command` 而不是直接执行：调用方要一边等进程跑、一边消费已经落盘的段。
fn build_transcode_command(
    ffmpeg: &str,
    source: &Path,
    profile: &MediaProfile,
    segment_dir: &Path,
    strategy: TranscodeStrategy,
) -> Result<Command, String> {
    let gop_frames = profile
        .video_fps
        .checked_mul(profile.keyframe_interval_seconds)
        .ok_or_else(|| "GOP 帧数溢出".to_string())?;
    let filter = format!(
        "scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2:color=black,fps={}",
        profile.width, profile.height, profile.width, profile.height, profile.video_fps
    );
    let mut command = common::hidden_command(ffmpeg);
    command.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]);
    command.arg(source);
    command.args(["-map", "0:v:0", "-map", "0:a:0?"]);

    // `-progress` 把周期性进度写成 key=value 行。写到文件而不是 stdout：
    // 主循环保持 25ms 轮询（取消响应快），不必阻塞在读管道上等下一行。
    command.args(["-nostats", "-progress"]);
    command.arg(progress_file_path(segment_dir));

    // 根据策略决定视频参数。
    match strategy {
        TranscodeStrategy::Passthrough => {
            // 直通:只换容器、不重编码，仍然整片复制（源编码/分辨率/帧率与目标一致时才有此档）。
            command.args(["-c:v", "copy"]);
        }
        TranscodeStrategy::Hardware(backend) => {
            // 硬件编码:用 P2a 选出的编码器。
            let codec_for_encoder = match profile.video_codec {
                MediaVideoCodec::H264 => VideoCodec::H264,
                MediaVideoCodec::H265 => VideoCodec::H265,
            };
            let params = encoder::EncodeParams {
                bitrate_kbps: profile.bitrate_kbps,
                gop: gop_frames,
            };
            let encoder_args = encoder::encoder_args(backend, codec_for_encoder, params);
            command.args(["-vf", &filter, "-fps_mode", "cfr"]);
            for arg in encoder_args {
                command.arg(arg);
            }
        }
        TranscodeStrategy::Software => {
            // 软编:libx264 / libx265,沿用 v1 参数。
            let video_encoder = match profile.video_codec {
                MediaVideoCodec::H264 => "libx264",
                MediaVideoCodec::H265 => "libx265",
            };
            command.args(["-vf", &filter, "-fps_mode", "cfr", "-c:v", video_encoder]);
            command.args(["-b:v", &format!("{}k", profile.bitrate_kbps)]);
            command.args(["-g", &gop_frames.to_string(), "-keyint_min", &gop_frames.to_string()]);
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
        }
    }

    append_audio_args(&mut command, profile);
    command.args([
        "-avoid_negative_ts",
        "disabled",
        "-f",
        "segment",
        "-segment_time",
        &SEGMENT_SECONDS.to_string(),
        "-segment_format",
        "matroska",
        "-reset_timestamps",
        "0",
    ]);
    command.arg(segment_dir.join(SEGMENT_PATTERN));
    Ok(command)
}

fn progress_file_path(segment_dir: &Path) -> PathBuf {
    segment_dir.join("transcode.ffprogress")
}

/// 段文件名模板。5 位序号足够覆盖 100 小时以上的素材。
const SEGMENT_PATTERN: &str = "seg-%05d.mkv";

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
    let mut command = common::hidden_command(ffmpeg);
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
    let mut command = common::hidden_command(ffmpeg);
    command.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]);
    command.arg(container);
    command.args(["-map", "0:a:0", "-c:a", "copy", "-f", format]);
    command.arg(output);
    run_cancellable(command, cancel, "ffmpeg 提取音频访问单元")
        .map_err(|error| format!("提取 {expected_codec} 音频失败: {error}"))
}

fn run_cancellable(
    command: Command,
    cancel: &AtomicBool,
    operation: &str,
) -> Result<(), String> {
    run_cancellable_with_progress(command, cancel, operation, None, None, None)
}

/// 运行可取消的外部命令，并可选地上报阶段内进度。
///
/// `progress_file` 是传给 ffmpeg `-progress` 的路径：主循环按固定间隔读这个文件，
/// 而不是阻塞在 stdout 上等下一行——后者会让取消响应被 ffmpeg 的输出节奏绑死。
fn run_cancellable_with_progress(
    mut command: Command,
    cancel: &AtomicBool,
    operation: &str,
    progress_file: Option<&Path>,
    total_us: Option<u64>,
    progress: Option<&FilePreparationProgressCallback>,
) -> Result<(), String> {
    check_cancel(cancel)?;
    if let Some(path) = progress_file {
        let _ = fs::remove_file(path);
    }
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("{operation}启动失败: {error}"))?;
    // stderr 必须一直在读：管道写满会把子进程阻塞在日志输出上，现场表现就是"转码卡死"。
    // 收集放独立线程，主线程才能继续用 25ms 轮询处理取消。
    let mut stderr_reader = child.stderr.take().map(|stderr| {
        std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(stderr);
            let mut data = Vec::new();
            let _ = reader.read_to_end(&mut data);
            data
        })
    });
    let mut last_report = Instant::now()
        .checked_sub(PROGRESS_REPORT_INTERVAL)
        .unwrap_or_else(Instant::now);
    loop {
        if cancel.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(reader) = stderr_reader.take() {
                let _ = reader.join();
            }
            if let Some(path) = progress_file {
                let _ = fs::remove_file(path);
            }
            return Err(format!("{operation}已取消"));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let stderr = stderr_reader
                    .take()
                    .map(|reader| reader.join().unwrap_or_default())
                    .unwrap_or_default();
                if status.success() {
                    // 收尾补一次满格，避免进度停在最后一格。
                    if let (Some(total), Some(callback)) = (total_us, progress) {
                        report_with_progress(
                            Some(callback),
                            FilePreparationPhase::Transcoding,
                            Some(total),
                            Some(total),
                        );
                    }
                    if let Some(path) = progress_file {
                        let _ = fs::remove_file(path);
                    }
                    return Ok(());
                }
                if let Some(path) = progress_file {
                    let _ = fs::remove_file(path);
                }
                return Err(format!(
                    "{operation}失败: {}",
                    last_stderr_line_bytes(&stderr)
                ));
            }
            Ok(None) => {
                if let (Some(path), Some(total), Some(callback)) =
                    (progress_file, total_us, progress)
                {
                    if last_report.elapsed() >= PROGRESS_REPORT_INTERVAL {
                        last_report = Instant::now();
                        if let Some(out_us) = read_progress_out_time_us(path) {
                            report_with_progress(
                                Some(callback),
                                FilePreparationPhase::Transcoding,
                                Some(out_us.min(total)),
                                Some(total),
                            );
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                if let Some(reader) = stderr_reader.take() {
                    let _ = reader.join();
                }
                if let Some(path) = progress_file {
                    let _ = fs::remove_file(path);
                }
                return Err(format!("{operation}等待失败: {error}"));
            }
        }
    }
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
    is_first: bool,
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
    if frame_values.len() > packet_values.len() {
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
    // 容器里的音频包与裸流里的 ADTS 包一一对应，解码帧列表却总是少一个：每个 mkv 文件
    // 的第一个音频包都会被标成 skip_samples（CodecDelay 是每文件记录一次）。帧列表因此
    // 少一帧，可按帧建索引会让每段都丢一个包——60 秒素材少 0.38 秒，片子越长差得越多。
    // 所以索引按包建，帧列表只用来补每个包的样本数；只有第一段的首包是真预热，才跳过。
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
    // 只有**第一段**的首包是真·编码器预热（AAC 会吐出一个 PTS 早于首帧的包）；后续
    // 分段的首包被 mkv 标成 skip_samples 纯属每个文件都要记一次 CodecDelay 的产物，
    // 它承载的是真实音频，丢掉就会每段少一个包。
    let drop_leading = if is_first { leading_packets } else { 0 };
    let mut index = Vec::with_capacity(packet_values.len());
    // 容器包缺时间戳时的兜底：按前一个包的起点加一个包的时长往后推。
    let mut fallback_pts: Option<i64> = None;
    for (packet_index, packet) in packet_values.iter().enumerate() {
        let offset = json_u64(packet.get("pos"))
            .ok_or_else(|| format!("音频包 {packet_index} 缺少裸流偏移"))?;
        let length = json_u64(packet.get("size"))
            .ok_or_else(|| format!("音频包 {packet_index} 缺少裸流长度"))?;
        let sample_count = packet_index
            .checked_sub(leading_packets)
            .and_then(|frame_index| frame_values.get(frame_index))
            .and_then(|frame| json_u64(frame.get("nb_samples")))
            .map(|samples| u32::try_from(samples).unwrap_or(u32::MAX))
            .filter(|samples| *samples > 0)
            .unwrap_or_else(|| default_audio_samples(profile.audio_codec));
        let duration_90k = duration_for_samples(sample_count, sample_rate);
        let pts_90k = container_packet_values
            .get(packet_index)
            .and_then(|packet| json_time_90k(packet.get("pts_time")))
            .or(fallback_pts)
            .unwrap_or_default();
        fallback_pts = Some(
            pts_90k.saturating_add(i64::try_from(duration_90k).unwrap_or(i64::MAX)),
        );
        if packet_index < drop_leading {
            continue;
        }
        index.push(RawAudioAccessUnitIndex {
            offset,
            length,
            pts_90k,
            duration_90k,
            sample_rate_hz: sample_rate,
            sample_count,
            codec: profile.audio_codec,
        });
    }
    Ok(index)
}

fn run_probe<const N: usize>(
    ffprobe: &str,
    args: [&str; N],
    input: &Path,
    cancel: &AtomicBool,
) -> Result<Value, String> {
    check_cancel(cancel)?;
    let mut child = common::hidden_command(ffprobe)
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
    report_with_progress(callback, phase, None, None);
}

/// 带阶段内进度的上报。`completed`/`total` 用同一单位（字节或微秒），
/// 前端据此刻画进度条与剩余时间。
fn report_with_progress(
    callback: Option<&FilePreparationProgressCallback>,
    phase: FilePreparationPhase,
    completed: Option<u64>,
    total: Option<u64>,
) {
    if let Some(callback) = callback {
        callback(FilePreparationProgress {
            phase,
            completed,
            total,
        });
    }
}

#[derive(Debug, Clone)]
struct SourceSnapshot {
    length: u64,
    modified: Option<SystemTime>,
    /// 首尾采样指纹：用于在元数据未变时低成本复核内容是否被换掉。
    sample_sha256: String,
    sha256: String,
}

/// 一条快速指纹记录。字段全部参与命中判断，任一不符就退回全量哈希。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SourceIndexEntry {
    length: u64,
    modified_ns: u64,
    sample_sha256: String,
    sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SourceIndex {
    version: u32,
    entries: HashMap<String, SourceIndexEntry>,
}

impl SourceIndex {
    fn empty() -> Self {
        Self {
            version: SOURCE_INDEX_VERSION,
            entries: HashMap::new(),
        }
    }
}

fn source_index_path(root: &Path) -> PathBuf {
    root.join(SOURCE_INDEX_FILE)
}

/// 读取快速指纹索引。文件缺失、损坏或版本不符都当作空索引重建——
/// 它只影响速度，不能成为媒体准备的失败原因。
fn load_source_index(root: &Path) -> SourceIndex {
    match read_json::<SourceIndex>(&source_index_path(root)) {
        Ok(index) if index.version == SOURCE_INDEX_VERSION => index,
        _ => SourceIndex::empty(),
    }
}

fn store_source_index(root: &Path, index: &SourceIndex) {
    let path = source_index_path(root);
    let temp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let Ok(data) = serde_json::to_vec(index) else {
        return;
    };
    // 缓存根目录可能还不存在：首次启动时 `SourceSnapshot::capture` 早于
    // `prepare_file_profile` 的 create_dir_all 执行，不补建就会静默写失败，
    // 表现是"每次都算一遍整片 SHA256"。
    if fs::create_dir_all(root).is_err() {
        return;
    }
    if fs::write(&temp, data).is_ok() {
        let _ = fs::rename(&temp, &path);
    }
}

fn metadata_modified_ns(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|delta| delta.as_nanos() as u64)
        .unwrap_or(0)
}

/// 计算首尾采样指纹：文件很小时就是全文件，稍大时只读头尾各 64KB。
///
/// 与「大小 + 修改时间」组合，足以识别正常的文件替换；即使有人刻意把
/// 修改时间改回去，采样不同仍会触发全量重算。
fn quick_sample_sha256(path: &Path, length: u64) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| format!("打开源文件失败: {error}"))?;
    let mut digest = Sha256::new();
    digest.update(length.to_le_bytes());
    let mut buffer = vec![0_u8; QUICK_SAMPLE_BYTES as usize];
    let head = length.min(QUICK_SAMPLE_BYTES);
    if head > 0 {
        file.read_exact(&mut buffer[..head as usize])
            .map_err(|error| format!("读取源文件头部失败: {error}"))?;
        digest.update(&buffer[..head as usize]);
    }
    if length > QUICK_SAMPLE_BYTES {
        let tail = QUICK_SAMPLE_BYTES.min(length - QUICK_SAMPLE_BYTES);
        file.seek(SeekFrom::End(-(tail as i64)))
            .map_err(|error| format!("定位源文件尾部失败: {error}"))?;
        file.read_exact(&mut buffer[..tail as usize])
            .map_err(|error| format!("读取源文件尾部失败: {error}"))?;
        digest.update(&buffer[..tail as usize]);
    }
    Ok(hex_encode(&digest.finalize()))
}

impl SourceSnapshot {
    fn capture(
        path: &Path,
        cancel: &AtomicBool,
        progress: Option<&FilePreparationProgressCallback>,
    ) -> Result<Self, String> {
        let metadata =
            fs::metadata(path).map_err(|error| format!("读取源文件信息失败: {error}"))?;
        let length = metadata.len();
        let modified_ns = metadata_modified_ns(&metadata);
        // 用规范化路径做键：同一个素材经符号链接/相对路径选中时也能命中。
        let index_key = path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned();
        let sample_sha256 = quick_sample_sha256(path, length)?;

        let cache_root = cache_root();
        let mut index = load_source_index(&cache_root);
        let reused = index.entries.get(&index_key).is_some_and(|entry| {
            entry.length == length
                && entry.modified_ns == modified_ns
                && entry.sample_sha256 == sample_sha256
        });
        let sha256 = if reused {
            let cached = index
                .entries
                .get(&index_key)
                .map(|entry| entry.sha256.clone())
                .unwrap_or_default();
            tracing::debug!(path = %path.display(), "源文件快速指纹命中，跳过全量哈希");
            cached
        } else {
            let sha256 = sha256_file(path, cancel, progress.map(|callback| (callback, length)))?;
            // 条目数到顶就整体重建，避免长期使用后索引无限增长。
            if index.entries.len() >= SOURCE_INDEX_MAX_ENTRIES {
                index.entries.clear();
            }
            index.entries.insert(
                index_key,
                SourceIndexEntry {
                    length,
                    modified_ns,
                    sample_sha256: sample_sha256.clone(),
                    sha256: sha256.clone(),
                },
            );
            store_source_index(&cache_root, &index);
            sha256
        };
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
            sample_sha256,
            sha256,
        })
    }
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
    // 元数据一致时用首尾采样复核即可：这里曾经每次都重算整片 SHA256，
    // 等于每档转码策略（直通/硬件/软编）都白读一遍整个文件，长素材代价是分钟级。
    // 正确性由前面的复制快照阶段保证（那一步已完整比对过 SHA256）。
    let sample = quick_sample_sha256(path, snapshot.length)?;
    if sample != snapshot.sample_sha256 {
        return Err("源文件内容在准备期间发生变化，已放弃缓存发布".into());
    }
    Ok(())
}

/// 计算整片 SHA256。传入 `progress` 时按字节数上报进度（首次准备大文件时才看得到）。
fn sha256_file(
    path: &Path,
    cancel: &AtomicBool,
    progress: Option<(&FilePreparationProgressCallback, u64)>,
) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| format!("打开源文件失败: {error}"))?;
    let mut digest = Sha256::new();
    // 同 copy_source_snapshot：缓冲一律走堆，避免 2MB 线程栈被顶爆。
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut hashed = 0_u64;
    let mut last_report = Instant::now();
    loop {
        check_cancel(cancel)?;
        let count = file
            .read(&mut buffer[..])
            .map_err(|error| format!("读取源文件失败: {error}"))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        hashed = hashed.saturating_add(count as u64);
        if let Some((callback, total)) = progress {
            if last_report.elapsed() >= PROGRESS_REPORT_INTERVAL {
                last_report = Instant::now();
                report_with_progress(
                    Some(callback),
                    FilePreparationPhase::HashingSource,
                    Some(hashed.min(total)),
                    Some(total),
                );
            }
        }
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
    let output = common::hidden_command(path)
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
    // 统一走 `source::ffprobe_bin`：Windows 下必须显式带 `.exe`，
    // 三处各自实现曾因扩展名/候选顺序不一致而在 Windows 上全部落空。
    crate::source::ffprobe_bin()
}
