//! daemon 子进程管理与 JSON-RPC 2.0 IPC 桥接。
//!
//! 生命周期:
//!   1. Tauri app 启动时调 `Daemon::spawn`,拉起 `uvp-daemon --stdio` 子进程。
//!   2. stdin/stdout 走 tokio async pipe;stderr inherit(daemon 的 slog 打到 Tauri 控制台便于调试)。
//!   3. Rust 侧维护 `pending: HashMap<request_id, oneshot::Sender>`,收到匹配 id 的 response 就 resolve。
//!   4. 收到 method 字段(无 id)视为 event → `app.emit(<method>, payload)` 转发到前端。
//!
//! 关闭:窗口关闭前 Tauri command 侧发 stop_device,然后 `Daemon::shutdown` 会 kill 子进程。

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex};

/// JSON-RPC 帧的 id 类型(daemon 侧我们统一发 u64)。
type RequestId = u64;

/// 单次请求-响应超时:5 秒。daemon start_device 内部 REGISTER 用的是自己的 15s 超时,
/// 但 handler 是 async 立即返 request_id,所以 5s 足够拿到 handler 的立即回执。
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// daemon 子进程句柄,存到 Tauri State,所有 command 走这里。
pub struct Daemon {
    stdin: Mutex<Option<ChildStdin>>, // 写侧用 Mutex 串行化多请求 write
    child: Mutex<Option<Child>>,      // 用于 shutdown 时 kill
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<RequestId, oneshot::Sender<Value>>>>,
}

impl Daemon {
    /// 拉起 daemon 子进程。
    ///
    /// binary 由调用方给出(方便测试 mock 与生产切换)。生产由 lib.rs 定位:
    /// dev 模式相对路径 apps/daemon/uvp-daemon,release 打包后走 sidecar。
    pub async fn spawn(binary_path: std::path::PathBuf, app: AppHandle) -> Result<Self> {
        tracing::info!("spawning daemon: {}", binary_path.display());
        let mut child = Command::new(&binary_path)
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // daemon slog 走 Tauri 控制台
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to spawn {}", binary_path.display()))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("daemon stdin missing"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("daemon stdout missing"))?;

        let pending: Arc<Mutex<HashMap<RequestId, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // 起 stdout reader task:按行读 → 分派到 pending 或 app.emit
        {
            let pending = Arc::clone(&pending);
            let app_clone = app.clone();
            tokio::spawn(async move {
                if let Err(err) = read_stdout_loop(stdout, pending, app_clone).await {
                    tracing::error!("daemon stdout reader error: {err:?}");
                }
                tracing::info!("daemon stdout loop exited");
            });
        }

        // TODO(R2): 子进程崩溃监测。当前依赖 stdout reader 遇 EOF 判定,
        // 更完善的做法是 tokio::spawn 一个 child.wait() 任务,退出后 emit
        // device_state=Failed 让 UI 感知。留待 M3 再补,M2 阶段前端已订阅 EOF 时
        // stdout 关闭 → 事件流断,用户会看到"注册中"卡住 = 隐式信号。

        Ok(Self {
            stdin: Mutex::new(Some(stdin)),
            child: Mutex::new(Some(child)),
            next_id: AtomicU64::new(1),
            pending,
        })
    }

    /// 发一条 JSON-RPC 请求,阻塞等响应(带超时)。
    ///
    /// 由 Tauri command 处理器调,同步返 result 到前端 invoke Promise。
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let frame = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id,
        });
        let mut line = serde_json::to_vec(&frame)?;
        line.push(b'\n');

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        // 写 stdin(串行化)
        {
            let mut stdin_guard = self.stdin.lock().await;
            let stdin = stdin_guard
                .as_mut()
                .ok_or_else(|| anyhow!("daemon stdin closed"))?;
            stdin.write_all(&line).await.context("write stdin")?;
            stdin.flush().await.context("flush stdin")?;
        }

        // 等响应 or 超时
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err(anyhow!("daemon channel dropped (crash?)")),
            Err(_) => {
                // 清 pending,避免 memory leak
                self.pending.lock().await.remove(&id);
                Err(anyhow!("daemon call timeout after {:?}", REQUEST_TIMEOUT))
            }
        }
    }

    /// 优雅关闭:关 stdin(daemon 侧 stdio EOF → 正常退出)→ 等 3s → kill。
    pub async fn shutdown(&self) {
        // 关 stdin 让 daemon 主循环 EOF 退出
        {
            let mut stdin_guard = self.stdin.lock().await;
            *stdin_guard = None;
        }
        let mut child_guard = self.child.lock().await;
        if let Some(mut child) = child_guard.take() {
            let wait = tokio::time::timeout(std::time::Duration::from_secs(3), child.wait());
            match wait.await {
                Ok(Ok(status)) => tracing::info!("daemon exited gracefully: {status}"),
                Ok(Err(err)) => tracing::warn!("daemon wait error: {err}"),
                Err(_) => {
                    tracing::warn!("daemon shutdown timeout, killing");
                    let _ = child.kill().await;
                }
            }
        }
    }
}

/// stdout reader 循环:按 JSON-lines 解帧,分派 response / event。
async fn read_stdout_loop(
    stdout: ChildStdout,
    pending: Arc<Mutex<HashMap<RequestId, oneshot::Sender<Value>>>>,
    app: AppHandle,
) -> Result<()> {
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!("daemon stdout non-JSON line: {err} | raw={line}");
                continue;
            }
        };

        // 判定 response vs notification:含 id 字段 → response, 否则 event。
        if let Some(id_val) = value.get("id") {
            // id 可能是 null(parse error 响应),视为不匹配 pending,忽略
            if id_val.is_null() {
                tracing::warn!("daemon returned response with null id: {value}");
                continue;
            }
            let id = match id_val.as_u64() {
                Some(n) => n,
                None => {
                    tracing::warn!("daemon returned non-u64 id: {id_val}");
                    continue;
                }
            };
            let mut pending_guard = pending.lock().await;
            if let Some(sender) = pending_guard.remove(&id) {
                let _ = sender.send(value);
            } else {
                tracing::warn!("orphan response for id={id}: {value}");
            }
        } else if let Some(method) = value.get("method").and_then(|v| v.as_str()) {
            // event notification: emit 到前端
            let payload = value.get("params").cloned().unwrap_or(Value::Null);
            forward_event(&app, method, payload);
        } else {
            tracing::warn!("daemon frame is neither response nor event: {value}");
        }
    }
    Ok(())
}

/// 把 daemon event 转发到前端 Tauri event bus。
///
/// 按 method 派发到不同 event name 便于前端订阅:
///   device_state → "device_state"
///   sip_trace    → "sip_trace"
///   其他        → "daemon_event" + 原 method 挂在 payload.method
fn forward_event(app: &AppHandle, method: &str, params: Value) {
    match method {
        "device_state" => {
            // 前端 App.vue 订阅 "device_state",payload 是 state 字符串或 object
            // daemon 侧 handlers.publishState 塞的是 map (state, registered_expires_secs, ...),
            // 老 App.vue 用 e.payload 直接当 string 用;为了兼容,如果 payload.state 存在就把它抽出来发。
            if let Some(state_val) = params.get("state") {
                // 主事件用字符串(App.vue 简单订阅),同时 sip_trace/其他细节前端另做
                let _ = app.emit("device_state", state_val.clone());
                // 附带完整 payload,便于将来前端要 expires 等字段
                let _ = app.emit("device_state_full", params);
            } else {
                let _ = app.emit("device_state", params);
            }
        }
        "sip_trace" => {
            // 前端 Simulator.vue 订阅 "sip_trace",payload 结构见 daemon tracerToIPC。
            // 需要补 ts_ms 若前端形态期望毫秒时间戳(daemon 已经塞了 ts_ms)。
            let _ = app.emit("sip_trace", params);
        }
        "bulk_stats" => {
            // 前端可选订阅,用于展示丢包量
            let _ = app.emit("bulk_stats", params);
        }
        _ => {
            let mut wrapped = serde_json::Map::new();
            wrapped.insert("method".into(), Value::String(method.into()));
            wrapped.insert("params".into(), params);
            let _ = app.emit("daemon_event", Value::Object(wrapped));
        }
    }
}

