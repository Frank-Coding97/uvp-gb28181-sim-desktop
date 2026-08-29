//! 预览帧通道。
//!
//! 预览走的是采集侧 FFmpeg 旁路出来的 MJPEG：**每帧自包含，没有帧间依赖**。
//! 这一条性质消掉了此前整套复杂度——不需要保序、不需要重同步、不需要参数集
//! 关键帧、不需要逐帧 ACK、不需要消费进度反馈。丢中间帧的代价就是丢一帧。
//!
//! 因此这里只保留最新帧：它同时是最低延迟和最简单的策略。

use media_rtp::{PreviewJpeg, PreviewPhase, PreviewSink, PreviewStatus};

pub const PREVIEW_MAGIC: [u8; 4] = *b"UVPJ";
pub const PREVIEW_VERSION: u16 = 1;
/// `magic(4) + version(2) + header_len(2) + generation(8) + sequence(8) + captured_at_ms(8)`。
pub const PREVIEW_HEADER_LEN: usize = 32;

/// 把一帧打包成前端可直接解析的字节。
pub fn encode_preview_frame(frame: &PreviewJpeg) -> Vec<u8> {
    let mut out = Vec::with_capacity(PREVIEW_HEADER_LEN + frame.data.len());
    out.extend_from_slice(&PREVIEW_MAGIC);
    out.extend_from_slice(&PREVIEW_VERSION.to_le_bytes());
    out.extend_from_slice(&(PREVIEW_HEADER_LEN as u16).to_le_bytes());
    out.extend_from_slice(&frame.generation.to_le_bytes());
    out.extend_from_slice(&frame.sequence.to_le_bytes());
    out.extend_from_slice(&frame.captured_at_ms.to_le_bytes());
    out.extend_from_slice(&frame.data);
    out
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
}

impl PreviewSink for DesktopPreviewBus {
    fn publish(&self, frame: PreviewJpeg) {
        let Ok(mut order) = self.order.lock() else {
            return;
        };
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
        drop(order);

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
        self.frames.send_replace(None);
        if let Ok(mut order) = self.order.lock() {
            order.phase = PreviewPhase::Stopped;
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
    fn envelope_头部是小端采集时刻_其后原样是jpeg字节() {
        let encoded = encode_preview_frame(&frame(7, 11, 0x0102));
        let expected_header = [
            b'U', b'V', b'P', b'J', 1, 0, 32, 0, 7, 0, 0, 0, 0, 0, 0, 0, 11, 0, 0, 0, 0, 0, 0, 0,
            2, 1, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(&encoded[..PREVIEW_HEADER_LEN], &expected_header);
        assert_eq!(&encoded[PREVIEW_HEADER_LEN..], &frame(7, 11, 0x0102).data);
    }

    #[test]
    fn 订阅者只拿到最新帧() {
        let bus = DesktopPreviewBus::new();
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
}
