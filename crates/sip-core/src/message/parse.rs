//! SIP 报文解析。
//!
//! 按 RFC 3261 报文格式:起始行 CRLF,若干头行 CRLF,空行 CRLF,消息体。
//! 头行支持续行(以空白开头的行接续上一行)。body 长度以 Content-Length 为准,
//! 缺失则取空行之后的全部剩余字节。

use common::{Error, Result};

use super::headers::Headers;
use super::{Method, Request, Response, SipMessage};

/// 解析一条完整 SIP 消息。
pub fn parse_message(bytes: &[u8]) -> Result<SipMessage> {
    // 找到头部与 body 的分界(空行 \r\n\r\n)。
    let sep = find_double_crlf(bytes)
        .ok_or_else(|| Error::Sip("SIP 报文缺少头/体分隔空行".into()))?;
    let head = std::str::from_utf8(&bytes[..sep])
        .map_err(|_| Error::Sip("SIP 头部非 UTF-8".into()))?;
    let body_start = sep + 4;

    let mut lines = head.split("\r\n");
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
        pending = Some((name.trim().to_string(), value.trim().to_string()));
    }
    if let Some((n, v)) = pending.take() {
        headers.append(n, v);
    }

    // body 长度:以 Content-Length 为准,缺失取剩余全部。
    let body = match headers.get("Content-Length").and_then(|s| s.trim().parse::<usize>().ok()) {
        Some(len) => {
            let end = (body_start + len).min(bytes.len());
            bytes[body_start..end].to_vec()
        }
        None => bytes[body_start..].to_vec(),
    };

    // 起始行:响应以 "SIP/2.0 " 开头,否则视为请求。
    if let Some(rest) = start_line.strip_prefix("SIP/2.0 ") {
        let (code, reason) = rest.split_once(' ').unwrap_or((rest, ""));
        let status = code
            .parse::<u16>()
            .map_err(|_| Error::Sip(format!("非法状态码: {code}")))?;
        Ok(SipMessage::Response(Response {
            status,
            reason: reason.to_string(),
            headers,
            body,
        }))
    } else {
        let mut parts = start_line.split_whitespace();
        let method = Method::parse(parts.next().unwrap_or(""))?;
        let uri = parts
            .next()
            .ok_or_else(|| Error::Sip("请求行缺少 URI".into()))?
            .to_string();
        Ok(SipMessage::Request(Request {
            method,
            uri,
            headers,
            body,
        }))
    }
}

/// 找到头/体分隔的 `\r\n\r\n` 起始下标。
fn find_double_crlf(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|w| w == b"\r\n\r\n")
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
}
