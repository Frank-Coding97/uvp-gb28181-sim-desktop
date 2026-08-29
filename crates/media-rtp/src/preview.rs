use std::time::Instant;

/// 从唯一 H.264 采集流切出的完整访问单元。
#[derive(Debug, Clone)]
pub struct CapturedAccessUnit {
    pub generation: u64,
    pub sequence: u64,
    pub data: Vec<u8>,
    pub key_frame: bool,
    pub config_keyframe: bool,
    /// 主采集 stdout 切出完整 AU 的 Unix 毫秒时刻。
    pub captured_at_ms: u64,
    /// 只在 Rust 进程内用于 watchdog，不跨 IPC。
    pub captured_at_mono: Instant,
}

impl CapturedAccessUnit {
    pub fn new(
        generation: u64,
        sequence: u64,
        data: Vec<u8>,
        key_frame: bool,
        captured_at_ms: u64,
    ) -> Self {
        let config_keyframe = h264_nal_types(&data, |nal_type| matches!(nal_type, 5 | 7 | 8));
        let has_idr = h264_nal_types(&data, |nal_type| nal_type == 5);
        let has_sps = h264_nal_types(&data, |nal_type| nal_type == 7);
        let has_pps = h264_nal_types(&data, |nal_type| nal_type == 8);
        Self {
            generation,
            sequence,
            data,
            key_frame,
            config_keyframe: config_keyframe && has_idr && has_sps && has_pps,
            captured_at_ms,
            captured_at_mono: Instant::now(),
        }
    }
}

/// 预览用的独立 JPEG 帧。每帧自包含，前端可安全 latest-wins。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewJpeg {
    pub generation: u64,
    pub sequence: u64,
    pub data: Vec<u8>,
    /// 对应主 H.264 AU 被切出的时刻，而不是 JPEG reader 的读取时刻。
    pub captured_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewPhase {
    Idle,
    WaitingRegistration,
    CaptureStarting,
    H264Ready,
    PreviewStarting,
    WaitingKeyframe,
    Playing,
    Recovering,
    Unavailable,
    Stopping,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PreviewStatus {
    pub generation: u64,
    pub preview_generation: u64,
    pub phase: PreviewPhase,
    pub reason: Option<String>,
    pub h264_input_frames: u64,
    pub jpeg_output_frames: u64,
    pub dropped_frames: u64,
    pub recoveries: u32,
    pub last_input_at_ms: Option<u64>,
    pub last_output_at_ms: Option<u64>,
}

impl PreviewStatus {
    pub fn new(generation: u64, phase: PreviewPhase) -> Self {
        Self {
            generation,
            preview_generation: 1,
            phase,
            reason: None,
            h264_input_frames: 0,
            jpeg_output_frames: 0,
            dropped_frames: 0,
            recoveries: 0,
            last_input_at_ms: None,
            last_output_at_ms: None,
        }
    }
}

/// 预览帧接收端。实现方只保留最新帧即可——旧帧对实时预览没有价值。
pub trait PreviewSink: Send + Sync {
    fn publish(&self, frame: PreviewJpeg);

    fn status(&self, status: PreviewStatus) {
        let _ = status;
    }

    /// 采集结束时通知，让预览侧收尾。
    fn stopped(&self) {}

    /// 预览旁路无法启用（例如它把采集拖垮，为保住推流而降级）。
    ///
    /// 必须让用户看到原因：否则界面只会停在"启动中"，又变成一轮盲猜。
    fn unavailable(&self, reason: String) {
        let _ = reason;
    }
}

/// 遍历 Annex-B NAL；只返回是否至少命中一次 predicate。
fn h264_nal_types(mut data: &[u8], mut predicate: impl FnMut(u8) -> bool) -> bool {
    while let Some((offset, start_len)) = find_start_code(data) {
        data = &data[offset + start_len..];
        if let Some(header) = data.first() {
            if predicate(header & 0x1f) {
                return true;
            }
        }
    }
    false
}

fn find_start_code(data: &[u8]) -> Option<(usize, usize)> {
    for index in 0..data.len().saturating_sub(2) {
        if data[index] == 0 && data[index + 1] == 0 {
            if data[index + 2] == 1 {
                return Some((index, 3));
            }
            if data.get(index + 2) == Some(&0) && data.get(index + 3) == Some(&1) {
                return Some((index, 4));
            }
        }
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 从 MJPEG 字节流里切出完整的 JPEG 帧。
///
/// JPEG 以 SOI(FF D8 FF) 开头、EOI(FF D9) 结尾。熵编码数据里的 0xFF 会被转义成
/// FF 00，所以 FF D9 只可能是真正的帧结束标记。不完整的尾部留在缓冲里等后续数据。
pub fn drain_jpeg_frames(buf: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    loop {
        let Some(start) = find(buf, &[0xFF, 0xD8, 0xFF]) else {
            // 没有任何起始标记：留最后两字节，防止 SOI 正好跨在读取边界上。
            let keep = buf.len().min(2);
            buf.drain(..buf.len() - keep);
            break;
        };
        let Some(end_offset) = find(&buf[start + 2..], &[0xFF, 0xD9]) else {
            if start > 0 {
                buf.drain(..start);
            }
            break;
        };
        let end = start + 2 + end_offset + 2;
        frames.push(buf[start..end].to_vec());
        buf.drain(..end);
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nal(kind: u8, payload: u8) -> Vec<u8> {
        vec![0, 0, 0, 1, kind, payload]
    }

    #[test]
    fn captured_access_unit只把_sps_pps_idr_组合识别为配置关键帧() {
        let mut config = nal(7, 1);
        config.extend(nal(8, 2));
        config.extend(nal(5, 3));
        assert!(CapturedAccessUnit::new(7, 1, config, true, 10).config_keyframe);

        assert!(!CapturedAccessUnit::new(7, 2, nal(5, 3), true, 11).config_keyframe);
        assert!(!CapturedAccessUnit::new(7, 3, nal(1, 3), false, 12).config_keyframe);
    }

    #[test]
    fn preview_status保留代际和持久计数() {
        let mut status = PreviewStatus::new(9, PreviewPhase::WaitingKeyframe);
        status.h264_input_frames = 3;
        status.reason = Some("waiting for SPS/PPS/IDR".into());
        assert_eq!(status.generation, 9);
        assert_eq!(status.h264_input_frames, 3);
        assert_eq!(status.phase, PreviewPhase::WaitingKeyframe);
    }

    fn jpeg(marker: u8, payload_len: usize) -> Vec<u8> {
        let mut frame = vec![0xFF, 0xD8, 0xFF, marker];
        frame.extend(std::iter::repeat(0x42).take(payload_len));
        frame.extend_from_slice(&[0xFF, 0xD9]);
        frame
    }

    #[test]
    fn 切出连续的完整帧() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&jpeg(0xE0, 8));
        buf.extend_from_slice(&jpeg(0xE1, 12));

        let frames = drain_jpeg_frames(&mut buf);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], jpeg(0xE0, 8));
        assert_eq!(frames[1], jpeg(0xE1, 12));
        assert!(buf.is_empty());
    }

    #[test]
    fn 不完整的尾部留在缓冲里等后续数据() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&jpeg(0xE0, 4));
        let partial = jpeg(0xE1, 20);
        buf.extend_from_slice(&partial[..10]);

        let frames = drain_jpeg_frames(&mut buf);
        assert_eq!(frames.len(), 1, "只应切出已经完整的那一帧");
        assert_eq!(buf, partial[..10], "残缺尾部必须原样保留");

        // 补齐剩余字节后应能切出第二帧。
        buf.extend_from_slice(&partial[10..]);
        let frames = drain_jpeg_frames(&mut buf);
        assert_eq!(frames, vec![partial]);
        assert!(buf.is_empty());
    }

    #[test]
    fn 丢弃起始标记之前的垃圾字节() {
        let mut buf = vec![0x00, 0x11, 0x22];
        buf.extend_from_slice(&jpeg(0xE0, 4));

        let frames = drain_jpeg_frames(&mut buf);
        assert_eq!(frames, vec![jpeg(0xE0, 4)]);
    }

    #[test]
    fn 无起始标记时不无限堆积缓冲() {
        let mut buf = vec![0x00; 4096];
        drain_jpeg_frames(&mut buf);
        // 只保留可能跨边界的两个字节，避免读取线程把内存吃光。
        assert!(buf.len() <= 2);
    }
}
