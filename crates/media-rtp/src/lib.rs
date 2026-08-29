//! RTP / PS 媒体封装与推流引擎。
//!
//! 负责把码流(或伪码流)按 RTP 打包发送给上级平台。压测时按 `active_ratio`
//! 只让部分设备真推流,码率/帧率受控。
//!
//! - `rtp`:RTP 打包、SSRC/时间戳/序号管理、UDP 发送、发送统计
//! - `ps`:PS(Program Stream)封装,复用 H.264
//! - `source`:视频源抽象(空媒体 / 真实文件循环)
//! - `pusher`:把视频源→PS→RTP 按帧率推流的驱动
//! - `receiver`:RTP 音频**接收**(语音广播/对讲的下行方向)
//! - `rtcp`:RTCP 复合包解析与接收报告(RR)反馈统计

pub mod ps;
pub mod pusher;
pub mod receiver;
pub mod rtcp;
pub mod rtp;
pub mod source;

pub use ps::{AudioCodec, PsMuxer, VideoCodec};
pub use pusher::{
    push_audio_stream, push_stream, push_stream_controlled, push_stream_prepared, AudioPushSummary,
    PlaybackControl, SessionTelemetry, AUDIO_CLOCK_HZ, G711_PACKET_BYTES_20MS,
};
pub use receiver::{AudioReceiver, RecvStats, RecvStatsSnapshot, PT_PCMA, PT_PCMU};
pub use rtcp::{
    build_rtcp_rr, looks_like_rtcp, parse_compound, ReceptionReport, RtcpFeedback,
    RtcpFeedbackSnapshot, RtcpPacket,
};
pub use rtp::{build_rtcp_sr, PreparedMedia, RtpSender, SendStats, CLOCK_HZ, PT_PS};
pub use source::{
    list_live_sources, prepare_video_source, FileSource, Frame, LightSource, LiveAudioCodec,
    LiveAudioSource, LiveAvDevice, LiveScreenDevice, LiveSource, LiveSourceCatalog, LiveSourceSpec,
    LiveVideoCodec, LiveVideoProfile, NoneSource, VideoSource,
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
