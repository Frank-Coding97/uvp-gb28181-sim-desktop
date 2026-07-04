//! PS(Program Stream)封装。
//!
//! 实现 docs/40-protocol/media-ps-rtp.md 的 PS 层:把一帧 H.264 访问单元封成
//! Program Stream。每帧输出 = Pack Header (+ 关键帧带 System Header + PSM) + PES。
//! 时间戳基于 90kHz。这是能被主流国标平台(WVP 等)解析的最小可用封装。

/// 视频流 ID(PES stream_id,视频为 0xE0)。
const STREAM_ID_VIDEO: u8 = 0xE0;
/// 音频流 ID(PES stream_id,音频为 0xC0)。
const STREAM_ID_AUDIO: u8 = 0xC0;

/// 把 90kHz 时间戳编码为 PES 里的 5 字节 PTS/DTS 结构。
/// `marker_bits` 为高 4 位标志(PTS-only=0b0010,PTS+DTS 时 PTS=0b0011)。
fn encode_pts(ts: u32, marker_bits: u8) -> [u8; 5] {
    let ts = ts as u64;
    let mut b = [0u8; 5];
    // '0010'/'0011' + PTS[32..30] + marker
    b[0] = (marker_bits << 4) | ((((ts >> 30) & 0x07) as u8) << 1) | 0x01;
    b[1] = ((ts >> 22) & 0xff) as u8;
    b[2] = ((((ts >> 15) & 0x7f) as u8) << 1) | 0x01;
    b[3] = ((ts >> 7) & 0xff) as u8;
    b[4] = ((((ts) & 0x7f) as u8) << 1) | 0x01;
    b
}

/// 写入 MPEG-2 PS Pack Header(00 00 01 BA + 10 字节)。
///
/// 严格按 ISO/IEC 13818-1 §2.5.3.2 打包 SCR(33bit,分 3/15/15)+ 各 marker 位:
///   `01` + SCR[32..30] + m + SCR[29..15] + m + SCR[14..0] + m + SCR_ext(9,置0) + m
///   + program_mux_rate(22) + m + m + reserved(5) + pack_stuffing_length(3)。
fn write_pack_header(out: &mut Vec<u8>, scr: u64) {
    out.extend_from_slice(&[0x00, 0x00, 0x01, 0xBA]);
    let scr = scr & 0x1_FFFF_FFFF; // 33 bit
    let s32_30 = ((scr >> 30) & 0x07) as u8;
    let s29_15 = ((scr >> 15) & 0x7FFF) as u16;
    let s14_0 = (scr & 0x7FFF) as u16;

    // 字节 4:'01' + SCR[32..30](3) + marker(1) + SCR[29..28](2)
    out.push(0x40 | (s32_30 << 3) | 0x04 | ((s29_15 >> 13) as u8 & 0x03));
    // 字节 5:SCR[27..20](8)
    out.push((s29_15 >> 5) as u8);
    // 字节 6:SCR[19..15](5) + marker(1) + SCR[14..13](2)
    out.push((((s29_15 & 0x1F) as u8) << 3) | 0x04 | ((s14_0 >> 13) as u8 & 0x03));
    // 字节 7:SCR[12..5](8)
    out.push((s14_0 >> 5) as u8);
    // 字节 8:SCR[4..0](5) + marker(1) + SCR_ext[8..7](2,置0)
    out.push((((s14_0 & 0x1F) as u8) << 3) | 0x04);
    // 字节 9:SCR_ext[6..0](7,置0) + marker(1)
    out.push(0x01);
    // 字节 10-12:program_mux_rate(22) + marker + marker。固定 mux_rate。
    let mux_rate: u32 = 6106; // ~ 27.9 Mbit? 用常见占位值,平台不校验
    out.push((mux_rate >> 14) as u8);
    out.push((mux_rate >> 6) as u8);
    out.push((((mux_rate & 0x3F) as u8) << 2) | 0x03);
    // 字节 13:reserved(5,置1) + pack_stuffing_length(3=0)
    out.push(0xF8);
}

/// 写入 System Header(00 00 01 BB ...),仅关键帧带。极简固定内容。
fn write_system_header(out: &mut Vec<u8>) {
    out.extend_from_slice(&[0x00, 0x00, 0x01, 0xBB]);
    out.extend_from_slice(&[0x00, 0x0C]); // header_length = 12
    out.extend_from_slice(&[0x80, 0x1D, 0x81]); // rate_bound 等(固定)
    out.extend_from_slice(&[0x04, 0xE1, 0x7F]); // audio/video bound
    out.extend_from_slice(&[0xE0, 0xE0, 0xE8]); // video stream 描述
    out.extend_from_slice(&[0xC0, 0xC0, 0x20]); // audio 占位
}

/// 视频编码,决定 PSM 的 stream_type(GB/T 28181 附录 / ISO13818-1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoCodec {
    /// H.264/AVC,stream_type 0x1B。
    #[default]
    H264,
    /// H.265/HEVC,stream_type 0x24。
    H265,
}

impl VideoCodec {
    /// PSM 中的 stream_type。
    pub fn stream_type(&self) -> u8 {
        match self {
            VideoCodec::H264 => 0x1B,
            VideoCodec::H265 => 0x24,
        }
    }
}

/// 音频编码,决定 PSM 的 stream_type。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioCodec {
    /// G.711 A-law,stream_type 0x90。
    #[default]
    G711A,
    /// AAC(ADTS),stream_type 0x0F。
    Aac,
}

impl AudioCodec {
    /// PSM 中的 stream_type。
    pub fn stream_type(&self) -> u8 {
        match self {
            AudioCodec::G711A => 0x90,
            AudioCodec::Aac => 0x0F,
        }
    }
}

/// 写入 Program Stream Map(00 00 01 BC),声明单条视频 ES(按 `video` 编码)。
///
/// program_stream_map_length = 其后到末尾(含 CRC32)的字节数。
fn write_psm(out: &mut Vec<u8>, video: VideoCodec) {
    out.extend_from_slice(&[0x00, 0x00, 0x01, 0xBC]);
    // 主体:marker/version(2) + program_stream_info_length(2)=0
    // + elementary_stream_map_length(2)=4 + 一条 ES 映射(4)+ CRC32(4)。
    let body: [u8; 12] = [
        0xE1,
        0xFF, // marker/version
        0x00,
        0x00, // program_stream_info_length = 0
        0x00,
        0x04, // elementary_stream_map_length = 4
        video.stream_type(),
        0xE0,
        0x00,
        0x00, // ES map: 视频(elementary_stream_id 0xE0)
        0x00,
        0x00, // CRC32 前 2 字节(占位,平台通常不校验)
    ];
    let psm_len = (body.len() + 2) as u16;
    out.extend_from_slice(&psm_len.to_be_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(&[0x00, 0x00]); // CRC32 后 2 字节占位
}

/// 写入含音频的 PSM:声明视频(按 `video`)+ 音频(按 `audio`)两条 ES。
fn write_psm_av(out: &mut Vec<u8>, video: VideoCodec, audio: AudioCodec) {
    out.extend_from_slice(&[0x00, 0x00, 0x01, 0xBC]);
    // elementary_stream_map_length = 8(两条 ES 映射,各 4 字节)。
    let body: [u8; 16] = [
        0xE1,
        0xFF, // marker/version
        0x00,
        0x00, // program_stream_info_length = 0
        0x00,
        0x08, // elementary_stream_map_length = 8
        video.stream_type(),
        0xE0,
        0x00,
        0x00, // ES map: 视频(0xE0)
        audio.stream_type(),
        0xC0,
        0x00,
        0x00, // ES map: 音频(0xC0)
        0x00,
        0x00, // CRC32 前 2 字节占位
    ];
    let psm_len = (body.len() + 2) as u16;
    out.extend_from_slice(&psm_len.to_be_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(&[0x00, 0x00]); // CRC32 后 2 字节占位
}

/// 写入一个视频 PES 包(stream_id 0xE0),承载 `data`(可能是一帧的一部分)。
fn write_pes(out: &mut Vec<u8>, data: &[u8], pts: u32, with_pts: bool) {
    write_pes_stream(out, STREAM_ID_VIDEO, data, pts, with_pts);
}

/// 写入一个 PES 包,指定 stream_id(视频 0xE0 / 音频 0xC0)。
fn write_pes_stream(out: &mut Vec<u8>, stream_id: u8, data: &[u8], pts: u32, with_pts: bool) {
    out.extend_from_slice(&[0x00, 0x00, 0x01, stream_id]);
    let header = if with_pts {
        encode_pts(pts, 0b0010)
    } else {
        [0u8; 5]
    };
    let header_len = if with_pts { 5u8 } else { 0 };
    let pes_len = data.len() + 3 + header_len as usize; // 3 = 后续两个标志字节 + header_len 字段
                                                        // PES_packet_length(可为 0 表示未指定,视频常见;这里给真实值,超 65535 则置 0)
    let len_field = if pes_len > 0xffff { 0 } else { pes_len as u16 };
    out.extend_from_slice(&len_field.to_be_bytes());
    out.push(0x80); // '10' + 各标志=0
    out.push(if with_pts { 0x80 } else { 0x00 }); // PTS_DTS_flags=10
    out.push(header_len);
    if with_pts {
        out.extend_from_slice(&header);
    }
    out.extend_from_slice(data);
}

/// PS 封装器:维护 90kHz 时间戳,把视频访问单元封成 PS 字节流。
pub struct PsMuxer {
    /// 单个 PES 最大负载(过大时拆多个 PES)。
    pes_max: usize,
    /// 视频编码(决定 PSM stream_type)。
    video: VideoCodec,
    /// 音频编码(决定 PSM stream_type)。
    audio: AudioCodec,
}

impl Default for PsMuxer {
    fn default() -> Self {
        PsMuxer {
            pes_max: 60_000,
            video: VideoCodec::default(),
            audio: AudioCodec::default(),
        }
    }
}

impl PsMuxer {
    /// 新建封装器(H.264 + G.711A)。
    pub fn new() -> Self {
        Self::default()
    }

    /// 指定视频编码(H.264/H.265)新建。
    pub fn with_video(video: VideoCodec) -> Self {
        PsMuxer {
            video,
            ..Self::default()
        }
    }

    /// 指定视频与音频编码新建。
    pub fn with_codecs(video: VideoCodec, audio: AudioCodec) -> Self {
        PsMuxer {
            pes_max: 60_000,
            video,
            audio,
        }
    }

    /// 封装一帧 H.264 访问单元(Annex B,含起始码)。
    /// `key_frame` 为真时前置 System Header + PSM(便于平台快速出图)。
    /// 返回可直接交给 RTP 分片发送的 PS 字节。
    pub fn mux_frame(&self, au: &[u8], pts: u32, key_frame: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(au.len() + 64);
        write_pack_header(&mut out, pts as u64);
        if key_frame {
            write_system_header(&mut out);
            write_psm(&mut out, self.video);
        }
        // 大帧拆多个 PES,首个 PES 带 PTS。
        let mut first = true;
        for chunk in au.chunks(self.pes_max.max(1)) {
            write_pes(&mut out, chunk, pts, first);
            first = false;
        }
        out
    }

    /// 封装一帧视频 + 其后紧随的音频包(G.711A 音视频复合流)。
    /// `audio` 为该帧时间窗内的若干 G.711A 分包(每包一个音频 PES);为空则等价于 [`mux_frame`]。
    /// 关键帧处 PSM 声明音视频两条 ES。
    pub fn mux_frame_av(&self, au: &[u8], pts: u32, key_frame: bool, audio: &[Vec<u8>]) -> Vec<u8> {
        let has_audio = !audio.is_empty();
        let mut out = Vec::with_capacity(au.len() + 128);
        write_pack_header(&mut out, pts as u64);
        if key_frame {
            write_system_header(&mut out);
            if has_audio {
                write_psm_av(&mut out, self.video, self.audio);
            } else {
                write_psm(&mut out, self.video);
            }
        }
        // 视频 PES(大帧拆分,首个带 PTS)。
        let mut first = true;
        for chunk in au.chunks(self.pes_max.max(1)) {
            write_pes(&mut out, chunk, pts, first);
            first = false;
        }
        // 音频 PES(stream_id 0xC0),各带 PTS,紧随视频后。
        for a in audio {
            write_pes_stream(&mut out, STREAM_ID_AUDIO, a, pts, true);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 关键帧含_pack_system_psm() {
        let mux = PsMuxer::new();
        let au = &[0x00, 0x00, 0x00, 0x01, 0x67, 0x42]; // 伪 SPS
        let ps = mux.mux_frame(au, 900, true);
        // Pack header 起始。
        assert_eq!(&ps[0..4], &[0x00, 0x00, 0x01, 0xBA]);
        // 含 System Header(BB)与 PSM(BC)。
        assert!(ps.windows(4).any(|w| w == [0x00, 0x00, 0x01, 0xBB]));
        assert!(ps.windows(4).any(|w| w == [0x00, 0x00, 0x01, 0xBC]));
        // 含视频 PES(E0)。
        assert!(ps.windows(4).any(|w| w == [0x00, 0x00, 0x01, 0xE0]));
    }

    #[test]
    fn 非关键帧仅_pack_pes() {
        let mux = PsMuxer::new();
        let ps = mux.mux_frame(&[0x00, 0x00, 0x01, 0x61], 1800, false);
        assert_eq!(&ps[0..4], &[0x00, 0x00, 0x01, 0xBA]);
        assert!(!ps.windows(4).any(|w| w == [0x00, 0x00, 0x01, 0xBB])); // 无 System Header
        assert!(ps.windows(4).any(|w| w == [0x00, 0x00, 0x01, 0xE0])); // 有 PES
    }

    #[test]
    fn 音视频复合流含音频pes与音频psm() {
        let mux = PsMuxer::new();
        let au = &[0x00, 0x00, 0x00, 0x01, 0x65, 0x11]; // 伪 IDR
        let audio = vec![vec![0xD5u8; 160], vec![0xD5u8; 160]]; // 两个 G.711A 包
        let ps = mux.mux_frame_av(au, 900, true, &audio);
        // 含视频 PES(E0)与音频 PES(C0)。
        assert!(ps.windows(4).any(|w| w == [0x00, 0x00, 0x01, 0xE0]));
        assert!(ps.windows(4).any(|w| w == [0x00, 0x00, 0x01, 0xC0]));
        // PSM(BC)含音频 ES 映射(0x90 0xC0)。
        assert!(ps.windows(4).any(|w| w == [0x00, 0x00, 0x01, 0xBC]));
        assert!(ps.windows(2).any(|w| w == [0x90, 0xC0]));
    }

    #[test]
    fn 无音频时av等价纯视频_不含c0() {
        let mux = PsMuxer::new();
        let ps = mux.mux_frame_av(&[0x00, 0x00, 0x01, 0x61], 1800, false, &[]);
        assert!(!ps.windows(4).any(|w| w == [0x00, 0x00, 0x01, 0xC0])); // 无音频 PES
        assert!(ps.windows(4).any(|w| w == [0x00, 0x00, 0x01, 0xE0])); // 有视频 PES
    }

    #[test]
    fn h265_psm_stream_type_0x24() {
        let mux = PsMuxer::with_video(VideoCodec::H265);
        let ps = mux.mux_frame(&[0x00, 0x00, 0x00, 0x01, 0x40, 0x01], 900, true); // 伪 VPS
                                                                                  // PSM 视频 ES 映射 stream_type=0x24, elementary_stream_id=0xE0。
        assert!(ps.windows(2).any(|w| w == [0x24, 0xE0]));
        assert!(!ps.windows(2).any(|w| w == [0x1B, 0xE0])); // 不再是 H.264
    }

    #[test]
    fn aac_psm_stream_type_0x0f() {
        let mux = PsMuxer::with_codecs(VideoCodec::H264, AudioCodec::Aac);
        let au = &[0x00, 0x00, 0x00, 0x01, 0x65, 0x11];
        let audio = vec![vec![0xFFu8; 32]]; // 伪 AAC ADTS
        let ps = mux.mux_frame_av(au, 900, true, &audio);
        // 音频 ES 映射 stream_type=0x0F, elementary_stream_id=0xC0。
        assert!(ps.windows(2).any(|w| w == [0x0F, 0xC0]));
    }

    #[test]
    fn pts_编码_5字节() {
        let b = encode_pts(0, 0b0010);
        assert_eq!(b.len(), 5);
        assert_eq!(b[0] >> 4, 0b0010);
    }
}
