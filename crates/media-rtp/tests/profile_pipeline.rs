use media_rtp::ffmpeg_bin;
use media_rtp::file_profile::{prepare_file_profile, PreparedFileSource};
use media_rtp::profile::{MediaAudioCodec, MediaProfile, MediaVideoCodec};
use serde_json::{json, Value};
use std::any::Any;
use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

const PROFILE_MATRIX_DIR: &str = "profile-matrix";
const BITRATE_MATRIX_DIR: &str = "profile-bitrate-matrix";
const SOURCE_DURATION_SECONDS: f64 = 15.0;
const GOP_SOURCE_DURATION_SECONDS: f64 = 30.0;
const BITRATE_SOURCE_DURATION_SECONDS: f64 = 60.0;
const BITRATE_WINDOW_START_SECONDS: u64 = 5;
const BITRATE_WINDOW_SECONDS: u64 = 55;
const BITRATE_MAX_CONCURRENCY: usize = 2;

static PROFILE_PIPELINE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[test]
#[ignore = "需要真实 ffmpeg/ffprobe；显式运行 cargo test -p media-rtp --test profile_pipeline -- --ignored --nocapture"]
fn profile_pipeline_matrix_uses_real_file_media() {
    let _guard = PROFILE_PIPELINE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("媒体 profile 测试互斥锁异常");
    let ffmpeg = ffmpeg_bin().unwrap_or_else(|| panic!("未找到 ffmpeg，不能把缺少依赖标记为通过"));
    let ffprobe = locate_ffprobe(&ffmpeg)
        .unwrap_or_else(|| panic!("未找到 ffprobe，不能把缺少依赖标记为通过"));
    let report_root = prepare_test_output_root(PROFILE_MATRIX_DIR);
    let cache_root = report_root.join("cache");
    std::env::set_var("UVP_MEDIA_PROFILE_CACHE_DIR", &cache_root);

    let source = report_root.join("source-motion-audio.mkv");
    let source_command = make_synthetic_source(&ffmpeg, &source, SOURCE_DURATION_SECONDS);
    let source_probe = probe_json(
        &ffprobe,
        &[
            "-v".into(),
            "error".into(),
            "-show_streams".into(),
            "-show_format".into(),
            "-of".into(),
            "json".into(),
        ],
        &source,
        "探测合成源",
    );
    let source_duration = json_f64(
        source_probe
            .get("format")
            .and_then(|format| format.get("duration")),
    )
    .expect("合成源缺少可解析的 duration");
    assert!(
        (source_duration - SOURCE_DURATION_SECONDS).abs() <= 0.5,
        "合成源时长应接近 {SOURCE_DURATION_SECONDS:.1}s，实际 {source_duration:.6}s"
    );
    assert_source_has_video_and_audio(&source_probe);
    let motion_hashes = assert_source_has_motion(&ffmpeg, &source);

    let gop_source = report_root.join("source-gop-motion-audio.mkv");
    let gop_source_command =
        make_synthetic_source(&ffmpeg, &gop_source, GOP_SOURCE_DURATION_SECONDS);
    let gop_source_probe = probe_json(
        &ffprobe,
        &[
            "-v".into(),
            "error".into(),
            "-show_streams".into(),
            "-show_format".into(),
            "-of".into(),
            "json".into(),
        ],
        &gop_source,
        "探测 GOP 合成源",
    );
    let gop_source_duration = json_f64(
        gop_source_probe
            .get("format")
            .and_then(|format| format.get("duration")),
    )
    .expect("GOP 合成源缺少可解析的 duration");
    assert!(
        (gop_source_duration - GOP_SOURCE_DURATION_SECONDS).abs() <= 0.5,
        "GOP 合成源时长应接近 {GOP_SOURCE_DURATION_SECONDS:.1}s，实际 {gop_source_duration:.6}s"
    );
    assert_source_has_video_and_audio(&gop_source_probe);
    let gop_motion_hashes = assert_source_has_motion(&ffmpeg, &gop_source);

    let mut cases = Vec::new();
    let mut video_case_count = 0;
    for &(width, height) in &[(640, 480), (1280, 720), (1920, 1080)] {
        for &video_fps in &[15, 20, 25, 30] {
            for &video_codec in &[MediaVideoCodec::H264, MediaVideoCodec::H265] {
                let profile = MediaProfile {
                    width,
                    height,
                    video_fps,
                    bitrate_kbps: 2000,
                    keyframe_interval_seconds: 1,
                    video_codec,
                    audio_codec: MediaAudioCodec::Aac,
                    audio_sample_rate_hz: 16000,
                };
                cases.push(verify_profile(
                    &ffmpeg,
                    &ffprobe,
                    &source,
                    source_duration,
                    "video",
                    &format!(
                        "video-{width}x{height}-{video_fps}-{}",
                        video_codec_name(video_codec)
                    ),
                    &profile,
                ));
                video_case_count += 1;
            }
        }
    }
    assert_eq!(video_case_count, 24, "视频尺寸/FPS/编码矩阵必须是 24 组合");

    for &(label, audio_codec, sample_rate) in &[
        ("g711-a-8k", MediaAudioCodec::G711A, 8000),
        ("g711-u-8k", MediaAudioCodec::G711U, 8000),
        ("aac-8k", MediaAudioCodec::Aac, 8000),
        ("aac-16k", MediaAudioCodec::Aac, 16000),
    ] {
        let profile = MediaProfile {
            width: 1280,
            height: 720,
            video_fps: 25,
            bitrate_kbps: 2000,
            keyframe_interval_seconds: 1,
            video_codec: MediaVideoCodec::H264,
            audio_codec,
            audio_sample_rate_hz: sample_rate,
        };
        cases.push(verify_profile(
            &ffmpeg,
            &ffprobe,
            &source,
            source_duration,
            "audio",
            label,
            &profile,
        ));
    }

    for &gop_seconds in &[1, 2, 4] {
        let profile = MediaProfile {
            width: 1280,
            height: 720,
            video_fps: 25,
            bitrate_kbps: 2000,
            keyframe_interval_seconds: gop_seconds,
            video_codec: MediaVideoCodec::H264,
            audio_codec: MediaAudioCodec::Aac,
            audio_sample_rate_hz: 16000,
        };
        cases.push(verify_profile(
            &ffmpeg,
            &ffprobe,
            &gop_source,
            gop_source_duration,
            "gop",
            &format!("gop-{gop_seconds}s"),
            &profile,
        ));
    }

    assert_eq!(
        cases.iter().filter(|case| case["group"] == "video").count(),
        24
    );
    assert_eq!(
        cases.iter().filter(|case| case["group"] == "audio").count(),
        4
    );
    assert_eq!(
        cases.iter().filter(|case| case["group"] == "gop").count(),
        3
    );

    let report = json!({
        "schema_version": 1,
        "source": source.to_string_lossy(),
        "source_command": source_command,
        "source_probe": source_probe,
        "source_duration_seconds": source_duration,
        "source_motion_hashes": motion_hashes,
        "gop_source": gop_source.to_string_lossy(),
        "gop_source_command": gop_source_command,
        "gop_source_probe": gop_source_probe,
        "gop_source_duration_seconds": gop_source_duration,
        "gop_source_motion_hashes": gop_motion_hashes,
        "tools": {
            "ffmpeg": ffmpeg,
            "ffmpeg_version": tool_version(&ffmpeg),
            "ffprobe": ffprobe,
            "ffprobe_version": tool_version(&ffprobe),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        },
        "matrix": {
            "video_combinations": 24,
            "audio_formats": ["g711_a@8000", "g711_u@8000", "aac@8000", "aac@16000"],
            "gop_scenarios_seconds": [1, 2, 4],
            "case_records": cases.len(),
        },
        "cache_root": cache_root.to_string_lossy(),
        "cases": cases,
    });
    let report_path = report_root.join("profile-matrix.json");
    fs::write(
        &report_path,
        serde_json::to_vec_pretty(&report).expect("序列化 profile 矩阵报告失败"),
    )
    .expect("写入 profile 矩阵报告失败");
    println!(
        "[profile-matrix] 通过 {} 条记录；报告：{}；缓存媒体：{}",
        report["cases"].as_array().map_or(0, Vec::len),
        report_path.display(),
        cache_root.display()
    );
}

#[test]
#[ignore = "需要真实 ffmpeg/ffprobe；显式运行 cargo test -p media-rtp --test profile_pipeline -- --ignored --nocapture"]
fn profile_pipeline_bitrate_matrix_uses_video_payload() {
    let _guard = PROFILE_PIPELINE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("媒体 profile 测试互斥锁异常");
    let ffmpeg = ffmpeg_bin().unwrap_or_else(|| panic!("未找到 ffmpeg，不能把缺少依赖标记为通过"));
    let ffprobe = locate_ffprobe(&ffmpeg)
        .unwrap_or_else(|| panic!("未找到 ffprobe，不能把缺少依赖标记为通过"));
    let report_root = prepare_test_output_root(BITRATE_MATRIX_DIR);
    let cache_root = report_root.join("cache");
    std::env::set_var("UVP_MEDIA_PROFILE_CACHE_DIR", &cache_root);

    let source = report_root.join("source-high-motion-720p25.mkv");
    let source_command = if test_resume_enabled() && source.is_file() {
        vec!["复用已有高运动码率合成源".into(), source.to_string_lossy().into_owned()]
    } else {
        make_high_motion_source(&ffmpeg, &source, BITRATE_SOURCE_DURATION_SECONDS)
    };
    let source_probe = probe_json(
        &ffprobe,
        &[
            "-v".into(),
            "error".into(),
            "-show_streams".into(),
            "-show_format".into(),
            "-of".into(),
            "json".into(),
        ],
        &source,
        "探测高运动码率合成源",
    );
    let source_duration = json_f64(
        source_probe
            .get("format")
            .and_then(|format| format.get("duration")),
    )
    .expect("高运动码率合成源缺少可解析的 duration");
    assert!(
        (source_duration - BITRATE_SOURCE_DURATION_SECONDS).abs() <= 0.5,
        "高运动码率合成源时长应接近 {BITRATE_SOURCE_DURATION_SECONDS:.1}s，实际 {source_duration:.6}s"
    );
    assert_source_has_video_and_audio(&source_probe);
    let motion_hashes = assert_source_has_motion(&ffmpeg, &source);

    let mut profiles = Vec::with_capacity(12);
    for &(video_codec, codec_name) in &[
        (MediaVideoCodec::H264, "h264"),
        (MediaVideoCodec::H265, "hevc"),
    ] {
        for &bitrate_kbps in &[600, 1200, 2000, 4000, 6000, 8000] {
            let profile = MediaProfile {
                width: 1280,
                height: 720,
                video_fps: 25,
                bitrate_kbps,
                keyframe_interval_seconds: 1,
                video_codec,
                audio_codec: MediaAudioCodec::Aac,
                audio_sample_rate_hz: 16000,
            };
            profiles.push((format!("bitrate-{bitrate_kbps}-{codec_name}"), profile));
        }
    }
    let cases = run_bitrate_cases(&ffmpeg, &ffprobe, &source, source_duration, &profiles);
    assert_eq!(cases.len(), 12, "码率矩阵必须是 12 组合");

    let report = json!({
        "schema_version": 1,
        "source": source.to_string_lossy(),
        "source_command": source_command,
        "source_probe": source_probe,
        "source_duration_seconds": source_duration,
        "source_motion_hashes": motion_hashes,
        "tools": {
            "ffmpeg": ffmpeg,
            "ffmpeg_version": tool_version(&ffmpeg),
            "ffprobe": ffprobe,
            "ffprobe_version": tool_version(&ffprobe),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        },
        "matrix": {
            "video_size": "1280x720",
            "video_fps": 25,
            "video_codecs": ["h264", "hevc"],
            "target_bitrates_kbps": [600, 1200, 2000, 4000, 6000, 8000],
            "discard_leading_seconds": BITRATE_WINDOW_START_SECONDS,
            "measurement_seconds": BITRATE_WINDOW_SECONDS,
            "payload_only": true,
            "tolerance_fraction": 0.30,
            "max_concurrency": BITRATE_MAX_CONCURRENCY,
            "case_records": cases.len(),
        },
        "cache_root": cache_root.to_string_lossy(),
        "cases": cases,
    });
    let report_path = report_root.join("profile-bitrate-matrix.json");
    fs::write(
        &report_path,
        serde_json::to_vec_pretty(&report).expect("序列化 profile 码率矩阵报告失败"),
    )
    .expect("写入 profile 码率矩阵报告失败");
    let failures = report["cases"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|case| case.get("passed").and_then(Value::as_bool) != Some(true))
        .map(|case| {
            format!(
                "{}: {}",
                case.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                case.get("failure")
                    .and_then(Value::as_str)
                    .unwrap_or("未提供失败原因")
            )
        })
        .collect::<Vec<_>>();
    println!(
        "[profile-bitrate-matrix] {} 条记录；报告：{}；缓存媒体：{}",
        report["cases"].as_array().map_or(0, Vec::len),
        report_path.display(),
        cache_root.display()
    );
    assert!(
        failures.is_empty(),
        "码率矩阵存在失败（详见 {}）：{}",
        report_path.display(),
        failures.join("；")
    );
}

fn make_synthetic_source(ffmpeg: &str, output: &Path, duration_seconds: f64) -> Vec<String> {
    let args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        "testsrc2=size=320x240:rate=60".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        "sine=frequency=880:sample_rate=48000".into(),
        "-t".into(),
        duration_seconds.to_string(),
        "-map".into(),
        "0:v:0".into(),
        "-map".into(),
        "1:a:0".into(),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "ultrafast".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-c:a".into(),
        "aac".into(),
        "-ar".into(),
        "48000".into(),
        "-ac".into(),
        "1".into(),
        "-f".into(),
        "matroska".into(),
        output.to_string_lossy().into_owned(),
    ];
    run_checked(ffmpeg, &args, "生成运动画面和音轨合成源");
    args
}

fn make_high_motion_source(ffmpeg: &str, output: &Path, duration_seconds: f64) -> Vec<String> {
    let args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        "testsrc2=size=1280x720:rate=25".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        "sine=frequency=880:sample_rate=48000".into(),
        "-t".into(),
        duration_seconds.to_string(),
        "-map".into(),
        "0:v:0".into(),
        "-map".into(),
        "1:a:0".into(),
        "-vf".into(),
        "noise=alls=20:allf=t+u:all_seed=28181".into(),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "ultrafast".into(),
        "-crf".into(),
        "18".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-c:a".into(),
        "aac".into(),
        "-ar".into(),
        "48000".into(),
        "-ac".into(),
        "1".into(),
        "-f".into(),
        "matroska".into(),
        output.to_string_lossy().into_owned(),
    ];
    run_checked(ffmpeg, &args, "生成高运动码率合成源");
    args
}

fn run_bitrate_cases(
    ffmpeg: &str,
    ffprobe: &str,
    source: &Path,
    source_duration: f64,
    profiles: &[(String, MediaProfile)],
) -> Vec<Value> {
    let mut cases = Vec::with_capacity(profiles.len());
    for batch in profiles.chunks(BITRATE_MAX_CONCURRENCY) {
        let handles = batch
            .iter()
            .map(|(case_name, profile)| {
                let ffmpeg = ffmpeg.to_string();
                let ffprobe = ffprobe.to_string();
                let source = source.to_path_buf();
                let case_name = case_name.clone();
                let profile = profile.clone();
                thread::spawn(move || {
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        let mut record = verify_profile(
                            &ffmpeg,
                            &ffprobe,
                            &source,
                            source_duration,
                            "bitrate",
                            &case_name,
                            &profile,
                        );
                        let payload = record
                            .get("video")
                            .and_then(|video| video.get("payload_bitrate"))
                            .cloned();
                        let within_tolerance = payload
                            .as_ref()
                            .and_then(|payload| payload.get("within_tolerance"))
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        record["passed"] = json!(within_tolerance);
                        if !within_tolerance {
                            record["failure"] = json!(format_bitrate_failure(payload.as_ref()));
                        }
                        record
                    }));
                    match result {
                        Ok(record) => record,
                        Err(payload) => json!({
                            "group": "bitrate",
                            "name": case_name,
                            "profile": serde_json::to_value(&profile)
                                .expect("序列化失败 profile 失败"),
                            "passed": false,
                            "failure": panic_message(payload),
                        }),
                    }
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            cases.push(
                handle.join().unwrap_or_else(|payload| {
                    panic!("码率案例线程异常：{}", panic_message(payload))
                }),
            );
        }
    }
    cases
}

fn format_bitrate_failure(payload: Option<&Value>) -> String {
    let Some(payload) = payload else {
        return "缺少视频 payload 码率测量结果".into();
    };
    format!(
        "平均 {:.3} kbps，目标 {} kbps，误差 {:.1}%（允许 ±30%）",
        payload
            .get("average_bitrate_kbps")
            .and_then(Value::as_f64)
            .unwrap_or_default(),
        payload
            .get("target_bitrate_kbps")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        payload
            .get("relative_error_percent")
            .and_then(Value::as_f64)
            .unwrap_or_default(),
    )
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    "未知 panic".into()
}

fn verify_profile(
    ffmpeg: &str,
    ffprobe: &str,
    source: &Path,
    source_duration: f64,
    group: &str,
    case_name: &str,
    profile: &MediaProfile,
) -> Value {
    profile
        .validate()
        .unwrap_or_else(|error| panic!("{group}/{case_name} profile 校验失败：{error}"));
    let cancel = AtomicBool::new(false);
    let prepared = prepare_file_profile(source, profile, &cancel, None)
        .unwrap_or_else(|error| panic!("{group}/{case_name} prepare_file_profile 失败：{error}"));
    assert_eq!(
        prepared.profile(),
        profile,
        "{group}/{case_name} profile 被改变"
    );
    assert!(
        !prepared.video_index().is_empty(),
        "{group}/{case_name} 无视频 AU"
    );
    assert!(
        prepared.audio_index().len() > 0,
        "{group}/{case_name} 无音频 AU；合成源必须带音轨"
    );
    assert_eq!(
        prepared
            .video_index()
            .first()
            .expect("视频 AU 不应为空")
            .pts_90k,
        0,
        "{group}/{case_name} 视频 PTS 未从 0 开始"
    );
    assert!(
        prepared.video_index().windows(2).all(|pair| {
            pair[1].pts_90k > pair[0].pts_90k
                && pair[0].duration_90k > 0
                && pair[1].duration_90k > 0
        }),
        "{group}/{case_name} 视频 PTS/持续时间不单调"
    );
    assert!(
        prepared.audio_index().windows(2).all(|pair| {
            pair[1].pts_90k > pair[0].pts_90k
                && pair[0].duration_90k > 0
                && pair[1].duration_90k > 0
        }),
        "{group}/{case_name} 音频 PTS/持续时间不单调"
    );
    assert!(
        prepared
            .video_index()
            .first()
            .expect("视频 AU 不应为空")
            .key_frame,
        "{group}/{case_name} 首个视频 AU 不是关键帧"
    );

    let container_path = prepared.cache_dir().join("media.mkv");
    assert!(
        container_path.is_file(),
        "{group}/{case_name} 准备结果缺少输出容器"
    );
    let container_probe = probe_prepared_container(ffprobe, &container_path);
    let container_video_stream = stream_with_type(&container_probe, "video", "输出容器视频");
    assert_eq!(
        container_video_stream
            .get("codec_name")
            .and_then(Value::as_str),
        Some(video_codec_name(profile.video_codec)),
        "{group}/{case_name} 输出容器视频编码不匹配"
    );
    assert_eq!(
        json_u64(container_video_stream.get("width")),
        Some(u64::from(profile.width)),
        "{group}/{case_name} 输出容器视频宽度不匹配"
    );
    assert_eq!(
        json_u64(container_video_stream.get("height")),
        Some(u64::from(profile.height)),
        "{group}/{case_name} 输出容器视频高度不匹配"
    );
    let container_frame_rate = container_video_stream
        .get("r_frame_rate")
        .and_then(Value::as_str)
        .map(parse_frame_rate)
        .unwrap_or_else(|| panic!("{group}/{case_name} 输出容器缺少视频帧率"));
    assert!(
        (container_frame_rate - f64::from(profile.video_fps)).abs() < 0.001,
        "{group}/{case_name} 输出容器视频帧率 {container_frame_rate} != {}",
        profile.video_fps
    );
    let container_video_frames = json_u64(container_video_stream.get("nb_read_frames"))
        .unwrap_or_else(|| panic!("{group}/{case_name} 输出容器未返回视频 frame count"));

    let video_path = prepared.video_path().to_path_buf();
    let video_format = match profile.video_codec {
        MediaVideoCodec::H264 => "h264",
        MediaVideoCodec::H265 => "hevc",
    };
    let video_probe = probe_raw_video(ffprobe, &video_path, video_format);
    let video_stream = first_stream(&video_probe, "视频");
    assert_eq!(
        video_stream.get("codec_name").and_then(Value::as_str),
        Some(video_codec_name(profile.video_codec)),
        "{group}/{case_name} ffprobe 视频编码不匹配"
    );
    assert_eq!(
        json_u64(video_stream.get("width")),
        Some(u64::from(profile.width)),
        "{group}/{case_name} ffprobe 视频宽度不匹配"
    );
    assert_eq!(
        json_u64(video_stream.get("height")),
        Some(u64::from(profile.height)),
        "{group}/{case_name} ffprobe 视频高度不匹配"
    );
    let probed_video_frames = json_u64(video_stream.get("nb_read_frames"))
        .unwrap_or_else(|| panic!("{group}/{case_name} ffprobe 未返回视频 frame count"));
    assert_eq!(
        probed_video_frames as usize,
        prepared.video_index().len(),
        "{group}/{case_name} ffprobe 帧数与视频 AU 数量不一致"
    );
    assert_eq!(
        container_video_frames as usize,
        prepared.video_index().len(),
        "{group}/{case_name} 输出容器帧数与视频 AU 数量不一致"
    );
    let video_decode = decode_raw(ffmpeg, &video_path, video_format, None, "视频");
    assert!(video_decode, "{group}/{case_name} 视频解码失败");

    let mut prepared_source = prepared.open_source_unpaced(false).unwrap_or_else(|error| {
        panic!("{group}/{case_name} 打开 PreparedFileSource 失败：{error}")
    });
    let read_metrics = read_prepared_source(
        &mut prepared_source,
        profile.video_codec.into(),
        profile.audio_codec.into(),
        group,
        case_name,
    );
    let read_video_frames = read_metrics.video.len();
    let read_audio_units = read_metrics.audio.len();
    assert_eq!(
        read_video_frames,
        prepared.video_index().len(),
        "{group}/{case_name} PreparedFileSource 视频读取数量不一致"
    );
    assert_eq!(
        read_audio_units,
        prepared.audio_index().len(),
        "{group}/{case_name} PreparedFileSource 音频读取数量不一致"
    );
    for (index, (timed, indexed)) in read_metrics
        .video
        .iter()
        .zip(prepared.video_index())
        .enumerate()
    {
        assert_eq!(
            timed.pts_90k, indexed.pts_90k,
            "{group}/{case_name} TimedVideoAu #{index} PTS 与索引不一致"
        );
        assert_eq!(
            timed.duration_90k, indexed.duration_90k,
            "{group}/{case_name} TimedVideoAu #{index} 时长与索引不一致"
        );
        assert_eq!(
            timed.key_frame, indexed.key_frame,
            "{group}/{case_name} TimedVideoAu #{index} 关键帧标记与索引不一致"
        );
    }
    for (index, (timed, indexed)) in read_metrics
        .audio
        .iter()
        .zip(prepared.audio_index())
        .enumerate()
    {
        assert_eq!(
            timed.pts_90k, indexed.pts_90k,
            "{group}/{case_name} TimedAudioAu #{index} PTS 与索引不一致"
        );
        assert_eq!(
            timed.duration_90k, indexed.duration_90k,
            "{group}/{case_name} TimedAudioAu #{index} 时长与索引不一致"
        );
        assert_eq!(
            timed.sample_rate_hz, indexed.sample_rate_hz,
            "{group}/{case_name} TimedAudioAu #{index} 采样率与索引不一致"
        );
        assert_eq!(
            timed.sample_count, indexed.sample_count,
            "{group}/{case_name} TimedAudioAu #{index} 样本数与索引不一致"
        );
    }

    let video_last = prepared.video_index().last().expect("视频 AU 不应为空");
    let video_duration_90k = video_last.pts_90k.saturating_add(video_last.duration_90k);
    let prepared_duration_seconds = prepared.duration_90k() as f64 / 90_000.0;
    assert!(
        (prepared_duration_seconds - source_duration).abs() <= 0.2,
        "{group}/{case_name} 准备结果与源时长偏差超过 200ms：源 {source_duration:.6}s，结果 {prepared_duration_seconds:.6}s"
    );
    let expected_video_frames =
        (video_duration_90k as f64 / 90_000.0 * f64::from(profile.video_fps)).round() as i64;
    assert!(
        ((prepared.video_index().len() as i64) - expected_video_frames).abs() <= 1,
        "{group}/{case_name} 视频 frame count 不符合输出 FPS：实际 {}，期望约 {}",
        prepared.video_index().len(),
        expected_video_frames
    );
    let timed_video_first_pts = read_metrics
        .video
        .first()
        .expect("TimedVideoAu 不应为空")
        .pts_90k;
    let timed_video_last_pts = read_metrics
        .video
        .last()
        .expect("TimedVideoAu 不应为空")
        .pts_90k;
    let timed_video_span_90k = timed_video_last_pts.saturating_sub(timed_video_first_pts);
    if read_metrics.video.len() > 1 {
        let measured_step = timed_video_span_90k as f64 / (read_metrics.video.len() - 1) as f64;
        let expected_step = 90_000.0 / f64::from(profile.video_fps);
        assert!(
            (measured_step - expected_step).abs() <= 2.0,
            "{group}/{case_name} TimedVideoAu PTS 平均间隔 {measured_step:.3} 不符合目标 FPS {}",
            profile.video_fps
        );
    }

    let key_positions: Vec<usize> = prepared
        .video_index()
        .iter()
        .enumerate()
        .filter_map(|(index, au)| au.key_frame.then_some(index))
        .collect();
    assert!(!key_positions.is_empty(), "{group}/{case_name} 无关键帧");
    if group == "gop" {
        assert!(
            key_positions.len() >= 3,
            "{group}/{case_name} 30 秒 GOP 场景至少需要首帧后观察到两个周期关键帧，实际 {} 个",
            key_positions.len()
        );
    }
    let max_keyframe_gap = key_positions
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .max()
        .unwrap_or(0);
    let max_allowed_gap = usize::try_from(
        profile
            .video_fps
            .saturating_mul(profile.keyframe_interval_seconds),
    )
    .expect("GOP 帧数转换失败")
    .saturating_add(1);
    assert!(
        max_keyframe_gap == 0 || max_keyframe_gap <= max_allowed_gap,
        "{group}/{case_name} 关键帧间隔过大：实际 {max_keyframe_gap} 帧，允许 {max_allowed_gap} 帧"
    );
    let payload_bitrate = (group == "bitrate")
        .then(|| measure_video_payload_bitrate(prepared.video_index(), profile));

    let audio_path = prepared
        .audio_path()
        .unwrap_or_else(|| panic!("{group}/{case_name} 准备结果缺少音频文件"));
    let audio_format = match profile.audio_codec {
        MediaAudioCodec::G711A => "alaw",
        MediaAudioCodec::G711U => "mulaw",
        MediaAudioCodec::Aac => "aac",
    };
    let audio_probe = probe_raw_audio(
        ffprobe,
        audio_path,
        audio_format,
        profile.audio_codec,
        prepared.audio_sample_rate_hz().expect("音频采样率缺失"),
    );
    let audio_stream = first_stream(&audio_probe, "音频");
    let expected_audio_codec = match profile.audio_codec {
        MediaAudioCodec::G711A => "pcm_alaw",
        MediaAudioCodec::G711U => "pcm_mulaw",
        MediaAudioCodec::Aac => "aac",
    };
    assert_eq!(
        audio_stream.get("codec_name").and_then(Value::as_str),
        Some(expected_audio_codec),
        "{group}/{case_name} ffprobe 音频编码不匹配"
    );
    let expected_sample_rate = u64::from(profile.effective_audio_sample_rate_hz());
    assert_eq!(
        json_u64(audio_stream.get("sample_rate")),
        Some(expected_sample_rate),
        "{group}/{case_name} ffprobe 音频采样率不匹配"
    );
    assert_eq!(
        json_u64(audio_stream.get("channels")),
        Some(1),
        "{group}/{case_name} ffprobe 音频声道不匹配"
    );
    let probed_audio_frames = json_u64(audio_stream.get("nb_read_frames"))
        .unwrap_or_else(|| panic!("{group}/{case_name} ffprobe 未返回音频 frame count"));
    let (aac_priming_packet_count, aac_skip_samples) =
        if profile.audio_codec == MediaAudioCodec::Aac {
            let packet_probe = probe_container_audio_packets(ffprobe, &container_path);
            aac_skip_samples_from_packets(&packet_probe)
        } else {
            (0, 0)
        };
    let effective_audio_frames = probed_audio_frames
        .checked_sub(aac_priming_packet_count as u64)
        .unwrap_or_else(|| panic!("{group}/{case_name} AAC priming 帧数超过裸音频帧数"));
    if profile.audio_codec == MediaAudioCodec::Aac {
        assert!(
            aac_priming_packet_count > 0,
            "{group}/{case_name} AAC 输出缺少可识别的 skip_samples priming 包"
        );
    }
    assert_eq!(
        effective_audio_frames as usize,
        prepared.audio_index().len(),
        "{group}/{case_name} 排除 AAC priming 后的有效音频帧数与音频 AU 数量不一致"
    );
    let audio_decode = decode_raw(
        ffmpeg,
        audio_path,
        audio_format,
        (profile.audio_codec != MediaAudioCodec::Aac).then_some(expected_sample_rate as u32),
        "音频",
    );
    assert!(audio_decode, "{group}/{case_name} 音频解码失败");
    assert!(
        prepared.audio_index().iter().all(|au| {
            au.codec == profile.audio_codec
                && u64::from(au.sample_rate_hz) == expected_sample_rate
                && au.sample_count > 0
        }),
        "{group}/{case_name} 音频 AU 存在编码、采样率、样本数或数据异常"
    );
    let total_audio_samples: u64 = prepared
        .audio_index()
        .iter()
        .map(|au| u64::from(au.sample_count))
        .sum();
    let audio_duration_seconds = total_audio_samples as f64 / expected_sample_rate as f64;
    assert!(
        (audio_duration_seconds - source_duration).abs() <= 0.2,
        "{group}/{case_name} 音频有效时长偏差超过 200ms：源 {source_duration:.6}s，结果 {audio_duration_seconds:.6}s"
    );

    println!(
        "[profile-matrix] {group}/{case_name}: {}x{} {}fps {:?}/{:?}@{}Hz, video_frames={}, audio_aus={}, duration={:.3}s",
        profile.width,
        profile.height,
        profile.video_fps,
        profile.video_codec,
        profile.audio_codec,
        expected_sample_rate,
        prepared.video_index().len(),
        prepared.audio_index().len(),
        prepared_duration_seconds,
    );
    json!({
        "group": group,
        "name": case_name,
        "profile": serde_json::to_value(profile).expect("序列化 MediaProfile 失败"),
        "prepared": {
            "cache_key": prepared.cache_key(),
            "cache_dir": prepared.cache_dir().to_string_lossy(),
            "video_path": video_path.to_string_lossy(),
            "audio_path": audio_path.to_string_lossy(),
            "duration_90k": prepared.duration_90k(),
            "duration_seconds": prepared_duration_seconds,
        },
        "video": {
            "codec_name": video_stream.get("codec_name"),
            "width": video_stream.get("width"),
            "height": video_stream.get("height"),
            "container_r_frame_rate": container_video_stream.get("r_frame_rate"),
            "container_avg_frame_rate": container_video_stream.get("avg_frame_rate"),
            "raw_probe_r_frame_rate_ignored": video_stream.get("r_frame_rate"),
            "ffprobe_frame_count": probed_video_frames,
            "container_frame_count": container_video_frames,
            "index_frame_count": prepared.video_index().len(),
            "source_read_frame_count": read_video_frames,
            "decoded": video_decode,
            "timed_video_first_pts_90k": timed_video_first_pts,
            "timed_video_last_pts_90k": timed_video_last_pts,
            "timed_video_span_90k": timed_video_span_90k,
            "timed_video_measured_fps": if timed_video_span_90k > 0 {
                (read_metrics.video.len() - 1) as f64 * 90_000.0 / timed_video_span_90k as f64
            } else {
                0.0
            },
            "first_pts_90k": prepared.video_index().first().expect("视频 AU 不应为空").pts_90k,
            "last_end_pts_90k": video_duration_90k,
            "key_frame_count": key_positions.len(),
            "max_keyframe_gap_frames": max_keyframe_gap,
            "payload_bitrate": payload_bitrate.map(|metrics| json!({
                "window_start_seconds": metrics.window_start_seconds,
                "window_duration_seconds": metrics.window_duration_seconds,
                "covered_window_seconds": metrics.covered_window_seconds,
                "payload_bytes": metrics.payload_bytes,
                "frame_count": metrics.frame_count,
                "average_bitrate_kbps": metrics.average_bitrate_kbps,
                "target_bitrate_kbps": metrics.target_bitrate_kbps,
                "relative_error_percent": metrics.relative_error * 100.0,
                "within_tolerance": metrics.within_tolerance,
            })),
        },
        "audio": {
            "codec_name": audio_stream.get("codec_name"),
            "sample_rate_hz": audio_stream.get("sample_rate"),
            "channels": audio_stream.get("channels"),
            "ffprobe_frame_count": probed_audio_frames,
            "aac_priming_packet_count": aac_priming_packet_count,
            "aac_skip_samples": aac_skip_samples,
            "effective_frame_count": effective_audio_frames,
            "index_au_count": prepared.audio_index().len(),
            "source_read_au_count": read_audio_units,
            "decoded": audio_decode,
            "total_samples": total_audio_samples,
            "effective_duration_seconds": audio_duration_seconds,
            "first_pts_90k": prepared.audio_index().first().expect("音频 AU 不应为空").pts_90k,
            "last_end_pts_90k": prepared.audio_index().last().expect("音频 AU 不应为空").pts_90k
                + prepared.audio_index().last().expect("音频 AU 不应为空").duration_90k,
        },
    })
}

#[derive(Debug, Clone, Copy)]
struct VideoPayloadBitrateMetrics {
    window_start_seconds: u64,
    window_duration_seconds: u64,
    covered_window_seconds: f64,
    payload_bytes: u64,
    frame_count: usize,
    average_bitrate_kbps: f64,
    target_bitrate_kbps: u32,
    relative_error: f64,
    within_tolerance: bool,
}

fn measure_video_payload_bitrate(
    video_index: &[media_rtp::file_profile::VideoAccessUnitIndex],
    profile: &MediaProfile,
) -> VideoPayloadBitrateMetrics {
    let window_start_90k = BITRATE_WINDOW_START_SECONDS.saturating_mul(90_000);
    let window_end_90k =
        window_start_90k.saturating_add(BITRATE_WINDOW_SECONDS.saturating_mul(90_000));
    let selected = video_index
        .iter()
        .filter(|au| au.pts_90k >= window_start_90k && au.pts_90k < window_end_90k)
        .collect::<Vec<_>>();
    assert!(
        !selected.is_empty(),
        "码率测量窗口没有视频 AU：{}..{} ticks",
        window_start_90k,
        window_end_90k
    );
    let first_pts_90k = selected.first().expect("码率测量视频 AU 不应为空").pts_90k;
    let last = selected.last().expect("码率测量视频 AU 不应为空");
    let last_end_90k = last.pts_90k.saturating_add(last.duration_90k);
    let covered_window_seconds = last_end_90k.saturating_sub(first_pts_90k) as f64 / 90_000.0;
    let payload_bytes = selected
        .iter()
        .map(|au| au.length)
        .fold(0_u64, u64::saturating_add);
    let average_bitrate_kbps =
        payload_bytes as f64 * 8.0 / (BITRATE_WINDOW_SECONDS as f64 * 1_000.0);
    let target_bitrate_kbps = profile.bitrate_kbps;
    let relative_error = (average_bitrate_kbps - f64::from(target_bitrate_kbps)).abs()
        / f64::from(target_bitrate_kbps);
    let within_tolerance = (covered_window_seconds - BITRATE_WINDOW_SECONDS as f64).abs() <= 0.1
        && relative_error <= 0.30;
    VideoPayloadBitrateMetrics {
        window_start_seconds: BITRATE_WINDOW_START_SECONDS,
        window_duration_seconds: BITRATE_WINDOW_SECONDS,
        covered_window_seconds,
        payload_bytes,
        frame_count: selected.len(),
        average_bitrate_kbps,
        target_bitrate_kbps,
        relative_error,
        within_tolerance,
    }
}

#[derive(Debug, Clone, Copy)]
struct TimedVideoMetrics {
    pts_90k: u64,
    duration_90k: u64,
    key_frame: bool,
}

#[derive(Debug, Clone, Copy)]
struct TimedAudioMetrics {
    pts_90k: u64,
    duration_90k: u64,
    sample_rate_hz: u32,
    sample_count: u32,
}

#[derive(Debug, Default)]
struct PreparedReadMetrics {
    video: Vec<TimedVideoMetrics>,
    audio: Vec<TimedAudioMetrics>,
}

fn read_prepared_source(
    source: &mut PreparedFileSource,
    expected_video_codec: media_rtp::ps::VideoCodec,
    expected_audio_codec: media_rtp::ps::AudioCodec,
    group: &str,
    case_name: &str,
) -> PreparedReadMetrics {
    let mut metrics = PreparedReadMetrics::default();
    while let Some(event) = source
        .next_timed_media_event()
        .unwrap_or_else(|error| panic!("{group}/{case_name} 读取媒体事件失败：{error}"))
    {
        match event {
            media_rtp::source::MediaEvent::Video(video) => {
                assert!(
                    !video.data.is_empty(),
                    "{group}/{case_name} 视频 AU 数据为空"
                );
                assert!(
                    video.duration_90k > 0,
                    "{group}/{case_name} 视频 AU 时长为空"
                );
                assert_eq!(
                    video.codec, expected_video_codec,
                    "{group}/{case_name} TimedVideoAu 编码不匹配"
                );
                if let Some(previous) = metrics.video.last() {
                    assert!(
                        video.pts_90k > previous.pts_90k,
                        "{group}/{case_name} TimedVideoAu PTS 不单调"
                    );
                }
                metrics.video.push(TimedVideoMetrics {
                    pts_90k: video.pts_90k,
                    duration_90k: video.duration_90k,
                    key_frame: video.key_frame,
                });
            }
            media_rtp::source::MediaEvent::Audio(audio) => {
                assert!(
                    !audio.data.is_empty() && audio.sample_count > 0 && audio.duration_90k > 0,
                    "{group}/{case_name} PreparedFileSource 返回空音频 AU"
                );
                assert_eq!(
                    audio.codec, expected_audio_codec,
                    "{group}/{case_name} TimedAudioAu 编码不匹配"
                );
                if let Some(previous) = metrics.audio.last() {
                    assert!(
                        audio.pts_90k > previous.pts_90k,
                        "{group}/{case_name} TimedAudioAu PTS 不单调"
                    );
                }
                metrics.audio.push(TimedAudioMetrics {
                    pts_90k: audio.pts_90k,
                    duration_90k: audio.duration_90k,
                    sample_rate_hz: audio.sample_rate_hz,
                    sample_count: audio.sample_count,
                });
            }
            media_rtp::source::MediaEvent::Discontinuity { .. } => {}
        }
    }
    metrics
}

fn probe_raw_video(ffprobe: &str, input: &Path, format: &str) -> Value {
    probe_json(
        ffprobe,
        &[
            "-v".into(),
            "error".into(),
            "-f".into(),
            format.into(),
            "-count_frames".into(),
            "-show_entries".into(),
            "stream=codec_name,width,height,nb_read_frames".into(),
            "-show_streams".into(),
            "-of".into(),
            "json".into(),
        ],
        input,
        "探测裸视频",
    )
}

fn probe_prepared_container(ffprobe: &str, input: &Path) -> Value {
    probe_json(
        ffprobe,
        &[
            "-v".into(),
            "error".into(),
            "-count_frames".into(),
            "-show_streams".into(),
            "-show_format".into(),
            "-of".into(),
            "json".into(),
        ],
        input,
        "探测准备输出容器",
    )
}

fn probe_container_audio_packets(ffprobe: &str, input: &Path) -> Value {
    probe_json(
        ffprobe,
        &[
            "-v".into(),
            "error".into(),
            "-select_streams".into(),
            "a:0".into(),
            "-show_packets".into(),
            "-show_entries".into(),
            "packet=pts_time,duration_time,side_data_list".into(),
            "-of".into(),
            "json".into(),
        ],
        input,
        "探测准备输出音频包",
    )
}

fn stream_with_type<'a>(probe: &'a Value, codec_type: &str, kind: &str) -> &'a Value {
    probe
        .get("streams")
        .and_then(Value::as_array)
        .and_then(|streams| {
            streams
                .iter()
                .find(|stream| stream.get("codec_type").and_then(Value::as_str) == Some(codec_type))
        })
        .unwrap_or_else(|| panic!("ffprobe 未找到{kind}流"))
}

fn aac_skip_samples_from_packets(probe: &Value) -> (usize, u64) {
    let mut packet_count = 0;
    let mut skip_samples: u64 = 0;
    for packet in probe
        .get("packets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for side_data in packet
            .get("side_data_list")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(samples) = json_u64(side_data.get("skip_samples")) else {
                continue;
            };
            if samples > 0 {
                packet_count += 1;
                skip_samples = skip_samples.saturating_add(samples);
            }
        }
    }
    (packet_count, skip_samples)
}

fn probe_raw_audio(
    ffprobe: &str,
    input: &Path,
    format: &str,
    codec: MediaAudioCodec,
    sample_rate: u32,
) -> Value {
    let mut args = vec!["-v".into(), "error".into(), "-f".into(), format.into()];
    if codec != MediaAudioCodec::Aac {
        args.extend(["-ar".into(), sample_rate.to_string()]);
    }
    args.extend([
        "-count_frames".into(),
        "-show_entries".into(),
        "stream=codec_name,sample_rate,channels,nb_read_frames".into(),
        "-show_streams".into(),
        "-of".into(),
        "json".into(),
    ]);
    probe_json(ffprobe, &args, input, "探测裸音频")
}

fn decode_raw(
    ffmpeg: &str,
    input: &Path,
    format: &str,
    sample_rate: Option<u32>,
    kind: &str,
) -> bool {
    let mut args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-f".into(),
        format.into(),
    ];
    if let Some(sample_rate) = sample_rate {
        args.extend([
            "-ar".into(),
            sample_rate.to_string(),
            "-ac".into(),
            "1".into(),
        ]);
    }
    args.extend([
        "-i".into(),
        input.to_string_lossy().into_owned(),
        "-f".into(),
        "null".into(),
        "-".into(),
    ]);
    let output = Command::new(ffmpeg)
        .args(&args)
        .output()
        .unwrap_or_else(|error| panic!("启动 {kind} 解码失败：{error}"));
    if !output.status.success() {
        eprintln!(
            "[profile-matrix] {kind} 解码失败：{}",
            last_output_line(&output)
        );
    }
    output.status.success()
}

fn assert_source_has_video_and_audio(probe: &Value) {
    let streams = probe
        .get("streams")
        .and_then(Value::as_array)
        .expect("合成源缺少 streams");
    assert!(
        streams
            .iter()
            .any(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("video")),
        "合成源缺少视频轨"
    );
    assert!(
        streams
            .iter()
            .any(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("audio")),
        "合成源缺少音频轨"
    );
}

fn assert_source_has_motion(ffmpeg: &str, source: &Path) -> Vec<String> {
    let args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-i".into(),
        source.to_string_lossy().into_owned(),
        "-map".into(),
        "0:v:0".into(),
        "-vf".into(),
        "select='eq(n,0)+eq(n,30)'".into(),
        "-frames:v".into(),
        "2".into(),
        "-f".into(),
        "framemd5".into(),
        "-".into(),
    ];
    let output = Command::new(ffmpeg)
        .args(&args)
        .output()
        .expect("启动运动画面校验失败");
    assert!(
        output.status.success(),
        "运动画面校验失败：{}",
        last_output_line(&output)
    );
    let hashes = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim_start().starts_with('#') && !line.trim().is_empty())
        .filter_map(|line| line.split_whitespace().last())
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert!(hashes.len() >= 2, "运动画面校验未得到两个解码帧 hash");
    assert_ne!(hashes[0], hashes[1], "合成源前后帧相同，不能证明含运动画面");
    hashes
}

fn prepare_test_output_root(directory: &str) -> PathBuf {
    let base = std::env::var_os("UVP_MEDIA_TEST_OUTPUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            std::env::temp_dir().join(format!(
                "uvp-media-implementation-{}-{nonce}",
                std::process::id()
            ))
        });
    let root = base.join(directory);
    let resume = test_resume_enabled();
    if root.exists() && !resume {
        panic!(
            "profile 矩阵输出目录已存在：{}；请更换 UVP_MEDIA_TEST_OUTPUT_DIR，或显式设置 UVP_MEDIA_TEST_RESUME=1 复用已有产物",
            root.display()
        );
    }
    fs::create_dir_all(&root).expect("创建 profile 矩阵报告目录失败");
    root
}

fn test_resume_enabled() -> bool {
    matches!(
        std::env::var("UVP_MEDIA_TEST_RESUME").as_deref(),
        Ok("1") | Ok("true")
    )
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

fn probe_json(ffprobe: &str, args: &[String], input: &Path, operation: &str) -> Value {
    let mut full_args = args.to_vec();
    full_args.push(input.to_string_lossy().into_owned());
    let output = run_checked(ffprobe, &full_args, operation);
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{operation} JSON 解析失败：{error}"))
}

fn run_checked(program: &str, args: &[String], operation: &str) -> Output {
    let output = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("{operation}启动失败：{error}"));
    if !output.status.success() {
        panic!("{operation}失败：{}", last_output_line(&output));
    }
    output
}

fn first_stream<'a>(probe: &'a Value, kind: &str) -> &'a Value {
    probe
        .get("streams")
        .and_then(Value::as_array)
        .and_then(|streams| streams.first())
        .unwrap_or_else(|| panic!("ffprobe 未找到{kind}流"))
}

fn json_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
    })
}

fn json_f64(value: Option<&Value>) -> Option<f64> {
    value.and_then(|value| {
        value
            .as_f64()
            .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
    })
}

fn parse_frame_rate(value: &str) -> f64 {
    let (numerator, denominator) = value
        .split_once('/')
        .unwrap_or_else(|| panic!("无法解析 ffprobe 帧率：{value}"));
    let numerator: f64 = numerator.parse().expect("帧率分子不是数字");
    let denominator: f64 = denominator.parse().expect("帧率分母不是数字");
    assert!(denominator > 0.0, "帧率分母必须大于 0");
    numerator / denominator
}

fn video_codec_name(codec: MediaVideoCodec) -> &'static str {
    match codec {
        MediaVideoCodec::H264 => "h264",
        MediaVideoCodec::H265 => "hevc",
    }
}

fn tool_version(tool: &str) -> String {
    let output = Command::new(tool)
        .arg("-version")
        .output()
        .unwrap_or_else(|error| panic!("读取媒体工具版本失败：{error}"));
    assert!(output.status.success(), "媒体工具版本探测失败：{tool}");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

fn last_output_line(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .last()
        .unwrap_or("未知错误")
        .trim()
        .to_string()
}
