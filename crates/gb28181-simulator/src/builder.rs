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

/// 把 MANSCDP XML(UTF-8 String)按信令编码转为报文体字节(§6.10,修复中文乱码)。
///
/// 同时把 `<?xml ... encoding="..."?>` 声明改写为实际编码,保证声明与字节一致
/// (原代码声明固定 GB2312 但字节是 UTF-8,导致平台解码中文乱码)。
pub fn encode_xml_body(xml: &str, encoding: common::SignalingEncoding) -> Vec<u8> {
    // 1) 改写 XML 声明里的 encoding 值以匹配实际编码。
    let fixed = rewrite_xml_encoding(xml, encoding.xml_name());
    // 2) 按目标字符集编码字节。
    match encoding {
        common::SignalingEncoding::Utf8 => fixed.into_bytes(),
        common::SignalingEncoding::Gb18030 => {
            let (bytes, _, _) = encoding_rs::GB18030.encode(&fixed);
            bytes.into_owned()
        }
    }
}

/// 把 XML 声明的 `encoding="XXX"` 改成给定值;无声明则原样返回。
fn rewrite_xml_encoding(xml: &str, enc_name: &str) -> String {
    if let Some(start) = xml.find("encoding=\"") {
        let val_start = start + "encoding=\"".len();
        if let Some(rel_end) = xml[val_start..].find('"') {
            let val_end = val_start + rel_end;
            let mut out = String::with_capacity(xml.len());
            out.push_str(&xml[..val_start]);
            out.push_str(enc_name);
            out.push_str(&xml[val_end..]);
            return out;
        }
    }
    xml.to_string()
}

/// 把入站报文体字节按信令编码解码为 UTF-8 String(供解析)。
/// 先按声明/配置编码尝试;GB18030 解码兼容 GB2312/ASCII。
pub fn decode_xml_body(bytes: &[u8], encoding: common::SignalingEncoding) -> String {
    match encoding {
        common::SignalingEncoding::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        common::SignalingEncoding::Gb18030 => {
            let (text, _, _) = encoding_rs::GB18030.decode(bytes);
            text.into_owned()
        }
    }
}

/// 构造一次注册会话所需的标识(同一会话内 from-tag/call-id 保持一致)。
#[derive(Debug, Clone)]
pub struct DialogIds {
    /// From tag。
    pub from_tag: String,
    /// Call-ID。
    pub call_id: String,
}

impl DialogIds {
    /// 新建一组随机标识。
    pub fn new() -> Self {
        DialogIds {
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

/// Via 头值。`protocol` 必须来自**实际使用的传输对象**
/// ([`sip_core::SipTransport::protocol`]),而不是配置里单独声明的字段 ——
/// 后者曾导致"配置写 TCP、Via 写 TCP、实际从 UDP socket 发出",平台按 Via 回包就走错路径。
fn via(protocol: common::Transport, local_host: &str, local_port: u16) -> String {
    format!(
        "SIP/2.0/{} {}:{};rport;branch={}",
        protocol,
        local_host,
        local_port,
        rand_token("z9hG4bK")
    )
}

/// 构造 REGISTER 请求。`authorization` 为 None 时是首次(无鉴权)请求;
/// 为 Some 时是携带 Authorization 的重新请求。每次调用生成新的 Via branch;
/// `cseq` 递增,`local` 为本端 host:port。
#[allow(clippy::too_many_arguments)]
pub fn register(
    cfg: &DeviceConfig,
    ids: &DialogIds,
    cseq: u32,
    local_host: &str,
    local_port: u16,
    authorization: Option<&str>,
    expires: u32,
    protocol: common::Transport,
) -> Request {
    let aor = device_aor(cfg);
    let mut headers = Headers::new();
    headers.append("Via", via(protocol, local_host, local_port));
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
    // 附录 I:注册及其响应消息头部应带 X-GB-Ver 声明支持的协议版本,
    // 双方据此避免向对方发送不能识别的消息。
    headers.append("X-GB-Ver", cfg.gb_version.x_gb_ver());
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
#[allow(clippy::too_many_arguments)]
pub fn message_xml(
    cfg: &DeviceConfig,
    ids: &DialogIds,
    cseq: u32,
    local_host: &str,
    local_port: u16,
    xml: &str,
    protocol: common::Transport,
) -> Request {
    let aor = device_aor(cfg);
    let mut headers = Headers::new();
    headers.append("Via", via(protocol, local_host, local_port));
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
        body: encode_xml_body(xml, cfg.signaling_encoding),
    }
}

/// 为入站请求构造 200 OK 响应。按 SIP 规则回显 Via/From/To/Call-ID/CSeq
/// (若 To 无 tag 则补一个随机 tag),用于应答平台的 OPTIONS / 简单 MESSAGE。
pub fn response_ok(request: &sip_core::Request) -> sip_core::Response {
    response_status(request, 200, "OK")
}

/// 为入站请求构造任意状态响应，并回显事务标识头。
pub fn response_status(
    request: &sip_core::Request,
    status: u16,
    reason: &str,
) -> sip_core::Response {
    response_status_tagged(request, status, reason, &rand_token(""))
}

/// 同 [`response_ok`],但当 To 无 tag 时用指定 `to_tag` 补(UAS 生成)。
///
/// 订阅场景需要:200 里的 To-tag 必须与后续对话内 NOTIFY 的 From-tag 一致,
/// 才能与平台的订阅对话匹配,故由调用方指定同一个 tag。
pub fn response_ok_tagged(request: &sip_core::Request, to_tag: &str) -> sip_core::Response {
    response_status_tagged(request, 200, "OK", to_tag)
}

/// INVITE 的 200 OK:在通用 2xx 之上补 **Contact** 与 **Subject**。
///
/// 为什么 Contact 不是可选项:
///
/// - RFC 3261 §12.1.1:对 INVITE 的 2xx **必须**带 Contact,它是这条对话的
///   远端目标(逐跳可达地址)。
/// - RFC 3261 §13.2.2.4:UAC 的 ACK **按应答里的 Contact 定目的地**。缺了它,
///   平台只能退回用 INVITE 的 Request-URI(`sip:<通道ID>@<平台域>`)当目标 ——
///   报文会被发往平台自己而不是设备。实测:UVP 平台(sipgo UAC)在缺 Contact 时
///   把 ACK 的目标算成 `<平台域>:5060`,ACK 到不了设备,平台判定 INVITE 失败 →
///   回滚 → 关掉 ZLM 收流端口;设备侧表现为媒体 TCP 连接被对端关闭
///   (Broken pipe),而 SIP 侧连 ACK 都收不到。
/// - GB/T 28181 附录 K:设备应答里回填"媒体流发送者ID:发送方序列号",
///   平台据此把这条码流与点播会话关联。
pub fn response_ok_invite(
    request: &sip_core::Request,
    contact: &str,
    subject: Option<&str>,
) -> sip_core::Response {
    let mut resp = response_ok(request);
    resp.headers.set("Contact", contact);
    if let Some(subject) = subject {
        resp.headers.set("Subject", subject);
    }
    resp
}

fn response_status_tagged(
    request: &sip_core::Request,
    status: u16,
    reason: &str,
    to_tag: &str,
) -> sip_core::Response {
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
        status,
        reason: reason.into(),
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
    protocol: common::Transport,
) -> Request {
    let aor = device_aor(cfg);
    let mut headers = Headers::new();
    headers.append("Via", via(protocol, local_host, local_port));
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
        body: encode_xml_body(xml, cfg.signaling_encoding),
    }
}

/// 设备主动发起的 INVITE 会话所需的对话标识。
///
/// 语音广播里设备是**主叫**(§9.12.1 图27 信令 5),对话由设备侧创建:
/// Call-ID / From-tag 自己生成,平台的 To-tag 从 200 OK 里学到之后回填。
#[derive(Debug, Clone)]
pub struct OutboundDialog {
    /// 本端生成的 Call-ID。
    pub call_id: String,
    /// 本端 From-tag。
    pub from_tag: String,
    /// 对端 AOR(To 头的 URI 部分,如 `sip:34020000002000000001@域`)。
    pub remote_uri: String,
    /// 对端 To-tag,收到 200 OK 后回填;ACK/BYE 必须带上。
    pub remote_tag: Option<String>,
    /// 会话内 CSeq 基准(INVITE 用的那个;ACK 复用同一个,BYE 用新的)。
    pub invite_cseq: u32,
}

impl OutboundDialog {
    /// 新建一个待发起的出站对话。
    pub fn new(remote_uri: String, invite_cseq: u32) -> Self {
        OutboundDialog {
            call_id: rand_token(""),
            from_tag: rand_token(""),
            remote_uri,
            remote_tag: None,
            invite_cseq,
        }
    }

    /// To 头值(带上已学到的对端 tag)。
    fn to_header(&self) -> String {
        match &self.remote_tag {
            Some(tag) => format!("<{}>;tag={tag}", self.remote_uri),
            None => format!("<{}>", self.remote_uri),
        }
    }
}

/// 构造设备主动发起的 INVITE(语音广播的信令 5)。
///
/// `subject` 是附录 L 的 Subject 头域,标准 §9.2.3 c) 要求携带;
/// `sdp` 为 offer 正文(`build_broadcast_offer` 生成)。
#[allow(clippy::too_many_arguments)]
pub fn invite(
    cfg: &DeviceConfig,
    dialog: &OutboundDialog,
    local_host: &str,
    local_port: u16,
    subject: &sip_core::Subject,
    sdp: &str,
    protocol: common::Transport,
) -> Request {
    let aor = device_aor(cfg);
    let mut headers = Headers::new();
    headers.append("Via", via(protocol, local_host, local_port));
    headers.append("From", format!("<{aor}>;tag={}", dialog.from_tag));
    headers.append("To", dialog.to_header());
    headers.append("Call-ID", dialog.call_id.clone());
    headers.append("CSeq", format!("{} INVITE", dialog.invite_cseq));
    headers.append(
        "Contact",
        format!("<sip:{}@{}:{}>", cfg.device_id, local_host, local_port),
    );
    headers.append("Subject", subject.to_string());
    headers.append("Max-Forwards", "70");
    headers.append("User-Agent", "UVP-GB28181-Sim");
    headers.append("Content-Type", "application/sdp");
    Request {
        method: Method::Invite,
        uri: dialog.remote_uri.clone(),
        headers,
        body: sdp.as_bytes().to_vec(),
    }
}

/// 构造 ACK(确认平台的 200 OK,完成 INVITE 会话建立)。
///
/// ACK 与 INVITE **共用同一个 CSeq 序号**(RFC 3261 §13.2.2.4),
/// 但 Via branch 必须是新的(它是一个独立事务)。
pub fn ack(
    cfg: &DeviceConfig,
    dialog: &OutboundDialog,
    local_host: &str,
    local_port: u16,
    protocol: common::Transport,
) -> Request {
    let aor = device_aor(cfg);
    let mut headers = Headers::new();
    headers.append("Via", via(protocol, local_host, local_port));
    headers.append("From", format!("<{aor}>;tag={}", dialog.from_tag));
    headers.append("To", dialog.to_header());
    headers.append("Call-ID", dialog.call_id.clone());
    headers.append("CSeq", format!("{} ACK", dialog.invite_cseq));
    headers.append("Max-Forwards", "70");
    Request {
        method: Method::Ack,
        uri: dialog.remote_uri.clone(),
        headers,
        body: Vec::new(),
    }
}

/// 构造设备主动发起的 BYE(挂断自己发起的会话)。
pub fn bye_outbound(
    cfg: &DeviceConfig,
    dialog: &OutboundDialog,
    cseq: u32,
    local_host: &str,
    local_port: u16,
    protocol: common::Transport,
) -> Request {
    let aor = device_aor(cfg);
    let mut headers = Headers::new();
    headers.append("Via", via(protocol, local_host, local_port));
    headers.append("From", format!("<{aor}>;tag={}", dialog.from_tag));
    headers.append("To", dialog.to_header());
    headers.append("Call-ID", dialog.call_id.clone());
    headers.append("CSeq", format!("{cseq} BYE"));
    headers.append("Max-Forwards", "70");
    Request {
        method: Method::Bye,
        uri: dialog.remote_uri.clone(),
        headers,
        body: Vec::new(),
    }
}

/// 平台发起的会话里,设备侧主动发 BYE 所需的对话标识。
///
/// 附录 K a) 要求监测到媒体流丢失后**通过会话内 BYE 通知**对端。设备是被叫,
/// 所以 From/To 相对原 INVITE 反转:本端是原 To(带本端 tag),平台是原 From。
#[derive(Debug, Clone)]
pub struct InboundDialog {
    /// 会话 Call-ID(沿用 INVITE 的)。
    pub call_id: String,
    /// 本端在 200 OK 里给出的 To-tag,作 BYE 的 From-tag。
    pub local_tag: String,
    /// 平台侧完整 From 头值(含其 tag),作 BYE 的 To。
    pub remote_from: String,
    /// 平台地址(BYE 的目的地)。
    pub peer: std::net::SocketAddr,
    /// 原 INVITE 的 Request-URI,作为 BYE 的 Request-URI 兜底。
    pub request_uri: String,
}

/// 构造被叫侧主动挂断的 BYE(附录 K a) 的媒体流丢失通知)。
pub fn bye_inbound(
    cfg: &DeviceConfig,
    dialog: &InboundDialog,
    cseq: u32,
    local_host: &str,
    local_port: u16,
    protocol: common::Transport,
) -> Request {
    let aor = device_aor(cfg);
    let mut headers = Headers::new();
    headers.append("Via", via(protocol, local_host, local_port));
    headers.append("From", format!("<{aor}>;tag={}", dialog.local_tag));
    headers.append("To", dialog.remote_from.clone());
    headers.append("Call-ID", dialog.call_id.clone());
    headers.append("CSeq", format!("{cseq} BYE"));
    headers.append("Max-Forwards", "70");
    Request {
        method: Method::Bye,
        uri: dialog.request_uri.clone(),
        headers,
        body: Vec::new(),
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
            inbound_identity_mode: common::InboundIdentityMode::Compatible,
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
            register_expires_secs: crate::device::DEFAULT_REGISTER_EXPIRES,
            register_retry_max_secs: crate::device::DEFAULT_REGISTER_RETRY_MAX_SECS,
            gb_version: common::GbVersion::V2022,
            signaling_encoding: common::SignalingEncoding::Gb18030,
        }
    }

    #[test]
    fn register_含必备头() {
        let ids = DialogIds::new();
        let req = register(&cfg(), &ids, 1, "5.6.7.8", 5070, None, 3600, Transport::Udp);
        let text = String::from_utf8(req.to_bytes()).unwrap();
        assert!(text.starts_with("REGISTER sip:34020000002000000001@1.2.3.4:5060 SIP/2.0\r\n"));
        assert!(text.contains("branch=z9hG4bK"));
        assert!(text.contains("Expires: 3600"));
        assert!(text.contains("CSeq: 1 REGISTER"));
        // 附录 I:注册消息头部应带 X-GB-Ver。cfg() 是 2022 版 → 3.0。
        assert!(text.contains("X-GB-Ver: 3.0"), "缺 X-GB-Ver:{text}");
        assert!(!text.contains("Authorization"));
    }

    #[test]
    fn register_的版本标识跟随配置() {
        let ids = DialogIds::new();
        let mut c = cfg();
        c.gb_version = common::GbVersion::V2016;
        let req = register(&c, &ids, 1, "5.6.7.8", 5070, None, 3600, Transport::Udp);
        let text = String::from_utf8(req.to_bytes()).unwrap();
        assert!(text.contains("X-GB-Ver: 2.0"), "2016 应声明 2.0:{text}");
    }

    #[test]
    fn register_带鉴权() {
        let ids = DialogIds::new();
        let first = register(&cfg(), &ids, 1, "5.6.7.8", 5070, None, 3600, Transport::Udp);
        let authenticated = register(
            &cfg(),
            &ids,
            2,
            "5.6.7.8",
            5070,
            Some("Digest xxx"),
            3600,
            Transport::Udp,
        );
        let text = String::from_utf8(authenticated.to_bytes()).unwrap();
        assert!(text.contains("Authorization: Digest xxx"));
        assert!(text.contains("CSeq: 2 REGISTER"));
        assert_eq!(
            first.headers.get("Call-ID"),
            authenticated.headers.get("Call-ID")
        );
        assert_eq!(first.headers.get("From"), authenticated.headers.get("From"));
        assert_ne!(first.headers.get("Via"), authenticated.headers.get("Via"));
    }

    #[test]
    fn message_携带_xml_体() {
        let ids = DialogIds::new();
        let req = message_xml(
            &cfg(),
            &ids,
            3,
            "5.6.7.8",
            5070,
            "<Notify/>",
            Transport::Udp,
        );
        let text = String::from_utf8(req.to_bytes()).unwrap();
        assert!(text.contains("Content-Type: Application/MANSCDP+xml"));
        assert!(text.contains("Content-Length: 9"));
        assert!(text.ends_with("<Notify/>"));
    }

    #[test]
    fn 中文编码_gb18030与utf8往返() {
        let xml =
            "<?xml version=\"1.0\" encoding=\"GB2312\"?>\n<Notify><Name>前门相机</Name></Notify>";
        // GB18030:声明改写为 GB18030,字节为 GB18030 编码,解码回原文。
        let g = encode_xml_body(xml, common::SignalingEncoding::Gb18030);
        assert!(g.windows(7).any(|w| w == b"GB18030"), "声明应含 GB18030");
        let back = decode_xml_body(&g, common::SignalingEncoding::Gb18030);
        assert!(back.contains("前门相机"), "GB18030 往返应还原中文");
        // GB18030 字节与 UTF-8 不同(证明真编码了,非直接塞 UTF-8)。
        assert_ne!(g, xml.replace("GB2312", "GB18030").into_bytes());
        // UTF-8:声明改 UTF-8,字节即 UTF-8。
        let u = encode_xml_body(xml, common::SignalingEncoding::Utf8);
        assert!(u.windows(5).any(|w| w == b"UTF-8"));
        assert!(decode_xml_body(&u, common::SignalingEncoding::Utf8).contains("前门相机"));
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
        let req = notify_in_dialog(
            &cfg(),
            &dialog,
            7,
            "5.6.7.8",
            5070,
            "<Notify/>",
            Transport::Udp,
        );
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
