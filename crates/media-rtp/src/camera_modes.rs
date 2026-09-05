//! AVFoundation input modes are independent of the encoded output profile.

use common::{Error, Result};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CameraInputMode {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CameraModeRange {
    width: u32,
    height: u32,
    min_fps: f64,
    max_fps: f64,
}

fn parse_modes(stderr: &str) -> Vec<CameraModeRange> {
    stderr
        .lines()
        .filter_map(|line| {
            let (prefix, range) = line.split_once("@[")?;
            let (width, height) = prefix.split_whitespace().last()?.split_once('x')?;
            let (range, suffix) = range.split_once(']')?;
            if suffix.trim() != "fps" {
                return None;
            }
            let mut rates = range.split_whitespace();
            let mode = CameraModeRange {
                width: width.parse().ok()?,
                height: height.parse().ok()?,
                min_fps: rates.next()?.parse().ok()?,
                max_fps: rates.next()?.parse().ok()?,
            };
            (mode.width > 0
                && mode.height > 0
                && mode.min_fps.is_finite()
                && mode.max_fps.is_finite()
                && mode.min_fps > 0.0
                && mode.max_fps >= mode.min_fps
                && rates.next().is_none())
            .then_some(mode)
        })
        .collect()
}

fn select_mode(
    modes: &[CameraModeRange],
    width: u32,
    height: u32,
    fps: u32,
) -> Option<CameraInputMode> {
    let preferred_fps = f64::from(super::camera_input_fps(fps));
    modes
        .iter()
        .map(|mode| CameraInputMode {
            width: mode.width,
            height: mode.height,
            // AVFoundation in FFmpeg selects a range by maxFrameRate and uses
            // its minFrameDuration; arbitrary rates inside a range are rejected.
            fps: mode.max_fps,
        })
        .min_by(|left, right| {
            let rank = |mode: &CameraInputMode| {
                (
                    (f64::from(fps) - mode.fps).max(0.0),
                    u8::from(mode.width < width || mode.height < height),
                    (f64::from(mode.width) / f64::from(mode.height)
                        - f64::from(width) / f64::from(height))
                    .abs(),
                    (u64::from(mode.width) * u64::from(mode.height))
                        .abs_diff(u64::from(width) * u64::from(height)),
                    (mode.fps - preferred_fps).abs(),
                )
            };
            rank(left)
                .partial_cmp(&rank(right))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

pub(super) fn probe_camera_mode(
    ffmpeg: &str,
    index: u32,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<CameraInputMode> {
    let modes = probe_modes(ffmpeg, index, Duration::from_secs(10))?;
    select_mode(&modes, width, height, fps)
        .ok_or_else(|| Error::Media(format!("摄像头 {index} 未返回可用的采集尺寸和帧率")))
}

fn probe_modes(ffmpeg: &str, index: u32, timeout: Duration) -> Result<Vec<CameraModeRange>> {
    // FFmpeg's AVFoundation backend has no list_formats option. An unsupported
    // input size enumerates all formats and exits before starting frame capture.
    let mut child = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-f",
            "avfoundation",
            "-video_size",
            "1x1",
            "-framerate",
            "30",
            "-i",
            &format!("{index}:none"),
            "-frames:v",
            "1",
            "-f",
            "null",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| Error::Media(format!("摄像头 {index} 能力查询启动失败: {error}")))?;
    let stderr = child.stderr.take().expect("piped camera capability stderr");
    let reader = std::thread::spawn(move || {
        let mut output = String::new();
        for line in BufReader::new(stderr)
            .lines()
            .map_while(std::result::Result::ok)
        {
            if output.len() + line.len() < 128 * 1024 {
                output.push_str(&line);
                output.push('\n');
            }
        }
        output
    });
    let deadline = Instant::now() + timeout;
    let wait_error = loop {
        match child.try_wait() {
            Ok(Some(_)) => break None,
            Err(error) => break Some(format!("能力查询失败: {error}")),
            Ok(None) if Instant::now() >= deadline => {
                break Some("能力查询超时，请检查摄像头权限或设备状态".into())
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    if wait_error.is_some() {
        let _ = child.kill();
    }
    let _ = child.wait();
    let stderr = reader.join().unwrap_or_default();
    if let Some(error) = wait_error {
        return Err(Error::Media(format!("摄像头 {index} {error}")));
    }
    let modes = parse_modes(&stderr);
    if modes.is_empty() {
        let tail = stderr
            .lines()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" | ");
        return Err(Error::Media(format!(
            "摄像头 {index} 未返回支持的采集模式: {tail}"
        )));
    }
    Ok(modes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESK_VIEW: &str = "[AVFoundation indev @ 0x123] Selected video size (1x1) is not supported by the device.\n[AVFoundation indev @ 0x123] Supported modes:\n[AVFoundation indev @ 0x123]   1920x1440@[15.000000 30.000000]fps\nError opening input files: Input/output error";

    struct ProbeFixture(std::path::PathBuf);
    impl ProbeFixture {
        fn new(script: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let id = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir =
                std::env::temp_dir().join(format!("uvp-camera-probe-{}-{id}", std::process::id()));
            std::fs::create_dir(&dir).unwrap();
            let path = dir.join("ffmpeg");
            std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(dir)
        }
        fn binary(&self) -> String {
            self.0.join("ffmpeg").to_string_lossy().into_owned()
        }
    }
    impl Drop for ProbeFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn capability_probe_accepts_expected_error_exit_and_propagates_permission_errors() {
        let fixture =
            ProbeFixture::new(&format!("cat >&2 <<'MODES'\n{DESK_VIEW}\nMODES\nexit 251"));
        assert_eq!(
            probe_camera_mode(&fixture.binary(), 1, 1280, 720, 25)
                .unwrap()
                .width,
            1920
        );
        let denied = ProbeFixture::new("echo 'not authorized to capture video' >&2\nexit 1");
        assert!(probe_camera_mode(&denied.binary(), 1, 1280, 720, 25)
            .unwrap_err()
            .to_string()
            .contains("not authorized"));
    }

    #[test]
    fn hung_capability_probe_is_killed_within_deadline() {
        let fixture = ProbeFixture::new("exec /bin/sleep 5");
        let started = Instant::now();
        let error = probe_modes(&fixture.binary(), 1, Duration::from_millis(60)).unwrap_err();
        assert!(error.to_string().contains("超时"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn desk_view_selects_advertised_native_size_for_720p_output() {
        assert_eq!(
            select_mode(&parse_modes(DESK_VIEW), 1280, 720, 25),
            Some(CameraInputMode {
                width: 1920,
                height: 1440,
                fps: 30.0
            })
        );
    }

    #[test]
    fn ordinary_camera_prefers_matching_resolution_without_upscaling() {
        let modes = parse_modes("1280x720@[15 30]fps\n1920x1080@[15 30]fps\n3840x2160@[15 30]fps");
        assert_eq!(
            select_mode(&modes, 1920, 1080, 25),
            Some(CameraInputMode {
                width: 1920,
                height: 1080,
                fps: 30.0
            })
        );
        assert_eq!(select_mode(&modes, 1280, 720, 25).unwrap().width, 1280);
    }

    #[test]
    fn ffmpeg_requires_the_advertised_range_maximum() {
        let modes = parse_modes("1920x1080@[15 60]fps");
        assert_eq!(select_mode(&modes, 1920, 1080, 25).unwrap().fps, 60.0);
        let modes = parse_modes("1920x1080@[15 60]fps\n1920x1080@[15 30]fps");
        assert_eq!(select_mode(&modes, 1920, 1080, 25).unwrap().fps, 30.0);
    }

    #[test]
    fn respects_fractional_and_low_frame_rate_ranges() {
        let modes = parse_modes("1920x1080@[29.970030 29.970030]fps");
        assert_eq!(select_mode(&modes, 1920, 1080, 25).unwrap().fps, 29.970030);
        let modes = parse_modes("1920x1440@[15 15]fps");
        assert_eq!(select_mode(&modes, 1920, 1080, 25).unwrap().fps, 15.0);
    }

    #[test]
    fn prefers_requested_cadence_and_preserves_legacy_60_fps() {
        let modes = parse_modes("1920x1080@[15 15]fps\n1280x720@[15 60]fps");
        assert_eq!(select_mode(&modes, 1920, 1080, 25).unwrap().width, 1280);
        assert_eq!(select_mode(&modes, 1280, 720, 60).unwrap().fps, 60.0);
    }

    #[test]
    fn rejects_invalid_modes_and_permission_errors_without_guessing() {
        for text in [
            "not authorized",
            "0x720@[15 30]fps",
            "1280x720@[NaN 30]fps",
            "1280x720@[30 15]fps",
            "1280x720@[0 30]fps",
            "1280x720@[15 inf]fps",
        ] {
            assert!(parse_modes(text).is_empty(), "{text}");
            assert!(select_mode(&parse_modes(text), 1280, 720, 25).is_none());
        }
    }
}
