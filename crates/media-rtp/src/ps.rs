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
fn encode_pts(ts: u64, marker_bits: u8) -> [u8; 5] {
    let ts = ts & 0x1_FFFF_FFFF;
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
    // 字节 10-12:program_mux_rate(22) + marker + marker。
    // 单位是 50 字节/秒;给一个覆盖常见码率的值(6106*50*8 ≈ 2.4 Mbps),
    // 宽容解析器不校验,严格解析器至少不会算出荒谬的比特率。
    let mux_rate: u32 = 6106;
    out.push((mux_rate >> 14) as u8);
    out.push((mux_rate >> 6) as u8);
    out.push((((mux_rate & 0x3F) as u8) << 2) | 0x03);
    // 字节 13:reserved(5,置1) + pack_stuffing_length(3=0)
    out.push(0xF8);
}

/// 写入 System Header(00 00 01 BB ...),仅关键帧带。
///
/// `has_audio` 决定是否声明音频流。之前**恒声明一路音频**,纯视频流里就出现了
/// "System Header 说有音频、PSM 里只有视频"的自相矛盾:宽容解析器无感,
/// 严格的会告警。
fn write_system_header(out: &mut Vec<u8>, has_audio: bool) {
    out.extend_from_slice(&[0x00, 0x00, 0x01, 0xBB]);
    let header_len: u16 = if has_audio { 12 } else { 9 };
    out.extend_from_slice(&header_len.to_be_bytes());
    out.extend_from_slice(&[0x80, 0x1D, 0x81]); // rate_bound + marker
                                                // audio_bound(6) / video_bound(5) 等:有音频时声明 1 路音频。
    if has_audio {
        out.extend_from_slice(&[0x04, 0xE1, 0x7F]);
    } else {
        out.extend_from_slice(&[0x00, 0xE1, 0x7F]);
    }
    out.extend_from_slice(&[0xE0, 0xE0, 0xE8]); // video stream 描述
    if has_audio {
        out.extend_from_slice(&[0xC0, 0xC0, 0x20]); // audio stream 描述
    }
}

/// MPEG-2 CRC-32(ISO/IEC 13818-1 附录 A):多项式 0x04C11DB7,初值 0xFFFFFFFF,
/// 不反转输入输出、无最终异或。
///
/// PSM 末尾的 CRC 之前是 4 字节 0 占位。WVP/ZLMediaKit 不校验,但部分硬 NVR /
/// 国标网关会因此丢包 —— 属于"能跑但不合规"的典型。
fn mpeg2_crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= (byte as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
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
    /// G.711 A-law / PCMA,stream_type 0x90。
    #[default]
    G711A,
    /// G.711 μ-law / PCMU,stream_type 0x91。
    G711U,
    /// AAC-LC with ADTS headers,stream_type 0x0F。
    Aac,
    /// Opus uses private-data stream_type 0x06 with an `Opus` registration descriptor.
    /// This is an extension rather than a codec mandated by GB/T 28181.
    Opus,
}

impl AudioCodec {
    /// PSM 中的 stream_type。
    pub const fn stream_type(self) -> u8 {
        match self {
            Self::G711A => 0x90,
            Self::G711U => 0x91,
            Self::Aac => 0x0F,
            Self::Opus => 0x06,
        }
    }

    /// 每个编码访问单元在 90 kHz PS 时钟中的持续时间。
    pub const fn frame_duration_90k(self) -> u32 {
        match self {
            Self::Aac => 1_920,                              // 1024 samples / 48 kHz
            Self::G711A | Self::G711U | Self::Opus => 1_800, // 20 ms
        }
    }

    /// PSM elementary_stream_info descriptor。Opus 通过注册描述符标识私有流。
    const fn descriptor(self) -> &'static [u8] {
        match self {
            Self::Opus => &[0x05, 0x04, b'O', b'p', b'u', b's'],
            _ => &[],
        }
    }
}

/// 写入 Program Stream Map(00 00 01 BC),声明单条视频 ES(按 `video` 编码)。
///
/// program_stream_map_length = 其后到末尾(含 CRC32)的字节数。
fn write_psm(out: &mut Vec<u8>, video: VideoCodec) {
    write_psm_streams(out, &[(video.stream_type(), STREAM_ID_VIDEO, &[])]);
}

/// 写入含音频的 PSM:声明视频(按 `video`)+ 音频(按 `audio`)两条 ES。
fn write_psm_av(out: &mut Vec<u8>, video: VideoCodec, audio: AudioCodec) {
    write_psm_streams(
        out,
        &[
            (video.stream_type(), STREAM_ID_VIDEO, &[]),
            (audio.stream_type(), STREAM_ID_AUDIO, audio.descriptor()),
        ],
    );
}

/// 写入 PSM(00 00 01 BC),声明给定的若干条 ES,并附**真实的 MPEG-2 CRC32**。
fn write_psm_streams(out: &mut Vec<u8>, streams: &[(u8, u8, &[u8])]) {
    let elementary_map_len: usize = streams.iter().map(|(_, _, d)| 4 + d.len()).sum();
    // psm_len 覆盖 marker/version 之后到 CRC32 结束的全部字节。
    let psm_len = 2 + 2 + 2 + elementary_map_len + 4;

    // CRC 覆盖从 packet_start_code_prefix 到 CRC 之前的全部字节。
    let mut framed = Vec::with_capacity(6 + psm_len);
    framed.extend_from_slice(&[0x00, 0x00, 0x01, 0xBC]);
    framed.extend_from_slice(&(psm_len as u16).to_be_bytes());
    framed.extend_from_slice(&[0xE1, 0xFF]); // current_next=1 + version + marker
    framed.extend_from_slice(&[0x00, 0x00]); // program_stream_info_length = 0
    framed.extend_from_slice(&(elementary_map_len as u16).to_be_bytes());
    for (stream_type, es_id, descriptor) in streams {
        framed.push(*stream_type);
        framed.push(*es_id);
        framed.extend_from_slice(&(descriptor.len() as u16).to_be_bytes());
        framed.extend_from_slice(descriptor);
    }
    let crc = mpeg2_crc32(&framed);
    framed.extend_from_slice(&crc.to_be_bytes());
    out.extend_from_slice(&framed);
}

/// 写入一个视频 PES 包(stream_id 0xE0),承载 `data`(可能是一帧的一部分)。
fn write_pes(out: &mut Vec<u8>, data: &[u8], pts: u64, with_pts: bool) {
    write_pes_stream(out, STREAM_ID_VIDEO, data, pts, with_pts);
}

/// 写入一个 PES 包,指定 stream_id(视频 0xE0 / 音频 0xC0)。
fn write_pes_stream(out: &mut Vec<u8>, stream_id: u8, data: &[u8], pts: u64, with_pts: bool) {
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

/// SCR 相对 PTS 的提前量(90kHz 单位,即 100ms)。
///
/// 规范上 SCR 应略早于该 pack 内最早的 PTS,给解码器留缓冲余量。
/// 之前 SCR 直接等于 PTS,在严格的 T-STD 模型下相当于"刚好迟到"。
const SCR_LEAD_90K: u64 = 9_000;

/// 33 位 PS 时钟掩码。
const PS_CLOCK_MASK: u64 = 0x1_FFFF_FFFF;

/// PS 封装器:维护 90kHz 时间戳,把视频访问单元封成 PS 字节流。
///
/// 有状态:缓存最近看到的参数集(H.264 SPS/PPS、H.265 VPS/SPS/PPS),在关键帧
/// 缺少参数集时无条件重插。解码器容忍重复参数集,但缺参数集会导致"平台永远出不了图",
/// 而参数集是否随 IDR 出现取决于编码器配置(改个 `-bsf:v` 就可能丢),
/// 由封装层兜底才可靠。
pub struct PsMuxer {
    /// 单个 PES 最大负载(过大时拆多个 PES)。
    pes_max: usize,
    /// 视频编码(决定 PSM stream_type)。
    video: VideoCodec,
    /// 音频编码(决定 PSM stream_type)。
    audio: AudioCodec,
    /// 即使当前视频帧暂时没有音频包,关键帧 PSM 也声明音频 ES。
    declare_audio: bool,
    /// 缓存的参数集(NAL 类型 → 含起始码的整段),关键帧兜底重插用。
    cached_parameter_sets: Vec<(u8, Vec<u8>)>,
    /// 参数集重插时的临时拼装缓冲,避免每个关键帧都新分配。
    scratch_au: Vec<u8>,
}

impl Default for PsMuxer {
    fn default() -> Self {
        PsMuxer {
            pes_max: 60_000,
            video: VideoCodec::default(),
            audio: AudioCodec::default(),
            declare_audio: false,
            cached_parameter_sets: Vec::new(),
            scratch_au: Vec::new(),
        }
    }
}

/// 遍历 Annex B 码流里的 NAL 单元,回调 (去起始码的载荷, 含起始码的整段)。
fn for_each_nal(au: &[u8], mut f: impl FnMut(&[u8], &[u8])) {
    let mut starts: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i + 3 <= au.len() {
        if au[i] == 0 && au[i + 1] == 0 {
            if au[i + 2] == 1 {
                starts.push((i, 3));
                i += 3;
                continue;
            }
            if i + 4 <= au.len() && au[i + 2] == 0 && au[i + 3] == 1 {
                starts.push((i, 4));
                i += 4;
                continue;
            }
        }
        i += 1;
    }
    for (idx, &(pos, sc_len)) in starts.iter().enumerate() {
        let end = starts.get(idx + 1).map(|(p, _)| *p).unwrap_or(au.len());
        let payload = &au[pos + sc_len..end];
        if !payload.is_empty() {
            f(payload, &au[pos..end]);
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

    /// 指定视频编码及是否声明音频 ES。
    pub fn with_video_and_audio(video: VideoCodec, has_audio: bool) -> Self {
        PsMuxer {
            video,
            declare_audio: has_audio,
            ..Self::default()
        }
    }

    /// 指定视频与音频编码新建。
    pub fn with_codecs(video: VideoCodec, audio: AudioCodec) -> Self {
        PsMuxer {
            video,
            audio,
            declare_audio: true,
            ..Self::default()
        }
    }

    /// 扫描 AU:更新参数集缓存,返回"本 AU 里已出现的参数集 NAL 类型"。
    fn scan_parameter_sets(&mut self, au: &[u8]) -> Vec<u8> {
        let mut present = Vec::new();
        let mut found: Vec<(u8, Vec<u8>)> = Vec::new();
        let video = self.video;
        for_each_nal(au, |payload, whole| {
            let nal_type = match video {
                // H.264 取低 5 位;H.265 取 (byte >> 1) & 0x3F。
                VideoCodec::H264 => payload[0] & 0x1F,
                VideoCodec::H265 => (payload[0] >> 1) & 0x3F,
            };
            let is_ps = match video {
                // 7=SPS 8=PPS
                VideoCodec::H264 => matches!(nal_type, 7 | 8),
                // 32=VPS 33=SPS 34=PPS
                VideoCodec::H265 => matches!(nal_type, 32..=34),
            };
            if is_ps {
                present.push(nal_type);
                found.push((nal_type, whole.to_vec()));
            }
        });
        for (nal_type, bytes) in found {
            match self
                .cached_parameter_sets
                .iter_mut()
                .find(|(t, _)| *t == nal_type)
            {
                Some(slot) => slot.1 = bytes,
                None => self.cached_parameter_sets.push((nal_type, bytes)),
            }
        }
        present
    }

    /// 关键帧兜底:若 AU 缺少已缓存的参数集,把"缺失参数集 + 原 AU"拼进 scratch。
    /// 返回是否应改用 scratch(false 表示零拷贝直接用原切片)。
    fn with_parameter_sets(&mut self, au: &[u8], key_frame: bool) -> bool {
        let present = self.scan_parameter_sets(au);
        if !key_frame || self.cached_parameter_sets.is_empty() {
            return false;
        }
        let mut missing: Vec<usize> = self
            .cached_parameter_sets
            .iter()
            .enumerate()
            .filter(|(_, (t, _))| !present.contains(t))
            .map(|(i, _)| i)
            .collect();
        if missing.is_empty() {
            return false;
        }
        // 参数集按 NAL 类型升序(H.264: SPS→PPS;H.265: VPS→SPS→PPS)。
        missing.sort_by_key(|i| self.cached_parameter_sets[*i].0);
        let mut scratch = std::mem::take(&mut self.scratch_au);
        scratch.clear();
        for i in missing {
            let bytes = &self.cached_parameter_sets[i].1;
            scratch.extend_from_slice(bytes);
        }
        scratch.extend_from_slice(au);
        self.scratch_au = scratch;
        true
    }

    /// 封装一帧 H.264 访问单元(Annex B,含起始码)。
    /// `key_frame` 为真时前置 System Header + PSM(便于平台快速出图)。
    /// 返回可直接交给 RTP 分片发送的 PS 字节。
    pub fn mux_frame(&mut self, au: &[u8], pts: u64, key_frame: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(au.len() + 128);
        self.mux_into(&mut out, au, pts, key_frame, &[], pts);
        out
    }

    /// 封装一帧视频及同一时间窗内的完整编码音频访问单元。
    /// 每个音频元素独立写入一个 PES；关键帧处 PSM 声明音视频两条 ES。
    pub fn mux_frame_av(
        &mut self,
        au: &[u8],
        pts: u64,
        key_frame: bool,
        audio: &[Vec<u8>],
    ) -> Vec<u8> {
        self.mux_frame_av_with_audio_pts(au, pts, key_frame, audio, pts)
    }

    /// 封装视频和音频,并允许音频从独立的 90kHz 时钟起点开始。
    /// 音频访问单元的 PTS 按编码帧时长递增：G.711/Opus 为 20ms，AAC-LC 为 1024/48kHz。
    pub fn mux_frame_av_with_audio_pts(
        &mut self,
        au: &[u8],
        pts: u64,
        key_frame: bool,
        audio: &[Vec<u8>],
        audio_start_pts: u64,
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(au.len() + 128);
        self.mux_into(&mut out, au, pts, key_frame, audio, audio_start_pts);
        out
    }

    /// 封装到调用方提供的缓冲(**复用同一块内存**)。
    ///
    /// 推流热路径每帧都调用:原来每帧 `Vec::with_capacity` + 全帧 memcpy,
    /// 1080p 关键帧就是一次 200KB 的分配加拷贝。传入复用缓冲后稳态零分配。
    pub fn mux_into(
        &mut self,
        out: &mut Vec<u8>,
        au: &[u8],
        pts: u64,
        key_frame: bool,
        audio: &[Vec<u8>],
        audio_start_pts: u64,
    ) {
        out.clear();
        let has_audio = self.declare_audio || !audio.is_empty();
        // SCR 略早于 PTS,给解码器留缓冲余量(33 位时钟上做环回减法)。
        let scr = pts.wrapping_sub(SCR_LEAD_90K) & PS_CLOCK_MASK;
        write_pack_header(out, scr);
        if key_frame {
            write_system_header(out, has_audio);
            if has_audio {
                write_psm_av(out, self.video, self.audio);
            } else {
                write_psm(out, self.video);
            }
        }
        // 关键帧缺参数集时用缓存兜底重插。
        let use_scratch = self.with_parameter_sets(au, key_frame);
        let pes_max = self.pes_max.max(1);
        {
            let effective: &[u8] = if use_scratch { &self.scratch_au } else { au };
            // 视频 PES(大帧拆分,首个带 PTS)。
            let mut first = true;
            for chunk in effective.chunks(pes_max) {
                write_pes(out, chunk, pts, first);
                first = false;
            }
        }
        // 每个编码访问单元单独封装音频 PES，并按真实编码帧时长递增 PTS。
        let frame_duration = self.audio.frame_duration_90k();
        for (index, packet) in audio.iter().enumerate() {
            let audio_pts = audio_start_pts
                .wrapping_add((index as u64).wrapping_mul(u64::from(frame_duration)));
            write_pes_stream(out, STREAM_ID_AUDIO, packet, audio_pts, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 关键帧含_pack_system_psm() {
        let mut mux = PsMuxer::new();
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
        let mut mux = PsMuxer::new();
        let ps = mux.mux_frame(&[0x00, 0x00, 0x01, 0x61], 1800, false);
        assert_eq!(&ps[0..4], &[0x00, 0x00, 0x01, 0xBA]);
        assert!(!ps.windows(4).any(|w| w == [0x00, 0x00, 0x01, 0xBB])); // 无 System Header
        assert!(ps.windows(4).any(|w| w == [0x00, 0x00, 0x01, 0xE0])); // 有 PES
    }

    #[test]
    fn 音视频复合流含音频pes与音频psm() {
        let mut mux = PsMuxer::new();
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
        let mut mux = PsMuxer::new();
        let ps = mux.mux_frame_av(&[0x00, 0x00, 0x01, 0x61], 1800, false, &[]);
        assert!(!ps.windows(4).any(|w| w == [0x00, 0x00, 0x01, 0xC0])); // 无音频 PES
        assert!(ps.windows(4).any(|w| w == [0x00, 0x00, 0x01, 0xE0])); // 有视频 PES
    }

    #[test]
    fn h265_psm_stream_type_0x24() {
        let mut mux = PsMuxer::with_video(VideoCodec::H265);
        let ps = mux.mux_frame(&[0x00, 0x00, 0x00, 0x01, 0x40, 0x01], 900, true); // 伪 VPS
                                                                                  // PSM 视频 ES 映射 stream_type=0x24, elementary_stream_id=0xE0。
        assert!(ps.windows(2).any(|w| w == [0x24, 0xE0]));
        assert!(!ps.windows(2).any(|w| w == [0x1B, 0xE0])); // 不再是 H.264
    }

    #[test]
    fn aac_psm_stream_type_0x0f() {
        let mut mux = PsMuxer::with_codecs(VideoCodec::H264, AudioCodec::Aac);
        let au = &[0x00, 0x00, 0x00, 0x01, 0x65, 0x11];
        let audio = vec![vec![0xFFu8; 32]]; // 伪 AAC ADTS
        let ps = mux.mux_frame_av(au, 900, true, &audio);
        // 音频 ES 映射 stream_type=0x0F, elementary_stream_id=0xC0。
        assert!(ps.windows(2).any(|w| w == [0x0F, 0xC0]));
    }

    /// 从 PS 字节里取出 PSM 段(从 00 00 01 BC 起,长度由其后 2 字节给出)。
    fn extract_psm(ps: &[u8]) -> &[u8] {
        let pos = ps
            .windows(4)
            .position(|w| w == [0x00, 0x00, 0x01, 0xBC])
            .expect("应含 PSM");
        let len = u16::from_be_bytes([ps[pos + 4], ps[pos + 5]]) as usize;
        &ps[pos..pos + 6 + len]
    }

    #[test]
    fn psm_带真实_mpeg2_crc32() {
        let mut mux = PsMuxer::new();
        let ps = mux.mux_frame(&[0x00, 0x00, 0x00, 0x01, 0x65, 0x11], 900, true);
        let psm = extract_psm(&ps);
        // CRC 不再是 4 字节 0 占位。
        let crc_bytes = &psm[psm.len() - 4..];
        assert_ne!(crc_bytes, &[0, 0, 0, 0], "PSM CRC32 不应是占位 0");
        // 自校验:对"整段 PSM 去掉末尾 CRC"重算,应与写入值一致。
        let expect = mpeg2_crc32(&psm[..psm.len() - 4]);
        assert_eq!(crc_bytes, &expect.to_be_bytes());
    }

    #[test]
    fn crc32_对拍已知向量() {
        // MPEG-2 CRC-32("123456789") 的标准值。
        assert_eq!(mpeg2_crc32(b"123456789"), 0x0376_E6E7);
        assert_eq!(mpeg2_crc32(&[]), 0xFFFF_FFFF);
    }

    #[test]
    fn scr_早于_pts_一百毫秒() {
        let mut mux = PsMuxer::new();
        let pts = 900_000u64;
        let ps = mux.mux_frame(&[0x00, 0x00, 0x00, 0x01, 0x61], pts, false);
        // 从 pack header 反解 SCR 的 33 位值。
        let b = &ps[4..10];
        let s32_30 = ((b[0] >> 3) & 0x07) as u64;
        let s29_15 = (((b[0] & 0x03) as u64) << 13) | ((b[1] as u64) << 5) | ((b[2] >> 3) as u64);
        let s14_0 = (((b[2] & 0x03) as u64) << 13) | ((b[3] as u64) << 5) | ((b[4] >> 3) as u64);
        let scr = (s32_30 << 30) | (s29_15 << 15) | s14_0;
        assert_eq!(scr, pts - SCR_LEAD_90K, "SCR 应比 PTS 早 100ms");
    }

    #[test]
    fn 纯视频流的_system_header_不声明音频() {
        let mut mux = PsMuxer::new();
        let ps = mux.mux_frame(&[0x00, 0x00, 0x00, 0x01, 0x65, 0x11], 900, true);
        // System Header 里不应出现音频流描述(C0 C0 20)。
        assert!(
            !ps.windows(3).any(|w| w == [0xC0, 0xC0, 0x20]),
            "纯视频流不该声明音频,否则与 PSM 自相矛盾"
        );

        // 有音频时应声明。
        let mut mux_av = PsMuxer::with_codecs(VideoCodec::H264, AudioCodec::G711A);
        let ps_av = mux_av.mux_frame_av(
            &[0x00, 0x00, 0x00, 0x01, 0x65, 0x11],
            900,
            true,
            &[vec![0xD5; 160]],
        );
        assert!(ps_av.windows(3).any(|w| w == [0xC0, 0xC0, 0x20]));
    }

    #[test]
    fn 关键帧缺参数集时用缓存兜底重插() {
        let mut mux = PsMuxer::new();
        // 第一帧带 SPS(7)/PPS(8) + IDR。
        let mut first = Vec::new();
        first.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0x00]); // SPS
        first.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE]); // PPS
        first.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x11]); // IDR
        let _ = mux.mux_frame(&first, 900, true);

        // 第二个关键帧**只有 IDR**,封装层应把缓存的 SPS/PPS 重插进去。
        let bare_idr = [0u8, 0, 0, 1, 0x65, 0x22];
        let ps = mux.mux_frame(&bare_idr, 1800, true);
        assert!(
            ps.windows(2).any(|w| w == [0x67, 0x42]),
            "缺失的 SPS 应被重插"
        );
        assert!(
            ps.windows(2).any(|w| w == [0x68, 0xCE]),
            "缺失的 PPS 应被重插"
        );

        // 非关键帧不重插(避免无谓膨胀)。
        let non_key = mux.mux_frame(&[0, 0, 0, 1, 0x61, 0x33], 2700, false);
        assert!(!non_key.windows(2).any(|w| w == [0x67, 0x42]));
    }

    #[test]
    fn h265_参数集兜底按_vps_sps_pps_顺序() {
        let mut mux = PsMuxer::with_video(VideoCodec::H265);
        let mut first = Vec::new();
        first.extend_from_slice(&[0, 0, 0, 1, 0x40, 0x01]); // VPS (32)
        first.extend_from_slice(&[0, 0, 0, 1, 0x42, 0x01]); // SPS (33)
        first.extend_from_slice(&[0, 0, 0, 1, 0x44, 0x01]); // PPS (34)
        first.extend_from_slice(&[0, 0, 0, 1, 0x26, 0x01]); // IDR_W_RADL (19)
        let _ = mux.mux_frame(&first, 900, true);

        let ps = mux.mux_frame(&[0, 0, 0, 1, 0x26, 0x02], 1800, true);
        let vps = ps.windows(2).position(|w| w == [0x40, 0x01]).unwrap();
        let sps = ps.windows(2).position(|w| w == [0x42, 0x01]).unwrap();
        let pps = ps.windows(2).position(|w| w == [0x44, 0x01]).unwrap();
        assert!(vps < sps && sps < pps, "参数集应按 VPS→SPS→PPS 顺序重插");
    }

    #[test]
    fn mux_into_复用缓冲不改变输出() {
        let mut a = PsMuxer::new();
        let mut b = PsMuxer::new();
        let au = [0u8, 0, 0, 1, 0x65, 0x11];
        let expect = a.mux_frame(&au, 900, true);

        let mut buf = Vec::new();
        b.mux_into(&mut buf, &au, 900, true, &[], 900);
        assert_eq!(buf, expect);
        // 第二次复用同一 buffer 应先清空,不出现残留。
        b.mux_into(&mut buf, &au, 900, true, &[], 900);
        assert_eq!(buf, expect);
    }

    #[test]
    fn pts_编码_5字节() {
        let b = encode_pts(0, 0b0010);
        assert_eq!(b.len(), 5);
        assert_eq!(b[0] >> 4, 0b0010);
    }
}
