//! 视频源抽象(可插拔)。
//!
//! 对应压测三档媒体强度(docs/10-functional/stress-testing.md#2):
//! - `NoneSource`:空媒体(不产帧)—— A 档
//! - `LightSource`:合成低码率伪包 —— B 档(测 RTP 通道/带宽,内容不要求可解码)
//! - `FileSource`:H.264 文件循环 —— C 档 / 单设备联调

use common::{Error, Result};

/// 一帧编码数据(H.264 Annex B 访问单元,含起始码)。
#[derive(Debug, Clone)]
pub struct Frame {
    /// 帧字节(Annex B)。
    pub data: Vec<u8>,
    /// 是否关键帧(含 SPS/PPS/IDR)。
    pub key_frame: bool,
}

/// 视频源:按需产出下一帧。实现方负责循环/结束语义。
pub trait VideoSource: Send {
    /// 取下一帧;返回 `None` 表示无更多帧(有限源)。循环源永不返回 None。
    fn next_frame(&mut self) -> Option<Frame>;
}

/// 空媒体源:永不产帧(A 档,只维持信令)。
pub struct NoneSource;

impl VideoSource for NoneSource {
    fn next_frame(&mut self) -> Option<Frame> {
        None
    }
}

/// 轻量伪包源(B 档):按目标码率合成固定大小伪 NAL 帧,无限循环。
///
/// 用途:压测平台 RTP 端口/会话/带宽调度,不追求可解码内容。
/// 每帧字节数 = bitrate_kbps*1000/8/fps(下限 64);约每秒一个关键帧标记。
pub struct LightSource {
    frame_bytes: usize,
    keyframe_interval: u32,
    counter: u32,
}

impl LightSource {
    /// 按目标码率(kbps)与帧率构造。
    pub fn new(bitrate_kbps: u32, fps: u32) -> Self {
        let fps = fps.max(1);
        let frame_bytes = ((bitrate_kbps as usize * 1000) / 8 / fps as usize).max(64);
        LightSource {
            frame_bytes,
            keyframe_interval: fps,
            counter: 0,
        }
    }
}

impl VideoSource for LightSource {
    fn next_frame(&mut self) -> Option<Frame> {
        let key_frame = self.counter % self.keyframe_interval == 0;
        self.counter = self.counter.wrapping_add(1);
        // 伪 NAL:Annex B 起始码 + NAL 头(IDR type5 / slice type1)+ 填充占位。
        let nal_type: u8 = if key_frame { 0x65 } else { 0x61 };
        let mut data = Vec::with_capacity(self.frame_bytes + 5);
        data.extend_from_slice(&[0, 0, 0, 1, nal_type]);
        data.resize(self.frame_bytes + 5, 0xAB);
        Some(Frame { data, key_frame })
    }
}

/// H.264 Annex B 文件循环源。加载时按起始码切成访问单元,循环产出。
pub struct FileSource {
    frames: Vec<Frame>,
    cursor: usize,
}

impl FileSource {
    /// 从 H.264 Annex B 裸流文件加载并切帧。
    pub fn from_path(path: &str) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(Error::Io)?;
        Self::from_bytes(&bytes)
    }

    /// 从内存中的 Annex B 字节切帧。
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let nals = split_annex_b(bytes);
        if nals.is_empty() {
            return Err(Error::Media("H.264 流为空或无起始码".into()));
        }
        // 简化的成帧:把连续 NAL 聚成访问单元 —— 遇到 VCL 帧(slice)边界切帧。
        // 关键帧判定:该帧含 IDR(type 5)或 SPS(7)/PPS(8)。
        let frames = group_into_frames(nals);
        Ok(FileSource { frames, cursor: 0 })
    }

    /// 帧总数。
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// 是否无帧。
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

impl VideoSource for FileSource {
    fn next_frame(&mut self) -> Option<Frame> {
        if self.frames.is_empty() {
            return None;
        }
        let f = self.frames[self.cursor].clone();
        self.cursor = (self.cursor + 1) % self.frames.len(); // 循环
        Some(f)
    }
}

/// 一个带起始码位置的 NAL 单元(含起始码)。
struct Nal<'a> {
    bytes: &'a [u8],
    nal_type: u8,
}

/// 按 Annex B 起始码(00 00 01 或 00 00 00 01)切分 NAL,保留起始码。
fn split_annex_b(data: &[u8]) -> Vec<Nal<'_>> {
    let mut positions = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            positions.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut nals = Vec::new();
    for idx in 0..positions.len() {
        let start = positions[idx];
        let end = if idx + 1 < positions.len() {
            positions[idx + 1]
        } else {
            data.len()
        };
        let bytes = &data[start..end];
        // 起始码后第一个字节是 NAL 头,type = 低 5 位。
        let nal_type = bytes.get(3).map(|b| b & 0x1f).unwrap_or(0);
        nals.push(Nal { bytes, nal_type });
    }
    nals
}

/// 把 NAL 聚成访问单元(帧)。遇到新的 VCL slice(type 1/5)作为帧起点。
/// SPS(7)/PPS(8)/IDR(5) 出现的帧标记为关键帧。
fn group_into_frames(nals: Vec<Nal<'_>>) -> Vec<Frame> {
    let mut frames = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut cur_key = false;
    let mut has_vcl = false;

    for nal in nals {
        let t = nal.nal_type;
        let is_vcl = t == 1 || t == 5;
        // 已有一个含 VCL 的帧,又来新 VCL → 切帧。
        if is_vcl && has_vcl {
            frames.push(Frame {
                data: std::mem::take(&mut cur),
                key_frame: cur_key,
            });
            cur_key = false;
            has_vcl = false;
        }
        if t == 5 || t == 7 || t == 8 {
            cur_key = true;
        }
        if is_vcl {
            has_vcl = true;
        }
        cur.extend_from_slice(nal.bytes);
    }
    if !cur.is_empty() {
        frames.push(Frame {
            data: cur,
            key_frame: cur_key,
        });
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个含 SPS/PPS/IDR + 非关键帧的 Annex B 流。
    fn sample_h264() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&[0, 0, 0, 1, 0x67, 0xAA]); // SPS(7)
        v.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xBB]); // PPS(8)
        v.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x11]); // IDR slice(5)
        v.extend_from_slice(&[0, 0, 0, 1, 0x61, 0x22]); // 非 IDR slice(1)
        v
    }

    #[test]
    fn 切帧_关键帧识别() {
        let src = FileSource::from_bytes(&sample_h264()).unwrap();
        assert_eq!(src.len(), 2); // IDR 帧(含SPS/PPS/IDR)+ 非关键帧
        let mut s = src;
        let f1 = s.next_frame().unwrap();
        assert!(f1.key_frame);
        let f2 = s.next_frame().unwrap();
        assert!(!f2.key_frame);
    }

    #[test]
    fn 文件源循环() {
        let mut s = FileSource::from_bytes(&sample_h264()).unwrap();
        let total = s.len();
        // 取 2*total+1 帧不 panic,且回到起点。
        for _ in 0..(total * 2 + 1) {
            assert!(s.next_frame().is_some());
        }
    }

    #[test]
    fn 空源不产帧() {
        let mut n = NoneSource;
        assert!(n.next_frame().is_none());
    }

    #[test]
    fn 空流报错() {
        assert!(FileSource::from_bytes(&[0xFF, 0xFF]).is_err());
    }

    #[test]
    fn 轻量源按码率产帧() {
        // 200kbps @ 25fps → 每帧 200*1000/8/25 = 1000 字节。
        let mut s = LightSource::new(200, 25);
        let f0 = s.next_frame().unwrap();
        assert!(f0.key_frame); // 首帧为关键帧
        assert_eq!(f0.data.len(), 1000 + 5); // 起始码+NAL头+填充
        assert_eq!(&f0.data[..5], &[0, 0, 0, 1, 0x65]); // IDR
        let f1 = s.next_frame().unwrap();
        assert!(!f1.key_frame); // 第二帧非关键帧
        assert_eq!(&f1.data[..5], &[0, 0, 0, 1, 0x61]); // slice
    }

    #[test]
    fn 轻量源最小帧下限() {
        let mut s = LightSource::new(1, 25); // 极低码率 → 命中 64 字节下限
        let f = s.next_frame().unwrap();
        assert_eq!(f.data.len(), 64 + 5);
    }
}
