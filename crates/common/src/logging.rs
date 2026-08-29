//! 日志初始化 + 运行时级别调整 + 实时日志分发。
//!
//! 统一用 `tracing`。启动时 [`init`] 装订阅器:
//! - 用 `reload` 层承载级别 filter,[`set_level`] 可运行时热改级别(无需重启);
//! - 自定义 [`SinkLayer`] 把每条日志格式化后写入环形缓冲(供 UI 打开时拉历史)
//!   并推给注册的实时回调(desktop 层据此发 Tauri 事件给前端日志页)。

use std::io::{self, Write};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::Context;
use tracing_subscriber::{fmt, prelude::*, reload, EnvFilter, Layer, Registry};

/// 一条结构化日志(供前端展示)。
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogLine {
    /// 毫秒时间戳。
    pub ts_ms: u64,
    /// 级别:ERROR/WARN/INFO/DEBUG/TRACE。
    pub level: String,
    /// 目标模块(target)。
    pub target: String,
    /// 日志正文(message 字段)。
    pub message: String,
}

/// 实时日志回调:每产生一条日志调用一次。desktop 用它发 Tauri 事件。
type LogCallback = Box<dyn Fn(&LogLine) + Send + Sync>;

/// 全局:级别 reload 句柄。
static RELOAD: OnceLock<reload::Handle<EnvFilter, Registry>> = OnceLock::new();
/// 全局:最近日志环形缓冲(容量固定,满则丢最旧)。
static BUFFER: OnceLock<Mutex<std::collections::VecDeque<LogLine>>> = OnceLock::new();
/// 全局:实时回调(可后注册)。
static CALLBACK: OnceLock<RwLock<Option<Arc<LogCallback>>>> = OnceLock::new();
/// 实时回调的有界异步入口；日志生产线程永不直接执行 UI/文件回调。
static CALLBACK_TX: OnceLock<SyncSender<LogLine>> = OnceLock::new();

const BUFFER_CAP: usize = 2000;
const CONSOLE_QUEUE_CAP: usize = 1024;
const CALLBACK_QUEUE_CAP: usize = 2048;

/// `tracing_subscriber::fmt` 的非阻塞控制台出口。
///
/// GUI 的采集、注销和 IPC 线程绝不能直接等待终端消费 stdout。终端暂停或消费过慢时，
/// 只丢控制台副本；结构化环形缓冲和 desktop 文件回调仍由 [`SinkLayer`] 处理。
#[derive(Clone)]
struct NonBlockingConsole {
    tx: SyncSender<Vec<u8>>,
}

impl NonBlockingConsole {
    fn spawn() -> Self {
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(CONSOLE_QUEUE_CAP);
        let _ = std::thread::Builder::new()
            .name("uvp-log-console".into())
            .spawn(move || {
                let stdout = io::stdout();
                let mut stdout = stdout.lock();
                while let Ok(bytes) = rx.recv() {
                    let _ = stdout.write_all(&bytes);
                    let _ = stdout.flush();
                }
            });
        Self { tx }
    }

    #[cfg(test)]
    fn from_sender(tx: SyncSender<Vec<u8>>) -> Self {
        Self { tx }
    }
}

struct NonBlockingConsoleWriter {
    tx: SyncSender<Vec<u8>>,
    bytes: Vec<u8>,
}

impl NonBlockingConsoleWriter {
    fn submit(&mut self) {
        if !self.bytes.is_empty() {
            let _ = self.tx.try_send(std::mem::take(&mut self.bytes));
        }
    }
}

impl Write for NonBlockingConsoleWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.submit();
        Ok(())
    }
}

impl Drop for NonBlockingConsoleWriter {
    fn drop(&mut self) {
        self.submit();
    }
}

impl<'a> MakeWriter<'a> for NonBlockingConsole {
    type Writer = NonBlockingConsoleWriter;

    fn make_writer(&'a self) -> Self::Writer {
        NonBlockingConsoleWriter {
            tx: self.tx.clone(),
            bytes: Vec::with_capacity(256),
        }
    }
}

fn buffer() -> &'static Mutex<std::collections::VecDeque<LogLine>> {
    BUFFER.get_or_init(|| Mutex::new(std::collections::VecDeque::with_capacity(BUFFER_CAP)))
}
fn callback_slot() -> &'static RwLock<Option<Arc<LogCallback>>> {
    CALLBACK.get_or_init(|| RwLock::new(None))
}

fn callback_dispatcher() -> &'static SyncSender<LogLine> {
    CALLBACK_TX.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<LogLine>(CALLBACK_QUEUE_CAP);
        let _ = std::thread::Builder::new()
            .name("uvp-log-callback".into())
            .spawn(move || {
                while let Ok(line) = rx.recv() {
                    let callback = callback_slot()
                        .read()
                        .ok()
                        .and_then(|slot| slot.as_ref().cloned());
                    if let Some(callback) = callback {
                        callback(&line);
                    }
                }
            });
        tx
    })
}

/// 从 Event 提取 message 字段(及其它字段拼接)。
struct MessageVisitor {
    message: String,
}
impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let mut s = format!("{value:?}");
            // 去掉 Debug 包裹的引号(message 通常是字符串字面量)。
            if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
                s = s[1..s.len() - 1].to_string();
            }
            if self.message.is_empty() {
                self.message = s;
            } else {
                self.message = format!("{s} {}", self.message);
            }
        } else {
            let kv = format!("{}={value:?}", field.name());
            if self.message.is_empty() {
                self.message = kv;
            } else {
                self.message.push(' ');
                self.message.push_str(&kv);
            }
        }
    }
}

/// 自定义 layer:把日志写环形缓冲 + 推实时回调。
struct SinkLayer;

impl<S: Subscriber> Layer<S> for SinkLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut v = MessageVisitor {
            message: String::new(),
        };
        event.record(&mut v);
        let line = LogLine {
            ts_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            level: meta.level().to_string(),
            target: meta.target().to_string(),
            message: v.message,
        };
        if let Ok(mut buf) = buffer().lock() {
            if buf.len() >= BUFFER_CAP {
                buf.pop_front();
            }
            buf.push_back(line.clone());
        }
        let _ = callback_dispatcher().try_send(line);
    }
}

/// 初始化全局日志订阅器。重复调用安全(二次初始化被忽略)。
pub fn init() {
    let base = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let (filter, handle) = reload::Layer::new(base);
    let _ = RELOAD.set(handle);
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_target(false)
                .with_writer(NonBlockingConsole::spawn()),
        )
        .with(SinkLayer)
        .try_init();
}

/// 运行时热改全局日志级别。`level` 取 error/warn/info/debug/trace(大小写不敏感),
/// 也接受完整 EnvFilter 语法(如 `info,media_rtp=debug`)。返回是否成功。
pub fn set_level(level: &str) -> bool {
    let Some(handle) = RELOAD.get() else {
        return false;
    };
    let Ok(new_filter) = EnvFilter::try_new(level) else {
        return false;
    };
    handle.reload(new_filter).is_ok()
}

/// 当前生效的日志级别文本。
pub fn current_level() -> String {
    RELOAD
        .get()
        .and_then(|h| h.with_current(|f| f.to_string()).ok())
        .unwrap_or_else(|| "info".into())
}

/// 注册实时日志回调(覆盖上一次)。desktop 用它把日志发给前端。
pub fn set_callback<F: Fn(&LogLine) + Send + Sync + 'static>(cb: F) {
    if let Ok(mut slot) = callback_slot().write() {
        *slot = Some(Arc::new(Box::new(cb)));
    }
}

/// 取最近缓冲的日志(供前端打开日志页时拉历史)。`max` 限制返回条数(取最新)。
pub fn recent(max: usize) -> Vec<LogLine> {
    buffer()
        .lock()
        .map(|buf| {
            let n = buf.len().min(max);
            buf.iter().skip(buf.len() - n).cloned().collect()
        })
        .unwrap_or_default()
}

/// 级别文本 → tracing Level(校验用)。
pub fn parse_level(s: &str) -> Option<Level> {
    match s.to_ascii_lowercase().as_str() {
        "error" => Some(Level::ERROR),
        "warn" => Some(Level::WARN),
        "info" => Some(Level::INFO),
        "debug" => Some(Level::DEBUG),
        "trace" => Some(Level::TRACE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{callback_dispatcher, set_callback, LogLine, NonBlockingConsole};
    use std::io::Write;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use tracing_subscriber::fmt::MakeWriter;

    #[test]
    fn 控制台背压时产生日志的线程不能被阻塞() {
        let (tx, rx) = mpsc::sync_channel(1);
        let console = NonBlockingConsole::from_sender(tx);

        let mut first = console.make_writer();
        first.write_all(b"first").unwrap();
        drop(first);

        let started = Instant::now();
        for _ in 0..10_000 {
            let mut writer = console.make_writer();
            writer.write_all(b"overflow").unwrap();
        }

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "日志队列满载时 write/drop 必须立即返回"
        );
        assert_eq!(rx.try_recv().unwrap(), b"first");
    }

    #[test]
    fn 实时回调阻塞时产生日志的线程不能被阻塞() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = std::sync::Arc::new(std::sync::Mutex::new(release_rx));
        let first = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        set_callback(move |_| {
            if first.swap(false, std::sync::atomic::Ordering::AcqRel) {
                let _ = entered_tx.send(());
                let _ = release_rx.lock().unwrap().recv();
            }
        });
        let line = LogLine {
            ts_ms: 1,
            level: "INFO".into(),
            target: "test".into(),
            message: "blocked callback".into(),
        };
        callback_dispatcher().try_send(line.clone()).unwrap();
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("回调线程应收到首条日志");

        let started = Instant::now();
        for _ in 0..10_000 {
            let _ = callback_dispatcher().try_send(line.clone());
        }
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "回调队列满载时日志生产线程必须立即返回"
        );

        release_tx.send(()).unwrap();
        set_callback(|_| {});
    }
}
