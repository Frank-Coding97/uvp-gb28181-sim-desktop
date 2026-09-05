//! 统一的媒体时间轴契约。
//!
//! 新来源以访问单元携带的 PTS/时长交给下游。旧的 `VideoSource` 实现仍可由
//! [`LegacyMediaClock`] 在边界处补齐时间戳；这样不会让旧 CLI/压测调用者被迫
//! 同时改造，也避免 PS 和 RTP 各自重新推算一套时钟。

use crate::ps::{AudioCodec, VideoCodec};

/// MPEG/GB28181 媒体时钟频率。
pub const MEDIA_CLOCK_HZ: u64 = 90_000;
/// MPEG-PS PTS/SCR 的 33 位回绕掩码。
pub const PS_TIMESTAMP_MASK: u64 = (1_u64 << 33) - 1;
/// RTP 时间戳的 32 位回绕掩码。
pub const RTP_TIMESTAMP_MASK: u64 = (1_u64 << 32) - 1;

/// 带时间戳的视频编码访问单元。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedVideoAu {
    /// Annex-B 编码访问单元。
    pub data: Vec<u8>,
    /// 实际视频编码。
    pub codec: VideoCodec,
    /// 是否包含可独立解码的关键帧/参数集。
    pub key_frame: bool,
    /// 90kHz 时钟上的绝对 PTS，内部不主动截断回绕。
    pub pts_90k: u64,
    /// 访问单元持续时间，单位为 90kHz ticks。
    pub duration_90k: u64,
}

impl TimedVideoAu {
    pub fn new(
        data: Vec<u8>,
        codec: VideoCodec,
        key_frame: bool,
        pts_90k: u64,
        duration_90k: u64,
    ) -> Self {
        Self {
            data,
            codec,
            key_frame,
            pts_90k,
            duration_90k,
        }
    }
}

/// 带时间戳的音频编码访问单元。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedAudioAu {
    /// 编码后的音频访问单元（AAC 时包含 ADTS 头）。
    pub data: Vec<u8>,
    /// 实际音频编码。
    pub codec: AudioCodec,
    /// 该访问单元的有效采样率。
    pub sample_rate_hz: u32,
    /// 该访问单元包含的单声道样本数。
    pub sample_count: u32,
    /// 90kHz 时钟上的绝对 PTS，内部不主动截断回绕。
    pub pts_90k: u64,
    /// 访问单元持续时间，单位为 90kHz ticks。
    pub duration_90k: u64,
}

impl TimedAudioAu {
    /// 从采样数创建一个访问单元。连续访问单元若使用非整除采样率，
    /// 应改用 [`SampleClock`] 计算 duration 以累计余数。
    pub fn from_samples(
        data: Vec<u8>,
        codec: AudioCodec,
        sample_rate_hz: u32,
        sample_count: u32,
        pts_90k: u64,
    ) -> Self {
        Self {
            data,
            codec,
            sample_rate_hz,
            sample_count,
            pts_90k,
            duration_90k: duration_for_samples(sample_count, sample_rate_hz),
        }
    }

    pub fn with_duration(
        data: Vec<u8>,
        codec: AudioCodec,
        sample_rate_hz: u32,
        sample_count: u32,
        pts_90k: u64,
        duration_90k: u64,
    ) -> Self {
        Self {
            data,
            codec,
            sample_rate_hz,
            sample_count,
            pts_90k,
            duration_90k,
        }
    }
}

/// 媒体事件。音频和视频可以独立到达；discontinuity 不携带媒体负载。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaEvent {
    Video(TimedVideoAu),
    Audio(TimedAudioAu),
    Discontinuity { pts_90k: u64 },
}

impl MediaEvent {
    pub fn pts_90k(&self) -> u64 {
        match self {
            Self::Video(au) => au.pts_90k,
            Self::Audio(au) => au.pts_90k,
            Self::Discontinuity { pts_90k } => *pts_90k,
        }
    }
}

/// 以余数累计把样本数换算为 90kHz ticks。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleClock {
    sample_rate_hz: u32,
    remainder: u64,
}

impl SampleClock {
    pub fn new(sample_rate_hz: u32) -> Option<Self> {
        (sample_rate_hz > 0).then_some(Self {
            sample_rate_hz,
            remainder: 0,
        })
    }

    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    pub fn remainder(&self) -> u64 {
        self.remainder
    }

    pub fn duration_for_samples(&mut self, sample_count: u32) -> u64 {
        let numerator = u64::from(sample_count) * MEDIA_CLOCK_HZ + self.remainder;
        let duration = numerator / u64::from(self.sample_rate_hz);
        self.remainder = numerator % u64::from(self.sample_rate_hz);
        duration
    }
}

/// 旧的按 FPS/音频包读取入口的时间戳适配器。
#[derive(Debug, Clone)]
pub struct LegacyMediaClock {
    fps: u32,
    video_pts_90k: u64,
    video_remainder: u64,
    audio_pts_90k: u64,
    audio_codec: AudioCodec,
    audio_sample_rate_hz: u32,
    audio_clock: SampleClock,
}

impl LegacyMediaClock {
    pub fn new(fps: u32, audio_codec: AudioCodec, audio_sample_rate_hz: u32) -> Self {
        let audio_sample_rate_hz = if audio_sample_rate_hz > 0 {
            audio_sample_rate_hz
        } else {
            audio_codec.default_sample_rate_hz()
        };
        Self {
            fps: fps.max(1),
            video_pts_90k: 0,
            video_remainder: 0,
            audio_pts_90k: 0,
            audio_codec,
            audio_sample_rate_hz,
            audio_clock: SampleClock::new(audio_sample_rate_hz)
                .expect("legacy audio sample rate must be positive"),
        }
    }

    pub fn video(&mut self, data: Vec<u8>, codec: VideoCodec, key_frame: bool) -> TimedVideoAu {
        let duration = duration_for_rate(MEDIA_CLOCK_HZ, self.fps, &mut self.video_remainder);
        let au = TimedVideoAu::new(data, codec, key_frame, self.video_pts_90k, duration);
        self.video_pts_90k = self.video_pts_90k.saturating_add(duration);
        au
    }

    pub fn audio(&mut self, data: Vec<u8>, sample_count: Option<u32>) -> TimedAudioAu {
        let sample_count = sample_count.unwrap_or_else(|| self.audio_codec.default_sample_count());
        let duration = self.audio_clock.duration_for_samples(sample_count);
        let au = TimedAudioAu::with_duration(
            data,
            self.audio_codec,
            self.audio_sample_rate_hz,
            sample_count,
            self.audio_pts_90k,
            duration,
        );
        self.audio_pts_90k = self.audio_pts_90k.saturating_add(duration);
        au
    }
}

/// 把内部 90kHz 时间戳投影到 PS 的 33 位字段。
pub const fn ps_timestamp(pts_90k: u64) -> u64 {
    pts_90k & PS_TIMESTAMP_MASK
}

/// 把内部 90kHz 时间戳投影到 RTP 的 32 位字段。
pub const fn rtp_timestamp(pts_90k: u64) -> u32 {
    (pts_90k & RTP_TIMESTAMP_MASK) as u32
}

fn duration_for_samples(sample_count: u32, sample_rate_hz: u32) -> u64 {
    if sample_rate_hz == 0 {
        return 0;
    }
    u64::from(sample_count) * MEDIA_CLOCK_HZ / u64::from(sample_rate_hz)
}

fn duration_for_rate(numerator: u64, denominator: u32, remainder: &mut u64) -> u64 {
    let denominator = u64::from(denominator.max(1));
    let total = numerator + *remainder;
    let duration = total / denominator;
    *remainder = total % denominator;
    duration
}
