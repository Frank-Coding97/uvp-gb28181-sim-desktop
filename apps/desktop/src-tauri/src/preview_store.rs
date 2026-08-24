//! 有界本机预览存储和编码帧 IPC envelope。
//!
//! 该模块只处理编码访问单元，不处理 Tauri、FFmpeg 或 UI。生产者永远只覆盖
//! 最新帧；新会话/断序后必须先通过带参数集的关键帧重新同步。

use std::sync::Mutex;

use media_rtp::{is_config_keyframe, PreviewPacket, VideoCodec};

const MAGIC: [u8; 4] = *b"UVP1";
const VERSION: u8 = 1;
const HEADER_LEN: usize = 48;
const MAX_PAYLOAD_LEN: usize = 32 * 1024 * 1024;

const FLAG_KEYFRAME: u16 = 1;
const FLAG_CONFIG: u16 = 1 << 1;

/// 一个编码访问单元的固定版本二进制 envelope。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewEnvelope {
    pub packet: PreviewPacket,
    pub config_keyframe: bool,
}

impl PreviewEnvelope {
    pub fn from_packet(packet: PreviewPacket) -> Self {
        let config_keyframe = is_config_keyframe(&packet);
        Self {
            packet,
            config_keyframe,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let packet = &self.packet;
        let mut flags = 0_u16;
        if packet.key_frame {
            flags |= FLAG_KEYFRAME;
        }
        if self.config_keyframe {
            flags |= FLAG_CONFIG;
        }
        let payload_len = packet.data.len() as u32;
        let mut output = Vec::with_capacity(HEADER_LEN + packet.data.len());
        output.extend_from_slice(&MAGIC);
        output.push(VERSION);
        output.push(codec_to_byte(packet.codec));
        output.extend_from_slice(&flags.to_le_bytes());
        output.extend_from_slice(&packet.session_id.to_le_bytes());
        output.extend_from_slice(&packet.sequence.to_le_bytes());
        output.extend_from_slice(&packet.fps.to_le_bytes());
        output.extend_from_slice(&packet.pts_90k.to_le_bytes());
        output.extend_from_slice(&packet.captured_at_ms.to_le_bytes());
        output.extend_from_slice(&payload_len.to_le_bytes());
        output.extend_from_slice(&packet.data);
        output
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < HEADER_LEN {
            return Err(format!(
                "preview envelope header truncated: {} bytes",
                bytes.len()
            ));
        }
        if bytes[..4] != MAGIC {
            return Err("preview envelope magic mismatch".into());
        }
        if bytes[4] != VERSION {
            return Err(format!(
                "unsupported preview envelope version: {}",
                bytes[4]
            ));
        }
        let codec = codec_from_byte(bytes[5])?;
        let flags = u16::from_le_bytes([bytes[6], bytes[7]]);
        let session_id = read_u64(bytes, 8);
        let sequence = read_u64(bytes, 16);
        let fps = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
        let pts_90k = read_u64(bytes, 28);
        let captured_at_ms = read_u64(bytes, 36);
        let payload_len = u32::from_le_bytes(bytes[44..48].try_into().unwrap()) as usize;
        if payload_len > MAX_PAYLOAD_LEN {
            return Err(format!("preview payload too large: {payload_len} bytes"));
        }
        let expected_len = HEADER_LEN
            .checked_add(payload_len)
            .ok_or_else(|| "preview envelope length overflow".to_string())?;
        if bytes.len() != expected_len {
            return Err(format!(
                "preview envelope payload length mismatch: header says {payload_len}, actual {}",
                bytes.len().saturating_sub(HEADER_LEN)
            ));
        }
        let packet = PreviewPacket {
            data: bytes[HEADER_LEN..].to_vec(),
            key_frame: flags & FLAG_KEYFRAME != 0,
            codec,
            session_id,
            sequence,
            fps,
            pts_90k,
            captured_at_ms,
        };
        let config_keyframe = flags & FLAG_CONFIG != 0;
        if config_keyframe != is_config_keyframe(&packet) {
            return Err("preview envelope config-keyframe flag mismatch".into());
        }
        Ok(Self {
            packet,
            config_keyframe,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PreviewStoreStats {
    pub dropped_frames: u64,
    pub sequence_gaps: u64,
    pub stale_frames: u64,
}

#[derive(Debug, Default)]
struct StoreInner {
    session_id: Option<u64>,
    last_sequence: Option<u64>,
    latest: Option<PreviewPacket>,
    latest_config: Option<PreviewPacket>,
    needs_keyframe: bool,
    stats: PreviewStoreStats,
}

/// 线程安全的 latest-wins 预览存储。
#[derive(Debug, Default)]
pub struct PreviewFrameStore {
    inner: Mutex<StoreInner>,
}

impl PreviewFrameStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 发布永不等待消费者；普通帧最多保留一帧，配置关键帧独立保留一帧。
    pub fn publish(&self, packet: PreviewPacket) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if inner.session_id != Some(packet.session_id) {
            inner.session_id = Some(packet.session_id);
            inner.last_sequence = None;
            inner.latest = None;
            inner.latest_config = None;
            inner.needs_keyframe = true;
        }
        if inner
            .last_sequence
            .is_some_and(|last| packet.sequence <= last)
        {
            inner.stats.stale_frames = inner.stats.stale_frames.saturating_add(1);
            return;
        }
        if let Some(last) = inner.last_sequence {
            if packet.sequence > last.saturating_add(1) {
                inner.needs_keyframe = true;
                inner.stats.sequence_gaps = inner.stats.sequence_gaps.saturating_add(1);
            }
        }
        inner.last_sequence = Some(packet.sequence);
        let config_keyframe = is_config_keyframe(&packet);
        if config_keyframe {
            inner.latest_config = Some(packet.clone());
            inner.needs_keyframe = false;
        }
        if inner.needs_keyframe && !config_keyframe {
            inner.stats.dropped_frames = inner.stats.dropped_frames.saturating_add(1);
            return;
        }
        inner.latest = Some(packet);
    }

    /// 返回比 `after_sequence` 更新的当前帧；断序期间优先返回配置关键帧。
    pub fn latest_after(&self, session_id: u64, after_sequence: u64) -> Option<PreviewPacket> {
        let Ok(inner) = self.inner.lock() else {
            return None;
        };
        if inner.session_id != Some(session_id) {
            return None;
        }
        if inner.needs_keyframe {
            return inner.latest_config.clone();
        }
        inner
            .latest
            .as_ref()
            .filter(|packet| packet.sequence > after_sequence)
            .cloned()
    }

    pub fn stats(&self) -> PreviewStoreStats {
        self.inner
            .lock()
            .map(|inner| inner.stats)
            .unwrap_or_default()
    }

    #[allow(dead_code)] // T3/T7 在预览会话 teardown 时调用。
    pub fn clear(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            *inner = StoreInner::default();
        }
    }
}

fn codec_to_byte(codec: VideoCodec) -> u8 {
    match codec {
        VideoCodec::H264 => 0,
        VideoCodec::H265 => 1,
    }
}

fn codec_from_byte(value: u8) -> Result<VideoCodec, String> {
    match value {
        0 => Ok(VideoCodec::H264),
        1 => Ok(VideoCodec::H265),
        _ => Err(format!("unsupported preview codec id: {value}")),
    }
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(session_id: u64, sequence: u64, key_frame: bool, data: Vec<u8>) -> PreviewPacket {
        PreviewPacket {
            data,
            key_frame,
            codec: VideoCodec::H264,
            session_id,
            sequence,
            fps: 30,
            pts_90k: sequence * 3_000,
            captured_at_ms: sequence * 33,
        }
    }

    fn config(session_id: u64, sequence: u64) -> PreviewPacket {
        packet(
            session_id,
            sequence,
            true,
            vec![
                0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 0, 1, 0x65, 3,
            ],
        )
    }

    #[test]
    fn envelope_round_trip_preserves_metadata_and_payload() {
        let original = PreviewEnvelope::from_packet(config(7, 4));
        let decoded = PreviewEnvelope::decode(&original.encode()).expect("envelope should decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn envelope_rejects_truncated_and_unknown_version() {
        assert!(PreviewEnvelope::decode(&[0; 10]).is_err());
        let mut bytes = PreviewEnvelope::from_packet(config(1, 1)).encode();
        bytes[4] = 99;
        assert!(PreviewEnvelope::decode(&bytes)
            .unwrap_err()
            .contains("version"));
    }

    #[test]
    fn store_keeps_latest_frame_and_config_keyframe() {
        let store = PreviewFrameStore::new();
        store.publish(config(1, 1));
        for sequence in 2..=301 {
            store.publish(packet(1, sequence, false, vec![sequence as u8]));
        }
        assert_eq!(
            store.latest_after(1, 1).map(|frame| frame.sequence),
            Some(301)
        );
        assert_eq!(store.stats().dropped_frames, 0);
    }

    #[test]
    fn store_requires_config_keyframe_after_gap_and_drops_stale_frames() {
        let store = PreviewFrameStore::new();
        store.publish(config(2, 1));
        store.publish(packet(2, 3, false, vec![3]));
        assert_eq!(store.stats().sequence_gaps, 1);
        assert_eq!(
            store.latest_after(2, 1).map(|frame| frame.sequence),
            Some(1)
        );
        store.publish(packet(2, 2, false, vec![2]));
        assert_eq!(store.stats().stale_frames, 1);
        store.publish(config(2, 4));
        assert_eq!(
            store.latest_after(2, 1).map(|frame| frame.sequence),
            Some(4)
        );
    }

    #[test]
    fn new_session_does_not_expose_old_frames() {
        let store = PreviewFrameStore::new();
        store.publish(config(1, 1));
        store.publish(config(2, 1));
        assert!(store.latest_after(1, 0).is_none());
        assert_eq!(
            store.latest_after(2, 0).map(|frame| frame.session_id),
            Some(2)
        );
    }
}
