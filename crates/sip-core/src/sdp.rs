//! SDP(Session Description Protocol)解析与构造。
//!
//! GB28181 使用的 SDP 子集:o/s/c/t/m=video/a=rtpmap/y=<ssrc>。
//! 实现足够解析平台 INVITE 的 SDP(提取 RTP 目标地址与 SSRC)、
//! 构造设备侧 200 OK 的 SDP(声明本端 RTP 端口与 SSRC)。

use std::fmt;
use std::net::IpAddr;

use common::{Error, Result};

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
    /// 媒体描述(m=video ...)。
    pub media: MediaDescription,
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
    /// rtpmap(如 "96 PS/90000")。
    pub rtpmap: Option<String>,
    /// SSRC 数值(GB28181 y= 行),用于填 RTP 头。
    pub ssrc: Option<u32>,
    /// SSRC 原始文本,用于回显 y= 行。
    ///
    /// 国标 SSRC 是**定长 10 位十进制字符串**(首位 0=实时/1=回放,
    /// 中 5 位域区号,末 4 位序号),如 `0200004235`。若按 u32 解析再
    /// 序列化会丢掉前导零(变成 `200004235`),平台按字符串匹配收不到流。
    /// 故解析时保留原文,回显时原样写回。
    pub ssrc_raw: Option<String>,
}

impl SessionDescription {
    /// 解析 SDP 文本(逐行提取 o/s/c/t/m/a/y)。
    pub fn parse(text: &str) -> Result<Self> {
        let mut origin = None;
        let mut session_name = None;
        let mut connection = None;
        let mut timing = None;
        let mut media_desc = None;
        let mut rtpmap = None;
        let mut ssrc = None;
        let mut ssrc_raw = None;
        let mut download_speed = None;

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
                media_desc = Some(parse_media(rest)?);
            } else if let Some(rest) = line.strip_prefix("a=") {
                if rest.starts_with("rtpmap:") {
                    rtpmap = Some(rest.trim_start_matches("rtpmap:").to_string());
                } else if let Some(spd) = rest.strip_prefix("downloadspeed:") {
                    // 下载倍速可能写成 "4" 或 "4:1",取首段。
                    download_speed = spd.split(':').next().and_then(|s| s.trim().parse().ok());
                }
            } else if let Some(rest) = line.strip_prefix("y=") {
                let raw = rest.trim();
                ssrc = raw.parse::<u32>().ok();
                // 保留原文以便回显时不丢前导零(国标 SSRC 定长 10 位)。
                if ssrc.is_some() {
                    ssrc_raw = Some(raw.to_string());
                }
            }
        }

        let mut media = media_desc.ok_or_else(|| Error::Sip("SDP 缺少 m= 行".into()))?;
        media.rtpmap = rtpmap;
        media.ssrc = ssrc;
        media.ssrc_raw = ssrc_raw;

        Ok(SessionDescription {
            origin: origin.ok_or_else(|| Error::Sip("SDP 缺少 o= 行".into()))?,
            session_name: session_name.unwrap_or_else(|| "Play".to_string()),
            connection: connection.ok_or_else(|| Error::Sip("SDP 缺少 c= 行".into()))?,
            timing: timing.unwrap_or(Timing { start: 0, stop: 0 }),
            media,
            download_speed,
        })
    }

    /// 构造一个最简 GB28181 SDP(设备侧 200 OK):本端媒体地址、端口、SSRC。
    ///
    /// - `username`:o= 行用户名,填设备/通道 ID(平台 ACK 会据此定位)。
    /// - `tcp`:true 时 m= proto 用 `TCP/RTP/AVP`(与平台一致),否则 `RTP/AVP`。
    /// - `ssrc_raw`:平台 `y=` 行原文。国标 SSRC 定长 10 位,含前导零(如
    ///   `0200004235`),平台按**字符串**匹配来流,故必须原样回显;传 None 时
    ///   按数值格式化(仅在平台未给 SSRC 的退化场景)。
    pub fn new_device_response(
        username: &str,
        local_ip: IpAddr,
        rtp_port: u16,
        ssrc: u32,
        ssrc_raw: Option<String>,
        tcp: bool,
    ) -> Self {
        let ip_str = local_ip.to_string();
        let addr_type = if local_ip.is_ipv4() { "IP4" } else { "IP6" };
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
            media: MediaDescription {
                media: "video".into(),
                port: rtp_port,
                proto: if tcp {
                    "TCP/RTP/AVP".into()
                } else {
                    "RTP/AVP".into()
                },
                fmt: vec!["96".into()],
                rtpmap: Some("96 PS/90000".into()),
                ssrc: Some(ssrc),
                ssrc_raw,
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
        write!(
            f,
            "m={} {} {} {}\r\n",
            self.media.media,
            self.media.port,
            self.media.proto,
            self.media.fmt.join(" ")
        )?;
        if let Some(ref rm) = self.media.rtpmap {
            write!(f, "a=rtpmap:{}\r\n", rm)?;
        }
        // TCP 模式:设备侧 200 OK 必须回 a=setup:active + a=sendonly。
        if self.media.proto.contains("TCP") {
            write!(f, "a=setup:active\r\n")?;
            write!(f, "a=connection:new\r\n")?;
            write!(f, "a=sendonly\r\n")?;
        }
        // 优先写回原文,保住国标定长 10 位 SSRC 的前导零。
        if let Some(ref raw) = self.media.ssrc_raw {
            write!(f, "y={}\r\n", raw)?;
        } else if let Some(ssrc) = self.media.ssrc {
            write!(f, "y={}\r\n", ssrc)?;
        }
        Ok(())
    }
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
    Ok(MediaDescription {
        media: parts[0].into(),
        port: parts[1]
            .parse()
            .map_err(|_| Error::Sip("m= 端口非法".into()))?,
        proto: parts[2].into(),
        fmt: parts[3..].iter().map(|&s| s.to_string()).collect(),
        rtpmap: None,
        ssrc: None,
        ssrc_raw: None,
    })
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
            None,
            false,
        );
        let text = sdp.to_string();
        assert!(text.contains("o=34020000001320000132 "));
        assert!(text.contains("c=IN IP4 10.0.0.2"));
        assert!(text.contains("m=video 8000 RTP/AVP 96"));
        assert!(text.contains("y=2864434397")); // 0xAABBCCDD

        // TCP 模式 proto 应为 TCP/RTP/AVP。
        let sdp_tcp = SessionDescription::new_device_response("dev", ip, 8000, 1, None, true);
        assert!(sdp_tcp.to_string().contains("m=video 8000 TCP/RTP/AVP 96"));
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
