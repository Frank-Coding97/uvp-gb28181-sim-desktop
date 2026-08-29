//! 附录 K(媒体流保活机制)与附录 L(Subject 头域)的回归,
//! 外加 A.2.3.2.1 设备配置修改真实生效的回归。
//!
//! 这几条都是"平台侧能观察到、但设备侧以前默默不干活"的行为:
//! - 附录 K c):同一媒体源的重新请求要**释放旧链路并重建**。旧实现一律回 486,
//!   平台重启后那个通道就永久拉不到流了。
//! - A.2.3.2.1:平台改设备名/心跳周期后,再查 ConfigDownload 必须读回新值。
//!   旧实现只打一行日志就回 OK,平台以为改成功了,其实什么都没变。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use common::{DeviceId, GbVersion, Transport};
use gb28181_simulator::{DeviceConfig, DeviceInfo, DeviceSimulator};
use sip_core::{Headers, Method, Request, SipMessage, SipTransport, UdpTransport};

const DEVICE_ID: &str = "34020000001320000001";
const PLATFORM_ID: &str = "34020000002000000001";

fn cfg(server_port: u16) -> DeviceConfig {
    DeviceConfig {
        device_id: DeviceId::new(DEVICE_ID).unwrap(),
        username: DEVICE_ID.into(),
        password: "12345678".into(),
        server_host: "127.0.0.1".into(),
        server_port,
        server_domain: PLATFORM_ID.into(),
        transport: Transport::Udp,
        heartbeat_interval_secs: 60,
        register_expires_secs: 3600,
        register_retry_max_secs: 60,
        channels: vec![],
        device_info: DeviceInfo {
            device_name: "原始设备名".into(),
            manufacturer: "UVP".into(),
            model: "Sim".into(),
            firmware: "0.1".into(),
        },
        video_source: None,
        video_fps: 25,
        // 轻量伪流:让 INVITE 真的建起一路会话(否则没有源就不会占用会话槽位)。
        light_bitrate_kbps: Some(128),
        gb_version: GbVersion::V2022,
        signaling_encoding: common::SignalingEncoding::Gb18030,
    }
}

#[allow(clippy::too_many_arguments)]
fn platform_request(
    method: Method,
    call_id: &str,
    cseq: u32,
    branch: &str,
    platform_addr: SocketAddr,
    content_type: Option<&str>,
    subject: Option<&str>,
    body: &[u8],
) -> Request {
    let mut h = Headers::new();
    h.set(
        "Via",
        format!(
            "SIP/2.0/UDP {}:{};branch={branch}",
            platform_addr.ip(),
            platform_addr.port()
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
    if let Some(s) = subject {
        h.set("Subject", s);
    }
    Request {
        method,
        uri: format!("sip:{DEVICE_ID}@{PLATFORM_ID}"),
        headers: h,
        body: body.to_vec(),
    }
}

/// 平台侧收流端口的 SDP offer。
fn video_offer(port: u16) -> String {
    format!(
        "v=0\r\no={PLATFORM_ID} 0 0 IN IP4 127.0.0.1\r\ns=Play\r\nc=IN IP4 127.0.0.1\r\n\
t=0 0\r\nm=video {port} RTP/AVP 96\r\na=rtpmap:96 PS/90000\r\na=recvonly\r\ny=0100000001\r\n"
    )
}

/// 发一条请求给设备,收集最终响应(跳过 1xx)。
async fn send_and_wait_final(
    platform: &Arc<dyn SipTransport>,
    device_addr: SocketAddr,
    req: Request,
    call_id: &str,
) -> sip_core::Response {
    let mut rx = platform.register(call_id);
    platform
        .send_to(&SipMessage::Request(req), device_addr)
        .await
        .unwrap();
    for _ in 0..10 {
        let inc = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("应在 3s 内收到响应")
            .expect("channel open");
        if let SipMessage::Response(resp) = inc.message {
            if resp.status >= 200 {
                return resp;
            }
        }
    }
    panic!("未收到最终响应");
}

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
async fn 附录k_同媒体源重新点播应重建而非回486() {
    let platform: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let device: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform.local_addr().unwrap();
    let device_addr = device.local_addr().unwrap();

    let sim = Arc::new(DeviceSimulator::new(cfg(platform_addr.port())));
    let _served = spawn_device(&device, &sim).await;

    let media = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let media_port = media.local_addr().unwrap().port();
    let subject_a = format!("{DEVICE_ID}:0000000001,{PLATFORM_ID}:0000000001");

    // 第一次点播:正常建流。
    let resp = send_and_wait_final(
        &platform,
        device_addr,
        platform_request(
            Method::Invite,
            "kc-call-1",
            1,
            "z9hG4bK-kc-1",
            platform_addr,
            Some("application/sdp"),
            Some(&subject_a),
            video_offer(media_port).as_bytes(),
        ),
        "kc-call-1",
    )
    .await;
    assert_eq!(resp.status, 200, "首次点播应 200 OK");

    // 第二次:**同一媒体源标识**的重新请求(平台重启/丢了 200 后重来)。
    // 附录 K c) 要求释放旧链路并按新请求重建 —— 必须还是 200,而不是 486。
    let resp = send_and_wait_final(
        &platform,
        device_addr,
        platform_request(
            Method::Invite,
            "kc-call-2",
            2,
            "z9hG4bK-kc-2",
            platform_addr,
            Some("application/sdp"),
            Some(&subject_a),
            video_offer(media_port).as_bytes(),
        ),
        "kc-call-2",
    )
    .await;
    assert_eq!(
        resp.status, 200,
        "同一媒体源的重新请求应重建链路(附录 K c)),而不是回 {} ",
        resp.status
    );

    // 第三次:**另一个媒体源**(发送方序列号不同)。模拟器同时只维持一路,按 SIP 回 486。
    let subject_b = format!("{DEVICE_ID}:0000000009,{PLATFORM_ID}:0000000001");
    let resp = send_and_wait_final(
        &platform,
        device_addr,
        platform_request(
            Method::Invite,
            "kc-call-3",
            3,
            "z9hG4bK-kc-3",
            platform_addr,
            Some("application/sdp"),
            Some(&subject_b),
            video_offer(media_port).as_bytes(),
        ),
        "kc-call-3",
    )
    .await;
    assert_eq!(resp.status, 486, "不同媒体源且已有会话时应回 486 Busy Here");
}

#[tokio::test]
async fn 设备配置修改_basicparam_真实生效并可查回() {
    let platform: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let device: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform.local_addr().unwrap();
    let device_addr = device.local_addr().unwrap();

    let sim = Arc::new(DeviceSimulator::new(cfg(platform_addr.port())));
    let _served = spawn_device(&device, &sim).await;
    // 设备的应答 MESSAGE 发往平台域 AOR。
    let mut inbound = platform.register_inbound(PLATFORM_ID);

    // ── 改配置:A.2.3.2.1 DeviceConfig / BasicParam ────────────────
    let config_xml = format!(
        "<?xml version=\"1.0\"?><Control><CmdType>DeviceConfig</CmdType><SN>1</SN>\
<DeviceID>{DEVICE_ID}</DeviceID><BasicParam><Name>NewName</Name>\
<Expiration>1800</Expiration><HeartBeatInterval>15</HeartBeatInterval>\
<HeartBeatCount>5</HeartBeatCount></BasicParam></Control>"
    );
    platform
        .send_to(
            &SipMessage::Request(platform_request(
                Method::Message,
                "cfg-call-1",
                1,
                "z9hG4bK-cfg-1",
                platform_addr,
                Some("Application/MANSCDP+xml"),
                None,
                config_xml.as_bytes(),
            )),
            device_addr,
        )
        .await
        .unwrap();

    // 设备会回一条 DeviceConfig 应答 MESSAGE。
    let msg = tokio::time::timeout(Duration::from_secs(3), inbound.recv())
        .await
        .expect("应收到配置应答")
        .expect("channel open");
    let SipMessage::Request(msg) = msg.message else {
        panic!("期望 MESSAGE");
    };
    let body = String::from_utf8_lossy(&msg.body);
    assert!(
        body.contains("<CmdType>DeviceConfig</CmdType>"),
        "A.2.6.8:应答 CmdType 应为 DeviceConfig:{body}"
    );

    // ── 查配置:改过的值必须读回来 ────────────────────────────────
    let query_xml = format!(
        "<?xml version=\"1.0\"?><Query><CmdType>ConfigDownload</CmdType><SN>2</SN>\
<DeviceID>{DEVICE_ID}</DeviceID><ConfigType>BasicParam</ConfigType></Query>"
    );
    platform
        .send_to(
            &SipMessage::Request(platform_request(
                Method::Message,
                "cfg-call-2",
                2,
                "z9hG4bK-cfg-2",
                platform_addr,
                Some("Application/MANSCDP+xml"),
                None,
                query_xml.as_bytes(),
            )),
            device_addr,
        )
        .await
        .unwrap();

    let msg = tokio::time::timeout(Duration::from_secs(3), inbound.recv())
        .await
        .expect("应收到配置查询应答")
        .expect("channel open");
    let SipMessage::Request(msg) = msg.message else {
        panic!("期望 MESSAGE");
    };
    let body = String::from_utf8_lossy(&msg.body);
    assert!(
        body.contains("<CmdType>ConfigDownload</CmdType>"),
        "应是配置查询应答:{body}"
    );
    assert!(body.contains("<Name>NewName</Name>"), "设备名应已改:{body}");
    assert!(
        body.contains("<Expiration>1800</Expiration>"),
        "注册有效期应已改:{body}"
    );
    assert!(
        body.contains("<HeartBeatInterval>15</HeartBeatInterval>"),
        "心跳周期应已改:{body}"
    );
    assert!(
        body.contains("<HeartBeatCount>5</HeartBeatCount>"),
        "心跳超时次数应已改:{body}"
    );

    // ── DeviceInfo 也应读到新名字 ─────────────────────────────────
    let info_xml = format!(
        "<?xml version=\"1.0\"?><Query><CmdType>DeviceInfo</CmdType><SN>3</SN>\
<DeviceID>{DEVICE_ID}</DeviceID></Query>"
    );
    platform
        .send_to(
            &SipMessage::Request(platform_request(
                Method::Message,
                "cfg-call-3",
                3,
                "z9hG4bK-cfg-3",
                platform_addr,
                Some("Application/MANSCDP+xml"),
                None,
                info_xml.as_bytes(),
            )),
            device_addr,
        )
        .await
        .unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(3), inbound.recv())
        .await
        .expect("应收到 DeviceInfo 应答")
        .expect("channel open");
    let SipMessage::Request(msg) = msg.message else {
        panic!("期望 MESSAGE");
    };
    let body = String::from_utf8_lossy(&msg.body);
    assert!(
        body.contains("<DeviceName>NewName</DeviceName>"),
        "DeviceInfo 应读到新设备名:{body}"
    );
}

#[tokio::test]
async fn 设备配置修改_空字段不覆盖原值() {
    let platform: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let device: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform.local_addr().unwrap();
    let device_addr = device.local_addr().unwrap();

    let sim = Arc::new(DeviceSimulator::new(cfg(platform_addr.port())));
    let _served = spawn_device(&device, &sim).await;
    let mut inbound = platform.register_inbound(PLATFORM_ID);

    // A.2.3.2.1:配置修改可以只带部分字段。只改心跳周期,设备名不该被清空。
    let config_xml = format!(
        "<?xml version=\"1.0\"?><Control><CmdType>DeviceConfig</CmdType><SN>1</SN>\
<DeviceID>{DEVICE_ID}</DeviceID><BasicParam><HeartBeatInterval>30</HeartBeatInterval>\
</BasicParam></Control>"
    );
    platform
        .send_to(
            &SipMessage::Request(platform_request(
                Method::Message,
                "cfg2-call-1",
                1,
                "z9hG4bK-cfg2-1",
                platform_addr,
                Some("Application/MANSCDP+xml"),
                None,
                config_xml.as_bytes(),
            )),
            device_addr,
        )
        .await
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(3), inbound.recv())
        .await
        .expect("应收到配置应答");

    let query_xml = format!(
        "<?xml version=\"1.0\"?><Query><CmdType>ConfigDownload</CmdType><SN>2</SN>\
<DeviceID>{DEVICE_ID}</DeviceID><ConfigType>BasicParam</ConfigType></Query>"
    );
    platform
        .send_to(
            &SipMessage::Request(platform_request(
                Method::Message,
                "cfg2-call-2",
                2,
                "z9hG4bK-cfg2-2",
                platform_addr,
                Some("Application/MANSCDP+xml"),
                None,
                query_xml.as_bytes(),
            )),
            device_addr,
        )
        .await
        .unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(3), inbound.recv())
        .await
        .expect("应收到配置查询应答")
        .expect("channel open");
    let SipMessage::Request(msg) = msg.message else {
        panic!("期望 MESSAGE");
    };
    let body = String::from_utf8_lossy(&msg.body);
    assert!(
        body.contains("<HeartBeatInterval>30</HeartBeatInterval>"),
        "心跳周期应已改:{body}"
    );
    // 设备名保持原值(GB18030 编码下中文会乱码,故只断言"没有变成空")。
    assert!(
        !body.contains("<Name></Name>") && !body.contains("<Name/>"),
        "未携带的字段不应被清空:{body}"
    );
    assert!(
        body.contains("<Expiration>3600</Expiration>"),
        "未携带的注册有效期应保持原值:{body}"
    );
}
