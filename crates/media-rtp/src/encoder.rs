//! 视频编码器选择:硬件优先,软编兜底(media-pipeline-v2 P2a)。
//!
//! 采集仍由 FFmpeg 子进程完成,这里只决定 `-c:v` 及其参数:
//! - Windows:NVENC → QSV → AMF → Media Foundation → x264/x265
//! - macOS:VideoToolbox → x264/x265
//!
//! 每个候选在首次使用时用 `testsrc2` 实际试编 12 帧,校验"有输出 + 每个 IDR
//! 前都带参数集 + IDR 按 GOP 出现";驱动在但编不出来、或不按 GOP 出关键帧的
//! 编码器会被跳过。结果进程内缓存;运行期失败可调用 [`mark_failed`],下次
//! 选择时跳过该后端。
//!
//! 统一参数:无 B 帧、固定 GOP(`-g` 加 `-force_key_frames`,AMF H.264 不认 `-g`)、
//! CBR(`-maxrate`/`-bufsize` 等于码率)、`dump_extra=freq=keyframe` 在每个关键帧
//! 前补参数集。调试时可用环境变量 `UVP_VIDEO_ENCODER` 强制指定后端。

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use serde::Serialize;

use crate::ps::VideoCodec;

/// 编码后端。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EncoderBackend {
    Nvenc,
    Qsv,
    Amf,
    MediaFoundation,
    VideoToolbox,
    /// libx264 / libx265。
    Software,
}

impl EncoderBackend {
    /// 是否为硬件编码。
    pub const fn is_hardware(self) -> bool {
        !matches!(self, Self::Software)
    }

    /// 界面上显示的名称。
    pub const fn label(self) -> &'static str {
        match self {
            Self::Nvenc => "NVENC",
            Self::Qsv => "Quick Sync",
            Self::Amf => "AMD AMF",
            Self::MediaFoundation => "Media Foundation",
            Self::VideoToolbox => "VideoToolbox",
            Self::Software => "CPU",
        }
    }

    /// FFmpeg 编码器名。
    pub const fn ffmpeg_encoder(self, codec: VideoCodec) -> &'static str {
        match (self, codec) {
            (Self::Nvenc, VideoCodec::H264) => "h264_nvenc",
            (Self::Nvenc, VideoCodec::H265) => "hevc_nvenc",
            (Self::Qsv, VideoCodec::H264) => "h264_qsv",
            (Self::Qsv, VideoCodec::H265) => "hevc_qsv",
            (Self::Amf, VideoCodec::H264) => "h264_amf",
            (Self::Amf, VideoCodec::H265) => "hevc_amf",
            (Self::MediaFoundation, VideoCodec::H264) => "h264_mf",
            (Self::MediaFoundation, VideoCodec::H265) => "hevc_mf",
            (Self::VideoToolbox, VideoCodec::H264) => "h264_videotoolbox",
            (Self::VideoToolbox, VideoCodec::H265) => "hevc_videotoolbox",
            (Self::Software, VideoCodec::H264) => "libx264",
            (Self::Software, VideoCodec::H265) => "libx265",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "nvenc" => Some(Self::Nvenc),
            "qsv" => Some(Self::Qsv),
            "amf" => Some(Self::Amf),
            "mf" | "media_foundation" => Some(Self::MediaFoundation),
            "videotoolbox" | "vt" => Some(Self::VideoToolbox),
            "software" | "cpu" | "x264" | "x265" => Some(Self::Software),
            _ => None,
        }
    }
}

/// 当前平台的候选顺序(不含软编,软编永远是最后兜底)。
pub fn hardware_candidates() -> &'static [EncoderBackend] {
    if cfg!(windows) {
        &[
            EncoderBackend::Nvenc,
            EncoderBackend::Qsv,
            EncoderBackend::Amf,
            EncoderBackend::MediaFoundation,
        ]
    } else if cfg!(target_os = "macos") {
        &[EncoderBackend::VideoToolbox]
    } else {
        &[]
    }
}

/// 编码参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeParams {
    pub bitrate_kbps: u32,
    /// GOP 长度(帧)。
    pub gop: u32,
}

/// 生成 `-c:v` 起的编码器参数(不含输出格式和输出目标)。纯函数,便于单测。
pub fn encoder_args(backend: EncoderBackend, codec: VideoCodec, params: EncodeParams) -> Vec<String> {
    let gop = params.gop.max(1);
    let bitrate = format!("{}k", params.bitrate_kbps.max(1));
    let mut args: Vec<String> = vec!["-c:v".into(), backend.ffmpeg_encoder(codec).into()];
    let backend_args: &[&str] = match backend {
        EncoderBackend::Nvenc => &[
            "-preset", "p1", "-tune", "ull", "-rc", "cbr", "-zerolatency", "1", "-delay", "0",
            "-forced-idr", "1",
        ],
        EncoderBackend::Qsv => &["-preset", "veryfast", "-look_ahead", "0", "-async_depth", "1"],
        EncoderBackend::Amf => &[
            "-usage", "ultralowlatency", "-quality", "speed", "-rc", "cbr", "-forced_idr", "1",
        ],
        EncoderBackend::MediaFoundation => &[
            "-hw_encoding", "1", "-scenario", "live_streaming", "-rate_control", "cbr",
        ],
        // 不用 -prio_speed:FFmpeg 6 才有,旧版会让探测失败而误回退软编。
        EncoderBackend::VideoToolbox => &["-realtime", "1", "-allow_sw", "0"],
        EncoderBackend::Software => &["-preset", "ultrafast", "-tune", "zerolatency"],
    };
    args.extend(backend_args.iter().map(|arg| (*arg).to_string()));
    if backend == EncoderBackend::Software {
        // 软编沿用 v1 参数:单 slice、无场景切换、无前瞻。
        match codec {
            VideoCodec::H264 => args.extend([
                "-x264-params".into(),
                format!(
                    "slices=1:sliced-threads=0:repeat-headers=1:keyint={gop}:min-keyint={gop}:scenecut=0:rc-lookahead=0"
                ),
                "-refs".into(),
                "1".into(),
            ]),
            VideoCodec::H265 => args.extend([
                "-x265-params".into(),
                format!(
                    "repeat-headers=1:keyint={gop}:min-keyint={gop}:scenecut=0:rc-lookahead=0:log-level=error"
                ),
            ]),
        }
    }
    args.extend([
        "-bf".into(),
        "0".into(),
        "-g".into(),
        gop.to_string(),
        // 部分硬件编码器(AMF H.264)不认 -g,按帧号强制关键帧兜底。
        "-force_key_frames".into(),
        format!("expr:gte(n,n_forced*{gop})"),
        "-b:v".into(),
        bitrate.clone(),
        "-maxrate".into(),
        bitrate.clone(),
        "-bufsize".into(),
        bitrate,
        "-pix_fmt".into(),
        if backend.is_hardware() { "nv12" } else { "yuv420p" }.into(),
        // 每个关键帧前补参数集,迟加入的订阅者/解码器都能起播。
        "-bsf:v".into(),
        "dump_extra=freq=keyframe".into(),
    ]);
    args
}

/// 选择结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SelectedEncoder {
    pub codec: VideoCodec,
    pub backend: EncoderBackend,
    pub hardware: bool,
}

impl SelectedEncoder {
    fn new(codec: VideoCodec, backend: EncoderBackend) -> Self {
        Self {
            codec,
            backend,
            hardware: backend.is_hardware(),
        }
    }
}

#[derive(Default)]
struct Registry {
    selected: HashMap<VideoCodec, EncoderBackend>,
    failed: HashSet<(VideoCodec, EncoderBackend)>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

/// 环境变量强制指定的后端(调试用)。
fn forced_backend() -> Option<EncoderBackend> {
    std::env::var("UVP_VIDEO_ENCODER")
        .ok()
        .and_then(|value| EncoderBackend::parse(&value))
}

/// 为一种编码选择后端。首次调用会试编探测(阻塞约 0.3~2 秒),之后走缓存。
pub fn select(ffmpeg: &str, codec: VideoCodec) -> SelectedEncoder {
    if let Some(backend) = forced_backend() {
        return SelectedEncoder::new(codec, backend);
    }
    let failed: HashSet<EncoderBackend> = {
        let registry = registry().lock().unwrap_or_else(|error| error.into_inner());
        if let Some(backend) = registry.selected.get(&codec) {
            return SelectedEncoder::new(codec, *backend);
        }
        registry
            .failed
            .iter()
            .filter(|(failed_codec, _)| *failed_codec == codec)
            .map(|(_, backend)| *backend)
            .collect()
    };
    let backend = hardware_candidates()
        .iter()
        .copied()
        .filter(|backend| !failed.contains(backend))
        .find(|backend| {
            let result = probe(ffmpeg, *backend, codec);
            match &result {
                Ok(()) => tracing::info!(?codec, encoder = backend.ffmpeg_encoder(codec), "硬件编码器探测通过"),
                Err(reason) => tracing::info!(?codec, encoder = backend.ffmpeg_encoder(codec), %reason, "硬件编码器不可用"),
            }
            result.is_ok()
        })
        .unwrap_or(EncoderBackend::Software);
    if !backend.is_hardware() {
        tracing::warn!(?codec, "没有可用的硬件编码器,使用 CPU 编码");
    }
    registry()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .selected
        .insert(codec, backend);
    SelectedEncoder::new(codec, backend)
}

/// 运行期发现某后端不可用(如首帧前 FFmpeg 退出),下次选择跳过它。
pub fn mark_failed(codec: VideoCodec, backend: EncoderBackend) {
    if !backend.is_hardware() {
        return;
    }
    let mut registry = registry().lock().unwrap_or_else(|error| error.into_inner());
    registry.failed.insert((codec, backend));
    if registry.selected.get(&codec) == Some(&backend) {
        registry.selected.remove(&codec);
    }
    tracing::warn!(?codec, encoder = backend.ffmpeg_encoder(codec), "硬件编码器运行失败,下次改用其他后端");
}

/// 两种编码当前的选择结果(未探测过的会触发探测)。
pub fn status(ffmpeg: &str) -> Vec<SelectedEncoder> {
    [VideoCodec::H264, VideoCodec::H265]
        .into_iter()
        .map(|codec| select(ffmpeg, codec))
        .collect()
}

/// 探测用的 GOP:12 帧里应出现 3 个 IDR(第 0、5、10 帧)。
const PROBE_GOP: u32 = 5;
const PROBE_FRAMES: u32 = 12;

/// 用测试图试编,校验输出可作为直播码流使用。
fn probe(ffmpeg: &str, backend: EncoderBackend, codec: VideoCodec) -> Result<(), String> {
    let mut args: Vec<String> = [
        "-hide_banner",
        "-nostdin",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x240:rate=25",
        "-frames:v",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    args.push(PROBE_FRAMES.to_string());
    args.extend(encoder_args(
        backend,
        codec,
        EncodeParams {
            bitrate_kbps: 500,
            gop: PROBE_GOP,
        },
    ));
    args.extend([
        "-f".into(),
        match codec {
            VideoCodec::H264 => "h264",
            VideoCodec::H265 => "hevc",
        }
        .into(),
        "pipe:1".into(),
    ]);
    let output = run_with_timeout(ffmpeg, &args, std::time::Duration::from_secs(10))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.lines().last().unwrap_or("编码器启动失败").to_string());
    }
    let check = check_stream(&output.stdout, codec);
    if check.frames == 0 {
        return Err("没有输出帧".into());
    }
    if check.irap_frames < 2 {
        return Err(format!("关键帧不按 GOP 出现({} 帧中只有 {} 个)", check.frames, check.irap_frames));
    }
    if !check.params_before_every_irap {
        return Err("关键帧前缺少参数集".into());
    }
    Ok(())
}

fn run_with_timeout(
    ffmpeg: &str,
    args: &[String],
    timeout: std::time::Duration,
) -> Result<std::process::Output, String> {
    use std::process::Stdio;
    let mut child = common::hidden_command(ffmpeg)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("启动 ffmpeg 失败: {error}"))?;
    let deadline = std::time::Instant::now() + timeout;
    // 输出很小(几十 KB),读线程避免管道写满阻塞子进程。
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stdout.as_mut() {
            let _ = std::io::Read::read_to_end(pipe, &mut buf);
        }
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stderr.as_mut() {
            let _ = std::io::Read::read_to_end(pipe, &mut buf);
        }
        buf
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("试编超时".into());
            }
            Err(error) => return Err(format!("等待 ffmpeg 失败: {error}")),
        }
    };
    Ok(std::process::Output {
        status,
        stdout: out_reader.join().unwrap_or_default(),
        stderr: err_reader.join().unwrap_or_default(),
    })
}

/// 码流结构检查结果。
#[derive(Debug, Default, PartialEq, Eq)]
struct StreamCheck {
    frames: usize,
    irap_frames: usize,
    params_before_every_irap: bool,
}

/// 统计帧数、IRAP 帧数,并检查每个 IRAP 帧前(上一帧之后)都带参数集。
///
/// 以"首个 slice"标志区分帧边界:H.264 `first_mb_in_slice == 0`,
/// H.265 `first_slice_segment_in_pic_flag == 1`,二者都是 slice 头的首位。
fn check_stream(data: &[u8], codec: VideoCodec) -> StreamCheck {
    let mut check = StreamCheck {
        params_before_every_irap: true,
        ..StreamCheck::default()
    };
    let mut params_since_frame: HashSet<u8> = HashSet::new();
    let required: &[u8] = match codec {
        VideoCodec::H264 => &[7, 8],
        VideoCodec::H265 => &[32, 33, 34],
    };
    let mut index = 0;
    while index + 3 < data.len() {
        if !(data[index] == 0 && data[index + 1] == 0 && data[index + 2] == 1) {
            index += 1;
            continue;
        }
        let header = index + 3;
        index = header;
        let (nal_type, first_slice_byte, is_vcl, is_irap) = match codec {
            VideoCodec::H264 => {
                let nal_type = data[header] & 0x1f;
                (nal_type, header + 1, matches!(nal_type, 1 | 5), nal_type == 5)
            }
            VideoCodec::H265 => {
                let nal_type = (data[header] >> 1) & 0x3f;
                (nal_type, header + 2, nal_type < 32, (16..=21).contains(&nal_type))
            }
        };
        if !is_vcl {
            if required.contains(&nal_type) {
                params_since_frame.insert(nal_type);
            }
            continue;
        }
        let first_slice = data
            .get(first_slice_byte)
            .is_some_and(|byte| byte & 0x80 != 0);
        if !first_slice {
            continue;
        }
        check.frames += 1;
        if is_irap {
            check.irap_frames += 1;
            if !required.iter().all(|nal| params_since_frame.contains(nal)) {
                check.params_before_every_irap = false;
            }
        }
        params_since_frame.clear();
    }
    check
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> EncodeParams {
        EncodeParams {
            bitrate_kbps: 4000,
            gop: 50,
        }
    }

    fn has_pair(args: &[String], key: &str, value: &str) -> bool {
        args.windows(2).any(|pair| pair[0] == key && pair[1] == value)
    }

    #[test]
    fn 所有后端都带统一的低延迟参数() {
        for backend in [
            EncoderBackend::Nvenc,
            EncoderBackend::Qsv,
            EncoderBackend::Amf,
            EncoderBackend::MediaFoundation,
            EncoderBackend::VideoToolbox,
            EncoderBackend::Software,
        ] {
            for codec in [VideoCodec::H264, VideoCodec::H265] {
                let args = encoder_args(backend, codec, params());
                assert!(has_pair(&args, "-c:v", backend.ffmpeg_encoder(codec)), "{backend:?} {codec:?}");
                assert!(has_pair(&args, "-bf", "0"));
                assert!(has_pair(&args, "-g", "50"));
                assert!(has_pair(&args, "-force_key_frames", "expr:gte(n,n_forced*50)"));
                assert!(has_pair(&args, "-b:v", "4000k"));
                assert!(has_pair(&args, "-maxrate", "4000k"));
                assert!(has_pair(&args, "-bsf:v", "dump_extra=freq=keyframe"));
                let pix = if backend.is_hardware() { "nv12" } else { "yuv420p" };
                assert!(has_pair(&args, "-pix_fmt", pix));
            }
        }
    }

    #[test]
    fn 软编保留v1参数() {
        let args = encoder_args(EncoderBackend::Software, VideoCodec::H264, params());
        assert!(has_pair(&args, "-preset", "ultrafast"));
        assert!(args.iter().any(|arg| arg.contains("repeat-headers=1") && arg.contains("keyint=50")));
        let args = encoder_args(EncoderBackend::Software, VideoCodec::H265, params());
        assert!(has_pair(&args, "-c:v", "libx265"));
        assert!(args.iter().any(|arg| arg.starts_with("repeat-headers=1")));
    }

    #[test]
    fn 平台候选顺序() {
        let candidates = hardware_candidates();
        if cfg!(windows) {
            assert_eq!(
                candidates,
                &[
                    EncoderBackend::Nvenc,
                    EncoderBackend::Qsv,
                    EncoderBackend::Amf,
                    EncoderBackend::MediaFoundation
                ]
            );
        } else if cfg!(target_os = "macos") {
            assert_eq!(candidates, &[EncoderBackend::VideoToolbox]);
        }
        assert!(!candidates.contains(&EncoderBackend::Software));
    }

    #[test]
    fn 序列化名与前端类型一致() {
        let json = serde_json::to_string(&SelectedEncoder::new(VideoCodec::H265, EncoderBackend::MediaFoundation)).unwrap();
        assert_eq!(json, r#"{"codec":"h265","backend":"media_foundation","hardware":true}"#);
        assert_eq!(serde_json::to_string(&EncoderBackend::VideoToolbox).unwrap(), r#""video_toolbox""#);
    }

    #[test]
    fn 后端名解析() {
        assert_eq!(EncoderBackend::parse(" NVENC "), Some(EncoderBackend::Nvenc));
        assert_eq!(EncoderBackend::parse("mf"), Some(EncoderBackend::MediaFoundation));
        assert_eq!(EncoderBackend::parse("cpu"), Some(EncoderBackend::Software));
        assert_eq!(EncoderBackend::parse("gpu"), None);
    }

    fn h264_frame(nal_type: u8) -> Vec<u8> {
        // 起始码 + NAL 头 + first_mb_in_slice=0(ue '1' → 首位为 1)。
        vec![0, 0, 0, 1, nal_type, 0x88, 0x10]
    }

    #[test]
    fn 码流检查_每个idr前都有参数集() {
        let mut stream = Vec::new();
        for gop in 0..3 {
            stream.extend([0, 0, 0, 1, 0x67, 0x42]);
            stream.extend([0, 0, 0, 1, 0x68, 0xCE]);
            stream.extend(h264_frame(0x65));
            for _ in 0..4 {
                stream.extend(h264_frame(0x41));
            }
            let _ = gop;
        }
        let check = check_stream(&stream, VideoCodec::H264);
        assert_eq!(check.frames, 15);
        assert_eq!(check.irap_frames, 3);
        assert!(check.params_before_every_irap);
    }

    #[test]
    fn 码流检查_第二个idr缺参数集() {
        let mut stream = Vec::new();
        stream.extend([0, 0, 0, 1, 0x67, 0x42]);
        stream.extend([0, 0, 0, 1, 0x68, 0xCE]);
        stream.extend(h264_frame(0x65));
        stream.extend(h264_frame(0x41));
        stream.extend(h264_frame(0x65));
        let check = check_stream(&stream, VideoCodec::H264);
        assert_eq!(check.irap_frames, 2);
        assert!(!check.params_before_every_irap);
    }

    #[test]
    fn 码流检查_多slice帧只计一次() {
        let mut stream = vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xCE];
        stream.extend(h264_frame(0x65));
        // 同一帧的第二个 slice:first_mb_in_slice != 0 → 首位为 0。
        stream.extend([0, 0, 0, 1, 0x65, 0x40, 0x10]);
        let check = check_stream(&stream, VideoCodec::H264);
        assert_eq!(check.frames, 1);
        assert_eq!(check.irap_frames, 1);
    }

    #[test]
    fn 失败标记后跳过且软编不受影响() {
        // 只操作 H265 + 一个不会被其他测试探测的后端,避免与真实探测互相干扰。
        mark_failed(VideoCodec::H265, EncoderBackend::Qsv);
        assert!(registry()
            .lock()
            .unwrap()
            .failed
            .contains(&(VideoCodec::H265, EncoderBackend::Qsv)));
        mark_failed(VideoCodec::H265, EncoderBackend::Software);
        assert!(!registry()
            .lock()
            .unwrap()
            .failed
            .contains(&(VideoCodec::H265, EncoderBackend::Software)));
    }

    /// 本机真实试编。需要 FFmpeg;CI 无 GPU 时也能跑(会回退到软编)。
    #[test]
    #[ignore = "调用本机 FFmpeg 与 GPU,手动运行"]
    fn 本机探测结果() {
        let Some(ffmpeg) = crate::source::ffmpeg_bin() else {
            eprintln!("未找到 ffmpeg,跳过");
            return;
        };
        for codec in [VideoCodec::H264, VideoCodec::H265] {
            for backend in hardware_candidates()
                .iter()
                .copied()
                .chain([EncoderBackend::Software])
            {
                eprintln!(
                    "{codec:?} {:<18} {:?}",
                    backend.ffmpeg_encoder(codec),
                    probe(&ffmpeg, backend, codec)
                );
            }
            eprintln!("=> 选中 {:?}", select(&ffmpeg, codec));
        }
    }
}
