use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Error, Result};

use super::{drain_frames, ffmpeg_bin, Frame, VideoSource, G711_PACKET_BYTES};

const DEFAULT_SCREEN_WIDTH: u32 = 1280;
const DEFAULT_SCREEN_HEIGHT: u32 = 720;
const DEFAULT_SCREEN_BITRATE_KBPS: u32 = 2500;
/// 默认 GOP:25 fps 下 2 秒一个关键帧,与前端下拉框的推荐值一致。
const DEFAULT_GOP: u32 = 50;
const MIN_GOP: u32 = 1;
const MAX_GOP: u32 = 600;
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

/// Validated output profile for a live capture source (screen or camera).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveVideoProfile {
    pub width: u32,
    pub height: u32,
    pub bitrate_kbps: u32,
    pub codec: LiveVideoCodec,
    /// Key-frame interval in frames. Encoders force an IDR every `gop` frames.
    pub gop: u32,
}

impl Default for LiveVideoProfile {
    fn default() -> Self {
        Self {
            width: DEFAULT_SCREEN_WIDTH,
            height: DEFAULT_SCREEN_HEIGHT,
            bitrate_kbps: DEFAULT_SCREEN_BITRATE_KBPS,
            codec: LiveVideoCodec::H264,
            gop: DEFAULT_GOP,
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
        if !(MIN_GOP..=MAX_GOP).contains(&self.gop) {
            return Err(Error::Media(format!(
                "gop must be between {MIN_GOP} and {MAX_GOP} frames"
            )));
        }
        Ok(())
    }

    /// Render the encoding parameters as canonical URI query pairs (no leading `&`).
    fn as_query(&self) -> String {
        format!(
            "width={}&height={}&bitrate={}&codec={}&gop={}",
            self.width,
            self.height,
            self.bitrate_kbps,
            self.codec.as_str(),
            self.gop,
        )
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
    /// AVFoundation camera and optional microphone indexes, plus the output profile.
    Camera {
        video_index: u32,
        audio_index: Option<u32>,
        audio_codec: LiveAudioCodec,
        profile: LiveVideoProfile,
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
            let (index, audio, audio_codec, profile) = parse_camera_query(rest)?;
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
                profile,
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
            "invalid live URI '{uri}'; expected live:camera:<video_index>?audio=<audio_index|none>&audio_codec=<g711a|g711u|aac|opus> or live:screen:<display_id>?audio=<system|microphone:index|none>&audio_codec=<g711a|g711u|aac|opus>, both optionally with &width=<even>&height=<even>&bitrate=<kbps>&codec=<h264|h265>&gop=<frames>"
        )))
    }

    /// Render the stable URI form used for persistence and IPC.
    pub fn canonical_uri(&self) -> String {
        match self {
            Self::Camera {
                video_index,
                audio_index,
                audio_codec,
                profile,
            } => format!(
                "live:camera:{video_index}?audio={}&audio_codec={}&{}",
                audio_index.map_or_else(|| "none".into(), |index| index.to_string()),
                audio_codec.as_str(),
                profile.as_query(),
            ),
            Self::Screen {
                display_id,
                audio,
                audio_codec,
                profile,
            } => format!(
                "live:screen:{display_id}?audio={}&audio_codec={}&{}",
                audio.as_uri_value(),
                audio_codec.as_str(),
                profile.as_query(),
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

/// 记录 URI 里出现过的编码参数,最后组装成校验过的 profile。
/// 摄像头与屏幕两条 URI 共用同一套 `width/height/bitrate/codec/gop` 语义。
#[derive(Default)]
struct ProfileQuery {
    width: Option<u32>,
    height: Option<u32>,
    bitrate_kbps: Option<u32>,
    codec: Option<LiveVideoCodec>,
    gop: Option<u32>,
}

impl ProfileQuery {
    /// 吃掉一个编码参数;返回 `false` 表示这个 key 不是编码参数,交回调用方处理。
    fn accept(&mut self, key: &str, value: &str, label: &str) -> Result<bool> {
        match key {
            "width" => set_once(
                &mut self.width,
                parse_decimal(value, "video width")?,
                key,
                label,
            )?,
            "height" => set_once(
                &mut self.height,
                parse_decimal(value, "video height")?,
                key,
                label,
            )?,
            "bitrate" => set_once(
                &mut self.bitrate_kbps,
                parse_decimal(value, "video bitrate")?,
                key,
                label,
            )?,
            "codec" => set_once(&mut self.codec, LiveVideoCodec::parse(value)?, key, label)?,
            "gop" => set_once(
                &mut self.gop,
                parse_decimal(value, "video gop")?,
                key,
                label,
            )?,
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// 分辨率/码率/编码必须成组出现(省略则整体取默认);GOP 可单独给。
    fn finish(self, label: &str) -> Result<LiveVideoProfile> {
        let defaults = LiveVideoProfile::default();
        let gop = self.gop.unwrap_or(defaults.gop);
        let profile = match (self.width, self.height, self.bitrate_kbps, self.codec) {
            (None, None, None, None) => LiveVideoProfile { gop, ..defaults },
            (Some(width), Some(height), Some(bitrate_kbps), Some(codec)) => LiveVideoProfile {
                width,
                height,
                bitrate_kbps,
                codec,
                gop,
            },
            _ => {
                return Err(Error::Media(format!(
                    "{label} URI must provide width, height, bitrate, and codec together"
                )))
            }
        };
        profile.validate()?;
        Ok(profile)
    }
}

fn set_once<T>(slot: &mut Option<T>, value: T, key: &str, label: &str) -> Result<()> {
    if slot.is_some() {
        return Err(Error::Media(format!(
            "{label} URI repeats the '{key}' parameter"
        )));
    }
    *slot = Some(value);
    Ok(())
}

fn parse_camera_query(rest: &str) -> Result<(&str, &str, LiveAudioCodec, LiveVideoProfile)> {
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
    let mut profile = ProfileQuery::default();
    for item in query.split('&') {
        let (key, value) = item
            .split_once('=')
            .ok_or_else(|| Error::Media("camera URI query must contain key=value pairs".into()))?;
        if key.is_empty() || value.is_empty() || value.contains('=') {
            return Err(Error::Media(
                "camera URI contains an invalid query parameter".into(),
            ));
        }
        match key {
            "audio" => set_once(&mut audio, value, key, "camera")?,
            "audio_codec" => set_once(
                &mut audio_codec,
                LiveAudioCodec::parse(value)?,
                key,
                "camera",
            )?,
            _ if profile.accept(key, value, "camera")? => {}
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
        profile.finish("camera")?,
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
    let mut profile = ProfileQuery::default();
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
            "audio" => set_once(&mut audio, LiveAudioSource::parse(value)?, key, "screen")?,
            "audio_codec" => set_once(
                &mut audio_codec,
                LiveAudioCodec::parse(value)?,
                key,
                "screen",
            )?,
            _ if profile.accept(key, value, "screen")? => {}
            _ => {
                return Err(Error::Media(format!(
                    "unsupported screen URI parameter '{key}'"
                )))
            }
        }
    }

    let audio =
        audio.ok_or_else(|| Error::Media("screen URI must include an 'audio' parameter".into()))?;
    Ok((
        id,
        audio,
        audio_codec.unwrap_or_default(),
        profile.finish("screen")?,
    ))
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
    let mut command = Command::new(ffmpeg);
    command.args(["-hide_banner", "-nostdin", "-encoders"]);
    let Ok(output) = crate::source::run_command_bounded(&mut command, Duration::from_secs(8))
    else {
        return (false, false);
    };
    if output.timed_out || !output.status.success() {
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
    let mut command = Command::new(ffmpeg);
    command.args([
        "-nostdin",
        "-f",
        "avfoundation",
        "-list_devices",
        "true",
        "-i",
        "",
    ]);
    let output = crate::source::run_command_bounded(&mut command, Duration::from_secs(10))
        .map_err(|e| Error::Media(format!("failed to enumerate AVFoundation devices: {e}")))?;
    if output.timed_out {
        return Err(Error::Media(
            "AVFoundation device enumeration timed out after 10 seconds".into(),
        ));
    }
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
    rx: mpsc::Receiver<Frame>,
    buf: VecDeque<Frame>,
    audio_rx: Option<mpsc::Receiver<Vec<u8>>>,
    audio_buf: VecDeque<Vec<u8>>,
    audio_enabled: bool,
    audio_credit_90k: u32,
    audio_codec: LiveAudioCodec,
    fps: u32,
    codec: LiveVideoCodec,
    children: Vec<Child>,
    /// FFmpeg 会话级诊断文件。子进程 wait 后删除,避免临时目录无限积累。
    report_files: Vec<std::path::PathBuf>,
    stop_flag: Arc<AtomicBool>,
    producer_alive: Arc<AtomicBool>,
    terminal_error: Arc<Mutex<Option<String>>>,
    #[cfg(target_os = "macos")]
    screen_stream: Option<screencapturekit::stream::SCStream>,
    last_key: Option<Frame>,
    consecutive_empty: u32,
    /// 强制关键帧请求标志(A.2.3.1.7 IFrameCmd)。
    ///
    /// 只有自带编码器的路径能兑现:屏幕采集的 H.264 分支走 OpenH264,
    /// 编码线程每拍检查这个标志并调 `force_intra_frame()`。
    /// ffmpeg 子进程路径(摄像头、H.265 屏幕)没有运行中改 GOP 的接口,故为 None。
    force_keyframe: Option<Arc<AtomicBool>>,
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
                    profile,
                } => Self::capture_camera(video_index, audio_index, audio_codec, profile, fps),
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
        profile: LiveVideoProfile,
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
        let video_label = format!("camera video index {video_index} at {input_fps} fps");
        let input = audio_index.map_or_else(
            || format!("{video_index}:none"),
            |index| format!("{video_index}:{index}"),
        );
        // 输出编码参数全部来自 URI(前端"摄像头视频参数"那一列),不再硬编码 1280x720/libx264。
        //
        // `fps=<目标帧率>` 是必须的:AVFoundation 输入按 30/60 帧采集(见
        // `camera_input_fps`),而推流循环按设备配置的帧率消费。若不做帧率归一化,
        // 30fps 的编码结果会被 25fps 的消费者"追不上",缓冲每秒净增 5 帧,触顶后
        // 快进丢弃到最近关键帧 —— 表现为每隔几秒画面瞬跳一次,跟机器性能无关。
        // `format=yuv420p` 则保证无论源是什么像素格式,输出都是平台能解的 4:2:0。
        let scale_filter = format!(
            "scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,fps={fps},format=yuv420p",
            w = profile.width,
            h = profile.height,
            fps = fps
        );
        let bitrate = format!("{}k", profile.bitrate_kbps);
        // 恒定码率的缓冲上限取 2 倍码率:低延迟场景不希望 VBV 攒太久。
        let bufsize = format!("{}k", profile.bitrate_kbps.saturating_mul(2));
        let gop_text = profile.gop.to_string();
        let video_encoder = match profile.codec {
            LiveVideoCodec::H264 => "libx264",
            LiveVideoCodec::H265 => "libx265",
        };
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
            "-f",
            "avfoundation",
            "-framerate",
            &input_fps_text,
            "-i",
            &input,
            "-map",
            "0:v:0",
            "-c:v",
            video_encoder,
            // ultrafast 关闭 CABAC/亚像素搜索,运动场景同码率下块效应明显;
            // veryfast 在 720p/1080p 实时编码上 CPU 代价仍可控,画面显著更干净。
            "-preset",
            "veryfast",
            "-tune",
            "zerolatency",
            "-pix_fmt",
            "yuv420p",
            "-vf",
            &scale_filter,
            "-b:v",
            &bitrate,
            "-maxrate",
            &bitrate,
            "-bufsize",
            &bufsize,
            "-g",
            &gop_text,
            "-fps_mode",
            "passthrough",
        ]);
        // x264 的 slice 参数是 libx264 专属;libx265 用 -x265-params 表达同样的意图。
        // 两者都固定 GOP/关闭场景切换:GB28181 平台按关键帧起播,关键帧间隔必须可预期;
        // CBR + force-cfr 则保证运动时码率不塌、时间基稳定(花屏与抖动的直接来源)。
        match profile.codec {
            LiveVideoCodec::H264 => {
                let params = format!(
                    "nal-hrd=cbr:force-cfr=1:keyint={gop}:min-keyint={gop}:scenecut=0:slices=1:sliced-threads=0",
                    gop = profile.gop
                );
                command.args(["-profile:v", "high"]);
                command.args(["-x264-params", &params]);
                command.args(["-f", "h264"]);
            }
            LiveVideoCodec::H265 => {
                let params = format!(
                    "log-level=error:keyint={gop}:min-keyint={gop}:scenecut=0",
                    gop = profile.gop
                );
                command.args(["-x265-params", &params]);
                command.args(["-tag:v", "hvc1", "-f", "hevc"]);
            }
        }
        command.arg("pipe:1");
        // 带音频时编码后的音频 ES 走 pipe:2(stderr),于是 stderr 不能再承载日志。
        // 单进程是刻意的:一个 AVFoundation 会话同时拥有连续互通相机的两条轨,
        // 第二个进程会让 iPhone 麦克风在视频活动时卡住。
        //
        // 但"日志完全看不到"是不可接受的:失败时只能报一句"stopped unexpectedly"。
        // 用 ffmpeg 自带的 FFREPORT 把日志写到文件,既不占 stderr 也保住了诊断。
        let report_path = if audio_index.is_some() {
            let path = crate::source::create_private_work_file("ffmpeg-camera", "log")?;
            // FFREPORT 的 file= 里 ':' 与 '\\' 需转义。文件已由 Rust 以 create_new
            // 原子占位并在 Unix 设为 0600,FFmpeg 只截断本次会话的高熵路径。
            let escaped = path
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace(':', "\\:");
            command.env("FFREPORT", format!("file={escaped}:level=32"));
            Some(path)
        } else {
            None
        };
        let report_cleanup = TemporaryFileCleanup::new(report_path.clone());
        if audio_index.is_some() {
            command.args(["-map", "0:a:0"]);
            audio_codec.append_ffmpeg_output(&mut command, "pipe:2");
        }
        let video = match command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(video) => video,
            Err(error) => {
                if let Some(path) = report_path.as_deref() {
                    let _ = std::fs::remove_file(path);
                }
                return Err(Error::Media(format!(
                    "failed to start {video_label}: {error}"
                )));
            }
        };

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
        let (tx, rx) = mpsc::sync_channel((fps * 3) as usize);
        let mut stdout = startup.children[0]
            .stdout
            .take()
            .ok_or_else(|| Error::Media(format!("failed to open {video_label} stdout")))?;
        let ready = Arc::clone(&video_ready);
        let alive = Arc::clone(&producer_alive);
        let video_stop = Arc::clone(&stop_flag);
        let video_error = Arc::clone(&terminal_error);
        let report_tail_for_thread = report_path.clone();
        let video_codec = profile.codec;
        std::thread::spawn(move || {
            let mut pending = Vec::with_capacity(256 * 1024);
            let mut chunk = [0_u8; 32 * 1024];
            // 区分"正常 EOF"与"读失败":之前 `Ok(0) | Err(_) => break` 把两者
            // 合成一句"stopped unexpectedly",错误 kind 直接丢了。
            let mut read_error_kind: Option<String> = None;
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) => break,
                    Err(e) => {
                        read_error_kind = Some(e.kind().to_string());
                        break;
                    }
                    Ok(n) => {
                        ready.store(true, Ordering::Release);
                        pending.extend_from_slice(&chunk[..n]);
                        // NAL 头布局按编码分支:H.265 是 2 字节头、类型位不同,
                        // 用 H.264 的切分器会把 VPS/SPS/PPS 和关键帧判断全判错。
                        match video_codec {
                            LiveVideoCodec::H264 => drain_frames(&mut pending, &tx),
                            LiveVideoCodec::H265 => drain_hevc_frames(&mut pending, &tx),
                        }
                    }
                }
            }
            if !video_stop.load(Ordering::Acquire) {
                if let Ok(mut error) = video_error.lock() {
                    let mut msg = format!("AVFoundation {video_label} stopped unexpectedly");
                    if let Some(kind) = read_error_kind {
                        msg.push_str(&format!(" (read error: {kind})"));
                    }
                    if let Some(tail) = report_tail_for_thread
                        .as_deref()
                        .map(read_report_tail)
                        .filter(|t| !t.is_empty())
                    {
                        msg.push_str(&format!("; ffmpeg log: {tail}"));
                    }
                    *error = Some(msg);
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
                                    Ok(()) => {}
                                    // 满了就丢:保低延迟。但必须计数,否则现场无法
                                    // 区分"采集丢包"与"网络丢包"。
                                    Err(mpsc::TrySendError::Full(_)) => {
                                        crate::source::note_dropped_frame()
                                    }
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

        if let Err(error) = wait_for_ffmpeg_startup(
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
        ) {
            // 带音频时 stderr 被音频占用,诊断信息在 FFREPORT 文件里。
            let tail = report_path
                .as_deref()
                .map(read_report_tail)
                .unwrap_or_default();
            if let Some(path) = report_path.as_deref() {
                let _ = std::fs::remove_file(path);
            }
            if tail.is_empty() {
                return Err(error);
            }
            return Err(Error::Media(format!("{error}; ffmpeg log: {tail}")));
        }
        let children = startup.disarm();
        let report_files = report_cleanup.persist().into_iter().collect();

        Ok(Self {
            rx,
            buf: VecDeque::new(),
            audio_rx,
            audio_buf: VecDeque::new(),
            audio_enabled: audio_index.is_some(),
            audio_credit_90k: 0,
            audio_codec,
            fps,
            codec: profile.codec,
            children,
            report_files,
            stop_flag,
            producer_alive,
            terminal_error,
            screen_stream: None,
            last_key: None,
            consecutive_empty: 0,
            // 摄像头走 ffmpeg 子进程编码,没有运行中强制 IDR 的接口。
            force_keyframe: None,
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

        let (tx, rx) = mpsc::sync_channel((fps * 3) as usize);
        let (raw_tx, raw_rx) = mpsc::sync_channel::<Vec<u8>>(2);
        let video_ready = Arc::new(AtomicBool::new(false));
        let producer_alive = Arc::new(AtomicBool::new(true));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let terminal_error = Arc::new(Mutex::new(None));
        let ready = Arc::clone(&video_ready);
        let encoder_stop = Arc::clone(&stop_flag);
        let encoder_alive = Arc::clone(&producer_alive);
        let mut screen_startup = StartupChildren::empty();
        // 只有 OpenH264 分支能在运行中强制 IDR(A.2.3.1.7 IFrameCmd);
        // H.265 走 ffmpeg 子进程,没有运行中改 GOP 的接口。
        let mut force_keyframe: Option<Arc<AtomicBool>> = None;
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
                let force_flag = Arc::new(AtomicBool::new(false));
                force_keyframe = Some(Arc::clone(&force_flag));
                std::thread::spawn(move || {
                    // OpenH264 的 EncoderConfig 不提供 IDR 周期,只能在编码循环里
                    // 每 gop 帧显式 force_intra_frame(),否则只有第一帧是关键帧,
                    // 平台中途接流要等很久才能出画。
                    let gop = profile.gop.max(1) as u64;
                    let mut encoded: u64 = 0;
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
                        // 平台下发 IFrameCmd 时立刻插一个 IDR,并把 GOP 计数归零 ——
                        // 不归零的话下一个周期性 IDR 会紧跟着来,白费一次码率。
                        if force_flag.swap(false, Ordering::AcqRel) {
                            encoder.force_intra_frame();
                            encoded = 0;
                            tracing::info!("屏幕编码器:按 IFrameCmd 强制插入 IDR");
                        } else if encoded % gop == 0 {
                            encoder.force_intra_frame();
                        }
                        encoded = encoded.wrapping_add(1);
                        let yuv =
                            YUVBuffer::from_rgb8_source(RgbSliceU8::new(&rgb, (width, height)));
                        let Ok(bitstream) = encoder.encode(&yuv) else {
                            continue;
                        };
                        let data = bitstream.to_vec();
                        if !data.is_empty() {
                            ready.store(true, Ordering::Release);
                            if tx
                                .try_send(Frame {
                                    key_frame: h264_is_keyframe(&data),
                                    data: data.into(),
                                })
                                .is_err()
                            {
                                // 通道满/断开:丢这一帧并计数(消费侧还会快进到最近关键帧)。
                                crate::source::note_dropped_frame();
                            }
                        } else if encoded > 1 {
                            // OpenH264 在码率控制下会主动跳帧(enable_skip_frame),
                            // 返回空码流。之前这里静默 continue,丢帧完全不可见。
                            crate::source::note_dropped_frame();
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
                let gop_text = profile.gop.to_string();
                let child = Command::new(&ffmpeg)
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
                        &gop_text,
                        "-f",
                        "hevc",
                        "pipe:1",
                    ])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|e| Error::Media(format!("failed to start H.265 encoder: {e}")))?;
                // spawn 成功后第一时间交给 armed guard；此后任一 pipe/tail 初始化失败,
                // StartupChildren::drop 都会 kill+wait，不能让 std::process::Child drop 后孤儿化。
                screen_startup.children.push(child);
                let child_index = screen_startup.children.len() - 1;
                let mut stdin = screen_startup.children[child_index]
                    .stdin
                    .take()
                    .ok_or_else(|| Error::Media("failed to open H.265 encoder stdin".into()))?;
                let mut stdout = screen_startup.children[child_index]
                    .stdout
                    .take()
                    .ok_or_else(|| Error::Media("failed to open H.265 encoder stdout".into()))?;
                let stderr_tail = capture_stderr_tail(
                    &mut screen_startup.children[child_index],
                    "H.265 screen encoder",
                )?;
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
                                drain_hevc_frames(&mut pending, &tx);
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
                    // 编码器跟不上时这里会丢帧:必须计数,否则现场只能看到"平台卡",
                    // 看不到"本机采集已经在丢"。
                    if raw_tx.try_send(packed).is_err() {
                        crate::source::note_dropped_frame();
                    }
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
                        let encoder = command
                            .stdin(Stdio::piped())
                            .stdout(Stdio::piped())
                            .stderr(Stdio::piped())
                            .spawn()
                            .map_err(|error| {
                                Error::Media(format!("failed to start {label}: {error}"))
                            })?;
                        screen_startup.children.push(encoder);
                        let child_index = screen_startup.children.len() - 1;
                        let stdin = screen_startup.children[child_index]
                            .stdin
                            .take()
                            .ok_or_else(|| Error::Media(format!("failed to open {label} stdin")))?;
                        let mut stdout = screen_startup.children[child_index]
                            .stdout
                            .take()
                            .ok_or_else(|| {
                                Error::Media(format!("failed to open {label} stdout"))
                            })?;
                        let tail =
                            capture_stderr_tail(&mut screen_startup.children[child_index], &label)?;
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
                                                Ok(()) => {}
                                                Err(mpsc::TrySendError::Full(_)) => {
                                                    crate::source::note_dropped_frame()
                                                }
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
                                        Ok(()) => {}
                                        Err(mpsc::TrySendError::Full(_)) => {
                                            crate::source::note_dropped_frame()
                                        }
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
            report_files: Vec::new(),
            stop_flag,
            producer_alive,
            terminal_error,
            screen_stream: Some(stream),
            last_key: None,
            consecutive_empty: 0,
            force_keyframe,
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
struct TemporaryFileCleanup(Option<std::path::PathBuf>);

#[cfg(target_os = "macos")]
impl TemporaryFileCleanup {
    fn new(path: Option<std::path::PathBuf>) -> Self {
        Self(path)
    }

    fn persist(mut self) -> Option<std::path::PathBuf> {
        self.0.take()
    }
}

#[cfg(target_os = "macos")]
impl Drop for TemporaryFileCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
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

/// 读取 FFREPORT 日志文件的最后若干行(带音频的摄像头路径唯一的诊断来源)。
fn read_report_tail(path: &std::path::Path) -> String {
    const MAX_LINES: usize = 12;
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let mut lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() > MAX_LINES {
        lines = lines.split_off(lines.len() - MAX_LINES);
    }
    lines.join(" | ")
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
fn drain_hevc_frames(buf: &mut Vec<u8>, tx: &mpsc::SyncSender<Frame>) {
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
            if tx
                .try_send(Frame {
                    data: bytes::Bytes::copy_from_slice(&buf[frame_start..position]),
                    key_frame,
                })
                .is_err()
            {
                crate::source::note_dropped_frame();
            }
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
        for report in self.report_files.drain(..) {
            if let Err(error) = std::fs::remove_file(&report) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::debug!(%error, path = %report.display(), "清理 FFmpeg 临时报告失败");
                }
            }
        }
    }
}

impl VideoSource for LiveSource {
    /// 强制关键帧(A.2.3.1.7):屏幕采集的 H.264 分支自带 OpenH264 编码器,
    /// 置个标志让编码线程下一帧插 IDR;ffmpeg 子进程路径无法运行中干预,返回 false。
    fn request_keyframe(&mut self) -> bool {
        match &self.force_keyframe {
            Some(flag) => {
                flag.store(true, Ordering::Release);
                true
            }
            None => false,
        }
    }

    fn next_frame(&mut self) -> Option<Frame> {
        while let Ok(frame) = self.rx.try_recv() {
            self.buf.push_back(frame);
        }
        let cap = (self.fps * 3) as usize;
        if self.buf.len() > cap {
            if let Some(index) = self.buf.iter().rposition(|frame| frame.key_frame) {
                self.buf.drain(..index);
            } else {
                while self.buf.len() > cap {
                    self.buf.pop_front();
                }
            }
        }
        if let Some(frame) = self.buf.pop_front() {
            self.consecutive_empty = 0;
            if frame.key_frame {
                self.last_key = Some(frame.clone());
            }
            Some(frame)
        } else {
            self.consecutive_empty = self.consecutive_empty.saturating_add(1);
            let repeat_limit = (self.fps / 12).max(2);
            (self.consecutive_empty <= repeat_limit)
                .then(|| self.last_key.clone())
                .flatten()
        }
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

    #[test]
    fn 摄像头_uri_省略编码档时取默认() {
        let spec = LiveSourceSpec::parse("live:camera:0?audio=none").expect("parse");
        assert_eq!(
            spec,
            LiveSourceSpec::Camera {
                video_index: 0,
                audio_index: None,
                audio_codec: LiveAudioCodec::G711A,
                profile: LiveVideoProfile::default(),
            }
        );
    }

    #[test]
    fn 摄像头_uri_携带编码档与gop() {
        let spec = LiveSourceSpec::parse(
            "live:camera:2?audio=1&audio_codec=aac&width=1920&height=1080&bitrate=4000&codec=h265&gop=100",
        )
        .expect("parse");
        assert_eq!(
            spec,
            LiveSourceSpec::Camera {
                video_index: 2,
                audio_index: Some(1),
                audio_codec: LiveAudioCodec::Aac,
                profile: LiveVideoProfile {
                    width: 1920,
                    height: 1080,
                    bitrate_kbps: 4000,
                    codec: LiveVideoCodec::H265,
                    gop: 100,
                },
            }
        );
    }

    #[test]
    fn gop_可以单独给出() {
        let spec = LiveSourceSpec::parse("live:screen:3?audio=system&gop=25").expect("parse");
        let LiveSourceSpec::Screen { profile, .. } = spec else {
            panic!("expected screen spec");
        };
        assert_eq!(profile.gop, 25);
        // 其余参数仍是默认档。
        assert_eq!(profile.width, DEFAULT_SCREEN_WIDTH);
        assert_eq!(profile.codec, LiveVideoCodec::H264);
    }

    #[test]
    fn 编码档参数必须成组出现() {
        let error = LiveSourceSpec::parse("live:screen:3?audio=none&width=1920").expect_err("err");
        assert!(
            error.to_string().contains("together"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn 重复参数被拒() {
        let error =
            LiveSourceSpec::parse("live:camera:0?audio=none&gop=25&gop=50").expect_err("err");
        assert!(
            error.to_string().contains("repeats"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn gop_越界被拒() {
        assert!(LiveSourceSpec::parse("live:camera:0?audio=none&gop=0").is_err());
        assert!(LiveSourceSpec::parse("live:camera:0?audio=none&gop=601").is_err());
    }

    #[test]
    fn 未知参数被拒() {
        assert!(LiveSourceSpec::parse("live:camera:0?audio=none&fps=30").is_err());
        assert!(LiveSourceSpec::parse("live:screen:1?audio=none&fps=30").is_err());
    }

    #[test]
    fn 规范化_uri_可往返() {
        for uri in [
            "live:camera:0?audio=none&audio_codec=g711a&width=1280&height=720&bitrate=2500&codec=h264&gop=50",
            "live:screen:4?audio=microphone:2&audio_codec=opus&width=640&height=360&bitrate=800&codec=h265&gop=25",
        ] {
            let spec = LiveSourceSpec::parse(uri).expect("parse");
            assert_eq!(spec.canonical_uri(), uri);
            assert_eq!(LiveSourceSpec::parse(&spec.canonical_uri()).expect("re-parse"), spec);
        }
    }

    #[test]
    fn legacy_短屏幕_uri_仍兼容() {
        for uri in ["live:0", "live:ffmpeg:0"] {
            let spec = LiveSourceSpec::parse(uri).expect("parse");
            assert!(matches!(spec, LiveSourceSpec::Screen { display_id: 0, .. }));
        }
        // 规范形式则要求非零 display_id。
        assert!(LiveSourceSpec::parse("live:screen:0?audio=none").is_err());
    }
}
