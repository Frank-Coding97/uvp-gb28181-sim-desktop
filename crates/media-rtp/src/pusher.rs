//! 推流驱动:把视频源按帧率封 PS 并经 RTP 发出。
//!
//! 供 gb28181-simulator 在 INVITE 建流后调用。按 `fps` 定时取帧、PS 封装、RTP 分片发送,
//! 时间戳按 90kHz / fps 递增,直到 `stop` 完成。

use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use common::{Error, Result};

use crate::ps::PsMuxer;
use crate::rtp::CLOCK_HZ;
use crate::source::VideoSource;

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
    /// 待处理的强制关键帧请求(A.2.3.1.7 IFrameCmd)。推流循环消费后清零。
    keyframe_requested: AtomicBool,
    /// 本次会话的发送统计与 RTCP 反馈句柄,由推流循环在建立通道后回填。
    ///
    /// 发送器是在推流任务内部创建的,而"平台收得怎么样"要给设备控制台看 ——
    /// 借这个已经在两侧共享的控制块把句柄递出来,免得给推流函数再加出参。
    telemetry: std::sync::OnceLock<SessionTelemetry>,
}

/// 一次推流会话的遥测句柄(发送侧 + 平台反馈侧)。
#[derive(Debug, Clone)]
pub struct SessionTelemetry {
    /// 本端发送统计。
    pub stats: Arc<crate::rtp::SendStats>,
    /// 平台回报的收流质量(RTCP RR)。
    pub feedback: Arc<crate::rtcp::RtcpFeedback>,
    /// 本端 RTCP 端口;TCP 模式或未抢到 RTP+1 时为 None。
    pub local_rtcp_port: Option<u16>,
    /// 该流的 RTP 时钟频率,用于把抖动换算成毫秒。
    pub clock_hz: u32,
    /// 发送通道建立的时刻。`RtcpFeedback` 里的"最近一次报告"记的是相对这个
    /// 时刻的毫秒数,消费方算"报告有多旧"必须用同一个原点。
    pub started: std::time::Instant,
}

impl SessionTelemetry {
    /// 距发送通道建立至今的毫秒数(传给 `RtcpFeedback::snapshot`)。
    pub fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
}

impl Default for PlaybackControl {
    fn default() -> Self {
        Self {
            speed_milli: AtomicU32::new(1000),
            paused: AtomicBool::new(false),
            seek_permille_plus1: AtomicU32::new(0),
            keyframe_requested: AtomicBool::new(false),
            telemetry: std::sync::OnceLock::new(),
        }
    }
}

impl PlaybackControl {
    /// 新建默认控制(1.0 倍速、未暂停)。
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 推流循环建立发送通道后回填遥测句柄。重复调用只有第一次生效。
    pub fn set_telemetry(&self, telemetry: SessionTelemetry) {
        let _ = self.telemetry.set(telemetry);
    }

    /// 本次会话的遥测句柄;通道还没建立时为 None。
    pub fn telemetry(&self) -> Option<&SessionTelemetry> {
        self.telemetry.get()
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

    /// 请求下一帧强制为关键帧(GB/T 28181-2022 A.2.3.1.7 的 IFrameCmd)。
    ///
    /// 平台调这个命令是为了**立刻拿到一个能起解的画面**(新客户端接流、花屏恢复)。
    /// 能不能真兑现取决于源:实时编码源可以让编码器立刻出 IDR;
    /// 文件循环源只能等自带的周期 IDR(硬标关键帧会产出坏流)。
    pub fn request_keyframe(&self) {
        self.keyframe_requested.store(true, Ordering::Relaxed);
    }

    /// 取出并清除待处理的强制关键帧请求。
    pub fn take_keyframe_request(&self) -> bool {
        self.keyframe_requested.swap(false, Ordering::Relaxed)
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
    let prepared = crate::rtp::PreparedMedia::prepare(dst, use_tcp, false).await?;
    push_stream_prepared(prepared, source, dst, ssrc, fps, control, stop).await
}

/// 用**已预先绑定端口**的媒体通道推流。
///
/// 调用方(设备侧 INVITE 处理)需要在发 200 OK 之前就知道本端 RTP 端口,
/// 才能把真实端口写进 SDP;实际连接/accept 留到这里做。
pub async fn push_stream_prepared(
    prepared: crate::rtp::PreparedMedia,
    mut source: Box<dyn VideoSource>,
    dst: SocketAddr,
    ssrc: u32,
    fps: u32,
    control: Arc<PlaybackControl>,
    stop: impl Future<Output = ()>,
) -> Result<()> {
    let use_tcp = prepared.is_tcp();
    let mut sender = prepared.into_sender(ssrc).await?;
    let local_rtcp_port = sender.local_rtcp_port();
    control.set_telemetry(SessionTelemetry {
        stats: sender.stats(),
        feedback: sender.feedback(),
        local_rtcp_port,
        clock_hz: CLOCK_HZ,
        started: sender.started(),
    });
    tracing::info!(%dst, ssrc = format!("{ssrc:#x}"), use_tcp, fps, live = source.is_live(), local_rtcp_port, "推流开始:已建立发送通道");
    // PSM stream_type 跟随源编码,并按源能力稳定声明音频 ES。
    let audio_codec = source.audio_codec();
    let audio_frame_duration = audio_codec.frame_duration_90k();
    let mut mux = if source.has_audio() {
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
    let mut keyframes_sent: u64 = 0;
    let mut bytes_sent: u64 = 0;
    let mut first_frame = true;
    let mut empty_polls: u64 = 0;

    // PS 封装复用缓冲:稳态零分配(原来每帧一次 Vec::with_capacity + 全帧 memcpy)。
    let mut ps_buf: Vec<u8> = Vec::with_capacity(64 * 1024);

    // 节拍改用**绝对 deadline**。原来每轮新建 `sleep(interval)`,周期 =
    // 本拍工作耗时 + 固定睡眠,于是实际帧率恒低于目标、RTP 时钟比墙钟越走越慢,
    // 平台侧表现为延迟单调增长且永不追回。
    let mut next_tick = tokio::time::Instant::now();
    // 落后超过这个量就认为"错过的拍不再补发"(等价 MissedTickBehavior::Skip),
    // 避免暂停/卡顿恢复后瞬间突发一大串包打爆队列。
    const MAX_LAG: std::time::Duration = std::time::Duration::from_millis(500);
    // RTCP SR 周期(RFC 3550 建议 5s 量级)。部分平台靠 RTCP 判活。
    let rtcp_interval = std::time::Duration::from_secs(5);
    let mut next_rtcp = tokio::time::Instant::now() + rtcp_interval;

    tokio::pin!(stop);
    let result = loop {
        // 按当前倍速动态计算取帧间隔(倍速越高间隔越短);暂停时用固定轮询间隔。
        let sleep_us = if control.is_paused() {
            50_000 // 暂停:50ms 轮询等待恢复,不发送。
        } else {
            (base_interval_us / control.speed().max(0.01)).max(1.0) as u64
        };
        next_tick += std::time::Duration::from_micros(sleep_us);
        let now = tokio::time::Instant::now();
        if next_tick + MAX_LAG < now {
            next_tick = now;
        }
        let sleep = tokio::time::sleep_until(next_tick);
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
                // 处理强制关键帧请求(A.2.3.1.7 IFrameCmd)。
                // 只有实时编码源能兑现;文件源如实返回 false,上层据此如实上报。
                if control.take_keyframe_request() {
                    let honored = source.request_keyframe();
                    tracing::info!(honored, "推流:收到强制关键帧请求");
                }
                if control.is_paused() {
                    continue; // 暂停中,不取帧。
                }
                if let Some(frame) = source.next_frame() {
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
                    mux.mux_into(
                        &mut ps_buf,
                        &frame.data,
                        ps_timestamp,
                        frame.key_frame,
                        &audio,
                        audio_start_pts,
                    );
                    let ps = &ps_buf;
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
                    if let Err(e) = sender.send_frame(ps, timestamp).await {
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
                    // 周期发 RTCP SR,把发送包数/字节数反馈给平台(FR-32)。
                    if tokio::time::Instant::now() >= next_rtcp {
                        if let Err(error) = sender.send_rtcp_sr(timestamp).await {
                            // RTCP 失败不该中断媒体流:UDP 下平台可能压根没开 RTCP 端口。
                            tracing::debug!(%error, "RTCP SR 发送失败(忽略)");
                        }
                        next_rtcp = tokio::time::Instant::now() + rtcp_interval;
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{FileSource, Frame};
    use tokio::net::UdpSocket;

    fn sample_h264() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&[0, 0, 0, 1, 0x67, 0xAA]);
        v.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xBB]);
        v.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x11]);
        v.extend_from_slice(&[0, 0, 0, 1, 0x61, 0x22]);
        v
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
                    data: bytes::Bytes::from_static(&[0, 0, 0, 1, 0x65, 0x11]),
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
    fn 强制关键帧_请求消费一次() {
        let c = PlaybackControl::new();
        assert!(!c.take_keyframe_request(), "无请求时应为 false");
        c.request_keyframe();
        assert!(c.take_keyframe_request(), "应取到一次请求");
        assert!(!c.take_keyframe_request(), "取过之后应被清除");
        // 连续多次请求合并成一次:平台连点 IFrameCmd 不该攒出一串 IDR。
        c.request_keyframe();
        c.request_keyframe();
        assert!(c.take_keyframe_request());
        assert!(!c.take_keyframe_request());
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

/// G.711 的 RTP 时钟频率(Hz)。附录 C.2.4:8 kHz、单声道。
pub const AUDIO_CLOCK_HZ: u32 = 8_000;
/// 一个 20 ms 的 G.711 访问单元字节数(8000 Hz × 0.02 s × 1 字节)。
pub const G711_PACKET_BYTES_20MS: usize = 160;
/// A-law 的静音码字。设备没有音频输入时用它把通道撑起来 ——
/// 一个没接麦克风的 IPC 在对讲上行方向本来就是这个行为。
const ALAW_SILENCE: u8 = 0xD5;

/// 纯音频上行推流(语音对讲的设备→平台方向,§9.12.2 a) + 附录 C.2.4)。
///
/// 与 [`push_stream_prepared`] 的关键区别:**不做 PS 封装**。附录 C.2.4 规定语音比特流
/// 直接用 RTP 打包(PT=8 / PCMA / 8 kHz / 单声道),一个 RTP 包装整数个音频编码帧、
/// 时长 20~180 ms。把音频塞进 PS 再发是视音频复合流的做法,纯音频会话这么做对端解不出。
///
/// 音频来源:优先取 `source` 的音频轨(与复合流同一条解析路径);源无音频时发 A-law 静音,
/// 保证对讲通道能建起来 —— 但会在返回的统计里如实反映"发的是静音"。
pub async fn push_audio_stream(
    prepared: crate::rtp::PreparedMedia,
    mut source: Option<Box<dyn VideoSource>>,
    dst: SocketAddr,
    ssrc: u32,
    stop: impl Future<Output = ()>,
) -> Result<AudioPushSummary> {
    let mut sender = prepared.into_sender(ssrc).await?;
    // 附录 C.2.4 a):G.711 A 律的负载类型固定 8。
    sender.set_payload_type(crate::receiver::PT_PCMA);
    let has_audio_source = source.as_ref().is_some_and(|s| s.has_audio());
    tracing::info!(
        %dst, ssrc = format!("{ssrc:#x}"), has_audio_source,
        "音频上行推流开始(裸 G.711A / PT=8)"
    );

    let silence = vec![ALAW_SILENCE; G711_PACKET_BYTES_20MS];
    let mut queue: std::collections::VecDeque<Vec<u8>> = std::collections::VecDeque::new();
    let mut timestamp: u32 = 0;
    let mut summary = AudioPushSummary::default();
    // 首包置 marker(RFC 3551 §4.1 的 talkspurt 起始)。
    let mut first = true;

    // 20 ms 绝对 deadline:与视频推流同样的理由 —— 用"上次 deadline + 20ms"而不是
    // "干完活再睡 20ms",否则每拍的处理耗时会累积成时钟漂移。
    let period = std::time::Duration::from_millis(20);
    let mut deadline = tokio::time::Instant::now() + period;
    let stop = std::pin::pin!(stop);
    let mut stop = stop;

    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = tokio::time::sleep_until(deadline) => {}
        }
        deadline += period;

        if queue.is_empty() {
            if let Some(src) = source.as_mut() {
                // 音频轨与视频帧同一条解复用路径:必须推进视频游标才能拿到下一窗音频。
                let _ = src.next_frame();
                for au in src.next_audio() {
                    if !au.is_empty() {
                        queue.push_back(au);
                    }
                }
                if let Some(error) = src.take_error() {
                    tracing::warn!(%error, "音频上行:采集后端报错,结束会话");
                    summary.source_error = Some(error);
                    break;
                }
            }
        }
        let au = match queue.pop_front() {
            Some(au) => {
                summary.captured_packets += 1;
                au
            }
            None => {
                summary.silence_packets += 1;
                silence.clone()
            }
        };
        if let Err(error) = sender.send_audio(&au, timestamp, first).await {
            tracing::warn!(%error, "音频上行发送失败,结束会话");
            summary.source_error = Some(error.to_string());
            break;
        }
        first = false;
        summary.bytes += au.len() as u64;
        // 时间戳按**采样数**递增(8 kHz 下 20 ms = 160),不是按 90 kHz 视频时钟。
        timestamp = timestamp.wrapping_add(au.len() as u32);
    }

    tracing::info!(
        captured = summary.captured_packets,
        silence = summary.silence_packets,
        bytes = summary.bytes,
        "音频上行推流结束"
    );
    Ok(summary)
}

/// 音频上行推流的结果汇总。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioPushSummary {
    /// 真实采集到的音频包数。
    pub captured_packets: u64,
    /// 填充静音的包数(源无音频或供不上时)。
    pub silence_packets: u64,
    /// 发出的音频负载字节数。
    pub bytes: u64,
    /// 提前结束的原因(采集后端错误 / 发送失败);正常停止为 None。
    pub source_error: Option<String>,
}

impl AudioPushSummary {
    /// 是否**全程只发了静音**(即"通道通了但没有真实音源")。
    pub fn silence_only(&self) -> bool {
        self.captured_packets == 0 && self.silence_packets > 0
    }
}

#[cfg(test)]
mod audio_tests {
    use super::*;
    use crate::rtp::PreparedMedia;
    use tokio::net::UdpSocket;

    /// 只提供音频、不提供视频帧的测试源(模拟"对讲只要音频")。
    struct AudioOnlySource {
        au: Vec<u8>,
        left: usize,
    }

    impl VideoSource for AudioOnlySource {
        fn next_frame(&mut self) -> Option<crate::source::Frame> {
            None
        }
        fn next_audio(&mut self) -> Vec<Vec<u8>> {
            if self.left == 0 {
                return Vec::new();
            }
            self.left -= 1;
            vec![self.au.clone()]
        }
        fn has_audio(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn 纯音频上行_按pt8发裸g711并按采样数递增时间戳() {
        let rx = UdpSocket::bind("127.0.0.1:0").await.expect("bind rx");
        let dst = rx.local_addr().expect("addr");
        let prepared = PreparedMedia::prepare(dst, false, false)
            .await
            .expect("prepare");
        let source = Box::new(AudioOnlySource {
            au: vec![0xAA; G711_PACKET_BYTES_20MS],
            left: 3,
        });
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            push_audio_stream(prepared, Some(source), dst, 0x1234, async {
                let _ = stop_rx.await;
            })
            .await
        });

        let mut buf = [0u8; 512];
        let mut seen = Vec::new();
        for _ in 0..3 {
            let n = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv(&mut buf))
                .await
                .expect("应在 2s 内收到音频包")
                .expect("recv");
            seen.push(buf[..n].to_vec());
        }
        let _ = stop_tx.send(());
        let summary = task.await.expect("join").expect("push");

        // 每包 = 12 字节 RTP 头 + 160 字节 G.711。
        for pkt in &seen {
            assert_eq!(pkt.len(), 12 + G711_PACKET_BYTES_20MS);
            assert_eq!(pkt[1] & 0x7F, crate::receiver::PT_PCMA, "负载类型应为 8");
        }
        // 首包 marker=1,后续 0(RFC 3551 §4.1)。
        assert_eq!(seen[0][1] & 0x80, 0x80, "首包应置 marker");
        assert_eq!(seen[1][1] & 0x80, 0x00, "后续包不应置 marker");
        // 时间戳按 160 采样递增。
        let ts = |p: &Vec<u8>| u32::from_be_bytes([p[4], p[5], p[6], p[7]]);
        assert_eq!(
            ts(&seen[1]).wrapping_sub(ts(&seen[0])),
            G711_PACKET_BYTES_20MS as u32
        );
        assert!(summary.captured_packets >= 3, "应发出真实采集的音频包");
    }

    #[tokio::test]
    async fn 无音频源时发静音并如实标记() {
        let rx = UdpSocket::bind("127.0.0.1:0").await.expect("bind rx");
        let dst = rx.local_addr().expect("addr");
        let prepared = PreparedMedia::prepare(dst, false, false)
            .await
            .expect("prepare");
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            push_audio_stream(prepared, None, dst, 1, async {
                let _ = stop_rx.await;
            })
            .await
        });
        let mut buf = [0u8; 512];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv(&mut buf))
            .await
            .expect("应收到静音包")
            .expect("recv");
        assert_eq!(n, 12 + G711_PACKET_BYTES_20MS);
        assert!(
            buf[12..n].iter().all(|&b| b == ALAW_SILENCE),
            "无音源时负载应为 A-law 静音码字"
        );
        let _ = stop_tx.send(());
        let summary = task.await.expect("join").expect("push");
        assert!(summary.silence_only(), "应如实反映全程只发了静音");
    }
}
