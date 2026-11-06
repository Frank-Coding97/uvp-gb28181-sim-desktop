//! 日志脱敏与导出文本渲染。
//!
//! 脱敏放在这里而不是桌面端命令里，是为了让它能被 `cargo test -p common` 直接跑到：
//! 导出文件一旦发出去就收不回来，这条规则必须有单测守着。
//!
//! 脱敏范围（只覆盖真正会泄露凭据的字段）：
//! - SIP 头的 `Authorization` / `Proxy-Authorization`（Digest 的 username/realm/response 全在里面）
//! - MANSCDP/XML 的 `<Password>`
//! - 查询串 / 键值日志的 `password=`
//! - JSON 的 `"password"`
//!
//! 刻意不脱敏的东西：DeviceID、Call-ID、CSeq、平台域名。这些是排查联调问题的主键，
//! 抹掉之后日志就失去价值了。

/// 需要整行抹掉值的 SIP 头（比对时统一小写）。
const SECRET_HEADERS: [&str; 2] = ["authorization", "proxy-authorization"];

/// 渲染导出文本：头部注释 + 逐块正文。返回（文本, 脱敏处数）。
///
/// 头部固定写明脱敏事实与不可恢复性，避免接收方误以为拿到的是完整报文。
pub fn render_export(tab_label: &str, exported_at: &str, blocks: &[String]) -> (String, usize) {
    let mut out = String::with_capacity(blocks.iter().map(|b| b.len() + 2).sum::<usize>() + 512);
    out.push_str("# UVP GB28181 桌面端日志导出\n");
    out.push_str(&format!("# 导出时间: {exported_at}\n"));
    out.push_str(&format!("# 范围: {tab_label}\n"));
    out.push_str("# 脱敏: Authorization / Proxy-Authorization / Password 已替换为 ******，不可恢复\n");
    out.push_str("#\n\n");
    let mut redacted = 0;
    for block in blocks {
        let (clean, count) = redact_text(block);
        redacted += count;
        out.push_str(&clean);
        if !clean.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    (out, redacted)
}

/// 逐行脱敏，返回（脱敏后文本, 命中处数）。
pub fn redact_text(text: &str) -> (String, usize) {
    let mut redacted = 0;
    let mut out = String::with_capacity(text.len());
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let (clean, count) = redact_line(line);
        redacted += count;
        out.push_str(&clean);
    }
    (out, redacted)
}

fn redact_line(line: &str) -> (String, usize) {
    // 1) 敏感头整行替换值，保留头名：导出后仍能看出「这里原本有认证」。
    let trimmed = line.trim_start();
    if let Some((name, _)) = trimmed.split_once(':') {
        let key = name.trim().to_ascii_lowercase();
        if SECRET_HEADERS.contains(&key.as_str()) {
            let indent = &line[..line.len() - trimmed.len()];
            return (format!("{indent}{}: ******", name.trim()), 1);
        }
    }
    // 2) 行内口令字段。
    redact_inline(line)
}

/// 处理 `<Password>x</Password>`、`password=x&`、`"password": "x"` 三种形态。
///
/// 只认这三种真正的口令载体。像 `passwordPolicy=len>=12` 这种「键名里含 password」
/// 的配置项必须原样保留 —— 排障时把非敏感内容抹掉，等于制造新的谜题。
fn redact_inline(line: &str) -> (String, usize) {
    let mut out = String::with_capacity(line.len());
    let mut cursor = 0usize;
    let mut hits = 0usize;
    while cursor < line.len() {
        let Some(rel) = find_ci(&line[cursor..], "password") else {
            out.push_str(&line[cursor..]);
            break;
        };
        let key_start = cursor + rel;
        let key_end = key_start + "password".len();
        let after = &line[key_end..];

        // 形态一：`<Password>值</Password>`
        if line[..key_start].ends_with('<') {
            if let Some(tag_close) = after.find('>') {
                let value_start = key_end + tag_close + 1;
                if let Some(close) = find_ci(&line[value_start..], "</password") {
                    out.push_str(&line[cursor..value_start]);
                    out.push_str("******");
                    hits += 1;
                    cursor = value_start + close;
                    continue;
                }
            }
        }

        // 形态二：`password=值`（查询串 / 表单 / 键值日志）
        if let Some(value) = after.strip_prefix('=') {
            let value_start = key_end + 1 + (value.len() - value.trim_start().len());
            out.push_str(&line[cursor..value_start]);
            out.push_str("******");
            hits += 1;
            cursor = value_start + value_end_offset(&line[value_start..]);
            continue;
        }

        // 形态三：`"password": "值"`
        if line[..key_start].ends_with('"') {
            let trimmed = after.trim_start();
            if trimmed.starts_with("\":") {
                let colon_end = key_end + (after.len() - trimmed.len()) + 2;
                let tail = &line[colon_end..];
                if let Some(open) = tail.find('"') {
                    let value_start = colon_end + open + 1;
                    if let Some(close) = line[value_start..].find('"') {
                        out.push_str(&line[cursor..value_start]);
                        out.push_str("******");
                        hits += 1;
                        cursor = value_start + close;
                        continue;
                    }
                }
            }
        }

        // 不是口令字段：原样放行，但从键尾继续扫描。
        out.push_str(&line[cursor..key_end]);
        cursor = key_end;
    }
    (out, hits)
}

/// 口令值的结束位置（相对偏移）。
fn value_end_offset(rest: &str) -> usize {
    for (index, ch) in rest.char_indices() {
        if matches!(ch, '&' | '"' | '\'' | ';' | '<' | ' ' | '\t' | '\r' | '\n') {
            return index;
        }
    }
    rest.len()
}

/// ASCII 大小写不敏感查找（日志是字节流文本，不需要 Unicode 折叠）。
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let hay = haystack.as_bytes();
    let pat = needle.as_bytes();
    if pat.is_empty() || hay.len() < pat.len() {
        return None;
    }
    (0..=hay.len() - pat.len()).find(|&index| hay[index..index + pat.len()].eq_ignore_ascii_case(pat))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_authorization_header_but_keeps_name() {
        let (clean, hits) = redact_text(
            "REGISTER sip:34020000002000000001@10.0.0.1 SIP/2.0\nAuthorization: Digest username=\"3402\", nonce=\"abc\", response=\"deadbeef\"\n",
        );
        assert_eq!(hits, 1);
        assert!(clean.contains("Authorization: ******"));
        assert!(!clean.contains("deadbeef"));
        assert!(!clean.contains("username="));
        // 头名保留，方便定位「这条请求带认证」。
        assert!(clean.contains("REGISTER sip:"));
    }

    #[test]
    fn redacts_proxy_authorization_case_insensitively() {
        let (clean, hits) = redact_text("PROXY-AUTHORIZATION: Digest response=\"secret\"");
        assert_eq!(hits, 1);
        assert_eq!(clean, "PROXY-AUTHORIZATION: ******");
    }

    #[test]
    fn leaves_unrelated_headers_untouched() {
        let raw = "From: <sip:34020000001320000001@3402000000>;tag=1\nCall-ID: abc@10.0.0.1\nCSeq: 1 REGISTER";
        let (clean, hits) = redact_text(raw);
        assert_eq!(hits, 0);
        assert_eq!(clean, raw);
    }

    #[test]
    fn redacts_xml_password_tag() {
        let (clean, hits) = redact_text(
            "<DeviceInfo><DeviceID>3402</DeviceID><Password>Admin12345</Password></DeviceInfo>",
        );
        assert_eq!(hits, 1);
        assert!(clean.contains("<Password>******</Password>"));
        assert!(!clean.contains("Admin12345"));
        // DeviceID 是排障必需信息，不能被顺手抹掉。
        assert!(clean.contains("<DeviceID>3402</DeviceID>"));
    }

    #[test]
    fn redacts_query_and_json_password() {
        let (query, qhits) = redact_text("GET /api?user=admin&password=Admin12345&x=1");
        assert_eq!(qhits, 1);
        assert!(query.contains("password=******&x=1"), "实际输出: {query}");

        let (json, jhits) = redact_text(r#"{"username":"admin","password":"Admin12345"}"#);
        assert_eq!(jhits, 1);
        assert!(!json.contains("Admin12345"), "实际输出: {json}");
        assert!(json.contains("\"username\":\"admin\""), "实际输出: {json}");
    }

    #[test]
    fn ignores_words_containing_password() {
        let raw = "# 密码策略 passwordPolicy=len>=12";
        let (clean, hits) = redact_text(raw);
        assert_eq!(hits, 0, "passwordPolicy 不是口令字段");
        assert_eq!(clean, raw);
    }

    #[test]
    fn render_export_states_redaction_in_header() {
        let blocks = vec![
            "Authorization: Digest response=\"deadbeef\"".to_string(),
            "<Password>p@ss</Password>".to_string(),
        ];
        let (text, hits) = render_export("SIP 报文", "2026-11-06T13:50:00", &blocks);
        assert_eq!(hits, 2);
        assert!(text.starts_with("# UVP GB28181 桌面端日志导出"));
        assert!(text.contains("不可恢复"));
        assert!(!text.contains("deadbeef"));
        assert!(!text.contains("p@ss"));
    }
}
