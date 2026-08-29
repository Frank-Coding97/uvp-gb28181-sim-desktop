//! RTP / PS 媒体封装与推流引擎。
//!
//! 负责把码流(或伪码流)按 RTP 打包发送给上级平台。压测时按 `active_ratio`
//! 只让部分设备真推流,码率/帧率受控。
//!
//! - `rtp`:RTP 打包、SSRC/时间戳/序号管理、UDP 发送、发送统计
//! - `ps`:PS(Program Stream)封装,复用 H.264
//! - `source`:视频源抽象(空媒体 / 真实文件循环)
//! - `pusher`:把视频源→PS→RTP 按帧率推流的驱动

pub mod preview;
pub mod preview_worker;
pub use preview::{CapturedAccessUnit, PreviewJpeg, PreviewPhase, PreviewSink, PreviewStatus};
pub use preview_worker::{
    preview_worker_args, spawn_preview_worker, PreviewControl, PreviewSendResult,
    PreviewWorkerHandle, PreviewWorkerInput,
};
pub mod ps;
pub mod pusher;
pub mod rtp;
pub mod source;

pub use ps::{AudioCodec, PsMuxer, VideoCodec};
pub use pusher::{contains_codec_config, push_stream, push_stream_controlled, PlaybackControl};
pub use rtp::{build_rtcp_sr, RtpSender, SendStats, CLOCK_HZ, PT_PS};
pub use source::{
    ffmpeg_bin, list_live_sources, prepare_video_source, start_shared_media, FileSource, Frame,
    LightSource, LiveAudioCodec, LiveAudioSource, LiveAvDevice, LiveScreenDevice, LiveSource,
    LiveSourceCatalog, LiveSourceSpec, LiveVideoCodec, LiveVideoProfile, NoneSource, SharedMedia,
    SharedVideoSource, VideoSource,
};

/// RTP 发送模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtpMode {
    /// UDP 数据报,每包一发。
    Udp,
    /// TCP 主动连接,RFC 4571 解帧(2 字节大端长度 + 包体)。
    TcpActive,
    /// TCP 被动监听,等平台连入。
    TcpPassive,
}
