//! Stable, serializable errors crossing the Tauri IPC/event boundary.

use serde::Serialize;

/// Error payload returned by commands and emitted by runtime events.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct IpcError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

pub type IpcResult<T> = std::result::Result<T, IpcError>;

impl IpcError {
    /// Construct an IPC error from a stable machine code and a safe public message.
    pub fn new(code: impl Into<String>, message: impl AsRef<str>) -> Self {
        Self {
            code: code.into(),
            message: sanitize_public_message(message.as_ref()),
            details: None,
            retryable: None,
            action: None,
        }
    }

    pub fn with_details(mut self, details: impl AsRef<str>) -> Self {
        let details = sanitize_public_message(details.as_ref());
        if !details.is_empty() && details != self.message {
            self.details = Some(details);
        }
        self
    }

    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = Some(retryable);
        self
    }

    pub fn with_action(mut self, action: impl Into<String>) -> Self {
        let action = action.into();
        if !action.trim().is_empty() {
            self.action = Some(action);
        }
        self
    }

    pub fn config(message: impl AsRef<str>) -> Self {
        Self::new("config.invalid", message)
    }

    pub fn device_not_running(message: impl AsRef<str>) -> Self {
        Self::new("device.not_running", message)
            .with_retryable(false)
            .with_action("请先启动设备后再执行此操作")
    }

    pub fn internal(message: impl AsRef<str>) -> Self {
        // Unknown failures intentionally have no guessed action.
        Self::new("internal.error", message)
    }
}

impl From<common::Error> for IpcError {
    fn from(error: common::Error) -> Self {
        let message = error.to_string();
        match error {
            common::Error::Config(_) => Self::config(message),
            common::Error::Sip(_) => Self::new("sip.protocol", message),
            common::Error::Timeout(_) => Self::new("sip.timeout", message)
                .with_retryable(true)
                .with_action("请检查平台地址、网络连通性与防火墙后重试"),
            common::Error::Rejected { status, .. } if matches!(status, 401 | 403 | 407) => {
                Self::new("sip.rejected.auth", message)
                    .with_details(format!("SIP status {status}"))
                    .with_retryable(false)
                    .with_action("请核对 SIP 用户名、密码、平台域 ID 与设备权限")
            }
            common::Error::Rejected { status, .. }
                if matches!(status, 480 | 486 | 500 | 503 | 504) =>
            {
                Self::new("sip.rejected.busy", message)
                    .with_details(format!("SIP status {status}"))
                    .with_retryable(true)
                    .with_action("请降低设备并发或放慢爬坡速率后重试")
            }
            common::Error::Rejected { status, .. } => {
                Self::new("sip.rejected", message).with_details(format!("SIP status {status}"))
            }
            common::Error::Gb28181(_) => Self::new("gb28181.protocol", message),
            common::Error::Media(_) => Self::new("media.error", message),
            common::Error::Io(error) => io_error(error),
        }
    }
}

impl From<String> for IpcError {
    fn from(message: String) -> Self {
        from_legacy_message(&message)
    }
}

impl From<&str> for IpcError {
    fn from(message: &str) -> Self {
        from_legacy_message(message)
    }
}

fn io_error(error: std::io::Error) -> IpcError {
    let message = format!("网络错误: {error}");
    match error.kind() {
        std::io::ErrorKind::AddrInUse => IpcError::new("io.address_in_use", message)
            .with_retryable(true)
            .with_action("请关闭占用端口的重复实例或其它程序后重试"),
        std::io::ErrorKind::PermissionDenied => IpcError::new("io.permission_denied", message)
            .with_retryable(false)
            .with_action("请检查系统权限或选择允许访问的位置后重试"),
        std::io::ErrorKind::NotFound => IpcError::new("io.not_found", message)
            .with_retryable(false)
            .with_action("请确认文件、目录或运行时依赖仍然存在"),
        std::io::ErrorKind::StorageFull => IpcError::new("io.storage_full", message)
            .with_retryable(true)
            .with_action("请清理磁盘空间后重试"),
        _ => IpcError::new("io.error", message),
    }
}

fn from_legacy_message(message: &str) -> IpcError {
    let normalized = message.trim();
    let lower = normalized.to_ascii_lowercase();

    if contains_any(normalized, &["已有设备在运行", "设备实例已在运行"]) {
        return IpcError::new("device.already_running", normalized)
            .with_retryable(false)
            .with_action("请先停止当前设备实例");
    }
    if contains_any(normalized, &["设备未启动", "没有正在运行的设备"]) {
        return IpcError::device_not_running(normalized);
    }
    if contains_any(
        normalized,
        &["已有压测正在运行", "已有压测正在运行或停止中"],
    ) {
        return IpcError::new("stress.already_running", normalized)
            .with_retryable(true)
            .with_action("请等待当前压测完全退出后重试");
    }
    if contains_any(normalized, &["没有正在运行的压测", "无正在运行的压测"]) {
        return IpcError::new("stress.not_running", normalized).with_retryable(false);
    }
    if contains_any(normalized, &["无压测数据可导出", "尚未运行过压测"]) {
        return IpcError::new("report.no_data", normalized).with_retryable(false);
    }
    if contains_any(
        normalized,
        &["尚未配置密码", "未配置 sip 密码", "尚未配置 sip 密码"],
    ) || lower.contains("credential missing")
    {
        return IpcError::new("credential.missing", normalized)
            .with_retryable(false)
            .with_action("请在平台设置中填写并保存 SIP 密码");
    }
    if contains_any(
        normalized,
        &[
            "配置错误",
            "必须在",
            "不能为空",
            "无效",
            "非法",
            "只能包含",
            "只能是",
            "不支持域名",
            "未知的目录",
        ],
    ) {
        return IpcError::config(normalized);
    }
    if contains_any(
        normalized,
        &[
            "媒体",
            "视频源",
            "实时采集",
            "采集设备",
            "ffmpeg",
            "媒体预览",
            "文件选择对话框",
        ],
    ) || lower.contains("ffmpeg")
    {
        return IpcError::new("media.error", normalized);
    }
    if contains_any(normalized, &["诊断包", "环境快照", "日志文件"]) {
        return IpcError::new("diagnostics.export_failed", normalized);
    }
    if contains_any(
        normalized,
        &["报告目录", "写报告", "导出任务", "报告序列化"],
    ) {
        return IpcError::new("report.export_failed", normalized);
    }
    if contains_any(
        normalized,
        &["address already in use", "已在使用", "端口被占用"],
    ) || lower.contains("eaddrinuse")
    {
        return IpcError::new("io.address_in_use", normalized)
            .with_retryable(true)
            .with_action("请关闭占用端口的重复实例或其它程序后重试");
    }
    if contains_any(normalized, &["permission denied", "权限不足"]) {
        return IpcError::new("io.permission_denied", normalized)
            .with_retryable(false)
            .with_action("请检查系统权限后重试");
    }
    if contains_any(normalized, &["no space left", "磁盘空间不足"]) {
        return IpcError::new("io.storage_full", normalized)
            .with_retryable(true)
            .with_action("请清理磁盘空间后重试");
    }

    IpcError::internal(normalized)
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    let lower = haystack.to_ascii_lowercase();
    needles
        .iter()
        .any(|needle| lower.contains(&needle.to_ascii_lowercase()))
}

/// Remove secrets and unbounded process diagnostics before data reaches the webview.
///
/// This function is deliberately conservative: IPC errors are user-facing summaries, not a place
/// for raw SIP authentication material or FFmpeg stderr. Full diagnostics stay in protected logs.
pub fn sanitize_public_message(input: &str) -> String {
    let mut lines = Vec::new();
    for raw_line in input.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        let ffmpeg_diagnostics = lower.contains("ffmpeg")
            && [
                "stderr",
                "ffmpeg log:",
                "未返回可解析",
                "详细输出",
                "error output",
                "退出码",
                "exited with",
            ]
            .iter()
            .any(|marker| lower.contains(marker));
        if ffmpeg_diagnostics {
            lines.push("FFmpeg 处理失败（详细输出已隐藏）".to_string());
            continue;
        }
        if let Some(redacted) = redact_auth_header(line) {
            lines.push(redacted);
            continue;
        }
        lines.push(redact_inline_secret(line));
    }
    lines.join(" ").trim().to_string()
}

fn redact_auth_header(input: &str) -> Option<String> {
    let lower = input.to_ascii_lowercase();
    let headers = [
        "authorization",
        "proxy-authorization",
        "www-authenticate",
        "proxy-authenticate",
        "authentication-info",
    ];
    let (start, header) = headers
        .iter()
        .filter_map(|header| {
            lower
                .find(&format!("{header}:"))
                .map(|start| (start, *header))
        })
        .min_by_key(|(start, _)| *start)?;
    let original_header = &input[start..start + header.len()];
    Some(format!("{}{}: <已脱敏>", &input[..start], original_header))
}

fn redact_inline_secret(input: &str) -> String {
    let mut output = input.to_string();
    for key in ["password", "passwd", "pwd", "密码"] {
        output = redact_key_values(&output, key);
    }
    // Digest challenge/response data is not useful in an IPC error and can support offline attacks.
    if output.to_ascii_lowercase().contains("digest ") {
        return "Digest 鉴权信息已脱敏".to_string();
    }
    output
}

fn redact_key_values(input: &str, key: &str) -> String {
    let key_lower = key.to_ascii_lowercase();
    let mut output = String::with_capacity(input.len());
    let mut copied_until = 0usize;
    let mut search_from = 0usize;

    while search_from < input.len() {
        let lower_tail = input[search_from..].to_ascii_lowercase();
        let Some(relative_start) = lower_tail.find(&key_lower) else {
            break;
        };
        let start = search_from + relative_start;
        let after_key = start + key.len();
        let separator = input[after_key..]
            .find(['=', ':'])
            .filter(|offset| *offset <= 3)
            .map(|offset| after_key + offset);
        let Some(separator) = separator else {
            search_from = after_key;
            continue;
        };

        let mut value_start = separator + 1;
        while let Some(ch) = input[value_start..].chars().next() {
            if !ch.is_whitespace() {
                break;
            }
            value_start += ch.len_utf8();
        }
        let (redact_start, redact_end) = match input[value_start..].chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let content_start = value_start + quote.len_utf8();
                let content_end = input[content_start..]
                    .find(quote)
                    .map(|offset| content_start + offset)
                    .unwrap_or(input.len());
                (content_start, content_end)
            }
            Some(_) => {
                let value_end = input[value_start..]
                    .find(|c: char| c.is_whitespace() || c == ',' || c == ';')
                    .map(|offset| value_start + offset)
                    .unwrap_or(input.len());
                (value_start, value_end)
            }
            None => (value_start, value_start),
        };

        output.push_str(&input[copied_until..redact_start]);
        output.push_str("<已脱敏>");
        copied_until = redact_end;
        search_from = redact_end.max(after_key);
    }

    output.push_str(&input[copied_until..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_omits_absent_optional_fields() {
        let value = serde_json::to_value(IpcError::new("config.invalid", "字段错误")).unwrap();
        assert_eq!(value["code"], "config.invalid");
        assert_eq!(value["message"], "字段错误");
        assert!(value.get("details").is_none());
        assert!(value.get("retryable").is_none());
        assert!(value.get("action").is_none());
    }

    #[test]
    fn common_error_mapping_preserves_categories_and_actions() {
        let timeout = IpcError::from(common::Error::Timeout("REGISTER".into()));
        assert_eq!(timeout.code, "sip.timeout");
        assert_eq!(timeout.retryable, Some(true));
        assert!(timeout.action.as_deref().unwrap().contains("网络"));

        let auth = IpcError::from(common::Error::rejected(401, "Unauthorized"));
        assert_eq!(auth.code, "sip.rejected.auth");
        assert_eq!(auth.retryable, Some(false));
        assert!(auth.action.as_deref().unwrap().contains("密码"));

        let busy = IpcError::from(common::Error::rejected(503, "Service Unavailable"));
        assert_eq!(busy.code, "sip.rejected.busy");
        assert_eq!(busy.retryable, Some(true));
        assert!(busy.action.as_deref().unwrap().contains("并发"));
    }

    #[test]
    fn io_error_mapping_is_stable() {
        let occupied = IpcError::from(common::Error::Io(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "occupied",
        )));
        assert_eq!(occupied.code, "io.address_in_use");

        let full = IpcError::from(common::Error::Io(std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            "full",
        )));
        assert_eq!(full.code, "io.storage_full");
    }

    #[test]
    fn legacy_messages_map_known_boundaries_but_unknowns_have_no_action() {
        assert_eq!(IpcError::from("设备未启动").code, "device.not_running");
        assert_eq!(
            IpcError::from("该平台档案尚未配置密码").code,
            "credential.missing"
        );
        assert_eq!(
            IpcError::from("创建诊断包失败").code,
            "diagnostics.export_failed"
        );
        let unknown = IpcError::from("意料之外的故障");
        assert_eq!(unknown.code, "internal.error");
        assert_eq!(unknown.message, "意料之外的故障");
        assert!(unknown.action.is_none());
    }

    #[test]
    fn sanitizer_removes_credentials_digest_and_ffmpeg_stderr() {
        let input = "Authorization: Digest username=\"3402\", response=\"deadbeef\"\n\
                     password=hunter2 request failed\n\
                     FFmpeg stderr: /Users/alice/private.mov: secret diagnostic";
        let safe = sanitize_public_message(input);
        assert!(safe.contains("Authorization: <已脱敏>"));
        assert!(safe.contains("password=<已脱敏>"));
        assert!(safe.contains("详细输出已隐藏"));
        assert!(!safe.contains("deadbeef"));
        assert!(!safe.contains("hunter2"));
        assert!(!safe.contains("private.mov"));
    }

    #[test]
    fn sanitizer_handles_repeated_and_context_prefixed_secrets() {
        let input = "request failed: Authorization: Basic raw-token\n\
                     password=one password=two passwd='three'\n\
                     Proxy-Authorization: Bearer proxy-token";
        let safe = sanitize_public_message(input);
        assert!(safe.contains("request failed: Authorization: <已脱敏>"));
        assert!(safe.contains("Proxy-Authorization: <已脱敏>"));
        assert_eq!(safe.matches("<已脱敏>").count(), 5, "实际: {safe}");
        for secret in ["raw-token", "one", "two", "three", "proxy-token"] {
            assert!(!safe.contains(secret), "{secret} 泄漏: {safe}");
        }
    }

    #[test]
    fn ffmpeg_unparseable_output_is_hidden_without_stderr_label() {
        let safe = sanitize_public_message(
            "FFmpeg 未返回可解析的 AVFoundation 设备: /Users/alice/private.mov secret",
        );
        assert_eq!(safe, "FFmpeg 处理失败（详细输出已隐藏）");
        assert!(!safe.contains("private.mov"));
    }

    #[test]
    fn ffmpeg_runtime_report_tail_is_hidden() {
        let safe = sanitize_public_message(
            "AVFoundation producer stopped; ffmpeg log: /Users/alice/private.mov secret",
        );
        assert_eq!(safe, "FFmpeg 处理失败（详细输出已隐藏）");
        assert!(!safe.contains("private.mov"));
        assert!(!safe.contains("secret"));
    }

    #[test]
    fn details_are_sanitized_and_not_duplicated() {
        let error = IpcError::new("media.error", "采集失败")
            .with_details("Proxy-Authorization: Digest response=secret");
        assert_eq!(
            error.details.as_deref(),
            Some("Proxy-Authorization: <已脱敏>")
        );
        assert!(!error.details.unwrap().contains("secret"));
    }
}
