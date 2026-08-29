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

const INPUT_CAPACITY: usize = 2;
const STDERR_TAIL_LINES: usize = 32;
const OUTPUT_WATCHDOG: Duration = Duration::from_secs(2);
const WRITE_DEADLINE: Duration = Duration::from_millis(500);
const STOP_DEADLINE: Duration = Duration::from_secs(2);
const MAX_AUTOMATIC_RECOVERIES: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewSendResult {
    Sent,
    Dropped,
    Stale,
    Disconnected,
}

/// 主采集 reader 使用的有界、非阻塞预览入口。
#[derive(Clone)]
pub struct PreviewWorkerInput {
    generation: u64,
    tx: mpsc::SyncSender<CapturedAccessUnit>,
    discontinuity: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
}

impl PreviewWorkerInput {
    pub fn try_send(&self, access_unit: CapturedAccessUnit) -> PreviewSendResult {
        if access_unit.generation != self.generation {
            return PreviewSendResult::Stale;
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
    let (input, rx) = preview_input_channel(generation, INPUT_CAPACITY);
    let discontinuity = Arc::clone(&input.discontinuity);
    let dropped = Arc::clone(&input.dropped);
    let stop = Arc::new(AtomicBool::new(false));
    let active_child = Arc::new(Mutex::new(None));
    let control = Arc::new(PreviewControl::default());

    let supervisor_stop = Arc::clone(&stop);
    let supervisor_child = Arc::clone(&active_child);
    let supervisor_control = Arc::clone(&control);
    let join = std::thread::spawn(move || {
        run_supervisor(
            &ffmpeg,
            fps.max(1),
            generation,
            sink,
            rx,
            discontinuity,
            dropped,
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

fn preview_input_channel(
    generation: u64,
    capacity: usize,
) -> (PreviewWorkerInput, mpsc::Receiver<CapturedAccessUnit>) {
    let (tx, rx) = mpsc::sync_channel(capacity.max(1));
    let discontinuity = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicU64::new(0));
    (
        PreviewWorkerInput {
            generation,
            tx,
            discontinuity,
            dropped,
        },
        rx,
    )
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
    sink: Arc<dyn PreviewSink>,
    rx: mpsc::Receiver<CapturedAccessUnit>,
    discontinuity: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
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
                status.reason = Some("preview input discontinuity; waiting for SPS/PPS/IDR".into());
                sink.status(status.clone());
            }
        }

        let mut attempt_failure = None;
        if let Some(current) = attempt.as_mut() {
            while let Ok(event) = current.events.try_recv() {
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
                    AttemptEvent::UnexpectedOutput => {
                        attempt_failure = Some(
                            "preview FFmpeg produced JPEG without a matching H.264 input".into(),
                        );
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

            if attempt_failure.is_none() {
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

        let access_unit = match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(access_unit) => access_unit,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if access_unit.generation != generation {
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
            status.reason = Some("waiting for SPS/PPS/IDR after recovery".into());
            sink.status(status.clone());
        }
        if attempt.is_none() {
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
                Arc::clone(&active_child),
                Arc::clone(&stop),
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
            if let Err(reason) = current.feed(&access_unit, &stop) {
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
    Output { meta: FrameMeta, data: Vec<u8> },
    UnexpectedOutput,
    ReaderFailed(String),
    ReaderEnded,
}

struct PreviewAttempt {
    pid: u32,
    preview_generation: u64,
    stdin: Option<ChildStdin>,
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
        active_child: Arc<Mutex<Option<Child>>>,
        stop: Arc<AtomicBool>,
    ) -> std::result::Result<Self, String> {
        let args = preview_worker_args(fps, 480);
        let mut child = Command::new(ffmpeg)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("failed to start preview FFmpeg: {error}"))?;
        let pid = child.id();
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to open preview FFmpeg stdin".to_string())?;
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
            stdin: Some(stdin),
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
        access_unit: &CapturedAccessUnit,
        stop: &AtomicBool,
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

    fn stop(&mut self, active_child: &Arc<Mutex<Option<Child>>>) {
        self.stdin.take();
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
        "h264".into(),
        "-i".into(),
        "pipe:0".into(),
        "-map".into(),
        "0:v:0".into(),
        "-an".into(),
        "-vf".into(),
        format!("scale={}:-2,format=yuvj420p", width.max(160)),
        "-c:v".into(),
        "mjpeg".into(),
        "-threads:v".into(),
        "1".into(),
        "-q:v".into(),
        "8".into(),
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

#[cfg(not(unix))]
fn set_nonblocking(_stdin: &mut ChildStdin) -> std::io::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingSink {
        frames: Mutex<Vec<PreviewJpeg>>,
        statuses: Mutex<Vec<PreviewStatus>>,
    }

    impl PreviewSink for RecordingSink {
        fn publish(&self, frame: PreviewJpeg) {
            self.frames.lock().unwrap().push(frame);
        }

        fn status(&self, status: PreviewStatus) {
            self.statuses.lock().unwrap().push(status);
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
    fn preview入口满载时立即丢弃且标记discontinuity() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let discontinuity = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicU64::new(0));
        let input = PreviewWorkerInput {
            generation: 7,
            tx,
            discontinuity: Arc::clone(&discontinuity),
            dropped: Arc::clone(&dropped),
        };
        assert_eq!(input.try_send(access_unit(7, 1)), PreviewSendResult::Sent);
        assert_eq!(
            input.try_send(access_unit(7, 2)),
            PreviewSendResult::Dropped
        );
        assert!(discontinuity.load(Ordering::Acquire));
        assert_eq!(dropped.load(Ordering::Acquire), 1);
        assert_eq!(input.try_send(access_unit(8, 3)), PreviewSendResult::Stale);
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
                "testsrc=duration=1:size=160x120:rate=10",
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
        assert!(statuses
            .iter()
            .any(|status| status.phase == PreviewPhase::Playing));
    }
}
