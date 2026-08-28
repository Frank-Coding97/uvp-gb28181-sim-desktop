//! SDP(Session Description Protocol)解析与构造。
//!
//! GB28181 使用的 SDP 子集:o/s/c/t/m=video/a=rtpmap/y=<ssrc>。
//! 实现足够解析平台 INVITE 的 SDP(提取 RTP 目标地址与 SSRC)、
//! 构造设备侧 200 OK 的 SDP(声明本端 RTP 端口与 SSRC)。

use std::fmt;
use std::net::IpAddr;

use common::{Error, Result};

/// 媒体方向(a=sendonly / recvonly / sendrecv / inactive)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    SendOnly,
    RecvOnly,
    SendRecv,
    Inactive,
}

impl Direction {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "sendonly" => Direction::SendOnly,
            "recvonly" => Direction::RecvOnly,
            "sendrecv" => Direction::SendRecv,
            "inactive" => Direction::Inactive,
            _ => return None,
        })
    }

    fn as_str(self) -> &'static str {
        match self {
            Direction::SendOnly => "sendonly",
            Direction::RecvOnly => "recvonly",
            Direction::SendRecv => "sendrecv",
            Direction::Inactive => "inactive",
        }
    }
}

/// TCP 连接角色(a=setup,RFC 4145)。
///
/// GB28181 的 RTP over TCP 靠这个属性区分谁发起连接:
/// - `active`:本端主动连对端(TCP-ACTIVE)
/// - `passive`:本端监听等对端连(TCP-PASSIVE)
/// - `actpass`:由对端决定
///
/// 之前只靠 `m=` proto 里是否含 "TCP" 判断,无法区分主被动 —— 平台要求设备 passive
/// 时会直接收不到流。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setup {
    Active,
    Passive,
    ActPass,
    HoldConn,
}

impl Setup {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "active" => Setup::Active,
            "passive" => Setup::Passive,
            "actpass" => Setup::ActPass,
            "holdconn" => Setup::HoldConn,
            _ => return None,
        })
    }

    fn as_str(self) -> &'static str {
        match self {
            Setup::Active => "active",
            Setup::Passive => "passive",
            Setup::ActPass => "actpass",
            Setup::HoldConn => "holdconn",
        }
    }

    /// 对端声明 `setup` 时,本端应取的角色(RFC 4145 §4.1)。
    pub fn answer(self) -> Setup {
        match self {
            Setup::Active => Setup::Passive,
            Setup::Passive | Setup::ActPass => Setup::Active,
            Setup::HoldConn => Setup::HoldConn,
        }
    }
}

/// 一个 SDP 会话描述(面向单路视频流)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDescription {
    /// 会话源:username, session_id, session_version, address。
    pub origin: Origin,
    /// 会话名(s=)。
    pub session_name: String,
    /// 连接地址(c=)。
    pub connection: Connection,
    /// 时间范围(t=,GB28181 常用 0 0 表示永久)。
    pub timing: Timing,
    /// 视频媒体描述(m=video ...)。
    pub media: MediaDescription,
    /// 音频媒体描述(m=audio ...)。音视频复合流时平台会给两条 m= 行,
    /// 早期实现是覆盖式赋值,音频那条被静默丢弃。
    pub audio: Option<MediaDescription>,
    /// 媒体方向(a=sendonly 等)。
    pub direction: Option<Direction>,
    /// TCP 连接角色(a=setup)。
    pub setup: Option<Setup>,
    /// 下载倍速(GB28181 录像下载 `a=downloadspeed:N`;仅下载模式携带)。
    pub download_speed: Option<u32>,
}

impl SessionDescription {
    /// 是否为录像下载会话(会话名 `s=Download`)。
    pub fn is_download(&self) -> bool {
        self.session_name.eq_ignore_ascii_case("Download")
    }

    /// 是否为历史回放会话(会话名 `s=Playback`)。
    pub fn is_playback(&self) -> bool {
        self.session_name.eq_ignore_ascii_case("Playback")
    }

    /// 是否要求 RTP over TCP(看 m= proto)。
    pub fn is_tcp(&self) -> bool {
        self.media.proto.to_ascii_uppercase().contains("TCP")
    }

    /// 本端在 TCP 模式下该扮演的角色。
    ///
    /// 平台给了 `a=setup` 就按 RFC 4145 取反;没给则按国标惯例默认设备主动连平台
    /// (TCP-ACTIVE)。
    pub fn local_tcp_setup(&self) -> Setup {
        self.setup.map(Setup::answer).unwrap_or(Setup::Active)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub username: String,
    pub sess_id: String,
    pub sess_version: String,
    pub net_type: String,
    pub addr_type: String,
    pub addr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub net_type: String,
    pub addr_type: String,
    pub addr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timing {
    pub start: u64,
    pub stop: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaDescription {
    /// 媒体类型(video/audio)。
    pub media: String,
    /// 端口。
    pub port: u16,
    /// 传输协议(RTP/AVP)。
    pub proto: String,
    /// 格式列表(如 "96")。
    pub fmt: Vec<String>,
    /// 首个 rtpmap(如 "96 PS/90000")。保留单值字段以兼容既有调用方。
    pub rtpmap: Option<String>,
    /// 该媒体段下的全部 rtpmap(音视频复合流会有多条)。
    pub rtpmaps: Vec<String>,
    /// SSRC(GB28181 y= 行)。
    pub ssrc: Option<u32>,
}

impl MediaDescription {
    fn new(media: String, port: u16, proto: String, fmt: Vec<String>) -> Self {
        MediaDescription {
            media,
            port,
            proto,
            fmt,
            rtpmap: None,
            rtpmaps: Vec::new(),
            ssrc: None,
        }
    }

    fn push_rtpmap(&mut self, value: String) {
        if self.rtpmap.is_none() {
            self.rtpmap = Some(value.clone());
        }
        self.rtpmaps.push(value);
    }
}

impl SessionDescription {
    /// 解析 SDP 文本(逐行提取 o/s/c/t/m/a/y)。
    ///
    /// `a=rtpmap` 归属于**它上面最近的那条 m= 行**;方向与 setup 取首次出现的值。
    pub fn parse(text: &str) -> Result<Self> {
        let mut origin = None;
        let mut session_name = None;
        let mut connection = None;
        let mut timing = None;
        let mut medias: Vec<MediaDescription> = Vec::new();
        let mut ssrc = None;
        let mut download_speed = None;
        let mut direction = None;
        let mut setup = None;

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("o=") {
                origin = Some(parse_origin(rest)?);
            } else if let Some(rest) = line.strip_prefix("s=") {
                session_name = Some(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("c=") {
                connection = Some(parse_connection(rest)?);
            } else if let Some(rest) = line.strip_prefix("t=") {
                timing = Some(parse_timing(rest)?);
            } else if let Some(rest) = line.strip_prefix("m=") {
                medias.push(parse_media(rest)?);
            } else if let Some(rest) = line.strip_prefix("a=") {
                if let Some(rm) = rest.strip_prefix("rtpmap:") {
                    if let Some(current) = medias.last_mut() {
                        current.push_rtpmap(rm.to_string());
                    }
                } else if let Some(spd) = rest.strip_prefix("downloadspeed:") {
                    // 下载倍速可能写成 "4" 或 "4:1",取首段。
                    download_speed = spd.split(':').next().and_then(|s| s.trim().parse().ok());
                } else if let Some(s) = rest.strip_prefix("setup:") {
                    if setup.is_none() {
                        setup = Setup::parse(s.trim());
                    }
                } else if let Some(d) = Direction::parse(rest.trim()) {
                    if direction.is_none() {
                        direction = Some(d);
                    }
                }
            } else if let Some(rest) = line.strip_prefix("y=") {
                ssrc = rest.trim().parse::<u32>().ok();
            }
        }

        if medias.is_empty() {
            return Err(Error::Sip("SDP 缺少 m= 行".into()));
        }
        // 取首条视频段作为主媒体;无视频段(纯音频广播)时退回首条。
        let video_idx = medias
            .iter()
            .position(|m| m.media.eq_ignore_ascii_case("video"))
            .unwrap_or(0);
        let mut media = medias.remove(video_idx);
        media.ssrc = ssrc;
        let audio = medias
            .into_iter()
            .find(|m| m.media.eq_ignore_ascii_case("audio"));

        Ok(SessionDescription {
            origin: origin.ok_or_else(|| Error::Sip("SDP 缺少 o= 行".into()))?,
            session_name: session_name.unwrap_or_else(|| "Play".to_string()),
            connection: connection.ok_or_else(|| Error::Sip("SDP 缺少 c= 行".into()))?,
            timing: timing.unwrap_or(Timing { start: 0, stop: 0 }),
            media,
            audio,
            direction,
            setup,
            download_speed,
        })
    }

    /// 构造一个最简 GB28181 SDP(设备侧 200 OK):本端媒体地址、端口、SSRC。
    ///
    /// - `username`:o= 行用户名,填设备/通道 ID(平台 ACK 会据此定位)。
    /// - `rtp_port`:**本端真实 RTP 端口**。SDP 里端口 0 表示该媒体被禁用,
    ///   所以必须回填实际端口而不是常量 0。
    /// - `tcp`:true 时 m= proto 用 `TCP/RTP/AVP`(与平台一致),否则 `RTP/AVP`。
    /// - 设备是发送方,恒输出 `a=sendonly`。
    pub fn new_device_response(
        username: &str,
        local_ip: IpAddr,
        rtp_port: u16,
        ssrc: u32,
        tcp: bool,
    ) -> Self {
        Self::new_device_response_with_setup(username, local_ip, rtp_port, ssrc, tcp, None)
    }

    /// 同 [`Self::new_device_response`],但显式指定 TCP 角色(`a=setup`)。
    pub fn new_device_response_with_setup(
        username: &str,
        local_ip: IpAddr,
        rtp_port: u16,
        ssrc: u32,
        tcp: bool,
        setup: Option<Setup>,
    ) -> Self {
        let ip_str = local_ip.to_string();
        let addr_type = if local_ip.is_ipv4() { "IP4" } else { "IP6" };
        let mut media = MediaDescription::new(
            "video".into(),
            rtp_port,
            if tcp {
                "TCP/RTP/AVP".into()
            } else {
                "RTP/AVP".into()
            },
            vec!["96".into()],
        );
        media.push_rtpmap("96 PS/90000".into());
        media.ssrc = Some(ssrc);
        SessionDescription {
            origin: Origin {
                username: username.to_string(),
                sess_id: "0".into(),
                sess_version: "0".into(),
                net_type: "IN".into(),
                addr_type: addr_type.into(),
                addr: ip_str.clone(),
            },
            session_name: "Play".into(),
            connection: Connection {
                net_type: "IN".into(),
                addr_type: addr_type.into(),
                addr: ip_str,
            },
            timing: Timing { start: 0, stop: 0 },
            media,
            audio: None,
            // 设备侧点播/回放一律单向发送。
            direction: Some(Direction::SendOnly),
            setup: if tcp {
                Some(setup.unwrap_or(Setup::Active))
            } else {
                None
            },
            download_speed: None,
        }
    }
}

/// 序列化为 SDP 文本(通过 Display,`.to_string()` 自动可用)。
impl fmt::Display for SessionDescription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v=0\r\n")?;
        write!(
            f,
            "o={} {} {} {} {} {}\r\n",
            self.origin.username,
            self.origin.sess_id,
            self.origin.sess_version,
            self.origin.net_type,
            self.origin.addr_type,
            self.origin.addr
        )?;
        write!(f, "s={}\r\n", self.session_name)?;
        write!(
            f,
            "c={} {} {}\r\n",
            self.connection.net_type, self.connection.addr_type, self.connection.addr
        )?;
        write!(f, "t={} {}\r\n", self.timing.start, self.timing.stop)?;
        write_media(f, &self.media)?;
        if let Some(dir) = self.direction {
            write!(f, "a={}\r\n", dir.as_str())?;
        }
        if let Some(setup) = self.setup {
            write!(f, "a=setup:{}\r\n", setup.as_str())?;
            // RFC 4145:主动方应声明新建连接。
            if setup == Setup::Active {
                write!(f, "a=connection:new\r\n")?;
            }
        }
        if let Some(speed) = self.download_speed {
            write!(f, "a=downloadspeed:{speed}\r\n")?;
        }
        if let Some(ref audio) = self.audio {
            write_media(f, audio)?;
        }
        if let Some(ssrc) = self.media.ssrc {
            write!(f, "y={}\r\n", ssrc)?;
        }
        Ok(())
    }
}

fn write_media(f: &mut fmt::Formatter<'_>, m: &MediaDescription) -> fmt::Result {
    write!(
        f,
        "m={} {} {} {}\r\n",
        m.media,
        m.port,
        m.proto,
        m.fmt.join(" ")
    )?;
    for rm in &m.rtpmaps {
        write!(f, "a=rtpmap:{rm}\r\n")?;
    }
    Ok(())
}

/// 构造语音广播的 SDP offer(设备作接收方,收平台下行 G.711A)。
///
/// 设备为 UAC 主叫,`a=recvonly`,`m=audio {port} RTP/AVP 8 0`,
/// 首选 PCMA(G.711A,pt=8),兼容 PCMU(pt=0);`y={ssrc}`。TCP 时用 `TCP/RTP/AVP`。
pub fn build_broadcast_offer(
    username: &str,
    local_ip: IpAddr,
    port: u16,
    ssrc: u32,
    tcp: bool,
) -> String {
    let ip = local_ip.to_string();
    let at = if local_ip.is_ipv4() { "IP4" } else { "IP6" };
    let proto = if tcp { "TCP/RTP/AVP" } else { "RTP/AVP" };
    let mut sdp = String::new();
    sdp.push_str("v=0\r\n");
    sdp.push_str(&format!("o={username} 0 0 IN {at} {ip}\r\n"));
    sdp.push_str("s=Broadcast\r\n");
    sdp.push_str(&format!("c=IN {at} {ip}\r\n"));
    sdp.push_str("t=0 0\r\n");
    sdp.push_str(&format!("m=audio {port} {proto} 8 0\r\n"));
    sdp.push_str("a=rtpmap:8 PCMA/8000\r\n");
    sdp.push_str("a=rtpmap:0 PCMU/8000\r\n");
    sdp.push_str("a=recvonly\r\n");
    if tcp {
        sdp.push_str("a=setup:active\r\n");
        sdp.push_str("a=connection:new\r\n");
    }
    sdp.push_str(&format!("y={ssrc}\r\n"));
    sdp
}

fn parse_origin(s: &str) -> Result<Origin> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 6 {
        return Err(Error::Sip("o= 格式非法".into()));
    }
    Ok(Origin {
        username: parts[0].into(),
        sess_id: parts[1].into(),
        sess_version: parts[2].into(),
        net_type: parts[3].into(),
        addr_type: parts[4].into(),
        addr: parts[5].into(),
    })
}

fn parse_connection(s: &str) -> Result<Connection> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 3 {
        return Err(Error::Sip("c= 格式非法".into()));
    }
    Ok(Connection {
        net_type: parts[0].into(),
        addr_type: parts[1].into(),
        addr: parts[2].into(),
    })
}

fn parse_timing(s: &str) -> Result<Timing> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(Error::Sip("t= 格式非法".into()));
    }
    Ok(Timing {
        start: parts[0].parse().unwrap_or(0),
        stop: parts[1].parse().unwrap_or(0),
    })
}

fn parse_media(s: &str) -> Result<MediaDescription> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 4 {
        return Err(Error::Sip("m= 格式非法".into()));
    }
    Ok(MediaDescription::new(
        parts[0].into(),
        parts[1]
            .parse()
            .map_err(|_| Error::Sip("m= 端口非法".into()))?,
        parts[2].into(),
        parts[3..].iter().map(|&s| s.to_string()).collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 广播offer_recvonly与g711() {
        let ip: IpAddr = "192.168.1.50".parse().unwrap();
        let sdp = build_broadcast_offer("34020000001320000001", ip, 30000, 0x1234, false);
        assert!(sdp.contains("s=Broadcast\r\n"));
        assert!(sdp.contains("m=audio 30000 RTP/AVP 8 0\r\n"));
        assert!(sdp.contains("a=rtpmap:8 PCMA/8000\r\n"));
        assert!(sdp.contains("a=rtpmap:0 PCMU/8000\r\n"));
        assert!(sdp.contains("a=recvonly\r\n"));
        assert!(sdp.contains("y=4660\r\n")); // 0x1234
                                             // TCP 变体带 setup/connection + TCP/RTP/AVP。
        let tcp = build_broadcast_offer("d", ip, 30000, 1, true);
        assert!(tcp.contains("m=audio 30000 TCP/RTP/AVP 8 0\r\n"));
        assert!(tcp.contains("a=setup:active\r\n"));
    }

    const SAMPLE_SDP: &str = "v=0\r
o=Platform 0 0 IN IP4 192.168.1.100\r
s=Play\r
c=IN IP4 192.168.1.100\r
t=0 0\r
m=video 30000 RTP/AVP 96\r
a=rtpmap:96 PS/90000\r
y=1234567890\r
";

    #[test]
    fn 解析_sdp_提取关键字段() {
        let sdp = SessionDescription::parse(SAMPLE_SDP).unwrap();
        assert_eq!(sdp.origin.username, "Platform");
        assert_eq!(sdp.connection.addr, "192.168.1.100");
        assert_eq!(sdp.media.port, 30000);
        assert_eq!(sdp.media.ssrc, Some(1234567890));
    }

    #[test]
    fn 构造设备侧_sdp() {
        let ip: IpAddr = "10.0.0.2".parse().unwrap();
        let sdp = SessionDescription::new_device_response(
            "34020000001320000132",
            ip,
            8000,
            0xAABBCCDD,
            false,
        );
        let text = sdp.to_string();
        assert!(text.contains("o=34020000001320000132 "));
        assert!(text.contains("c=IN IP4 10.0.0.2"));
        assert!(text.contains("m=video 8000 RTP/AVP 96"));
        assert!(text.contains("y=2864434397")); // 0xAABBCCDD
        assert!(text.contains("a=sendonly"), "设备是发送方,应声明 sendonly");
        assert!(
            !text.contains("m=video 0 "),
            "端口 0 在 SDP 里表示媒体被禁用"
        );

        // TCP 模式 proto 应为 TCP/RTP/AVP 且带 a=setup。
        let sdp_tcp = SessionDescription::new_device_response("dev", ip, 8000, 1, true);
        let tcp_text = sdp_tcp.to_string();
        assert!(tcp_text.contains("m=video 8000 TCP/RTP/AVP 96"));
        assert!(tcp_text.contains("a=setup:active"));
    }

    #[test]
    fn 音视频复合流保留两条_m_行() {
        let av = "v=0\r\n\
o=34020000001320000001 0 0 IN IP4 192.168.1.100\r\n\
s=Play\r\n\
c=IN IP4 192.168.1.100\r\n\
t=0 0\r\n\
m=video 30000 RTP/AVP 96 98\r\n\
a=rtpmap:96 PS/90000\r\n\
a=rtpmap:98 H264/90000\r\n\
m=audio 30002 RTP/AVP 8\r\n\
a=rtpmap:8 PCMA/8000\r\n\
a=sendonly\r\n\
y=1234567890\r\n";
        let sdp = SessionDescription::parse(av).unwrap();
        assert_eq!(sdp.media.media, "video");
        assert_eq!(sdp.media.port, 30000);
        assert_eq!(
            sdp.media.rtpmaps,
            vec!["96 PS/90000".to_string(), "98 H264/90000".to_string()],
            "视频段的多条 rtpmap 都要保留"
        );
        let audio = sdp.audio.as_ref().expect("音频段不应被丢弃");
        assert_eq!(audio.port, 30002);
        assert_eq!(audio.rtpmaps, vec!["8 PCMA/8000".to_string()]);
        assert_eq!(sdp.direction, Some(Direction::SendOnly));

        // 往返后音频段仍在。
        let back = SessionDescription::parse(&sdp.to_string()).unwrap();
        assert_eq!(back, sdp);
    }

    #[test]
    fn 解析_setup_并推导本端角色() {
        let base = "v=0\r\n\
o=p 0 0 IN IP4 192.168.1.100\r\n\
s=Play\r\n\
c=IN IP4 192.168.1.100\r\n\
t=0 0\r\n\
m=video 30000 TCP/RTP/AVP 96\r\n\
a=rtpmap:96 PS/90000\r\n";

        // 平台声明 passive → 设备应 active(主动连平台)。
        let passive = SessionDescription::parse(&format!("{base}a=setup:passive\r\n")).unwrap();
        assert_eq!(passive.setup, Some(Setup::Passive));
        assert_eq!(passive.local_tcp_setup(), Setup::Active);

        // 平台声明 active → 设备应 passive(监听等平台连)。
        let active = SessionDescription::parse(&format!("{base}a=setup:active\r\n")).unwrap();
        assert_eq!(active.local_tcp_setup(), Setup::Passive);

        // 未声明 → 按国标惯例设备主动连。
        let none = SessionDescription::parse(base).unwrap();
        assert_eq!(none.setup, None);
        assert_eq!(none.local_tcp_setup(), Setup::Active);
        assert!(none.is_tcp());
    }

    #[test]
    fn sdp_往返() {
        let sdp = SessionDescription::parse(SAMPLE_SDP).unwrap();
        let text = sdp.to_string();
        let back = SessionDescription::parse(&text).unwrap();
        assert_eq!(sdp, back);
    }

    #[test]
    fn 解析下载会话_倍速与会话名() {
        let dl = "v=0\r\n\
o=34020000001320000001 0 0 IN IP4 192.168.1.100\r\n\
s=Download\r\n\
c=IN IP4 192.168.1.100\r\n\
t=0 0\r\n\
m=video 30000 RTP/AVP 96\r\n\
a=rtpmap:96 PS/90000\r\n\
a=downloadspeed:4\r\n\
y=1234567890\r\n";
        let sdp = SessionDescription::parse(dl).unwrap();
        assert!(sdp.is_download());
        assert!(!sdp.is_playback());
        assert_eq!(sdp.download_speed, Some(4));
    }

    #[test]
    fn 普通点播非下载非回放() {
        let sdp = SessionDescription::parse(SAMPLE_SDP).unwrap();
        assert!(!sdp.is_download());
        assert!(!sdp.is_playback());
        assert_eq!(sdp.download_speed, None);
    }
}
