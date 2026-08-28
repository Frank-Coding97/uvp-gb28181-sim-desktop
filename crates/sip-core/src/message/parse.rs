//! SIP 报文解析。
//!
//! 按 RFC 3261 报文格式:起始行、若干头行、空行、消息体。相对最初版本补齐了几处
//! 现场会踩到的健壮性问题:
//!
//! - **行尾兼容 LF**:规范要求 CRLF,但部分平台/中间设备会改写成纯 LF。只认 CRLF
//!   会把整条报文丢掉,现象是"平台明明发了、设备完全没反应"。
//! - **紧凑头**(RFC 3261 §7.3.3):`i/m/l/c/f/t/v/...` 展开成全名,否则按 Call-ID
//!   路由和按 Content-Length 取 body 全部失效。
//! - **不完整报文可识别**:Content-Length 大于实际剩余字节时返回
//!   [`ParseOutcome::Incomplete`] 而不是静默截断,这是 TCP 流式重组的前提。
//! - **长度上限**:拒绝声明超大 Content-Length 的报文,避免被单条报文拖爆内存。

use common::{Error, Result};

use super::headers::Headers;
use super::{Method, Request, Response, SipMessage};

/// 单条报文 body 的上限(16 MiB)。国标最大的 body 是目录/录像应答,远小于此;
/// 超过即视为畸形或恶意,直接拒绝而不是先分配再失败。
const MAX_BODY_LEN: usize = 16 * 1024 * 1024;

/// 头部区域的上限(1 MiB)。TCP 流式重组时用来防止"永远等不到空行"的无界堆积。
pub const MAX_HEAD_LEN: usize = 1024 * 1024;

/// 增量解析结果。
pub enum ParseOutcome {
    /// 解析出一条完整消息,并给出它在输入中占用的字节数(TCP 用于推进读游标)。
    Complete {
        message: SipMessage,
        consumed: usize,
    },
    /// 目前的字节还不足以构成一条完整消息,需要继续读。
    Incomplete,
}

/// 解析一条**完整**的 SIP 消息。字节不足时报错(UDP 语义:一个数据报一条消息)。
pub fn parse_message(bytes: &[u8]) -> Result<SipMessage> {
    match parse_incremental(bytes)? {
        ParseOutcome::Complete { message, .. } => Ok(message),
        ParseOutcome::Incomplete => Err(Error::Sip(
            "SIP 报文不完整(缺少头/体分隔空行或 body 短于 Content-Length)".into(),
        )),
    }
}

/// 增量解析:适用于 TCP 流。不修改输入,由调用方按 `consumed` 推进缓冲。
pub fn parse_incremental(bytes: &[u8]) -> Result<ParseOutcome> {
    let Some(sep) = find_header_body_separator(bytes) else {
        if bytes.len() > MAX_HEAD_LEN {
            return Err(Error::Sip("SIP 头部超过长度上限,拒绝".into()));
        }
        return Ok(ParseOutcome::Incomplete);
    };
    let head = std::str::from_utf8(&bytes[..sep.head_end])
        .map_err(|_| Error::Sip("SIP 头部非 UTF-8".into()))?;
    let body_start = sep.body_start;

    // 行尾统一按 LF 切分,再各自去掉尾部 CR —— 一次处理 CRLF / LF / 混用。
    let mut lines = head.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    let start_line = lines
        .next()
        .ok_or_else(|| Error::Sip("SIP 报文为空".into()))?;

    // 收集头(处理续行:以空格/制表符开头的行拼到上一行)。
    let mut headers = Headers::new();
    let mut pending: Option<(String, String)> = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some((_, v)) = pending.as_mut() {
                v.push(' ');
                v.push_str(line.trim());
            }
            continue;
        }
        if let Some((n, v)) = pending.take() {
            headers.append(n, v);
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| Error::Sip(format!("非法头行: {line}")))?;
        pending = Some((
            expand_compact(name.trim()).to_string(),
            value.trim().to_string(),
        ));
    }
    if let Some((n, v)) = pending.take() {
        headers.append(n, v);
    }

    // body 长度:以 Content-Length 为准。缺失时按 UDP 语义取剩余全部
    // (TCP 侧调用方应确保对端给出 Content-Length;RFC 3261 §18.3 对流式传输是强制的)。
    let (body, consumed) = match headers.get("Content-Length").map(|s| s.trim().to_string()) {
        Some(raw) => {
            let len: usize = raw
                .parse()
                .map_err(|_| Error::Sip(format!("非法 Content-Length: {raw}")))?;
            if len > MAX_BODY_LEN {
                return Err(Error::Sip(format!(
                    "Content-Length {len} 超过上限 {MAX_BODY_LEN}"
                )));
            }
            let end = body_start.saturating_add(len);
            if end > bytes.len() {
                // 声明长度大于实际剩余:报文不完整,而不是"截断后照用"。
                return Ok(ParseOutcome::Incomplete);
            }
            (bytes[body_start..end].to_vec(), end)
        }
        None => (bytes[body_start..].to_vec(), bytes.len()),
    };

    // 起始行:响应以 "SIP/2.0" 开头,否则视为请求。
    let message = if let Some(rest) = strip_version_prefix(start_line) {
        let rest = rest.trim_start();
        let (code, reason) = rest.split_once(' ').unwrap_or((rest, ""));
        let status = code
            .parse::<u16>()
            .map_err(|_| Error::Sip(format!("非法状态码: {code}")))?;
        if !(100..=699).contains(&status) {
            return Err(Error::Sip(format!("状态码超出 100..=699: {status}")));
        }
        SipMessage::Response(Response {
            status,
            reason: reason.trim().to_string(),
            headers,
            body,
        })
    } else {
        let mut parts = start_line.split_whitespace();
        let method = Method::parse(parts.next().unwrap_or(""))?;
        let uri = parts
            .next()
            .ok_or_else(|| Error::Sip("请求行缺少 URI".into()))?
            .to_string();
        // 版本号存在时校验;缺失(极少数实现)不因此丢弃报文。
        if let Some(version) = parts.next() {
            if !version.eq_ignore_ascii_case("SIP/2.0") {
                return Err(Error::Sip(format!("不支持的 SIP 版本: {version}")));
            }
        }
        SipMessage::Request(Request {
            method,
            uri,
            headers,
            body,
        })
    };

    Ok(ParseOutcome::Complete { message, consumed })
}

/// `SIP/2.0` 前缀(大小写不敏感),命中则返回其后的内容。
fn strip_version_prefix(line: &str) -> Option<&str> {
    const V: &str = "SIP/2.0";
    if line.len() >= V.len() && line[..V.len()].eq_ignore_ascii_case(V) {
        Some(&line[V.len()..])
    } else {
        None
    }
}

/// 紧凑头 → 全名(RFC 3261 §20 + RFC 3265/3515 扩展)。
///
/// 单字符头名一律按紧凑形式展开;多字符头名原样返回。
fn expand_compact(name: &str) -> &str {
    if name.len() != 1 {
        return name;
    }
    match name.as_bytes()[0].to_ascii_lowercase() {
        b'i' => "Call-ID",
        b'm' => "Contact",
        b'e' => "Content-Encoding",
        b'l' => "Content-Length",
        b'c' => "Content-Type",
        b'f' => "From",
        b's' => "Subject",
        b'k' => "Supported",
        b't' => "To",
        b'v' => "Via",
        b'o' => "Event",
        b'u' => "Allow-Events",
        b'r' => "Refer-To",
        b'b' => "Referred-By",
        b'x' => "Session-Expires",
        _ => name,
    }
}

/// 头/体分隔位置。
struct Separator {
    /// 头部区域结束下标(不含空行)。
    head_end: usize,
    /// body 起始下标(空行之后)。
    body_start: usize,
}

/// 找到头/体分隔的空行,兼容 `\r\n\r\n`、`\n\n` 与混用形式。
fn find_header_body_separator(bytes: &[u8]) -> Option<Separator> {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            // 当前行以 \n 结束;检查下一行是否为空行。
            let next = i + 1;
            match bytes.get(next) {
                Some(b'\n') => {
                    return Some(Separator {
                        head_end: trim_cr(bytes, i),
                        body_start: next + 1,
                    })
                }
                Some(b'\r') if bytes.get(next + 1) == Some(&b'\n') => {
                    return Some(Separator {
                        head_end: trim_cr(bytes, i),
                        body_start: next + 2,
                    })
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// 把 head_end 从 `\n` 位置回退掉可能存在的 `\r`。
fn trim_cr(bytes: &[u8], lf_index: usize) -> usize {
    if lf_index > 0 && bytes[lf_index - 1] == b'\r' {
        lf_index - 1
    } else {
        lf_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析注册请求() {
        let raw = "REGISTER sip:34020000002000000001@1.2.3.4:5060 SIP/2.0\r\n\
            Via: SIP/2.0/UDP 5.6.7.8:5060;branch=z9hG4bK123\r\n\
            From: <sip:34020000001320000001@3402.gov>;tag=abc\r\n\
            CSeq: 1 REGISTER\r\n\
            Content-Length: 0\r\n\r\n";
        let msg = SipMessage::parse(raw.as_bytes()).unwrap();
        match msg {
            SipMessage::Request(r) => {
                assert_eq!(r.method, Method::Register);
                assert_eq!(r.uri, "sip:34020000002000000001@1.2.3.4:5060");
                assert_eq!(r.headers.cseq_number(), Some(1));
                assert_eq!(r.headers.via_branch(), Some("z9hG4bK123"));
                assert!(r.body.is_empty());
            }
            _ => panic!("应为请求"),
        }
    }

    #[test]
    fn 解析响应带_body() {
        let raw = "SIP/2.0 200 OK\r\n\
            CSeq: 2 MESSAGE\r\n\
            Content-Length: 5\r\n\r\nHELLO";
        let msg = SipMessage::parse(raw.as_bytes()).unwrap();
        match msg {
            SipMessage::Response(r) => {
                assert_eq!(r.status, 200);
                assert_eq!(r.reason, "OK");
                assert_eq!(r.body, b"HELLO");
            }
            _ => panic!("应为响应"),
        }
    }

    #[test]
    fn 解析序列化往返() {
        let raw = "MESSAGE sip:x@h SIP/2.0\r\n\
            Via: SIP/2.0/UDP h:5060;branch=z9hG4bKx\r\n\
            CSeq: 3 MESSAGE\r\n\
            Content-Length: 2\r\n\r\nhi";
        let msg = SipMessage::parse(raw.as_bytes()).unwrap();
        let bytes = msg.to_bytes();
        let again = SipMessage::parse(&bytes).unwrap();
        assert_eq!(again.to_bytes(), bytes);
    }

    #[test]
    fn 缺少空行报错() {
        assert!(SipMessage::parse(b"REGISTER sip:x SIP/2.0\r\n").is_err());
    }

    #[test]
    fn 纯_lf_行尾也能解析() {
        let raw = "MESSAGE sip:34020000001320000001@h SIP/2.0\n\
            Via: SIP/2.0/UDP h:5060;branch=z9hG4bKlf\n\
            Call-ID: lf-call\n\
            CSeq: 7 MESSAGE\n\
            Content-Length: 3\n\nabc";
        let msg = SipMessage::parse(raw.as_bytes()).unwrap();
        match msg {
            SipMessage::Request(r) => {
                assert_eq!(r.headers.call_id(), Some("lf-call"));
                assert_eq!(r.headers.via_branch(), Some("z9hG4bKlf"));
                assert_eq!(r.body, b"abc");
            }
            _ => panic!("应为请求"),
        }
    }

    #[test]
    fn 紧凑头展开为全名() {
        let raw = "MESSAGE sip:x@h SIP/2.0\r\n\
            v: SIP/2.0/UDP h:5060;branch=z9hG4bKc\r\n\
            i: compact-call\r\n\
            f: <sip:a@h>;tag=t1\r\n\
            t: <sip:b@h>\r\n\
            CSeq: 9 MESSAGE\r\n\
            l: 2\r\n\r\nok";
        let msg = SipMessage::parse(raw.as_bytes()).unwrap();
        match msg {
            SipMessage::Request(r) => {
                assert_eq!(r.headers.call_id(), Some("compact-call"));
                assert_eq!(r.headers.via_branch(), Some("z9hG4bKc"));
                assert_eq!(r.headers.from_tag(), Some("t1"));
                assert_eq!(r.body, b"ok", "紧凑 l: 头必须被当作 Content-Length");
            }
            _ => panic!("应为请求"),
        }
    }

    #[test]
    fn body_短于声明长度视为不完整() {
        let raw = b"MESSAGE sip:x@h SIP/2.0\r\nCSeq: 1 MESSAGE\r\nContent-Length: 10\r\n\r\nshort";
        match parse_incremental(raw).unwrap() {
            ParseOutcome::Incomplete => {}
            ParseOutcome::Complete { .. } => panic!("不应被静默截断成完整报文"),
        }
        // 完整报文解析入口应报错而不是返回残缺 body。
        assert!(SipMessage::parse(raw).is_err());
    }

    #[test]
    fn 增量解析给出准确消耗长度支持流式粘包() {
        let first = "MESSAGE sip:x@h SIP/2.0\r\nCSeq: 1 MESSAGE\r\nContent-Length: 2\r\n\r\nAB";
        let second = "MESSAGE sip:y@h SIP/2.0\r\nCSeq: 2 MESSAGE\r\nContent-Length: 0\r\n\r\n";
        let mut buf = Vec::new();
        buf.extend_from_slice(first.as_bytes());
        buf.extend_from_slice(second.as_bytes());

        let consumed = match parse_incremental(&buf).unwrap() {
            ParseOutcome::Complete { message, consumed } => {
                match message {
                    SipMessage::Request(r) => assert_eq!(r.body, b"AB"),
                    _ => panic!("应为请求"),
                }
                consumed
            }
            ParseOutcome::Incomplete => panic!("首条应完整"),
        };
        assert_eq!(consumed, first.len(), "消耗长度必须刚好是首条报文");

        match parse_incremental(&buf[consumed..]).unwrap() {
            ParseOutcome::Complete { message, consumed } => {
                assert_eq!(consumed, second.len());
                match message {
                    SipMessage::Request(r) => assert_eq!(r.uri, "sip:y@h"),
                    _ => panic!("应为请求"),
                }
            }
            ParseOutcome::Incomplete => panic!("第二条应完整"),
        }
    }

    #[test]
    fn 超大_content_length_被拒() {
        let raw = format!(
            "MESSAGE sip:x@h SIP/2.0\r\nCSeq: 1 MESSAGE\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_LEN + 1
        );
        assert!(SipMessage::parse(raw.as_bytes()).is_err());
    }

    #[test]
    fn 非法状态码与版本被拒() {
        assert!(SipMessage::parse(b"SIP/2.0 999 Weird\r\nCSeq: 1 X\r\n\r\n").is_err());
        assert!(SipMessage::parse(b"MESSAGE sip:x@h SIP/3.0\r\nCSeq: 1 MESSAGE\r\n\r\n").is_err());
    }

    #[test]
    fn 折行续行拼接() {
        let raw = "MESSAGE sip:x@h SIP/2.0\r\n\
            Subject: line one\r\n\
            \tline two\r\n\
            CSeq: 1 MESSAGE\r\n\
            Content-Length: 0\r\n\r\n";
        let msg = SipMessage::parse(raw.as_bytes()).unwrap();
        match msg {
            SipMessage::Request(r) => {
                assert_eq!(r.headers.get("Subject"), Some("line one line two"))
            }
            _ => panic!("应为请求"),
        }
    }
}
