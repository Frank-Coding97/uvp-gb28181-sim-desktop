use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::preview::{
    drain_jpeg_frames, CapturedAccessUnit, PreviewJpeg, PreviewPhase, PreviewSink, PreviewStatus,
};
use crate::ps::VideoCodec;

const MIN_INPUT_CAPACITY: usize = 2;
const MAX_INPUT_CAPACITY: usize = 8;
const STDERR_TAIL_LINES: usize = 32;
const OUTPUT_WATCHDOG: Duration = Duration::from_secs(2);
const WRITE_DEADLINE: Duration = Duration::from_millis(500);
const STOP_DEADLINE: Duration = Duration::from_secs(2);
const MAX_AUTOMATIC_RECOVERIES: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewSendResult {
    Sent,
    Dropped,
    /// The preview has no active consumer; the AU was intentionally not queued.
    Inactive,
    Stale,
    Disconnected,
}

type PreviewActivity = Arc<dyn Fn() -> bool + Send + Sync>;

#[cfg(test)]
type PreviewCommandOverride = Arc<Mutex<Option<Command>>>;

/// 主采集 reader 使用的有界、非阻塞预览入口。
#[derive(Clone)]
pub struct PreviewWorkerInput {
    generation: u64,
    tx: mpsc::SyncSender<CapturedAccessUnit>,
    discontinuity: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
    active: PreviewActivity,
    inactive_seen: Arc<AtomicBool>,
}

impl PreviewWorkerInput {
    /// Returns whether the preview currently has an active consumer.
    ///
    /// Producers can use this before cloning an encoded AU. `try_send` checks
    /// the predicate again to close the common hide/detach race.
    pub fn is_active(&self) -> bool {
        let active = (self.active)();
        if !active {
            self.inactive_seen.store(true, Ordering::Release);
        }
        active
    }

    pub fn try_send(&self, access_unit: CapturedAccessUnit) -> PreviewSendResult {
        if access_unit.generation != self.generation {
            return PreviewSendResult::Stale;
        }
        if !self.is_active() {
            return PreviewSendResult::Inactive;
        }
        match self.tx.try_send(access_unit) {
            Ok(()) => PreviewSendResult::Sent,
            Err(mpsc::TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                self.discontinuity.store(true, Ordering::Release);
                PreviewSendResult::Dropped
            }
            Err(mpsc::TrySendError::Disconnected(_)) => PreviewSendResult::Disconnected,
        }
    }
}

#[derive(Default)]
pub struct PreviewControl {
    retry_requested: AtomicBool,
}

impl PreviewControl {
    pub fn retry(&self) {
        self.retry_requested.store(true, Ordering::Release);
    }
}

/// PreviewWorker 的显式所有权。Drop 只回收自己持有的子进程。
pub struct PreviewWorkerHandle {
    stop: Arc<AtomicBool>,
    active_child: Arc<Mutex<Option<Child>>>,
    join: Option<JoinHandle<()>>,
    control: Arc<PreviewControl>,
}

impl PreviewWorkerHandle {
    pub fn control(&self) -> Arc<PreviewControl> {
        Arc::clone(&self.control)
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        kill_active_child(&self.active_child);
        let Some(join) = self.join.take() else {
            return;
        };
        let deadline = Instant::now() + STOP_DEADLINE;
        while !join.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if join.is_finished() {
            let _ = join.join();
        } else {
            tracing::warn!("preview worker supervisor did not stop within deadline");
        }
    }
}

impl Drop for PreviewWorkerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 启动隔离预览 supervisor。此时不会打开 FFmpeg；收到首个 config keyframe 才创建 decoder。
pub fn spawn_preview_worker(
    ffmpeg: String,
    fps: u32,
    generation: u64,
    sink: Arc<dyn PreviewSink>,
) -> (PreviewWorkerInput, PreviewWorkerHandle) {
    spawn_preview_worker_with_codec(ffmpeg, fps, generation, VideoCodec::H264, sink)
}

/// 启动指定编码的隔离预览 supervisor；不会重新打开或采集视频设备。
pub fn spawn_preview_worker_with_codec(
    ffmpeg: String,
    fps: u32,
    generation: u64,
    codec: VideoCodec,
    sink: Arc<dyn PreviewSink>,
) -> (PreviewWorkerInput, PreviewWorkerHandle) {
    spawn_preview_worker_inner(
        ffmpeg,
        fps,
        generation,
        codec,
        sink,
        #[cfg(test)]
        None,
    )
}

#[cfg(test)]
pub(crate) fn spawn_preview_worker_with_command_for_test(
    command: Command,
    fps: u32,
    generation: u64,
    sink: Arc<dyn PreviewSink>,
) -> (PreviewWorkerInput, PreviewWorkerHandle) {
    spawn_preview_worker_inner(
        "test-command-override".into(),
        fps,
        generation,
        VideoCodec::H264,
        sink,
        Some(Arc::new(Mutex::new(Some(command)))),
    )
}

fn spawn_preview_worker_inner(
    ffmpeg: String,
    fps: u32,
    generation: u64,
    codec: VideoCodec,
    sink: Arc<dyn PreviewSink>,
    #[cfg(test)] command_override: Option<PreviewCommandOverride>,
) -> (PreviewWorkerInput, PreviewWorkerHandle) {
    let activity_sink = Arc::clone(&sink);
    let activity: PreviewActivity = Arc::new(move || activity_sink.is_active());
    let (input, rx) =
        preview_input_channel_with_activity(generation, preview_input_capacity(fps), activity);
    let discontinuity = Arc::clone(&input.discontinuity);
    let dropped = Arc::clone(&input.dropped);
    let inactive_seen = Arc::clone(&input.inactive_seen);
    let stop = Arc::new(AtomicBool::new(false));
    let active_child = Arc::new(Mutex::new(None));
    let control = Arc::new(PreviewControl::default());

    let supervisor_stop = Arc::clone(&stop);
    let supervisor_child = Arc::clone(&active_child);
    let supervisor_control = Arc::clone(&control);
    #[cfg(test)]
    let supervisor_command_override = command_override.clone();
    let join = std::thread::spawn(move || {
        run_supervisor(
            &ffmpeg,
            fps.max(1),
            generation,
            codec,
            sink,
            rx,
            discontinuity,
            dropped,
            inactive_seen,
            #[cfg(test)]
            supervisor_command_override,
            supervisor_stop,
            supervisor_child,
            supervisor_control,
        );
    });

    (
        input,
        PreviewWorkerHandle {
            stop,
            active_child,
            join: Some(join),
            control,
        },
    )
}

fn preview_input_capacity(fps: u32) -> usize {
    fps.max(1)
        .div_ceil(4)
        .clamp(MIN_INPUT_CAPACITY as u32, MAX_INPUT_CAPACITY as u32) as usize
}

#[cfg(test)]
fn preview_input_channel(
    generation: u64,
    capacity: usize,
) -> (PreviewWorkerInput, mpsc::Receiver<CapturedAccessUnit>) {
    preview_input_channel_with_activity(generation, capacity, Arc::new(|| true))
}

fn preview_input_channel_with_activity(
    generation: u64,
    capacity: usize,
    active: PreviewActivity,
) -> (PreviewWorkerInput, mpsc::Receiver<CapturedAccessUnit>) {
    let (tx, rx) = mpsc::sync_channel(capacity.max(1));
    let discontinuity = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicU64::new(0));
    let inactive_seen = Arc::new(AtomicBool::new(false));
    (
        PreviewWorkerInput {
            generation,
            tx,
            discontinuity,
            dropped,
            active,
            inactive_seen,
        },
        rx,
    )
}

fn drain_input(rx: &mpsc::Receiver<CapturedAccessUnit>) {
    while rx.try_recv().is_ok() {}
}

fn next_preview_generation(current: u64) -> u64 {
    current.wrapping_add(1).max(1)
}

#[cfg(test)]
pub(crate) fn preview_input_channel_for_test(
    generation: u64,
    capacity: usize,
) -> (PreviewWorkerInput, mpsc::Receiver<CapturedAccessUnit>) {
    preview_input_channel(generation, capacity)
}

#[allow(clippy::too_many_arguments)]
fn run_supervisor(
    ffmpeg: &str,
    fps: u32,
    generation: u64,
    codec: VideoCodec,
    sink: Arc<dyn PreviewSink>,
    rx: mpsc::Receiver<CapturedAccessUnit>,
    discontinuity: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
    inactive_seen: Arc<AtomicBool>,
    #[cfg(test)] command_override: Option<PreviewCommandOverride>,
    stop: Arc<AtomicBool>,
    active_child: Arc<Mutex<Option<Child>>>,
    control: Arc<PreviewControl>,
) {
    let mut status = PreviewStatus::new(generation, PreviewPhase::WaitingKeyframe);
    let mut attempt: Option<PreviewAttempt> = None;
    let mut failures = 0_u32;
    let mut retry_not_before: Option<Instant> = None;
    sink.status(status.clone());

    while !stop.load(Ordering::Acquire) {
        // Demand can disappear between two supervisor polls. Producers also
        // latch a short inactive interval through `PreviewWorkerInput`, so a
        // hide/show transition cannot leave a decoder consuming delta frames
        // with a stale reference picture.
        let active = sink.is_active();
        let inactive_latched = inactive_seen.swap(false, Ordering::AcqRel);
        if !active || inactive_latched {
            stop_attempt(&mut attempt, &active_child);
            retry_not_before = None;
            discontinuity.swap(false, Ordering::AcqRel);
            drain_input(&rx);
            if status.phase != PreviewPhase::Paused {
                status.phase = PreviewPhase::Paused;
                status.reason = None;
                sink.status(status.clone());
            }
            if !active {
                std::thread::sleep(Duration::from_millis(25));
            }
            continue;
        }
        if status.phase == PreviewPhase::Paused {
            // Drop anything sent during the transition before assigning the
            // new decoder lifecycle its preview generation.
            discontinuity.swap(false, Ordering::AcqRel);
            drain_input(&rx);
            status.preview_generation = next_preview_generation(status.preview_generation);
            status.phase = PreviewPhase::WaitingKeyframe;
            status.reason = None;
            sink.status(status.clone());
        }

        if control.retry_requested.swap(false, Ordering::AcqRel) {
            stop_attempt(&mut attempt, &active_child);
            failures = 0;
            retry_not_before = None;
            status.preview_generation = status.preview_generation.wrapping_add(1).max(1);
            status.phase = PreviewPhase::WaitingKeyframe;
            status.reason = None;
            status.recoveries = 0;
            sink.status(status.clone());
        }

        if discontinuity.swap(false, Ordering::AcqRel) {
            status.dropped_frames = dropped.load(Ordering::Acquire);
            while rx.try_recv().is_ok() {}
            if attempt.is_some() {
                register_failure(
                    "preview input queue overflowed; waiting for a new config keyframe".into(),
                    &mut attempt,
                    &active_child,
                    &sink,
                    &mut status,
                    &mut failures,
                    &mut retry_not_before,
                );
            } else if status.phase != PreviewPhase::Unavailable {
                status.phase = PreviewPhase::WaitingKeyframe;
                status.reason = Some(format!(
                    "preview input discontinuity; waiting for {}",
                    config_keyframe_label(codec)
                ));
                sink.status(status.clone());
            }
        }

        let mut attempt_failure = None;
        let mut became_inactive = false;
        if let Some(current) = attempt.as_mut() {
            while let Ok(event) = current.events.try_recv() {
                if !sink.is_active() {
                    inactive_seen.store(true, Ordering::Release);
                    became_inactive = true;
                    break;
                }
                match event {
                    AttemptEvent::Output { meta, data } => {
                        if meta.generation != generation
                            || meta.preview_generation != status.preview_generation
                        {
                            continue;
                        }
                        current.output_count = current.output_count.saturating_add(1);
                        current.last_output = Some(Instant::now());
                        status.jpeg_output_frames = status.jpeg_output_frames.saturating_add(1);
                        status.last_output_at_ms = Some(now_ms());
                        status.phase = PreviewPhase::Playing;
                        status.reason = None;
                        sink.publish(PreviewJpeg {
                            generation: meta.generation,
                            sequence: meta.sequence,
                            captured_at_ms: meta.captured_at_ms,
                            data,
                        });
                        sink.status(status.clone());
                    }
                    #[cfg(windows)]
                    AttemptEvent::InputWritten => {
                        current.pending_write_since.pop_front();
                        current.written_count = current.written_count.saturating_add(1);
                        current.last_input = Some(Instant::now());
                    }
                    #[cfg(windows)]
                    AttemptEvent::WriterFailed(reason) => {
                        attempt_failure = Some(reason);
                        break;
                    }
                    AttemptEvent::UnexpectedOutput => {
                        attempt_failure = Some(format!(
                            "preview FFmpeg produced JPEG without a matching {} input",
                            codec_name(codec)
                        ));
                        break;
                    }
                    AttemptEvent::ReaderFailed(reason) => {
                        attempt_failure = Some(reason);
                        break;
                    }
                    AttemptEvent::ReaderEnded => {
                        attempt_failure = Some("preview FFmpeg stdout ended unexpectedly".into());
                        break;
                    }
                }
            }

            if !became_inactive && attempt_failure.is_none() {
                if let Some(exit) = child_exit_status(&active_child) {
                    attempt_failure = Some(format!(
                        "preview FFmpeg exited ({exit}); stderr: {}",
                        locked_tail(&current.stderr_tail)
                    ));
                }
            }
            if attempt_failure.is_none()
                && current.written_count > current.output_count
                && current
                    .last_input
                    .is_some_and(|at| at.elapsed() >= OUTPUT_WATCHDOG)
                && current
                    .last_output
                    .map_or(true, |at| at.elapsed() >= OUTPUT_WATCHDOG)
            {
                attempt_failure = Some(format!(
                    "preview FFmpeg produced no JPEG within {} ms; input={}, output={}, stderr: {}",
                    OUTPUT_WATCHDOG.as_millis(),
                    current.written_count,
                    current.output_count,
                    locked_tail(&current.stderr_tail)
                ));
            }
            #[cfg(windows)]
            if attempt_failure.is_none()
                && current
                    .pending_write_since
                    .front()
                    .is_some_and(|at| at.elapsed() >= WRITE_DEADLINE)
            {
                attempt_failure = Some(format!(
                    "preview FFmpeg stdin write stayed pending for at least {} ms; input={}, output={}, stderr: {}",
                    WRITE_DEADLINE.as_millis(),
                    current.written_count,
                    current.output_count,
                    locked_tail(&current.stderr_tail)
                ));
            }
        }
        if became_inactive {
            continue;
        }
        if let Some(reason) = attempt_failure {
            register_failure(
                reason,
                &mut attempt,
                &active_child,
                &sink,
                &mut status,
                &mut failures,
                &mut retry_not_before,
            );
            continue;
        }

        if !sink.is_active() {
            inactive_seen.store(true, Ordering::Release);
            continue;
        }

        #[cfg(windows)]
        if attempt
            .as_ref()
            .is_some_and(|current| !current.pending_write_since.is_empty())
        {
            // Let the writer acknowledge the current AU before consuming the
            // next one from the outer queue. The next loop still processes
            // events, demand changes, and the pending-write deadline.
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }

        let access_unit = match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(access_unit) => access_unit,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if !sink.is_active() {
            inactive_seen.store(true, Ordering::Release);
            continue;
        }
        if access_unit.generation != generation {
            continue;
        }
        if access_unit.codec != codec {
            status.phase = PreviewPhase::WaitingKeyframe;
            status.reason = Some(format!(
                "preview input codec mismatch: expected {}, got {}",
                codec_name(codec),
                codec_name(access_unit.codec)
            ));
            sink.status(status.clone());
            continue;
        }
        status.h264_input_frames = status.h264_input_frames.saturating_add(1);
        status.last_input_at_ms = Some(access_unit.captured_at_ms);

        if status.phase == PreviewPhase::Unavailable {
            continue;
        }
        if retry_not_before.is_some_and(|deadline| Instant::now() < deadline) {
            continue;
        }
        if retry_not_before.take().is_some() {
            status.phase = PreviewPhase::WaitingKeyframe;
            status.reason = Some(format!(
                "waiting for {} after recovery",
                config_keyframe_label(codec)
            ));
            sink.status(status.clone());
        }
        if attempt.is_none() {
            if !sink.is_active() {
                inactive_seen.store(true, Ordering::Release);
                continue;
            }
            if !access_unit.config_keyframe {
                if status.phase != PreviewPhase::WaitingKeyframe {
                    status.phase = PreviewPhase::WaitingKeyframe;
                    sink.status(status.clone());
                }
                continue;
            }
            match PreviewAttempt::spawn(
                ffmpeg,
                fps,
                status.preview_generation,
                codec,
                Arc::clone(&active_child),
                Arc::clone(&stop),
                #[cfg(test)]
                command_override.clone(),
            ) {
                Ok(started) => {
                    tracing::info!(
                        generation,
                        preview_generation = status.preview_generation,
                        pid = started.pid,
                        "preview worker FFmpeg started"
                    );
                    status.phase = PreviewPhase::PreviewStarting;
                    status.reason = None;
                    sink.status(status.clone());
                    attempt = Some(started);
                }
                Err(reason) => {
                    register_failure(
                        reason,
                        &mut attempt,
                        &active_child,
                        &sink,
                        &mut status,
                        &mut failures,
                        &mut retry_not_before,
                    );
                    continue;
                }
            }
        }
        if let Some(current) = attempt.as_mut() {
            if let Err(reason) = current.feed(access_unit, &stop) {
                register_failure(
                    reason,
                    &mut attempt,
                    &active_child,
                    &sink,
                    &mut status,
                    &mut failures,
                    &mut retry_not_before,
                );
            }
        }
    }

    status.phase = PreviewPhase::Stopping;
    sink.status(status.clone());
    stop_attempt(&mut attempt, &active_child);
    status.phase = PreviewPhase::Stopped;
    status.reason = None;
    sink.status(status);
    sink.stopped();
}

#[allow(clippy::too_many_arguments)]
fn register_failure(
    reason: String,
    attempt: &mut Option<PreviewAttempt>,
    active_child: &Arc<Mutex<Option<Child>>>,
    sink: &Arc<dyn PreviewSink>,
    status: &mut PreviewStatus,
    failures: &mut u32,
    retry_not_before: &mut Option<Instant>,
) {
    stop_attempt(attempt, active_child);
    *failures = failures.saturating_add(1);
    status.reason = Some(reason.clone());
    if *failures <= MAX_AUTOMATIC_RECOVERIES {
        status.preview_generation = status.preview_generation.wrapping_add(1).max(1);
        status.recoveries = *failures;
        status.phase = PreviewPhase::Recovering;
        *retry_not_before = (*failures == MAX_AUTOMATIC_RECOVERIES)
            .then(|| Instant::now() + Duration::from_secs(1));
        tracing::warn!(
            generation = status.generation,
            preview_generation = status.preview_generation,
            recovery = *failures,
            %reason,
            "preview worker recovering without restarting camera capture"
        );
        sink.status(status.clone());
    } else {
        status.recoveries = MAX_AUTOMATIC_RECOVERIES;
        status.phase = PreviewPhase::Unavailable;
        *retry_not_before = None;
        tracing::error!(
            generation = status.generation,
            preview_generation = status.preview_generation,
            %reason,
            "preview worker unavailable; camera capture remains running"
        );
        sink.status(status.clone());
        sink.unavailable(reason);
    }
}

#[derive(Debug, Clone, Copy)]
struct FrameMeta {
    generation: u64,
    preview_generation: u64,
    sequence: u64,
    captured_at_ms: u64,
}

enum AttemptEvent {
    Output {
        meta: FrameMeta,
        data: Vec<u8>,
    },
    #[cfg(windows)]
    InputWritten,
    #[cfg(windows)]
    WriterFailed(String),
    UnexpectedOutput,
    ReaderFailed(String),
    ReaderEnded,
}

struct PreviewAttempt {
    pid: u32,
    preview_generation: u64,
    #[cfg(not(windows))]
    stdin: Option<ChildStdin>,
    #[cfg(windows)]
    stdin_tx: Option<mpsc::SyncSender<Vec<u8>>>,
    #[cfg(windows)]
    stdin_join: Option<JoinHandle<()>>,
    #[cfg(windows)]
    pending_write_since: VecDeque<Instant>,
    events: mpsc::Receiver<AttemptEvent>,
    stdout_join: Option<JoinHandle<()>>,
    stderr_join: Option<JoinHandle<()>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    timing_fifo: Arc<Mutex<VecDeque<FrameMeta>>>,
    written_count: u64,
    output_count: u64,
    last_input: Option<Instant>,
    last_output: Option<Instant>,
}

impl PreviewAttempt {
    fn spawn(
        ffmpeg: &str,
        fps: u32,
        preview_generation: u64,
        codec: VideoCodec,
        active_child: Arc<Mutex<Option<Child>>>,
        stop: Arc<AtomicBool>,
        #[cfg(test)] command_override: Option<PreviewCommandOverride>,
    ) -> std::result::Result<Self, String> {
        let args = preview_worker_args_with_codec(codec, fps, 0);
        #[cfg(test)]
        let has_command_override = command_override.is_some();
        #[cfg(test)]
        let mut command = if let Some(command_override) = command_override {
            command_override
                .lock()
                .map_err(|_| "preview test command lock poisoned".to_string())?
                .take()
                .ok_or_else(|| "preview test command was already consumed".to_string())?
        } else {
            Command::new(ffmpeg)
        };
        #[cfg(test)]
        if !has_command_override {
            command.args(&args);
        }
        #[cfg(not(test))]
        let mut command = Command::new(ffmpeg);
        #[cfg(not(test))]
        command.args(&args);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("failed to start preview FFmpeg: {error}"))?;
        let pid = child.id();
        #[cfg(unix)]
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to open preview FFmpeg stdin".to_string())?;
        #[cfg(windows)]
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to open preview FFmpeg stdin".to_string())?;
        #[cfg(unix)]
        set_nonblocking(&mut stdin)
            .map_err(|error| format!("failed to configure preview stdin: {error}"))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| "failed to open preview FFmpeg stdout".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "failed to open preview FFmpeg stderr".to_string())?;
        *active_child
            .lock()
            .map_err(|_| "preview child lock poisoned".to_string())? = Some(child);

        let timing_fifo = Arc::new(Mutex::new(VecDeque::<FrameMeta>::new()));
        let reader_fifo = Arc::clone(&timing_fifo);
        let (events_tx, events) = mpsc::channel();
        #[cfg(windows)]
        let (stdin_tx, stdin_join) = {
            let (write_tx, write_rx) = mpsc::sync_channel::<Vec<u8>>(1);
            let writer_events = events_tx.clone();
            let writer_stop = Arc::clone(&stop);
            let stdin_join = std::thread::spawn(move || {
                let mut stdin = stdin;
                while let Ok(data) = write_rx.recv() {
                    if writer_stop.load(Ordering::Acquire) {
                        break;
                    }
                    if let Err(error) = write_all_bounded(&mut stdin, &data, &writer_stop) {
                        if !writer_stop.load(Ordering::Acquire) {
                            let _ = writer_events.send(AttemptEvent::WriterFailed(format!(
                                "preview FFmpeg stdin write failed: {error}"
                            )));
                        }
                        break;
                    }
                    if writer_stop.load(Ordering::Acquire) {
                        break;
                    }
                    if writer_events.send(AttemptEvent::InputWritten).is_err() {
                        break;
                    }
                }
            });
            (Some(write_tx), Some(stdin_join))
        };
        let reader_events = events_tx.clone();
        let reader_stop = Arc::clone(&stop);
        let stdout_join = std::thread::spawn(move || {
            let mut pending = Vec::with_capacity(256 * 1024);
            let mut chunk = [0_u8; 64 * 1024];
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) => {
                        if !reader_stop.load(Ordering::Acquire) {
                            let _ = reader_events.send(AttemptEvent::ReaderEnded);
                        }
                        break;
                    }
                    Err(error) => {
                        if !reader_stop.load(Ordering::Acquire) {
                            let _ = reader_events.send(AttemptEvent::ReaderFailed(format!(
                                "preview FFmpeg stdout read failed: {error}"
                            )));
                        }
                        break;
                    }
                    Ok(size) => {
                        pending.extend_from_slice(&chunk[..size]);
                        for data in drain_jpeg_frames(&mut pending) {
                            let meta = reader_fifo
                                .lock()
                                .ok()
                                .and_then(|mut fifo| fifo.pop_front());
                            let Some(meta) = meta else {
                                let _ = reader_events.send(AttemptEvent::UnexpectedOutput);
                                return;
                            };
                            if reader_events
                                .send(AttemptEvent::Output { meta, data })
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                }
            }
        });

        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
        let stderr_output = Arc::clone(&stderr_tail);
        let stderr_join = std::thread::spawn(move || {
            for line in BufReader::new(stderr)
                .lines()
                .map_while(std::result::Result::ok)
            {
                tracing::warn!(pid, line = %line, "preview FFmpeg stderr");
                if let Ok(mut tail) = stderr_output.lock() {
                    if tail.len() == STDERR_TAIL_LINES {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
            }
        });

        Ok(Self {
            pid,
            preview_generation,
            #[cfg(not(windows))]
            stdin: Some(stdin),
            #[cfg(windows)]
            stdin_tx,
            #[cfg(windows)]
            stdin_join,
            #[cfg(windows)]
            pending_write_since: VecDeque::new(),
            events,
            stdout_join: Some(stdout_join),
            stderr_join: Some(stderr_join),
            stderr_tail,
            timing_fifo,
            written_count: 0,
            output_count: 0,
            last_input: None,
            last_output: None,
        })
    }

    fn feed(
        &mut self,
        access_unit: CapturedAccessUnit,
        #[cfg(windows)] _stop: &AtomicBool,
        #[cfg(not(windows))] stop: &AtomicBool,
    ) -> std::result::Result<(), String> {
        let meta = FrameMeta {
            generation: access_unit.generation,
            preview_generation: self.preview_generation,
            sequence: access_unit.sequence,
            captured_at_ms: access_unit.captured_at_ms,
        };
        self.timing_fifo
            .lock()
            .map_err(|_| "preview timing FIFO lock poisoned".to_string())?
            .push_back(meta);
        #[cfg(windows)]
        {
            let result = self
                .stdin_tx
                .as_ref()
                .ok_or_else(|| "preview FFmpeg stdin queue is closed".to_string())?
                .try_send(access_unit.data);
            match result {
                Ok(()) => {
                    self.pending_write_since.push_back(Instant::now());
                    return Ok(());
                }
                Err(mpsc::TrySendError::Full(_)) => {
                    if let Ok(mut fifo) = self.timing_fifo.lock() {
                        fifo.clear();
                    }
                    return Err("preview FFmpeg stdin queue stayed full".into());
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    if let Ok(mut fifo) = self.timing_fifo.lock() {
                        fifo.clear();
                    }
                    return Err("preview FFmpeg stdin writer stopped".into());
                }
            }
        }
        #[cfg(not(windows))]
        {
            let result = write_all_bounded(
                self.stdin
                    .as_mut()
                    .ok_or_else(|| "preview FFmpeg stdin is closed".to_string())?,
                &access_unit.data,
                stop,
            );
            if let Err(error) = result {
                if let Ok(mut fifo) = self.timing_fifo.lock() {
                    fifo.clear();
                }
                return Err(format!("preview FFmpeg stdin write failed: {error}"));
            }
            self.written_count = self.written_count.saturating_add(1);
            self.last_input = Some(Instant::now());
            Ok(())
        }
    }

    fn stop(&mut self, active_child: &Arc<Mutex<Option<Child>>>) {
        #[cfg(not(windows))]
        self.stdin.take();
        #[cfg(windows)]
        {
            // Killing the child closes the read side of its stdin pipe and
            // releases a writer blocked in the OS before we join that thread.
            kill_active_child(active_child);
            self.stdin_tx.take();
            if let Some(join) = self.stdin_join.take() {
                let _ = join.join();
            }
            self.pending_write_since.clear();
        }
        #[cfg(not(windows))]
        kill_active_child(active_child);
        if let Some(join) = self.stdout_join.take() {
            let _ = join.join();
        }
        if let Some(join) = self.stderr_join.take() {
            let _ = join.join();
        }
        if let Ok(mut fifo) = self.timing_fifo.lock() {
            fifo.clear();
        }
        tracing::info!(pid = self.pid, "preview worker FFmpeg stopped");
    }
}

fn stop_attempt(attempt: &mut Option<PreviewAttempt>, active_child: &Arc<Mutex<Option<Child>>>) {
    if let Some(mut current) = attempt.take() {
        current.stop(active_child);
    } else {
        kill_active_child(active_child);
    }
}

fn kill_active_child(active_child: &Arc<Mutex<Option<Child>>>) {
    let Ok(mut guard) = active_child.lock() else {
        return;
    };
    if let Some(mut child) = guard.take() {
        let pid = child.id();
        let _ = child.kill();
        let _ = child.wait();
        tracing::debug!(pid, "owned preview child reaped");
    }
}

fn child_exit_status(active_child: &Arc<Mutex<Option<Child>>>) -> Option<ExitStatus> {
    active_child.lock().ok().and_then(|mut child| {
        child
            .as_mut()
            .and_then(|child| child.try_wait().ok())
            .flatten()
    })
}

pub fn preview_worker_args(fps: u32, width: u32) -> Vec<String> {
    preview_worker_args_with_codec(VideoCodec::H264, fps, width)
}

/// 生成指定编码的 FFmpeg 预览参数；width 为 0 时保留原始尺寸，否则按指定宽度缩放。
/// 输入只从 stdin 读取，输出仍为 MJPEG stdout。
pub fn preview_worker_args_with_codec(codec: VideoCodec, fps: u32, width: u32) -> Vec<String> {
    let input_format = match codec {
        VideoCodec::H264 => "h264",
        VideoCodec::H265 => "hevc",
    };
    vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-loglevel".into(),
        "warning".into(),
        "-fflags".into(),
        "+genpts".into(),
        "-flags".into(),
        "low_delay".into(),
        "-probesize".into(),
        "32".into(),
        "-analyzeduration".into(),
        "0".into(),
        "-r".into(),
        fps.max(1).to_string(),
        "-f".into(),
        input_format.into(),
        "-i".into(),
        "pipe:0".into(),
        "-map".into(),
        "0:v:0".into(),
        "-an".into(),
        "-vf".into(),
        if width == 0 {
            "format=yuvj420p".into()
        } else {
            format!("scale={}:-2,format=yuvj420p", width.max(160))
        },
        "-c:v".into(),
        "mjpeg".into(),
        "-threads:v".into(),
        "1".into(),
        "-q:v".into(),
        "2".into(),
        "-fps_mode".into(),
        "passthrough".into(),
        "-f".into(),
        "mjpeg".into(),
        "-flush_packets".into(),
        "1".into(),
        "pipe:1".into(),
    ]
}

#[cfg(unix)]
fn set_nonblocking(stdin: &mut ChildStdin) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let fd = stdin.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn write_all_bounded(
    writer: &mut ChildStdin,
    mut data: &[u8],
    stop: &AtomicBool,
) -> std::io::Result<()> {
    let deadline = Instant::now() + WRITE_DEADLINE;
    while !data.is_empty() {
        if stop.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "preview worker stopping",
            ));
        }
        match writer.write(data) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "preview stdin closed",
                ))
            }
            Ok(size) => data = &data[size..],
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "preview stdin stayed blocked",
                    ));
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    writer.flush()
}

fn locked_tail(tail: &Arc<Mutex<VecDeque<String>>>) -> String {
    tail.lock()
        .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join(" | "))
        .unwrap_or_else(|_| "stderr unavailable".into())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn codec_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "h264",
        VideoCodec::H265 => "h265",
    }
}

fn config_keyframe_label(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "SPS/PPS/IDR",
        VideoCodec::H265 => "VPS/SPS/PPS/IRAP",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Condvar;

    struct RecordingSink {
        frames: Mutex<Vec<PreviewJpeg>>,
        statuses: Mutex<Vec<PreviewStatus>>,
        active: AtomicBool,
    }

    impl Default for RecordingSink {
        fn default() -> Self {
            Self {
                frames: Mutex::new(Vec::new()),
                statuses: Mutex::new(Vec::new()),
                active: AtomicBool::new(true),
            }
        }
    }

    impl RecordingSink {
        fn inactive() -> Self {
            Self {
                active: AtomicBool::new(false),
                ..Self::default()
            }
        }

        fn set_active(&self, active: bool) {
            self.active.store(active, Ordering::Release);
        }

        fn latest_status(&self) -> Option<PreviewStatus> {
            self.statuses.lock().unwrap().last().cloned()
        }
    }

    impl PreviewSink for RecordingSink {
        fn is_active(&self) -> bool {
            self.active.load(Ordering::Acquire)
        }

        fn publish(&self, frame: PreviewJpeg) {
            self.frames.lock().unwrap().push(frame);
        }

        fn status(&self, status: PreviewStatus) {
            self.statuses.lock().unwrap().push(status);
        }
    }

    struct InitialStatusGateSink {
        active: AtomicBool,
        initial_entered: (Mutex<bool>, Condvar),
        release_initial: (Mutex<bool>, Condvar),
        statuses: Mutex<Vec<PreviewStatus>>,
    }

    impl InitialStatusGateSink {
        fn new() -> Self {
            Self {
                active: AtomicBool::new(true),
                initial_entered: (Mutex::new(false), Condvar::new()),
                release_initial: (Mutex::new(false), Condvar::new()),
                statuses: Mutex::new(Vec::new()),
            }
        }

        fn wait_initial_status(&self) {
            let (entered, ready) = (&self.initial_entered.0, &self.initial_entered.1);
            let mut entered = entered.lock().unwrap();
            while !*entered {
                entered = ready.wait(entered).unwrap();
            }
        }

        fn set_active(&self, active: bool) {
            self.active.store(active, Ordering::Release);
        }

        fn release_initial_status(&self) {
            let (released, ready) = (&self.release_initial.0, &self.release_initial.1);
            *released.lock().unwrap() = true;
            ready.notify_all();
        }

        fn has_pause_resume_transition(&self) -> bool {
            self.statuses.lock().unwrap().windows(2).any(|pair| {
                pair[0].phase == PreviewPhase::Paused
                    && pair[1].phase == PreviewPhase::WaitingKeyframe
                    && pair[1].preview_generation > pair[0].preview_generation
            })
        }

        fn statuses(&self) -> Vec<PreviewStatus> {
            self.statuses.lock().unwrap().clone()
        }
    }

    impl PreviewSink for InitialStatusGateSink {
        fn is_active(&self) -> bool {
            self.active.load(Ordering::Acquire)
        }

        fn publish(&self, _frame: PreviewJpeg) {}

        fn status(&self, status: PreviewStatus) {
            let first = {
                let mut statuses = self.statuses.lock().unwrap();
                let first = statuses.is_empty();
                statuses.push(status);
                first
            };
            if !first {
                return;
            }
            *self.initial_entered.0.lock().unwrap() = true;
            self.initial_entered.1.notify_all();
            let (released, ready) = (&self.release_initial.0, &self.release_initial.1);
            let mut released = released.lock().unwrap();
            while !*released {
                released = ready.wait(released).unwrap();
            }
        }
    }

    #[cfg(windows)]
    struct PauseOnStartingSink {
        active: AtomicBool,
        statuses: Mutex<Vec<PreviewStatus>>,
    }

    #[cfg(windows)]
    impl PauseOnStartingSink {
        fn new() -> Self {
            Self {
                active: AtomicBool::new(true),
                statuses: Mutex::new(Vec::new()),
            }
        }

        fn latest_status(&self) -> Option<PreviewStatus> {
            self.statuses.lock().unwrap().last().cloned()
        }
    }

    #[cfg(windows)]
    impl PreviewSink for PauseOnStartingSink {
        fn is_active(&self) -> bool {
            self.active.load(Ordering::Acquire)
        }

        fn publish(&self, _frame: PreviewJpeg) {}

        fn status(&self, status: PreviewStatus) {
            let should_pause = status.phase == PreviewPhase::PreviewStarting;
            self.statuses.lock().unwrap().push(status);
            if should_pause {
                // This runs after PreviewAttempt::spawn and before the
                // supervisor calls feed, so a blocking stdin write is forced
                // to race with the demand transition deterministically.
                self.active.store(false, Ordering::Release);
            }
        }
    }

    fn access_unit(generation: u64, sequence: u64) -> CapturedAccessUnit {
        CapturedAccessUnit::new(
            generation,
            sequence,
            vec![0, 0, 0, 1, 5, sequence as u8],
            true,
            sequence,
        )
    }

    fn config_access_unit(generation: u64, sequence: u64) -> CapturedAccessUnit {
        let mut data = vec![0, 0, 0, 1, 7, 1];
        data.extend([0, 0, 0, 1, 8, 2]);
        data.extend([0, 0, 0, 1, 5, 3]);
        CapturedAccessUnit::new(generation, sequence, data, true, sequence)
    }

    fn wait_until(timeout: Duration, predicate: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if predicate() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        predicate()
    }

    #[test]
    fn native_preview_preserves_resolution_and_uses_high_quality_jpeg() {
        for codec in [VideoCodec::H264, VideoCodec::H265] {
            let args = preview_worker_args_with_codec(codec, 25, 0);
            let filter = args.windows(2).find(|pair| pair[0] == "-vf").unwrap();
            assert_eq!(filter[1], "format=yuvj420p");
            assert!(args.windows(2).any(|pair| pair == ["-q:v", "2"]));
        }
    }

    #[test]
    fn worker命令只读h264标准输入且不打开采集设备() {
        let args = preview_worker_args(30, 480);
        let command = args.join(" ");
        assert!(command.contains("-f h264 -i pipe:0"));
        assert!(command.contains("-fps_mode passthrough"));
        assert!(command.ends_with("-f mjpeg -flush_packets 1 pipe:1"));
        assert!(!command.contains("avfoundation"));
        assert!(!command.contains("pipe:3"));
        assert!(!command.contains(" cfr"));
    }

    #[test]
    fn worker命令按编码选择hevc输入且输出仍为mjpeg() {
        let args = preview_worker_args_with_codec(crate::ps::VideoCodec::H265, 30, 480);
        let command = args.join(" ");
        assert!(command.contains("-f hevc -i pipe:0"));
        assert!(command.ends_with("-f mjpeg -flush_packets 1 pipe:1"));
        assert!(!command.contains("avfoundation"));
    }

    #[test]
    fn preview入口满载时立即丢弃且标记discontinuity() {
        let (input, _rx) = preview_input_channel_for_test(7, 1);
        assert_eq!(input.try_send(access_unit(7, 1)), PreviewSendResult::Sent);
        assert_eq!(
            input.try_send(access_unit(7, 2)),
            PreviewSendResult::Dropped
        );
        assert!(input.discontinuity.load(Ordering::Acquire));
        assert_eq!(input.dropped.load(Ordering::Acquire), 1);
        assert_eq!(input.try_send(access_unit(8, 3)), PreviewSendResult::Stale);
    }

    #[test]
    fn preview入口按帧率保留约四分之一秒抖动预算() {
        assert_eq!(preview_input_capacity(1), 2);
        assert_eq!(preview_input_capacity(15), 4);
        assert_eq!(preview_input_capacity(30), 8);
        assert_eq!(preview_input_capacity(120), 8);
    }

    #[test]
    fn 无活跃订阅时预览入口不入队且不启动解码器() {
        let sink = Arc::new(RecordingSink::inactive());
        let sink_trait: Arc<dyn PreviewSink> = sink.clone();
        let (input, mut worker) =
            spawn_preview_worker("preview-ffmpeg-must-not-start".into(), 10, 77, sink_trait);

        assert!(wait_until(Duration::from_secs(1), || {
            sink.latest_status()
                .is_some_and(|status| status.phase == PreviewPhase::Paused)
        }));
        assert_ne!(
            input.try_send(config_access_unit(77, 1)),
            PreviewSendResult::Sent,
            "隐藏预览不应把 AU 放入旁路队列"
        );
        std::thread::sleep(Duration::from_millis(100));
        assert!(worker.active_child.lock().unwrap().is_none());
        let statuses = sink.statuses.lock().unwrap().clone();
        assert!(statuses
            .iter()
            .all(|status| status.phase != PreviewPhase::Unavailable));
        assert_eq!(sink.latest_status().unwrap().h264_input_frames, 0);
        worker.stop();
    }

    #[test]
    fn 短暂无需求由入口锁存后仍暂停恢复且不消耗失败预算() {
        let sink = Arc::new(InitialStatusGateSink::new());
        let sink_trait: Arc<dyn PreviewSink> = sink.clone();
        let (input, mut worker) =
            spawn_preview_worker("preview-ffmpeg-must-not-start".into(), 10, 77, sink_trait);

        // Hold the supervisor before its first demand poll, then make the
        // inactive interval exist only in the producer's observation.
        sink.wait_initial_status();
        sink.set_active(false);
        assert!(!input.is_active());
        sink.set_active(true);
        sink.release_initial_status();

        assert!(wait_until(Duration::from_secs(1), || {
            sink.has_pause_resume_transition()
        }));
        let statuses = sink.statuses();
        assert!(statuses.iter().all(|status| {
            status.recoveries == 0
                && status.phase != PreviewPhase::Recovering
                && status.phase != PreviewPhase::Unavailable
        }));
        assert!(worker.active_child.lock().unwrap().is_none());
        worker.stop();
    }

    #[test]
    fn 隐藏预览释放decoder且恢复只等新的配置关键帧() {
        let Some(ffmpeg) = crate::source::ffmpeg_bin() else {
            return;
        };
        let output = Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=2:size=320x180:rate=10",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-x264-params",
                "repeat-headers=1:keyint=10:min-keyint=10:scenecut=0:slices=1:sliced-threads=0",
                "-bf",
                "0",
                "-f",
                "h264",
                "pipe:1",
            ])
            .output()
            .expect("应能生成 H.264 pause/resume fixture");
        assert!(output.status.success());

        #[derive(Default)]
        struct Collector(Mutex<Vec<crate::source::Frame>>);
        impl crate::source::FrameSender for Collector {
            fn send_frame(&self, frame: crate::source::Frame) {
                self.0.lock().unwrap().push(frame);
            }
        }
        let collector = Collector::default();
        let mut raw = output.stdout;
        raw.extend_from_slice(&[0, 0, 1, 9, 0xf0]);
        crate::source::drain_frames(&mut raw, &collector);
        let frames = collector.0.into_inner().unwrap();
        let Some(config_frame) = frames.iter().find(|frame| frame.key_frame) else {
            panic!("fixture should contain a config keyframe");
        };
        let Some(delta_frame) = frames.iter().find(|frame| !frame.key_frame) else {
            panic!("fixture should contain a non-keyframe");
        };

        let sink = Arc::new(RecordingSink::default());
        let sink_trait: Arc<dyn PreviewSink> = sink.clone();
        let (input, mut worker) = spawn_preview_worker(ffmpeg, 10, 77, sink_trait);
        let config =
            CapturedAccessUnit::new(77, 1, config_frame.data.clone(), config_frame.key_frame, 1);
        assert_eq!(input.try_send(config.clone()), PreviewSendResult::Sent);
        assert!(wait_until(Duration::from_secs(4), || {
            worker.active_child.lock().unwrap().is_some()
        }));

        sink.set_active(false);
        assert!(wait_until(Duration::from_secs(2), || {
            sink.latest_status()
                .is_some_and(|status| status.phase == PreviewPhase::Paused)
                && worker.active_child.lock().unwrap().is_none()
        }));
        let paused = sink.latest_status().unwrap();
        let frames_at_pause = sink.frames.lock().unwrap().len();
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(sink.frames.lock().unwrap().len(), frames_at_pause);
        assert!(sink
            .statuses
            .lock()
            .unwrap()
            .iter()
            .all(|status| status.phase != PreviewPhase::Unavailable));

        sink.set_active(true);
        assert!(wait_until(Duration::from_secs(2), || {
            sink.latest_status().is_some_and(|status| {
                status.phase == PreviewPhase::WaitingKeyframe
                    && status.preview_generation > paused.preview_generation
            })
        }));
        let delta = CapturedAccessUnit::new(77, 2, delta_frame.data.clone(), false, 2);
        assert_eq!(input.try_send(delta), PreviewSendResult::Sent);
        std::thread::sleep(Duration::from_millis(250));
        assert!(worker.active_child.lock().unwrap().is_none());

        let resumed_config = CapturedAccessUnit::new(77, 3, config.data.clone(), true, 3);
        assert_eq!(resumed_config.sequence, 3);
        assert_eq!(input.try_send(resumed_config), PreviewSendResult::Sent);
        for (index, frame) in frames
            .iter()
            .filter(|frame| !frame.key_frame)
            .take(2)
            .enumerate()
        {
            let follow_up = CapturedAccessUnit::new(
                77,
                index as u64 + 4,
                frame.data.clone(),
                false,
                index as u64 + 4,
            );
            assert_eq!(input.try_send(follow_up), PreviewSendResult::Sent);
        }
        assert!(wait_until(Duration::from_secs(4), || {
            sink.frames.lock().unwrap().len() > frames_at_pause
        }));
        let resumed_frames = sink.frames.lock().unwrap()[frames_at_pause..].to_vec();
        assert!(!resumed_frames.is_empty(), "恢复后应收到实际 JPEG");
        assert!(resumed_frames
            .iter()
            .all(|frame| frame.generation == 77 && frame.sequence >= 3));
        let resumed_status = sink.latest_status().unwrap();
        assert!(resumed_status.preview_generation > paused.preview_generation);
        assert_eq!(resumed_status.recoveries, paused.recoveries);
        worker.stop();
    }

    #[cfg(windows)]
    #[test]
    fn windows隐藏期间大输入不会阻塞supervisor回收decoder() {
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 30",
        ]);
        let sink = Arc::new(PauseOnStartingSink::new());
        let sink_trait: Arc<dyn PreviewSink> = sink.clone();
        let (input, mut worker) =
            spawn_preview_worker_with_command_for_test(command, 10, 77, sink_trait);

        let mut data = vec![0, 0, 0, 1, 7, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0, 1, 5, 3];
        data.resize(4 * 1024 * 1024, 0xa5);
        assert_eq!(
            input.try_send(CapturedAccessUnit::new(77, 1, data, true, 1)),
            PreviewSendResult::Sent
        );

        assert!(wait_until(Duration::from_secs(2), || {
            sink.latest_status()
                .is_some_and(|status| status.phase == PreviewPhase::Paused)
                && worker.active_child.lock().unwrap().is_none()
        }));
        assert!(sink
            .latest_status()
            .is_some_and(|status| status.phase == PreviewPhase::Paused));
        worker.stop();
    }

    #[cfg(windows)]
    #[test]
    fn windows停读期间单个大输入按写入期限回收decoder() {
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 30",
        ]);
        let sink = Arc::new(RecordingSink::default());
        let sink_trait: Arc<dyn PreviewSink> = sink.clone();
        let (input, mut worker) =
            spawn_preview_worker_with_command_for_test(command, 10, 77, sink_trait);

        let mut data = vec![0, 0, 0, 1, 7, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0, 1, 5, 3];
        data.resize(4 * 1024 * 1024, 0xa5);
        let started_at = Instant::now();
        assert_eq!(
            input.try_send(CapturedAccessUnit::new(77, 1, data, true, 1)),
            PreviewSendResult::Sent
        );

        assert!(wait_until(Duration::from_secs(2), || {
            worker.active_child.lock().unwrap().is_none()
                && sink.latest_status().is_some_and(|status| {
                    status.phase == PreviewPhase::Recovering && status.recoveries == 1
                })
        }));
        assert!(started_at.elapsed() < Duration::from_secs(2));
        assert!(sink.is_active(), "写入超时不应伪造隐藏状态");
        worker.stop();
    }

    #[test]
    fn 自动恢复推进预览代际且第三次连续失败进入不可用() {
        let sink = Arc::new(RecordingSink::default());
        let sink_trait: Arc<dyn PreviewSink> = sink.clone();
        let active_child = Arc::new(Mutex::new(None));
        let mut attempt = None;
        let mut status = PreviewStatus::new(7, PreviewPhase::Playing);
        let mut failures = 0;
        let mut retry_not_before = None;

        register_failure(
            "first".into(),
            &mut attempt,
            &active_child,
            &sink_trait,
            &mut status,
            &mut failures,
            &mut retry_not_before,
        );
        assert_eq!(status.preview_generation, 2);
        assert_eq!(status.phase, PreviewPhase::Recovering);

        register_failure(
            "second".into(),
            &mut attempt,
            &active_child,
            &sink_trait,
            &mut status,
            &mut failures,
            &mut retry_not_before,
        );
        assert_eq!(status.preview_generation, 3);
        assert_eq!(status.phase, PreviewPhase::Recovering);

        register_failure(
            "third".into(),
            &mut attempt,
            &active_child,
            &sink_trait,
            &mut status,
            &mut failures,
            &mut retry_not_before,
        );
        assert_eq!(status.preview_generation, 3);
        assert_eq!(status.phase, PreviewPhase::Unavailable);
        assert_eq!(failures, MAX_AUTOMATIC_RECOVERIES + 1);
    }

    #[test]
    fn 独立worker把h264访问单元逐帧转成mjpeg() {
        let Some(ffmpeg) = crate::source::ffmpeg_bin() else {
            return;
        };
        let output = Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=1:size=1920x1080:rate=10",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-x264-params",
                "repeat-headers=1:keyint=1:min-keyint=1:scenecut=0:slices=1:sliced-threads=0",
                "-bf",
                "0",
                "-f",
                "h264",
                "pipe:1",
            ])
            .output()
            .expect("应能生成 H.264 fixture");
        assert!(output.status.success());

        #[derive(Default)]
        struct Collector(Mutex<Vec<crate::source::Frame>>);
        impl crate::source::FrameSender for Collector {
            fn send_frame(&self, frame: crate::source::Frame) {
                self.0.lock().unwrap().push(frame);
            }
        }
        let collector = Collector::default();
        let mut raw = output.stdout;
        // drain_frames 保留最后一个未闭合 NAL；追加 AUD 起始码让尾帧完成。
        raw.extend_from_slice(&[0, 0, 1, 9, 0xf0]);
        crate::source::drain_frames(&mut raw, &collector);
        let frames = collector.0.into_inner().unwrap();
        assert!(frames.len() >= 8, "应切出接近 10 帧，实际 {}", frames.len());

        let sink = Arc::new(RecordingSink::default());
        let sink_trait: Arc<dyn PreviewSink> = sink.clone();
        let (input, mut worker) = spawn_preview_worker(ffmpeg, 10, 77, sink_trait);
        let input_count = frames.len();
        for (index, frame) in frames.into_iter().enumerate() {
            let access_unit = CapturedAccessUnit::new(
                77,
                index as u64 + 1,
                frame.data,
                frame.key_frame,
                index as u64 + 100,
            );
            assert_eq!(input.try_send(access_unit), PreviewSendResult::Sent);
            std::thread::sleep(Duration::from_millis(30));
        }

        // Annex-B decoder 需要下一帧起始码才能确认当前帧结束；持续流允许固定一帧在途。
        let expected_output = input_count.saturating_sub(1);
        let deadline = Instant::now() + Duration::from_secs(4);
        while sink.frames.lock().unwrap().len() < expected_output && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        worker.stop();
        let frames = sink.frames.lock().unwrap();
        let statuses = sink.statuses.lock().unwrap().clone();
        assert_eq!(frames.len(), expected_output, "statuses: {statuses:#?}");
        assert!(frames
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence));
        assert!(frames
            .iter()
            .all(|frame| frame.data.starts_with(&[0xff, 0xd8, 0xff])));
        for frame in frames.iter() {
            // Read the JPEG SOF segment to verify the worker's actual output pixels.
            let mut offset = 2;
            let dimensions = loop {
                assert!(offset + 8 < frame.data.len(), "missing JPEG SOF");
                assert_eq!(frame.data[offset], 0xff);
                let marker = frame.data[offset + 1];
                let length =
                    u16::from_be_bytes([frame.data[offset + 2], frame.data[offset + 3]]) as usize;
                if matches!(marker, 0xc0..=0xc2) {
                    break (
                        u16::from_be_bytes([frame.data[offset + 7], frame.data[offset + 8]]),
                        u16::from_be_bytes([frame.data[offset + 5], frame.data[offset + 6]]),
                    );
                }
                offset += 2 + length;
            };
            assert_eq!(
                dimensions,
                (1920, 1080),
                "preview must preserve source detail"
            );
        }
        assert!(statuses
            .iter()
            .any(|status| status.phase == PreviewPhase::Playing));
    }
}
