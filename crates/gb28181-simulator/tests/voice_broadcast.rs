//! 语音广播 / 语音对讲的端到端回归(GB/T 28181-2022 §9.12)。
//!
//! 这里用一个**假平台**把标准 §9.12.1 图27 的信令 1~5、14、15、17、18 全跑一遍:
//!
//! ```text
//! 平台 --MESSAGE(Broadcast Notify)--> 设备      (信令 1)
//! 设备 --200 OK-------------------->             (信令 2)
//! 设备 --MESSAGE(Broadcast Response)-->          (信令 3)
//! 平台 --200 OK-------------------->             (信令 4)
//! 设备 --INVITE(Subject + s=Play + m=audio recvonly)--> 平台  (信令 5)
//! 平台 --200 OK(SDP)-------------->                          (信令 14)
//! 设备 --ACK---------------------->                          (信令 15)
//! 平台 ==RTP(PCMA)===============>  设备收流
//! 平台 --BYE---------------------->                          (信令 17)
//! 设备 --200 OK------------------->                          (信令 18)
//! ```
//!
//! 之前的实现只到"构造 SDP offer 并打日志"为止,SDP 里端口还写死 0 ——
//! 也就是说广播这条链路从来没真正建起来过。这个测试锁住它不再退回去。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use common::{DeviceId, GbVersion, Transport};
use gb28181_simulator::{DeviceConfig, DeviceInfo, DeviceSimulator};
use sip_core::{Headers, Method, Request, SipMessage, SipTransport, UdpTransport};

const DEVICE_ID: &str = "34020000001320000001";
const PLATFORM_ID: &str = "34020000002000000001";

fn cfg(server_host: &str, server_port: u16, video_source: Option<String>) -> DeviceConfig {
    DeviceConfig {
        device_id: DeviceId::new(DEVICE_ID).unwrap(),
        username: DEVICE_ID.into(),
        password: "12345678".into(),
        server_host: server_host.into(),
        server_port,
        server_domain: PLATFORM_ID.into(),
        transport: Transport::Udp,
        heartbeat_interval_secs: 3600,
        register_expires_secs: 3600,
        register_retry_max_secs: 60,
        channels: vec![],
        device_info: DeviceInfo {
            device_name: "Broadcast-Sim".into(),
            manufacturer: "UVP".into(),
            model: "Sim".into(),
            firmware: "0.1".into(),
        },
        video_source,
        video_fps: 25,
        light_bitrate_kbps: None,
        gb_version: GbVersion::V2022,
        signaling_encoding: common::SignalingEncoding::Gb18030,
    }
}

/// 构造平台发来的请求(带最小必需头)。
fn platform_request(
    method: Method,
    uri: &str,
    call_id: &str,
    cseq: u32,
    platform_addr: SocketAddr,
    content_type: Option<&str>,
    body: &[u8],
) -> Request {
    let mut h = Headers::new();
    h.set(
        "Via",
        format!(
            "SIP/2.0/UDP {}:{};branch=z9hG4bK{}{}",
            platform_addr.ip(),
            platform_addr.port(),
            call_id,
            cseq
        ),
    );
    h.set(
        "From",
        format!("<sip:{PLATFORM_ID}@{PLATFORM_ID}>;tag=ptag"),
    );
    h.set("To", format!("<sip:{DEVICE_ID}@{PLATFORM_ID}>"));
    h.set("Call-ID", call_id);
    h.set("CSeq", format!("{cseq} {}", method.as_str()));
    h.set("Max-Forwards", "70");
    if let Some(ct) = content_type {
        h.set("Content-Type", ct);
    }
    Request {
        method,
        uri: uri.to_string(),
        headers: h,
        body: body.to_vec(),
    }
}

/// 起一台设备:只跑入站分派(不注册),便于单独验证 §9.12 的信令。
async fn spawn_device(
    device: &Arc<dyn SipTransport>,
    sim: &Arc<DeviceSimulator>,
) -> tokio::task::JoinHandle<()> {
    let device_addr = device.local_addr().unwrap();
    sim.set_local_addr_for_test(&device_addr.ip().to_string(), device_addr.port());
    let mut inbound = device.register_inbound_many(vec![DEVICE_ID.to_string()]);
    let sim = Arc::clone(sim);
    let tp = Arc::clone(device);
    tokio::spawn(async move {
        while let Some(inc) = inbound.recv().await {
            if let Err(error) = sim.answer_inbound(&tp, &inc).await {
                eprintln!("入站处理失败: {error}");
            }
        }
    })
}

#[tokio::test]
async fn 语音广播_完整信令闭环并真的收到音频() {
    let platform: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let device: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform.local_addr().unwrap();
    let device_addr = device.local_addr().unwrap();

    let sim = Arc::new(DeviceSimulator::new(cfg(
        "127.0.0.1",
        platform_addr.port(),
        None,
    )));
    let _served = spawn_device(&device, &sim).await;

    // 平台侧收设备发来的东西:Broadcast Response(MESSAGE)与 INVITE 都发到平台域 AOR。
    let mut inbound = platform.register_inbound(PLATFORM_ID);

    // ── 信令 1:平台下发语音广播通知 ────────────────────────────────
    let notify_xml = format!(
        "<?xml version=\"1.0\"?><Notify><CmdType>Broadcast</CmdType><SN>9</SN>\
<SourceID>{PLATFORM_ID}</SourceID><TargetID>{DEVICE_ID}</TargetID></Notify>"
    );
    let req = platform_request(
        Method::Message,
        &format!("sip:{DEVICE_ID}@{PLATFORM_ID}"),
        "bcast-call-1",
        1,
        platform_addr,
        Some("Application/MANSCDP+xml"),
        notify_xml.as_bytes(),
    );
    platform
        .send_to(&SipMessage::Request(req), device_addr)
        .await
        .unwrap();

    // ── 信令 3:收设备的 Broadcast Response,并回 200(信令 4)────────
    let response_msg = tokio::time::timeout(Duration::from_secs(3), inbound.recv())
        .await
        .expect("应在 3s 内收到 Broadcast Response")
        .expect("channel open");
    let SipMessage::Request(msg) = response_msg.message else {
        panic!("期望 MESSAGE 请求");
    };
    assert_eq!(msg.method, Method::Message);
    let body = String::from_utf8_lossy(&msg.body);
    // GB18030 编码下中文会乱,但 XML 标签是 ASCII,足以判定。
    assert!(
        body.contains("<CmdType>Broadcast</CmdType>"),
        "应是广播应答:{body}"
    );
    assert!(
        body.contains("<Result>OK</Result>"),
        "TargetID 属本设备应回 OK"
    );
    platform
        .send_to(
            &SipMessage::Response(sip_core::Response {
                status: 200,
                reason: "OK".into(),
                headers: msg.headers.clone(),
                body: Vec::new(),
            }),
            response_msg.from,
        )
        .await
        .unwrap();

    // ── 信令 5:设备主动发 INVITE ───────────────────────────────────
    let invite_msg = tokio::time::timeout(Duration::from_secs(3), inbound.recv())
        .await
        .expect("应在 3s 内收到设备的 INVITE")
        .expect("channel open");
    let SipMessage::Request(invite) = invite_msg.message else {
        panic!("期望 INVITE 请求");
    };
    assert_eq!(invite.method, Method::Invite);

    // 附录 L:必须携带 Subject,且媒体流发送者是平台、接收者是设备。
    let subject_raw = invite
        .headers
        .get("Subject")
        .expect("INVITE 应带 Subject 头");
    let subject = sip_core::Subject::parse(subject_raw).expect("Subject 格式应合法");
    assert_eq!(subject.sender_id, PLATFORM_ID, "发送者应是语音流发送者");
    assert_eq!(subject.receiver_id, DEVICE_ID, "接收者应是本设备");
    assert!(
        subject.sender_seq.starts_with('0'),
        "实时流序列号首位应为 0:{}",
        subject.sender_seq
    );

    // §9.12.1.3.2:s=Play、m=audio、a=recvonly、f=v/////a/1/8/1,端口必须是真实端口。
    let offer_text = String::from_utf8(invite.body.clone()).expect("SDP 应是 UTF-8");
    let offer = sip_core::SessionDescription::parse(&offer_text).expect("SDP 应可解析");
    assert_eq!(offer.session_name, "Play");
    assert!(offer.is_audio_only(), "广播 offer 应只有 m=audio");
    assert_eq!(offer.direction, Some(sip_core::Direction::RecvOnly));
    assert_eq!(offer.format, Some(sip_core::MediaFormat::g711_audio()));
    let recv_port = offer.media.port;
    assert_ne!(recv_port, 0, "SDP 必须写真实收流端口,不能是 0");

    // ── 信令 14:平台回 200 OK(带媒体服务器的 SDP)────────────────
    let mut ok_headers = invite.headers.clone();
    ok_headers.set(
        "To",
        format!("<sip:{PLATFORM_ID}@{PLATFORM_ID}>;tag=srvtag"),
    );
    ok_headers.set("Content-Type", "application/sdp");
    let answer = format!(
        "v=0\r\no={PLATFORM_ID} 0 0 IN IP4 127.0.0.1\r\ns=Play\r\nc=IN IP4 127.0.0.1\r\n\
t=0 0\r\nm=audio 6000 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\na=sendonly\r\ny=0100000001\r\n\
f=v/////a/1/8/1\r\n"
    );
    platform
        .send_to(
            &SipMessage::Response(sip_core::Response {
                status: 200,
                reason: "OK".into(),
                headers: ok_headers,
                body: answer.into_bytes(),
            }),
            invite_msg.from,
        )
        .await
        .unwrap();

    // ── 信令 15:设备应发 ACK,且必须带上平台的 To-tag ─────────────
    let ack_msg = tokio::time::timeout(Duration::from_secs(3), inbound.recv())
        .await
        .expect("应在 3s 内收到 ACK")
        .expect("channel open");
    let SipMessage::Request(ack) = ack_msg.message else {
        panic!("期望 ACK 请求");
    };
    assert_eq!(ack.method, Method::Ack);
    assert_eq!(
        ack.headers.to_tag(),
        Some("srvtag"),
        "ACK 的 To 必须带平台在 200 OK 里给的 tag"
    );
    assert_eq!(
        ack.headers.get("Call-ID"),
        invite.headers.get("Call-ID"),
        "ACK 应在 INVITE 的对话内"
    );

    // ── 媒体:平台向设备 SDP 声明的端口推 G.711A ───────────────────
    let media = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let target = SocketAddr::new(device_addr.ip(), recv_port);
    for seq in 0..5u16 {
        let mut pkt = vec![0x80, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        pkt[2..4].copy_from_slice(&seq.to_be_bytes());
        pkt.extend_from_slice(&[0xAA; 160]); // 满幅 A-law,电平应显著非零
        media.send_to(&pkt, target).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // 设备侧应统计到收流(含电平),证明这条链路真的通了。
    let mut snapshot = None;
    for _ in 0..50 {
        if let Some(s) = sim.broadcast_snapshot().await {
            if s.recv.packets >= 5 {
                snapshot = Some(s);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let snapshot = snapshot.expect("设备应统计到广播收流");
    assert_eq!(snapshot.source_id, PLATFORM_ID);
    assert_eq!(snapshot.local_port, recv_port);
    assert_eq!(snapshot.recv.packets, 5);
    assert_eq!(snapshot.recv.payload_bytes, 5 * 160);
    assert_eq!(snapshot.recv.lost, 0);
    assert_eq!(snapshot.recv.payload_type, Some(8));
    assert!(
        snapshot.recv.level_pct > 50,
        "满幅音频的电平应显著非零,实际 {}",
        snapshot.recv.level_pct
    );

    // ── 信令 17:平台发 BYE 拆链路,设备应回 200 并停收流 ──────────
    let call_id = invite.headers.get("Call-ID").unwrap().to_string();
    let mut bye = platform_request(
        Method::Bye,
        &format!("sip:{DEVICE_ID}@{PLATFORM_ID}"),
        &call_id,
        2,
        platform_addr,
        None,
        &[],
    );
    bye.headers.set("Call-ID", call_id.clone());
    platform
        .send_to(&SipMessage::Request(bye), device_addr)
        .await
        .unwrap();

    let mut closed = false;
    for _ in 0..50 {
        if sim.broadcast_snapshot().await.is_none() {
            closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(closed, "平台 BYE 后广播会话应被拆除");
}

#[tokio::test]
async fn 语音广播_目标不属本设备时不发_invite() {
    let platform: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let device: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform.local_addr().unwrap();
    let device_addr = device.local_addr().unwrap();

    let sim = Arc::new(DeviceSimulator::new(cfg(
        "127.0.0.1",
        platform_addr.port(),
        None,
    )));
    let _served = spawn_device(&device, &sim).await;
    let mut inbound = platform.register_inbound(PLATFORM_ID);

    let notify_xml = format!(
        "<?xml version=\"1.0\"?><Notify><CmdType>Broadcast</CmdType><SN>9</SN>\
<SourceID>{PLATFORM_ID}</SourceID><TargetID>34020000001320009999</TargetID></Notify>"
    );
    let req = platform_request(
        Method::Message,
        &format!("sip:{DEVICE_ID}@{PLATFORM_ID}"),
        "bcast-call-2",
        1,
        platform_addr,
        Some("Application/MANSCDP+xml"),
        notify_xml.as_bytes(),
    );
    platform
        .send_to(&SipMessage::Request(req), device_addr)
        .await
        .unwrap();

    // 应收到 ERROR 的广播应答。
    let msg = tokio::time::timeout(Duration::from_secs(3), inbound.recv())
        .await
        .expect("应收到广播应答")
        .expect("channel open");
    let SipMessage::Request(msg) = msg.message else {
        panic!("期望 MESSAGE");
    };
    let body = String::from_utf8_lossy(&msg.body);
    assert!(
        body.contains("<Result>ERROR</Result>"),
        "非本设备应回 ERROR"
    );

    // 且**不应**再发 INVITE。
    let follow = tokio::time::timeout(Duration::from_millis(400), inbound.recv()).await;
    assert!(follow.is_err(), "拒绝广播后不应发起 INVITE");
    assert!(sim.broadcast_snapshot().await.is_none());
}

#[tokio::test]
async fn 纯音频invite_按裸g711上行而非ps() {
    let platform: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let device: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform.local_addr().unwrap();
    let device_addr = device.local_addr().unwrap();

    let sim = Arc::new(DeviceSimulator::new(cfg(
        "127.0.0.1",
        platform_addr.port(),
        None,
    )));
    let _served = spawn_device(&device, &sim).await;
    let mut responses = platform.register("talk-call-1");

    // 平台侧的音频收流端口。
    let media = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let media_port = media.local_addr().unwrap().port();

    // §9.12.2 a):平台只请求 m=audio(语音对讲上行)。
    let offer = format!(
        "v=0\r\no={PLATFORM_ID} 0 0 IN IP4 127.0.0.1\r\ns=Play\r\nc=IN IP4 127.0.0.1\r\n\
t=0 0\r\nm=audio {media_port} RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\na=recvonly\r\n\
y=0100000002\r\nf=v/////a/1/8/1\r\n"
    );
    let invite = platform_request(
        Method::Invite,
        &format!("sip:{DEVICE_ID}@{PLATFORM_ID}"),
        "talk-call-1",
        1,
        platform_addr,
        Some("application/sdp"),
        offer.as_bytes(),
    );
    platform
        .send_to(&SipMessage::Request(invite), device_addr)
        .await
        .unwrap();

    // 设备应回 200 OK,SDP 为纯音频 + PCMA。
    let mut answer = None;
    for _ in 0..10 {
        let msg = tokio::time::timeout(Duration::from_secs(3), responses.recv())
            .await
            .expect("应收到应答")
            .expect("channel open");
        if let SipMessage::Response(resp) = msg.message {
            if resp.status == 200 {
                answer = Some(resp);
                break;
            }
            // 100 Trying 先到,继续等 200。
        }
    }
    let answer = answer.expect("应收到 200 OK");
    let sdp_text = String::from_utf8(answer.body.clone()).expect("SDP UTF-8");
    let sdp = sip_core::SessionDescription::parse(&sdp_text).expect("SDP 可解析");
    assert!(sdp.is_audio_only(), "对讲应答应是纯音频:{sdp_text}");
    assert!(
        sdp.media.rtpmaps.iter().any(|r| r.contains("PCMA/8000")),
        "应声明 PCMA/8000:{sdp_text}"
    );
    assert_eq!(sdp.direction, Some(sip_core::Direction::SendOnly));
    assert_eq!(sdp.format, Some(sip_core::MediaFormat::g711_audio()));

    // 平台侧应收到**裸 G.711**(PT=8、160 字节载荷),而不是 PS(PT=96)。
    let mut buf = [0u8; 512];
    let n = tokio::time::timeout(Duration::from_secs(3), media.recv(&mut buf))
        .await
        .expect("应在 3s 内收到上行音频")
        .expect("recv");
    assert_eq!(n, 12 + 160, "应是 12 字节 RTP 头 + 160 字节 G.711");
    assert_eq!(buf[1] & 0x7F, 8, "负载类型应为 8(PCMA),不是 96(PS)");
}
