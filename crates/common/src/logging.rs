//! 日志初始化 + 运行时级别调整 + 实时日志分发。
//!
//! 统一用 `tracing`。启动时 [`init`] 装订阅器:
//! - 用 `reload` 层承载级别 filter,[`set_level`] 可运行时热改级别(无需重启);
//! - 自定义 [`SinkLayer`] 把每条日志格式化后写入环形缓冲(供 UI 打开时拉历史)
//!   并推给注册的实时回调(desktop 层据此发 Tauri 事件给前端日志页)。

use std::sync::{Arc, Mutex, OnceLock, RwLock};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
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

const BUFFER_CAP: usize = 2000;

fn buffer() -> &'static Mutex<std::collections::VecDeque<LogLine>> {
    BUFFER.get_or_init(|| Mutex::new(std::collections::VecDeque::with_capacity(BUFFER_CAP)))
}
fn callback_slot() -> &'static RwLock<Option<Arc<LogCallback>>> {
    CALLBACK.get_or_init(|| RwLock::new(None))
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
        if let Ok(slot) = callback_slot().read() {
            if let Some(cb) = slot.as_ref() {
                cb(&line);
            }
        }
    }
}

/// 初始化全局日志订阅器。重复调用安全(二次初始化被忽略)。
pub fn init() {
    let base = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let (filter, handle) = reload::Layer::new(base);
    let _ = RELOAD.set(handle);
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false))
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
