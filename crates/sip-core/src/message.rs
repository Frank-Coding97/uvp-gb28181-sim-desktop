//! SIP 消息模型。
//!
//! 实现 GB28181 所需的 SIP 子集(见 docs/40-protocol/sip.md):请求方法、起始行、
//! 头字段、消息体的解析与序列化。刻意保持精简 —— 只覆盖设备侧用到的头,
//! 未知头在解析时原样保留,序列化时回写,保证与平台交互不丢信息。

use std::fmt;

use common::{Error, Result};

mod headers;
mod parse;

pub use headers::Headers;

/// SIP 请求方法(GB28181 子集)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// 注册 / 注销。
    Register,
    /// MANSCDP 载体:心跳 / 查询 / 应答 / 通知。
    Message,
    /// 点播建立会话。
    Invite,
    /// INVITE 最终响应的确认。
    Ack,
    /// 结束会话(停流)。
    Bye,
    /// 探活。
    Options,
}

impl Method {
    /// 方法名(大写),用于起始行与 CSeq。
    pub fn as_str(&self) -> &'static str {
        match self {
            Method::Register => "REGISTER",
            Method::Message => "MESSAGE",
            Method::Invite => "INVITE",
            Method::Ack => "ACK",
            Method::Bye => "BYE",
            Method::Options => "OPTIONS",
        }
    }

    /// 从方法名解析。
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "REGISTER" => Method::Register,
            "MESSAGE" => Method::Message,
            "INVITE" => Method::Invite,
            "ACK" => Method::Ack,
            "BYE" => Method::Bye,
            "OPTIONS" => Method::Options,
            other => return Err(Error::Sip(format!("不支持的 SIP 方法: {other}"))),
        })
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// SIP 消息:请求或响应。
#[derive(Debug, Clone)]
pub enum SipMessage {
    /// 请求(起始行含方法与 Request-URI)。
    Request(Request),
    /// 响应(起始行含状态码与原因短语)。
    Response(Response),
}

/// SIP 请求。
#[derive(Debug, Clone)]
pub struct Request {
    /// 请求方法。
    pub method: Method,
    /// Request-URI(如 `sip:34020000002000000001@host:5060`)。
    pub uri: String,
    /// 头集合。
    pub headers: Headers,
    /// 消息体(MANSCDP XML / SDP,可空)。
    pub body: Vec<u8>,
}

/// SIP 响应。
#[derive(Debug, Clone)]
pub struct Response {
    /// 状态码(如 200 / 401 / 404)。
    pub status: u16,
    /// 原因短语(如 "OK" / "Unauthorized")。
    pub reason: String,
    /// 头集合。
    pub headers: Headers,
    /// 消息体。
    pub body: Vec<u8>,
}

impl SipMessage {
    /// 从字节解析 SIP 消息(请求或响应)。见 [`parse`] 模块。
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        parse::parse_message(bytes)
    }

    /// 序列化为字节(含 CRLF 行分隔与空行 + body)。
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            SipMessage::Request(r) => r.to_bytes(),
            SipMessage::Response(r) => r.to_bytes(),
        }
    }
}

impl Request {
    /// 序列化请求为字节。
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = format!("{} {} SIP/2.0\r\n", self.method, self.uri).into_bytes();
        self.headers.write_into(&mut out, self.body.len());
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}

impl Response {
    /// 序列化响应为字节。
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = format!("SIP/2.0 {} {}\r\n", self.status, self.reason).into_bytes();
        self.headers.write_into(&mut out, self.body.len());
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_往返() {
        for m in [
            Method::Register,
            Method::Message,
            Method::Invite,
            Method::Ack,
            Method::Bye,
            Method::Options,
        ] {
            assert_eq!(Method::parse(m.as_str()).unwrap(), m);
        }
        assert!(Method::parse("PUBLISH").is_err());
    }

    #[test]
    fn 响应序列化含状态行() {
        let mut headers = Headers::new();
        headers.set("Call-ID", "abc@host");
        let resp = Response {
            status: 200,
            reason: "OK".into(),
            headers,
            body: Vec::new(),
        };
        let text = String::from_utf8(resp.to_bytes()).unwrap();
        assert!(text.starts_with("SIP/2.0 200 OK\r\n"));
        assert!(text.contains("Call-ID: abc@host\r\n"));
        assert!(text.contains("Content-Length: 0\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
    }
}
