//! PS(Program Stream)封装。
//!
//! 实现 docs/40-protocol/media-ps-rtp.md 的 PS 层:把一帧 H.264 访问单元封成
//! Program Stream。每帧输出 = Pack Header (+ 关键帧带 System Header + PSM) + PES。
//! 时间戳基于 90kHz。这是能被主流国标平台(WVP 等)解析的最小可用封装。

/// 视频流 ID(PES stream_id,视频为 0xE0)。
const STREAM_ID_VIDEO: u8 = 0xE0;

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

/// 写入 Pack Header(00 00 01 BA + 10 字节 SCR/mux_rate)。
fn write_pack_header(out: &mut Vec<u8>, scr: u32) {
    out.extend_from_slice(&[0x00, 0x00, 0x01, 0xBA]);
    // MPEG-2 PS:'01' + SCR(33bit,这里用 scr 低位近似)+ mux_rate + stuffing。
    let s = scr as u64;
    let b0 = 0x44u8 | ((((s >> 30) & 0x07) as u8) << 3) | (0x04) ; // 简化:'01' + SCR 高位
    out.push(b0);
    out.push(((s >> 22) & 0xff) as u8);
    out.push((((s >> 15) & 0x7f) as u8) << 1 | 0x04 | (((s >> 20) & 0x03) as u8));
    out.push(((s >> 7) & 0xff) as u8);
    out.push((((s & 0x7f) as u8) << 1) | 0x04 | 0x01);
    // program_mux_rate(22bit)+ marker,固定给一个值。
    out.extend_from_slice(&[0x01, 0x89, 0xC3]);
    // reserved(5bit)+ pack_stuffing_length(3bit=0)
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

/// 写入 Program Stream Map(00 00 01 BC),声明视频为 H.264(stream_type 0x1B)。
fn write_psm(out: &mut Vec<u8>) {
    out.extend_from_slice(&[0x00, 0x00, 0x01, 0xBC]);
    // program_stream_map_length = 后续字节数。
    let map = [
        0xE1u8, 0x00, // current_next + reserved
        0x00, 0x00, // program_stream_info_length = 0
        0x00, 0x04, // elementary_stream_map_length = 4
        0x1B, 0xE0, 0x00, 0x00, // stream_type=0x1B(H.264), elem_stream_id=0xE0, info_len=0
    ];
    out.extend_from_slice(&((map.len() as u16 + 4).to_be_bytes())); // + CRC 占位
    out.extend_from_slice(&map);
    out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // CRC32 占位
}

/// 写入一个视频 PES 包,承载 `data`(可能是一帧的一部分)。
fn write_pes(out: &mut Vec<u8>, data: &[u8], pts: u32, with_pts: bool) {
    out.extend_from_slice(&[0x00, 0x00, 0x01, STREAM_ID_VIDEO]);
    let header = if with_pts { encode_pts(pts, 0b0010) } else { [0u8; 5] };
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

/// PS 封装器:维护 90kHz 时间戳,把 H.264 访问单元封成 PS 字节流。
pub struct PsMuxer {
    /// 单个 PES 最大负载(过大时拆多个 PES)。
    pes_max: usize,
}

impl Default for PsMuxer {
    fn default() -> Self {
        PsMuxer { pes_max: 60_000 }
    }
}

impl PsMuxer {
    /// 新建封装器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 封装一帧 H.264 访问单元(Annex B,含起始码)。
    /// `key_frame` 为真时前置 System Header + PSM(便于平台快速出图)。
    /// 返回可直接交给 RTP 分片发送的 PS 字节。
    pub fn mux_frame(&self, au: &[u8], pts: u32, key_frame: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(au.len() + 64);
        write_pack_header(&mut out, pts);
        if key_frame {
            write_system_header(&mut out);
            write_psm(&mut out);
        }
        // 大帧拆多个 PES,首个 PES 带 PTS。
        let mut first = true;
        for chunk in au.chunks(self.pes_max.max(1)) {
            write_pes(&mut out, chunk, pts, first);
            first = false;
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
    fn pts_编码_5字节() {
        let b = encode_pts(0, 0b0010);
        assert_eq!(b.len(), 5);
        assert_eq!(b[0] >> 4, 0b0010);
    }
}
