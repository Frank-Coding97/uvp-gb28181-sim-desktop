//! 日志导出的 I/O 壳。
//!
//! 脱敏与渲染在 `common::log_redact`（那边的单测跟着 `cargo test -p common` 跑），
//! 这里只负责把前端递来的文本块写成文件。前端没有机会绕过脱敏：它拿到的是
//! 渲染后的文本，原始报文从不下发回前端二次拼装。

use serde::Serialize;

/// 导出结果（前端用于提示路径与脱敏处数）。
#[derive(Debug, Clone, Serialize)]
pub struct LogExportResult {
    pub path: String,
    pub byte_len: u64,
    /// 命中并替换的敏感字段数；0 表示这份日志里本来没有凭据。
    pub redacted: usize,
    pub line_count: usize,
}

/// 渲染并写入目标文件。
pub fn write_export(
    tab_label: &str,
    exported_at: &str,
    blocks: &[String],
    target: &std::path::Path,
) -> Result<LogExportResult, String> {
    let (text, redacted) = common::log_redact::render_export(tab_label, exported_at, blocks);
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建目录 {} 失败: {error}", parent.display()))?;
        }
    }
    std::fs::write(target, text.as_bytes())
        .map_err(|error| format!("写入 {} 失败: {error}", target.display()))?;
    Ok(LogExportResult {
        path: target.to_string_lossy().into_owned(),
        byte_len: text.len() as u64,
        redacted,
        line_count: blocks.len(),
    })
}
