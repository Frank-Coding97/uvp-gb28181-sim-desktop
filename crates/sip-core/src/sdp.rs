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
    /// SSRC(GB28181 y= 行)。
    pub ssrc: Option<u32>,
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
                }
            } else if let Some(rest) = line.strip_prefix("y=") {
                ssrc = rest.trim().parse::<u32>().ok();
            }
        }

        let mut media = media_desc.ok_or_else(|| Error::Sip("SDP 缺少 m= 行".into()))?;
        media.rtpmap = rtpmap;
        media.ssrc = ssrc;

        Ok(SessionDescription {
            origin: origin.ok_or_else(|| Error::Sip("SDP 缺少 o= 行".into()))?,
            session_name: session_name.unwrap_or_else(|| "Play".to_string()),
            connection: connection.ok_or_else(|| Error::Sip("SDP 缺少 c= 行".into()))?,
            timing: timing.unwrap_or(Timing { start: 0, stop: 0 }),
            media,
        })
    }

    /// 构造一个最简 GB28181 SDP(设备侧 200 OK):本端媒体地址、端口、SSRC。
    pub fn new_device_response(local_ip: IpAddr, rtp_port: u16, ssrc: u32) -> Self {
        let ip_str = local_ip.to_string();
        SessionDescription {
            origin: Origin {
                username: "Device".into(),
                sess_id: "0".into(),
                sess_version: "0".into(),
                net_type: "IN".into(),
                addr_type: if local_ip.is_ipv4() { "IP4" } else { "IP6" }.into(),
                addr: ip_str.clone(),
            },
            session_name: "Play".into(),
            connection: Connection {
                net_type: "IN".into(),
                addr_type: if local_ip.is_ipv4() { "IP4" } else { "IP6" }.into(),
                addr: ip_str,
            },
            timing: Timing { start: 0, stop: 0 },
            media: MediaDescription {
                media: "video".into(),
                port: rtp_port,
                proto: "RTP/AVP".into(),
                fmt: vec!["96".into()],
                rtpmap: Some("96 PS/90000".into()),
                ssrc: Some(ssrc),
            },
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
        if let Some(ssrc) = self.media.ssrc {
            write!(f, "y={}\r\n", ssrc)?;
        }
        Ok(())
    }
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let sdp = SessionDescription::new_device_response(ip, 8000, 0xAABBCCDD);
        let text = sdp.to_string();
        assert!(text.contains("c=IN IP4 10.0.0.2"));
        assert!(text.contains("m=video 8000 RTP/AVP 96"));
        assert!(text.contains("y=2864434397")); // 0xAABBCCDD
    }

    #[test]
    fn sdp_往返() {
        let sdp = SessionDescription::parse(SAMPLE_SDP).unwrap();
        let text = sdp.to_string();
        let back = SessionDescription::parse(&text).unwrap();
        assert_eq!(sdp, back);
    }
}
