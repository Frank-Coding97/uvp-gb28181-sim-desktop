//! 把文件等已有 `VideoSource` 接到本地预览旁路。
//!
//! `PreviewingSource` 只读取一次源事件：主路拿到原始 `MediaEvent`，预览 worker
//! 拿到其中视频 AU 的副本。预览队列始终走 `try_send`，因此解码器变慢不会阻塞
//! 文件回放或音视频上行。

use std::sync::Arc;

use common::{Error, Result};

use crate::preview::{next_capture_generation, CapturedAccessUnit, PreviewSink};
use crate::preview_worker::{
    spawn_preview_worker_with_codec, PreviewControl, PreviewWorkerHandle, PreviewWorkerInput,
};
use crate::source::{Frame, MediaEvent, VideoSource};

/// 为一个已有视频源增加独立的本地 JPEG 预览旁路。
///
/// 该类型只应包装没有自带预览 worker 的源（例如 `PreparedFileSource`）。摄像头和
/// 屏幕源已经在自己的单一采集进程旁路预览，再包一层会造成重复预览，构造时会拒绝。
pub struct PreviewingSource {
    source: Box<dyn VideoSource>,
    preview_input: PreviewWorkerInput,
    preview_worker: PreviewWorkerHandle,
    generation: u64,
    sequence: u64,
}

impl PreviewingSource {
    /// 使用应用当前解析到的 FFmpeg 启动文件预览旁路。
    pub fn new(source: Box<dyn VideoSource>, fps: u32, sink: Arc<dyn PreviewSink>) -> Result<Self> {
        if source.preview_control().is_some() {
            return Err(Error::Media(
                "视频源已经拥有本地预览 worker，不能重复包装".into(),
            ));
        }
        let ffmpeg = crate::source::ffmpeg_bin()
            .ok_or_else(|| Error::Media("未找到 ffmpeg，无法启用文件预览".into()))?;
        Ok(Self::start(source, fps, sink, ffmpeg))
    }

    fn start(
        source: Box<dyn VideoSource>,
        fps: u32,
        sink: Arc<dyn PreviewSink>,
        ffmpeg: String,
    ) -> Self {
        let generation = next_capture_generation();
        let codec = source.codec();
        let (preview_input, preview_worker) =
            spawn_preview_worker_with_codec(ffmpeg, fps, generation, codec, sink);
        Self {
            source,
            preview_input,
            preview_worker,
            generation,
            sequence: 0,
        }
    }

    fn dispatch_video(&mut self, data: &[u8], codec: crate::ps::VideoCodec, key_frame: bool) {
        self.sequence = self.sequence.wrapping_add(1).max(1);
        if !self.preview_input.is_active() {
            return;
        }
        let access_unit = CapturedAccessUnit::with_codec(
            self.generation,
            self.sequence,
            data.to_vec(),
            key_frame,
            crate::source::now_ms(),
            codec,
        );
        // 预览始终是旁路；队列已满时丢弃该副本，主路继续拿到原始 AU。
        let _ = self.preview_input.try_send(access_unit);
    }
}

impl VideoSource for PreviewingSource {
    fn next_frame(&mut self) -> Option<Frame> {
        let frame = self.source.next_frame()?;
        let codec = self.source.codec();
        self.dispatch_video(&frame.data, codec, frame.key_frame);
        Some(frame)
    }

    fn next_audio(&mut self) -> Vec<Vec<u8>> {
        self.source.next_audio()
    }

    fn supports_timed_events(&self) -> bool {
        self.source.supports_timed_events()
    }

    fn timed_events_are_paced(&self) -> bool {
        self.source.timed_events_are_paced()
    }

    fn next_media_event(&mut self) -> Option<MediaEvent> {
        let event = self.source.next_media_event()?;
        if let MediaEvent::Video(video) = &event {
            // `video` 是主路将继续消费的同一个 TimedVideoAu；worker 只拥有字节副本。
            self.dispatch_video(&video.data, video.codec, video.key_frame);
        }
        Some(event)
    }

    fn audio_sample_rate_hz(&self) -> u32 {
        self.source.audio_sample_rate_hz()
    }

    fn has_audio(&self) -> bool {
        self.source.has_audio()
    }

    fn audio_codec(&self) -> crate::ps::AudioCodec {
        self.source.audio_codec()
    }

    fn codec(&self) -> crate::ps::VideoCodec {
        self.source.codec()
    }

    fn seek(&mut self, permille: u32) {
        self.source.seek(permille);
    }

    fn is_live(&self) -> bool {
        self.source.is_live()
    }

    fn take_error(&mut self) -> Option<String> {
        self.source.take_error()
    }

    fn preview_control(&self) -> Option<Arc<PreviewControl>> {
        Some(self.preview_worker.control())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::PreviewSink;
    use crate::ps::{AudioCodec, VideoCodec};
    use crate::source::{TimedAudioAu, TimedVideoAu};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[derive(Default)]
    struct NoopSink;

    impl PreviewSink for NoopSink {
        fn publish(&self, _frame: crate::preview::PreviewJpeg) {}
    }

    struct TimedSource {
        events: VecDeque<MediaEvent>,
        reads: Arc<AtomicUsize>,
        seek: Arc<Mutex<Option<u32>>>,
    }

    impl VideoSource for TimedSource {
        fn next_frame(&mut self) -> Option<Frame> {
            None
        }

        fn supports_timed_events(&self) -> bool {
            true
        }

        fn timed_events_are_paced(&self) -> bool {
            true
        }

        fn next_media_event(&mut self) -> Option<MediaEvent> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            self.events.pop_front()
        }

        fn audio_sample_rate_hz(&self) -> u32 {
            16_000
        }

        fn has_audio(&self) -> bool {
            true
        }

        fn audio_codec(&self) -> AudioCodec {
            AudioCodec::Aac
        }

        fn codec(&self) -> VideoCodec {
            VideoCodec::H264
        }

        fn seek(&mut self, permille: u32) {
            *self.seek.lock().unwrap() = Some(permille);
        }

        fn take_error(&mut self) -> Option<String> {
            Some("source test error".into())
        }
    }

    fn new_timed_source(
        events: Vec<MediaEvent>,
    ) -> (TimedSource, Arc<AtomicUsize>, Arc<Mutex<Option<u32>>>) {
        let reads = Arc::new(AtomicUsize::new(0));
        let seek = Arc::new(Mutex::new(None));
        (
            TimedSource {
                events: events.into(),
                reads: Arc::clone(&reads),
                seek: Arc::clone(&seek),
            },
            reads,
            seek,
        )
    }

    #[test]
    fn timed_event_video_audio_and_source_controls_are_transparent() {
        let video = TimedVideoAu::new(vec![0, 0, 0, 1, 1, 2], VideoCodec::H264, false, 90, 3_000);
        let audio =
            TimedAudioAu::with_duration(vec![1, 2, 3], AudioCodec::Aac, 16_000, 320, 90, 1_800);
        let (source, reads, seek) = new_timed_source(vec![
            MediaEvent::Video(video.clone()),
            MediaEvent::Audio(audio.clone()),
            MediaEvent::Discontinuity { pts_90k: 3_090 },
        ]);
        let mut source = PreviewingSource::new(Box::new(source), 15, Arc::new(NoopSink))
            .expect("测试需要可用的 ffmpeg");

        assert!(source.supports_timed_events());
        assert!(source.timed_events_are_paced());
        assert!(source.has_audio());
        assert_eq!(source.audio_codec(), AudioCodec::Aac);
        assert_eq!(source.audio_sample_rate_hz(), 16_000);
        assert_eq!(source.next_media_event(), Some(MediaEvent::Video(video)));
        assert_eq!(source.next_media_event(), Some(MediaEvent::Audio(audio)));
        assert_eq!(
            source.next_media_event(),
            Some(MediaEvent::Discontinuity { pts_90k: 3_090 })
        );
        assert_eq!(
            reads.load(Ordering::Relaxed),
            3,
            "包装器只能读取一次每个事件"
        );

        source.seek(600);
        assert_eq!(*seek.lock().unwrap(), Some(600));
        assert_eq!(source.take_error(), Some("source test error".into()));
        assert!(source.preview_control().is_some());
    }

    struct LegacySource {
        frames: usize,
    }

    impl VideoSource for LegacySource {
        fn next_frame(&mut self) -> Option<Frame> {
            self.frames += 1;
            (self.frames == 1).then_some(Frame {
                data: vec![0, 0, 0, 1, 1, 0],
                key_frame: false,
            })
        }

        fn next_audio(&mut self) -> Vec<Vec<u8>> {
            vec![vec![7, 8]]
        }

        fn has_audio(&self) -> bool {
            true
        }
    }

    #[test]
    fn legacy_frame_and_audio_still_use_one_capture_path() {
        let mut source =
            PreviewingSource::new(Box::new(LegacySource { frames: 0 }), 30, Arc::new(NoopSink))
                .expect("测试需要可用的 ffmpeg");
        assert!(!source.supports_timed_events());
        let frame = source.next_frame().expect("应透传首帧");
        assert_eq!(frame.data, vec![0, 0, 0, 1, 1, 0]);
        assert_eq!(source.next_audio(), vec![vec![7, 8]]);
        assert!(source.next_frame().is_none());
    }

    #[test]
    fn visibility_changes_preserve_main_video_audio_and_timestamps() {
        struct DemandSink(std::sync::atomic::AtomicBool);
        impl PreviewSink for DemandSink {
            fn is_active(&self) -> bool {
                self.0.load(Ordering::Acquire)
            }
            fn publish(&self, _frame: crate::preview::PreviewJpeg) {}
        }
        let sink = Arc::new(DemandSink(std::sync::atomic::AtomicBool::new(false)));
        let events = (0..12)
            .flat_map(|index| {
                let pts = index * 3_600;
                [
                    MediaEvent::Video(TimedVideoAu::new(
                        vec![0, 0, 0, 1, 1, index as u8],
                        VideoCodec::H264,
                        false,
                        pts,
                        3_600,
                    )),
                    MediaEvent::Audio(TimedAudioAu::with_duration(
                        vec![index as u8; 16],
                        AudioCodec::Aac,
                        16_000,
                        640,
                        pts,
                        3_600,
                    )),
                ]
            })
            .collect::<Vec<_>>();
        let (source, reads, _) = new_timed_source(events.clone());
        let mut source = PreviewingSource::new(Box::new(source), 25, sink.clone())
            .expect("测试需要可用的 ffmpeg");
        let generation = source.generation;
        for (index, expected) in events.iter().enumerate() {
            sink.0.store(index % 4 == 0, Ordering::Release);
            assert_eq!(source.next_media_event().as_ref(), Some(expected));
            assert_eq!(
                source.generation, generation,
                "visibility must not recreate the source"
            );
        }
        assert_eq!(reads.load(Ordering::Relaxed), events.len());
    }
}
