use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

use media_rtp::PreviewStatus;

use crate::preview_channel::{encode_preview_frame, DesktopPreviewBus};

pub trait PreviewForwardTarget: Send + Sync {
    /// Tauri Raw Channel 的 send 是非阻塞入队；关闭时返回错误。
    fn send(&self, bytes: Vec<u8>) -> Result<(), ()>;
}

pub trait PreviewEventSink: Send + Sync {
    fn publish_status(&self, status: &PreviewStatus);
}

#[derive(Clone)]
struct ActiveTarget {
    token: u64,
    target: Arc<dyn PreviewForwardTarget>,
}

/// 应用级唯一预览转发器。页面 attach 只替换渲染目标，不拥有媒体采集。
pub struct PreviewManager {
    bus: Arc<DesktopPreviewBus>,
    targets: tokio::sync::watch::Sender<Option<ActiveTarget>>,
    next_token: AtomicU64,
    stop: Arc<AtomicBool>,
    worker: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl PreviewManager {
    pub fn new(bus: Arc<DesktopPreviewBus>) -> Self {
        let (targets, _) = tokio::sync::watch::channel(None);
        Self {
            bus,
            targets,
            next_token: AtomicU64::new(1),
            stop: Arc::new(AtomicBool::new(false)),
            worker: tokio::sync::Mutex::new(None),
        }
    }

    pub async fn attach(
        &self,
        target: Arc<dyn PreviewForwardTarget>,
        events: Arc<dyn PreviewEventSink>,
    ) -> u64 {
        self.ensure_worker(events).await;
        let token = self.next_token.fetch_add(1, Ordering::Relaxed).max(1);
        self.targets
            .send_replace(Some(ActiveTarget { token, target }));
        token
    }

    /// 旧页面迟到的 detach 不能解除后来页面的 Channel。
    pub fn detach(&self, token: u64) -> bool {
        self.targets.send_if_modified(|slot| {
            if slot.as_ref().is_some_and(|target| target.token == token) {
                *slot = None;
                true
            } else {
                false
            }
        })
    }

    #[cfg(test)]
    pub fn active_token(&self) -> Option<u64> {
        self.targets.borrow().as_ref().map(|target| target.token)
    }

    pub async fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        self.targets.send_replace(None);
        let worker = self.worker.lock().await.take();
        if let Some(mut worker) = worker {
            if tokio::time::timeout(Duration::from_millis(600), &mut worker)
                .await
                .is_err()
            {
                tracing::warn!("preview manager did not stop within 600ms");
                worker.abort();
                let _ = worker.await;
            }
        }
    }

    async fn ensure_worker(&self, events: Arc<dyn PreviewEventSink>) {
        let mut guard = self.worker.lock().await;
        if guard.as_ref().is_some_and(|worker| !worker.is_finished()) {
            return;
        }
        *guard = None;
        self.stop.store(false, Ordering::Release);
        let bus = Arc::clone(&self.bus);
        let targets = self.targets.clone();
        let stop = Arc::clone(&self.stop);
        *guard = Some(tokio::spawn(async move {
            forward_preview(bus, targets, stop, events).await;
        }));
    }
}

async fn forward_preview(
    bus: Arc<DesktopPreviewBus>,
    targets: tokio::sync::watch::Sender<Option<ActiveTarget>>,
    stop: Arc<AtomicBool>,
    events: Arc<dyn PreviewEventSink>,
) {
    let mut frames = bus.subscribe();
    let mut statuses = bus.subscribe_status();
    let mut target_changes = targets.subscribe();
    events.publish_status(&statuses.borrow().clone());
    let initial_target = { targets.borrow().clone() };
    let initial_frame = { frames.borrow().clone() };
    if let (Some(target), Some(frame)) = (initial_target, initial_frame) {
        send_or_detach(&targets, target, encode_preview_frame(&frame));
    }
    tracing::info!("preview manager worker started");

    while !stop.load(Ordering::Acquire) {
        tokio::select! {
            changed = frames.changed() => {
                if changed.is_err() {
                    break;
                }
                let frame = frames.borrow_and_update().clone();
                let target = { targets.borrow().clone() };
                if let (Some(frame), Some(target)) = (frame, target) {
                    send_or_detach(&targets, target, encode_preview_frame(&frame));
                }
            }
            changed = statuses.changed() => {
                if changed.is_err() {
                    break;
                }
                events.publish_status(&statuses.borrow_and_update().clone());
            }
            changed = target_changes.changed() => {
                if changed.is_err() {
                    break;
                }
                let target = target_changes.borrow_and_update().clone();
                events.publish_status(&statuses.borrow().clone());
                if let (Some(target), Some(frame)) = (target, frames.borrow().clone()) {
                    send_or_detach(&targets, target, encode_preview_frame(&frame));
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
    tracing::info!("preview manager worker stopped");
}

fn send_or_detach(
    targets: &tokio::sync::watch::Sender<Option<ActiveTarget>>,
    active: ActiveTarget,
    bytes: Vec<u8>,
) {
    if active.target.send(bytes).is_ok() {
        return;
    }
    targets.send_if_modified(|slot| {
        if slot
            .as_ref()
            .is_some_and(|target| target.token == active.token)
        {
            *slot = None;
            true
        } else {
            false
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use media_rtp::{PreviewJpeg, PreviewSink};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakeTarget {
        frames: Mutex<Vec<Vec<u8>>>,
        closed: std::sync::atomic::AtomicBool,
    }

    impl PreviewForwardTarget for FakeTarget {
        fn send(&self, bytes: Vec<u8>) -> Result<(), ()> {
            if self.closed.load(std::sync::atomic::Ordering::Acquire) {
                return Err(());
            }
            self.frames.lock().unwrap().push(bytes);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeEvents;
    impl PreviewEventSink for FakeEvents {
        fn publish_status(&self, _status: &media_rtp::PreviewStatus) {}
    }

    fn frame(sequence: u64) -> PreviewJpeg {
        PreviewJpeg {
            generation: 7,
            sequence,
            captured_at_ms: sequence,
            data: vec![0xff, 0xd8, 0xff, sequence as u8, 0xff, 0xd9],
        }
    }

    #[tokio::test]
    async fn 重复attach替换目标且旧token不能detach新目标() {
        let bus = Arc::new(crate::preview_channel::DesktopPreviewBus::new());
        let manager = PreviewManager::new(bus.clone());
        let first = Arc::new(FakeTarget::default());
        let second = Arc::new(FakeTarget::default());
        let events = Arc::new(FakeEvents);

        let first_token = manager.attach(first.clone(), events.clone()).await;
        let second_token = manager.attach(second.clone(), events).await;
        assert_ne!(first_token, second_token);
        assert!(!manager.detach(first_token));
        bus.publish(frame(1));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(first.frames.lock().unwrap().is_empty());
        assert_eq!(second.frames.lock().unwrap().len(), 1);
        assert!(manager.detach(second_token));
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn channel关闭只detach且重新attach继续收到帧() {
        let bus = Arc::new(crate::preview_channel::DesktopPreviewBus::new());
        let manager = PreviewManager::new(bus.clone());
        let closed = Arc::new(FakeTarget::default());
        closed
            .closed
            .store(true, std::sync::atomic::Ordering::Release);
        manager.attach(closed, Arc::new(FakeEvents)).await;
        bus.publish(frame(1));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(manager.active_token(), None);

        let replacement = Arc::new(FakeTarget::default());
        manager
            .attach(replacement.clone(), Arc::new(FakeEvents))
            .await;
        bus.publish(frame(2));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!replacement.frames.lock().unwrap().is_empty());
        manager.shutdown().await;
    }
}
