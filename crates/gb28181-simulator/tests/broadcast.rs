//! 语音广播下行(反向 INVITE + RTP 收流 + G.711A 解码)集成测试。
//!
//! 用真实 UDP/TCP socket 扮演平台:收设备的 INVITE、回 200 OK(SDP)、发 RTP、
//! 最后 BYE,验证设备侧会话建立、音频回调与结束事件。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{AudioTransport, DeviceEvent, DeviceId, DeviceObserver, GbVersion, SignalingEncoding, Transport};
use gb28181_protocol::manscdp::BroadcastNotify;
use gb28181_simulator::broadcast::{BroadcastAudioSink, BroadcastRunner};
use gb28181_simulator::{ChannelConfig, DeviceConfig, DeviceInfo};
use sip_core::{Headers, Method, Request, Response, SipMessage, SipTransport};

const DEVICE_ID: &str = "34020000001310000001";
const CHANNEL_ID: &str = "34020000001320000001";
const PLATFORM_ID: &str = "34020000002000000001";
const DOMAIN: &str = "3402000000";

fn config(platform_port: u16, audio: AudioTransport) -> DeviceConfig {
    DeviceConfig {
        device_id: DeviceId::new(DEVICE_ID).unwrap(),
        username: DEVICE_ID.into(),
        password: "12345678".into(),
        server_host: "127.0.0.1".into(),
        server_port: platform_port,
        server_id: PLATFORM_ID.into(),
        server_domain: DOMAIN.into(),
        transport: Transport::Udp,
        audio_transport: audio,
        register_expires_secs: 3_600,
        heartbeat_interval_secs: 60,
        heartbeat_fail_threshold: 3,
        channels: vec![ChannelConfig {
            channel_id: DeviceId::new(CHANNEL_ID).unwrap(),
            name: "Camera-1".into(),
            status: "ON".into(),
        }],
        device_info: DeviceInfo {
            device_name: "BroadcastTest".into(),
            manufacturer: "UVP".into(),
            model: "Sim".into(),
            firmware: "1.0".into(),
        },
        video_source: None,
        media_profile: None,
        video_fps: 25,
        light_bitrate_kbps: None,
        gb_version: GbVersion::V2022,
        signaling_encoding: SignalingEncoding::Utf8,
    }
}

#[derive(Default)]
struct CaptureObserver {
    events: Mutex<Vec<String>>,
}

impl DeviceObserver for CaptureObserver {
    fn on_event(&self, event: DeviceEvent) {
        let tag = match event {
            DeviceEvent::BroadcastStarted { .. } => "started",
            DeviceEvent::BroadcastEnded { .. } => "ended",
            DeviceEvent::BroadcastProgress { .. } => "progress",
            _ => return,
        };
        self.events.lock().unwrap().push(tag.into());
    }
}

#[derive(Default)]
struct CaptureSink {
    pcm: Mutex<Vec<i16>>,
}

impl BroadcastAudioSink for CaptureSink {
    fn on_pcm(&self, pcm: &[i16], _sample_rate: u32) {
        self.pcm.lock().unwrap().extend_from_slice(pcm);
    }
}

fn notify() -> BroadcastNotify {
    BroadcastNotify {
        cmd_type: "Broadcast".into(),
        sn: 7,
        source_id: PLATFORM_ID.into(),
        target_id: CHANNEL_ID.into(),
    }
}

/// 平台侧收到 INVITE 后回 200 OK。
fn answer(req: &Request, sdp: &str) -> SipMessage {
    let mut headers = Headers::new();
    for via in req.headers.get_all("Via") {
        headers.append("Via", via.to_string());
    }
    headers.append("From", req.headers.get("From").unwrap().to_string());
    headers.append(
        "To",
        format!("{};tag=platformtag", req.headers.get("To").unwrap()),
    );
    headers.append("Call-ID", req.headers.call_id().unwrap().to_string());
    headers.append("CSeq", req.headers.cseq().unwrap().to_string());
    headers.append("Contact", "<sip:platform@127.0.0.1:5099>");
    headers.append("Content-Type", "application/sdp");
    SipMessage::Response(Response {
        status: 200,
        reason: "OK".into(),
        headers,
        body: sdp.as_bytes().to_vec(),
    })
}

fn rtp_packet(payload: &[u8]) -> Vec<u8> {
    let mut packet = vec![0x80, 8, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1];
    packet.extend_from_slice(payload);
    packet
}

async fn wait_for_pcm(sink: &Arc<CaptureSink>, min: usize) -> bool {
    for _ in 0..100 {
        if sink.pcm.lock().unwrap().len() >= min {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    false
}

fn bye_request(call_id: &str) -> Request {
    let mut headers = Headers::new();
    headers.set("Call-ID", call_id);
    headers.set("CSeq", "2 BYE");
    Request {
        method: Method::Bye,
        uri: format!("sip:{DEVICE_ID}@127.0.0.1"),
        headers,
        body: Vec::new(),
    }
}

#[tokio::test]
async fn udp_广播收流并解码() {
    let platform = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
    let platform_addr = platform.local_addr().unwrap();
    let mut inbound = platform.register_inbound(PLATFORM_ID);
    let device = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
    let device_addr = device.local_addr().unwrap();

    let observer = Arc::new(CaptureObserver::default());
    let sink = Arc::new(CaptureSink::default());
    let runner = Arc::new(BroadcastRunner::new(
        config(platform_addr.port(), AudioTransport::Udp),
        observer.clone(),
    ));
    runner.set_audio_sink(sink.clone());

    assert!(runner.start(
        Arc::clone(&device),
        "127.0.0.1".into(),
        device_addr.port(),
        notify()
    ));

    // 平台收 INVITE。
    let incoming = tokio::time::timeout(Duration::from_secs(5), inbound.recv())
        .await
        .expect("等待 INVITE 超时")
        .expect("入站队列关闭");
    let SipMessage::Request(invite) = incoming.message else {
        panic!("应为 INVITE 请求")
    };
    assert_eq!(invite.method, Method::Invite);
    let sdp = String::from_utf8_lossy(&invite.body).to_string();
    assert!(sdp.contains("s=Broadcast"));
    assert!(sdp.contains("m=audio "));
    assert!(sdp.contains("RTP/AVP 8 0"));
    let local_port: u16 = sdp
        .lines()
        .find(|line| line.starts_with("m=audio "))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|port| port.parse().ok())
        .expect("offer 缺少媒体端口");
    assert!(local_port > 0);

    let call_id = invite.headers.call_id().unwrap().to_string();
    let answer_sdp = "v=0\r\n\
o=34020000002000000001 0 0 IN IP4 127.0.0.1\r\n\
s=Talk\r\n\
c=IN IP4 127.0.0.1\r\n\
t=0 0\r\n\
m=audio 40000 RTP/AVP 8\r\n\
a=sendonly\r\n\
a=rtpmap:8 PCMA/8000\r\n\
y=1234\r\n";
    platform
        .send_to(&answer(&invite, answer_sdp), device_addr)
        .await
        .unwrap();

    // 平台发 2 个 RTP 包(每包 2 个 A-law 采样)。
    let sender = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sender
        .send_to(&rtp_packet(&[0xD5, 0x55]), (device_addr.ip(), local_port))
        .await
        .unwrap();
    sender
        .send_to(&rtp_packet(&[0xD5, 0xD5]), (device_addr.ip(), local_port))
        .await
        .unwrap();

    assert!(wait_for_pcm(&sink, 4).await, "未收到解码后的 PCM");
    let pcm = sink.pcm.lock().unwrap().clone();
    assert_eq!(pcm.len(), 4);
    assert_eq!(pcm[0], 8); // A-law 0xD5 静音
    assert_eq!(pcm[1], -8); // A-law 0x55

    // 平台 BYE → 指定 Call-ID 的会话被命中并结束。
    let bye = bye_request(&call_id);
    assert!(runner.handle_bye(&bye).await);
    assert!(!runner.is_active());
    let events = observer.events.lock().unwrap().clone();
    assert!(events.contains(&"started".to_string()), "应有 started 事件: {events:?}");
    assert!(events.contains(&"ended".to_string()), "应有 ended 事件: {events:?}");
}

#[tokio::test]
async fn tcp主动_设备连平台收流() {
    let platform = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
    let platform_addr = platform.local_addr().unwrap();
    let mut inbound = platform.register_inbound(PLATFORM_ID);
    let device = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
    let device_addr = device.local_addr().unwrap();

    // 平台先监听 TCP,等设备主动连入。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let media_port = listener.local_addr().unwrap().port();

    let observer = Arc::new(CaptureObserver::default());
    let sink = Arc::new(CaptureSink::default());
    let runner = Arc::new(BroadcastRunner::new(
        config(platform_addr.port(), AudioTransport::TcpActive),
        observer.clone(),
    ));
    runner.set_audio_sink(sink.clone());

    assert!(runner.start(
        Arc::clone(&device),
        "127.0.0.1".into(),
        device_addr.port(),
        notify()
    ));

    let incoming = tokio::time::timeout(Duration::from_secs(5), inbound.recv())
        .await
        .expect("等待 INVITE 超时")
        .expect("入站队列关闭");
    let SipMessage::Request(invite) = incoming.message else {
        panic!("应为 INVITE 请求")
    };
    let sdp = String::from_utf8_lossy(&invite.body).to_string();
    assert!(sdp.contains("TCP/RTP/AVP"), "TCP 主动 offer 应为 TCP/RTP/AVP");
    assert!(sdp.contains("a=setup:active"), "TCP 主动 offer 应 setup:active");

    let answer_sdp = format!(
        "v=0\r\n\
o=34020000002000000001 0 0 IN IP4 127.0.0.1\r\n\
s=Talk\r\n\
c=IN IP4 127.0.0.1\r\n\
t=0 0\r\n\
m=audio {media_port} TCP/RTP/AVP 8\r\n\
a=setup:passive\r\n\
a=sendonly\r\n\
a=rtpmap:8 PCMA/8000\r\n"
    );
    platform
        .send_to(&answer(&invite, &answer_sdp), device_addr)
        .await
        .unwrap();

    // 平台 accept,并按 RFC4571(2 字节大端长度)写 RTP 帧。
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("设备未连入平台 TCP")
        .expect("accept 失败");
    let packet = rtp_packet(&[0xD5, 0x55, 0xD5]);
    use tokio::io::AsyncWriteExt;
    stream.write_all(&(packet.len() as u16).to_be_bytes()).await.unwrap();
    stream.write_all(&packet).await.unwrap();

    assert!(wait_for_pcm(&sink, 3).await, "TCP 主动模式未收到 PCM");
    assert_eq!(sink.pcm.lock().unwrap().len(), 3);

    let call_id = invite.headers.call_id().unwrap().to_string();
    assert!(runner.handle_bye(&bye_request(&call_id)).await);
}
