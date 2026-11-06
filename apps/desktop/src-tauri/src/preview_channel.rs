//! 预览帧通道。
//!
//! v2(media-pipeline-v2 P4)起有两种载荷:
//! - **编码帧**(主路):共享媒体总线上的 H.264/H.265 Annex B 访问单元,前端用
//!   WebCodecs `VideoDecoder` 硬件解码。必须保序;关键帧额外携带 codec 字符串。
//! - **JPEG**(回退):前端不支持该编码时,由 FFmpeg preview worker 转出的 MJPEG,
//!   每帧自包含,只保留最新帧。
//!
//! 帧头(小端):
//! `magic "UVPJ"(4) version=2(2) header_len(2) kind(1) flags(1) codec_len(2)
//!  generation(8) sequence(8) captured_at_ms(8)` = 36 字节,之后是 `codec_len`
//! 字节的 ASCII codec 字符串(仅关键帧),再之后是载荷。

use media_rtp::{PreviewJpeg, PreviewPhase, PreviewSink, PreviewStatus};

pub const PREVIEW_MAGIC: [u8; 4] = *b"UVPJ";
pub const PREVIEW_VERSION: u16 = 2;
pub const PREVIEW_HEADER_LEN: usize = 36;

/// 载荷类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PreviewPayloadKind {
    Jpeg = 0,
    H264 = 1,
    H265 = 2,
}

impl PreviewPayloadKind {
    pub fn from_codec(codec: media_rtp::VideoCodec) -> Self {
        match codec {
            media_rtp::VideoCodec::H264 => Self::H264,
            media_rtp::VideoCodec::H265 => Self::H265,
        }
    }
}

/// flags bit0:关键帧(编码帧);JPEG 恒为 1。
pub const PREVIEW_FLAG_KEY: u8 = 0x01;

/// 一帧的元数据。
#[derive(Debug, Clone, Copy)]
pub struct PreviewEnvelope<'a> {
    pub kind: PreviewPayloadKind,
    pub key_frame: bool,
    pub generation: u64,
    pub sequence: u64,
    pub captured_at_ms: u64,
    /// 关键帧携带的 WebCodecs codec 字符串,如 `avc1.64001F`。
    pub codec: Option<&'a str>,
}

/// 把一帧打包成前端可直接解析的字节。
pub fn encode_envelope(meta: PreviewEnvelope<'_>, payload: &[u8]) -> Vec<u8> {
    let codec = meta.codec.unwrap_or("").as_bytes();
    let codec_len = codec.len().min(u16::MAX as usize);
    let mut out = Vec::with_capacity(PREVIEW_HEADER_LEN + codec_len + payload.len());
    out.extend_from_slice(&PREVIEW_MAGIC);
    out.extend_from_slice(&PREVIEW_VERSION.to_le_bytes());
    out.extend_from_slice(&(PREVIEW_HEADER_LEN as u16).to_le_bytes());
    out.push(meta.kind as u8);
    out.push(if meta.key_frame { PREVIEW_FLAG_KEY } else { 0 });
    out.extend_from_slice(&(codec_len as u16).to_le_bytes());
    out.extend_from_slice(&meta.generation.to_le_bytes());
    out.extend_from_slice(&meta.sequence.to_le_bytes());
    out.extend_from_slice(&meta.captured_at_ms.to_le_bytes());
    out.extend_from_slice(&codec[..codec_len]);
    out.extend_from_slice(payload);
    out
}

/// JPEG 回退帧。
pub fn encode_preview_frame(frame: &PreviewJpeg) -> Vec<u8> {
    encode_envelope(
        PreviewEnvelope {
            kind: PreviewPayloadKind::Jpeg,
            key_frame: true,
            generation: frame.generation,
            sequence: frame.sequence,
            captured_at_ms: frame.captured_at_ms,
            codec: None,
        },
        &frame.data,
    )
}

#[derive(Debug)]
struct BusOrder {
    generation: u64,
    preview_generation: u64,
    last_sequence: u64,
    phase: PreviewPhase,
}

/// 采集侧到桌面预览的最新帧总线。
///
/// 用 watch 而不是队列：旧的预览帧没有任何价值，保留它只会推高延迟。
/// （H.264 时代必须保序是因为 delta 帧依赖前一帧，MJPEG 没有这个约束。）
pub struct DesktopPreviewBus {
    frames: tokio::sync::watch::Sender<Option<PreviewJpeg>>,
    statuses: tokio::sync::watch::Sender<PreviewStatus>,
    order: std::sync::Mutex<BusOrder>,
    published: std::sync::atomic::AtomicU64,
    active: std::sync::atomic::AtomicBool,
}

impl DesktopPreviewBus {
    pub fn new() -> Self {
        let (frames, _) = tokio::sync::watch::channel(None);
        let initial = PreviewStatus::new(0, PreviewPhase::Idle);
        let (statuses, _) = tokio::sync::watch::channel(initial);
        Self {
            frames,
            statuses,
            order: std::sync::Mutex::new(BusOrder {
                generation: 0,
                preview_generation: 0,
                last_sequence: 0,
                phase: PreviewPhase::Idle,
            }),
            published: std::sync::atomic::AtomicU64::new(0),
            active: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<Option<PreviewJpeg>> {
        self.frames.subscribe()
    }

    pub fn subscribe_status(&self) -> tokio::sync::watch::Receiver<PreviewStatus> {
        self.statuses.subscribe()
    }

    pub fn latest_status(&self) -> PreviewStatus {
        self.statuses.borrow().clone()
    }

    /// 编码帧主路自己发布状态(不经过 JPEG worker)。与 worker 状态共用代际排序。
    pub fn publish_status(&self, status: PreviewStatus) {
        PreviewSink::status(self, status);
    }

    pub fn set_active(&self, active: bool) {
        let Ok(_order) = self.order.lock() else {
            return;
        };
        let previous = self.active.swap(active, std::sync::atomic::Ordering::AcqRel);
        if previous != active {
            tracing::info!(active, "preview visibility demand changed");
        }
        if !active {
            self.frames.send_replace(None);
        }
    }
}

impl PreviewSink for DesktopPreviewBus {
    fn is_active(&self) -> bool {
        self.active.load(std::sync::atomic::Ordering::Acquire)
    }

    fn publish(&self, frame: PreviewJpeg) {
        let Ok(mut order) = self.order.lock() else {
            return;
        };
        if !self.is_active() {
            return;
        }
        if frame.generation < order.generation
            || (frame.generation == order.generation && frame.sequence <= order.last_sequence)
            || (frame.generation == order.generation && order.phase == PreviewPhase::Stopped)
        {
            return;
        }
        if frame.generation > order.generation {
            order.generation = frame.generation;
            order.preview_generation = 0;
            order.phase = PreviewPhase::PreviewStarting;
        }
        order.last_sequence = frame.sequence;
        let published = self
            .published
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if published == 1 || published % 150 == 0 {
            tracing::info!(
                published,
                bytes = frame.data.len(),
                "preview_bus publish jpeg"
            );
        }
        self.frames.send_replace(Some(frame));
    }

    fn status(&self, status: PreviewStatus) {
        let Ok(mut order) = self.order.lock() else {
            return;
        };
        let stale_generation = status.generation < order.generation
            || (status.generation == order.generation
                && status.preview_generation < order.preview_generation);
        let revives_stopped = status.generation == order.generation
            && status.preview_generation == order.preview_generation
            && order.phase == PreviewPhase::Stopped
            && status.phase != PreviewPhase::Stopped;
        if stale_generation || revives_stopped {
            return;
        }
        if status.generation > order.generation
            || status.preview_generation > order.preview_generation
        {
            order.last_sequence = 0;
            self.frames.send_replace(None);
        }
        order.generation = status.generation;
        order.preview_generation = status.preview_generation;
        order.phase = status.phase;
        drop(order);
        self.statuses.send_replace(status);
    }

    fn unavailable(&self, reason: String) {
        tracing::warn!(%reason, "preview_bus unavailable");
    }

    fn stopped(&self) {
        tracing::info!(
            published = self.published.load(std::sync::atomic::Ordering::Relaxed),
            "preview_bus stopped"
        );
        if let Ok(mut order) = self.order.lock() {
            order.phase = PreviewPhase::Stopped;
            self.frames.send_replace(None);
            let mut status = self.statuses.borrow().clone();
            status.generation = order.generation;
            status.preview_generation = order.preview_generation.max(1);
            status.phase = PreviewPhase::Stopped;
            status.reason = None;
            self.statuses.send_replace(status);
        }
        self.published
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(generation: u64, sequence: u64, captured_at_ms: u64) -> PreviewJpeg {
        PreviewJpeg {
            generation,
            sequence,
            data: vec![0xFF, 0xD8, 0xFF, 0xE0, captured_at_ms as u8, 0xFF, 0xD9],
            captured_at_ms,
        }
    }

    fn status(generation: u64, preview_generation: u64, phase: PreviewPhase) -> PreviewStatus {
        let mut status = PreviewStatus::new(generation, phase);
        status.preview_generation = preview_generation;
        status
    }

    #[test]
    fn envelope_v2_jpeg头部与载荷() {
        let encoded = encode_preview_frame(&frame(7, 11, 0x0102));
        let expected_header = [
            b'U', b'V', b'P', b'J', 2, 0, 36, 0, // magic version header_len
            0, PREVIEW_FLAG_KEY, 0, 0, // kind=jpeg flags=key codec_len=0
            7, 0, 0, 0, 0, 0, 0, 0, // generation
            11, 0, 0, 0, 0, 0, 0, 0, // sequence
            2, 1, 0, 0, 0, 0, 0, 0, // captured_at_ms
        ];
        assert_eq!(&encoded[..PREVIEW_HEADER_LEN], &expected_header);
        assert_eq!(&encoded[PREVIEW_HEADER_LEN..], &frame(7, 11, 0x0102).data);
    }

    #[test]
    fn envelope_v2_编码帧关键帧带codec字符串() {
        let au = [0, 0, 0, 1, 0x65, 0x88];
        let encoded = encode_envelope(
            PreviewEnvelope {
                kind: PreviewPayloadKind::H265,
                key_frame: true,
                generation: 3,
                sequence: 1,
                captured_at_ms: 9,
                codec: Some("hvc1.1.6.L120.90"),
            },
            &au,
        );
        assert_eq!(encoded[8], PreviewPayloadKind::H265 as u8);
        assert_eq!(encoded[9], PREVIEW_FLAG_KEY);
        assert_eq!(u16::from_le_bytes([encoded[10], encoded[11]]), 16);
        assert_eq!(&encoded[36..52], b"hvc1.1.6.L120.90");
        assert_eq!(&encoded[52..], &au);

        let delta = encode_envelope(
            PreviewEnvelope {
                kind: PreviewPayloadKind::H264,
                key_frame: false,
                generation: 3,
                sequence: 2,
                captured_at_ms: 10,
                codec: None,
            },
            &au,
        );
        assert_eq!(delta[9], 0);
        assert_eq!(&delta[10..12], &[0, 0]);
        assert_eq!(&delta[36..], &au);
    }

    #[test]
    fn 订阅者只拿到最新帧() {
        let bus = DesktopPreviewBus::new();
        bus.set_active(true);
        let mut receiver = bus.subscribe();
        bus.publish(frame(4, 1, 1));
        bus.publish(frame(4, 2, 2));
        bus.publish(frame(4, 3, 3));

        // MJPEG 每帧独立，跳过中间帧不影响后续画面，只保留最新才是最低延迟。
        assert_eq!(
            receiver
                .borrow_and_update()
                .as_ref()
                .map(|item| item.sequence),
            Some(3),
        );
    }

    #[test]
    fn 不可用状态可被多个新订阅者重复读取() {
        let bus = DesktopPreviewBus::new();
        let mut unavailable = status(9, 1, PreviewPhase::Unavailable);
        unavailable.reason = Some("decoder failed".into());
        bus.status(unavailable);

        let first = bus.subscribe_status();
        let second = bus.subscribe_status();
        assert_eq!(first.borrow().reason.as_deref(), Some("decoder failed"));
        assert_eq!(second.borrow().reason.as_deref(), Some("decoder failed"));
    }

    #[test]
    fn 旧代际帧和状态不能覆盖新代际() {
        let bus = DesktopPreviewBus::new();
        bus.set_active(true);
        bus.status(status(2, 1, PreviewPhase::Playing));
        bus.publish(frame(2, 8, 80));
        bus.status(status(1, 9, PreviewPhase::Unavailable));
        bus.publish(frame(1, 99, 99));

        assert_eq!(bus.latest_status().generation, 2);
        assert_eq!(bus.latest_status().phase, PreviewPhase::Playing);
        assert_eq!(bus.subscribe().borrow().as_ref().unwrap().sequence, 8);
    }

    #[test]
    fn stopped是当前预览代际的终态() {
        let bus = DesktopPreviewBus::new();
        bus.set_active(true);
        bus.status(status(3, 4, PreviewPhase::Playing));
        bus.publish(frame(3, 10, 100));
        bus.stopped();
        bus.publish(frame(3, 11, 110));
        bus.status(status(3, 4, PreviewPhase::Playing));

        assert!(bus.subscribe().borrow().is_none());
        assert_eq!(bus.latest_status().phase, PreviewPhase::Stopped);
    }

    #[test]
    fn 新preview代际可从不可用恢复到playing() {
        let bus = DesktopPreviewBus::new();
        bus.status(status(5, 1, PreviewPhase::Unavailable));
        bus.status(status(5, 2, PreviewPhase::Recovering));
        bus.status(status(5, 2, PreviewPhase::Playing));

        let current = bus.latest_status();
        assert_eq!(current.generation, 5);
        assert_eq!(current.preview_generation, 2);
        assert_eq!(current.phase, PreviewPhase::Playing);
    }

    #[test]
    fn hidden_preview_discards_cached_and_late_jpeg() {
        let bus = DesktopPreviewBus::new();
        let receiver = bus.subscribe();
        assert!(
            !bus.is_active(),
            "internal watch receivers are not visible demand"
        );
        bus.set_active(true);
        bus.publish(frame(7, 1, 10));
        assert!(receiver.borrow().is_some());
        bus.set_active(false);
        bus.publish(frame(7, 2, 20));
        assert!(receiver.borrow().is_none());
        bus.set_active(true);
        assert!(
            receiver.borrow().is_none(),
            "reconnect must wait for a fresh JPEG"
        );
        bus.publish(frame(7, 3, 30));
        assert_eq!(receiver.borrow().as_ref().unwrap().sequence, 3);
    }
}
