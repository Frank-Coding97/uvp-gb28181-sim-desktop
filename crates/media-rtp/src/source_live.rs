use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use common::{Error, Result};

use super::{drain_frames, ffmpeg_bin, Frame, FrameSender, VideoSource, G711_PACKET_BYTES};
use crate::preview::{CapturedAccessUnit, PreviewSink};
use crate::preview_worker::{
    spawn_preview_worker, PreviewControl, PreviewWorkerHandle, PreviewWorkerInput,
};

const DEFAULT_SCREEN_WIDTH: u32 = 1280;
const DEFAULT_SCREEN_HEIGHT: u32 = 720;
const DEFAULT_SCREEN_BITRATE_KBPS: u32 = 2500;
const MIN_SCREEN_DIMENSION: u32 = 160;
const MAX_SCREEN_LONG_SIDE: u32 = 3840;
const MAX_SCREEN_SHORT_SIDE: u32 = 2160;
const MIN_SCREEN_BITRATE_KBPS: u32 = 128;
const MAX_SCREEN_BITRATE_KBPS: u32 = 20_000;
const STDERR_TAIL_LINES: usize = 32;
type StderrCapture = (Arc<Mutex<VecDeque<String>>>, JoinHandle<()>);
static NEXT_CAMERA_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Video codec actually available for native live-screen capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveVideoCodec {
    /// H.264 / AVC Annex-B, encoded by OpenH264.
    H264,
    /// H.265 / HEVC Annex-B, encoded by macOS VideoToolbox through bundled FFmpeg.
    H265,
}

impl LiveVideoCodec {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "h264" => Ok(Self::H264),
            "h265" => Ok(Self::H265),
            _ => Err(Error::Media(
                "screen codec must be exactly 'h264' or 'h265'".into(),
            )),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "h265",
        }
    }

    const fn ps_codec(self) -> crate::ps::VideoCodec {
        match self {
            Self::H264 => crate::ps::VideoCodec::H264,
            Self::H265 => crate::ps::VideoCodec::H265,
        }
    }
}

/// Validated output profile for a native ScreenCaptureKit source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveVideoProfile {
    pub width: u32,
    pub height: u32,
    pub bitrate_kbps: u32,
    pub codec: LiveVideoCodec,
}

impl Default for LiveVideoProfile {
    fn default() -> Self {
        Self {
            width: DEFAULT_SCREEN_WIDTH,
            height: DEFAULT_SCREEN_HEIGHT,
            bitrate_kbps: DEFAULT_SCREEN_BITRATE_KBPS,
            codec: LiveVideoCodec::H264,
        }
    }
}

impl LiveVideoProfile {
    fn validate(self) -> Result<()> {
        validate_screen_dimension(self.width, "screen width")?;
        validate_screen_dimension(self.height, "screen height")?;
        let long_side = self.width.max(self.height);
        let short_side = self.width.min(self.height);
        if long_side > MAX_SCREEN_LONG_SIDE || short_side > MAX_SCREEN_SHORT_SIDE {
            return Err(Error::Media(format!(
                "screen resolution {}x{} exceeds the OpenH264 limit of 3840x2160",
                self.width, self.height
            )));
        }
        if !(MIN_SCREEN_BITRATE_KBPS..=MAX_SCREEN_BITRATE_KBPS).contains(&self.bitrate_kbps) {
            return Err(Error::Media(format!(
                "screen bitrate must be between {MIN_SCREEN_BITRATE_KBPS} and {MAX_SCREEN_BITRATE_KBPS} kbps"
            )));
        }
        Ok(())
    }
}

fn validate_screen_dimension(value: u32, label: &str) -> Result<()> {
    if value % 2 != 0 {
        return Err(Error::Media(format!("{label} must be an even number")));
    }
    if !(MIN_SCREEN_DIMENSION..=MAX_SCREEN_LONG_SIDE).contains(&value) {
        return Err(Error::Media(format!(
            "{label} must be between {MIN_SCREEN_DIMENSION} and {MAX_SCREEN_LONG_SIDE}"
        )));
    }
    Ok(())
}

/// Audio codec for live capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LiveAudioCodec {
    /// G.711 A-law / PCMA, 8 kHz mono, 20 ms.
    #[default]
    G711A,
    /// G.711 μ-law / PCMU, 8 kHz mono, 20 ms.
    G711U,
    /// AAC-LC in ADTS frames, 48 kHz mono, 1024 samples per frame.
    Aac,
    /// Opus in 20 ms packets, 48 kHz mono. PS carriage is a private extension.
    Opus,
}

impl LiveAudioCodec {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "g711a" => Ok(Self::G711A),
            "g711u" => Ok(Self::G711U),
            "aac" => Ok(Self::Aac),
            "opus" => Ok(Self::Opus),
            _ => Err(Error::Media(
                "live audio_codec must be 'g711a', 'g711u', 'aac', or 'opus'".into(),
            )),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::G711A => "g711a",
            Self::G711U => "g711u",
            Self::Aac => "aac",
            Self::Opus => "opus",
        }
    }

    const fn ffmpeg_encoder(self) -> &'static str {
        match self {
            Self::G711A => "pcm_alaw",
            Self::G711U => "pcm_mulaw",
            Self::Aac => "aac",
            Self::Opus => "libopus",
        }
    }

    const fn ffmpeg_format(self) -> &'static str {
        match self {
            Self::G711A => "alaw",
            Self::G711U => "mulaw",
            Self::Aac => "adts",
            Self::Opus => "opus",
        }
    }

    const fn sample_rate(self) -> i32 {
        match self {
            Self::G711A | Self::G711U => 8_000,
            Self::Aac | Self::Opus => 48_000,
        }
    }

    const fn frame_duration_90k(self) -> u32 {
        match self {
            Self::Aac => 1_920,
            Self::G711A | Self::G711U | Self::Opus => 1_800,
        }
    }

    fn append_ffmpeg_output(self, command: &mut Command, output: &str) {
        command.args(["-c:a", self.ffmpeg_encoder()]);
        match self {
            Self::Aac => {
                command.args(["-profile:a", "aac_low", "-b:a", "64k"]);
            }
            Self::Opus => {
                command.args([
                    "-application",
                    "lowdelay",
                    "-frame_duration",
                    "20",
                    "-b:a",
                    "48k",
                    "-vbr",
                    "on",
                ]);
            }
            Self::G711A | Self::G711U => {}
        }
        let sample_rate = self.sample_rate().to_string();
        command.args([
            "-ar",
            &sample_rate,
            "-ac",
            "1",
            "-f",
            self.ffmpeg_format(),
            output,
        ]);
    }

    const fn ps_codec(self) -> crate::ps::AudioCodec {
        match self {
            Self::G711A => crate::ps::AudioCodec::G711A,
            Self::G711U => crate::ps::AudioCodec::G711U,
            Self::Aac => crate::ps::AudioCodec::Aac,
            Self::Opus => crate::ps::AudioCodec::Opus,
        }
    }
}

/// Audio source available for native screen capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveAudioSource {
    /// Do not capture audio.
    None,
    /// Capture system playback through ScreenCaptureKit.
    System,
    /// Capture an AVFoundation microphone by index.
    Microphone(u32),
}

impl LiveAudioSource {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "none" => Ok(Self::None),
            "system" => Ok(Self::System),
            _ => value
                .strip_prefix("microphone:")
                .ok_or_else(|| {
                    Error::Media(
                        "screen audio must be 'none', 'system', or 'microphone:<index>'".into(),
                    )
                })
                .and_then(|index| {
                    parse_decimal(index, "AVFoundation microphone index").map(Self::Microphone)
                }),
        }
    }

    fn as_uri_value(self) -> String {
        match self {
            Self::None => "none".into(),
            Self::System => "system".into(),
            Self::Microphone(index) => format!("microphone:{index}"),
        }
    }

    pub const fn is_enabled(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// A fully parsed live-source URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveSourceSpec {
    /// AVFoundation camera and optional microphone indexes.
    Camera {
        video_index: u32,
        audio_index: Option<u32>,
        audio_codec: LiveAudioCodec,
    },
    /// ScreenCaptureKit display ID, optional system/microphone audio, and output profile.
    Screen {
        display_id: u32,
        audio: LiveAudioSource,
        audio_codec: LiveAudioCodec,
        profile: LiveVideoProfile,
    },
}

impl LiveSourceSpec {
    /// Parse a canonical live URI. The legacy short screen form remains compatible and uses defaults.
    pub fn parse(uri: &str) -> Result<Self> {
        if matches!(uri, "live:0" | "live:ffmpeg:0") {
            tracing::warn!(
                uri,
                "legacy live URI is deprecated; mapping it to the primary screen"
            );
            return Ok(Self::Screen {
                display_id: 0,
                audio: LiveAudioSource::None,
                audio_codec: LiveAudioCodec::default(),
                profile: LiveVideoProfile::default(),
            });
        }

        if let Some(rest) = uri.strip_prefix("live:camera:") {
            let (index, audio, audio_codec) = parse_camera_query(rest)?;
            let video_index = parse_decimal(index, "AVFoundation video index")?;
            let audio_index = if audio == "none" {
                None
            } else {
                Some(parse_decimal(audio, "AVFoundation audio index")?)
            };
            return Ok(Self::Camera {
                video_index,
                audio_index,
                audio_codec,
            });
        }

        if let Some(rest) = uri.strip_prefix("live:screen:") {
            let (id, audio, audio_codec, profile) = parse_screen_query(rest)?;
            let display_id = parse_decimal(id, "ScreenCaptureKit display_id")?;
            if display_id == 0 {
                return Err(Error::Media(
                    "canonical screen URI requires a non-zero ScreenCaptureKit display_id".into(),
                ));
            }
            return Ok(Self::Screen {
                display_id,
                audio,
                audio_codec,
                profile,
            });
        }

        Err(Error::Media(format!(
            "invalid live URI '{uri}'; expected live:camera:<video_index>?audio=<audio_index|none>&audio_codec=<g711a|g711u|aac|opus> or live:screen:<display_id>?audio=<system|microphone:index|none>&audio_codec=<g711a|g711u|aac|opus>&width=<even>&height=<even>&bitrate=<kbps>&codec=<h264|h265>"
        )))
    }

    /// Render the stable URI form used for persistence and IPC.
    pub fn canonical_uri(&self) -> String {
        match self {
            Self::Camera {
                video_index,
                audio_index,
                audio_codec,
            } => format!(
                "live:camera:{video_index}?audio={}&audio_codec={}",
                audio_index.map_or_else(|| "none".into(), |index| index.to_string()),
                audio_codec.as_str(),
            ),
            Self::Screen {
                display_id,
                audio,
                audio_codec,
                profile,
            } => format!(
                "live:screen:{display_id}?audio={}&audio_codec={}&width={}&height={}&bitrate={}&codec={}",
                audio.as_uri_value(),
                audio_codec.as_str(),
                profile.width,
                profile.height,
                profile.bitrate_kbps,
                profile.codec.as_str(),
            ),
        }
    }
}

impl std::str::FromStr for LiveSourceSpec {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

fn parse_camera_query(rest: &str) -> Result<(&str, &str, LiveAudioCodec)> {
    let (id, query) = rest
        .split_once('?')
        .ok_or_else(|| Error::Media("camera URI must include '?audio=...'".into()))?;
    if id.is_empty() || query.is_empty() || query.contains('?') {
        return Err(Error::Media(
            "camera URI has an invalid path or query string".into(),
        ));
    }
    let mut audio = None;
    let mut audio_codec = None;
    for item in query.split('&') {
        let (key, value) = item
            .split_once('=')
            .ok_or_else(|| Error::Media("camera URI query must contain key=value pairs".into()))?;
        match key {
            "audio" if audio.is_none() && !value.is_empty() => audio = Some(value),
            "audio_codec" if audio_codec.is_none() => {
                audio_codec = Some(LiveAudioCodec::parse(value)?)
            }
            "audio" | "audio_codec" => {
                return Err(Error::Media(format!(
                    "camera URI repeats or empties the '{key}' parameter"
                )))
            }
            _ => {
                return Err(Error::Media(format!(
                    "unknown camera URI parameter '{key}'"
                )))
            }
        }
    }
    Ok((
        id,
        audio.ok_or_else(|| Error::Media("camera URI must include audio".into()))?,
        audio_codec.unwrap_or_default(),
    ))
}

fn parse_screen_query(
    rest: &str,
) -> Result<(&str, LiveAudioSource, LiveAudioCodec, LiveVideoProfile)> {
    let (id, query) = rest
        .split_once('?')
        .ok_or_else(|| Error::Media("screen URI must include '?audio=...'".into()))?;
    if id.is_empty() || query.is_empty() || query.contains('?') {
        return Err(Error::Media(
            "screen URI has an invalid path or query string".into(),
        ));
    }

    let mut audio = None;
    let mut audio_codec = None;
    let mut width = None;
    let mut height = None;
    let mut bitrate_kbps = None;
    let mut codec = None;
    for item in query.split('&') {
        let (key, value) = item
            .split_once('=')
            .ok_or_else(|| Error::Media("screen URI query must contain key=value pairs".into()))?;
        if key.is_empty() || value.is_empty() || value.contains('=') {
            return Err(Error::Media(
                "screen URI contains an invalid query parameter".into(),
            ));
        }
        match key {
            "audio" => {
                if audio.is_some() {
                    return Err(Error::Media(
                        "screen URI repeats the 'audio' parameter".into(),
                    ));
                }
                audio = Some(LiveAudioSource::parse(value)?);
            }
            "audio_codec" => {
                if audio_codec.is_some() {
                    return Err(Error::Media(
                        "screen URI repeats the 'audio_codec' parameter".into(),
                    ));
                }
                audio_codec = Some(LiveAudioCodec::parse(value)?);
            }
            "width" => {
                if width.is_some() {
                    return Err(Error::Media(
                        "screen URI repeats the 'width' parameter".into(),
                    ));
                }
                width = Some(parse_decimal(value, "screen width")?);
            }
            "height" => {
                if height.is_some() {
                    return Err(Error::Media(
                        "screen URI repeats the 'height' parameter".into(),
                    ));
                }
                height = Some(parse_decimal(value, "screen height")?);
            }
            "bitrate" => {
                if bitrate_kbps.is_some() {
                    return Err(Error::Media(
                        "screen URI repeats the 'bitrate' parameter".into(),
                    ));
                }
                bitrate_kbps = Some(parse_decimal(value, "screen bitrate")?);
            }
            "codec" => {
                if codec.is_some() {
                    return Err(Error::Media(
                        "screen URI repeats the 'codec' parameter".into(),
                    ));
                }
                codec = Some(LiveVideoCodec::parse(value)?);
            }
            _ => {
                return Err(Error::Media(format!(
                    "unsupported screen URI parameter '{key}'"
                )))
            }
        }
    }

    let audio =
        audio.ok_or_else(|| Error::Media("screen URI must include an 'audio' parameter".into()))?;
    let profile = match (width, height, bitrate_kbps, codec) {
        (None, None, None, None) => LiveVideoProfile::default(),
        (Some(width), Some(height), Some(bitrate_kbps), Some(codec)) => {
            let profile = LiveVideoProfile {
                width,
                height,
                bitrate_kbps,
                codec,
            };
            profile.validate()?;
            profile
        }
        _ => {
            return Err(Error::Media(
                "screen URI must provide width, height, bitrate, and codec together".into(),
            ))
        }
    };

    Ok((id, audio, audio_codec.unwrap_or_default(), profile))
}

fn parse_decimal(value: &str, label: &str) -> Result<u32> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::Media(format!(
            "{label} must be an unsigned decimal integer"
        )));
    }
    value
        .parse()
        .map_err(|_| Error::Media(format!("{label} is out of range")))
}

/// Screen device exposed by the live-source catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveScreenDevice {
    pub display_id: u32,
    pub width: u32,
    pub height: u32,
    pub name: String,
    pub uri: String,
}

/// AVFoundation camera or microphone device exposed by the live-source catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAvDevice {
    pub index: u32,
    pub name: String,
}

/// Discoverable live inputs. Permission or backend failures are reported per source family so
/// one unavailable backend does not hide otherwise usable devices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSourceCatalog {
    pub ffmpeg_available: bool,
    pub aac_available: bool,
    pub opus_available: bool,
    pub screens: Vec<LiveScreenDevice>,
    pub cameras: Vec<LiveAvDevice>,
    pub microphones: Vec<LiveAvDevice>,
    pub screen_error: Option<String>,
    pub avfoundation_error: Option<String>,
}

/// Enumerate canonical live sources for the current platform.
pub fn list_live_sources() -> Result<LiveSourceCatalog> {
    let ffmpeg = ffmpeg_bin();
    let (aac_available, opus_available) = ffmpeg
        .as_deref()
        .map(detect_audio_encoders)
        .unwrap_or((false, false));
    #[cfg(target_os = "macos")]
    {
        let (screens, screen_error) =
            match screencapturekit::shareable_content::SCShareableContent::get() {
                Ok(content) => (
                    content
                        .displays()
                        .into_iter()
                        .map(|display| {
                            let display_id = display.display_id();
                            let width = display.width();
                            let height = display.height();
                            LiveScreenDevice {
                                display_id,
                                width,
                                height,
                                name: format!("Display {display_id} ({width}x{height})"),
                                uri: LiveSourceSpec::Screen {
                                    display_id,
                                    audio: LiveAudioSource::None,
                                    audio_codec: LiveAudioCodec::default(),
                                    profile: LiveVideoProfile::default(),
                                }
                                .canonical_uri(),
                            }
                        })
                        .collect(),
                    None,
                ),
                Err(error) => (
                    Vec::new(),
                    Some(format!(
                        "ScreenCaptureKit display enumeration failed (check Screen Recording permission): {error}"
                    )),
                ),
            };
        let (cameras, microphones, avfoundation_error) = match ffmpeg.as_deref() {
            Some(binary) => match enumerate_avfoundation(binary) {
                Ok((cameras, microphones)) => (cameras, microphones, None),
                Err(error) => (Vec::new(), Vec::new(), Some(error.to_string())),
            },
            None => (Vec::new(), Vec::new(), None),
        };
        Ok(LiveSourceCatalog {
            ffmpeg_available: ffmpeg.is_some(),
            aac_available,
            opus_available,
            screens,
            cameras,
            microphones,
            screen_error,
            avfoundation_error,
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(LiveSourceCatalog {
            ffmpeg_available: ffmpeg.is_some(),
            aac_available,
            opus_available,
            screens: Vec::new(),
            cameras: Vec::new(),
            microphones: Vec::new(),
            screen_error: Some("screen capture is only available on macOS".into()),
            avfoundation_error: Some("AVFoundation capture is only available on macOS".into()),
        })
    }
}

fn detect_audio_encoders(ffmpeg: &str) -> (bool, bool) {
    let Ok(output) = Command::new(ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .output()
    else {
        return (false, false);
    };
    if !output.status.success() {
        return (false, false);
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    let has_encoder = |name: &str| {
        listing.lines().any(|line| {
            let mut fields = line.split_whitespace();
            let _flags = fields.next();
            fields.next() == Some(name)
        })
    };
    (has_encoder("aac"), has_encoder("libopus"))
}

fn ensure_audio_encoder(ffmpeg: &str, codec: LiveAudioCodec) -> Result<()> {
    let (aac, opus) = detect_audio_encoders(ffmpeg);
    let available = match codec {
        LiveAudioCodec::Aac => aac,
        LiveAudioCodec::Opus => opus,
        LiveAudioCodec::G711A | LiveAudioCodec::G711U => true,
    };
    if available {
        Ok(())
    } else {
        Err(Error::Media(format!(
            "FFmpeg does not provide the '{}' audio encoder required by {}",
            codec.ffmpeg_encoder(),
            codec.as_str()
        )))
    }
}

#[cfg(target_os = "macos")]
fn enumerate_avfoundation(ffmpeg: &str) -> Result<(Vec<LiveAvDevice>, Vec<LiveAvDevice>)> {
    let output = Command::new(ffmpeg)
        .args(["-f", "avfoundation", "-list_devices", "true", "-i", ""])
        .output()
        .map_err(|e| Error::Media(format!("failed to enumerate AVFoundation devices: {e}")))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut cameras = Vec::new();
    let mut microphones = Vec::new();
    let mut section = None;
    for raw in stderr.lines() {
        if raw.contains("AVFoundation video devices") {
            section = Some(false);
            continue;
        }
        if raw.contains("AVFoundation audio devices") {
            section = Some(true);
            continue;
        }
        let Some((index, name)) = parse_avfoundation_line(raw) else {
            continue;
        };
        let device = LiveAvDevice { index, name };
        match section {
            Some(false) if !is_virtual_screen_input(&device.name) => cameras.push(device),
            Some(false) => {
                tracing::debug!(name = %device.name, "hiding AVFoundation virtual screen input from camera catalog");
            }
            Some(true) => microphones.push(device),
            None => {}
        }
    }
    if cameras.is_empty() && microphones.is_empty() {
        return Err(Error::Media(format!(
            "ffmpeg returned no parseable AVFoundation devices: {}",
            stderr_tail(&stderr)
        )));
    }
    Ok((cameras, microphones))
}

#[cfg(target_os = "macos")]
fn is_virtual_screen_input(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    normalized.contains("capture screen") || normalized.contains("screen capture")
}

#[cfg(target_os = "macos")]
fn parse_avfoundation_line(line: &str) -> Option<(u32, String)> {
    let marker = line.rfind(" [")?;
    let rest = &line[marker + 2..];
    let end = rest.find(']')?;
    let index = rest[..end].parse().ok()?;
    let name = rest[end + 1..].trim();
    (!name.is_empty()).then(|| (index, name.to_string()))
}

/// Realtime Annex-B video source with optional codec-aware audio access units.
pub struct LiveSource {
    rx: Arc<LatestFrameQueue>,
    buf: VecDeque<Frame>,
    audio_rx: Option<mpsc::Receiver<Vec<u8>>>,
    audio_buf: VecDeque<Vec<u8>>,
    audio_enabled: bool,
    audio_credit_90k: u32,
    audio_codec: LiveAudioCodec,
    fps: u32,
    codec: LiveVideoCodec,
    children: Vec<Child>,
    worker_threads: Vec<JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
    producer_alive: Arc<AtomicBool>,
    terminal_error: Arc<Mutex<Option<String>>>,
    preview_worker: Option<PreviewWorkerHandle>,
    preview_control: Option<Arc<PreviewControl>>,
    #[cfg(target_os = "macos")]
    screen_stream: Option<screencapturekit::stream::SCStream>,
    last_key: Option<Frame>,
    consecutive_empty: u32,
    needs_keyframe: bool,
}

/// 实时采集队列：宁可丢掉旧帧，也不让采集线程把历史画面排队播放。
/// `dropped` 让消费端知道需要等下一个关键帧重新同步解码器。
struct LatestFrameQueue {
    frames: Mutex<VecDeque<Frame>>,
    capacity: usize,
    dropped: AtomicBool,
    /// 累计入队帧数，仅用于诊断：能区分"没读到字节"和"读到了但切不出帧"。
    pushed: std::sync::atomic::AtomicU64,
}

impl LatestFrameQueue {
    fn new(capacity: usize) -> Self {
        Self {
            frames: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity: capacity.max(1),
            dropped: AtomicBool::new(false),
            pushed: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn push(&self, frame: Frame) {
        self.pushed.fetch_add(1, Ordering::Relaxed);
        let Ok(mut frames) = self.frames.lock() else {
            return;
        };
        if frames.len() >= self.capacity {
            frames.clear();
            self.dropped.store(true, Ordering::Release);
        }
        frames.push_back(frame);
    }

    fn pop(&self) -> Option<Frame> {
        self.frames
            .lock()
            .ok()
            .and_then(|mut frames| frames.pop_front())
    }

    fn pushed_total(&self) -> u64 {
        self.pushed.load(Ordering::Relaxed)
    }

    fn take_dropped(&self) -> bool {
        self.dropped.swap(false, Ordering::AcqRel)
    }
}

impl FrameSender for LatestFrameQueue {
    fn send_frame(&self, frame: Frame) {
        self.push(frame);
    }
}

/// 单一 H.264 AU 出口：主路先提交，预览只做有界 try_send。
struct EncodedFrameHub {
    primary: Arc<LatestFrameQueue>,
    preview: Option<PreviewWorkerInput>,
    generation: u64,
    sequence: std::sync::atomic::AtomicU64,
}

impl EncodedFrameHub {
    fn new(
        primary: Arc<LatestFrameQueue>,
        preview: Option<PreviewWorkerInput>,
        generation: u64,
    ) -> Self {
        Self {
            primary,
            preview,
            generation,
            sequence: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

impl FrameSender for EncodedFrameHub {
    fn send_frame(&self, frame: Frame) {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let captured_at_ms = crate::source::now_ms();
        let preview_frame = self.preview.as_ref().map(|_| {
            CapturedAccessUnit::new(
                self.generation,
                sequence,
                frame.data.clone(),
                frame.key_frame,
                captured_at_ms,
            )
        });

        // 这条顺序是硬合同：任何预览错误都发生在主路已经拿到 AU 之后。
        self.primary.push(frame);
        if let (Some(input), Some(access_unit)) = (&self.preview, preview_frame) {
            let _ = input.try_send(access_unit);
        }
    }
}

impl LiveSource {
    /// Parse a complete live URI and start its platform backend.
    pub fn capture(uri: &str, fps: u32, preview: Option<Arc<dyn PreviewSink>>) -> Result<Self> {
        let spec = LiveSourceSpec::parse(uri)?;
        let fps = fps.max(1);
        #[cfg(target_os = "macos")]
        {
            match spec {
                LiveSourceSpec::Camera {
                    video_index,
                    audio_index,
                    audio_codec,
                } => Self::capture_camera(video_index, audio_index, audio_codec, fps, preview),
                LiveSourceSpec::Screen {
                    display_id,
                    audio,
                    audio_codec,
                    profile,
                } => Self::capture_screen(display_id, audio, audio_codec, profile, fps),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = fps;
            let _ = preview;
            match spec {
                LiveSourceSpec::Camera { .. } => Err(Error::Media(
                    "canonical camera capture is not supported on this platform".into(),
                )),
                LiveSourceSpec::Screen { .. } => Err(Error::Media(
                    "canonical screen capture is not supported on this platform".into(),
                )),
            }
        }
    }

    /// 启动唯一 AVFoundation 采集；预览只消费它产出的 H.264 AU 副本。
    #[cfg(target_os = "macos")]
    fn capture_camera(
        video_index: u32,
        audio_index: Option<u32>,
        audio_codec: LiveAudioCodec,
        fps: u32,
        preview: Option<Arc<dyn PreviewSink>>,
    ) -> Result<Self> {
        Self::capture_camera_inner(video_index, audio_index, audio_codec, fps, preview)
    }

    #[cfg(target_os = "macos")]
    fn capture_camera_inner(
        video_index: u32,
        audio_index: Option<u32>,
        audio_codec: LiveAudioCodec,
        fps: u32,
        preview: Option<Arc<dyn PreviewSink>>,
    ) -> Result<Self> {
        let ffmpeg = ffmpeg_bin().ok_or_else(|| {
            Error::Media("macOS camera capture requires ffmpeg with AVFoundation support".into())
        })?;
        if audio_index.is_some() {
            ensure_audio_encoder(&ffmpeg, audio_codec)?;
        }
        let input_fps = camera_input_fps(fps);
        let video_label = format!("camera video index {video_index} at {input_fps} fps");
        let mut command = Command::new(&ffmpeg);
        let camera_args = camera_command_args_for_test(video_index, audio_index, audio_codec, fps);
        command.args(&camera_args);
        if audio_index.is_some() {
            // One AVFoundation session owns both Continuity Camera tracks. A second
            // process can make an iPhone microphone stall while video is active.
            command.args(["-map", "0:a:0"]);
            audio_codec.append_ffmpeg_output(&mut command, "pipe:2");
        }
        let video = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Media(format!("failed to start {video_label}: {e}")))?;

        let mut startup = StartupChildren::new(video);
        let video_tail = if audio_index.is_some() {
            Arc::new(Mutex::new(VecDeque::new()))
        } else {
            let (tail, join) = capture_stderr_tail(&mut startup.children[0], &video_label)?;
            startup.track(join);
            tail
        };
        let video_ready = Arc::new(AtomicBool::new(false));
        let producer_alive = Arc::new(AtomicBool::new(true));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let terminal_error = Arc::new(Mutex::new(None));
        let generation = NEXT_CAMERA_GENERATION
            .fetch_add(1, Ordering::Relaxed)
            .max(1);
        let (preview_input, preview_worker) = preview.map_or((None, None), |sink| {
            let (input, worker) = spawn_preview_worker(ffmpeg.clone(), fps, generation, sink);
            (Some(input), Some(worker))
        });
        let preview_control = preview_worker.as_ref().map(PreviewWorkerHandle::control);
        let tx = Arc::new(LatestFrameQueue::new(2));
        let rx = Arc::clone(&tx);
        let hub = EncodedFrameHub::new(Arc::clone(&tx), preview_input, generation);
        let mut stdout = startup.children[0]
            .stdout
            .take()
            .ok_or_else(|| Error::Media(format!("failed to open {video_label} stdout")))?;
        let ready = Arc::clone(&video_ready);
        let alive = Arc::clone(&producer_alive);
        let video_stop = Arc::clone(&stop_flag);
        let video_error = Arc::clone(&terminal_error);
        let video_reader = std::thread::spawn(move || {
            let mut pending = Vec::with_capacity(256 * 1024);
            let mut chunk = [0_u8; 32 * 1024];
            let mut total_bytes = 0_u64;
            let mut total_frames = 0_u64;
            tracing::info!(generation, "camera H.264 stdout reader started");
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        ready.store(true, Ordering::Release);
                        total_bytes += n as u64;
                        pending.extend_from_slice(&chunk[..n]);
                        drain_frames(&mut pending, &hub);
                        let drained = hub.primary.pushed_total();
                        if drained != total_frames && (drained <= 1 || drained % 150 == 0) {
                            tracing::info!(
                                total_bytes,
                                total_frames = drained,
                                pending_bytes = pending.len(),
                                "camera H.264 frames drained"
                            );
                        }
                        total_frames = drained;
                    }
                }
            }
            tracing::warn!(
                generation,
                total_bytes,
                total_frames,
                "camera H.264 reader stopped"
            );
            if !video_stop.load(Ordering::Acquire) {
                if let Ok(mut error) = video_error.lock() {
                    *error = Some(format!("AVFoundation {video_label} stopped unexpectedly"));
                }
            }
            alive.store(false, Ordering::Release);
        });
        startup.track(video_reader);

        let mut audio_rx = None;
        let mut audio_ready = None;
        let mut audio_tail = None;
        if let Some(index) = audio_index {
            let label = format!(
                "camera audio index {index} ({}) in shared AVFoundation session",
                audio_codec.as_str()
            );
            let ready = Arc::new(AtomicBool::new(false));
            let (audio_tx, receiver) = mpsc::sync_channel(50);
            let mut audio_pipe = startup.children[0]
                .stderr
                .take()
                .ok_or_else(|| Error::Media(format!("failed to open {label} output")))?;
            let ready_thread = Arc::clone(&ready);
            let audio_stop = Arc::clone(&stop_flag);
            let audio_error = Arc::clone(&terminal_error);
            let audio_reader = std::thread::spawn(move || {
                let mut parser = EncodedAudioParser::new(audio_codec);
                let mut chunk = [0_u8; 4096];
                loop {
                    match audio_pipe.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            for packet in parser.push(&chunk[..n]) {
                                ready_thread.store(true, Ordering::Release);
                                match audio_tx.try_send(packet) {
                                    Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                                    Err(mpsc::TrySendError::Disconnected(_)) => return,
                                }
                            }
                        }
                    }
                }
                if !audio_stop.load(Ordering::Acquire) {
                    if let Ok(mut error) = audio_error.lock() {
                        *error = Some(format!("AVFoundation {label} stopped unexpectedly"));
                    }
                }
            });
            startup.track(audio_reader);
            audio_rx = Some(receiver);
            audio_ready = Some(ready);
            audio_tail = Some(Arc::new(Mutex::new(VecDeque::new())));
        }

        wait_for_ffmpeg_startup(
            &mut startup.children,
            &video_ready,
            audio_ready.as_deref(),
            &video_tail,
            audio_tail.as_ref(),
            CameraStartup {
                video_index,
                audio_index,
                input_fps,
            },
        )?;
        let (children, worker_threads) = startup.disarm();

        Ok(Self {
            rx,
            buf: VecDeque::new(),
            audio_rx,
            audio_buf: VecDeque::new(),
            audio_enabled: audio_index.is_some(),
            audio_credit_90k: 0,
            audio_codec,
            fps,
            codec: LiveVideoCodec::H264,
            children,
            worker_threads,
            stop_flag,
            producer_alive,
            terminal_error,
            preview_worker,
            preview_control,
            screen_stream: None,
            last_key: None,
            consecutive_empty: 0,
            needs_keyframe: false,
        })
    }

    #[cfg(target_os = "macos")]
    fn capture_screen(
        display_id: u32,
        audio: LiveAudioSource,
        audio_codec: LiveAudioCodec,
        profile: LiveVideoProfile,
        fps: u32,
    ) -> Result<Self> {
        use openh264::encoder::{Encoder, EncoderConfig, RateControlMode};
        use openh264::formats::{RgbSliceU8, YUVBuffer};
        use openh264::OpenH264API;
        use screencapturekit::cm::{CMSampleBufferExt, SCFrameStatus};
        use screencapturekit::cv::CVPixelBufferLockFlags;
        use screencapturekit::prelude::*;

        profile.validate()?;
        let width = profile.width as usize;
        let height = profile.height as usize;
        let rgb_buffer_len = width * height * 3;
        let bgra_buffer_len = width * height * 4;

        let content = SCShareableContent::get().map_err(|e| {
            Error::Media(format!(
                "ScreenCaptureKit access failed (grant Screen Recording permission): {e}"
            ))
        })?;
        let displays = content.displays();
        if displays.is_empty() {
            return Err(Error::Media("ScreenCaptureKit reported no displays".into()));
        }
        let display = if display_id == 0 {
            displays.first()
        } else {
            displays.iter().find(|d| d.display_id() == display_id)
        }
        .ok_or_else(|| {
            Error::Media(format!(
                "ScreenCaptureKit display_id {display_id} not found"
            ))
        })?;

        let filter = SCContentFilter::create()
            .with_display(display)
            .with_excluding_windows(&[])
            .build();
        let config = SCStreamConfiguration::new()
            .with_width(profile.width)
            .with_height(profile.height)
            .with_pixel_format(PixelFormat::BGRA)
            .with_scales_to_fit(true)
            .with_fps(fps)
            .with_queue_depth(6)
            .with_captures_audio(matches!(audio, LiveAudioSource::System))
            .with_sample_rate(audio_codec.sample_rate())
            .with_channel_count(1);

        let tx = Arc::new(LatestFrameQueue::new(2));
        let rx = Arc::clone(&tx);
        let (raw_tx, raw_rx) = mpsc::sync_channel::<Vec<u8>>(2);
        let video_ready = Arc::new(AtomicBool::new(false));
        let producer_alive = Arc::new(AtomicBool::new(true));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let terminal_error = Arc::new(Mutex::new(None));
        let ready = Arc::clone(&video_ready);
        let encoder_stop = Arc::clone(&stop_flag);
        let encoder_alive = Arc::clone(&producer_alive);
        let mut screen_startup = StartupChildren::empty();
        match profile.codec {
            LiveVideoCodec::H264 => {
                let encoder_config = EncoderConfig::new()
                    .set_bitrate_bps(profile.bitrate_kbps * 1000)
                    .max_frame_rate(fps as f32)
                    .rate_control_mode(RateControlMode::Bitrate)
                    .enable_skip_frame(true);
                let mut encoder = Encoder::with_api_config(
                    OpenH264API::from_source(),
                    encoder_config,
                )
                .map_err(|e| {
                    Error::Media(format!("failed to initialize OpenH264 screen encoder: {e}"))
                })?;
                let encoder_thread = std::thread::spawn(move || {
                    while !encoder_stop.load(Ordering::Acquire) {
                        let bgra = match raw_rx.recv_timeout(Duration::from_millis(100)) {
                            Ok(frame) => frame,
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        };
                        let mut rgb = vec![0_u8; rgb_buffer_len];
                        for (pixel, out) in bgra.chunks_exact(4).zip(rgb.chunks_exact_mut(3)) {
                            out[0] = pixel[2];
                            out[1] = pixel[1];
                            out[2] = pixel[0];
                        }
                        let yuv =
                            YUVBuffer::from_rgb8_source(RgbSliceU8::new(&rgb, (width, height)));
                        let Ok(bitstream) = encoder.encode(&yuv) else {
                            continue;
                        };
                        let data = bitstream.to_vec();
                        if !data.is_empty() {
                            ready.store(true, Ordering::Release);
                            tx.send_frame(Frame {
                                key_frame: h264_is_keyframe(&data),
                                data,
                            });
                        }
                    }
                    encoder_alive.store(false, Ordering::Release);
                });
                screen_startup.track(encoder_thread);
            }
            LiveVideoCodec::H265 => {
                let ffmpeg = ffmpeg_bin().ok_or_else(|| {
                    Error::Media(
                        "H.265 screen encoding requires the bundled FFmpeg executable".into(),
                    )
                })?;
                let size = format!("{}x{}", profile.width, profile.height);
                let fps_text = fps.to_string();
                let bitrate = format!("{}k", profile.bitrate_kbps);
                let mut child = Command::new(&ffmpeg)
                    .args([
                        "-hide_banner",
                        "-nostdin",
                        "-f",
                        "rawvideo",
                        "-pixel_format",
                        "bgra",
                        "-video_size",
                        &size,
                        "-framerate",
                        &fps_text,
                        "-i",
                        "pipe:0",
                        "-an",
                        "-c:v",
                        "hevc_videotoolbox",
                        "-allow_sw",
                        "1",
                        "-realtime",
                        "1",
                        "-b:v",
                        &bitrate,
                        "-g",
                        &fps_text,
                        "-f",
                        "hevc",
                        "pipe:1",
                    ])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|e| Error::Media(format!("failed to start H.265 encoder: {e}")))?;
                let mut stdin = child
                    .stdin
                    .take()
                    .ok_or_else(|| Error::Media("failed to open H.265 encoder stdin".into()))?;
                let mut stdout = child
                    .stdout
                    .take()
                    .ok_or_else(|| Error::Media("failed to open H.265 encoder stdout".into()))?;
                let (stderr_tail, stderr_thread) =
                    capture_stderr_tail(&mut child, "H.265 screen encoder")?;
                screen_startup.track(stderr_thread);
                let writer_tail = Arc::clone(&stderr_tail);
                let reader_tail = Arc::clone(&stderr_tail);
                let writer_stop = Arc::clone(&stop_flag);
                let writer_error = Arc::clone(&terminal_error);
                let writer_thread = std::thread::spawn(move || {
                    while !writer_stop.load(Ordering::Acquire) {
                        let frame = match raw_rx.recv_timeout(Duration::from_millis(100)) {
                            Ok(frame) => frame,
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        };
                        if let Err(error) = std::io::Write::write_all(&mut stdin, &frame) {
                            if let Ok(mut terminal) = writer_error.lock() {
                                *terminal = Some(format!(
                                    "H.265 screen encoder input failed: {error}; stderr: {}",
                                    locked_tail(&writer_tail)
                                ));
                            }
                            break;
                        }
                    }
                });
                screen_startup.track(writer_thread);
                let reader_stop = Arc::clone(&stop_flag);
                let reader_error = Arc::clone(&terminal_error);
                let reader_thread = std::thread::spawn(move || {
                    let mut pending = Vec::with_capacity(512 * 1024);
                    let mut chunk = [0_u8; 64 * 1024];
                    loop {
                        match stdout.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                pending.extend_from_slice(&chunk[..n]);
                                drain_hevc_frames(&mut pending, tx.as_ref());
                                ready.store(true, Ordering::Release);
                            }
                        }
                    }
                    if !reader_stop.load(Ordering::Acquire) {
                        if let Ok(mut terminal) = reader_error.lock() {
                            *terminal = Some(format!(
                                "H.265 screen encoder stopped unexpectedly; stderr: {}",
                                locked_tail(&reader_tail)
                            ));
                        }
                    }
                    encoder_alive.store(false, Ordering::Release);
                });
                screen_startup.track(reader_thread);
                screen_startup.children.push(child);
            }
        }

        let delegate_alive = Arc::clone(&producer_alive);
        let delegate_error = Arc::clone(&terminal_error);
        let delegate = ErrorHandler::new(move |error| {
            let message = format!("ScreenCaptureKit stream stopped: {error}");
            tracing::warn!(error = %error, "ScreenCaptureKit stream stopped");
            if let Ok(mut terminal) = delegate_error.lock() {
                *terminal = Some(message);
            }
            delegate_alive.store(false, Ordering::Release);
        });
        let mut stream = SCStream::new_with_delegate(&filter, &config, delegate);
        if stream
            .add_output_handler(
                move |sample: CMSampleBuffer, _| {
                    if sample
                        .frame_status()
                        .is_some_and(|s| s != SCFrameStatus::Complete)
                    {
                        return;
                    }
                    let Some(pixel_buffer) = sample.image_buffer() else {
                        return;
                    };
                    let Ok(guard) = pixel_buffer.lock(CVPixelBufferLockFlags::READ_ONLY) else {
                        return;
                    };
                    if guard.width() != width || guard.height() != height {
                        return;
                    }
                    let source = guard.as_slice();
                    let stride = guard.bytes_per_row();
                    let row_bytes = width * 4;
                    if stride < row_bytes || source.len() < stride.saturating_mul(height) {
                        return;
                    }
                    let mut packed = Vec::with_capacity(bgra_buffer_len);
                    for y in 0..height {
                        let start = y * stride;
                        packed.extend_from_slice(&source[start..start + row_bytes]);
                    }
                    let _ = raw_tx.try_send(packed);
                },
                SCStreamOutputType::Screen,
            )
            .is_none()
        {
            return Err(Error::Media(
                "ScreenCaptureKit rejected the screen output handler".into(),
            ));
        }

        let mut audio_rx = None;
        let mut microphone_ready = None;
        let mut microphone_tail = None;
        match audio {
            LiveAudioSource::System => {
                let (audio_tx, receiver) = mpsc::sync_channel(50);
                match audio_codec {
                    LiveAudioCodec::G711A | LiveAudioCodec::G711U => {
                        let packetizer =
                            Arc::new(Mutex::new(G711Packetizer::new(audio_tx, audio_codec)));
                        let packetizer_handler = Arc::clone(&packetizer);
                        if stream
                            .add_output_handler(
                                move |sample: CMSampleBuffer, _| {
                                    let Some(list) = sample.audio_buffer_list() else {
                                        return;
                                    };
                                    let Ok(mut packetizer) = packetizer_handler.lock() else {
                                        return;
                                    };
                                    for buffer in &list {
                                        for bytes in buffer.data().chunks_exact(4) {
                                            packetizer.push(f32::from_ne_bytes([
                                                bytes[0], bytes[1], bytes[2], bytes[3],
                                            ]));
                                        }
                                    }
                                },
                                SCStreamOutputType::Audio,
                            )
                            .is_none()
                        {
                            return Err(Error::Media(
                                "ScreenCaptureKit rejected the system-audio output handler".into(),
                            ));
                        }
                    }
                    LiveAudioCodec::Aac | LiveAudioCodec::Opus => {
                        let ffmpeg = ffmpeg_bin().ok_or_else(|| {
                            Error::Media(
                                "AAC/Opus system-audio encoding requires bundled FFmpeg".into(),
                            )
                        })?;
                        ensure_audio_encoder(&ffmpeg, audio_codec)?;
                        let label = format!("system audio {} encoder", audio_codec.as_str());
                        let sample_rate = audio_codec.sample_rate().to_string();
                        let mut command = Command::new(&ffmpeg);
                        command.args([
                            "-hide_banner",
                            "-nostdin",
                            "-loglevel",
                            "warning",
                            "-f",
                            "f32le",
                            "-ar",
                            &sample_rate,
                            "-ac",
                            "1",
                            "-i",
                            "pipe:0",
                            "-vn",
                        ]);
                        audio_codec.append_ffmpeg_output(&mut command, "pipe:1");
                        let mut encoder = command
                            .stdin(Stdio::piped())
                            .stdout(Stdio::piped())
                            .stderr(Stdio::piped())
                            .spawn()
                            .map_err(|error| {
                                Error::Media(format!("failed to start {label}: {error}"))
                            })?;
                        let stdin = encoder
                            .stdin
                            .take()
                            .ok_or_else(|| Error::Media(format!("failed to open {label} stdin")))?;
                        let mut stdout = encoder.stdout.take().ok_or_else(|| {
                            Error::Media(format!("failed to open {label} stdout"))
                        })?;
                        let (tail, stderr_thread) = capture_stderr_tail(&mut encoder, &label)?;
                        screen_startup.track(stderr_thread);
                        let input = Arc::new(Mutex::new(stdin));
                        let callback_input = Arc::clone(&input);
                        if stream
                            .add_output_handler(
                                move |sample: CMSampleBuffer, _| {
                                    let Some(list) = sample.audio_buffer_list() else {
                                        return;
                                    };
                                    let Ok(mut input) = callback_input.lock() else {
                                        return;
                                    };
                                    for buffer in &list {
                                        if input.write_all(buffer.data()).is_err() {
                                            break;
                                        }
                                    }
                                },
                                SCStreamOutputType::Audio,
                            )
                            .is_none()
                        {
                            let _ = encoder.kill();
                            return Err(Error::Media(
                                "ScreenCaptureKit rejected the system-audio output handler".into(),
                            ));
                        }
                        let ready = Arc::new(AtomicBool::new(false));
                        let ready_thread = Arc::clone(&ready);
                        let audio_stop = Arc::clone(&stop_flag);
                        let audio_error = Arc::clone(&terminal_error);
                        let reader_tail = Arc::clone(&tail);
                        let reader_thread = std::thread::spawn(move || {
                            let mut parser = EncodedAudioParser::new(audio_codec);
                            let mut chunk = [0_u8; 4096];
                            loop {
                                match stdout.read(&mut chunk) {
                                    Ok(0) | Err(_) => break,
                                    Ok(n) => {
                                        for packet in parser.push(&chunk[..n]) {
                                            ready_thread.store(true, Ordering::Release);
                                            match audio_tx.try_send(packet) {
                                                Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                                                Err(mpsc::TrySendError::Disconnected(_)) => return,
                                            }
                                        }
                                    }
                                }
                            }
                            if !audio_stop.load(Ordering::Acquire) {
                                if let Ok(mut terminal) = audio_error.lock() {
                                    *terminal = Some(format!(
                                        "{label} stopped unexpectedly; stderr: {}",
                                        locked_tail(&reader_tail)
                                    ));
                                }
                            }
                        });
                        screen_startup.track(reader_thread);
                        screen_startup.children.push(encoder);
                        microphone_ready = Some(ready);
                        microphone_tail = Some(tail);
                    }
                }
                audio_rx = Some(receiver);
            }
            LiveAudioSource::Microphone(index) => {
                let ffmpeg = ffmpeg_bin().ok_or_else(|| {
                    Error::Media("screen microphone capture requires bundled FFmpeg".into())
                })?;
                ensure_audio_encoder(&ffmpeg, audio_codec)?;
                let label = format!("screen microphone index {index}");
                let input = format!("none:{index}");
                let mut command = Command::new(&ffmpeg);
                command.args([
                    "-hide_banner",
                    "-nostdin",
                    "-f",
                    "avfoundation",
                    "-i",
                    &input,
                    "-vn",
                ]);
                audio_codec.append_ffmpeg_output(&mut command, "pipe:1");
                let microphone = command
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|error| Error::Media(format!("failed to start {label}: {error}")))?;
                screen_startup.children.push(microphone);
                let child_index = screen_startup.children.len() - 1;
                let (tail, stderr_thread) =
                    capture_stderr_tail(&mut screen_startup.children[child_index], &label)?;
                screen_startup.track(stderr_thread);
                let ready = Arc::new(AtomicBool::new(false));
                let (audio_tx, receiver) = mpsc::sync_channel(50);
                let mut stdout = screen_startup.children[child_index]
                    .stdout
                    .take()
                    .ok_or_else(|| Error::Media(format!("failed to open {label} stdout")))?;
                let ready_thread = Arc::clone(&ready);
                let audio_stop = Arc::clone(&stop_flag);
                let audio_error = Arc::clone(&terminal_error);
                let reader_thread = std::thread::spawn(move || {
                    let mut parser = EncodedAudioParser::new(audio_codec);
                    let mut chunk = [0_u8; 4096];
                    loop {
                        match stdout.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                for packet in parser.push(&chunk[..n]) {
                                    ready_thread.store(true, Ordering::Release);
                                    match audio_tx.try_send(packet) {
                                        Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                                        Err(mpsc::TrySendError::Disconnected(_)) => return,
                                    }
                                }
                            }
                        }
                    }
                    if !audio_stop.load(Ordering::Acquire) {
                        if let Ok(mut terminal) = audio_error.lock() {
                            *terminal = Some(format!("AVFoundation {label} stopped unexpectedly"));
                        }
                    }
                });
                screen_startup.track(reader_thread);
                audio_rx = Some(receiver);
                microphone_ready = Some(ready);
                microphone_tail = Some(tail);
            }
            LiveAudioSource::None => {}
        }

        stream.start_capture().map_err(|e| {
            Error::Media(format!(
                "ScreenCaptureKit failed to start display {} (check permission): {e}",
                display.display_id()
            ))
        })?;
        let deadline = Instant::now()
            + if microphone_ready.is_some() {
                Duration::from_secs(15)
            } else {
                Duration::from_secs(5)
            };
        loop {
            let video_ok = video_ready.load(Ordering::Acquire);
            let microphone_ok = microphone_ready
                .as_deref()
                .map_or(true, |ready| ready.load(Ordering::Acquire));
            if video_ok && microphone_ok {
                break;
            }
            if !producer_alive.load(Ordering::Acquire) {
                let _ = stream.stop_capture();
                return Err(Error::Media(format!(
                    "ScreenCaptureKit display {} stopped before startup completed",
                    display.display_id()
                )));
            }
            for child in &mut screen_startup.children {
                if let Some(status) = child.try_wait().map_err(|error| {
                    Error::Media(format!(
                        "failed while waiting for screen capture helper: {error}"
                    ))
                })? {
                    let _ = stream.stop_capture();
                    return Err(Error::Media(format!(
                        "screen capture helper exited before startup ({status}); microphone stderr: {}",
                        microphone_tail
                            .as_ref()
                            .map_or_else(|| "n/a".into(), locked_tail)
                    )));
                }
            }
            if Instant::now() >= deadline {
                let _ = stream.stop_capture();
                let missing = match (video_ok, microphone_ok) {
                    (false, false) => "video and requested microphone audio",
                    (false, true) => "video",
                    (true, false) => "requested microphone audio",
                    (true, true) => unreachable!(),
                };
                return Err(Error::Media(format!(
                    "screen source produced no {missing} data before startup timeout; display {}; microphone stderr: {}",
                    display.display_id(),
                    microphone_tail
                        .as_ref()
                        .map_or_else(|| "n/a".into(), locked_tail)
                )));
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let (children, worker_threads) = screen_startup.disarm();
        Ok(Self {
            rx,
            buf: VecDeque::new(),
            audio_rx,
            audio_buf: VecDeque::new(),
            audio_enabled: audio.is_enabled(),
            audio_credit_90k: 0,
            audio_codec,
            fps,
            codec: profile.codec,
            children,
            worker_threads,
            stop_flag,
            producer_alive,
            terminal_error,
            preview_worker: None,
            preview_control: None,
            screen_stream: Some(stream),
            last_key: None,
            consecutive_empty: 0,
            needs_keyframe: false,
        })
    }
}

#[cfg(target_os = "macos")]
fn camera_input_fps(output_fps: u32) -> u32 {
    match output_fps {
        30 | 60 => output_fps,
        _ => 30,
    }
}

#[cfg(target_os = "macos")]
fn cfr_output_args(output_fps: &str) -> [&str; 4] {
    ["-r", output_fps, "-fps_mode", "cfr"]
}

#[cfg(target_os = "macos")]
fn camera_command_args_for_test(
    video_index: u32,
    audio_index: Option<u32>,
    _audio_codec: LiveAudioCodec,
    fps: u32,
) -> Vec<String> {
    let input_fps = camera_input_fps(fps);
    let input = audio_index.map_or_else(
        || format!("{video_index}:none"),
        |index| format!("{video_index}:{index}"),
    );
    let output_fps = fps.max(1).to_string();
    let cfr_args = cfr_output_args(&output_fps);
    let input_fps = input_fps.to_string();
    let x264_params = format!(
        "slices=1:sliced-threads=0:repeat-headers=1:keyint={}:min-keyint={}:scenecut=0:rc-lookahead=0",
        fps.max(1),
        fps.max(1)
    );
    vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-loglevel".into(),
        if audio_index.is_some() {
            "quiet".into()
        } else {
            "warning".into()
        },
        "-fflags".into(),
        "nobuffer".into(),
        "-flags".into(),
        "low_delay".into(),
        "-avioflags".into(),
        "direct".into(),
        "-probesize".into(),
        "32".into(),
        "-analyzeduration".into(),
        "0".into(),
        "-use_wallclock_as_timestamps".into(),
        "1".into(),
        "-f".into(),
        "avfoundation".into(),
        "-thread_queue_size".into(),
        "8".into(),
        "-pixel_format".into(),
        "uyvy422".into(),
        "-video_size".into(),
        "1280x720".into(),
        "-framerate".into(),
        input_fps,
        "-i".into(),
        input,
        "-map".into(),
        "0:v:0".into(),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "ultrafast".into(),
        "-tune".into(),
        "zerolatency".into(),
        "-x264-params".into(),
        x264_params,
        "-bf".into(),
        "0".into(),
        "-refs".into(),
        "1".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-vf".into(),
        "scale=1280:720:force_original_aspect_ratio=decrease,pad=1280:720:(ow-iw)/2:(oh-ih)/2"
            .into(),
        "-g".into(),
        output_fps.clone(),
        cfr_args[0].into(),
        cfr_args[1].into(),
        cfr_args[2].into(),
        cfr_args[3].into(),
        "-f".into(),
        "h264".into(),
        "-flush_packets".into(),
        "1".into(),
        "pipe:1".into(),
    ]
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy)]
struct CameraStartup {
    video_index: u32,
    audio_index: Option<u32>,
    input_fps: u32,
}

#[cfg(target_os = "macos")]
struct StartupChildren {
    children: Vec<Child>,
    worker_threads: Vec<JoinHandle<()>>,
    armed: bool,
}

#[cfg(target_os = "macos")]
impl StartupChildren {
    fn empty() -> Self {
        Self {
            children: Vec::new(),
            worker_threads: Vec::new(),
            armed: true,
        }
    }

    fn new(child: Child) -> Self {
        Self {
            children: vec![child],
            worker_threads: Vec::new(),
            armed: true,
        }
    }

    fn track(&mut self, thread: JoinHandle<()>) {
        self.worker_threads.push(thread);
    }

    fn disarm(mut self) -> (Vec<Child>, Vec<JoinHandle<()>>) {
        self.armed = false;
        (
            std::mem::take(&mut self.children),
            std::mem::take(&mut self.worker_threads),
        )
    }
}

#[cfg(target_os = "macos")]
impl Drop for StartupChildren {
    fn drop(&mut self) {
        if self.armed {
            let _ = stop_owned_children_and_threads(
                &mut self.children,
                &mut self.worker_threads,
                Duration::from_secs(1),
            );
        }
    }
}

fn capture_stderr_tail(child: &mut Child, label: &str) -> Result<StderrCapture> {
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::Media(format!("failed to open {label} stderr")))?;
    let tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
    let output = Arc::clone(&tail);
    let owned_label = label.to_string();
    let join = std::thread::spawn(move || {
        for line in BufReader::new(stderr)
            .lines()
            .map_while(std::result::Result::ok)
        {
            // 同时打进日志：只留在环形缓冲里的话，采集失败时除了"超时"什么都看不到，
            // FFmpeg 真正说了什么反而丢了。
            tracing::warn!(label = %owned_label, line = %line, "ffmpeg stderr");
            if let Ok(mut lines) = output.lock() {
                if lines.len() == STDERR_TAIL_LINES {
                    lines.pop_front();
                }
                lines.push_back(line);
            }
        }
    });
    Ok((tail, join))
}

#[cfg(target_os = "macos")]
fn wait_for_ffmpeg_startup(
    children: &mut [Child],
    video_ready: &AtomicBool,
    audio_ready: Option<&AtomicBool>,
    video_tail: &Arc<Mutex<VecDeque<String>>>,
    audio_tail: Option<&Arc<Mutex<VecDeque<String>>>>,
    camera: CameraStartup,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let video_ok = video_ready.load(Ordering::Acquire);
        let audio_ok = match audio_ready {
            Some(ready) => ready.load(Ordering::Acquire),
            None => true,
        };
        if video_ok && audio_ok {
            return Ok(());
        }
        for (position, child) in children.iter_mut().enumerate() {
            if let Some(status) = child
                .try_wait()
                .map_err(|e| Error::Media(format!("failed while waiting for ffmpeg: {e}")))?
            {
                kill_children(children);
                let kind = if position == 0 { "video" } else { "audio" };
                return Err(Error::Media(format!(
                    "AVFoundation {kind} capture exited before startup ({status}); video index {} at input {} fps, audio index {:?}; video stderr: {}; audio stderr: {}",
                    camera.video_index,
                    camera.input_fps,
                    camera.audio_index,
                    locked_tail(video_tail),
                    audio_tail.map_or_else(|| "n/a".into(), locked_tail)
                )));
            }
        }
        if Instant::now() >= deadline {
            kill_children(children);
            let missing = match (video_ok, audio_ok) {
                (false, false) => "video and requested audio",
                (false, true) => "video",
                (true, false) => "requested audio",
                (true, true) => unreachable!(),
            };
            return Err(Error::Media(format!(
                "AVFoundation produced no {missing} data within 15 seconds; video index {} at input {} fps, audio index {:?}; video stderr: {}; audio stderr: {}",
                camera.video_index,
                camera.input_fps,
                camera.audio_index,
                locked_tail(video_tail),
                audio_tail.map_or_else(|| "n/a".into(), locked_tail)
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(target_os = "macos")]
fn kill_children(children: &mut [Child]) {
    for child in children {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// 停止本采集会话显式持有的进程，并在同一截止时间内回收其 I/O/编码线程。
///
/// 顺序必须是 child first、reader second：reader 可能正阻塞在 pipe read，只有先关闭
/// 子进程端管道才会得到 EOF。任何超过截止时间的进程交给独立 reaper，调用方不再等待。
fn stop_owned_children_and_threads(
    children: &mut Vec<Child>,
    worker_threads: &mut Vec<JoinHandle<()>>,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    let mut pending_children = std::mem::take(children);
    for child in &mut pending_children {
        let _ = child.kill();
    }

    while !pending_children.is_empty() && Instant::now() < deadline {
        let mut index = 0;
        while index < pending_children.len() {
            match pending_children[index].try_wait() {
                Ok(Some(_)) => {
                    let mut child = pending_children.swap_remove(index);
                    let _ = child.wait();
                }
                Ok(None) => index += 1,
                Err(_) => {
                    let child = pending_children.swap_remove(index);
                    reap_child_in_background(child);
                }
            }
        }
        if !pending_children.is_empty() {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    let mut all_stopped = pending_children.is_empty();
    for child in pending_children.drain(..) {
        reap_child_in_background(child);
    }

    let pending_threads = std::mem::take(worker_threads);
    while pending_threads.iter().any(|thread| !thread.is_finished()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    for thread in pending_threads {
        if thread.is_finished() {
            let _ = thread.join();
        } else {
            all_stopped = false;
            // JoinHandle drop 只 detach；线程持有的 pipe 已随 owned child 终止，通常会立即 EOF。
        }
    }
    all_stopped
}

fn reap_child_in_background(mut child: Child) {
    let _ = std::thread::Builder::new()
        .name("uvp-owned-child-reaper".into())
        .spawn(move || {
            let _ = child.kill();
            let _ = child.wait();
        });
}

fn locked_tail(tail: &Arc<Mutex<VecDeque<String>>>) -> String {
    tail.lock()
        .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join(" | "))
        .unwrap_or_else(|_| "stderr unavailable".into())
}

#[cfg(target_os = "macos")]
fn stderr_tail(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|line| !line.trim().is_empty())
        .rev()
        .take(STDERR_TAIL_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ")
}

#[cfg(target_os = "macos")]
fn drain_hevc_frames<S: FrameSender>(buf: &mut Vec<u8>, tx: &S) {
    let mut starts = Vec::new();
    let mut index = 0;
    while index + 3 <= buf.len() {
        if buf[index] == 0 && buf[index + 1] == 0 && buf[index + 2] == 1 {
            starts.push(index);
            index += 3;
        } else {
            index += 1;
        }
    }
    if starts.len() < 2 {
        return;
    }

    let mut frame_start = starts[0];
    let mut key_frame = false;
    let mut has_vcl = false;
    let mut drained_to = 0;
    for position in starts {
        // The scanner normalizes both three- and four-byte Annex-B prefixes to
        // the final 00 00 01 sequence. HEVC then has a two-byte NAL header.
        let header = position + 3;
        let Some(first_header_byte) = buf.get(header) else {
            break;
        };
        if buf.get(header + 1).is_none() {
            break;
        }

        let nal_type = (first_header_byte >> 1) & 0x3f;
        let is_vcl = nal_type <= 31;
        let first_slice = is_vcl
            && buf
                .get(header + 2)
                .is_some_and(|payload| payload & 0x80 != 0);
        // VPS/SPS/PPS, AUD and prefix SEI belong to the following access unit.
        // A VCL NAL starts a new picture only when first_slice_segment_in_pic_flag
        // is set; additional slices must remain in the current frame.
        let starts_next_access_unit = first_slice || matches!(nal_type, 32..=35 | 39);
        if has_vcl && starts_next_access_unit {
            tx.send_frame(Frame {
                data: buf[frame_start..position].to_vec(),
                key_frame,
            });
            frame_start = position;
            key_frame = false;
            has_vcl = false;
            drained_to = position;
        }

        if (16..=21).contains(&nal_type) || (32..=34).contains(&nal_type) {
            key_frame = true;
        }
        has_vcl |= is_vcl;
    }

    // Keep the current access unit until a following boundary proves it is
    // complete. This also keeps parameter sets attached to the next IRAP frame.
    if drained_to > 0 {
        buf.drain(..drained_to);
    }
}

#[cfg(target_os = "macos")]
fn h264_is_keyframe(data: &[u8]) -> bool {
    data.windows(5).any(|window| {
        (window[..4] == [0, 0, 0, 1] && window[4] & 0x1f == 5)
            || (window[..3] == [0, 0, 1] && window[3] & 0x1f == 5)
    })
}

struct EncodedAudioParser {
    codec: LiveAudioCodec,
    pending: Vec<u8>,
    ogg_packet: Vec<u8>,
}

impl EncodedAudioParser {
    fn new(codec: LiveAudioCodec) -> Self {
        Self {
            codec,
            pending: Vec::with_capacity(16 * 1024),
            ogg_packet: Vec::new(),
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.pending.extend_from_slice(bytes);
        match self.codec {
            LiveAudioCodec::G711A | LiveAudioCodec::G711U => self.drain_g711(),
            LiveAudioCodec::Aac => self.drain_adts(),
            LiveAudioCodec::Opus => self.drain_ogg_opus(),
        }
    }

    fn drain_g711(&mut self) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        while self.pending.len() >= G711_PACKET_BYTES {
            frames.push(self.pending.drain(..G711_PACKET_BYTES).collect());
        }
        frames
    }

    fn drain_adts(&mut self) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        loop {
            let Some(sync) = self
                .pending
                .windows(2)
                .position(|window| window[0] == 0xff && window[1] & 0xf6 == 0xf0)
            else {
                let keep = self.pending.last().copied() == Some(0xff);
                self.pending.clear();
                if keep {
                    self.pending.push(0xff);
                }
                break;
            };
            if sync > 0 {
                self.pending.drain(..sync);
            }
            if self.pending.len() < 7 {
                break;
            }
            let frame_len = (usize::from(self.pending[3] & 0x03) << 11)
                | (usize::from(self.pending[4]) << 3)
                | (usize::from(self.pending[5] & 0xe0) >> 5);
            let header_len = if self.pending[1] & 0x01 == 0 { 9 } else { 7 };
            if frame_len < header_len || frame_len > 16 * 1024 {
                self.pending.drain(..1);
                continue;
            }
            if self.pending.len() < frame_len {
                break;
            }
            frames.push(self.pending.drain(..frame_len).collect());
        }
        frames
    }

    fn drain_ogg_opus(&mut self) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        loop {
            let Some(sync) = self.pending.windows(4).position(|window| window == b"OggS") else {
                let keep = self.pending.len().min(3);
                self.pending.drain(..self.pending.len() - keep);
                break;
            };
            if sync > 0 {
                self.pending.drain(..sync);
            }
            if self.pending.len() < 27 {
                break;
            }
            let segment_count = usize::from(self.pending[26]);
            let header_len = 27 + segment_count;
            if self.pending.len() < header_len {
                break;
            }
            let payload_len: usize = self.pending[27..header_len]
                .iter()
                .map(|&length| usize::from(length))
                .sum();
            let page_len = header_len + payload_len;
            if self.pending.len() < page_len {
                break;
            }
            let page: Vec<u8> = self.pending.drain(..page_len).collect();
            let mut cursor = header_len;
            for &length in &page[27..header_len] {
                let end = cursor + usize::from(length);
                self.ogg_packet.extend_from_slice(&page[cursor..end]);
                cursor = end;
                if length < 255 {
                    let packet = std::mem::take(&mut self.ogg_packet);
                    if !packet.is_empty()
                        && !packet.starts_with(b"OpusHead")
                        && !packet.starts_with(b"OpusTags")
                    {
                        frames.push(packet);
                    }
                }
            }
        }
        frames
    }
}

struct G711Packetizer {
    pending: Vec<u8>,
    tx: mpsc::SyncSender<Vec<u8>>,
    codec: LiveAudioCodec,
}

impl G711Packetizer {
    fn new(tx: mpsc::SyncSender<Vec<u8>>, codec: LiveAudioCodec) -> Self {
        Self {
            pending: Vec::with_capacity(G711_PACKET_BYTES * 2),
            tx,
            codec,
        }
    }

    fn push(&mut self, sample: f32) {
        let pcm = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        let encoded = match self.codec {
            LiveAudioCodec::G711A => linear_to_alaw(pcm),
            LiveAudioCodec::G711U => linear_to_ulaw(pcm),
            LiveAudioCodec::Aac | LiveAudioCodec::Opus => return,
        };
        self.pending.push(encoded);
        if self.pending.len() == G711_PACKET_BYTES {
            let packet =
                std::mem::replace(&mut self.pending, Vec::with_capacity(G711_PACKET_BYTES * 2));
            let _ = self.tx.try_send(packet);
        }
    }
}

fn linear_to_alaw(sample: i16) -> u8 {
    const SEGMENT_END: [i32; 8] = [0x1f, 0x3f, 0x7f, 0xff, 0x1ff, 0x3ff, 0x7ff, 0xfff];
    let mut pcm = i32::from(sample) >> 3;
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

fn linear_to_ulaw(sample: i16) -> u8 {
    const BIAS: i32 = 0x84;
    const CLIP: i32 = 32635;
    let mut pcm = i32::from(sample);
    let mask = if pcm < 0 {
        pcm = (-pcm).min(CLIP);
        0x7f
    } else {
        pcm = pcm.min(CLIP);
        0xff
    };
    pcm += BIAS;
    let exponent = (24 - pcm.leading_zeros() as i32).clamp(0, 7);
    let mantissa = (pcm >> (exponent + 3)) & 0x0f;
    ((exponent << 4 | mantissa) as u8) ^ mask
}

impl Drop for LiveSource {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Release);
        #[cfg(target_os = "macos")]
        if let Some(stream) = self.screen_stream.take() {
            let _ = stream.stop_capture();
        }
        let _ = stop_owned_children_and_threads(
            &mut self.children,
            &mut self.worker_threads,
            Duration::from_secs(1),
        );
        self.producer_alive.store(false, Ordering::Release);
        if let Some(mut worker) = self.preview_worker.take() {
            worker.stop();
        }
    }
}

impl VideoSource for LiveSource {
    fn next_frame(&mut self) -> Option<Frame> {
        while let Some(frame) = self.rx.pop() {
            self.buf.push_back(frame);
        }
        if self.rx.take_dropped() {
            self.buf.clear();
            self.needs_keyframe = true;
        }
        while let Some(frame) = self.buf.pop_front() {
            if self.needs_keyframe && !frame.key_frame {
                continue;
            }
            self.needs_keyframe = false;
            self.consecutive_empty = 0;
            if frame.key_frame {
                self.last_key = Some(frame.clone());
            }
            return Some(frame);
        }
        self.consecutive_empty = self.consecutive_empty.saturating_add(1);
        if self.needs_keyframe {
            return None;
        }
        let repeat_limit = (self.fps / 12).max(2);
        (self.consecutive_empty <= repeat_limit)
            .then(|| self.last_key.clone())
            .flatten()
    }

    fn next_audio(&mut self) -> Vec<Vec<u8>> {
        let Some(receiver) = &self.audio_rx else {
            return Vec::new();
        };
        while let Ok(packet) = receiver.try_recv() {
            self.audio_buf.push_back(packet);
        }
        let frame_duration = self.audio_codec.frame_duration_90k();
        self.audio_credit_90k = self
            .audio_credit_90k
            .saturating_add(crate::rtp::CLOCK_HZ / self.fps);
        let count = self.audio_credit_90k / frame_duration;
        self.audio_credit_90k %= frame_duration;
        // 实时媒体优先低延迟：仅保留当前发送窗和少量抖动余量。
        // 视频停顿期间生产端会因有界通道丢弃新包，因此消费后必须从队首
        // 删除陈旧音频，避免画面恢复后以生产速率追赶一个永远清不掉的积压。
        let keep = (count as usize).max(3);
        while self.audio_buf.len() > keep {
            self.audio_buf.pop_front();
        }
        (0..count)
            .filter_map(|_| self.audio_buf.pop_front())
            .collect()
    }

    fn has_audio(&self) -> bool {
        self.audio_enabled
    }

    fn audio_codec(&self) -> crate::ps::AudioCodec {
        self.audio_codec.ps_codec()
    }

    fn codec(&self) -> crate::ps::VideoCodec {
        self.codec.ps_codec()
    }

    fn is_live(&self) -> bool {
        self.producer_alive.load(Ordering::Acquire)
    }

    fn take_error(&mut self) -> Option<String> {
        self.terminal_error
            .lock()
            .ok()
            .and_then(|mut error| error.take())
    }

    fn preview_control(&self) -> Option<Arc<PreviewControl>> {
        self.preview_control.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[derive(Default)]
    struct CameraProbeSink {
        frames: std::sync::atomic::AtomicU64,
        changed_frames: std::sync::atomic::AtomicU64,
        last_fingerprint: std::sync::atomic::AtomicU64,
        first_published_at_ms: std::sync::atomic::AtomicU64,
        latencies_ms: Mutex<Vec<u64>>,
        latest_status: Mutex<Option<crate::preview::PreviewStatus>>,
    }

    #[cfg(target_os = "macos")]
    impl PreviewSink for CameraProbeSink {
        fn publish(&self, frame: crate::preview::PreviewJpeg) {
            use std::hash::{Hash, Hasher};

            let published_at_ms = crate::source::now_ms();
            self.first_published_at_ms
                .compare_exchange(0, published_at_ms, Ordering::AcqRel, Ordering::Acquire)
                .ok();
            self.latencies_ms
                .lock()
                .unwrap()
                .push(published_at_ms.saturating_sub(frame.captured_at_ms));
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            frame.data.hash(&mut hasher);
            let fingerprint = hasher.finish();
            let previous = self.last_fingerprint.swap(fingerprint, Ordering::AcqRel);
            if previous != 0 && previous != fingerprint {
                self.changed_frames.fetch_add(1, Ordering::Relaxed);
            }
            self.frames.fetch_add(1, Ordering::Relaxed);
        }

        fn status(&self, status: crate::preview::PreviewStatus) {
            *self.latest_status.lock().unwrap() = Some(status);
        }
    }

    #[cfg(target_os = "macos")]
    fn owned_ffmpeg_roles() -> (Vec<u32>, Vec<u32>) {
        let output = Command::new("/bin/ps")
            .args(["-Ao", "pid=,command="])
            .output()
            .expect("应能读取进程列表");
        let mut camera = Vec::new();
        let mut preview = Vec::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let line = line.trim_start();
            let Some((pid, command)) = line.split_once(char::is_whitespace) else {
                continue;
            };
            let Ok(pid) = pid.trim().parse() else {
                continue;
            };
            if command.contains("slices=1:sliced-threads=0:repeat-headers=1")
                && command.contains("avfoundation")
            {
                camera.push(pid);
            }
            if command.contains("-f h264 -i pipe:0")
                && command.contains("scale=480:-2,format=yuvj420p")
            {
                preview.push(pid);
            }
        }
        (camera, preview)
    }

    #[cfg(target_os = "macos")]
    fn signal_process(pid: u32, signal: libc::c_int) -> bool {
        unsafe { libc::kill(pid as libc::pid_t, signal) == 0 }
    }

    #[cfg(target_os = "macos")]
    fn percentile(values: &[u64], percentile: usize) -> u64 {
        if values.is_empty() {
            return 0;
        }
        let mut sorted = values.to_vec();
        sorted.sort_unstable();
        sorted[(sorted.len() - 1) * percentile.min(100) / 100]
    }

    /// 当前 Mac 真实摄像头硬门禁。默认 60 秒；普通 CI 不运行。
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "需要真实 Mac 摄像头与系统授权"]
    fn macos_camera_preview_probe() {
        let index = std::env::var("UVP_TEST_CAMERA_INDEX")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        let seconds = std::env::var("UVP_TEST_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(60)
            .max(5);
        let inject_faults = std::env::var("UVP_TEST_INJECT_FAULTS").as_deref() == Ok("1");
        if inject_faults {
            assert!(seconds >= 60, "故障注入门禁至少运行 60 秒");
        }
        let started_at_ms = crate::source::now_ms();
        let sink = Arc::new(CameraProbeSink::default());
        let sink_trait: Arc<dyn PreviewSink> = sink.clone();
        let uri = format!("live:camera:{index}?audio=none&audio_codec=g711a");
        let source =
            LiveSource::capture(&uri, 30, Some(sink_trait)).expect("真实摄像头采集应在授权后启动");
        let first_h264_deadline = Instant::now() + Duration::from_secs(5);
        while source.rx.pushed_total() == 0 && Instant::now() < first_h264_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let first_h264_ms = crate::source::now_ms().saturating_sub(started_at_ms);
        assert!(source.rx.pushed_total() > 0, "5 秒内没有 H.264 AU");
        assert!(first_h264_ms <= 5_000, "H.264 首帧 {first_h264_ms}ms");

        let first_jpeg_deadline = Instant::now() + Duration::from_secs(5);
        while sink.frames.load(Ordering::Acquire) == 0 && Instant::now() < first_jpeg_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            sink.frames.load(Ordering::Acquire) > 0,
            "5 秒内没有 MJPEG 帧"
        );
        let (initial_camera_pids, initial_preview_pids) = owned_ffmpeg_roles();
        assert_eq!(
            initial_camera_pids.len(),
            1,
            "只能有一个 AVFoundation 采集进程"
        );
        assert_eq!(initial_preview_pids.len(), 1, "应有一个隔离预览 worker");
        let initial_camera_pid = initial_camera_pids[0];
        let initial_preview_pid = initial_preview_pids[0];

        let run_started = Instant::now();
        let deadline = Instant::now() + Duration::from_secs(seconds);
        let mut killed_preview_pid = None;
        let mut stopped_preview_pid = None;
        let mut stop_released = false;
        while Instant::now() < deadline {
            let elapsed = run_started.elapsed();
            if inject_faults && killed_preview_pid.is_none() && elapsed >= Duration::from_secs(20) {
                assert!(
                    signal_process(initial_preview_pid, libc::SIGKILL),
                    "应能终止测试持有的预览 worker {initial_preview_pid}"
                );
                killed_preview_pid = Some(initial_preview_pid);
            }
            if inject_faults && stopped_preview_pid.is_none() && elapsed >= Duration::from_secs(40)
            {
                let (camera, preview) = owned_ffmpeg_roles();
                assert_eq!(camera, vec![initial_camera_pid], "预览恢复不得重启摄像头");
                assert_eq!(preview.len(), 1, "kill 后应已创建新的预览 worker");
                assert_ne!(preview[0], initial_preview_pid, "预览 worker PID 应已更新");
                assert!(
                    signal_process(preview[0], libc::SIGSTOP),
                    "应能暂停测试持有的预览 worker {}",
                    preview[0]
                );
                stopped_preview_pid = Some(preview[0]);
            }
            if inject_faults && !stop_released && elapsed >= Duration::from_secs(50) {
                if let Some(pid) = stopped_preview_pid {
                    // supervisor 可能已因 500ms stdin 背压把它局部回收；若仍存在则恢复。
                    let _ = signal_process(pid, libc::SIGCONT);
                    stop_released = true;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let h264 = source.rx.pushed_total();
        let jpeg = sink.frames.load(Ordering::Acquire);
        let first_jpeg_ms = sink
            .first_published_at_ms
            .load(Ordering::Acquire)
            .saturating_sub(started_at_ms);
        let latencies = sink.latencies_ms.lock().unwrap().clone();
        let latency_p50_ms = percentile(&latencies, 50);
        let latency_p95_ms = percentile(&latencies, 95);
        let midpoint = latencies.len() / 2;
        let first_half_p95_ms = percentile(&latencies[..midpoint], 95);
        let second_half_p95_ms = percentile(&latencies[midpoint..], 95);
        let changed_frames = sink.changed_frames.load(Ordering::Acquire);
        let (camera_pids, preview_pids) = owned_ffmpeg_roles();
        eprintln!(
            "camera_probe seconds={seconds} inject_faults={inject_faults} h264={h264} jpeg={jpeg} changed_jpeg={changed_frames} first_h264_ms={first_h264_ms} first_jpeg_ms={first_jpeg_ms} latency_p50_ms={latency_p50_ms} latency_p95_ms={latency_p95_ms} first_half_p95_ms={first_half_p95_ms} second_half_p95_ms={second_half_p95_ms} initial_camera_pid={initial_camera_pid} initial_preview_pid={initial_preview_pid} killed_preview_pid={killed_preview_pid:?} stopped_preview_pid={stopped_preview_pid:?} camera_pids={camera_pids:?} preview_pids={preview_pids:?} status={:?}",
            sink.latest_status.lock().unwrap()
        );
        assert_eq!(camera_pids, vec![initial_camera_pid], "摄像头 PID 不得变化");
        assert_eq!(preview_pids.len(), 1, "应有一个不打开设备的预览 worker");
        assert!(first_jpeg_ms <= 5_000, "MJPEG 首帧 {first_jpeg_ms}ms");
        assert!(h264 >= seconds.saturating_mul(27), "H.264 平均不足 27 FPS");
        assert!(jpeg >= seconds.saturating_mul(27), "MJPEG 平均不足 27 FPS");
        assert!(
            changed_frames >= jpeg.saturating_mul(9) / 10,
            "JPEG 长期重复，变化帧 {changed_frames}/{jpeg}"
        );
        assert!(
            latency_p95_ms < 1_000,
            "AU→JPEG P95 延迟 {latency_p95_ms}ms"
        );
        assert!(
            second_half_p95_ms <= first_half_p95_ms.saturating_add(500),
            "延迟持续增长：前半 P95={first_half_p95_ms}ms，后半 P95={second_half_p95_ms}ms"
        );
        if inject_faults {
            assert_ne!(
                preview_pids[0], initial_preview_pid,
                "故障后应由新 worker 服务"
            );
            let status = sink.latest_status.lock().unwrap().clone().unwrap();
            assert_eq!(status.phase, crate::preview::PreviewPhase::Playing);
            assert_eq!(status.recoveries, 2, "两种故障应各触发一次局部恢复");
        }

        drop(source);
        let stop_deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < stop_deadline {
            let (camera, preview) = owned_ffmpeg_roles();
            if camera.is_empty() && preview.is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("3 秒内仍有本应用 owned FFmpeg 进程");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn camera采集命令只有一个视频输出且不含预览旁路() {
        let args = camera_command_args_for_test(0, None, LiveAudioCodec::G711A, 30);
        let command = args.join(" ");

        assert_eq!(
            args.windows(2)
                .filter(|pair| pair[0] == "-f" && pair[1] == "avfoundation")
                .count(),
            1
        );
        assert_eq!(
            args.windows(2)
                .filter(|pair| pair[0] == "-f" && pair[1] == "h264")
                .count(),
            1
        );
        assert!(!command.contains("pipe:3"));
        assert!(!command.contains("mjpeg"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn camera采集必须显式请求横向画面() {
        let args = camera_command_args_for_test(0, None, LiveAudioCodec::G711A, 30);
        let input_position = args
            .iter()
            .position(|arg| arg == "-i")
            .expect("采集命令必须包含输入");
        let size_position = args
            .windows(2)
            .position(|pair| pair == ["-video_size", "1280x720"])
            .expect("AVFoundation 默认可能选择竖屏模式，必须显式请求 1280x720");

        assert!(size_position < input_position, "输入尺寸必须在 -i 之前生效");
    }

    #[test]
    fn cfr输出必须显式指定目标帧率() {
        assert_eq!(cfr_output_args("30"), ["-r", "30", "-fps_mode", "cfr"]);
    }

    fn frame(id: u8) -> Frame {
        Frame {
            data: vec![id],
            key_frame: false,
        }
    }

    #[test]
    fn encoded_frame_hub先交主路且预览满载不阻塞() {
        let primary = Arc::new(LatestFrameQueue::new(8));
        let (preview, _rx) = crate::preview_worker::preview_input_channel_for_test(7, 1);
        assert_eq!(
            preview.try_send(CapturedAccessUnit::new(7, 1, vec![0, 0, 1, 5, 1], true, 1,)),
            crate::preview_worker::PreviewSendResult::Sent
        );
        let hub = EncodedFrameHub::new(Arc::clone(&primary), Some(preview), 7);

        hub.send_frame(Frame {
            data: vec![0, 0, 1, 1, 2],
            key_frame: false,
        });

        assert_eq!(primary.pushed_total(), 1);
        assert_eq!(primary.pop().unwrap().data, vec![0, 0, 1, 1, 2]);
    }

    #[test]
    fn encoded_frame_hub给预览附加代际序号和主读取时刻() {
        let primary = Arc::new(LatestFrameQueue::new(8));
        let (preview, rx) = crate::preview_worker::preview_input_channel_for_test(9, 2);
        let hub = EncodedFrameHub::new(primary, Some(preview), 9);
        let mut data = vec![0, 0, 1, 7, 1];
        data.extend_from_slice(&[0, 0, 1, 8, 2]);
        data.extend_from_slice(&[0, 0, 1, 5, 3]);

        hub.send_frame(Frame {
            data,
            key_frame: true,
        });
        let access_unit = rx.try_recv().unwrap();
        assert_eq!(access_unit.generation, 9);
        assert_eq!(access_unit.sequence, 1);
        assert!(access_unit.config_keyframe);
        assert!(access_unit.captured_at_ms > 0);
    }

    #[test]
    fn 实时队列满载时只保留最新帧() {
        let queue = LatestFrameQueue::new(2);
        queue.push(frame(1));
        queue.push(frame(2));
        queue.push(frame(3));

        assert!(queue.take_dropped());
        assert_eq!(queue.pop().expect("最新帧应存在").data, vec![3]);
        assert!(queue.pop().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn owned子进程必须先终止再有界等待reader() {
        let mut child = Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("应能启动 fake owned child");
        let mut stdout = child.stdout.take().unwrap();
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stdout.read_to_end(&mut bytes);
        });
        let mut children = vec![child];
        let mut threads = vec![reader];

        let started = Instant::now();
        assert!(stop_owned_children_and_threads(
            &mut children,
            &mut threads,
            Duration::from_secs(1),
        ));

        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(threads.is_empty(), "reader JoinHandle 必须被显式回收");
        assert!(
            children.is_empty(),
            "owned child 必须被回收并移出所有权集合"
        );
    }
}
