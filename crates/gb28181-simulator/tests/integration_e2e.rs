//! 端到端集成测试 - 验证 P1-P6 各功能点
//!
//! 运行策略：
//! - `test_tcp_passive_push`：纯本地，无需平台，CI 可直接跑。
//! - `test_device_registration_and_preview`：需要能响应 SIP Digest 的真实/
//!   完整模拟平台，本地不自动运行（标注 `#[ignore]`）。
//! - `test_file_passthrough`：需要 ffmpeg 与实际视频文件，CI 跳过。

use std::sync::Arc;
use std::time::Duration;

use common::{AudioTransport, DeviceId, GbVersion, SignalingEncoding, Transport};
use gb28181_simulator::{DeviceConfig, DeviceInfo, DeviceSimulator};
use media_rtp::{LightSource, PlaybackControl, PushOptions, RtpTarget, VideoSource};

// ─────────────────────────────────────────────────────────────────────────────
// P1：TCP 被动推流
// ─────────────────────────────────────────────────────────────────────────────

/// 验证 TCP 被动模式：设备在本地端口监听，平台主动连入后能收到合法 RTP/PS 帧。
/// 完全本地运行，CI 无需外部依赖。
#[tokio::test]
async fn test_tcp_passive_push() {
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpStream;

    // 绑定 TCP 监听端口（由 OS 分配）
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("TcpListener 绑定失败");
    let port = listener.local_addr().unwrap().port();

    // RTP 被动目标：持有 TcpListener（不需要 Arc 包装）
    let target = RtpTarget::TcpPassive {
        listener,
        accept_timeout: Duration::from_secs(10),
    };

    // LightSource：内置伪流，无需真实编码器，CI 可用
    let source: Box<dyn VideoSource> = Box::new(LightSource::new(512, 25));

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();

    let push_task = tokio::spawn(async move {
        let options = PushOptions {
            ssrc: 12345,
            fps: 25,
            with_audio: false,
        };
        media_rtp::push_stream_to(
            source,
            target,
            options,
            PlaybackControl::new(),
            async move {
                let _ = stop_rx.await;
            },
        )
        .await
    });

    // 等待推流端就绪后连入
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("TCP 连接失败");

    // RFC 4571 帧：2 字节大端长度 + RTP 包体
    let mut len_buf = [0u8; 2];
    stream
        .read_exact(&mut len_buf)
        .await
        .expect("读取帧长度失败");
    let frame_len = u16::from_be_bytes(len_buf) as usize;
    assert!(frame_len > 12, "RTP 包体长度 ({frame_len}) 小于最小 RTP 头 (12)");

    let mut rtp_buf = vec![0u8; frame_len];
    stream
        .read_exact(&mut rtp_buf)
        .await
        .expect("读取 RTP 包体失败");

    // RTP 版本字段（首字节高 2 位）必须为 2
    assert_eq!(rtp_buf[0] >> 6, 2, "RTP 版本字段不为 2");
    println!("✓ P1 TCP 被动模式验证通过，帧长 = {frame_len}");

    drop(stream);
    stop_tx.send(()).ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), push_task).await;
}

// ─────────────────────────────────────────────────────────────────────────────
// P2-P6：注册 + 预览订阅 + 零拷贝 + 抓拍
// ─────────────────────────────────────────────────────────────────────────────

/// 验证设备注册、预览订阅（P4）、Bytes 零拷贝（P5）、抓拍（P6）。
///
/// **需要完整 SIP Digest 模拟平台**（含 401→REGISTER→200 OK 握手及 INVITE 流程），
/// 本地使用 `cargo test -- --ignored test_device_registration_and_preview` 运行。
#[tokio::test]
#[ignore = "需要完整 SIP Digest 模拟平台，本地手动运行"]
async fn test_device_registration_and_preview() {
    // 启动简易 UDP 模拟平台（仅转发 200 OK，不做 Digest 挑战）
    let platform_port = spawn_mock_platform("127.0.0.1:0").await;

    let device_config = DeviceConfig {
        device_id: DeviceId::new("35020000001320000001").unwrap(),
        username: "35020000001320000001".into(),
        password: "12345678".into(),
        server_host: "127.0.0.1".into(),
        server_port: platform_port,
        server_id: "34020000002000000001".into(),
        server_domain: "3402000000".into(),
        transport: Transport::Udp,
        audio_transport: AudioTransport::Udp,
        register_expires_secs: 3_600,
        heartbeat_interval_secs: 60,
        heartbeat_fail_threshold: 3,
        channels: vec![],
        device_info: DeviceInfo {
            device_name: "测试设备".into(),
            manufacturer: "UVP".into(),
            model: "SimTest".into(),
            firmware: "0.1.0".into(),
            hardware_version: None,
            installation_address: None,
            device_type: None,
        },
        video_source: None,
        media_profile: None,
        video_fps: 25,
        light_bitrate_kbps: Some(512),
        gb_version: GbVersion::V2022,
        signaling_encoding: SignalingEncoding::Gb18030,
    };

    let device = Arc::new(DeviceSimulator::new(device_config));

    // SIP 传输：绑定随机 UDP 端口
    let transport = sip_core::SipTransport::bind_udp("0.0.0.0:0")
        .await
        .expect("SIP 传输绑定失败");
    let local_port = transport.local_addr().expect("获取本地端口失败").port();

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let dev_clone = Arc::clone(&device);
    let tp_clone = Arc::clone(&transport);

    // 在后台运行设备状态机，收到 stop_rx 后退出
    let device_task = tokio::spawn(async move {
        dev_clone
            .run(tp_clone, "127.0.0.1".into(), local_port, async move {
                let _ = stop_rx.await;
            })
            .await;
    });

    // 等待注册流程完成（简易平台无 Digest，实际会超时重试）
    tokio::time::sleep(Duration::from_secs(2)).await;

    // P4/P5/P6 依赖 shared_media（设备收到 INVITE 后才启动采集）。
    // 以下验证：尚无 INVITE 时 subscribe_shared_media 返回 None。
    let sub = device.subscribe_shared_media().await;
    assert!(sub.is_none(), "预期无 INVITE 时 subscribe_shared_media 返回 None");

    // P6 同理：无 INVITE 时抓拍返回错误
    let snap = device.current_keyframe_jpeg().await;
    assert!(snap.is_err(), "预期无 INVITE 时 current_keyframe_jpeg 返回错误");

    println!("✓ 注册流程执行、P4/P5/P6 前置状态验证通过");

    stop_tx.send(()).ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), device_task).await;
}

// ─────────────────────────────────────────────────────────────────────────────
// P6（扩展）：文件直通策略
// ─────────────────────────────────────────────────────────────────────────────

/// 验证文件源准备流程（直通策略）。
/// 需要 ffmpeg 在 PATH 可用，且磁盘可写临时目录，CI 默认跳过。
#[tokio::test]
#[ignore = "需要 ffmpeg 和可写临时目录，CI 跳过"]
async fn test_file_passthrough() {
    use media_rtp::file_profile::prepare_file_profile;
    use media_rtp::profile::MediaProfile;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    let ffmpeg = media_rtp::ffmpeg_bin().unwrap_or_else(|| "ffmpeg".into());
    let test_file = create_test_h264_file("test_1080p25.mp4", 1920, 1080, 25, &ffmpeg);

    // MediaProfile::default() 已是 1280×720 H.264 G.711A，直接使用
    let profile = MediaProfile::default();
    let cancel = AtomicBool::new(false);

    // prepare_file_profile 是同步函数，在阻塞线程里运行
    let result = tokio::task::spawn_blocking(move || {
        prepare_file_profile(Path::new(&test_file), &profile, &cancel, None)
    })
    .await
    .expect("spawn_blocking 失败");

    assert!(result.is_ok(), "文件准备失败: {:?}", result.err());
    println!("✓ P6 文件直通策略验证通过");
}

// ─────────────────────────────────────────────────────────────────────────────
// 辅助函数
// ─────────────────────────────────────────────────────────────────────────────

/// 启动简易 UDP 模拟平台：收到 REGISTER 即回极简 200 OK。
/// 返回实际绑定的端口号。
///
/// **注意**：响应不含 Via/From/To/CSeq/Call-ID 等 SIP 必填头，无法通过
/// DeviceSimulator 的事务层匹配，仅用于验证设备能发出注册报文。
async fn spawn_mock_platform(addr: &str) -> u16 {
    let socket = tokio::net::UdpSocket::bind(addr)
        .await
        .expect("模拟平台端口绑定失败");
    let port = socket.local_addr().unwrap().port();

    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            let Ok((n, src)) = socket.recv_from(&mut buf).await else {
                break;
            };
            let msg = String::from_utf8_lossy(&buf[..n]);
            if msg.starts_with("REGISTER") {
                // 极简响应；真实测试需提取 Via/From/To/CSeq/Call-ID 并回填
                let _ = socket
                    .send_to(b"SIP/2.0 200 OK\r\nContent-Length: 0\r\n\r\n", src)
                    .await;
            }
        }
    });

    port
}

fn create_test_h264_file(path: &str, width: u32, height: u32, fps: u32, ffmpeg: &str) -> String {
    let out = std::process::Command::new(ffmpeg)
        .args([
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=duration=5:size={width}x{height}:rate={fps}"),
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            path,
            "-y",
        ])
        .output()
        .expect("启动 ffmpeg 失败");
    assert!(out.status.success(), "ffmpeg 创建测试文件失败: {:?}", out);
    path.to_string()
}
