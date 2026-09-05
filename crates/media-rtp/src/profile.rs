use serde::{Deserialize, Serialize};

/// 视频输出编码。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaVideoCodec {
    H264,
    H265,
}

/// 音频输出编码。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaAudioCodec {
    #[serde(rename = "g711_a")]
    G711A,
    #[serde(rename = "g711_u")]
    G711U,
    Aac,
}

/// 手机模拟器音视频页面中的画质预设。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaQualityPreset {
    Smooth,
    Standard,
    Hd,
    Uhd,
}

impl MediaQualityPreset {
    /// 按手机页面顺序返回全部预设。
    pub const ALL: [Self; 4] = [Self::Smooth, Self::Standard, Self::Hd, Self::Uhd];

    /// 返回预设的 `(width, height, fps, bitrate_kbps, keyframe_interval_seconds)`。
    pub const fn video_parameters(self) -> (u32, u32, u32, u32, u32) {
        match self {
            Self::Smooth => (640, 480, 15, 600, 1),
            Self::Standard => (1280, 720, 20, 1200, 1),
            Self::Hd => (1280, 720, 25, 2000, 1),
            Self::Uhd => (1920, 1080, 25, 4000, 1),
        }
    }
}

/// 统一的桌面端音视频输出配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaProfile {
    pub width: u32,
    pub height: u32,
    pub video_fps: u32,
    pub bitrate_kbps: u32,
    pub keyframe_interval_seconds: u32,
    pub video_codec: MediaVideoCodec,
    pub audio_codec: MediaAudioCodec,
    /// AAC 保留用户选择的采样率；G.711 的有效采样率始终为 8000 Hz。
    pub audio_sample_rate_hz: u32,
}

impl Default for MediaProfile {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            video_fps: 25,
            bitrate_kbps: 2000,
            keyframe_interval_seconds: 1,
            video_codec: MediaVideoCodec::H264,
            audio_codec: MediaAudioCodec::G711A,
            audio_sample_rate_hz: 16000,
        }
    }
}

impl MediaProfile {
    /// 返回编码器实际使用的音频采样率。
    pub const fn effective_audio_sample_rate_hz(&self) -> u32 {
        match self.audio_codec {
            MediaAudioCodec::G711A | MediaAudioCodec::G711U => 8000,
            MediaAudioCodec::Aac => self.audio_sample_rate_hz,
        }
    }

    /// 校验页面允许的输出参数。
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(
            (self.width, self.height),
            (640, 480) | (1280, 720) | (1920, 1080)
        ) {
            return Err(format!(
                "unsupported output resolution: {}x{}",
                self.width, self.height
            ));
        }
        if !(1..=120).contains(&self.video_fps) {
            return Err(format!(
                "video FPS must be between 1 and 120: {}",
                self.video_fps
            ));
        }
        if !matches!(self.bitrate_kbps, 600 | 1200 | 2000 | 4000 | 6000 | 8000) {
            return Err(format!(
                "unsupported video bitrate: {} kbps",
                self.bitrate_kbps
            ));
        }
        if !matches!(self.keyframe_interval_seconds, 1 | 2 | 4) {
            return Err(format!(
                "unsupported keyframe interval: {} seconds",
                self.keyframe_interval_seconds
            ));
        }
        if !matches!(self.audio_sample_rate_hz, 8000 | 16000) {
            return Err(format!(
                "unsupported audio sample rate: {} Hz",
                self.audio_sample_rate_hz
            ));
        }
        Ok(())
    }

    /// 返回四个视频参数同时命中的预设；编码和音频采样率不参与匹配。
    pub fn matched_preset(&self) -> Option<MediaQualityPreset> {
        MediaQualityPreset::ALL
            .iter()
            .copied()
            .find(|preset| preset.video_parameters() == self.video_parameters())
    }

    /// 应用预设的四个视频参数，并保留编码和音频采样率。
    pub fn apply_preset(&mut self, preset: MediaQualityPreset) {
        let (width, height, video_fps, bitrate_kbps, keyframe_interval_seconds) =
            preset.video_parameters();
        self.width = width;
        self.height = height;
        self.video_fps = video_fps;
        self.bitrate_kbps = bitrate_kbps;
        self.keyframe_interval_seconds = keyframe_interval_seconds;
    }

    fn video_parameters(&self) -> (u32, u32, u32, u32, u32) {
        (
            self.width,
            self.height,
            self.video_fps,
            self.bitrate_kbps,
            self.keyframe_interval_seconds,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_serde<T: serde::Serialize + serde::de::DeserializeOwned>() {}

    #[test]
    fn media_profile_default_matches_mobile_hd_defaults() {
        let profile = MediaProfile::default();

        assert_eq!(profile.width, 1280);
        assert_eq!(profile.height, 720);
        assert_eq!(profile.video_fps, 25);
        assert_eq!(profile.bitrate_kbps, 2000);
        assert_eq!(profile.keyframe_interval_seconds, 1);
        assert_eq!(profile.video_codec, MediaVideoCodec::H264);
        assert_eq!(profile.audio_codec, MediaAudioCodec::G711A);
        assert_eq!(profile.audio_sample_rate_hz, 16000);
        assert_eq!(profile.effective_audio_sample_rate_hz(), 8000);
        assert_eq!(profile.matched_preset(), Some(MediaQualityPreset::Hd));
    }

    #[test]
    fn codecs_are_serde_values_and_use_snake_case_names() {
        assert_serde::<MediaVideoCodec>();
        assert_serde::<MediaAudioCodec>();
        assert_serde::<MediaProfile>();
    }

    #[test]
    fn presets_change_only_video_parameters() {
        let presets = [
            (MediaQualityPreset::Smooth, (640, 480, 15, 600, 1)),
            (MediaQualityPreset::Standard, (1280, 720, 20, 1200, 1)),
            (MediaQualityPreset::Hd, (1280, 720, 25, 2000, 1)),
            (MediaQualityPreset::Uhd, (1920, 1080, 25, 4000, 1)),
        ];

        for (preset, expected) in presets {
            let mut profile = MediaProfile {
                video_codec: MediaVideoCodec::H265,
                audio_codec: MediaAudioCodec::Aac,
                audio_sample_rate_hz: 8000,
                ..MediaProfile::default()
            };

            profile.apply_preset(preset);

            assert_eq!(
                (
                    profile.width,
                    profile.height,
                    profile.video_fps,
                    profile.bitrate_kbps,
                    profile.keyframe_interval_seconds,
                ),
                expected
            );
            assert_eq!(profile.video_codec, MediaVideoCodec::H265);
            assert_eq!(profile.audio_codec, MediaAudioCodec::Aac);
            assert_eq!(profile.audio_sample_rate_hz, 8000);
        }
    }

    #[test]
    fn matched_preset_requires_all_four_video_values() {
        let mut profile = MediaProfile::default();
        assert_eq!(profile.matched_preset(), Some(MediaQualityPreset::Hd));

        profile.video_fps = 17;
        assert_eq!(profile.matched_preset(), None);
        assert!(profile.validate().is_ok());

        profile.video_fps = 25;
        profile.video_codec = MediaVideoCodec::H265;
        profile.audio_codec = MediaAudioCodec::Aac;
        profile.audio_sample_rate_hz = 8000;
        assert_eq!(profile.matched_preset(), Some(MediaQualityPreset::Hd));
    }

    #[test]
    fn g711_effective_sample_rate_is_fixed_but_aac_preserves_supported_rates() {
        for codec in [MediaAudioCodec::G711A, MediaAudioCodec::G711U] {
            for stored_rate in [8000, 16000] {
                let profile = MediaProfile {
                    audio_codec: codec,
                    audio_sample_rate_hz: stored_rate,
                    ..MediaProfile::default()
                };
                assert!(profile.validate().is_ok());
                assert_eq!(profile.effective_audio_sample_rate_hz(), 8000);
            }
        }

        for stored_rate in [8000, 16000] {
            let profile = MediaProfile {
                audio_codec: MediaAudioCodec::Aac,
                audio_sample_rate_hz: stored_rate,
                ..MediaProfile::default()
            };
            assert!(profile.validate().is_ok());
            assert_eq!(profile.effective_audio_sample_rate_hz(), stored_rate);
        }
    }

    #[test]
    fn validation_keeps_legacy_fps_range_and_rejects_non_spec_values() {
        for video_fps in [1, 30, 120] {
            let profile = MediaProfile {
                video_fps,
                ..MediaProfile::default()
            };
            assert!(profile.validate().is_ok(), "fps={video_fps}");
        }

        for video_fps in [0, 121] {
            let profile = MediaProfile {
                video_fps,
                ..MediaProfile::default()
            };
            assert!(profile.validate().is_err(), "fps={video_fps}");
        }

        for (width, height) in [(0, 720), (640, 360), (1920, 720)] {
            let profile = MediaProfile {
                width,
                height,
                ..MediaProfile::default()
            };
            assert!(profile.validate().is_err(), "resolution={width}x{height}");
        }

        for bitrate_kbps in [0, 500, 3000, 10_000] {
            let profile = MediaProfile {
                bitrate_kbps,
                ..MediaProfile::default()
            };
            assert!(profile.validate().is_err(), "bitrate={bitrate_kbps}");
        }

        for keyframe_interval_seconds in [0, 3, 5] {
            let profile = MediaProfile {
                keyframe_interval_seconds,
                ..MediaProfile::default()
            };
            assert!(
                profile.validate().is_err(),
                "gop={keyframe_interval_seconds}"
            );
        }

        for audio_sample_rate_hz in [0, 44_100] {
            let profile = MediaProfile {
                audio_codec: MediaAudioCodec::Aac,
                audio_sample_rate_hz,
                ..MediaProfile::default()
            };
            assert!(
                profile.validate().is_err(),
                "sample_rate={audio_sample_rate_hz}"
            );
        }
    }
}
