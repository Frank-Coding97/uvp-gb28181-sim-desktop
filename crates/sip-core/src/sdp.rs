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
    /// 媒体参数(附录 G `f=` 字段)。平台的 INVITE 与设备的应答都可携带。
    pub format: Option<MediaFormat>,
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
        let mut format = None;

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
            } else if let Some(rest) = line.strip_prefix("f=") {
                // f= 解析失败不该让整条 SDP 失败:媒体参数是描述性的,
                // 端口/地址/SSRC 才决定链路能否建立。
                format = MediaFormat::parse(rest);
                if format.is_none() {
                    tracing::debug!(field = %rest.trim(), "SDP f= 字段格式无法识别,忽略");
                }
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
            format,
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
            format: None,
        }
    }

    /// 是否为纯音频会话(m= 行只有 audio)。
    ///
    /// 语音对讲的上行方向(§9.12.2 a) + §9.2)平台会只请求 `m=audio`,
    /// 这时按视频源去准备推流是错的。
    pub fn is_audio_only(&self) -> bool {
        self.media.media.eq_ignore_ascii_case("audio") && self.audio.is_none()
    }

    /// 构造**纯音频**的设备侧 200 OK(语音对讲上行,§9.12.2 a))。
    ///
    /// 按附录 C.2.4 a):G.711 A 律负载类型 8、编码名 PCMA、时钟 8 kHz、单声道,
    /// `m` 字段 media 项为 `audio`。设备是音频发送方,故 `a=sendonly`。
    /// 注意这条流**不做 PS 封装** —— 语音比特流直接用 RTP 打包。
    pub fn new_device_audio_response(
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
            "audio".into(),
            rtp_port,
            if tcp {
                "TCP/RTP/AVP".into()
            } else {
                "RTP/AVP".into()
            },
            vec!["8".into()],
        );
        media.push_rtpmap("8 PCMA/8000".into());
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
            direction: Some(Direction::SendOnly),
            setup: if tcp {
                Some(setup.unwrap_or(Setup::Active))
            } else {
                None
            },
            download_speed: None,
            format: Some(MediaFormat::g711_audio()),
        }
    }

    /// 附加媒体参数(附录 G `f=`),链式使用。
    pub fn with_format(mut self, format: MediaFormat) -> Self {
        self.format = Some(format);
        self
    }
}

/// 取 `f=` 分段里的第 i 项并解析成整数;缺失或空串返回 None。
fn f_num(list: &[&str], i: usize) -> Option<u32> {
    list.get(i)?.trim().parse().ok()
}

/// 取 `f=` 分段里的第 i 项文本;缺失或空串返回 None。
fn f_text(list: &[&str], i: usize) -> Option<String> {
    let s = list.get(i)?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// 附录 G `f=` 字段:媒体参数描述。
///
/// 格式:`f=v/编码格式/分辨率/帧率/码率类型/码率大小a/编码格式/码率大小/采样率`。
/// 视频段与音频段之间**没有分隔符**,音频段直接以 `a` 开头(如 `f=v/////a/1/8/1`)。
/// 任一参数可留空,`v/////` 就表示"本会话没有视频参数可报"。
///
/// 各字段留成 `Option<u32>` / `Option<String>` 是为了**无损**:平台报什么就存什么,
/// 不猜默认值 —— 猜出来的帧率会让人以为设备真的协商过这个值。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MediaFormat {
    /// 视频编码格式:1 MPEG-4、2 H.264、3 SVAC、4 3GP、5 H.265。
    pub video_codec: Option<u32>,
    /// 分辨率:1 QCIF、2 CIF、3 4CIF、4 D1、5 720P、6 1080P/I,其余用 `WxH`。
    pub resolution: Option<String>,
    /// 帧率 0~99。
    pub fps: Option<u32>,
    /// 码率类型:1 固定码率(CBR)、2 可变码率(VBR)。
    pub bitrate_type: Option<u32>,
    /// 码率大小(kbps)。
    pub bitrate_kbps: Option<u32>,
    /// 音频编码格式:1 G.711、2 G.723.1、3 G.729、4 G.722.1、5 SVAC、6 AAC。
    pub audio_codec: Option<u32>,
    /// 音频码率档位。G.711 用 8(= 64 kbps)。
    pub audio_bitrate: Option<u32>,
    /// 音频采样率档位。1 = 8 kHz(G.711 固定用 1)。
    pub audio_sample_rate: Option<u32>,
}

/// 附录 G 的视频编码格式取值。
pub const F_VIDEO_H264: u32 = 2;
/// 附录 G 的视频编码格式取值(H.265/HEVC)。
pub const F_VIDEO_H265: u32 = 5;
/// 附录 G 的音频编码格式取值(G.711)。
pub const F_AUDIO_G711: u32 = 1;
/// 附录 G 的音频码率档位 8 = 64 kbps(注4:G.711 中使用 8)。
pub const F_AUDIO_BITRATE_64K: u32 = 8;
/// 附录 G 的音频采样率档位 1 = 8 kHz(注7:G.711 中使用 1)。
pub const F_AUDIO_RATE_8K: u32 = 1;

impl MediaFormat {
    /// 纯 G.711 音频:`f=v/////a/1/8/1`(标准 §9.12.1.3.2 的语音广播示例)。
    pub fn g711_audio() -> Self {
        MediaFormat {
            audio_codec: Some(F_AUDIO_G711),
            audio_bitrate: Some(F_AUDIO_BITRATE_64K),
            audio_sample_rate: Some(F_AUDIO_RATE_8K),
            ..Default::default()
        }
    }

    /// 视频(可带 G.711 音频)的媒体参数。分辨率/码率未知时传 None 留空。
    pub fn video(
        codec: u32,
        fps: Option<u32>,
        bitrate_kbps: Option<u32>,
        with_audio: bool,
    ) -> Self {
        let mut f = MediaFormat {
            video_codec: Some(codec),
            fps,
            // 报了码率就必须说明码率类型,否则平台侧字段对不齐。
            bitrate_type: bitrate_kbps.map(|_| 1),
            bitrate_kbps,
            ..Default::default()
        };
        if with_audio {
            let audio = Self::g711_audio();
            f.audio_codec = audio.audio_codec;
            f.audio_bitrate = audio.audio_bitrate;
            f.audio_sample_rate = audio.audio_sample_rate;
        }
        f
    }

    /// 是否携带音频段。
    pub fn has_audio(&self) -> bool {
        self.audio_codec.is_some()
            || self.audio_bitrate.is_some()
            || self.audio_sample_rate.is_some()
    }

    /// 解析 `f=` 的值(不含 `f=` 前缀)。非 `v` 开头视为非法,返回 None。
    pub fn parse(field: &str) -> Option<Self> {
        let rest = field.trim().strip_prefix('v')?;
        // 音频段以 'a' 起头且紧贴视频段;分辨率取值只可能是数字或 WxH,不含 'a'。
        let (video_part, audio_part) = match rest.split_once('a') {
            Some((v, a)) => (v, Some(a)),
            None => (rest, None),
        };
        // 前导空段来自 "v/" 的那个斜杠,跳过。
        let v: Vec<&str> = video_part.split('/').skip(1).collect();
        let a: Vec<&str> = audio_part
            .map(|s| s.split('/').skip(1).collect())
            .unwrap_or_default();
        Some(MediaFormat {
            video_codec: f_num(&v, 0),
            resolution: f_text(&v, 1),
            fps: f_num(&v, 2),
            bitrate_type: f_num(&v, 3),
            bitrate_kbps: f_num(&v, 4),
            audio_codec: f_num(&a, 0),
            audio_bitrate: f_num(&a, 1),
            audio_sample_rate: f_num(&a, 2),
        })
    }

    /// 渲染成 `f=` 的值(不含 `f=` 前缀)。
    pub fn to_field(&self) -> String {
        fn n(v: Option<u32>) -> String {
            v.map(|x| x.to_string()).unwrap_or_default()
        }
        let mut s = format!(
            "v/{}/{}/{}/{}/{}",
            n(self.video_codec),
            self.resolution.clone().unwrap_or_default(),
            n(self.fps),
            n(self.bitrate_type),
            n(self.bitrate_kbps),
        );
        if self.has_audio() {
            s.push_str(&format!(
                "a/{}/{}/{}",
                n(self.audio_codec),
                n(self.audio_bitrate),
                n(self.audio_sample_rate),
            ));
        }
        s
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
        // f= 排在 y= 之后:附录 G 的字段顺序里 y 在 f 之前。
        if let Some(ref format) = self.format {
            write!(f, "f={}\r\n", format.to_field())?;
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
/// 构造语音广播的 SDP offer(设备作接收方,收平台下行 G.711)。
///
/// 依据 GB/T 28181-2022 §9.12.1.3.2 的示例:`s=Play`(会话类型是实时点播,
/// 不是 "Broadcast" —— s 字段只有 Play/Playback/Download 三种取值,见附录 G)、
/// `m=audio <port> RTP/AVP 8`、`a=rtpmap:8 PCMA/8000`、`y=<ssrc>`、`f=v/////a/1/8/1`。
///
/// 方向是 `a=recvonly`:设备是**语音流接收者**,标准示例里的 `a=sendonly`
/// 是从媒体服务器(发送方)视角写的。
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
    sdp.push_str("s=Play\r\n");
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
    sdp.push_str(&format!("f={}\r\n", MediaFormat::g711_audio().to_field()));
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
        // s 字段只有 Play/Playback/Download 三种取值(附录 G),语音广播用 Play
        // (§9.12.1.3.2 示例)。早先写的 "s=Broadcast" 不是合法取值。
        assert!(sdp.contains("s=Play\r\n"), "实际:{sdp}");
        assert!(sdp.contains("m=audio 30000 RTP/AVP 8 0\r\n"));
        assert!(sdp.contains("a=rtpmap:8 PCMA/8000\r\n"));
        assert!(sdp.contains("a=rtpmap:0 PCMU/8000\r\n"));
        assert!(sdp.contains("a=recvonly\r\n"));
        assert!(sdp.contains("y=4660\r\n")); // 0x1234
                                             // §9.12.1.3.2 的音频参数描述。
        assert!(sdp.contains("f=v/////a/1/8/1\r\n"), "实际:{sdp}");
        // TCP 变体带 setup/connection + TCP/RTP/AVP。
        let tcp = build_broadcast_offer("d", ip, 30000, 1, true);
        assert!(tcp.contains("m=audio 30000 TCP/RTP/AVP 8 0\r\n"));
        assert!(tcp.contains("a=setup:active\r\n"));
    }

    #[test]
    fn f字段_解析与渲染往返() {
        // 标准示例:纯音频。
        let f = MediaFormat::parse("v/////a/1/8/1").expect("parse");
        assert_eq!(f, MediaFormat::g711_audio());
        assert_eq!(f.to_field(), "v/////a/1/8/1");
        // 视频 + 音频:H.264 / 720P / 25fps / CBR / 2500kbps。
        let f = MediaFormat::parse("v/2/5/25/1/2500a/1/8/1").expect("parse");
        assert_eq!(f.video_codec, Some(F_VIDEO_H264));
        assert_eq!(f.resolution.as_deref(), Some("5"));
        assert_eq!(f.fps, Some(25));
        assert_eq!(f.bitrate_type, Some(1));
        assert_eq!(f.bitrate_kbps, Some(2500));
        assert_eq!(f.audio_codec, Some(F_AUDIO_G711));
        assert_eq!(f.to_field(), "v/2/5/25/1/2500a/1/8/1");
        // WxH 形式的分辨率。
        let f = MediaFormat::parse("v/5/1920x1080/30/2/4000").expect("parse");
        assert_eq!(f.video_codec, Some(F_VIDEO_H265));
        assert_eq!(f.resolution.as_deref(), Some("1920x1080"));
        assert!(!f.has_audio());
        assert_eq!(f.to_field(), "v/5/1920x1080/30/2/4000");
        // 非 v 开头判非法。
        assert!(MediaFormat::parse("a/1/8/1").is_none());
        assert!(MediaFormat::parse("").is_none());
    }

    #[test]
    fn 设备应答可附带f字段() {
        let ip: IpAddr = "192.168.1.50".parse().unwrap();
        let sdp = SessionDescription::new_device_response("d", ip, 30000, 1, false)
            .with_format(MediaFormat::video(F_VIDEO_H264, Some(25), Some(2500), true));
        let text = sdp.to_string();
        assert!(text.contains("f=v/2//25/1/2500a/1/8/1\r\n"), "实际:{text}");
        // y= 必须排在 f= 之前(附录 G 的字段顺序)。
        let y = text.find("y=").expect("y=");
        let f = text.find("f=").expect("f=");
        assert!(y < f);
    }

    #[test]
    fn 纯音频sdp被识别为audio_only() {
        let audio_only = "v=0\r\no=d 0 0 IN IP4 192.168.1.100\r\ns=Play\r\n\
c=IN IP4 192.168.1.100\r\nt=0 0\r\nm=audio 8000 RTP/AVP 8\r\n\
a=rtpmap:8 PCMA/8000\r\na=sendrecv\r\ny=1\r\nf=v/////a/1/8/1\r\n";
        let sdp = SessionDescription::parse(audio_only).expect("parse");
        assert!(sdp.is_audio_only(), "只有 m=audio 时应判为纯音频会话");
        assert_eq!(sdp.media.media, "audio");
        assert_eq!(sdp.media.port, 8000);
        assert_eq!(sdp.direction, Some(Direction::SendRecv));
        assert_eq!(sdp.format, Some(MediaFormat::g711_audio()));
        // 视音频复合流不是纯音频。
        let mixed = SessionDescription::parse(SAMPLE_SDP).expect("parse");
        assert!(!mixed.is_audio_only());
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
