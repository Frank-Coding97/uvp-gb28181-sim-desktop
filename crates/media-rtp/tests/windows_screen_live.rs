//! Explicit Windows screen-capture acceptance. Never captures the desktop in ordinary tests.
#![cfg(windows)]
use media_rtp::profile::{MediaProfile, MediaVideoCodec};
use media_rtp::{
    LiveSource, MediaEvent, PreviewJpeg, PreviewPhase, PreviewSink, PreviewStatus, VideoSource,
};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

struct Sink {
    active: AtomicBool,
    count: AtomicU64,
    frame: Mutex<Option<PreviewJpeg>>,
    status: Mutex<Option<PreviewStatus>>,
}
impl PreviewSink for Sink {
    fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
    fn publish(&self, frame: PreviewJpeg) {
        self.count.fetch_add(1, Ordering::Relaxed);
        *self.frame.lock().unwrap() = Some(frame);
    }
    fn status(&self, status: PreviewStatus) {
        *self.status.lock().unwrap() = Some(status);
    }
}

#[derive(Default)]
struct Samples {
    video: u64,
    audio: u64,
    first: Option<u64>,
    last: Option<u64>,
    min_duration: u64,
}

fn sample(source: &mut LiveSource, samples: &mut Samples, duration: Duration) {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        if let Some(error) = source.take_error() {
            panic!("capture failed: {error}");
        }
        match source.next_media_event() {
            Some(MediaEvent::Video(au)) => {
                assert!(!au.data.is_empty());
                assert!(au.duration_90k > 0);
                if let Some(last) = samples.last {
                    assert!(au.pts_90k >= last, "video PTS regressed");
                }
                samples.first.get_or_insert(au.pts_90k);
                samples.last = Some(au.pts_90k);
                samples.video += 1;
                samples.min_duration = if samples.min_duration == 0 {
                    au.duration_90k
                } else {
                    samples.min_duration.min(au.duration_90k)
                };
            }
            Some(MediaEvent::Audio(_)) => {
                samples.audio += 1;
            }
            Some(MediaEvent::Discontinuity { .. }) => {
                panic!("live event queue lost data during screen acceptance")
            }
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }
}

#[test]
#[ignore = "requires an explicitly selected Windows display and an interactive desktop"]
fn windows_screen_real_capture_profile_pause_resume_and_release() {
    let uri = std::env::var("UVP_WINDOWS_SCREEN_TEST_URI")
        .expect("set explicit live:screen:<display_id>?audio=none URI");
    assert!(uri.starts_with("live:screen:"));
    assert!(
        uri.contains("audio=none"),
        "screen acceptance is video-only: {uri}"
    );
    let mut profile = MediaProfile::default();
    if std::env::var("UVP_WINDOWS_SCREEN_TEST_1080P").as_deref() == Ok("1") {
        profile.width = 1920;
        profile.height = 1080;
        profile.bitrate_kbps = 6000;
        profile.video_fps = 30;
    }
    if std::env::var("UVP_WINDOWS_SCREEN_TEST_HEVC").as_deref() == Ok("1") {
        profile.video_codec = MediaVideoCodec::H265;
    }
    let sink = Arc::new(Sink {
        active: AtomicBool::new(true),
        count: AtomicU64::new(0),
        frame: Mutex::new(None),
        status: Mutex::new(None),
    });
    let mut source = LiveSource::capture_with_profile(&uri, profile.clone(), Some(sink.clone()))
        .expect("open Windows desktop duplication");
    assert!(
        source.supports_timed_events(),
        "must preserve real encoded PTS"
    );
    let mut samples = Samples::default();
    sample(&mut source, &mut samples, Duration::from_secs(3));
    let expected = u64::from(profile.video_fps) * 3 * 7 / 10;
    assert!(
        samples.video >= expected,
        "insufficient live frames: {} < {expected}",
        samples.video
    );
    assert_eq!(
        samples.audio, 0,
        "video-only screen capture must not emit audio"
    );
    let first = sink
        .frame
        .lock()
        .unwrap()
        .clone()
        .expect("actual preview JPEG");
    assert!(first.data.starts_with(&[0xff, 0xd8]));
    let first_count = sink.count.load(Ordering::Relaxed);
    let first_sequence = first.sequence;

    sink.active.store(false, Ordering::Release);
    sample(&mut source, &mut samples, Duration::from_millis(300));
    assert_eq!(
        sink.status.lock().unwrap().as_ref().unwrap().phase,
        PreviewPhase::Paused
    );
    let paused_count = sink.count.load(Ordering::Relaxed);
    let video_before = samples.video;
    sample(&mut source, &mut samples, Duration::from_secs(1));
    assert_eq!(sink.count.load(Ordering::Relaxed), paused_count);
    assert!(
        samples.video > video_before,
        "preview pause must not pause screen capture"
    );

    sink.active.store(true, Ordering::Release);
    sample(&mut source, &mut samples, Duration::from_secs(2));
    let resumed = sink.frame.lock().unwrap().clone().expect("resumed JPEG");
    assert_eq!(
        resumed.generation, first.generation,
        "preview must not restart screen capture"
    );
    assert!(resumed.sequence > first_sequence);
    assert!(sink.count.load(Ordering::Relaxed) > first_count);

    // Decode metadata from the actual JPEG without writing desktop imagery to disk.
    use std::io::Write;
    use std::process::Stdio;
    let probe = std::env::var("UVP_FFPROBE_PATH").unwrap_or_else(|_| "ffprobe".into());
    let mut child = common::hidden_command(probe)
        .args([
            "-v",
            "error",
            "-f",
            "mjpeg",
            "-i",
            "pipe:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&resumed.data)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(metadata["streams"][0]["width"], profile.width);
    assert_eq!(metadata["streams"][0]["height"], profile.height);

    println!(
        "SCREEN_ACCEPTANCE video={} audio={} first_pts={:?} last_pts={:?} min_duration={} generation={} jpeg={}x{}",
        samples.video,
        samples.audio,
        samples.first,
        samples.last,
        samples.min_duration,
        resumed.generation,
        profile.width,
        profile.height
    );

    let stopped = Instant::now();
    drop(source);
    assert!(
        stopped.elapsed() < Duration::from_secs(4),
        "screen teardown must be bounded"
    );

    // Reopening verifies that the desktop duplication session was released.
    let mut reopened = LiveSource::capture_with_profile(&uri, profile, None)
        .expect("screen should reopen after stop");
    let mut again = Samples::default();
    sample(&mut reopened, &mut again, Duration::from_secs(1));
    assert!(again.video > 0, "reopened screen produced no frames");
    drop(reopened);
}
