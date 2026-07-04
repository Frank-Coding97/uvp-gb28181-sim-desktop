//! 推流驱动:把视频源按帧率封 PS 并经 RTP 发出。
//!
//! 供 gb28181-simulator 在 INVITE 建流后调用。按 `fps` 定时取帧、PS 封装、RTP 分片发送,
//! 时间戳按 90kHz / fps 递增,直到 `stop` 完成。

use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use common::Result;

use crate::ps::PsMuxer;
use crate::rtp::{RtpSender, CLOCK_HZ};
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
}

impl Default for PlaybackControl {
    fn default() -> Self {
        Self {
            speed_milli: AtomicU32::new(1000),
            paused: AtomicBool::new(false),
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
    mut source: Box<dyn VideoSource>,
    dst: SocketAddr,
    ssrc: u32,
    fps: u32,
    use_tcp: bool,
    control: Arc<PlaybackControl>,
    stop: impl Future<Output = ()>,
) -> Result<()> {
    let mut sender = if use_tcp {
        RtpSender::new_tcp(dst, ssrc).await?
    } else {
        RtpSender::new(dst, ssrc).await?
    };
    let mux = PsMuxer::new();
    let fps = fps.max(1);
    let ts_step = CLOCK_HZ / fps;
    let base_interval_us = 1_000_000f32 / fps as f32;
    let mut timestamp: u32 = 0;

    tokio::pin!(stop);
    loop {
        // 按当前倍速动态计算取帧间隔(倍速越高间隔越短);暂停时用固定轮询间隔。
        let sleep_us = if control.is_paused() {
            50_000 // 暂停:50ms 轮询等待恢复,不发送。
        } else {
            (base_interval_us / control.speed().max(0.01)).max(1.0) as u64
        };
        let sleep = tokio::time::sleep(std::time::Duration::from_micros(sleep_us));
        tokio::select! {
            _ = &mut stop => break,
            _ = sleep => {
                if control.is_paused() {
                    continue; // 暂停中,不取帧。
                }
                if let Some(frame) = source.next_frame() {
                    // 音视频复合流:取该帧时间窗的音频包,与视频一起封进 PS。
                    let audio = source.next_audio();
                    let ps = mux.mux_frame_av(&frame.data, timestamp, frame.key_frame, &audio);
                    sender.send_frame(&ps, timestamp).await?;
                    timestamp = timestamp.wrapping_add(ts_step);
                } else {
                    // 有限源耗尽,停流。
                    break;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::FileSource;
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
