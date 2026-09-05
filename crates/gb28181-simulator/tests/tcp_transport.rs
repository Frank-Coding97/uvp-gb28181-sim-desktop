//! SIP over TCP 端到端回归。
//!
//! 锁定两件事:
//! 1. 选 TCP 时**真的走 TCP**,且 Via 里的传输标识与实际通道一致
//!    (旧实现只有 UDP,却把 `SIP/2.0/TCP` 写进 Via,平台按 Via 回包就走错路径)。
//! 2. TCP 上大 body(整棵目录树)不需要分片也能完整送达 —— 这正是目录很大时
//!    应该选 TCP 的理由。

use std::sync::Arc;
use std::time::Duration;

use common::{DeviceId, GbVersion, Transport};
use gb28181_simulator::{ChannelConfig, DeviceConfig, DeviceInfo, DeviceSimulator};
use sip_core::{Headers, Method, Response, SipMessage, SipTransport, TcpTransport};

fn tcp_cfg(server_host: &str, server_port: u16, channels: usize) -> DeviceConfig {
    let channels = (0..channels)
        .map(|i| ChannelConfig {
            channel_id: DeviceId::new(format!("3402000000132000{:04}", 1000 + i)).unwrap(),
            name: format!("通道-{i}"),
            status: "ON".into(),
        })
        .collect();
    DeviceConfig {
        device_id: DeviceId::new("34020000001320000001").unwrap(),
        username: "34020000001320000001".into(),
        password: "12345678".into(),
        server_host: server_host.into(),
        server_port,
        server_domain: "34020000002000000001".into(),
        inbound_identity_mode: common::InboundIdentityMode::Compatible,
        transport: Transport::Tcp,
        heartbeat_interval_secs: 3600,
        register_expires_secs: 600,
        register_retry_max_secs: 60,
        channels,
        device_info: DeviceInfo {
            device_name: "TCP-Sim".into(),
            manufacturer: "UVP".into(),
            model: "Sim".into(),
            firmware: "0.1".into(),
        },
        video_source: None,
        video_fps: 25,
        light_bitrate_kbps: None,
        gb_version: GbVersion::V2022,
        signaling_encoding: common::SignalingEncoding::Gb18030,
    }
}

#[tokio::test]
async fn tcp_注册闭环_via_标识为_tcp_且授予有效期被采纳() {
    let platform: Arc<dyn SipTransport> = TcpTransport::bind("127.0.0.1:0").await.unwrap();
    let device: Arc<dyn SipTransport> = TcpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform.local_addr().unwrap();
    let device_addr = device.local_addr().unwrap();

    // mock 平台:对 REGISTER 直接回 200,并把 Expires 压到 120s(小于设备请求的 600s)。
    // 设备发出的 REGISTER/MESSAGE 的 Request-URI 指向平台域,故平台按平台域 AOR 收。
    let mut inbound = platform.register_inbound("34020000002000000001");
    let seen_via = Arc::new(parking_lot::Mutex::new(String::new()));
    let seen_via_task = Arc::clone(&seen_via);
    let platform_task = Arc::clone(&platform);
    tokio::spawn(async move {
        while let Some(inc) = inbound.recv().await {
            if let SipMessage::Request(req) = &inc.message {
                if req.method != Method::Register {
                    continue;
                }
                *seen_via_task.lock() = req.headers.get("Via").unwrap_or_default().to_string();
                let mut h = Headers::new();
                for via in req.headers.get_all("Via") {
                    h.append("Via", via.to_string());
                }
                h.set("From", req.headers.get("From").unwrap_or_default());
                h.set("To", req.headers.get("To").unwrap_or_default());
                h.set("Call-ID", req.headers.call_id().unwrap_or_default());
                h.set("CSeq", req.headers.cseq().unwrap_or_default());
                // 平台只授予 120 秒 —— 设备的续约周期必须以此为准。
                h.set("Expires", "120");
                let resp = SipMessage::Response(Response {
                    status: 200,
                    reason: "OK".into(),
                    headers: h,
                    body: Vec::new(),
                });
                let _ = platform_task.send_to(&resp, inc.from).await;
            }
        }
    });

    let sim = Arc::new(DeviceSimulator::new(tcp_cfg(
        &platform_addr.ip().to_string(),
        platform_addr.port(),
        1,
    )));
    let state = tokio::time::timeout(
        Duration::from_secs(5),
        sim.register(&device, &device_addr.ip().to_string(), device_addr.port()),
    )
    .await
    .expect("TCP 注册应在超时内完成")
    .expect("TCP 注册应成功");
    assert_eq!(state, gb28181_simulator::DeviceState::Registered);

    // Via 的传输标识必须与实际通道一致。
    let via = seen_via.lock().clone();
    assert!(
        via.contains("SIP/2.0/TCP"),
        "走 TCP 时 Via 必须写 TCP,实际:{via}"
    );
    // 平台授予 120s → 设备记住 120s(续约按其 80% 而非硬编码 2880s)。
    assert_eq!(
        sim.granted_expires(),
        120,
        "必须采纳平台授予的 Expires,而不是继续用请求值"
    );
    assert_eq!(device.protocol(), Transport::Tcp);
}

#[tokio::test]
async fn tcp_大目录应答不分片也能完整送达() {
    let platform: Arc<dyn SipTransport> = TcpTransport::bind("127.0.0.1:0").await.unwrap();
    let device: Arc<dyn SipTransport> = TcpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform.local_addr().unwrap();
    let device_addr = device.local_addr().unwrap();

    // 40 通道:UDP 下这条目录应答必然超过安全 MTU 需要分多条,TCP 下一条也能发完。
    let sim = Arc::new(DeviceSimulator::new(tcp_cfg(
        &platform_addr.ip().to_string(),
        platform_addr.port(),
        8,
    )));
    sim.set_local_addr_for_test(&device_addr.ip().to_string(), device_addr.port());

    let mut platform_inbound = platform.register_inbound("34020000002000000001");
    let mut device_inbound = device.register_inbound("34020000001320000001");

    // 平台侧发目录查询。
    let query = "<?xml version=\"1.0\"?><Query><CmdType>Catalog</CmdType><SN>1</SN>\
<DeviceID>34020000001320000001</DeviceID></Query>";
    let mut h = Headers::new();
    h.set("Call-ID", "catalog-tcp");
    h.set("CSeq", "1 MESSAGE");
    h.set("Via", "SIP/2.0/TCP 127.0.0.1:5060;branch=z9hG4bK-cat");
    h.set(
        "From",
        "<sip:34020000002000000001@34020000002000000001>;tag=p",
    );
    h.set("To", "<sip:34020000001320000001@34020000002000000001>");
    h.set("Content-Type", "Application/MANSCDP+xml");
    let req = SipMessage::Request(sip_core::Request {
        method: Method::Message,
        uri: "sip:34020000001320000001@127.0.0.1".into(),
        headers: h,
        body: query.as_bytes().to_vec(),
    });
    platform.send_to(&req, device_addr).await.unwrap();

    // 设备侧收到查询 → 应答。
    let inc = tokio::time::timeout(Duration::from_secs(3), device_inbound.recv())
        .await
        .expect("设备应收到目录查询")
        .unwrap();
    sim.answer_inbound(&device, &inc).await.unwrap();

    // 平台侧应收到:空 200(事务应答)之后是携带目录 XML 的独立 MESSAGE。
    let mut catalog_bodies: Vec<String> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(300), platform_inbound.recv()).await {
            Ok(Some(got)) => {
                if let SipMessage::Request(r) = &got.message {
                    if !r.body.is_empty() {
                        let (text, _, _) = encoding_rs::GB18030.decode(&r.body);
                        catalog_bodies.push(text.into_owned());
                    }
                }
            }
            Ok(None) => break,
            // 收到目录后再等一小会儿确认没有后续分片。
            Err(_) if !catalog_bodies.is_empty() => break,
            Err(_) => {}
        }
    }
    assert!(!catalog_bodies.is_empty(), "平台应收到目录应答");
    let joined = catalog_bodies.join("");
    assert!(joined.contains("<SumNum>8</SumNum>"), "应答应含 SumNum");
    assert_eq!(
        catalog_bodies.len(),
        1,
        "TCP 是流式传输,不受 MTU 限制,目录应答不该被分片"
    );
    for i in 0..8 {
        let id = format!("3402000000132000{:04}", 1000 + i);
        assert!(joined.contains(&id), "通道 {id} 应出现在目录应答里");
    }
}
