//! 推流驱动:把视频源按帧率封 PS 并经 RTP 发出。
//!
//! 供 gb28181-simulator 在 INVITE 建流后调用。按 `fps` 定时取帧、PS 封装、RTP 分片发送,
//! 时间戳按 90kHz / fps 递增,直到 `stop` 完成。

use std::future::Future;
use std::net::SocketAddr;

use common::Result;

use crate::ps::PsMuxer;
use crate::rtp::{RtpSender, CLOCK_HZ};
use crate::source::VideoSource;

/// 驱动一路推流:`source` 产帧 → PS 封装 → RTP 发送到 `dst`。
///
/// - `ssrc`:RTP SSRC(与 SDP 的 y= 一致)
/// - `fps`:帧率(决定取帧间隔与时间戳步进)
/// - `stop`:该 future 完成时优雅停流
///
/// 返回停流原因(正常停返回 Ok)。
pub async fn push_stream(
    mut source: Box<dyn VideoSource>,
    dst: SocketAddr,
    ssrc: u32,
    fps: u32,
    stop: impl Future<Output = ()>,
) -> Result<()> {
    let mut sender = RtpSender::new(dst, ssrc).await?;
    let mux = PsMuxer::new();
    let fps = fps.max(1);
    let ts_step = CLOCK_HZ / fps;
    let frame_interval = std::time::Duration::from_micros(1_000_000 / fps as u64);
    let mut timestamp: u32 = 0;
    let mut ticker = tokio::time::interval(frame_interval);

    tokio::pin!(stop);
    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = ticker.tick() => {
                if let Some(frame) = source.next_frame() {
                    let ps = mux.mux_frame(&frame.data, timestamp, frame.key_frame);
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
            push_stream(source, rx_addr, 0x2233, 50, async { let _ = rx_stop.await; }).await
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
}
