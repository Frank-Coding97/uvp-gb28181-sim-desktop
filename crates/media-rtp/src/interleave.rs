//! 音视频交织:PS 复用前按 PTS 重新排序。
//!
//! 实时采集的视频和音频来自不同管道/回调,到达顺序与采集时刻不完全一致。
//! 直接按到达顺序复用会让 PS 里的 PTS 来回跳,部分平台据此判断音画不同步
//! 或丢包。[`InterleavedSource`] 包装一个定时事件源,保留一个很小的窗口
//! (默认 60ms),窗口内的事件按 PTS 输出;超出窗口的事件立即放行。
//!
//! 规则:
//! - 事件 PTS 落后已见最大 PTS 超过窗口 → 视为就绪,按 PTS 从小到大输出;
//! - `Discontinuity` 先冲刷缓冲区,再原样透传,并重置窗口;
//! - 内部源暂时无事件、或缓冲最老事件已在墙钟上等待超过两倍窗口 → 全部冲刷,
//!   保证静音/画面停顿时不会无限期压住已到达的数据;
//! - seek 时丢弃缓冲。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::source::{EventFuture, Frame, MediaEvent, VideoSource};

/// 默认交织窗口:60ms(90kHz)。
pub const DEFAULT_WINDOW_90K: u64 = 90_000 * 60 / 1000;

/// 按 PTS 排序的小窗口缓冲。纯逻辑,不涉及 IO,便于单测。
#[derive(Debug)]
pub struct Interleaver {
    window_90k: u64,
    /// 按 PTS 升序;同 PTS 时视频在前(先给解码器参考帧)。
    buffer: VecDeque<MediaEvent>,
    /// 已见最大 PTS,决定哪些事件已就绪。
    max_seen_pts: Option<u64>,
    /// 缓冲中最老事件进入的墙钟时刻,用于停顿兜底冲刷。
    oldest_since: Option<Instant>,
    /// 排好序待输出的事件(冲刷结果)。
    ready: VecDeque<MediaEvent>,
}

impl Interleaver {
    pub fn new(window_90k: u64) -> Self {
        Self {
            window_90k,
            buffer: VecDeque::new(),
            max_seen_pts: None,
            oldest_since: None,
            ready: VecDeque::new(),
        }
    }

    /// 墙钟兜底:缓冲最老事件等待超过两倍窗口即冲刷。
    fn wall_limit(&self) -> Duration {
        Duration::from_micros(self.window_90k.saturating_mul(2_000_000) / 90_000)
    }

    fn sort_key(event: &MediaEvent) -> (u64, u8) {
        let rank = match event {
            MediaEvent::Video(_) => 0,
            MediaEvent::Audio(_) => 1,
            MediaEvent::Discontinuity { .. } => 2,
        };
        (event.pts_90k(), rank)
    }

    /// 放入一个事件。
    pub fn push(&mut self, event: MediaEvent) {
        if matches!(event, MediaEvent::Discontinuity { .. }) {
            self.flush();
            self.ready.push_back(event);
            self.max_seen_pts = None;
            return;
        }
        let pts = event.pts_90k();
        self.max_seen_pts = Some(self.max_seen_pts.map_or(pts, |max| max.max(pts)));
        let key = Self::sort_key(&event);
        let index = self
            .buffer
            .iter()
            .rposition(|queued| Self::sort_key(queued) <= key)
            .map_or(0, |index| index + 1);
        self.buffer.insert(index, event);
        if self.oldest_since.is_none() {
            self.oldest_since = Some(Instant::now());
        }
        self.release_ready();
    }

    /// 把落在窗口之外的事件移到输出队列。
    fn release_ready(&mut self) {
        let Some(max) = self.max_seen_pts else {
            return;
        };
        let mut released = false;
        // 落后最大 PTS 至少一个窗口才放行;不能用 saturating_sub,否则开头
        // max < window 时阈值为 0,PTS=0 的事件会被立刻放行。
        while self
            .buffer
            .front()
            .is_some_and(|event| event.pts_90k().saturating_add(self.window_90k) <= max)
        {
            if let Some(event) = self.buffer.pop_front() {
                self.ready.push_back(event);
                released = true;
            }
        }
        if self.buffer.is_empty() {
            self.oldest_since = None;
        } else if released {
            // 新的队首事件从现在开始计等待时长。
            self.oldest_since = Some(Instant::now());
        }
    }

    /// 停顿兜底:最老事件已在墙钟上等太久时全部冲刷。
    pub fn release_stale(&mut self) {
        if self
            .oldest_since
            .is_some_and(|since| since.elapsed() >= self.wall_limit())
        {
            self.flush();
        }
    }

    /// 把缓冲全部按序移到输出队列。
    pub fn flush(&mut self) {
        self.ready.extend(self.buffer.drain(..));
        self.oldest_since = None;
    }

    /// 取一个已就绪事件。
    pub fn pop(&mut self) -> Option<MediaEvent> {
        self.ready.pop_front()
    }

    /// 丢弃所有缓冲(seek)。
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.ready.clear();
        self.max_seen_pts = None;
        self.oldest_since = None;
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty() && self.ready.is_empty()
    }
}

/// 给定时事件源加上交织窗口。其余能力全部透传。
pub struct InterleavedSource {
    inner: Box<dyn VideoSource>,
    interleaver: Interleaver,
}

impl InterleavedSource {
    pub fn new(inner: Box<dyn VideoSource>, window_90k: u64) -> Self {
        Self {
            inner,
            interleaver: Interleaver::new(window_90k),
        }
    }

    /// 内部源暂无事件时的处理:有限源结束或停顿超时则冲刷,返回一个就绪事件。
    fn on_inner_idle(&mut self) -> Option<MediaEvent> {
        if !self.inner.is_live() {
            self.interleaver.flush();
        } else {
            self.interleaver.release_stale();
        }
        self.interleaver.pop()
    }
}

impl VideoSource for InterleavedSource {
    fn next_frame(&mut self) -> Option<Frame> {
        self.inner.next_frame()
    }

    fn next_audio(&mut self) -> Vec<Vec<u8>> {
        self.inner.next_audio()
    }

    fn supports_timed_events(&self) -> bool {
        self.inner.supports_timed_events()
    }

    fn timed_events_are_paced(&self) -> bool {
        self.inner.timed_events_are_paced()
    }

    fn next_media_event(&mut self) -> Option<MediaEvent> {
        loop {
            if let Some(event) = self.interleaver.pop() {
                return Some(event);
            }
            match self.inner.next_media_event() {
                Some(event) => self.interleaver.push(event),
                None => return self.on_inner_idle(),
            }
        }
    }

    fn has_async_events(&self) -> bool {
        self.inner.has_async_events()
    }

    fn recv_event(&mut self) -> EventFuture<'_> {
        Box::pin(async move {
            loop {
                if let Some(event) = self.interleaver.pop() {
                    return Some(event);
                }
                // 缓冲非空时只等一个窗口,到点按墙钟兜底放行,避免被内部 100ms 上限拖慢。
                let event = if self.interleaver.is_empty() {
                    self.inner.recv_event().await
                } else {
                    let wait = self.interleaver.wall_limit();
                    tokio::time::timeout(wait, self.inner.recv_event())
                        .await
                        .ok()
                        .flatten()
                };
                match event {
                    Some(event) => self.interleaver.push(event),
                    None => return self.on_inner_idle(),
                }
            }
        })
    }

    fn wait_event_blocking(&mut self, timeout: Duration) -> Option<MediaEvent> {
        loop {
            if let Some(event) = self.interleaver.pop() {
                return Some(event);
            }
            match self.inner.wait_event_blocking(timeout) {
                Some(event) => self.interleaver.push(event),
                None => return self.on_inner_idle(),
            }
        }
    }

    fn audio_sample_rate_hz(&self) -> u32 {
        self.inner.audio_sample_rate_hz()
    }

    fn has_audio(&self) -> bool {
        self.inner.has_audio()
    }

    fn audio_codec(&self) -> crate::ps::AudioCodec {
        self.inner.audio_codec()
    }

    fn codec(&self) -> crate::ps::VideoCodec {
        self.inner.codec()
    }

    fn seek(&mut self, permille: u32) {
        self.interleaver.clear();
        self.inner.seek(permille);
    }

    fn take_buffered(&mut self) -> Vec<MediaEvent> {
        let mut events = self.inner.take_buffered();
        for event in events.drain(..) {
            self.interleaver.push(event);
        }
        self.interleaver.flush();
        std::iter::from_fn(|| self.interleaver.pop()).collect()
    }

    fn is_live(&self) -> bool {
        self.inner.is_live()
    }

    fn take_error(&mut self) -> Option<String> {
        self.inner.take_error()
    }

    fn preview_control(&self) -> Option<std::sync::Arc<crate::preview_worker::PreviewControl>> {
        self.inner.preview_control()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ps::{AudioCodec, VideoCodec};
    use crate::source::{TimedAudioAu, TimedVideoAu};

    fn video(pts: u64) -> MediaEvent {
        MediaEvent::Video(TimedVideoAu::new(
            vec![0, 0, 0, 1, 0x61],
            VideoCodec::H264,
            false,
            pts,
            3_600,
        ))
    }

    fn audio(pts: u64) -> MediaEvent {
        MediaEvent::Audio(TimedAudioAu::from_samples(
            vec![0xD5; 160],
            AudioCodec::G711A,
            8_000,
            160,
            pts,
        ))
    }

    fn drain(il: &mut Interleaver) -> Vec<(char, u64)> {
        std::iter::from_fn(|| il.pop())
            .map(|event| {
                let kind = match event {
                    MediaEvent::Video(_) => 'v',
                    MediaEvent::Audio(_) => 'a',
                    MediaEvent::Discontinuity { .. } => 'd',
                };
                (kind, event.pts_90k())
            })
            .collect()
    }

    #[test]
    fn 窗口内乱序到达按pts输出() {
        let mut il = Interleaver::new(5_400);
        // 视频管道比音频快:先到 v@3600,再补到 a@0、a@1800。
        il.push(video(0));
        il.push(video(3_600));
        il.push(audio(0));
        il.push(audio(1_800));
        assert!(drain(&mut il).is_empty(), "窗口未推进前不应放行");
        il.push(video(7_200));
        il.push(audio(3_600));
        // max=7200,阈值 1800:v0、a0、a1800 就绪,同 PTS 视频在前。
        assert_eq!(drain(&mut il), vec![('v', 0), ('a', 0), ('a', 1_800)]);
        il.flush();
        assert_eq!(
            drain(&mut il),
            vec![('v', 3_600), ('a', 3_600), ('v', 7_200)]
        );
    }

    #[test]
    fn 间断先冲刷缓冲再透传并重置窗口() {
        let mut il = Interleaver::new(5_400);
        il.push(video(900_000));
        il.push(audio(900_000));
        il.push(MediaEvent::Discontinuity { pts_90k: 0 });
        assert_eq!(
            drain(&mut il),
            vec![('v', 900_000), ('a', 900_000), ('d', 0)]
        );
        // 间断后时间轴回到 0,不能被旧的 max_seen 立刻放行。
        il.push(video(0));
        assert!(drain(&mut il).is_empty());
    }

    #[test]
    fn 停顿超过墙钟上限时全部放行() {
        let mut il = Interleaver::new(90); // 1ms 窗口 → 2ms 墙钟上限
        il.push(video(0));
        il.release_stale();
        std::thread::sleep(Duration::from_millis(5));
        il.release_stale();
        assert_eq!(drain(&mut il), vec![('v', 0)]);
    }

    #[test]
    fn 有限源结束时冲刷剩余事件() {
        struct Finite(VecDeque<MediaEvent>);
        impl VideoSource for Finite {
            fn next_frame(&mut self) -> Option<Frame> {
                None
            }
            fn supports_timed_events(&self) -> bool {
                true
            }
            fn next_media_event(&mut self) -> Option<MediaEvent> {
                self.0.pop_front()
            }
        }
        let inner = Finite(VecDeque::from([video(3_600), audio(0), audio(1_800)]));
        let mut source = InterleavedSource::new(Box::new(inner), DEFAULT_WINDOW_90K);
        let got: Vec<u64> = std::iter::from_fn(|| source.next_media_event())
            .map(|event| event.pts_90k())
            .collect();
        assert_eq!(got, vec![0, 1_800, 3_600]);
    }
}
