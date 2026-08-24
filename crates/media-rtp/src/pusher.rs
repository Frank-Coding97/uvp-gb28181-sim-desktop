//! 推流驱动:把视频源按帧率封 PS 并经 RTP 发出。
//!
//! 供 gb28181-simulator 在 INVITE 建流后调用。按 `fps` 定时取帧、PS 封装、RTP 分片发送,
//! 时间戳按 90kHz / fps 递增,直到 `stop` 完成。

use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use common::{Error, Result};

use crate::ps::PsMuxer;
use crate::rtp::{RtpSender, CLOCK_HZ};
use crate::source::VideoSource;

static NEXT_PREVIEW_SESSION_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_preview_session_id() -> u64 {
    NEXT_PREVIEW_SESSION_ID.fetch_add(1, Ordering::Relaxed)
}

/// 推流过程中复制给桌面预览的编码帧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewPacket {
    pub data: Vec<u8>,
    pub key_frame: bool,
    pub codec: crate::ps::VideoCodec,
    /// 预览会话代际；新会话不会消费旧会话残留帧。
    pub session_id: u64,
    /// 会话内单调递增的访问单元序号。
    pub sequence: u64,
    /// 编码源帧率；预览通过裸 H.264 管道输入时用它补齐帧时间戳。
    pub fps: u32,
    pub pts_90k: u64,
    pub captured_at_ms: u64,
}

/// 编码访问单元是否携带解码所需的参数集。
///
/// 注册后再接入的预览/RTP 订阅者不能只从 IDR 开始：H.264 还需要 SPS/PPS，
/// H.265 还需要 VPS/SPS/PPS。该函数只识别 Annex B NAL 头，不解析具体参数内容。
pub fn is_config_keyframe(packet: &PreviewPacket) -> bool {
    packet.key_frame && contains_codec_config(&packet.data, packet.codec)
}

/// Annex B 访问单元是否包含当前编码对应的参数集。
pub fn contains_codec_config(data: &[u8], codec: crate::ps::VideoCodec) -> bool {
    let mut h264_sps = false;
    let mut h264_pps = false;
    let mut h265_vps = false;
    let mut h265_sps = false;
    let mut h265_pps = false;
    let mut index = 0;
    while index + 3 <= data.len() {
        if data[index] == 0 && data[index + 1] == 0 && data[index + 2] == 1 {
            let Some(&header) = data.get(index + 3) else {
                break;
            };
            match codec {
                crate::ps::VideoCodec::H264 => match header & 0x1f {
                    7 => h264_sps = true,
                    8 => h264_pps = true,
                    _ => {}
                },
                crate::ps::VideoCodec::H265 => match (header >> 1) & 0x3f {
                    32 => h265_vps = true,
                    33 => h265_sps = true,
                    34 => h265_pps = true,
                    _ => {}
                },
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    match codec {
        crate::ps::VideoCodec::H264 => h264_sps && h264_pps,
        crate::ps::VideoCodec::H265 => h265_vps && h265_sps && h265_pps,
    }
}

/// 可选预览帧接收器。发布必须是非阻塞的，不能影响 RTP 推流。
pub trait PreviewSink: Send + Sync {
    fn publish(&self, packet: PreviewPacket);

    /// 推流会话结束时通知预览订阅者。
    fn stopped(&self) {}
}

/// 回放控制:运行中可调的推流速率倍速 + 暂停开关(平台经 INFO/MANSRTSP 下发)。
///
/// 直播场景保持默认(1.0 倍速、不暂停),行为与原先一致;回放场景平台可下发
/// PLAY(Scale=N)调速、PAUSE 暂停、PLAY 恢复。倍速以千分比存储避免浮点原子。
#[derive(Debug)]
pub struct PlaybackControl {
    /// 播放倍速 ×1000(1000=1.0x;2000=2.0x;500=0.5x)。影响取帧间隔。
    speed_milli: AtomicU32,
    /// 是否暂停(暂停时保持会话但不取帧/发送)。
    paused: AtomicBool,
    /// 待处理的拖动(seek)请求:千分比位置+1(0=无请求,1=0‰,1001=1000‰=末尾)。
    /// 平台经 INFO PLAY Range 下发,推流循环消费后清零。回放拖动(§9.8)用。
    seek_permille_plus1: AtomicU32,
}

impl Default for PlaybackControl {
    fn default() -> Self {
        Self {
            speed_milli: AtomicU32::new(1000),
            paused: AtomicBool::new(false),
            seek_permille_plus1: AtomicU32::new(0),
        }
    }
}

impl PlaybackControl {
    /// 新建默认控制(1.0 倍速、未暂停)。
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 设置倍速(`scale` 为浮点倍数,如 2.0 / 0.5)。非正数忽略。
    pub fn set_speed(&self, scale: f32) {
        if scale > 0.0 {
            self.speed_milli
                .store((scale * 1000.0).round() as u32, Ordering::Relaxed);
        }
    }

    /// 当前倍速(浮点)。
    pub fn speed(&self) -> f32 {
        self.speed_milli.load(Ordering::Relaxed) as f32 / 1000.0
    }

    /// 暂停推流。
    pub fn pause(&self) {
        self.paused.store(true, Ordering::Relaxed);
    }

    /// 恢复推流。
    pub fn resume(&self) {
        self.paused.store(false, Ordering::Relaxed);
    }

    /// 是否暂停中。
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// 请求拖动(seek)到流的千分比位置(0..=1000)。回放 Range 定位用(§9.8)。
    pub fn seek_permille(&self, permille: u32) {
        let p = permille.min(1000);
        self.seek_permille_plus1.store(p + 1, Ordering::Relaxed);
    }

    /// 取出并清除待处理的 seek 请求(千分比 0..=1000);无请求返回 None。
    pub fn take_seek(&self) -> Option<u32> {
        let v = self.seek_permille_plus1.swap(0, Ordering::Relaxed);
        (v > 0).then(|| v - 1)
    }
}

/// 驱动一路推流:`source` 产帧 → PS 封装 → RTP 发送到 `dst`。
///
/// - `ssrc`:RTP SSRC(与 SDP 的 y= 一致)
/// - `fps`:帧率(决定取帧间隔与时间戳步进)
/// - `use_tcp`:true=TCP-ACTIVE(RFC 4571,连平台 TCP-PASSIVE);false=UDP
/// - `stop`:该 future 完成时优雅停流
///
/// 按固定帧率推流(直播场景;等价于 1.0 倍速、不暂停)。返回停流原因(正常停返回 Ok)。
pub async fn push_stream(
    source: Box<dyn VideoSource>,
    dst: SocketAddr,
    ssrc: u32,
    fps: u32,
    use_tcp: bool,
    stop: impl Future<Output = ()>,
) -> Result<()> {
    push_stream_controlled(
        source,
        dst,
        ssrc,
        fps,
        use_tcp,
        PlaybackControl::new(),
        stop,
    )
    .await
}

/// 带回放控制(倍速/暂停)的推流。`control` 可在运行中被平台经 INFO 调整:
/// 倍速影响取帧间隔(2x → 间隔减半),暂停时保持会话但不取帧/发送。
pub async fn push_stream_controlled(
    source: Box<dyn VideoSource>,
    dst: SocketAddr,
    ssrc: u32,
    fps: u32,
    use_tcp: bool,
    control: Arc<PlaybackControl>,
    stop: impl Future<Output = ()>,
) -> Result<()> {
    push_stream_controlled_with_preview(source, dst, ssrc, fps, use_tcp, control, None, stop).await
}

/// 带可选同帧预览发布的推流入口。
pub async fn push_stream_controlled_with_preview(
    mut source: Box<dyn VideoSource>,
    dst: SocketAddr,
    ssrc: u32,
    fps: u32,
    use_tcp: bool,
    control: Arc<PlaybackControl>,
    preview: Option<Arc<dyn PreviewSink>>,
    stop: impl Future<Output = ()>,
) -> Result<()> {
    let mut sender = if use_tcp {
        RtpSender::new_tcp(dst, ssrc).await?
    } else {
        RtpSender::new(dst, ssrc).await?
    };
    tracing::info!(%dst, ssrc = format!("{ssrc:#x}"), use_tcp, fps, live = source.is_live(), "推流开始:已建立发送通道");
    // PSM stream_type 跟随源编码,并按源能力稳定声明音频 ES。
    let audio_codec = source.audio_codec();
    let audio_frame_duration = audio_codec.frame_duration_90k();
    let mux = if source.has_audio() {
        PsMuxer::with_codecs(source.codec(), audio_codec)
    } else {
        PsMuxer::with_video(source.codec())
    };
    let fps = fps.max(1);
    let ts_step = CLOCK_HZ / fps;
    let base_interval_us = 1_000_000f32 / fps as f32;
    let mut timestamp: u32 = 0; // RTP timestamp wraps at 32 bits.
    let mut ps_timestamp: u64 = 0; // MPEG-PS SCR/PTS uses a 33-bit clock.
    const PS_CLOCK_MASK: u64 = 0x1_FFFF_FFFF;
    // 实时音频使用独立 33-bit/90kHz 时钟；每个访问单元按真实帧时长推进。
    let mut audio_timestamp: Option<u64> = None;

    // 诊断计数:发出的帧数 / 关键帧数 / 字节数,退出时汇报,定位"断在第几帧"。
    let mut frames_sent: u64 = 0;
    let session_id = next_preview_session_id();
    let mut keyframes_sent: u64 = 0;
    let mut bytes_sent: u64 = 0;
    let mut first_frame = true;
    let mut empty_polls: u64 = 0;

    tokio::pin!(stop);
    let result = loop {
        // 按当前倍速动态计算取帧间隔(倍速越高间隔越短);暂停时用固定轮询间隔。
        let sleep_us = if control.is_paused() {
            50_000 // 暂停:50ms 轮询等待恢复,不发送。
        } else {
            (base_interval_us / control.speed().max(0.01)).max(1.0) as u64
        };
        let sleep = tokio::time::sleep(std::time::Duration::from_micros(sleep_us));
        tokio::select! {
            _ = &mut stop => {
                tracing::info!(frames_sent, keyframes_sent, bytes_sent, "推流正常停止(收到 stop/BYE)");
                break Ok(());
            }
            _ = sleep => {
                if let Some(error) = source.take_error() {
                    tracing::warn!(%error, frames_sent, "推流:实时采集后端异常停止");
                    break Err(Error::Media(error));
                }
                // 处理待定拖动请求(§9.8 回放 Range 定位):跳转源游标到目标位置。
                if let Some(permille) = control.take_seek() {
                    source.seek(permille);
                }
                if control.is_paused() {
                    continue; // 暂停中,不取帧。
                }
                if let Some(frame) = source.next_frame() {
                    let frame_codec = source.codec();
                    let preview_packet = preview.as_ref().map(|_| PreviewPacket {
                        data: frame.data.clone(),
                        key_frame: frame.key_frame,
                        codec: frame_codec,
                        session_id,
                        sequence: frames_sent + 1,
                        fps,
                        pts_90k: ps_timestamp,
                        captured_at_ms: now_ms(),
                    });
                    // 音视频复合流：取该视频时间窗内的完整编码音频访问单元。
                    let audio = source.next_audio();
                    let audio_start_pts = audio_timestamp.map_or(ps_timestamp, |candidate| {
                        // 33-bit PS 时钟上的模运算：恢复时若音频落后视频超过 100ms，
                        // 重新对齐当前视频 PTS，避免静音/设备切换造成持续过期音频。
                        let lag = ps_timestamp.wrapping_sub(candidate) & PS_CLOCK_MASK;
                        if lag < (1_u64 << 32) && lag > u64::from(CLOCK_HZ / 10) {
                            ps_timestamp
                        } else {
                            candidate
                        }
                    });
                    let ps = mux.mux_frame_av_with_audio_pts(
                        &frame.data,
                        ps_timestamp,
                        frame.key_frame,
                        &audio,
                        audio_start_pts,
                    );
                    if !audio.is_empty() {
                        audio_timestamp = Some(
                            audio_start_pts.wrapping_add(
                                (audio.len() as u64).wrapping_mul(u64::from(audio_frame_duration)),
                            ) & PS_CLOCK_MASK,
                        );
                    }
                    if first_frame {
                        tracing::info!(
                            ps_bytes = ps.len(), key_frame = frame.key_frame, au_bytes = frame.data.len(),
                            "推流:准备发送首帧"
                        );
                    }
                    if let Err(e) = sender.send_frame(&ps, timestamp).await {
                        // 关键诊断:断在发送时,报告已发多少帧——区分"一帧没发出"vs"发了一阵才断"。
                        tracing::warn!(
                            error = %e, frames_sent, keyframes_sent, bytes_sent,
                            first_frame, ps_bytes = ps.len(),
                            "推流:发送失败(TCP 对端关闭/网络错误)"
                        );
                        break Err(e);
                    }
                    if first_frame {
                        tracing::info!("推流:首帧已成功发出");
                        first_frame = false;
                    }
                    if let (Some(sink), Some(packet)) = (&preview, preview_packet) {
                        sink.publish(packet);
                    }
                    frames_sent += 1;
                    if frame.key_frame {
                        keyframes_sent += 1;
                    }
                    bytes_sent += ps.len() as u64;
                    // 每 100 帧汇报一次心跳(约 4s@25fps),确认还在推。
                    if frames_sent % 100 == 0 {
                        tracing::debug!(frames_sent, keyframes_sent, bytes_sent, "推流进行中");
                    }
                    timestamp = timestamp.wrapping_add(ts_step);
                    ps_timestamp =
                        ps_timestamp.wrapping_add(u64::from(ts_step)) & PS_CLOCK_MASK;
                } else if source.is_live() {
                    // 实时源暂时无视频帧时仍消费并丢弃音频，防止有界通道保留旧包、
                    // 恢复画面后永久落后。下一有效帧会把音频时钟重新对齐视频 PTS。
                    let _ = source.next_audio();
                    audio_timestamp = None;
                    // 采集刚启动还没攒够第一帧或发生瞬时空档时不能停流。
                    // 时间戳不推进，下一有效帧续上。
                    empty_polls += 1;
                    if empty_polls % 50 == 0 {
                        tracing::debug!(empty_polls, frames_sent, "推流:实时源暂无帧,等待采集…");
                    }
                    continue;
                } else {
                    // 有限源耗尽,停流。
                    tracing::info!(frames_sent, keyframes_sent, bytes_sent, "推流:有限源耗尽,正常停止");
                    break Ok(());
                }
            }
        }
    };
    result
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{FileSource, Frame};
    use std::sync::Mutex;
    use tokio::net::UdpSocket;

    fn sample_h264() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&[0, 0, 0, 1, 0x67, 0xAA]);
        v.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xBB]);
        v.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x11]);
        v.extend_from_slice(&[0, 0, 0, 1, 0x61, 0x22]);
        v
    }

    struct CollectSink(Mutex<Vec<PreviewPacket>>);
    impl PreviewSink for CollectSink {
        fn publish(&self, packet: PreviewPacket) {
            self.0.lock().unwrap().push(packet);
        }
    }

    #[tokio::test]
    async fn 推流_收到_rtp_包() {
        let rx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let rx_addr = rx.local_addr().unwrap();

        let source = Box::new(FileSource::from_bytes(&sample_h264()).unwrap());
        // 高帧率快速产包;100ms 后停。
        let (tx_stop, rx_stop) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            push_stream(source, rx_addr, 0x2233, 50, false, async {
                let _ = rx_stop.await;
            })
            .await
        });

        // 收到至少一个 RTP 包。
        let mut buf = [0u8; 2048];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv(&mut buf))
            .await
            .expect("超时未收到 RTP")
            .unwrap();
        assert!(n >= 12);
        assert_eq!(buf[0], 0x80); // RTP V=2

        let _ = tx_stop.send(());
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn 推流发送的同一帧会发布给预览接收器() {
        let rx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let rx_addr = rx.local_addr().unwrap();
        let sink = Arc::new(CollectSink(Mutex::new(Vec::new())));
        let sink_read = Arc::clone(&sink);
        let source = Box::new(FileSource::from_bytes(&sample_h264()).unwrap());
        let (tx_stop, rx_stop) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            push_stream_controlled_with_preview(
                source,
                rx_addr,
                0x7788,
                50,
                false,
                PlaybackControl::new(),
                Some(sink_read as Arc<dyn PreviewSink>),
                async {
                    let _ = rx_stop.await;
                },
            )
            .await
        });
        let mut buf = [0u8; 2048];
        tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let _ = tx_stop.send(());
        handle.await.unwrap().unwrap();
        let packets = sink.0.lock().unwrap();
        assert!(!packets.is_empty());
        assert!(packets[0].key_frame);
        assert_eq!(packets[0].fps, 50);
        assert!(packets[0].session_id > 0);
        assert_eq!(packets[0].sequence, 1);
        assert!(packets.windows(2).all(|pair| {
            pair[0].session_id == pair[1].session_id && pair[1].sequence == pair[0].sequence + 1
        }));
        assert!(packets[0].data.windows(2).any(|w| w == [0, 0]));
        assert!(packets[0].data.iter().any(|byte| *byte & 0x1f == 5));
    }

    /// 模拟实时源:前 N 次 next_frame 返回 None(采集尚未就绪),之后持续产帧。
    /// 用于验证 pusher 不会因实时源启动初期暂空而提前停流(live 播不出的根因)。
    struct SlowLive {
        warmup: u32,
        calls: u32,
    }
    impl VideoSource for SlowLive {
        fn is_live(&self) -> bool {
            true
        }
        fn next_frame(&mut self) -> Option<Frame> {
            self.calls += 1;
            if self.calls <= self.warmup {
                None // 采集线程还没攒够第一帧
            } else {
                Some(Frame {
                    data: vec![0, 0, 0, 1, 0x65, 0x11],
                    key_frame: true,
                })
            }
        }
    }

    #[tokio::test]
    async fn 实时源启动初期暂空_不提前停流() {
        let rx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let rx_addr = rx.local_addr().unwrap();

        // 前 5 拍无帧(模拟 ffmpeg 采集预热),之后才产帧。
        let source = Box::new(SlowLive {
            warmup: 5,
            calls: 0,
        });
        let (tx_stop, rx_stop) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            push_stream(source, rx_addr, 0x4455, 50, false, async {
                let _ = rx_stop.await;
            })
            .await
        });

        // 若旧逻辑(None 即 break)仍在,warmup 期第一拍就停流,这里必超时。
        // 修复后应在预热结束后持续收到 RTP 包。
        let mut buf = [0u8; 2048];
        let n = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv(&mut buf))
            .await
            .expect("超时:实时源预热期被误判耗尽而停流")
            .unwrap();
        assert!(n >= 12);
        assert_eq!(buf[0], 0x80);

        let _ = tx_stop.send(());
        handle.await.unwrap().unwrap();
    }

    #[test]
    fn 回放控制_倍速与暂停开关() {
        let c = PlaybackControl::new();
        assert_eq!(c.speed(), 1.0);
        assert!(!c.is_paused());

        c.set_speed(2.0);
        assert_eq!(c.speed(), 2.0);
        c.set_speed(0.5);
        assert_eq!(c.speed(), 0.5);
        c.set_speed(-1.0); // 非正忽略,保持上一次。
        assert_eq!(c.speed(), 0.5);

        c.pause();
        assert!(c.is_paused());
        c.resume();
        assert!(!c.is_paused());
    }

    #[test]
    fn 回放拖动_seek请求消费一次() {
        let c = PlaybackControl::new();
        assert_eq!(c.take_seek(), None); // 无请求
        c.seek_permille(500);
        assert_eq!(c.take_seek(), Some(500)); // 取一次
        assert_eq!(c.take_seek(), None); // 已清除
        c.seek_permille(2000); // 越界钳到 1000
        assert_eq!(c.take_seek(), Some(1000));
    }

    #[tokio::test]
    async fn 暂停后不再产包_恢复后继续() {
        let rx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let rx_addr = rx.local_addr().unwrap();
        let control = PlaybackControl::new();
        let ctl = control.clone();

        let source = Box::new(FileSource::from_bytes(&sample_h264()).unwrap());
        let (tx_stop, rx_stop) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            push_stream_controlled(source, rx_addr, 0x99, 50, false, ctl, async {
                let _ = rx_stop.await;
            })
            .await
        });

        // 先收到至少一个包(在推流中)。
        let mut buf = [0u8; 2048];
        tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv(&mut buf))
            .await
            .expect("超时未收到 RTP")
            .unwrap();

        // 暂停后:清空积压 + 一小段等待窗口内应无新包。
        control.pause();
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        while rx.try_recv(&mut buf).is_ok() {} // 丢弃暂停前在途的包
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let paused_no_packet = rx.try_recv(&mut buf).is_err();
        assert!(paused_no_packet, "暂停期间不应再产包");

        // 恢复后应重新产包。
        control.resume();
        let got = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv(&mut buf)).await;
        assert!(got.is_ok(), "恢复后应重新收到 RTP");

        let _ = tx_stop.send(());
        let _ = handle.await;
    }
}
