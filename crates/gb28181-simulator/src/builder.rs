//! GB28181 SIP 请求构造。
//!
//! 按 docs/40-protocol/sip.md 构造设备侧发出的 REGISTER 与 MESSAGE(Keepalive)请求。
//! 头字段(Via/From/To/Contact/CSeq/Call-ID/Max-Forwards/Expires)按 GB28181 惯例填充。

use sip_core::{Headers, Method, Request};

use crate::device::DeviceConfig;

/// 生成随机 token(用于 branch/tag/Call-ID),避免碰撞。
pub fn rand_token(prefix: &str) -> String {
    let n: u64 = rand::random();
    format!("{prefix}{n:016x}")
}

/// 构造一次注册所需的会话标识(同一注册事务内 branch/from-tag/call-id 保持一致)。
#[derive(Debug, Clone)]
pub struct DialogIds {
    /// Via branch(每个事务唯一,须以 z9hG4bK 开头)。
    pub branch: String,
    /// From tag。
    pub from_tag: String,
    /// Call-ID。
    pub call_id: String,
}

impl DialogIds {
    /// 新建一组随机标识。
    pub fn new() -> Self {
        DialogIds {
            branch: rand_token("z9hG4bK"),
            from_tag: rand_token(""),
            call_id: rand_token(""),
        }
    }
}

impl Default for DialogIds {
    fn default() -> Self {
        Self::new()
    }
}

/// 设备侧 SIP URI:`sip:<device_id>@<domain>`。
fn device_aor(cfg: &DeviceConfig) -> String {
    format!("sip:{}@{}", cfg.device_id, cfg.server_domain)
}

/// 平台侧 SIP URI:`sip:<server_id>@<domain>`(server_id 用平台域的中心编码,
/// 这里以 domain 作为 server_id 前缀不可得时退化为 domain 本身)。
fn platform_uri(cfg: &DeviceConfig) -> String {
    // Request-URI 指向平台:sip:<domain>@<host:port>
    format!(
        "sip:{}@{}:{}",
        cfg.server_domain, cfg.server_host, cfg.server_port
    )
}

/// 构造 REGISTER 请求。`authorization` 为 None 时是首次(无鉴权)请求;
/// 为 Some 时是携带 Authorization 的重发。`cseq` 递增,`local` 为本端 host:port。
pub fn register(
    cfg: &DeviceConfig,
    ids: &DialogIds,
    cseq: u32,
    local_host: &str,
    local_port: u16,
    authorization: Option<&str>,
    expires: u32,
) -> Request {
    let aor = device_aor(cfg);
    let mut headers = Headers::new();
    headers.append(
        "Via",
        format!(
            "SIP/2.0/{} {}:{};rport;branch={}",
            cfg.transport, local_host, local_port, ids.branch
        ),
    );
    headers.append("From", format!("<{aor}>;tag={}", ids.from_tag));
    headers.append("To", format!("<{aor}>"));
    headers.append("Call-ID", ids.call_id.clone());
    headers.append("CSeq", format!("{cseq} REGISTER"));
    headers.append(
        "Contact",
        format!("<sip:{}@{}:{}>", cfg.device_id, local_host, local_port),
    );
    headers.append("Max-Forwards", "70");
    headers.append("Expires", expires.to_string());
    headers.append("User-Agent", "UVP-GB28181-Sim");
    if let Some(auth) = authorization {
        headers.append("Authorization", auth.to_string());
    }
    Request {
        method: Method::Register,
        uri: platform_uri(cfg),
        headers,
        body: Vec::new(),
    }
}

/// 构造承载 MANSCDP XML 的 MESSAGE 请求(如 Keepalive)。
pub fn message_xml(
    cfg: &DeviceConfig,
    ids: &DialogIds,
    cseq: u32,
    local_host: &str,
    local_port: u16,
    xml: &str,
) -> Request {
    let aor = device_aor(cfg);
    let mut headers = Headers::new();
    headers.append(
        "Via",
        format!(
            "SIP/2.0/{} {}:{};rport;branch={}",
            cfg.transport,
            local_host,
            local_port,
            rand_token("z9hG4bK")
        ),
    );
    headers.append("From", format!("<{aor}>;tag={}", ids.from_tag));
    headers.append(
        "To",
        format!("<sip:{}@{}>", cfg.server_domain, cfg.server_domain),
    );
    headers.append("Call-ID", ids.call_id.clone());
    headers.append("CSeq", format!("{cseq} MESSAGE"));
    headers.append("Max-Forwards", "70");
    headers.append("Content-Type", "Application/MANSCDP+xml");
    Request {
        method: Method::Message,
        uri: platform_uri(cfg),
        headers,
        body: xml.as_bytes().to_vec(),
    }
}

/// 为入站请求构造 200 OK 响应。按 SIP 规则回显 Via/From/To/Call-ID/CSeq
/// (若 To 无 tag 则补一个随机 tag),用于应答平台的 OPTIONS / 简单 MESSAGE。
pub fn response_ok(request: &sip_core::Request) -> sip_core::Response {
    response_ok_tagged(request, &rand_token(""))
}

/// 同 [`response_ok`],但当 To 无 tag 时用指定 `to_tag` 补(UAS 生成)。
///
/// 订阅场景需要:200 里的 To-tag 必须与后续对话内 NOTIFY 的 From-tag 一致,
/// 才能与平台的订阅对话匹配,故由调用方指定同一个 tag。
pub fn response_ok_tagged(request: &sip_core::Request, to_tag: &str) -> sip_core::Response {
    let mut headers = Headers::new();
    for via in request.headers.get_all("Via") {
        headers.append("Via", via.to_string());
    }
    if let Some(from) = request.headers.get("From") {
        headers.append("From", from.to_string());
    }
    if let Some(to) = request.headers.get("To") {
        if to.contains("tag=") {
            headers.append("To", to.to_string());
        } else {
            headers.append("To", format!("{to};tag={to_tag}"));
        }
    }
    if let Some(cid) = request.headers.call_id() {
        headers.append("Call-ID", cid.to_string());
    }
    if let Some(cseq) = request.headers.cseq() {
        headers.append("CSeq", cseq.to_string());
    }
    sip_core::Response {
        status: 200,
        reason: "OK".into(),
        headers,
        body: Vec::new(),
    }
}

/// 对话内 NOTIFY 所需的对话标识(从平台的 SUBSCRIBE 请求 + 本端 200 应答中提取)。
#[derive(Debug, Clone)]
pub struct NotifyDialog {
    /// 订阅对话 Call-ID(沿用 SUBSCRIBE 的)。
    pub call_id: String,
    /// 本端(设备)在 200 应答里生成的 To-tag,作为 NOTIFY 的 From-tag。
    pub local_tag: String,
    /// 平台侧完整 From 头值(如 `<sip:平台@域>;tag=xxx`),作为 NOTIFY 的 To。
    pub remote_from: String,
    /// 订阅事件包(Event 头,如 `Catalog` / `presence`);缺省回 `Catalog`。
    pub event: String,
    /// 订阅有效期(秒),写入 Subscription-State。
    pub expires: u32,
}

/// 构造对话内 NOTIFY 请求(设备 → 平台,承载 MANSCDP XML)。
///
/// 目录/报警订阅的变更通知须在 SUBSCRIBE 建立的对话内发送:沿用订阅 Call-ID,
/// From/To 相对 SUBSCRIBE 反转(本端为 From 带 local_tag,平台为 To),
/// 并带 `Event` 与 `Subscription-State: active;expires=N` 头。
#[allow(clippy::too_many_arguments)]
pub fn notify_in_dialog(
    cfg: &DeviceConfig,
    dialog: &NotifyDialog,
    cseq: u32,
    local_host: &str,
    local_port: u16,
    xml: &str,
) -> Request {
    let aor = device_aor(cfg);
    let mut headers = Headers::new();
    headers.append(
        "Via",
        format!(
            "SIP/2.0/{} {}:{};rport;branch={}",
            cfg.transport,
            local_host,
            local_port,
            rand_token("z9hG4bK")
        ),
    );
    // From = 本端设备(带本端 tag);To = 平台(沿用 SUBSCRIBE 的 From,含其 tag)。
    headers.append("From", format!("<{aor}>;tag={}", dialog.local_tag));
    headers.append("To", dialog.remote_from.clone());
    headers.append("Call-ID", dialog.call_id.clone());
    headers.append("CSeq", format!("{cseq} NOTIFY"));
    headers.append(
        "Contact",
        format!("<sip:{}@{}:{}>", cfg.device_id, local_host, local_port),
    );
    headers.append("Event", dialog.event.clone());
    headers.append(
        "Subscription-State",
        format!("active;expires={}", dialog.expires),
    );
    headers.append("Max-Forwards", "70");
    headers.append("Content-Type", "Application/MANSCDP+xml");
    Request {
        method: Method::Notify,
        uri: platform_uri(cfg),
        headers,
        body: xml.as_bytes().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{DeviceId, Transport};

    fn cfg() -> DeviceConfig {
        use crate::device::DeviceInfo;
        DeviceConfig {
            device_id: DeviceId::new("34020000001320000001").unwrap(),
            username: "34020000001320000001".into(),
            password: "12345678".into(),
            server_host: "1.2.3.4".into(),
            server_port: 5060,
            server_domain: "34020000002000000001".into(),
            transport: Transport::Udp,
            heartbeat_interval_secs: 60,
            channels: vec![],
            device_info: DeviceInfo {
                device_name: "Test".into(),
                manufacturer: "UVP".into(),
                model: "Sim".into(),
                firmware: "0.1".into(),
            },
            video_source: None,
            video_fps: 25,
            light_bitrate_kbps: None,
            gb_version: common::GbVersion::V2022,
        }
    }

    #[test]
    fn register_含必备头() {
        let ids = DialogIds::new();
        let req = register(&cfg(), &ids, 1, "5.6.7.8", 5070, None, 3600);
        let text = String::from_utf8(req.to_bytes()).unwrap();
        assert!(text.starts_with("REGISTER sip:34020000002000000001@1.2.3.4:5060 SIP/2.0\r\n"));
        assert!(text.contains("branch=z9hG4bK"));
        assert!(text.contains("Expires: 3600"));
        assert!(text.contains("CSeq: 1 REGISTER"));
        assert!(!text.contains("Authorization"));
    }

    #[test]
    fn register_带鉴权() {
        let ids = DialogIds::new();
        let req = register(&cfg(), &ids, 2, "5.6.7.8", 5070, Some("Digest xxx"), 3600);
        let text = String::from_utf8(req.to_bytes()).unwrap();
        assert!(text.contains("Authorization: Digest xxx"));
        assert!(text.contains("CSeq: 2 REGISTER"));
    }

    #[test]
    fn message_携带_xml_体() {
        let ids = DialogIds::new();
        let req = message_xml(&cfg(), &ids, 3, "5.6.7.8", 5070, "<Notify/>");
        let text = String::from_utf8(req.to_bytes()).unwrap();
        assert!(text.contains("Content-Type: Application/MANSCDP+xml"));
        assert!(text.contains("Content-Length: 9"));
        assert!(text.ends_with("<Notify/>"));
    }

    #[test]
    fn 对话内_notify_含事件与订阅状态头() {
        let dialog = NotifyDialog {
            call_id: "sub-call-xyz@plat".into(),
            local_tag: "devtag123".into(),
            remote_from: "<sip:34020000002000000001@34020000002000000001>;tag=plat99".into(),
            event: "Catalog".into(),
            expires: 3600,
        };
        let req = notify_in_dialog(&cfg(), &dialog, 7, "5.6.7.8", 5070, "<Notify/>");
        let text = String::from_utf8(req.to_bytes()).unwrap();
        assert!(text.starts_with("NOTIFY sip:34020000002000000001@1.2.3.4:5060 SIP/2.0\r\n"));
        // 沿用订阅 Call-ID;From 带本端 tag;To 为平台 From(带其 tag)。
        assert!(text.contains("Call-ID: sub-call-xyz@plat"));
        assert!(text.contains(";tag=devtag123"));
        assert!(text.contains("tag=plat99"));
        assert!(text.contains("CSeq: 7 NOTIFY"));
        assert!(text.contains("Event: Catalog"));
        assert!(text.contains("Subscription-State: active;expires=3600"));
    }
}
