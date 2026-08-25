use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Error, Result};

use super::{drain_frames, ffmpeg_bin, Frame, FrameSender, VideoSource, G711_PACKET_BYTES};

const DEFAULT_SCREEN_WIDTH: u32 = 1280;
const DEFAULT_SCREEN_HEIGHT: u32 = 720;
const DEFAULT_SCREEN_BITRATE_KBPS: u32 = 2500;
const MIN_SCREEN_DIMENSION: u32 = 160;
const MAX_SCREEN_LONG_SIDE: u32 = 3840;
const MAX_SCREEN_SHORT_SIDE: u32 = 2160;
const MIN_SCREEN_BITRATE_KBPS: u32 = 128;
const MAX_SCREEN_BITRATE_KBPS: u32 = 20_000;
const STDERR_TAIL_LINES: usize = 32;

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
    stop_flag: Arc<AtomicBool>,
    producer_alive: Arc<AtomicBool>,
    terminal_error: Arc<Mutex<Option<String>>>,
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
}

impl LatestFrameQueue {
    fn new(capacity: usize) -> Self {
        Self {
            frames: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity: capacity.max(1),
            dropped: AtomicBool::new(false),
        }
    }

    fn push(&self, frame: Frame) {
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

    fn take_dropped(&self) -> bool {
        self.dropped.swap(false, Ordering::AcqRel)
    }
}

impl FrameSender for LatestFrameQueue {
    fn send_frame(&self, frame: Frame) {
        self.push(frame);
    }
}

impl LiveSource {
    /// Parse a complete live URI and start its platform backend.
    pub fn capture(uri: &str, fps: u32) -> Result<Self> {
        let spec = LiveSourceSpec::parse(uri)?;
        let fps = fps.max(1);
        #[cfg(target_os = "macos")]
        {
            match spec {
                LiveSourceSpec::Camera {
                    video_index,
                    audio_index,
                    audio_codec,
                } => Self::capture_camera(video_index, audio_index, audio_codec, fps),
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

    #[cfg(target_os = "macos")]
    fn capture_camera(
        video_index: u32,
        audio_index: Option<u32>,
        audio_codec: LiveAudioCodec,
        fps: u32,
    ) -> Result<Self> {
        let ffmpeg = ffmpeg_bin().ok_or_else(|| {
            Error::Media("macOS camera capture requires ffmpeg with AVFoundation support".into())
        })?;
        if audio_index.is_some() {
            ensure_audio_encoder(&ffmpeg, audio_codec)?;
        }
        let input_fps = camera_input_fps(fps);
        let input_fps_text = input_fps.to_string();
        let output_fps_text = fps.to_string();
        let video_filter =
            "scale=1280:720:force_original_aspect_ratio=decrease,pad=1280:720:(ow-iw)/2:(oh-ih)/2"
                .to_string();
        let video_label = format!("camera video index {video_index} at {input_fps} fps");
        let input = audio_index.map_or_else(
            || format!("{video_index}:none"),
            |index| format!("{video_index}:{index}"),
        );
        let mut command = Command::new(&ffmpeg);
        command.args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            if audio_index.is_some() {
                "quiet"
            } else {
                "warning"
            },
            "-fflags",
            "nobuffer",
            "-flags",
            "low_delay",
            "-avioflags",
            "direct",
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
            "-f",
            "avfoundation",
            "-thread_queue_size",
            "1",
            "-pixel_format",
            "uyvy422",
            "-framerate",
            &input_fps_text,
            "-i",
            &input,
            "-map",
            "0:v:0",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-x264-params",
            &format!(
                "slices=1:sliced-threads=0:repeat-headers=1:keyint={fps}:min-keyint={fps}:scenecut=0:rc-lookahead=0"
            ),
            "-bf",
            "0",
            "-refs",
            "1",
            "-pix_fmt",
            "yuv420p",
            "-vf",
            &video_filter,
            "-g",
            &output_fps_text,
            "-fps_mode",
            "cfr",
            "-f",
            "h264",
            "-flush_packets",
            "1",
            "pipe:1",
        ]);
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
            capture_stderr_tail(&mut startup.children[0], &video_label)?
        };
        let video_ready = Arc::new(AtomicBool::new(false));
        let producer_alive = Arc::new(AtomicBool::new(true));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let terminal_error = Arc::new(Mutex::new(None));
        let tx = Arc::new(LatestFrameQueue::new(2));
        let rx = Arc::clone(&tx);
        let mut stdout = startup.children[0]
            .stdout
            .take()
            .ok_or_else(|| Error::Media(format!("failed to open {video_label} stdout")))?;
        let ready = Arc::clone(&video_ready);
        let alive = Arc::clone(&producer_alive);
        let video_stop = Arc::clone(&stop_flag);
        let video_error = Arc::clone(&terminal_error);
        std::thread::spawn(move || {
            let mut pending = Vec::with_capacity(256 * 1024);
            let mut chunk = [0_u8; 32 * 1024];
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        ready.store(true, Ordering::Release);
                        pending.extend_from_slice(&chunk[..n]);
                        drain_frames(&mut pending, tx.as_ref());
                    }
                }
            }
            if !video_stop.load(Ordering::Acquire) {
                if let Ok(mut error) = video_error.lock() {
                    *error = Some(format!("AVFoundation {video_label} stopped unexpectedly"));
                }
            }
            alive.store(false, Ordering::Release);
        });

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
            std::thread::spawn(move || {
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
        let children = startup.disarm();

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
            stop_flag,
            producer_alive,
            terminal_error,
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
                std::thread::spawn(move || {
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
                let stderr_tail = capture_stderr_tail(&mut child, "H.265 screen encoder")?;
                let writer_tail = Arc::clone(&stderr_tail);
                let reader_tail = Arc::clone(&stderr_tail);
                let writer_stop = Arc::clone(&stop_flag);
                let writer_error = Arc::clone(&terminal_error);
                std::thread::spawn(move || {
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
                let reader_stop = Arc::clone(&stop_flag);
                let reader_error = Arc::clone(&terminal_error);
                std::thread::spawn(move || {
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
                        let tail = capture_stderr_tail(&mut encoder, &label)?;
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
                        std::thread::spawn(move || {
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
                let tail = capture_stderr_tail(&mut screen_startup.children[child_index], &label)?;
                let ready = Arc::new(AtomicBool::new(false));
                let (audio_tx, receiver) = mpsc::sync_channel(50);
                let mut stdout = screen_startup.children[child_index]
                    .stdout
                    .take()
                    .ok_or_else(|| Error::Media(format!("failed to open {label} stdout")))?;
                let ready_thread = Arc::clone(&ready);
                let audio_stop = Arc::clone(&stop_flag);
                let audio_error = Arc::clone(&terminal_error);
                std::thread::spawn(move || {
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

        let children = screen_startup.disarm();
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
            stop_flag,
            producer_alive,
            terminal_error,
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
#[derive(Debug, Clone, Copy)]
struct CameraStartup {
    video_index: u32,
    audio_index: Option<u32>,
    input_fps: u32,
}

#[cfg(target_os = "macos")]
struct StartupChildren {
    children: Vec<Child>,
    armed: bool,
}

#[cfg(target_os = "macos")]
impl StartupChildren {
    fn empty() -> Self {
        Self {
            children: Vec::new(),
            armed: true,
        }
    }

    fn new(child: Child) -> Self {
        Self {
            children: vec![child],
            armed: true,
        }
    }

    fn disarm(mut self) -> Vec<Child> {
        self.armed = false;
        std::mem::take(&mut self.children)
    }
}

#[cfg(target_os = "macos")]
impl Drop for StartupChildren {
    fn drop(&mut self) {
        if self.armed {
            kill_children(&mut self.children);
        }
    }
}

fn capture_stderr_tail(child: &mut Child, label: &str) -> Result<Arc<Mutex<VecDeque<String>>>> {
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::Media(format!("failed to open {label} stderr")))?;
    let tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
    let output = Arc::clone(&tail);
    std::thread::spawn(move || {
        for line in BufReader::new(stderr)
            .lines()
            .map_while(std::result::Result::ok)
        {
            if let Ok(mut lines) = output.lock() {
                if lines.len() == STDERR_TAIL_LINES {
                    lines.pop_front();
                }
                lines.push_back(line);
            }
        }
    });
    Ok(tail)
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
        self.producer_alive.store(false, Ordering::Release);
        #[cfg(target_os = "macos")]
        if let Some(stream) = self.screen_stream.take() {
            let _ = stream.stop_capture();
        }
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(id: u8) -> Frame {
        Frame {
            data: vec![id],
            key_frame: false,
        }
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
}
