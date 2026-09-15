//! DirectShow input and Windows software encoding parameters.
use super::LiveCaptureProfile;

pub(super) fn command_args(
    video: &str,
    audio: Option<&str>,
    profile: &LiveCaptureProfile,
    mode_args: &[String],
) -> Vec<String> {
    use super::LiveVideoCodec;
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
    let gop = profile.keyframe_interval_frames();
    let codec_args: Vec<String> = match profile.video_codec {
        LiveVideoCodec::H264 => [
            "-c:v".into(), "libx264".into(), "-preset".into(), "ultrafast".into(),
            "-tune".into(), "zerolatency".into(), "-x264-params".into(),
            format!("slices=1:sliced-threads=0:repeat-headers=1:keyint={gop}:min-keyint={gop}:scenecut=0:rc-lookahead=0"),
            "-refs".into(), "1".into(),
        ].into(),
        LiveVideoCodec::H265 => [
            "-c:v".into(), "libx265".into(), "-preset".into(), "ultrafast".into(),
            "-tune".into(), "zerolatency".into(), "-x265-params".into(),
            format!("repeat-headers=1:keyint={gop}:min-keyint={gop}:scenecut=0:rc-lookahead=0:log-level=error"),
        ].into(),
    };
    args.extend(codec_args);
    args.extend([
        "-bf".into(),
        "0".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-b:v".into(),
        format!("{}k", profile.bitrate_kbps),
        "-g".into(),
        gop.to_string(),
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
            &mode,
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
            &[],
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
    }
}
