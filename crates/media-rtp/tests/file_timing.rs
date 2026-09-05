use media_rtp::file_profile::{prepare_file_profile, PreparedFileProfile, PreparedFileSource};
use media_rtp::profile::{MediaAudioCodec, MediaProfile, MediaVideoCodec};
use media_rtp::{ffmpeg_bin, MediaEvent, VideoSource};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

const CLOCK_HZ: u64 = 90_000;
const SOURCE_VIDEO_SECONDS: u64 = 60;
const SOURCE_AUDIO_DELAY_SECONDS: f64 = 0.5;
const TARGET_FPS: [u32; 4] = [15, 20, 25, 30];
const LOOP_COUNT: u32 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EventMark {
    kind: u8,
    pts_90k: u64,
    duration_90k: u64,
    sample_rate_hz: u32,
    sample_count: u32,
    key_frame: bool,
}

/// 用真实 ffmpeg/ffprobe 验收文件 profile 的视频时长、音轨偏移、循环和 seek。
///
/// 该测试刻意忽略：它会生成约 60 秒素材并为四个 FPS 各做一次转码。
#[test]
#[ignore = "需要真实 ffmpeg/ffprobe；显式运行 cargo test -p media-rtp --test file_timing -- --ignored --nocapture"]
fn file_profile_timing_duration_offset_loop_and_seek() {
    let ffmpeg = ffmpeg_bin().unwrap_or_else(|| panic!("未找到 ffmpeg，不能把缺少依赖标记为通过"));
    let ffprobe = locate_ffprobe(&ffmpeg)
        .unwrap_or_else(|| panic!("未找到 ffprobe，不能把缺少依赖标记为通过"));
    let output_root = prepare_output_root();
    let cache_root = output_root.join("cache");
    fs::create_dir_all(&cache_root).expect("创建文件时长测试缓存目录失败");
    std::env::set_var("UVP_MEDIA_PROFILE_CACHE_DIR", &cache_root);

    let source = output_root.join("source-60s-30fps-offset-audio.mkv");
    let source_command = make_synthetic_source(&ffmpeg, &source);
    let source_probe = probe_streams(&ffprobe, &source, "探测合成源");
    let source_video = stream_with_type(&source_probe, "video", "合成源视频");
    let source_audio = stream_with_type(&source_probe, "audio", "合成源音频");
    assert_eq!(
        source_video.get("codec_name").and_then(Value::as_str),
        Some("h264"),
        "合成源视频应为 H.264"
    );
    assert_eq!(
        source_audio.get("codec_name").and_then(Value::as_str),
        Some("aac"),
        "合成源音频应为 AAC"
    );
    let source_video_first_pts = first_frame_pts(&ffprobe, &source, "v:0", "合成源视频");
    let source_audio_first_pts = first_frame_pts(&ffprobe, &source, "a:0", "合成源音频");
    let expected_audio_delay_90k = (SOURCE_AUDIO_DELAY_SECONDS * CLOCK_HZ as f64).round() as u64;
    assert_close_u64(
        source_audio_first_pts,
        source_video_first_pts.saturating_add(expected_audio_delay_90k),
        900,
        "合成源音轨应比视频晚 500ms",
    );
    let source_count_probe = probe_json(
        &ffprobe,
        vec![
            "-v",
            "error",
            "-count_frames",
            "-show_entries",
            "stream=codec_type,nb_read_frames",
            "-of",
            "json",
        ],
        &source,
        "统计合成源帧数",
    );
    let source_video_frame_count = stream_with_type(&source_count_probe, "video", "合成源帧数")
        .get("nb_read_frames")
        .and_then(value_u64)
        .expect("合成源缺少视频帧数");
    assert_close_u64(
        source_video_frame_count,
        SOURCE_VIDEO_SECONDS * 30,
        1,
        "合成源应为 60 秒 30 FPS",
    );

    let mut profile_records = Vec::with_capacity(TARGET_FPS.len());
    let mut prepared_for_loop = None;
    for &fps in &TARGET_FPS {
        let profile = test_profile(fps);
        let cancel = AtomicBool::new(false);
        let prepared = prepare_file_profile(&source, &profile, &cancel, None)
            .unwrap_or_else(|error| panic!("{fps} FPS prepare_file_profile 失败：{error}"));
        let record = verify_profile_timing(
            &ffprobe,
            &prepared,
            &profile,
            source_audio_first_pts,
            expected_audio_delay_90k,
        );
        if fps == 25 {
            prepared_for_loop = Some(prepared.clone());
        }
        profile_records.push(record);
    }
    let prepared_for_loop = prepared_for_loop.expect("缺少 25 FPS 循环测试 profile");
    let loop_seek_record = verify_loop_and_seek(&prepared_for_loop);

    let report_path = output_root.join("file-timing-report.json");
    let report = json!({
        "schema_version": 1,
        "source": source,
        "source_command": source_command,
        "source_probe": source_probe,
        "source_video_frame_count": source_video_frame_count,
        "source_video_first_pts_90k": source_video_first_pts,
        "source_audio_first_pts_90k": source_audio_first_pts,
        "expected_audio_delay_90k": expected_audio_delay_90k,
        "ffmpeg": ffmpeg,
        "ffprobe": ffprobe,
        "cache_root": cache_root,
        "profiles": profile_records,
        "loop_seek": loop_seek_record,
    });
    fs::write(
        &report_path,
        serde_json::to_vec_pretty(&report).expect("序列化文件时长测试报告失败"),
    )
    .expect("写入文件时长测试报告失败");
    println!(
        "[file-timing] 通过：{} FPS profile、{} 次循环、{} 次 seek；产物目录 {}，报告 {}",
        TARGET_FPS.len(),
        LOOP_COUNT,
        LOOP_COUNT,
        output_root.display(),
        report_path.display(),
    );
}

fn test_profile(video_fps: u32) -> MediaProfile {
    MediaProfile {
        width: 640,
        height: 480,
        video_fps,
        bitrate_kbps: 600,
        keyframe_interval_seconds: 1,
        video_codec: MediaVideoCodec::H264,
        audio_codec: MediaAudioCodec::Aac,
        audio_sample_rate_hz: 8000,
    }
}

fn verify_profile_timing(
    ffprobe: &str,
    prepared: &PreparedFileProfile,
    profile: &MediaProfile,
    source_audio_first_pts: u64,
    expected_audio_delay_90k: u64,
) -> Value {
    let label = format!("{} FPS", profile.video_fps);
    assert_eq!(prepared.profile(), profile, "{label} profile 被改变");
    assert!(!prepared.video_index().is_empty(), "{label} 没有视频 AU");
    assert!(!prepared.audio_index().is_empty(), "{label} 没有音频 AU");

    let first_video = prepared.video_index().first().expect("视频 AU 不应为空");
    let last_video = prepared.video_index().last().expect("视频 AU 不应为空");
    assert_eq!(first_video.pts_90k, 0, "{label} 视频首个 PTS 应从 0 开始");
    let video_span_90k = last_video
        .pts_90k
        .saturating_add(last_video.duration_90k)
        .saturating_sub(first_video.pts_90k);
    let expected_video_span_90k = SOURCE_VIDEO_SECONDS * CLOCK_HZ;
    let video_tolerance_90k = CLOCK_HZ / u64::from(profile.video_fps) + 1;
    assert_close_u64(
        video_span_90k,
        expected_video_span_90k,
        video_tolerance_90k,
        &format!("{label} 视频时长应为 60 秒，允许一帧误差"),
    );
    let expected_video_frames = SOURCE_VIDEO_SECONDS as usize * profile.video_fps as usize;
    assert_close_u64(
        prepared.video_index().len() as u64,
        expected_video_frames as u64,
        1,
        &format!("{label} 视频帧数"),
    );

    let first_audio = prepared.audio_index().first().expect("音频 AU 不应为空");
    let last_audio = prepared.audio_index().last().expect("音频 AU 不应为空");
    assert_eq!(
        first_audio.codec,
        MediaAudioCodec::Aac,
        "{label} 音频编码应为 AAC"
    );
    assert_eq!(
        first_audio.sample_rate_hz, 8000,
        "{label} 音频采样率应为 8kHz"
    );
    assert!(
        prepared
            .audio_index()
            .iter()
            .all(|audio| audio.sample_count > 0 && audio.duration_90k > 0),
        "{label} 音频 AU 必须包含样本数和持续时间"
    );
    assert_close_u64(
        first_audio.pts_90k,
        source_audio_first_pts,
        900,
        &format!("{label} 音轨 500ms 起始偏移"),
    );
    assert_close_u64(
        first_audio.pts_90k,
        expected_audio_delay_90k,
        900,
        &format!("{label} 音频首个 PTS"),
    );
    let audio_span_90k = last_audio
        .pts_90k
        .saturating_add(last_audio.duration_90k)
        .saturating_sub(first_audio.pts_90k);
    let expected_audio_span_90k = SOURCE_VIDEO_SECONDS * CLOCK_HZ;
    let audio_au_tolerance_90k = first_audio.duration_90k;
    let audio_padding_90k = audio_span_90k as i64 - expected_audio_span_90k as i64;
    assert_close_u64(
        audio_span_90k,
        expected_audio_span_90k,
        audio_au_tolerance_90k,
        &format!("{label} 音频有效时长，AAC padding 允许一个 AU"),
    );

    let container_path = prepared.cache_dir().join("media.mkv");
    assert!(container_path.is_file(), "{label} 缺少输出容器");
    let container_video_probe = probe_json(
        ffprobe,
        vec![
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            "stream=codec_name,width,height,r_frame_rate,nb_read_frames",
            "-of",
            "json",
        ],
        &container_path,
        &format!("探测 {label} 输出视频"),
    );
    let container_video = first_stream(&container_video_probe, &format!("{label} 输出视频"));
    assert_eq!(
        container_video.get("codec_name").and_then(Value::as_str),
        Some("h264"),
        "{label} 输出视频编码不匹配"
    );
    assert_eq!(
        container_video.get("width").and_then(value_u64),
        Some(u64::from(profile.width)),
        "{label} 输出视频宽度不匹配"
    );
    assert_eq!(
        container_video.get("height").and_then(value_u64),
        Some(u64::from(profile.height)),
        "{label} 输出视频高度不匹配"
    );
    let output_fps = container_video
        .get("r_frame_rate")
        .and_then(Value::as_str)
        .map(parse_rational)
        .expect("输出视频缺少 r_frame_rate");
    assert!(
        (output_fps - f64::from(profile.video_fps)).abs() < 0.001,
        "{label} 输出视频 FPS {output_fps} != {}",
        profile.video_fps
    );
    assert_close_u64(
        container_video
            .get("nb_read_frames")
            .and_then(value_u64)
            .expect("输出视频缺少帧数") as u64,
        expected_video_frames as u64,
        1,
        &format!("{label} 输出容器视频帧数"),
    );

    let container_audio_probe = probe_json(
        ffprobe,
        vec![
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-count_frames",
            "-show_entries",
            "stream=codec_name,sample_rate,initial_padding,nb_read_frames",
            "-of",
            "json",
        ],
        &container_path,
        &format!("探测 {label} 输出音频"),
    );
    let container_audio = first_stream(&container_audio_probe, &format!("{label} 输出音频"));
    assert_eq!(
        container_audio.get("codec_name").and_then(Value::as_str),
        Some("aac"),
        "{label} 输出音频编码不匹配"
    );
    assert_eq!(
        container_audio.get("sample_rate").and_then(value_u64),
        Some(8000),
        "{label} 输出音频采样率不匹配"
    );

    let mut source = prepared
        .open_source_unpaced(false)
        .unwrap_or_else(|error| panic!("{label} 打开不主动限速的文件源失败：{error}"));
    assert!(!source.is_paced(), "{label} unpaced 文件源不应主动等待 PTS");
    let events = collect_events(&mut source, prepared, &label);
    assert_event_indices(&events, prepared, &label);

    json!({
        "video_fps": profile.video_fps,
        "cache_key": prepared.cache_key(),
        "cache_dir": prepared.cache_dir(),
        "video": {
            "frame_count": prepared.video_index().len(),
            "first_pts_90k": first_video.pts_90k,
            "last_end_pts_90k": last_video.pts_90k.saturating_add(last_video.duration_90k),
            "span_90k": video_span_90k,
            "expected_span_90k": expected_video_span_90k,
            "tolerance_90k": video_tolerance_90k,
            "container_r_frame_rate": container_video.get("r_frame_rate"),
        },
        "audio": {
            "au_count": prepared.audio_index().len(),
            "first_pts_90k": first_audio.pts_90k,
            "expected_delayed_pts_90k": expected_audio_delay_90k,
            "last_end_pts_90k": last_audio.pts_90k.saturating_add(last_audio.duration_90k),
            "span_from_first_90k": audio_span_90k,
            "expected_effective_span_90k": expected_audio_span_90k,
            "aac_padding_90k": audio_padding_90k,
            "aac_padding_tolerance_90k": audio_au_tolerance_90k,
            "container_sample_rate_hz": container_audio.get("sample_rate").and_then(value_u64),
            "container_initial_padding_samples": container_audio
                .get("initial_padding")
                .and_then(value_u64),
        },
        "unpaced_event_count": events.len(),
    })
}

fn collect_events(
    source: &mut PreparedFileSource,
    prepared: &PreparedFileProfile,
    label: &str,
) -> Vec<EventMark> {
    let expected_count = prepared.video_index().len() + prepared.audio_index().len();
    let mut events = Vec::with_capacity(expected_count);
    for index in 0..expected_count {
        let event = source
            .next_timed_media_event()
            .unwrap_or_else(|error| panic!("{label} 读取第 {index} 个 timed 事件失败：{error}"))
            .unwrap_or_else(|| panic!("{label} 在第 {index} 个 timed 事件提前结束"));
        events.push(event_mark(event, label));
    }
    assert!(
        source
            .next_timed_media_event()
            .unwrap_or_else(|error| panic!("{label} 读取 EOF 失败：{error}"))
            .is_none(),
        "{label} 非循环源应在所有视频/音频 AU 后结束"
    );
    events
}

fn event_mark(event: MediaEvent, label: &str) -> EventMark {
    match event {
        MediaEvent::Video(video) => EventMark {
            kind: 0,
            pts_90k: video.pts_90k,
            duration_90k: video.duration_90k,
            sample_rate_hz: 0,
            sample_count: 0,
            key_frame: video.key_frame,
        },
        MediaEvent::Audio(audio) => EventMark {
            kind: 1,
            pts_90k: audio.pts_90k,
            duration_90k: audio.duration_90k,
            sample_rate_hz: audio.sample_rate_hz,
            sample_count: audio.sample_count,
            key_frame: false,
        },
        MediaEvent::Discontinuity { .. } => {
            panic!("{label} PreparedFileSource 不应输出 discontinuity 事件")
        }
    }
}

fn assert_event_indices(events: &[EventMark], prepared: &PreparedFileProfile, label: &str) {
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].pts_90k <= pair[1].pts_90k),
        "{label} unpaced timed 事件 PTS 不单调"
    );
    let video_events = events
        .iter()
        .filter(|event| event.kind == 0)
        .collect::<Vec<_>>();
    let audio_events = events
        .iter()
        .filter(|event| event.kind == 1)
        .collect::<Vec<_>>();
    assert_eq!(
        video_events.len(),
        prepared.video_index().len(),
        "{label} 视频事件数不一致"
    );
    assert_eq!(
        audio_events.len(),
        prepared.audio_index().len(),
        "{label} 音频事件数不一致"
    );
    for (index, (event, expected)) in video_events.iter().zip(prepared.video_index()).enumerate() {
        assert_eq!(
            event.pts_90k, expected.pts_90k,
            "{label} 视频事件 {index} PTS 不一致"
        );
        assert_eq!(
            event.duration_90k, expected.duration_90k,
            "{label} 视频事件 {index} duration 不一致"
        );
        assert_eq!(
            event.key_frame, expected.key_frame,
            "{label} 视频事件 {index} 关键帧标记不一致"
        );
    }
    for (index, (event, expected)) in audio_events.iter().zip(prepared.audio_index()).enumerate() {
        assert_eq!(
            event.pts_90k, expected.pts_90k,
            "{label} 音频事件 {index} PTS 不一致"
        );
        assert_eq!(
            event.duration_90k, expected.duration_90k,
            "{label} 音频事件 {index} duration 不一致"
        );
        assert_eq!(
            event.sample_rate_hz, expected.sample_rate_hz,
            "{label} 音频事件 {index} 采样率不一致"
        );
        assert_eq!(
            event.sample_count, expected.sample_count,
            "{label} 音频事件 {index} sample_count 不一致"
        );
    }
}

fn verify_loop_and_seek(prepared: &PreparedFileProfile) -> Value {
    let label = "25 FPS 循环/seek";
    let mut source = prepared
        .open_source_unpaced(true)
        .unwrap_or_else(|error| panic!("{label} 打开循环文件源失败：{error}"));
    let baseline = collect_loop_cycle(&mut source, prepared, label, 0);
    let cycle_duration_90k = prepared.duration_90k();
    for cycle in 1..=LOOP_COUNT {
        let current = collect_loop_cycle(&mut source, prepared, label, cycle);
        assert_eq!(
            current.len(),
            baseline.len(),
            "{label} 第 {cycle} 轮事件数量漂移"
        );
        for (index, (expected, actual)) in baseline.iter().zip(current.iter()).enumerate() {
            assert_eq!(
                actual.kind, expected.kind,
                "{label} 第 {cycle} 轮事件 {index} 类型漂移"
            );
            assert_eq!(
                actual.duration_90k, expected.duration_90k,
                "{label} 第 {cycle} 轮事件 {index} duration 漂移"
            );
            assert_eq!(
                actual.pts_90k,
                expected
                    .pts_90k
                    .saturating_add(u64::from(cycle).saturating_mul(cycle_duration_90k)),
                "{label} 第 {cycle} 轮事件 {index} PTS 累积漂移"
            );
            assert_eq!(actual.sample_rate_hz, expected.sample_rate_hz);
            assert_eq!(actual.sample_count, expected.sample_count);
            assert_eq!(actual.key_frame, expected.key_frame);
        }
    }

    let seek_target_90k = cycle_duration_90k / 2;
    let expected_seek_pts_90k = expected_seek_video_pts(prepared, 500);
    for attempt in 0..LOOP_COUNT {
        source.seek(500);
        let first_video = loop {
            let event = source
                .next_timed_media_event()
                .unwrap_or_else(|error| panic!("{label} 第 {attempt} 次 seek 读取失败：{error}"))
                .unwrap_or_else(|| panic!("{label} 第 {attempt} 次 seek 提前 EOF"));
            if let MediaEvent::Video(video) = event {
                break video;
            }
        };
        assert_eq!(
            first_video.pts_90k, expected_seek_pts_90k,
            "{label} 第 {attempt} 次 seek 未回到关键帧 PTS"
        );
    }

    json!({
        "profile_video_fps": prepared.profile().video_fps,
        "cycle_count": LOOP_COUNT,
        "cycle_duration_90k": cycle_duration_90k,
        "cycle_duration_seconds": cycle_duration_90k as f64 / CLOCK_HZ as f64,
        "baseline_event_count": baseline.len(),
        "seek_permille": 500,
        "seek_target_90k": seek_target_90k,
        "seek_expected_keyframe_pts_90k": expected_seek_pts_90k,
        "seek_repeat_count": LOOP_COUNT,
    })
}

fn collect_loop_cycle(
    source: &mut PreparedFileSource,
    prepared: &PreparedFileProfile,
    label: &str,
    cycle: u32,
) -> Vec<EventMark> {
    let expected_count = prepared.video_index().len() + prepared.audio_index().len();
    let mut events = Vec::with_capacity(expected_count);
    for index in 0..expected_count {
        let event = source
            .next_timed_media_event()
            .unwrap_or_else(|error| panic!("{label} 第 {cycle} 轮事件 {index} 读取失败：{error}"))
            .unwrap_or_else(|| panic!("{label} 第 {cycle} 轮事件 {index} 提前 EOF"));
        events.push(event_mark(event, label));
    }
    events
}

fn expected_seek_video_pts(prepared: &PreparedFileProfile, permille: u32) -> u64 {
    let target = prepared
        .duration_90k()
        .saturating_mul(u64::from(permille.min(1000)))
        / 1000;
    let cursor = prepared
        .video_index()
        .iter()
        .position(|index| index.pts_90k >= target)
        .unwrap_or_else(|| prepared.video_index().len().saturating_sub(1));
    (0..=cursor)
        .rev()
        .find(|&index| prepared.video_index()[index].key_frame)
        .map(|index| prepared.video_index()[index].pts_90k)
        .unwrap_or_else(|| prepared.video_index()[cursor].pts_90k)
}

fn make_synthetic_source(ffmpeg: &str, output: &Path) -> Vec<String> {
    let args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-f".into(),
        "lavfi".into(),
        "-t".into(),
        SOURCE_VIDEO_SECONDS.to_string(),
        "-i".into(),
        "testsrc2=size=640x480:rate=30".into(),
        "-itsoffset".into(),
        SOURCE_AUDIO_DELAY_SECONDS.to_string(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        "sine=frequency=880:sample_rate=8000".into(),
        "-map".into(),
        "0:v:0".into(),
        "-map".into(),
        "1:a:0".into(),
        "-t".into(),
        format!(
            "{:.3}",
            SOURCE_VIDEO_SECONDS as f64 + SOURCE_AUDIO_DELAY_SECONDS
        ),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "ultrafast".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-bf".into(),
        "0".into(),
        "-g".into(),
        "30".into(),
        "-keyint_min".into(),
        "30".into(),
        "-sc_threshold".into(),
        "0".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "32k".into(),
        "-ar".into(),
        "8000".into(),
        "-ac".into(),
        "1".into(),
        "-avoid_negative_ts".into(),
        "disabled".into(),
        "-f".into(),
        "matroska".into(),
        output.to_string_lossy().into_owned(),
    ];
    run_checked(ffmpeg, &args, "生成 60 秒 30 FPS 延迟音频合成源");
    args
}

fn prepare_output_root() -> PathBuf {
    let base = std::env::var_os("UVP_FILE_TIMING_OUTPUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("系统时间早于 UNIX epoch")
        .as_nanos();
    let root = base.join(format!("uvp-file-timing-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&root).expect("创建文件时长测试输出目录失败");
    root
}

fn locate_ffprobe(ffmpeg: &str) -> Option<String> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("UVP_FFPROBE_PATH") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(parent) = Path::new(ffmpeg).parent() {
        candidates.push(parent.join(if cfg!(target_os = "windows") {
            "ffprobe.exe"
        } else {
            "ffprobe"
        }));
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

fn probe_streams(ffprobe: &str, input: &Path, label: &str) -> Value {
    probe_json(
        ffprobe,
        vec![
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ],
        input,
        label,
    )
}

fn first_frame_pts(ffprobe: &str, input: &Path, selector: &str, label: &str) -> u64 {
    let probe = probe_json(
        ffprobe,
        vec![
            "-v",
            "error",
            "-select_streams",
            selector,
            "-show_frames",
            "-show_entries",
            "frame=best_effort_timestamp_time",
            "-of",
            "json",
        ],
        input,
        &format!("读取 {label} 首帧 PTS"),
    );
    let value = probe
        .get("frames")
        .and_then(Value::as_array)
        .and_then(|frames| frames.first())
        .and_then(|frame| frame.get("best_effort_timestamp_time"));
    value
        .and_then(value_f64)
        .map(|seconds| (seconds * CLOCK_HZ as f64).round() as u64)
        .unwrap_or_else(|| panic!("{label} 缺少可解析的首帧 PTS"))
}

fn probe_json(ffprobe: &str, args: Vec<&str>, input: &Path, label: &str) -> Value {
    let output = Command::new(ffprobe)
        .args(args)
        .arg(input)
        .output()
        .unwrap_or_else(|error| panic!("{label} 启动 ffprobe 失败：{error}"));
    assert!(
        output.status.success(),
        "{label} ffprobe 失败：{}",
        stderr_text(&output)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{label} ffprobe JSON 解析失败：{error}"))
}

fn run_checked(program: &str, args: &[String], label: &str) {
    let output = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("{label} 启动失败：{error}"));
    assert!(
        output.status.success(),
        "{label} 失败：{}\n命令参数：{:?}",
        stderr_text(&output),
        args
    );
}

fn stream_with_type<'a>(probe: &'a Value, codec_type: &str, label: &str) -> &'a Value {
    probe
        .get("streams")
        .and_then(Value::as_array)
        .and_then(|streams| {
            streams
                .iter()
                .find(|stream| stream.get("codec_type").and_then(Value::as_str) == Some(codec_type))
        })
        .unwrap_or_else(|| panic!("{label} 缺少 {codec_type} stream"))
}

fn first_stream<'a>(probe: &'a Value, label: &str) -> &'a Value {
    probe
        .get("streams")
        .and_then(Value::as_array)
        .and_then(|streams| streams.first())
        .unwrap_or_else(|| panic!("{label} 缺少 stream"))
}

fn parse_rational(value: &str) -> f64 {
    let (numerator, denominator) = value
        .split_once('/')
        .unwrap_or_else(|| panic!("无法解析 ffprobe 帧率 {value}"));
    let numerator = numerator
        .parse::<f64>()
        .unwrap_or_else(|error| panic!("无法解析 ffprobe 帧率分子 {value}：{error}"));
    let denominator = denominator
        .parse::<f64>()
        .unwrap_or_else(|error| panic!("无法解析 ffprobe 帧率分母 {value}：{error}"));
    assert!(denominator > 0.0, "ffprobe 帧率分母必须大于 0：{value}");
    numerator / denominator
}

fn value_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_u64().map(|value| value as f64))
        .or_else(|| value.as_str()?.parse::<f64>().ok())
}

fn value_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse::<u64>().ok())
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

fn assert_close_u64(actual: u64, expected: u64, tolerance: u64, label: &str) {
    let delta = actual.abs_diff(expected);
    assert!(
        delta <= tolerance,
        "{label}：实际 {actual}，期望 {expected}，误差 {delta} > 容差 {tolerance}"
    );
}
