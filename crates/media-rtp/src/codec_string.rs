//! 由 Annex B 参数集生成 WebCodecs / RFC 6381 codec 字符串。
//!
//! 前端 `VideoDecoder.configure({ codec })` 需要 `avc1.PPCCLL` 或
//! `hvc1.<profile>.<compat>.<tier><level>.<constraints>`。这里在 Rust 侧解析
//! SPS,前端不再碰码流细节。
//!
//! 参考:RFC 6381 §3.3(avc1)、ISO/IEC 14496-15 Annex E(hvc1)。

use crate::ps::VideoCodec;

/// 从一个(含参数集的)访问单元生成 codec 字符串。找不到 SPS 时返回 `None`。
pub fn codec_string(access_unit: &[u8], codec: VideoCodec) -> Option<String> {
    let sps = find_nal(access_unit, codec, |nal_type| match codec {
        VideoCodec::H264 => nal_type == 7,
        VideoCodec::H265 => nal_type == 33,
    })?;
    match codec {
        VideoCodec::H264 => avc1(sps),
        VideoCodec::H265 => hvc1(sps),
    }
}

/// H.264:SPS 头之后依次是 profile_idc、constraint_set 标志字节、level_idc,
/// 三个字节都在可能出现防竞争字节之前,无需去除 0x03。
fn avc1(sps: &[u8]) -> Option<String> {
    // sps[0] 是 NAL 头。
    let profile = *sps.get(1)?;
    let constraints = *sps.get(2)?;
    let level = *sps.get(3)?;
    Some(format!("avc1.{profile:02X}{constraints:02X}{level:02X}"))
}

/// H.265:解析 SPS 里的 profile_tier_level(去掉防竞争字节后)。
fn hvc1(sps: &[u8]) -> Option<String> {
    // NAL 头 2 字节之后:sps_video_parameter_set_id(4) max_sub_layers_minus1(3)
    // temporal_id_nesting(1) = 1 字节,然后是 profile_tier_level。
    let rbsp = remove_emulation_prevention(sps.get(2..)?);
    let ptl = rbsp.get(1..13)?;
    let profile_space = ptl[0] >> 6;
    let tier = (ptl[0] >> 5) & 1;
    let profile_idc = ptl[0] & 0x1f;
    let compat = u32::from_be_bytes([ptl[1], ptl[2], ptl[3], ptl[4]]);
    let constraints = &ptl[5..11];
    let level_idc = ptl[11];

    let space = match profile_space {
        0 => "",
        1 => "A",
        2 => "B",
        _ => "C",
    };
    // 兼容标志按位反转后以十六进制输出(14496-15 E.3)。
    let compat = compat.reverse_bits();
    let tier = if tier == 0 { 'L' } else { 'H' };
    // 约束字节去掉末尾的 0 字节,每字节一段。
    let last_nonzero = constraints.iter().rposition(|byte| *byte != 0);
    let mut out = format!("hvc1.{space}{profile_idc}.{compat:X}.{tier}{level_idc}");
    match last_nonzero {
        Some(last) => {
            for byte in &constraints[..=last] {
                out.push_str(&format!(".{byte:X}"));
            }
        }
        None => out.push_str(".0"),
    }
    Some(out)
}

fn remove_emulation_prevention(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0;
    for &byte in data {
        if zeros >= 2 && byte == 0x03 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        out.push(byte);
    }
    out
}

/// 找到第一个满足条件的 NAL,返回 NAL 头起、到下一个起始码前的字节。
fn find_nal(data: &[u8], codec: VideoCodec, mut predicate: impl FnMut(u8) -> bool) -> Option<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    for (index, &start) in starts.iter().enumerate() {
        let header = *data.get(start)?;
        let nal_type = match codec {
            VideoCodec::H264 => header & 0x1f,
            VideoCodec::H265 => (header >> 1) & 0x3f,
        };
        if !predicate(nal_type) {
            continue;
        }
        let mut end = starts.get(index + 1).map_or(data.len(), |next| next - 3);
        // 四字节起始码的前导 0 不属于当前 NAL。
        while end > start && data[end - 1] == 0 {
            end -= 1;
        }
        return Some(&data[start..end]);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h264_各档位() {
        // High@3.1(x264/NVENC 常见)
        let au = [0, 0, 0, 1, 0x67, 0x64, 0x00, 0x1F, 0xAC, 0, 0, 0, 1, 0x68, 0xEB, 0, 0, 0, 1, 0x65, 0x88];
        assert_eq!(codec_string(&au, VideoCodec::H264).as_deref(), Some("avc1.64001F"));
        // Constrained Baseline@3.0
        let au = [0, 0, 1, 0x67, 0x42, 0xC0, 0x1E, 0xDA];
        assert_eq!(codec_string(&au, VideoCodec::H264).as_deref(), Some("avc1.42C01E"));
        // Main@4.0
        let au = [0, 0, 0, 1, 0x67, 0x4D, 0x40, 0x28, 0x95];
        assert_eq!(codec_string(&au, VideoCodec::H264).as_deref(), Some("avc1.4D4028"));
    }

    #[test]
    fn h264_没有sps返回none() {
        let au = [0, 0, 0, 1, 0x65, 0x88, 0x84];
        assert_eq!(codec_string(&au, VideoCodec::H264), None);
    }

    /// x265 / NVENC 输出的 1080p Main@L4 SPS 前缀。
    /// profile_space=0 tier=0 profile_idc=1,compat=0x60000000(位 1、2),
    /// progressive+frame_only 约束 = 0x90,level_idc=120(4.0)。
    fn hevc_main_sps() -> Vec<u8> {
        vec![
            0, 0, 0, 1, 0x42, 0x01, // SPS NAL 头(type 33)
            0x01, // vps_id=0, max_sub_layers_minus1=0, nesting=1
            0x01, // space=0 tier=0 profile=1
            0x60, 0x00, 0x00, 0x00, // compat flags
            0x90, 0x00, 0x00, 0x00, 0x00, 0x00, // constraint flags
            0x78, // level_idc=120
            0xA0, 0x03, 0xC0, 0x80,
        ]
    }

    #[test]
    fn h265_main() {
        assert_eq!(
            codec_string(&hevc_main_sps(), VideoCodec::H265).as_deref(),
            Some("hvc1.1.6.L120.90")
        );
    }

    #[test]
    fn h265_ptl内防竞争字节被去除() {
        // compat 字节里出现 00 00 → 编码器会插入 03。
        let sps = vec![
            0, 0, 0, 1, 0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03,
            0x00, 0x00, 0x00, 0x78, 0xA0,
        ];
        assert_eq!(
            codec_string(&sps, VideoCodec::H265).as_deref(),
            Some("hvc1.1.6.L120.90")
        );
    }

    #[test]
    fn h265_main10_高等级() {
        let sps = vec![
            0, 0, 1, 0x42, 0x01, 0x01, 0x22, // tier=1 profile=2
            0x20, 0x00, 0x00, 0x00, // compat 位 2
            0xB0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x99, 0xA0,
        ];
        assert_eq!(
            codec_string(&sps, VideoCodec::H265).as_deref(),
            Some("hvc1.2.4.H153.B0")
        );
    }

    #[test]
    fn 真实编码器输出() {
        let Some(ffmpeg) = crate::source::ffmpeg_bin() else {
            return;
        };
        for (encoder, format, codec, prefix) in [
            ("libx264", "h264", VideoCodec::H264, "avc1."),
            ("libx265", "hevc", VideoCodec::H265, "hvc1.1."),
        ] {
            let output = common::hidden_command(&ffmpeg)
                .args([
                    "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
                    "testsrc2=size=320x240:rate=25", "-frames:v", "1", "-c:v", encoder,
                    "-pix_fmt", "yuv420p", "-x265-params", "log-level=error", "-f", format, "pipe:1",
                ])
                .output()
                .expect("ffmpeg");
            if !output.status.success() {
                continue;
            }
            let value = codec_string(&output.stdout, codec).expect("应解析出 codec 字符串");
            assert!(value.starts_with(prefix), "{encoder}: {value}");
        }
    }
}
