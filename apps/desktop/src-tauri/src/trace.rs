use ipc_error::{IpcError, IpcResult};

use std::sync::Arc;

use tauri::{AppHandle, Emitter};

use super::*;

/// SIP 信令追踪观察者(FR-43):把每条收发报文结构化为 `sip_trace` 事件推给前端。
pub(crate) struct TraceEmitter {
    pub(crate) app: AppHandle,
}

/// 追踪面板里需要脱敏的头域(小写比较)。
///
/// `Authorization` / `Proxy-Authorization` 携带 Digest 的 `response=` 摘要,
/// 而同一条报文里 `username`/`realm`/`nonce`/`uri`/`method` 全是已知明文 ——
/// 等于把一份可离线爆破的口令哈希连同全部盐值一起交出去。
/// `WWW-Authenticate` 里的 nonce 本身不是秘密,但一并遮掉可以避免用户
/// 把整段信令贴进工单时无意泄露一次挑战-响应对。
pub(crate) const REDACTED_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "www-authenticate",
    "proxy-authenticate",
    "authentication-info",
];

/// 遮罩标记。刻意保留头域名与"有值"这个事实 —— 排障需要知道
/// "这条 REGISTER 到底带没带鉴权",只是不需要知道摘要是什么。
pub(crate) const REDACTED_MARK: &str = "<已脱敏>";

/// 把 SIP 报文文本里的鉴权头值替换成遮罩。
///
/// 只改头部不动正文:MANSCDP 正文里没有凭据(目录项的 `Password` 我们从不输出),
/// 而正文往下还要给用户看 XML。按行处理,遇到头部与正文之间的空行即停。
pub(crate) fn redact_sip_credentials(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_headers = true;
    for line in raw.split_inclusive('\n') {
        // 头部结束:只含 CR/LF 的空行。
        if in_headers && line.trim_end_matches(['\r', '\n']).is_empty() {
            in_headers = false;
            out.push_str(line);
            continue;
        }
        if in_headers {
            if let Some((name, _)) = line.split_once(':') {
                if REDACTED_HEADERS.contains(&name.trim().to_ascii_lowercase().as_str()) {
                    let eol = &line[line.trim_end_matches(['\r', '\n']).len()..];
                    out.push_str(name);
                    out.push_str(": ");
                    out.push_str(REDACTED_MARK);
                    out.push_str(eol);
                    continue;
                }
            }
        }
        out.push_str(line);
    }
    out
}

impl sip_core::TraceObserver for TraceEmitter {
    fn on_trace(&self, trace: sip_core::SipTrace<'_>) {
        let (method, status, cseq, call_id, summary) = match trace.message {
            sip_core::SipMessage::Request(r) => (
                r.method.as_str().to_string(),
                None,
                r.headers.cseq().map(str::to_string),
                r.headers.call_id().map(str::to_string),
                format!("{} {}", r.method.as_str(), r.uri),
            ),
            sip_core::SipMessage::Response(r) => (
                "SIP/2.0".to_string(),
                Some(r.status),
                r.headers.cseq().map(str::to_string),
                r.headers.call_id().map(str::to_string),
                format!("SIP/2.0 {} {}", r.status, r.reason),
            ),
        };
        // 完整报文文本(供前端展开查看详情);body 可能是 GB18030,按 lossy 解码展示。
        let raw = {
            let bytes = match trace.message {
                sip_core::SipMessage::Request(r) => r.to_bytes(),
                sip_core::SipMessage::Response(r) => r.to_bytes(),
            };
            // GB18030 兼容 ASCII 头;中文 body 用 GB18030 解码更可读。
            let (text, _, _) = encoding_rs::GB18030.decode(&bytes);
            redact_sip_credentials(&text)
        };
        let entry = serde_json::json!({
            "ts_ms": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64).unwrap_or(0),
            "direction": if matches!(trace.dir, sip_core::TraceDir::In) { "in" } else { "out" },
            "method": method,
            "status": status,
            "cseq": cseq,
            "call_id": call_id,
            "peer": trace.peer.to_string(),
            "summary": summary,
            "raw": raw,
        });
        let _ = self.app.emit("sip_trace", entry);
    }
}

// ── IPC 命令 ─────────────────────────────────────────────

/// 开关 SIP 信令追踪(FR-43)。开启后每条收发报文以 `sip_trace` 事件推给前端。
#[tauri::command]
pub(crate) async fn set_sip_trace(
    enabled: bool,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> IpcResult<String> {
    let guard = state.device.lock().await;
    match &*guard {
        Some(h) => {
            if enabled {
                h.transport
                    .set_tracer(Some(Arc::new(TraceEmitter { app: app.clone() })));
                Ok("信令追踪已开启".into())
            } else {
                h.transport.set_tracer(None);
                Ok("信令追踪已关闭".into())
            }
        }
        None => Err(IpcError::device_not_running("设备未启动")),
    }
}

// ── 数据传输对象 ──────────────────────────────────────────

/// 设置全局日志级别(error/warn/info/debug/trace,或 EnvFilter 语法)。返回生效后的级别。
#[tauri::command]
pub(crate) fn set_log_level(level: String) -> IpcResult<String> {
    if common::logging::set_level(&level) {
        Ok(common::logging::current_level())
    } else {
        Err(IpcError::config(format!("无效的日志级别: {level}")))
    }
}

/// 查询当前日志级别。
#[tauri::command]
pub(crate) fn get_log_level() -> String {
    common::logging::current_level()
}

/// 拉取最近缓冲的日志(前端打开日志页时补历史)。
#[tauri::command]
pub(crate) fn get_recent_logs(max: Option<usize>) -> Vec<common::logging::LogLine> {
    common::logging::recent(max.unwrap_or(500))
}
