//! Windows DirectShow camera and microphone discovery.
//!
//! The DirectShow backend writes its device and capability listing to stderr.
//! This module keeps that parsing and the strict input-mode selection separate
//! from the live capture command assembly in `source_live.rs`.

use common::{Error, Result};

use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// A DirectShow device with the friendly name shown in the UI and the stable
/// alternative name used for opening the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WindowsCameraDevice {
    pub index: u32,
    pub name: String,
    pub input_name: String,
}

/// The two device families emitted by `-list_devices true`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WindowsCameraDevices {
    pub cameras: Vec<WindowsCameraDevice>,
    pub microphones: Vec<WindowsCameraDevice>,
}

/// A DirectShow input mode selected for an exact output size and a requested
/// frame rate. `input_args` contains only options that belong before `-i`.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct WindowsCameraInputMode {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub input_args: Vec<String>,
    pub pixel_format: Option<String>,
    pub vcodec: Option<String>,
}

/// Enumerate DirectShow video and audio devices.
pub(super) fn list_devices(ffmpeg: &str) -> Result<WindowsCameraDevices> {
    let mut command = common::hidden_command(ffmpeg);
    command.args([
        "-hide_banner",
        "-nostdin",
        "-list_devices",
        "true",
        "-f",
        "dshow",
        "-i",
        "dummy",
    ]);
    let (status, stderr) = run_stderr_command(command, probe_timeout(), "DirectShow 设备枚举")?;
    let devices = parse_device_listing(&stderr);
    if devices.cameras.is_empty() && devices.microphones.is_empty() {
        let status = format_exit_status(status);
        return Err(Error::Media(format!(
            "FFmpeg 未返回可解析的 DirectShow 设备 ({status}): {}",
            stderr_tail(&stderr)
        )));
    }
    Ok(devices)
}

/// Resolve a UI camera index to the device whose alternative name is passed to
/// DirectShow. Indices are category-local and zero based.
pub(super) fn camera(devices: &WindowsCameraDevices, index: u32) -> Result<&WindowsCameraDevice> {
    devices
        .cameras
        .iter()
        .find(|device| device.index == index)
        .ok_or_else(|| Error::Media(format!("未找到 DirectShow 摄像头索引 {index}")))
}

/// Probe and select a DirectShow input mode for the requested output size.
///
/// The returned mode may be smaller than the requested size when the device
/// cannot capture it; the caller passes the actual size to the encoder so the
/// output profile stays at the configured resolution.
pub(super) fn probe_mode(
    ffmpeg: &str,
    camera: &WindowsCameraDevice,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<WindowsCameraInputMode> {
    let modes = probe_modes(ffmpeg, camera)?;
    select_mode(&modes, width, height, fps).ok_or_else(|| {
        Error::Media(format!(
            "DirectShow 摄像头 {} 没有可用的采集模式（请求 {}x{}@{}fps），该设备支持: {}",
            camera.index,
            width,
            height,
            fps,
            describe_modes(&modes)
        ))
    })
}

/// 枚举设备支持的全部模式，供音视频设置页限制可选分辨率/帧率。
pub(super) fn list_modes(
    ffmpeg: &str,
    camera: &WindowsCameraDevice,
) -> Result<Vec<super::LiveCameraMode>> {
    Ok(probe_modes(ffmpeg, camera)?
        .into_iter()
        .map(|mode| super::LiveCameraMode {
            width: mode.width,
            height: mode.height,
            min_fps: mode.min_fps,
            max_fps: mode.max_fps,
            format: mode
                .vcodec
                .or(mode.pixel_format)
                .unwrap_or_else(|| "raw".to_string()),
        })
        .collect())
}

/// 让 FFmpeg 打开设备并列出能力表。`-list_options true` 会打印模式后
/// 以「Error opening input file」收尾，退出码非零属正常现象。
fn probe_modes(ffmpeg: &str, camera: &WindowsCameraDevice) -> Result<Vec<CameraModeRange>> {
    if camera.input_name.trim().is_empty() {
        return Err(Error::Media(format!(
            "DirectShow 摄像头 {} 没有可用的 alternative name",
            camera.index
        )));
    }
    let input = format!("video={}", camera.input_name);
    let mut command = common::hidden_command(ffmpeg);
    command.args([
        "-hide_banner",
        "-nostdin",
        "-loglevel",
        "info",
        "-list_options",
        "true",
        "-f",
        "dshow",
        "-i",
        &input,
    ]);
    let (_status, stderr) = run_stderr_command(command, probe_timeout(), "DirectShow 能力查询")?;
    let modes = parse_mode_listing(&stderr);
    if modes.is_empty() {
        return Err(Error::Media(format!(
            "DirectShow 摄像头 {}（{}）未报告任何可用的采集模式，请确认设备未被其他程序占用: {}",
            camera.index,
            camera.name,
            stderr_tail(&stderr)
        )));
    }
    Ok(modes)
}

/// 把枚举到的模式压成一句人话，供错误提示直接展示。
///
/// 原始 stderr 是逐行 `vcodec=... min s=... max s=...` 的调试输出，
/// 用户从中看不出「这台设备最高只有 720p」这个关键事实。
fn describe_modes(modes: &[CameraModeRange]) -> String {
    const MAX_ITEMS: usize = 6;
    let mut entries: Vec<(&CameraModeRange, String)> = Vec::new();
    for mode in modes {
        let format = mode
            .vcodec
            .as_deref()
            .or(mode.pixel_format.as_deref())
            .unwrap_or("raw");
        let label = format!("{}x{}@{}({format})", mode.width, mode.height, format_fps(mode.max_fps));
        if entries.iter().any(|(existing, _)| {
            existing.width == mode.width
                && existing.height == mode.height
                && (existing.max_fps - mode.max_fps).abs() < 0.001
        }) {
            continue;
        }
        entries.push((mode, label));
    }
    entries.sort_by(|left, right| {
        let area = |mode: &CameraModeRange| u64::from(mode.width) * u64::from(mode.height);
        area(right.0)
            .cmp(&area(left.0))
            .then_with(|| right.0.max_fps.total_cmp(&left.0.max_fps))
    });
    let hidden = entries.len().saturating_sub(MAX_ITEMS);
    let mut text = entries
        .iter()
        .take(MAX_ITEMS)
        .map(|(_, label)| label.clone())
        .collect::<Vec<_>>()
        .join("、");
    if hidden > 0 {
        text.push_str(&format!("，另有 {hidden} 种更小尺寸"));
    }
    text
}

/// Parse the device section of FFmpeg's DirectShow listing.
///
/// This is intentionally private to the platform module; the public catalog
/// uses `LiveAvDevice` and the source layer decides how to persist its URI.
fn parse_device_listing(stderr: &str) -> WindowsCameraDevices {
    #[derive(Clone, Copy)]
    enum Section {
        Cameras,
        Microphones,
        Ignore,
    }

    struct PendingDevice {
        section: Section,
        name: String,
        input_name: Option<String>,
    }

    let mut cameras = Vec::new();
    let mut microphones = Vec::new();
    let mut section = None;
    let mut pending = None;

    let finish = |pending: &mut Option<PendingDevice>,
                  cameras: &mut Vec<WindowsCameraDevice>,
                  microphones: &mut Vec<WindowsCameraDevice>| {
        let Some(device) = pending.take() else {
            return;
        };
        let input_name = device
            .input_name
            .filter(|name| !is_none_name(name))
            .unwrap_or_else(|| device.name.clone());
        if is_none_name(&device.name) || is_none_name(&input_name) {
            return;
        }
        if matches!(device.section, Section::Ignore) {
            return;
        }
        let destination = match device.section {
            Section::Cameras => &mut *cameras,
            Section::Microphones => &mut *microphones,
            Section::Ignore => return,
        };
        if destination
            .iter()
            .any(|existing| existing.input_name.eq_ignore_ascii_case(&input_name))
        {
            return;
        }
        destination.push(WindowsCameraDevice {
            index: destination.len() as u32,
            name: device.name,
            input_name,
        });
    };

    for line in stderr.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("directshow video devices") {
            finish(&mut pending, &mut cameras, &mut microphones);
            section = Some(Section::Cameras);
            continue;
        }
        if lower.contains("directshow audio devices") {
            finish(&mut pending, &mut cameras, &mut microphones);
            section = Some(Section::Microphones);
            continue;
        }

        if lower.contains("alternative name") {
            if let Some(alternative) = quoted_value_after(line, "alternative name")
                .or_else(|| value_after(line, "alternative name"))
            {
                if let Some(device) = pending.as_mut() {
                    if !is_none_name(&alternative) {
                        device.input_name = Some(alternative);
                    }
                }
            }
            continue;
        }

        let Some(name) = first_quoted_value(line) else {
            continue;
        };
        let explicit_section = if lower.contains("(video)") {
            Some(Section::Cameras)
        } else if lower.contains("(audio)") {
            Some(Section::Microphones)
        } else if lower.contains("(none)") {
            Some(Section::Ignore)
        } else {
            None
        };
        let current_section = explicit_section.or(section);
        let Some(current_section) = current_section else {
            continue;
        };
        finish(&mut pending, &mut cameras, &mut microphones);
        pending = Some(PendingDevice {
            section: current_section,
            name,
            input_name: None,
        });
        section = Some(current_section);
    }
    finish(&mut pending, &mut cameras, &mut microphones);

    WindowsCameraDevices {
        cameras,
        microphones,
    }
}

/// Parse all mode lines from FFmpeg's `-list_options true` output.
fn parse_mode_listing(stderr: &str) -> Vec<CameraModeRange> {
    let mut modes = Vec::new();
    for line in stderr.lines() {
        let Some(mode) = parse_mode_line(line) else {
            continue;
        };
        if !modes
            .iter()
            .any(|existing: &CameraModeRange| existing == &mode)
        {
            modes.push(mode);
        }
    }
    modes
}

/// Select a DirectShow input mode.
///
/// Preference order, each step only used when the previous one has no answer:
/// 1. the requested size at the requested frame rate;
/// 2. the requested size at the highest rate the device offers (cfr output
///    duplicates frames instead of failing);
/// 3. the closest supported size — same aspect ratio first, then the largest
///    mode that does not exceed the requested area.
///
/// Step 3 mirrors the AVFoundation path, where the input mode is independent of
/// the encoded output profile; the encoder scales/pads it back to the requested
/// output size (`windows_capture::command_args`). `None` means the device
/// reported no usable mode at all.
fn select_mode(
    modes: &[CameraModeRange],
    width: u32,
    height: u32,
    fps: u32,
) -> Option<WindowsCameraInputMode> {
    select_size_mode(modes, width, height, fps)
        .or_else(|| select_nearest_mode(modes, width, height, fps))
}

/// Modes that capture the requested size, preferring one that reaches `fps`.
fn select_size_mode(
    modes: &[CameraModeRange],
    width: u32,
    height: u32,
    fps: u32,
) -> Option<WindowsCameraInputMode> {
    let requested_fps = f64::from(fps.max(1));
    let same_size: Vec<&CameraModeRange> = modes
        .iter()
        .filter(|mode| mode.width == width && mode.height == height)
        .collect();
    if same_size.is_empty() {
        return None;
    }
    let fps_reachable = same_size
        .iter()
        .any(|mode| mode.max_fps + f64::EPSILON >= requested_fps);
    same_size
        .into_iter()
        .filter(|mode| !fps_reachable || mode.max_fps + f64::EPSILON >= requested_fps)
        .map(|mode| {
            let input_fps = preferred_input_fps(mode, requested_fps);
            let format_rank = match (&mode.pixel_format, &mode.vcodec) {
                (Some(_), _) => 0_u8,
                (None, Some(_)) => 1,
                (None, None) => 2,
            };
            (
                (
                    // 帧率缺口优先于格式:一个 60fps 的 MJPEG 模式
                    // 优于同一尺寸下只有 10fps 的 raw 模式。
                    (requested_fps - mode.max_fps).max(0.0),
                    format_rank,
                    (input_fps - requested_fps).max(0.0),
                    (mode.max_fps - requested_fps).max(0.0),
                    mode.pixel_format.as_deref().unwrap_or(""),
                    mode.vcodec.as_deref().unwrap_or(""),
                ),
                mode,
                input_fps,
            )
        })
        .min_by(|left, right| {
            left.0
                .partial_cmp(&right.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, mode, input_fps)| mode.to_input_mode(input_fps))
}

/// Closest supported mode for an output size the device cannot capture.
///
/// Ranking keeps the requested aspect ratio first (a 4:3 fallback would be
/// letterboxed into 16:9), then prefers a mode that does not exceed the
/// requested area — picking the largest such mode — so that a 1080p profile on
/// a 720p webcam yields 720p rather than a smaller mode upscaled twice.
fn select_nearest_mode(
    modes: &[CameraModeRange],
    width: u32,
    height: u32,
    fps: u32,
) -> Option<WindowsCameraInputMode> {
    let requested_fps = f64::from(fps.max(1));
    let requested_area = u64::from(width) * u64::from(height);
    let requested_ratio = f64::from(width) / f64::from(height.max(1));
    // No mode may reach the requested rate: still start, `-fps_mode cfr`
    // duplicates frames up to the output rate instead of failing the stream.
    let fps_reachable = modes
        .iter()
        .any(|mode| mode.max_fps + f64::EPSILON >= requested_fps);
    modes
        .iter()
        .map(|mode| {
            let input_fps = preferred_input_fps(mode, requested_fps);
            let area = u64::from(mode.width) * u64::from(mode.height);
            let ratio_gap = (f64::from(mode.width) / f64::from(mode.height.max(1))
                - requested_ratio)
                .abs();
            let format_rank = match (&mode.pixel_format, &mode.vcodec) {
                (Some(_), _) => 0_u8,
                (None, Some(_)) => 1,
                (None, None) => 2,
            };
            (
                (
                    u8::from(fps_reachable && mode.max_fps + f64::EPSILON < requested_fps),
                    // 0.0001 的量化精度足够区分常见宽高比(4:3 / 16:9 / 16:10)。
                    (ratio_gap * 10_000.0).round() as u64,
                    area.saturating_sub(requested_area),
                    area.abs_diff(requested_area),
                    format_rank,
                    (requested_fps - mode.max_fps).max(0.0).round() as u64,
                    mode.vcodec.as_deref().unwrap_or(""),
                ),
                mode,
                input_fps,
            )
        })
        .min_by(|left, right| left.0.cmp(&right.0))
        .map(|(_, mode, input_fps)| mode.to_input_mode(input_fps))
}

/// DirectShow input frame rate for a mode.
///
/// Many Windows webcams expose a continuous-looking range but only negotiate
/// stable discrete rates (notably 30/60 for MJPEG). Feed 30 fps to DirectShow
/// for a 25 fps output profile when available; the encoder still emits the
/// requested output rate.
fn preferred_input_fps(mode: &CameraModeRange, requested_fps: f64) -> f64 {
    let candidate = if requested_fps == 25.0 && mode.max_fps >= 30.0 && mode.min_fps <= 30.0 {
        30.0
    } else {
        requested_fps
    };
    candidate.clamp(mode.min_fps, mode.max_fps)
}

fn probe_timeout() -> Duration {
    Duration::from_secs(10)
}

#[derive(Debug, Clone, PartialEq)]
struct CameraModeRange {
    width: u32,
    height: u32,
    min_fps: f64,
    max_fps: f64,
    pixel_format: Option<String>,
    vcodec: Option<String>,
}

impl CameraModeRange {
    fn to_input_mode(&self, fps: f64) -> WindowsCameraInputMode {
        let mut input_args = Vec::with_capacity(8);
        if let Some(pixel_format) = &self.pixel_format {
            input_args.extend(["-pixel_format".into(), pixel_format.clone()]);
        }
        if let Some(vcodec) = &self.vcodec {
            input_args.extend(["-vcodec".into(), vcodec.clone()]);
        }
        input_args.extend([
            "-video_size".into(),
            format!("{}x{}", self.width, self.height),
            "-framerate".into(),
            format_fps(fps),
        ]);
        WindowsCameraInputMode {
            width: self.width,
            height: self.height,
            fps,
            input_args,
            pixel_format: self.pixel_format.clone(),
            vcodec: self.vcodec.clone(),
        }
    }
}

fn parse_mode_line(line: &str) -> Option<CameraModeRange> {
    let pixel_format = option_value(line, "pixel_format=");
    let vcodec = option_value(line, "vcodec=");
    if pixel_format.is_none() && vcodec.is_none() {
        return None;
    }

    let min_size = size_after(line, "min s=")?;
    let min_size_start = line.find("min s=")?;
    let min_fps = float_after(&line[min_size_start..], "fps=")?;
    let max_size_start = min_size_start + line[min_size_start..].find("max s=")?;
    let max_size = size_after(&line[max_size_start..], "max s=")?;
    let max_fps = float_after(&line[max_size_start..], "fps=")?;
    if min_size != max_size
        || min_fps <= 0.0
        || max_fps < min_fps
        || !min_fps.is_finite()
        || !max_fps.is_finite()
    {
        return None;
    }
    Some(CameraModeRange {
        width: min_size.0,
        height: min_size.1,
        min_fps,
        max_fps,
        pixel_format,
        vcodec,
    })
}

fn option_value(line: &str, marker: &str) -> Option<String> {
    let start = line.find(marker)? + marker.len();
    line[start..]
        .split_whitespace()
        .next()
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn size_after(line: &str, marker: &str) -> Option<(u32, u32)> {
    let value = option_value(line, marker)?;
    let (width, height) = value.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

fn float_after(line: &str, marker: &str) -> Option<f64> {
    option_value(line, marker)?.parse().ok()
}

fn format_fps(fps: f64) -> String {
    if (fps - fps.round()).abs() < 0.000_001 {
        return format!("{}", fps.round() as u64);
    }
    let mut formatted = format!("{fps:.6}");
    while formatted.ends_with('0') {
        formatted.pop();
    }
    if formatted.ends_with('.') {
        formatted.pop();
    }
    formatted
}

fn first_quoted_value(line: &str) -> Option<String> {
    let start = line.find('"')? + 1;
    let end = line[start..].find('"')?;
    Some(line[start..start + end].to_owned())
}

fn quoted_value_after(line: &str, marker: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    let marker_start = lower.find(marker)? + marker.len();
    first_quoted_value(&line[marker_start..])
}

fn value_after(line: &str, marker: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    let marker_start = lower.find(marker)? + marker.len();
    line[marker_start..]
        .trim()
        .trim_matches('"')
        .split_whitespace()
        .next()
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn is_none_name(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("none")
}

fn run_stderr_command(
    mut command: Command,
    timeout: Duration,
    operation: &str,
) -> Result<(ExitStatus, String)> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| Error::Media(format!("{operation}启动失败: {error}")))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::Media(format!("{operation}无法读取 FFmpeg stderr")))?;
    let reader = thread::spawn(move || read_stderr(stderr));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(Error::Media(format!("{operation}超时，请检查设备状态")));
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(Error::Media(format!("{operation}失败: {error}")));
            }
        }
    };
    let stderr = reader
        .join()
        .map_err(|_| Error::Media(format!("{operation}输出读取线程异常")))?;
    Ok((status, stderr))
}

fn read_stderr(stderr: impl std::io::Read) -> String {
    use std::io::Read;

    const MAX_STDERR_BYTES: usize = 256 * 1024;
    let mut reader = std::io::BufReader::new(stderr);
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(size) => {
                let remaining = MAX_STDERR_BYTES.saturating_sub(bytes.len());
                bytes.extend_from_slice(&chunk[..size.min(remaining)]);
                if bytes.len() == MAX_STDERR_BYTES {
                    break;
                }
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn format_exit_status(status: ExitStatus) -> String {
    status
        .code()
        .map_or_else(|| "进程被信号终止".into(), |code| format!("退出码 {code}"))
}

fn stderr_tail(stderr: &str) -> String {
    stderr
        .lines()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE_LISTING: &str = r#"
[dshow @ 000001] DirectShow video devices (some may be both video and audio devices)
[dshow @ 000001]  "Logitech Webcam C925e"
[dshow @ 000001]     Alternative name "@device_pnp_\\?\\usb#vid_046d&pid_085b&mi_00#abc#{video}"
[dshow @ 000001]  "Virtual Camera"
[dshow @ 000001]     Alternative name "@device_sw_{virtual-camera}"
[dshow @ 000001]  "Logitech Webcam C925e"
[dshow @ 000001]     Alternative name "@device_pnp_\\?\\usb#vid_046d&pid_085b&mi_00#abc#{video}"
[dshow @ 000001]  "none"
[dshow @ 000001] DirectShow audio devices
[dshow @ 000001]  "Microphone (Logitech Webcam C925e)"
[dshow @ 000001]     Alternative name "@device_cm_{audio}\\wave_{abc}"
[dshow @ 000001]  "Alternative name"
[dshow @ 000001]     Alternative name "none"
"#;

    const DEVICE_LISTING_WITH_KIND_SUFFIXES: &str = r#"
[dshow @ 000001] "OBS Virtual Camera" (none)
[dshow @ 000001] "Logitech Webcam C925e" (video)
[dshow @ 000001]     Alternative name "@device_pnp_\\?\\usb#vid_046d&pid_085b&mi_00#abc#{video}" (video)
[dshow @ 000001] "Microphone (Logitech Webcam C925e)" (audio)
[dshow @ 000001]     Alternative name "@device_cm_{audio}\\wave_{abc}" (audio)
"#;

    const MODE_LISTING: &str = r#"
[dshow @ 000001]   pixel_format=yuyv422 min s=1280x720 fps=5 max s=1280x720 fps=10
[dshow @ 000001]   vcodec=mjpeg min s=1280x720 fps=5 max s=1280x720 fps=60.0002
[dshow @ 000001]   vcodec=mjpeg min s=1920x1080 fps=5 max s=1920x1080 fps=30 tv,pc,color,bpp=24
[dshow @ 000001]   pixel_format=yuyv422 min s=640x480 fps=5 max s=640x480 fps=30
[dshow @ 000001] Could not enumerate video devices (or no video devices found)
[dshow @ 000001] Could not RenderStream: I/O error
"#;

    #[test]
    fn directshow_device_listing_preserves_friendly_and_alternative_names() {
        let devices = parse_device_listing(DEVICE_LISTING);

        assert_eq!(devices.cameras.len(), 2);
        assert_eq!(devices.cameras[0].index, 0);
        assert_eq!(devices.cameras[0].name, "Logitech Webcam C925e");
        assert_eq!(
            devices.cameras[0].input_name,
            r#"@device_pnp_\\?\\usb#vid_046d&pid_085b&mi_00#abc#{video}"#
        );
        assert_eq!(devices.cameras[1].index, 1);
        assert_eq!(devices.cameras[1].name, "Virtual Camera");

        assert_eq!(devices.microphones.len(), 1);
        assert_eq!(devices.microphones[0].index, 0);
        assert_eq!(
            devices.microphones[0].input_name,
            r#"@device_cm_{audio}\\wave_{abc}"#
        );
    }

    #[test]
    fn directshow_device_listing_accepts_kind_suffixes_without_section_headers() {
        let devices = parse_device_listing(DEVICE_LISTING_WITH_KIND_SUFFIXES);

        assert_eq!(devices.cameras.len(), 1);
        assert_eq!(devices.cameras[0].name, "Logitech Webcam C925e");
        assert_eq!(
            devices.cameras[0].input_name,
            r#"@device_pnp_\\?\\usb#vid_046d&pid_085b&mi_00#abc#{video}"#
        );
        assert_eq!(devices.microphones.len(), 1);
        assert_eq!(
            devices.microphones[0].name,
            "Microphone (Logitech Webcam C925e)"
        );
    }

    #[test]
    fn directshow_mode_listing_parses_pixel_format_vcodec_and_ranges() {
        let modes = parse_mode_listing(MODE_LISTING);

        assert_eq!(modes.len(), 4);
        assert_eq!((modes[0].width, modes[0].height), (1280, 720));
        assert_eq!(modes[0].pixel_format.as_deref(), Some("yuyv422"));
        assert_eq!(modes[0].vcodec, None);
        assert_eq!(modes[1].vcodec.as_deref(), Some("mjpeg"));
        assert_eq!(modes[1].pixel_format, None);
        assert!((modes[1].max_fps - 60.0002).abs() < 0.000_001);
    }

    #[test]
    fn directshow_mode_selection_prefers_exact_size_then_nearest() {
        let modes = parse_mode_listing(MODE_LISTING);

        let hd = select_mode(&modes, 1280, 720, 30).expect("mjpeg can provide 720p30");
        assert_eq!(hd.vcodec.as_deref(), Some("mjpeg"));
        assert_eq!((hd.width, hd.height), (1280, 720));
        assert!((hd.fps - 30.0).abs() < f64::EPSILON);
        assert_eq!(
            hd.input_args,
            [
                "-vcodec",
                "mjpeg",
                "-video_size",
                "1280x720",
                "-framerate",
                "30"
            ]
        );

        let full_hd = select_mode(&modes, 1920, 1080, 30).expect("mjpeg can provide 1080p30");
        assert_eq!(full_hd.vcodec.as_deref(), Some("mjpeg"));

        let low = select_mode(&modes, 640, 480, 30).expect("yuyv422 can provide VGA30");
        assert_eq!(low.pixel_format.as_deref(), Some("yuyv422"));

        // 尺寸命中但设备帧率不够：保留该尺寸，帧率压到上限，
        // 输出端 cfr 会补帧，不因为差 1 fps 就整场采集失败。
        let capped = select_mode(&modes, 1280, 720, 61).expect("same size, capped fps");
        assert_eq!((capped.width, capped.height), (1280, 720));
        assert!(capped.fps < 61.0);
        assert_eq!(capped.vcodec.as_deref(), Some("mjpeg"));

        let capped_vga = select_mode(&modes, 640, 480, 31).expect("same size, capped fps");
        assert_eq!((capped_vga.width, capped_vga.height), (640, 480));
        assert!((capped_vga.fps - 30.0).abs() < f64::EPSILON);

        // 设备没有该尺寸(640x360)：退到同宽高比、且不超过请求面积的最近模式。
        let substituted = select_mode(&modes, 640, 360, 30).expect("nearest 16:9 mode");
        assert_eq!((substituted.width, substituted.height), (1280, 720));

        assert!(select_mode(&[], 1280, 720, 30).is_none());
    }

    #[test]
    fn describe_modes_summarizes_largest_sizes_first() {
        let modes = parse_mode_listing(MODE_LISTING);
        let text = describe_modes(&modes);

        assert!(text.starts_with("1920x1080@30(mjpeg)"), "{text}");
        assert!(text.contains("1280x720@60.0002(mjpeg)"), "{text}");
        assert!(text.contains("640x480@30(yuyv422)"), "{text}");
    }

    #[test]
    fn directshow_camera_index_resolves_category_local_device() {
        let devices = parse_device_listing(DEVICE_LISTING);

        assert_eq!(camera(&devices, 1).unwrap().name, "Virtual Camera");
        assert!(camera(&devices, 9).is_err());
    }
}
