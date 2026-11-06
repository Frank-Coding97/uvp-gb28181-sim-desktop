//! 应用级唯一预览转发器(media-pipeline-v2 P4)。
//!
//! 页面 attach 时才订阅共享媒体总线,detach 即退订:**没人看预览时没有任何
//! 预览开销**(没有订阅者、没有 FFmpeg 进程)。
//!
//! - 主路:总线上的 H.264/H.265 访问单元原样经 Tauri Raw Channel 发给前端,
//!   前端 WebCodecs 硬件解码。起播/间断/积压后只从带参数集的关键帧开始送。
//! - 回退:前端报告不支持该编码(`set_fallback(true)`)时,同一订阅改为喂给
//!   FFmpeg preview worker 转 JPEG,经 [`DesktopPreviewBus`] 送出。

use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

use media_rtp::{MediaEvent, PreviewPhase, PreviewStatus, SharedVideoSource, VideoSource};

use crate::preview_channel::{
    encode_envelope, encode_preview_frame, DesktopPreviewBus, PreviewEnvelope, PreviewPayloadKind,
};

pub trait PreviewForwardTarget: Send + Sync {
    /// Tauri Raw Channel 的 send 是非阻塞入队；关闭时返回错误。
    fn send(&self, bytes: Vec<u8>) -> Result<(), ()>;
}

pub trait PreviewEventSink: Send + Sync {
    fn publish_status(&self, status: &PreviewStatus);
}

/// 当前设备共享媒体的订阅入口。设备未启动或采集未就绪时返回 `None`。
pub trait PreviewMediaSource: Send + Sync {
    fn subscribe(&self) -> Pin<Box<dyn Future<Output = Option<PreviewSubscription>> + Send + '_>>;
}

/// 一次订阅:总线读取端 + 采集帧率(JPEG 回退 worker 需要)。
pub struct PreviewSubscription {
    pub source: SharedVideoSource,
    pub fps: u32,
}

#[derive(Clone)]
struct ActiveTarget {
    token: u64,
    target: Arc<dyn PreviewForwardTarget>,
}

/// 设备未就绪时重新订阅的间隔。
const RESUBSCRIBE_INTERVAL: Duration = Duration::from_millis(300);

pub struct PreviewManager {
    bus: Arc<DesktopPreviewBus>,
    media: std::sync::RwLock<Option<Arc<dyn PreviewMediaSource>>>,
    targets: tokio::sync::watch::Sender<Option<ActiveTarget>>,
    next_token: AtomicU64,
    /// 当前 attach 是否走 JPEG 回退。每次新 attach 复位为 false。
    fallback: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    /// JPEG 回退帧与状态事件的转发任务(应用级,常驻)。
    worker: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// 当前 attach 的编码帧转发任务。
    encoded: std::sync::Mutex<Option<EncodedTask>>,
}

struct EncodedTask {
    token: u64,
    stop: Arc<AtomicBool>,
    join: tokio::task::JoinHandle<()>,
}

impl EncodedTask {
    /// 任务会在 await 点被直接 abort,循环末尾的日志走不到,退订日志记在这里。
    fn cancel(self) {
        self.stop.store(true, Ordering::Release);
        self.join.abort();
        tracing::info!(token = self.token, "本地预览订阅结束");
    }
}

impl PreviewManager {
    pub fn new(bus: Arc<DesktopPreviewBus>) -> Self {
        let (targets, _) = tokio::sync::watch::channel(None);
        Self {
            bus,
            media: std::sync::RwLock::new(None),
            targets,
            next_token: AtomicU64::new(1),
            fallback: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
            worker: tokio::sync::Mutex::new(None),
            encoded: std::sync::Mutex::new(None),
        }
    }

    /// 注入共享媒体订阅入口(应用启动时设置一次)。
    pub fn set_media_source(&self, media: Arc<dyn PreviewMediaSource>) {
        *self.media.write().unwrap_or_else(|error| error.into_inner()) = Some(media);
    }

    pub async fn attach(
        &self,
        target: Arc<dyn PreviewForwardTarget>,
        events: Arc<dyn PreviewEventSink>,
    ) -> u64 {
        self.ensure_worker(events).await;
        let token = self.next_token.fetch_add(1, Ordering::Relaxed).max(1);
        // 新页面重新检测解码能力,不继承上一次的回退选择。
        self.fallback.store(false, Ordering::Release);
        let active = ActiveTarget { token, target };
        self.targets.send_if_modified(|slot| {
            *slot = Some(active.clone());
            true
        });
        self.refresh_bus_demand();
        self.spawn_encoded(active);
        token
    }

    /// 旧页面迟到的 detach 不能解除后来页面的 Channel。
    pub fn detach(&self, token: u64) -> bool {
        let detached = self.targets.send_if_modified(|slot| {
            if slot.as_ref().is_some_and(|target| target.token == token) {
                *slot = None;
                true
            } else {
                false
            }
        });
        if detached {
            self.cancel_encoded(Some(token));
            self.refresh_bus_demand();
        }
        detached
    }

    /// 前端不支持当前编码的硬件/软件解码时切到 JPEG 回退(或切回)。
    pub fn set_fallback(&self, fallback: bool) {
        let previous = self.fallback.swap(fallback, Ordering::AcqRel);
        if previous != fallback {
            tracing::info!(fallback, "本地预览解码方式切换");
        }
        self.refresh_bus_demand();
    }

    /// 重建当前预览订阅(不影响采集与平台推流)。
    pub fn restart(&self) -> bool {
        let Some(active) = self.targets.borrow().clone() else {
            return false;
        };
        self.spawn_encoded(active);
        true
    }

    #[cfg(test)]
    pub fn active_token(&self) -> Option<u64> {
        self.targets.borrow().as_ref().map(|target| target.token)
    }

    pub async fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        self.targets.send_if_modified(|slot| {
            *slot = None;
            true
        });
        self.cancel_encoded(None);
        self.refresh_bus_demand();
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

    /// JPEG worker 只在"有页面 + 回退模式"时干活;其余时间它的 supervisor 会结束 FFmpeg。
    fn refresh_bus_demand(&self) {
        let has_target = self.targets.borrow().is_some();
        self.bus
            .set_active(has_target && self.fallback.load(Ordering::Acquire));
    }

    fn cancel_encoded(&self, token: Option<u64>) {
        let mut slot = self.encoded.lock().unwrap_or_else(|error| error.into_inner());
        if token.is_none() || slot.as_ref().map(|task| task.token) == token {
            if let Some(task) = slot.take() {
                task.cancel();
            }
        }
    }

    fn spawn_encoded(&self, active: ActiveTarget) {
        let media = self
            .media
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let Some(media) = media else {
            tracing::warn!("预览未配置媒体源,只转发 JPEG 回退");
            return;
        };
        let stop = Arc::new(AtomicBool::new(false));
        let task_stop = Arc::clone(&stop);
        let bus = Arc::clone(&self.bus);
        let fallback = Arc::clone(&self.fallback);
        let targets = self.targets.clone();
        let token = active.token;
        let join = tokio::spawn(async move {
            forward_encoded(media, bus, targets, active, fallback, task_stop).await;
        });
        let mut slot = self.encoded.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(previous) = slot.replace(EncodedTask { token, stop, join }) {
            previous.cancel();
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

/// 起播/恢复判定:前一帧结束到本帧开始的空档超过这么多倍帧时长,
/// 视为本帧前有缺失(典型:缓存的起播关键帧与之后的直播帧之间)。
const GAP_TOLERANCE_FRAMES: u64 = 2;

/// 编码帧主路:订阅总线,把视频访问单元按序发给前端。
async fn forward_encoded(
    media: Arc<dyn PreviewMediaSource>,
    bus: Arc<DesktopPreviewBus>,
    targets: tokio::sync::watch::Sender<Option<ActiveTarget>>,
    active: ActiveTarget,
    fallback: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) {
    tracing::info!(token = active.token, "本地预览订阅开始");
    while !stop.load(Ordering::Acquire) {
        let Some(subscription) = media.subscribe().await else {
            bus.publish_status(PreviewStatus::new(
                media_rtp::next_capture_generation(),
                PreviewPhase::WaitingRegistration,
            ));
            tokio::time::sleep(RESUBSCRIBE_INTERVAL).await;
            continue;
        };
        let ended = forward_subscription(subscription, &bus, &targets, &active, &fallback, &stop).await;
        if ended == SubscriptionEnd::TargetClosed {
            break;
        }
        // 设备注销或采集重启:旧画面作废,等新的共享媒体。
        bus.publish_status(PreviewStatus::new(
            media_rtp::next_capture_generation(),
            PreviewPhase::Idle,
        ));
        tokio::time::sleep(RESUBSCRIBE_INTERVAL).await;
    }
    tracing::info!(token = active.token, "本地预览订阅结束(页面通道已关闭)");
}

#[derive(Debug, PartialEq, Eq)]
enum SubscriptionEnd {
    MediaStopped,
    TargetClosed,
    Cancelled,
}

async fn forward_subscription(
    subscription: PreviewSubscription,
    bus: &Arc<DesktopPreviewBus>,
    targets: &tokio::sync::watch::Sender<Option<ActiveTarget>>,
    active: &ActiveTarget,
    fallback: &AtomicBool,
    stop: &AtomicBool,
) -> SubscriptionEnd {
    let PreviewSubscription { mut source, fps } = subscription;
    let generation = media_rtp::next_capture_generation();
    bus.publish_status(PreviewStatus::new(generation, PreviewPhase::WaitingKeyframe));
    let mut state = KeyframeGate::default();
    let mut sequence = 0_u64;
    let mut playing = false;
    // JPEG 回退 worker:只在回退模式下按需创建,随订阅一起释放。
    let mut jpeg: Option<JpegFallback> = None;

    loop {
        if stop.load(Ordering::Acquire) {
            return SubscriptionEnd::Cancelled;
        }
        let Some(event) = source.recv_event().await else {
            if !source.is_live() {
                return SubscriptionEnd::MediaStopped;
            }
            continue;
        };
        let video = match event {
            MediaEvent::Video(video) => video,
            MediaEvent::Audio(_) => continue,
            MediaEvent::Discontinuity { .. } => {
                state.discontinuity();
                continue;
            }
        };
        if !state.admit(&video) {
            continue;
        }

        if fallback.load(Ordering::Acquire) {
            let worker = jpeg.get_or_insert_with(|| JpegFallback::spawn(bus, fps, video.codec));
            worker.send(&video);
            continue;
        }
        if jpeg.take().is_some() {
            // 从回退切回编码帧:必须从下一个关键帧重新起播。
            state.discontinuity();
            if !state.admit(&video) {
                continue;
            }
        }

        sequence += 1;
        let codec = video
            .key_frame
            .then(|| media_rtp::codec_string::codec_string(&video.data, video.codec))
            .flatten();
        let bytes = encode_envelope(
            PreviewEnvelope {
                kind: PreviewPayloadKind::from_codec(video.codec),
                key_frame: video.key_frame,
                generation,
                sequence,
                captured_at_ms: now_ms(),
                codec: codec.as_deref(),
            },
            &video.data,
        );
        if !send_or_detach(bus, targets, active.clone(), bytes) {
            return SubscriptionEnd::TargetClosed;
        }
        if !playing {
            playing = true;
            bus.publish_status(PreviewStatus::new(generation, PreviewPhase::Playing));
        }
    }
}

/// 只放行可解码的连续码流:起播、间断、时间轴缺口之后都要等带参数集的关键帧。
#[derive(Debug, Default)]
struct KeyframeGate {
    started: bool,
    last_end_90k: Option<u64>,
    last_duration_90k: u64,
}

impl KeyframeGate {
    fn discontinuity(&mut self) {
        self.started = false;
        self.last_end_90k = None;
    }

    fn admit(&mut self, video: &media_rtp::TimedVideoAu) -> bool {
        let decodable_key =
            video.key_frame && media_rtp::contains_codec_config(&video.data, video.codec);
        // 订阅端先补发缓存的起播关键帧,之后直播帧可能与它隔了大半个 GOP。
        let gap = self.last_end_90k.is_some_and(|end| {
            video.pts_90k
                > end.saturating_add(self.last_duration_90k.max(1) * GAP_TOLERANCE_FRAMES)
        });
        if gap {
            self.started = false;
        }
        self.last_end_90k = Some(video.pts_90k.saturating_add(video.duration_90k));
        self.last_duration_90k = video.duration_90k;
        if !self.started && !decodable_key {
            return false;
        }
        self.started = true;
        true
    }
}

/// JPEG 回退:把订阅到的访问单元交给 FFmpeg preview worker。
struct JpegFallback {
    generation: u64,
    sequence: u64,
    input: media_rtp::PreviewWorkerInput,
    _handle: media_rtp::PreviewWorkerHandle,
}

impl JpegFallback {
    fn spawn(bus: &Arc<DesktopPreviewBus>, fps: u32, codec: media_rtp::VideoCodec) -> Self {
        let generation = media_rtp::next_capture_generation();
        let sink: Arc<dyn media_rtp::PreviewSink> = Arc::clone(bus) as _;
        let (input, handle) = media_rtp::spawn_preview_worker_with_codec(
            media_rtp::ffmpeg_bin().unwrap_or_else(|| "ffmpeg".into()),
            fps.max(1),
            generation,
            codec,
            sink,
        );
        tracing::info!(?codec, "本地预览改用 JPEG 回退");
        Self {
            generation,
            sequence: 0,
            input,
            _handle: handle,
        }
    }

    fn send(&mut self, video: &media_rtp::TimedVideoAu) {
        self.sequence += 1;
        let _ = self.input.try_send(media_rtp::CapturedAccessUnit::with_codec(
            self.generation,
            self.sequence,
            video.data.to_vec(),
            video.key_frame,
            now_ms(),
            video.codec,
        ));
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// 应用级常驻任务:转发预览状态事件,以及回退模式下的 JPEG 帧。
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
    tracing::info!("preview manager worker started");

    while !stop.load(Ordering::Acquire) {
        tokio::select! {
            changed = frames.changed() => {
                if changed.is_err() {
                    break;
                }
                let target = { targets.borrow().clone() };
                let frame = frames.borrow_and_update().clone();
                if let (Some(frame), Some(target)) = (frame, target) {
                    send_or_detach(&bus, &targets, target, encode_preview_frame(&frame));
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
                target_changes.borrow_and_update();
                events.publish_status(&statuses.borrow().clone());
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
    tracing::info!("preview manager worker stopped");
}

/// 发送失败(页面 Channel 已关闭)时解除该目标。返回是否发送成功。
fn send_or_detach(
    bus: &DesktopPreviewBus,
    targets: &tokio::sync::watch::Sender<Option<ActiveTarget>>,
    active: ActiveTarget,
    bytes: Vec<u8>,
) -> bool {
    if active.target.send(bytes).is_ok() {
        return true;
    }
    targets.send_if_modified(|slot| {
        if slot
            .as_ref()
            .is_some_and(|target| target.token == active.token)
        {
            *slot = None;
            bus.set_active(false);
            true
        } else {
            false
        }
    });
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use media_rtp::{PreviewSink, SharedMedia, TimedVideoAu};
    use std::sync::{mpsc, Mutex};

    #[derive(Default)]
    struct FakeTarget {
        frames: Mutex<Vec<Vec<u8>>>,
        closed: AtomicBool,
    }

    impl PreviewForwardTarget for FakeTarget {
        fn send(&self, bytes: Vec<u8>) -> Result<(), ()> {
            if self.closed.load(Ordering::Acquire) {
                return Err(());
            }
            self.frames.lock().unwrap().push(bytes);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeEvents;
    impl PreviewEventSink for FakeEvents {
        fn publish_status(&self, _status: &PreviewStatus) {}
    }

    /// 由测试推事件的实时源。
    struct PushedSource(Mutex<mpsc::Receiver<MediaEvent>>);
    impl VideoSource for PushedSource {
        fn next_frame(&mut self) -> Option<media_rtp::Frame> {
            None
        }
        fn supports_timed_events(&self) -> bool {
            true
        }
        fn timed_events_are_paced(&self) -> bool {
            true
        }
        fn is_live(&self) -> bool {
            true
        }
        fn next_media_event(&mut self) -> Option<MediaEvent> {
            self.0.lock().unwrap().try_recv().ok()
        }
        fn wait_event_blocking(&mut self, timeout: Duration) -> Option<MediaEvent> {
            self.0.lock().unwrap().recv_timeout(timeout).ok()
        }
    }

    struct FakeMedia {
        media: Arc<SharedMedia>,
        subscribes: AtomicU64,
    }

    impl PreviewMediaSource for FakeMedia {
        fn subscribe(
            &self,
        ) -> Pin<Box<dyn Future<Output = Option<PreviewSubscription>> + Send + '_>> {
            self.subscribes.fetch_add(1, Ordering::Relaxed);
            let source = self.media.subscribe();
            Box::pin(async move { Some(PreviewSubscription { source, fps: 25 }) })
        }
    }

    const KEY: [u8; 21] = [
        0, 0, 0, 1, 0x67, 0x64, 0x00, 0x1F, 0xAC, 0, 0, 0, 1, 0x68, 0xEB, 0, 0, 0, 1, 0x65, 0x88,
    ];
    const DELTA: [u8; 6] = [0, 0, 0, 1, 0x41, 0x9A];

    fn au(data: &[u8], key: bool, index: u64) -> MediaEvent {
        MediaEvent::Video(TimedVideoAu::new(
            data.to_vec(),
            media_rtp::VideoCodec::H264,
            key,
            index * 3_600,
            3_600,
        ))
    }

    fn setup() -> (mpsc::Sender<MediaEvent>, Arc<FakeMedia>, PreviewManager, Arc<DesktopPreviewBus>) {
        let (tx, rx) = mpsc::channel();
        let shared = media_rtp::start_shared_media(Box::new(PushedSource(Mutex::new(rx))), 25);
        let media = Arc::new(FakeMedia {
            media: shared,
            subscribes: AtomicU64::new(0),
        });
        let bus = Arc::new(DesktopPreviewBus::new());
        let manager = PreviewManager::new(Arc::clone(&bus));
        manager.set_media_source(Arc::clone(&media) as Arc<dyn PreviewMediaSource>);
        (tx, media, manager, bus)
    }

    /// (kind, key, codec, sequence)
    fn decode(frame: &[u8]) -> (u8, bool, String, u64) {
        let codec_len = u16::from_le_bytes([frame[10], frame[11]]) as usize;
        let codec = String::from_utf8(frame[36..36 + codec_len].to_vec()).unwrap();
        let sequence = u64::from_le_bytes(frame[20..28].try_into().unwrap());
        (frame[8], frame[9] & 1 == 1, codec, sequence)
    }

    async fn wait_frames(target: &FakeTarget, count: usize) -> Vec<Vec<u8>> {
        for _ in 0..200 {
            if target.frames.lock().unwrap().len() >= count {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        target.frames.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn 没有页面时不订阅_attach后先发缓存关键帧_缺口后等下一个关键帧() {
        let (tx, media, manager, _bus) = setup();
        tx.send(au(&KEY, true, 0)).unwrap();
        for index in 1..=5 {
            tx.send(au(&DELTA, false, index)).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(media.subscribes.load(Ordering::Relaxed), 0, "没人预览时不应订阅");

        let target = Arc::new(FakeTarget::default());
        let token = manager.attach(target.clone(), Arc::new(FakeEvents)).await;
        let first = wait_frames(&target, 1).await;
        assert_eq!(decode(&first[0]), (1, true, "avc1.64001F".into(), 1), "起播用缓存关键帧");

        // 与起播关键帧隔了 5 帧的 delta 依赖缺失的参考帧,必须丢掉。
        tx.send(au(&DELTA, false, 6)).unwrap();
        tx.send(au(&KEY, true, 7)).unwrap();
        tx.send(au(&DELTA, false, 8)).unwrap();
        let frames = wait_frames(&target, 3).await;
        let decoded: Vec<_> = frames.iter().map(|frame| decode(frame)).collect();
        assert_eq!(
            decoded,
            vec![
                (1, true, "avc1.64001F".into(), 1),
                (1, true, "avc1.64001F".into(), 2),
                (1, false, String::new(), 3),
            ]
        );

        assert!(manager.detach(token));
        let subscribes = media.subscribes.load(Ordering::Relaxed);
        tx.send(au(&DELTA, false, 9)).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(target.frames.lock().unwrap().len(), 3, "detach 后不再发送");
        assert_eq!(media.subscribes.load(Ordering::Relaxed), subscribes);
        manager.shutdown().await;
        media.media.stop();
    }

    #[tokio::test]
    async fn 回退模式才激活jpeg总线_新attach复位() {
        let (_tx, media, manager, bus) = setup();
        let token = manager
            .attach(Arc::new(FakeTarget::default()), Arc::new(FakeEvents))
            .await;
        assert!(!bus.is_active(), "编码帧主路不需要 JPEG worker");
        manager.set_fallback(true);
        assert!(bus.is_active());
        assert!(manager.detach(token));
        assert!(!bus.is_active());
        manager
            .attach(Arc::new(FakeTarget::default()), Arc::new(FakeEvents))
            .await;
        assert!(!bus.is_active(), "新页面重新检测解码能力");
        manager.shutdown().await;
        media.media.stop();
    }

    #[tokio::test]
    async fn 重复attach替换目标且旧token不能detach新目标() {
        let (tx, media, manager, _bus) = setup();
        tx.send(au(&KEY, true, 0)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let first = Arc::new(FakeTarget::default());
        let second = Arc::new(FakeTarget::default());
        let first_token = manager.attach(first.clone(), Arc::new(FakeEvents)).await;
        let second_token = manager.attach(second.clone(), Arc::new(FakeEvents)).await;
        assert_ne!(first_token, second_token);
        assert!(!manager.detach(first_token));
        assert_eq!(manager.active_token(), Some(second_token));
        assert!(!wait_frames(&second, 1).await.is_empty());
        let first_count = first.frames.lock().unwrap().len();
        tx.send(au(&KEY, true, 1)).unwrap();
        wait_frames(&second, 2).await;
        assert_eq!(first.frames.lock().unwrap().len(), first_count, "旧页面不再收帧");
        manager.shutdown().await;
        media.media.stop();
    }

    #[tokio::test]
    async fn channel关闭后解除目标() {
        let (tx, media, manager, _bus) = setup();
        let closed = Arc::new(FakeTarget::default());
        closed.closed.store(true, Ordering::Release);
        manager.attach(closed, Arc::new(FakeEvents)).await;
        tx.send(au(&KEY, true, 0)).unwrap();
        for _ in 0..100 {
            if manager.active_token().is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(manager.active_token(), None);
        manager.shutdown().await;
        media.media.stop();
    }

    #[test]
    fn 关键帧门_间断后丢delta() {
        let mut gate = KeyframeGate::default();
        let video = |data: &[u8], key, index| match au(data, key, index) {
            MediaEvent::Video(video) => video,
            _ => unreachable!(),
        };
        assert!(!gate.admit(&video(&DELTA, false, 0)), "起播前 delta 丢弃");
        assert!(gate.admit(&video(&KEY, true, 1)));
        assert!(gate.admit(&video(&DELTA, false, 2)));
        gate.discontinuity();
        assert!(!gate.admit(&video(&DELTA, false, 3)));
        assert!(gate.admit(&video(&KEY, true, 4)));
        // 无参数集的 IDR 不能作为起播点。
        gate.discontinuity();
        assert!(!gate.admit(&video(&[0, 0, 0, 1, 0x65, 0x88], true, 5)));
    }
}
