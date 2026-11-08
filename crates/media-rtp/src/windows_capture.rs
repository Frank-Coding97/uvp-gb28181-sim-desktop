//! DirectShow input and Windows software encoding parameters.
use super::LiveCaptureProfile;

pub(super) fn command_args(
    video: &str,
    audio: Option<&str>,
    profile: &LiveCaptureProfile,
    // 摄像头实际能给的尺寸。与 profile 不一致时靠 scale/pad 补齐，
    // 保证推出去的画面尺寸始终等于配置的输出分辨率。
    input_size: (u32, u32),
    mode_args: &[String],
    // 需要烧进码流的 OSD；None 时滤镜与原来完全一致。
    osd: Option<&crate::osd::OsdBurnIn>,
    // 采集端是否做一次水平翻转。笔记本内置摄像头在 DirectShow 上普遍输出
    // 自拍镜像画面，直接推给平台会让左右方向与实际相反；这里统一翻正。
    mirror: bool,
) -> Vec<String> {
    // Use the Windows capture graph clock. Redirected cameras can report a
    // device clock that lags the microphone clock; keep actual capture timing
    // instead of synthesizing timestamps from the configured frame rate.
    let input = audio.map_or_else(
        || format!("video={video}"),
        |audio| format!("video={video}:audio={audio}"),
    );
    let mut args: Vec<String> = [
        "-hide_banner",
        "-nostdin",
        "-loglevel",
        if audio.is_some() { "quiet" } else { "warning" },
        "-f",
        "dshow",
        "-thread_queue_size",
        "8",
        "-rtbufsize",
        "64M",
        "-use_video_device_timestamps",
        "false",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    if audio.is_some() {
        // DirectShow's device default can buffer multiples of 500 ms.
        args.extend(["-audio_buffer_size".into(), "100".into()]);
    }
    args.extend_from_slice(mode_args);
    args.extend(["-i".into(), input, "-map".into(), "0:v:0".into()]);
    // 摄像头不支持配置的输出分辨率时(如 720p 摄像头配 1080p 档位),
    // 先按比例缩放再用黑边补齐,而不是把 720p 原始帧直接推给平台:
    // 平台的 SDP/目录里声明的是 profile 尺寸,实际帧也必须一致。
    //
    // 翻转必须排在 OSD 之前:drawtext 画在这种已经翻正的画面上,字幕才不会是反字。
    let mut chain: Vec<String> = Vec::new();
    if mirror {
        chain.push("hflip".to_string());
    }
    if input_size != (profile.width, profile.height) {
        chain.push(scale_pad_filter(profile.width, profile.height));
    }
    let base = chain.join(",");
    // OSD 必须画在缩放之后，坐标才对应输出分辨率。
    let filter = match osd {
        Some(osd) => osd.graph(&base, profile.width, profile.height),
        None => base,
    };
    if !filter.is_empty() {
        args.extend(["-vf".into(), filter]);
    }
    args.extend(encoder_args(profile));
    args
}

/// 把任意输入尺寸等比缩放到输出档位并补黑边，保证推流尺寸与 profile 一致。
pub(super) fn scale_pad_filter(width: u32, height: u32) -> String {
    format!(
        "scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2"
    )
}

/// 摄像头和屏幕采集共用的编码参数，保证两条路径的编码规格不会漂移。
///
/// 编码器由 `profile.encoder` 决定(硬件优先,见 [`crate::encoder`]);
/// 这里只追加输出节奏与输出目标。
pub(super) fn encoder_args(profile: &LiveCaptureProfile) -> Vec<String> {
    let mut args = crate::encoder::encoder_args(
        profile.encoder,
        profile.video_codec.ps_codec(),
        profile.encode_params(),
    );
    args.extend([
        "-fps_mode".into(),
        "cfr".into(),
        "-r".into(),
        profile.video_fps.to_string(),
        "-f".into(),
        profile.video_codec.ffmpeg_format().into(),
        "-flush_packets".into(),
        "1".into(),
        "pipe:1".into(),
    ]);
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::live::{LiveAudioCodec, LiveVideoCodec};

    fn profile(codec: LiveVideoCodec) -> LiveCaptureProfile {
        LiveCaptureProfile {
            width: 1920,
            height: 1080,
            video_fps: 30,
            bitrate_kbps: 6000,
            keyframe_interval_seconds: 2,
            video_codec: codec,
            audio_codec: LiveAudioCodec::Aac,
            audio_sample_rate_hz: 48000,
            encoder: crate::encoder::EncoderBackend::Software,
        }
    }

    #[test]
    fn dshow_command_preserves_requested_profile_and_unique_device() {
        let mode = [
            "-vcodec",
            "mjpeg",
            "-video_size",
            "1920x1080",
            "-framerate",
            "30",
        ]
        .map(str::to_string);
        let args = command_args(
            "@device_pnp_camera",
            None,
            &profile(LiveVideoCodec::H264),
            (1920, 1080),
            &mode,
            None,
            false,
        );
        for pair in [
            ["-f", "dshow"],
            ["-i", "video=@device_pnp_camera"],
            ["-c:v", "libx264"],
            ["-b:v", "6000k"],
            ["-g", "60"],
            ["-r", "30"],
            ["-fps_mode", "cfr"],
            ["-use_video_device_timestamps", "false"],
            ["-pix_fmt", "yuv420p"],
        ] {
            assert!(
                args.windows(2).any(|values| values == pair),
                "missing {pair:?}: {args:?}"
            );
        }
        assert_eq!(args.iter().filter(|arg| *arg == "-i").count(), 1);
        assert!(!args
            .iter()
            .any(|arg| arg.contains("avfoundation") || arg.contains("videotoolbox")));
        assert_eq!(args.last().map(String::as_str), Some("pipe:1"));
    }

    #[test]
    fn requested_microphone_shares_one_dshow_input_and_h265_remains_h265() {
        let args = command_args(
            "camera",
            Some("@device_cm_microphone"),
            &profile(LiveVideoCodec::H265),
            (1920, 1080),
            &[],
            None,
            false,
        );
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-i", "video=camera:audio=@device_cm_microphone"]));
        assert!(args.windows(2).any(|pair| pair == ["-c:v", "libx265"]));
        assert!(args.windows(2).any(|pair| pair == ["-f", "hevc"]));
        assert!(args.windows(2).any(|pair| pair == ["-bf", "0"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-audio_buffer_size", "100"]));
        // 输入尺寸已等于输出尺寸，不该再挂缩放滤镜。
        assert!(!args.iter().any(|arg| arg == "-vf"));
    }

    #[test]
    fn 选中硬件编码器时替换编码参数且输出尾部不变() {
        let mut nvenc = profile(LiveVideoCodec::H264);
        nvenc.encoder = crate::encoder::EncoderBackend::Nvenc;
        let args = command_args("camera", None, &nvenc, (1920, 1080), &[], None, false);
        assert!(args.windows(2).any(|pair| pair == ["-c:v", "h264_nvenc"]));
        assert!(args.windows(2).any(|pair| pair == ["-pix_fmt", "nv12"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-bsf:v", "dump_extra=freq=keyframe"]));
        assert!(!args.iter().any(|arg| arg.contains("x264")));
        let tail: Vec<&str> = args[args.len() - 9..].iter().map(String::as_str).collect();
        assert_eq!(
            tail,
            ["-fps_mode", "cfr", "-r", "30", "-f", "h264", "-flush_packets", "1", "pipe:1"]
        );
    }

    #[test]
    fn smaller_camera_input_is_scaled_back_to_the_output_profile() {
        let mode = [
            "-vcodec",
            "mjpeg",
            "-video_size",
            "1280x720",
            "-framerate",
            "30",
        ]
        .map(str::to_string);
        let args = command_args(
            "camera",
            None,
            &profile(LiveVideoCodec::H264),
            (1280, 720),
            &mode,
            None,
            false,
        );
        let filter = args
            .windows(2)
            .find(|pair| pair[0] == "-vf")
            .map(|pair| pair[1].as_str())
            .expect("smaller input must be scaled");
        assert!(filter.contains("scale=1920:1080"), "{filter}");
        assert!(filter.contains("pad=1920:1080"), "{filter}");
        // 滤镜必须挂在编码器之前，且输出帧率仍是配置值。
        let vf = args.iter().position(|arg| arg == "-vf").unwrap();
        let codec = args.iter().position(|arg| arg == "-c:v").unwrap();
        assert!(vf < codec);
        assert!(args.windows(2).any(|pair| pair == ["-r", "30"]));
    }

    #[test]
    fn osd_is_drawn_after_scaling_and_before_the_encoder() {
        use crate::osd::*;
        let osd = OsdBurnIn {
            overlay: OsdOverlay {
                base_width: 1920,
                base_height: 1080,
                time: Some(OsdTime {
                    anchor: OsdAnchor::Pixel { x: 10, y: 10 },
                    format: OsdTimeFormat::Dash,
                    style: OsdLayer::default(),
                }),
                texts: vec![],
                watermark: None,
                mask_base: (1920, 1080),
                masks: vec![],
            },
            font: "C:/f.ttc".into(),
        };
        let args = command_args(
            "camera",
            None,
            &profile(LiveVideoCodec::H264),
            (1280, 720),
            &[],
            Some(&osd),
            false,
        );
        let filter = args
            .windows(2)
            .find(|pair| pair[0] == "-vf")
            .map(|pair| pair[1].as_str())
            .expect("osd needs a filter");
        let scale = filter.find("scale=1920:1080").expect("scale first");
        assert!(scale < filter.find("drawtext").unwrap(), "{filter}");
        // 输入尺寸与输出一致时也必须挂 OSD 滤镜。
        let same = command_args(
            "camera",
            None,
            &profile(LiveVideoCodec::H264),
            (1920, 1080),
            &[],
            Some(&osd),
            false,
        );
        assert!(same.windows(2).any(|pair| pair[0] == "-vf" && pair[1].starts_with("null,drawtext")));
    }

    #[test]
    fn 摄像头默认水平翻转且翻转排在缩放与osd之前() {
        // 只在尺寸一致且无 OSD 时，翻转是唯一滤镜，不会顺手引入缩放。
        let plain = command_args("camera", None, &profile(LiveVideoCodec::H264), (1920, 1080), &[], None, true);
        assert!(
            plain.windows(2).any(|pair| pair == ["-vf", "hflip"]),
            "{plain:?}"
        );

        // 需要缩放时必须先翻转再缩放，避免滤镜顺序漂移。
        let scaled = command_args("camera", None, &profile(LiveVideoCodec::H264), (1280, 720), &[], None, true);
        let chain = scaled
            .windows(2)
            .find(|pair| pair[0] == "-vf")
            .map(|pair| pair[1].as_str())
            .expect("mirror needs a filter");
        assert!(chain.starts_with("hflip,scale=1920:1080"), "{chain}");
        assert!(chain.find("hflip").unwrap() < chain.find("scale=").unwrap(), "{chain}");

        // 翻转必须在 drawtext 之前，否则烧进画面的时间水印会变成反字。
        use crate::osd::*;
        let osd = OsdBurnIn {
            overlay: OsdOverlay {
                base_width: 1920,
                base_height: 1080,
                time: Some(OsdTime {
                    anchor: OsdAnchor::Pixel { x: 10, y: 10 },
                    format: OsdTimeFormat::Dash,
                    style: OsdLayer::default(),
                }),
                texts: vec![],
                watermark: None,
                mask_base: (1920, 1080),
                masks: vec![],
            },
            font: "C:/f.ttc".into(),
        };
        let with_osd = command_args("camera", None, &profile(LiveVideoCodec::H264), (1920, 1080), &[], Some(&osd), true);
        let chain = with_osd
            .windows(2)
            .find(|pair| pair[0] == "-vf")
            .map(|pair| pair[1].as_str())
            .expect("mirror + osd needs a filter");
        assert!(chain.starts_with("hflip,"), "{chain}");
        assert!(chain.find("hflip").unwrap() < chain.find("drawtext").unwrap(), "{chain}");
    }
}
