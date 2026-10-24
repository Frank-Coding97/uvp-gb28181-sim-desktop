use super::snapshot::*;
use super::*;
use sip_core::{Headers, Method, Request, Response, SipMessage, UdpTransport};

/// 经真实 UDP 接收循环注入请求,而不是手工伪造 `Incoming`。
///
/// 这样单测同时覆盖来源限流与反放大字节信用；若直接构造 `Incoming`,随后发送的
/// Response 理应被传输层视为"没有对应入站请求"并阻断。
async fn deliver_udp_request(
    device: &Arc<dyn SipTransport>,
    platform: &Arc<UdpTransport>,
    request: Request,
) -> sip_core::Incoming {
    let aor = request
        .uri
        .strip_prefix("sip:")
        .and_then(|rest| rest.split(['@', ';', '>']).next())
        .filter(|value| !value.is_empty())
        .expect("测试请求 URI 应含 AOR")
        .to_string();
    let mut inbound = device.register_inbound(&aor);
    platform
        .send_to(
            &SipMessage::Request(request),
            device.local_addr().expect("设备 UDP 地址"),
        )
        .await
        .expect("注入平台请求");
    tokio::time::timeout(std::time::Duration::from_secs(2), inbound.recv())
        .await
        .expect("设备未收到测试请求")
        .expect("设备入站路由提前关闭")
}

fn change(id: &str, event: &str, status: &str) -> gb28181_protocol::manscdp::CatalogNotifyItem {
    gb28181_protocol::manscdp::CatalogNotifyItem {
        device_id: id.into(),
        name: format!("Ch-{id}"),
        event: event.into(),
        status: status.into(),
    }
}

#[tokio::test]
async fn 子任务超时后被真正取消而非_detach() {
    struct DropProbe(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let probe = DropProbe(Arc::clone(&dropped));
    let task = tokio::spawn(async move {
        let _probe = probe;
        std::future::pending::<()>().await;
    });
    assert!(
        !await_task_with_timeout("取消测试", task, Duration::from_millis(20)).await,
        "超时路径应报告非正常完成"
    );
    assert!(
        dropped.load(std::sync::atomic::Ordering::Acquire),
        "abort 后 future 必须被 drop,资源所有权链才能收敛"
    );
}

#[test]
fn 附录n_同一通道的变更只保留最新一条() {
    let mut pending = PendingCatalogChanges::default();
    pending.merge(vec![change("1", "ADD", "ON")]);
    pending.merge(vec![change("1", "OFF", "OFF")]);
    pending.merge(vec![change("1", "ON", "ON")]);
    assert_eq!(
        pending.items.len(),
        1,
        "上线→下线→上线三条推给平台只是噪声,平台关心终态"
    );
    assert_eq!(pending.items[0].event, "ON");
    // 不同通道各占一条,且保持发生顺序。
    pending.merge(vec![change("2", "ADD", "ON"), change("3", "ADD", "ON")]);
    let ids: Vec<&str> = pending.items.iter().map(|c| c.device_id.as_str()).collect();
    assert_eq!(ids, vec!["1", "2", "3"]);
}

#[test]
fn 附录n_攒够条数立刻发出() {
    let mut pending = PendingCatalogChanges::default();
    // 空缓冲永远不发。
    assert!(!pending.should_flush());
    let batch: Vec<_> = (0..CATALOG_NOTIFY_BATCH_ITEMS - 1)
        .map(|i| change(&i.to_string(), "ADD", "ON"))
        .collect();
    pending.merge(batch);
    assert!(
        !pending.should_flush(),
        "差一条到阈值,且去抖窗口未过,应继续攒"
    );
    pending.merge(vec![change("999", "ADD", "ON")]);
    assert_eq!(pending.items.len(), CATALOG_NOTIFY_BATCH_ITEMS);
    assert!(pending.should_flush(), "到 100 条就该立刻通知");
}

#[test]
fn 附录n_去抖窗口过后发出并清空() {
    let mut pending = PendingCatalogChanges::default();
    pending.merge(vec![change("1", "UPDATE", "ON")]);
    assert!(!pending.should_flush());
    // 把"最后一条变更"的时刻往前拨到去抖窗口之外。
    pending.last = Some(std::time::Instant::now() - CATALOG_NOTIFY_DEBOUNCE);
    assert!(pending.should_flush());
    let taken = pending.take();
    assert_eq!(taken.len(), 1);
    assert!(pending.items.is_empty());
    assert!(pending.since.is_none() && pending.last.is_none());
    assert!(
        !pending.flush_scheduled,
        "取走后要允许下一批变更重新安排延迟任务"
    );
    assert!(!pending.should_flush());
}

#[test]
fn 附录n_变更不断时靠硬上限兜底() {
    let mut pending = PendingCatalogChanges::default();
    pending.merge(vec![change("1", "ADD", "ON")]);
    // 模拟"变更持续不断":第一条已等满上限,但去抖一直被最新变更重置。
    pending.since = Some(std::time::Instant::now() - CATALOG_NOTIFY_MAX_DELAY);
    pending.last = Some(std::time::Instant::now());
    assert!(pending.items.len() < CATALOG_NOTIFY_BATCH_ITEMS);
    assert!(pending.should_flush(), "去抖被反复重置也不能无限期攒着不发");
}

#[test]
fn 附录n_硬上限与附录n示例值一致() {
    assert_eq!(CATALOG_NOTIFY_BATCH_ITEMS, 100);
    assert_eq!(CATALOG_NOTIFY_MAX_DELAY, std::time::Duration::from_secs(60));
    assert!(
        CATALOG_NOTIFY_DEBOUNCE < CATALOG_NOTIFY_MAX_DELAY,
        "去抖窗口必须小于硬上限,否则上限永远先到"
    );
}

fn test_cfg(host: &str, port: u16) -> DeviceConfig {
    DeviceConfig {
        device_id: DeviceId::new("34020000001320000001").unwrap(),
        username: "34020000001320000001".into(),
        password: "12345678".into(),
        server_host: host.into(),
        server_port: port,
        server_domain: "34020000002000000001".into(),
        inbound_identity_mode: common::InboundIdentityMode::Compatible,
        transport: Transport::Udp,
        heartbeat_interval_secs: 60,
        channels: vec![],
        device_info: DeviceInfo {
            device_name: "Test Device".into(),
            manufacturer: "UVP".into(),
            model: "Sim".into(),
            firmware: "0.1.0".into(),
        },
        video_source: None,
        video_fps: 25,
        light_bitrate_kbps: None,
        register_expires_secs: DEFAULT_REGISTER_EXPIRES,
        register_retry_max_secs: DEFAULT_REGISTER_RETRY_MAX_SECS,
        gb_version: common::GbVersion::V2022,
        signaling_encoding: common::SignalingEncoding::Gb18030,
    }
}

fn inbound_request(method: Method, call_id: &str, from: Option<&str>) -> Request {
    let mut headers = Headers::new();
    headers.set(
        "Via",
        format!("SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bK-{call_id}"),
    );
    if let Some(from) = from {
        headers.set("From", from);
    }
    headers.set("To", "<sip:34020000001320000001@3402>");
    headers.set("Call-ID", call_id);
    headers.set("CSeq", format!("1 {}", method.as_str()));
    Request {
        method,
        uri: "sip:34020000001320000001@127.0.0.1".into(),
        headers,
        body: Vec::new(),
    }
}

async fn answer_and_status(
    sim: &Arc<DeviceSimulator>,
    device: &Arc<dyn SipTransport>,
    platform: &Arc<UdpTransport>,
    request: Request,
) -> Option<u16> {
    let call_id = request.headers.call_id().unwrap().to_string();
    let mut responses = platform.register(&call_id);
    let incoming = deliver_udp_request(device, platform, request).await;
    sim.answer_inbound(device, &incoming).await.unwrap();
    match tokio::time::timeout(Duration::from_millis(500), responses.recv()).await {
        Ok(Some(sip_core::Incoming {
            message: SipMessage::Response(response),
            ..
        })) => Some(response.status),
        _ => None,
    }
}

#[tokio::test]
async fn 入站身份模式严格校验_from_且_ack_静默拒绝() {
    let device_impl = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let device: Arc<dyn SipTransport> = device_impl;
    let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let mut cfg = test_cfg("127.0.0.1", platform.local_addr().unwrap().port());
    cfg.inbound_identity_mode = common::InboundIdentityMode::Strict;
    let sim = Arc::new(DeviceSimulator::new(cfg));

    assert_eq!(
        answer_and_status(
            &sim,
            &device,
            &platform,
            inbound_request(
                Method::Options,
                "strict-ok",
                Some("<sip:34020000002000000001@3402>;tag=p")
            ),
        )
        .await,
        Some(200)
    );
    assert_eq!(
        answer_and_status(
            &sim,
            &device,
            &platform,
            inbound_request(Method::Options, "strict-missing", None),
        )
        .await,
        Some(400)
    );
    assert_eq!(
        answer_and_status(
            &sim,
            &device,
            &platform,
            inbound_request(
                Method::Options,
                "strict-malformed",
                Some("Platform <sip:bad@3402")
            ),
        )
        .await,
        Some(400)
    );
    assert_eq!(
        answer_and_status(
            &sim,
            &device,
            &platform,
            inbound_request(
                Method::Options,
                "strict-wrong",
                Some("<sip:attacker@3402>;tag=p")
            ),
        )
        .await,
        Some(403)
    );
    assert_eq!(
        answer_and_status(
            &sim,
            &device,
            &platform,
            inbound_request(Method::Ack, "strict-ack", Some("<sip:attacker@3402>;tag=p")),
        )
        .await,
        None,
        "身份错误的 ACK 必须静默丢弃"
    );
}

#[tokio::test]
async fn 严格身份拒绝发生在控制命令副作用之前() {
    let device_impl = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let device: Arc<dyn SipTransport> = device_impl;
    let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let mut cfg = test_cfg("127.0.0.1", platform.local_addr().unwrap().port());
    cfg.inbound_identity_mode = common::InboundIdentityMode::Strict;
    let sim = Arc::new(DeviceSimulator::new(cfg));
    let mut request = inbound_request(
        Method::Message,
        "strict-no-side-effect",
        Some("<sip:attacker@3402>;tag=p"),
    );
    request.body = b"<Control><CmdType>DeviceControl</CmdType><SN>2</SN><DeviceID>34020000001320000001</DeviceID><RecordCmd>Record</RecordCmd></Control>".to_vec();

    assert_eq!(
        answer_and_status(&sim, &device, &platform, request).await,
        Some(403)
    );
    assert!(
        !sim.control_state.lock().recording,
        "错误身份 MESSAGE 不得修改设备控制状态"
    );
}

#[tokio::test]
async fn 入站身份兼容模式保持旧平台互操作() {
    let device_impl = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let device: Arc<dyn SipTransport> = device_impl;
    let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let sim = Arc::new(DeviceSimulator::new(test_cfg(
        "127.0.0.1",
        platform.local_addr().unwrap().port(),
    )));
    assert_eq!(
        answer_and_status(
            &sim,
            &device,
            &platform,
            inbound_request(Method::Options, "compatible-missing", None),
        )
        .await,
        Some(200)
    );
}

/// mock 平台:收到首个 REGISTER 回 401(带挑战),第二个回 200。
async fn mock_platform_register(
    platform: Arc<dyn SipTransport>,
    device_addr: SocketAddr,
    call_id: String,
) {
    let mut prx = platform.register(&call_id);
    let mut seen = 0;
    let mut first_via: Option<String> = None;
    let mut first_from: Option<String> = None;
    let mut first_cseq: Option<u32> = None;
    while let Some(inc) = prx.recv().await {
        if let SipMessage::Request(req) = inc.message {
            let cseq = req.headers.cseq().unwrap_or("1 REGISTER").to_string();
            let via = req
                .headers
                .get("Via")
                .expect("REGISTER 缺少 Via")
                .to_string();
            let from = req
                .headers
                .get("From")
                .expect("REGISTER 缺少 From")
                .to_string();
            let cseq_number = req.headers.cseq_number().expect("REGISTER 缺少 CSeq");
            let mut h = Headers::new();
            h.set("Call-ID", call_id.clone());
            h.set("CSeq", cseq);
            let resp = if seen == 0 {
                assert!(req.headers.get("Authorization").is_none());
                first_via = Some(via);
                first_from = Some(from);
                first_cseq = Some(cseq_number);
                h.set(
                    "WWW-Authenticate",
                    r#"Digest realm="3402000000", nonce="abc123""#,
                );
                Response {
                    status: 401,
                    reason: "Unauthorized".into(),
                    headers: h,
                    body: Vec::new(),
                }
            } else {
                assert!(req.headers.get("Authorization").is_some());
                assert_eq!(req.headers.get("Call-ID"), Some(call_id.as_str()));
                assert_eq!(first_from.as_deref(), Some(from.as_str()));
                assert_ne!(first_via.as_deref(), Some(via.as_str()));
                assert_eq!(Some(cseq_number), first_cseq.map(|value| value + 1));
                Response {
                    status: 200,
                    reason: "OK".into(),
                    headers: h,
                    body: Vec::new(),
                }
            };
            seen += 1;
            let _ = platform
                .send_to(&SipMessage::Response(resp), device_addr)
                .await;
            if seen == 2 {
                return;
            }
        }
    }
}

#[tokio::test]
async fn 注册闭环_401后带鉴权成功() {
    let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform_tp.local_addr().unwrap();
    let device_addr = device_tp.local_addr().unwrap();

    let sim = DeviceSimulator::new(test_cfg("127.0.0.1", platform_addr.port()));
    let platform_task = tokio::spawn(mock_platform_register(
        platform_tp.clone(),
        device_addr,
        sim.call_id().to_string(),
    ));

    let state = sim
        .register(&device_tp, "127.0.0.1", device_addr.port())
        .await
        .unwrap();
    assert_eq!(state, DeviceState::Registered);
    platform_task.await.unwrap();
}

#[tokio::test]
async fn options_回_200() {
    use sip_core::{Method, Request};

    let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform_tp.local_addr().unwrap();

    let sim = Arc::new(DeviceSimulator::new(test_cfg(
        "127.0.0.1",
        platform_addr.port(),
    )));

    // 平台构造发往设备 AOR 的 OPTIONS,并注册接收响应的队列。
    let mut h = Headers::new();
    h.set("Via", "SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bKopt");
    h.set("From", "<sip:34020000002000000001@3402>;tag=p1");
    h.set("To", "<sip:34020000001320000001@3402>");
    h.set("Call-ID", "opt-call-1");
    h.set("CSeq", "1 OPTIONS");
    let options = Request {
        method: Method::Options,
        uri: format!("sip:{}@127.0.0.1", sim.config().device_id),
        headers: h,
        body: Vec::new(),
    };
    let mut presp = platform_tp.register("opt-call-1");

    // 设备侧应答一条入站请求。
    let incoming = deliver_udp_request(&device_tp, &platform_tp, options).await;
    let answered = sim.answer_inbound(&device_tp, &incoming).await.unwrap();
    assert!(answered);

    // 平台应收到 200 OK(Call-ID 回显)。
    let got = tokio::time::timeout(std::time::Duration::from_secs(2), presp.recv())
        .await
        .expect("超时")
        .expect("关闭");
    match got.message {
        SipMessage::Response(r) => {
            assert_eq!(r.status, 200);
            assert_eq!(r.headers.call_id(), Some("opt-call-1"));
        }
        _ => panic!("应为响应"),
    }
}

#[tokio::test]
async fn 目录查询应答含通道() {
    use sip_core::Method;

    let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform_tp.local_addr().unwrap();

    let mut cfg = test_cfg("127.0.0.1", platform_addr.port());
    cfg.channels.push(ChannelConfig {
        channel_id: DeviceId::new("34020000001320000132").unwrap(),
        name: "Camera-1".into(),
        status: "ON".into(),
    });
    let sim = Arc::new(DeviceSimulator::new(cfg));

    let query_xml = r#"<?xml version="1.0"?>
<Query><CmdType>Catalog</CmdType><SN>999</SN><DeviceID>34020000001320000001</DeviceID></Query>"#;

    let mut h = Headers::new();
    h.set("Via", "SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bKcat");
    h.set("From", "<sip:34020000002000000001@3402>;tag=p2");
    h.set("To", "<sip:34020000001320000001@3402>");
    h.set("Call-ID", "cat-call-1");
    h.set("CSeq", "2 MESSAGE");
    h.set("Content-Type", "Application/MANSCDP+xml");
    let query_msg = sip_core::Request {
        method: Method::Message,
        uri: format!("sip:{}@127.0.0.1", sim.config().device_id),
        headers: h,
        body: query_xml.as_bytes().to_vec(),
    };
    let mut presp = platform_tp.register("cat-call-1");

    let incoming = deliver_udp_request(&device_tp, &platform_tp, query_msg).await;
    // 应答 MESSAGE 的 Request-URI 指向平台域,平台按 AOR 收独立应答。
    let mut preply = platform_tp.register_inbound("34020000002000000001");

    let answered = sim.answer_inbound(&device_tp, &incoming).await.unwrap();
    assert!(answered);

    // (1) 平台先收到对查询的空 200 OK(事务应答)。
    let ack = tokio::time::timeout(std::time::Duration::from_secs(2), presp.recv())
        .await
        .expect("超时")
        .expect("关闭");
    match ack.message {
        SipMessage::Response(r) => {
            assert_eq!(r.status, 200);
            assert!(r.body.is_empty(), "查询 200 OK 应为空 body");
        }
        _ => panic!("应先收到 200 响应"),
    }

    // (2) 平台再收到独立的 Catalog 应答 MESSAGE。
    let reply = tokio::time::timeout(std::time::Duration::from_secs(2), preply.recv())
        .await
        .expect("超时未收到应答 MESSAGE")
        .expect("关闭");
    match reply.message {
        SipMessage::Request(r) => {
            assert_eq!(r.method, Method::Message);
            let body_str = std::str::from_utf8(&r.body).unwrap();
            assert!(body_str.contains("<CmdType>Catalog</CmdType>"));
            assert!(body_str.contains("<SN>999</SN>"));
            assert!(body_str.contains("34020000001320000132")); // 通道 ID
            assert!(body_str.contains("<Name>Camera-1</Name>"));
        }
        _ => panic!("应为独立 MESSAGE 应答"),
    }
}

#[test]
fn 目录应答按gb版本差异化字段() {
    use gb28181_protocol::manscdp::Query;

    let query = Query {
        cmd_type: "Catalog".into(),
        sn: 1,
        device_id: "35020000001310000001".into(),
        interval: None,
        group_id: None,
        config_type: None,
        start_time: None,
        end_time: None,
        record_type: None,
        indistinct_query: None,
        stream_number: None,
        ..Default::default()
    };
    let ch = ChannelConfig {
        channel_id: DeviceId::new("35020000001310000132").unwrap(),
        name: "Camera-1".into(),
        status: "ON".into(),
    };

    // 2022:输出 SecurityLevelCode。
    let mut cfg = test_cfg("127.0.0.1", 5060);
    cfg.channels.push(ch.clone());
    cfg.gb_version = common::GbVersion::V2022;
    let xml = DeviceSimulator::new(cfg).handle_query(&query).unwrap();
    assert!(
        xml.contains("<SecurityLevelCode>"),
        "2022 应含 SecurityLevelCode"
    );

    // 2016:不输出该 2022 新增字段。
    let mut cfg16 = test_cfg("127.0.0.1", 5060);
    cfg16.channels.push(ch);
    cfg16.gb_version = common::GbVersion::V2016;
    let xml16 = DeviceSimulator::new(cfg16).handle_query(&query).unwrap();
    assert!(
        !xml16.contains("SecurityLevelCode"),
        "2016 不应含 2022 新增字段"
    );
}

#[test]
fn 目录项逐通道扩展字段生效() {
    use gb28181_protocol::id_codec::{CatalogNode, CatalogNodeType};
    use gb28181_protocol::manscdp::Query;

    let query = Query {
        cmd_type: "Catalog".into(),
        sn: 1,
        device_id: "34020000001320000001".into(),
        ..Default::default()
    };
    let mut cfg = test_cfg("127.0.0.1", 5060);
    cfg.gb_version = common::GbVersion::V2022;
    let sim = DeviceSimulator::new(cfg);
    let root = "34020000001320000001";
    // 两路通道:一路填满扩展字段,一路完全不填(验证默认路径没被破坏)。
    let mut custom = CatalogNode::new(
        "34020000001320000011",
        CatalogNodeType::VideoChannel,
        "东门枪机",
        root,
    );
    custom.address = Some("上海市浦东新区世纪大道 1 号东门".into());
    custom.longitude = Some(121.500_123);
    custom.latitude = Some(31.239_456);
    custom.ptz_type = Some(3);
    custom.capture_position_type =
        Some(gb28181_protocol::manscdp::capture_position::CITY_ROAD.into());
    sim.set_catalog_tree(vec![
        CatalogNode::new(root, CatalogNodeType::Device, "本设备", root),
        custom,
        CatalogNode::new(
            "34020000001320000012",
            CatalogNodeType::VideoChannel,
            "默认通道",
            root,
        ),
    ]);

    let xml = sim.handle_query(&query).unwrap();
    assert!(
        xml.contains("<Address>上海市浦东新区世纪大道 1 号东门</Address>"),
        "逐通道安装地址应出现在目录项里,实际:{xml}"
    );
    assert!(xml.contains("121.500123"), "逐通道经度应生效");
    assert!(xml.contains("31.239456"), "逐通道纬度应生效");
    assert!(
        xml.contains("<PTZType>3</PTZType>"),
        "逐通道摄像机类型应生效"
    );
    assert!(
        xml.contains("<CapturePositionType>0020100</CapturePositionType>"),
        "附录 O 采集部位类型应生效"
    );
    // 没填扩展字段的那一路仍要有必选 Address(取设备级默认),不能变成空元素。
    assert!(
        !xml.contains("<Address></Address>") && !xml.contains("<Address/>"),
        "留空不该产出空 Address 元素"
    );
}

#[test]
fn 目录项采集部位类型仅2022输出() {
    use gb28181_protocol::id_codec::{CatalogNode, CatalogNodeType};
    use gb28181_protocol::manscdp::Query;

    let query = Query {
        cmd_type: "Catalog".into(),
        sn: 1,
        device_id: "34020000001320000001".into(),
        ..Default::default()
    };
    let root = "34020000001320000001";
    let mut node = CatalogNode::new(
        "34020000001320000011",
        CatalogNodeType::VideoChannel,
        "枪机",
        root,
    );
    node.capture_position_type = Some("0020100".into());
    node.ptz_type = Some(1);

    let mut cfg = test_cfg("127.0.0.1", 5060);
    cfg.gb_version = common::GbVersion::V2016;
    let sim = DeviceSimulator::new(cfg);
    sim.set_catalog_tree(vec![
        CatalogNode::new(root, CatalogNodeType::Device, "本设备", root),
        node,
    ]);
    let xml = sim.handle_query(&query).unwrap();
    assert!(
        !xml.contains("CapturePositionType"),
        "CapturePositionType 是 2022 新增字段,发给 2016 平台只会让它报解析错误"
    );
    assert!(
        xml.contains("<PTZType>1</PTZType>"),
        "PTZType 两版都有,应照常输出"
    );
}

#[test]
fn 回放range解析折算千分比() {
    // npt=1800- → 1800/3600 = 500‰。
    let p = DeviceSimulator::parse_range_permille("PLAY MANSRTSP/1.0\r\nRange: npt=1800-\r\n");
    assert_eq!(p, Some(500));
    // npt=now- → 不定位。
    assert_eq!(
        DeviceSimulator::parse_range_permille("PLAY MANSRTSP/1.0\r\nRange: npt=now-\r\n"),
        None
    );
    // 无 Range → None。
    assert_eq!(
        DeviceSimulator::parse_range_permille("PLAY MANSRTSP/1.0\r\nScale: 2.0\r\n"),
        None
    );
}

/// OSD 配置命令(国标 A.2.3.2.11):平台下发 DeviceConfig+OSDConfig,
/// 设备回 DeviceConfig 应答 + 上报 OsdConfig 事件(供 UI 展示已应用)。
#[tokio::test]
async fn osd配置命令_应答并上报事件() {
    use common::{DeviceEvent, DeviceObserver};
    use std::sync::atomic::{AtomicBool, Ordering};
    // 捕获 OsdConfig 事件的观察者。
    struct Cap {
        got: Arc<AtomicBool>,
    }
    impl DeviceObserver for Cap {
        fn on_event(&self, e: DeviceEvent) {
            if let DeviceEvent::OsdConfig {
                time_show,
                osd_show,
            } = e
            {
                assert!(time_show && osd_show);
                self.got.store(true, Ordering::SeqCst);
            }
        }
    }
    let got = Arc::new(AtomicBool::new(false));
    let sim = DeviceSimulator::with_observer(
        test_cfg("127.0.0.1", 5060),
        Arc::new(Cap { got: got.clone() }),
    );
    let ctrl = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceConfig</CmdType><SN>1</SN><DeviceID>d</DeviceID>\
             <OSDConfig><TimeShowFlag>1</TimeShowFlag><OSDShowFlag>1</OSDShowFlag></OSDConfig></Control>",
        )
        .unwrap();
    let xml = sim.handle_control(&ctrl).await.unwrap();
    // 应答 CmdType=DeviceConfig(A.2.6.8),且事件已上报。
    assert!(xml.contains("<CmdType>DeviceConfig</CmdType>"));
    assert!(got.load(Ordering::SeqCst), "应上报 OsdConfig 事件");
}

/// 扩展查询(FR-17):AlarmStatus/HomePositionQuery/StorageCard/CruiseTrack*/PTZPreciseStatus。
#[test]
fn 扩展查询应答_各cmdtype() {
    let mk = |cmd: &str, group: Option<u32>| gb28181_protocol::manscdp::Query {
        cmd_type: cmd.into(),
        sn: 1,
        device_id: "34020000001320000001".into(),
        interval: None,
        group_id: group,
        config_type: None,
        start_time: None,
        end_time: None,
        record_type: None,
        indistinct_query: None,
        stream_number: None,
        ..Default::default()
    };
    let mut cfg = test_cfg("127.0.0.1", 5060);
    cfg.channels.push(ChannelConfig {
        channel_id: DeviceId::new("34020000001320000001").unwrap(),
        name: "Ch".into(),
        status: "ON".into(),
    });
    let sim = DeviceSimulator::new(cfg);

    // 预置一条巡航轨迹 1 = [1,3],并置布防态,便于验证读回。
    {
        let mut s = sim.control_state.lock();
        s.alarming = true;
        s.home_set = true;
        s.cruise_tracks.insert(1, vec![1, 3]);
    }

    let alarm = sim.handle_query(&mk("AlarmStatus", None)).unwrap();
    assert!(alarm.contains("<CmdType>AlarmStatus</CmdType>"));
    assert!(alarm.contains("<DutyStatus>ALARM</DutyStatus>"));

    let home = sim.handle_query(&mk("HomePositionQuery", None)).unwrap();
    assert!(home.contains("<ResetTime>30</ResetTime>"));
    assert!(home.contains("<PresetIndex>1</PresetIndex>"));

    let sd = sim.handle_query(&mk("SDCardStatus", None)).unwrap();
    assert!(sd.contains("<TotalCapacity>32768</TotalCapacity>"));

    let list = sim.handle_query(&mk("CruiseTrackListQuery", None)).unwrap();
    assert!(list.contains("<TrackList Num=\"1\">"));
    assert!(list.contains("<Name>巡航 1</Name>"));

    let detail = sim.handle_query(&mk("CruiseTrackQuery", Some(1))).unwrap();
    assert!(detail.contains("<GroupID>1</GroupID>"));
    assert!(detail.contains("<PresetID>3</PresetID>"));

    let precise = sim.handle_query(&mk("PTZPosition", None)).unwrap();
    assert!(precise.contains("<Zoom>1.00</Zoom>"));
}

/// 测试辅助:按字面 IP 判定(域名交由 `resolve_safe_target` 在解析后判定)。
fn ssrf_ip(host: &str) -> Result<()> {
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => reject_unsafe_ip(ip),
        Err(_) => Ok(()),
    }
}

#[test]
fn ssrf_拒绝受限地址() {
    assert!(ssrf_ip("127.0.0.1").is_err());
    assert!(ssrf_ip("169.254.1.1").is_err());
    assert!(ssrf_ip("192.168.1.10").is_err());
    assert!(ssrf_ip("10.0.0.1").is_err());
    assert!(ssrf_ip("224.0.0.1").is_err());
    // 云元数据端点必须被拒(169.254.169.254 属链路本地)。
    assert!(ssrf_ip("169.254.169.254").is_err());
    // 运营商级 NAT(100.64.0.0/10)与文档段(203.0.113.0/24)同样拒绝。
    assert!(ssrf_ip("100.64.0.1").is_err());
    assert!(ssrf_ip("203.0.113.9").is_err());
    // IPv6 唯一本地与链路本地也拒绝。
    assert!(ssrf_ip("fd00::1").is_err());
    assert!(ssrf_ip("fe80::1").is_err());
    // 真正的公网 IP 放行;域名留给解析后判定(见 resolve_safe_target)。
    assert!(ssrf_ip("8.8.8.8").is_ok());
    assert!(ssrf_ip("upload.example.com").is_ok());
}

#[test]
fn ssrf_ipv6内嵌的内网v4也要拦() {
    // 这几种写法都是"把一个 IPv4 塞进 IPv6",只按 IPv6 规则看的话
    // 它们全是普通全球单播地址,会被放行 —— 于是 SSRF 防护形同虚设。
    //
    // ::/96 IPv4-兼容地址:::127.0.0.1
    assert!(ssrf_ip("::7f00:1").is_err(), "IPv4 兼容地址里的环回");
    assert!(
        ssrf_ip("::c0a8:1").is_err(),
        "IPv4 兼容地址里的 192.168.0.1"
    );
    // 6to4:2002:<v4>::/48
    assert!(
        ssrf_ip("2002:a9fe:a9fe::1").is_err(),
        "6to4 包裹云元数据地址"
    );
    assert!(
        ssrf_ip("2002:c0a8:101::1").is_err(),
        "6to4 包裹 192.168.1.1"
    );
    // NAT64:64:ff9b::<v4>
    assert!(ssrf_ip("64:ff9b::a00:1").is_err(), "NAT64 包裹 10.0.0.1");
    // fec0::/10 站点本地(已废弃但仍可能被路由)
    assert!(ssrf_ip("fec0::1").is_err());
    // 反例:6to4 包裹一个公网 IPv4 应放行,别把规则做成"凡 2002:: 全拒"。
    assert!(ssrf_ip("2002:0808:0808::1").is_ok(), "6to4 包裹 8.8.8.8");
    // 普通公网 IPv6 照常放行。
    assert!(ssrf_ip("2400:3200::1").is_ok());
}

#[test]
fn 上传url的crlf注入被拒绝() {
    // quick-xml 会把 &#13;&#10; 解成真实 CR LF,若原样插进请求行,
    // 恶意平台就能在一次 PUT 里追加请求头甚至第二个完整请求(HTTP 请求走私)。
    let err =
        reject_header_unsafe("路径", "/a\r\nX-Injected: 1").expect_err("含 CRLF 的路径必须拒绝");
    assert!(err.to_string().contains("请求注入"), "实际: {err}");
    assert!(reject_header_unsafe("主机", "evil\ncom").is_err());
    assert!(
        reject_header_unsafe("路径", "/a b").is_err(),
        "裸空格会截断请求行"
    );
    assert!(reject_header_unsafe("路径", "/a\tb").is_err());
    assert!(reject_header_unsafe("路径", "/a\0b").is_err());
    // 正常路径(含 percent-encode 的空格与查询串)放行。
    assert!(reject_header_unsafe("路径", "/upload/snap%20001.jpg?sn=3").is_ok());
    assert!(reject_header_unsafe("主机", "upload.example.com").is_ok());
}

#[tokio::test]
async fn 上传拒绝非常规端口与注入路径() {
    // 端口不在白名单:任意端口 = 一个公网端口扫描器。
    let err = http_put_jpeg("http://8.8.8.8:22/x.jpg", b"x")
        .await
        .expect_err("22 端口应被拒");
    assert!(err.to_string().contains("不在允许范围"), "实际: {err}");
    // 路径注入在连接之前就被拦下(所以用一个不可达地址也能测)。
    let err = http_put_jpeg("http://8.8.8.8:80/a\r\nHost: evil\r\n\r\nGET /x", b"x")
        .await
        .expect_err("CRLF 路径应被拒");
    assert!(err.to_string().contains("请求注入"), "实际: {err}");
    // userinfo 不能用来伪装 host。
    let err = http_put_jpeg("http://8.8.8.8@127.0.0.1:80/x.jpg", b"x")
        .await
        .expect_err("剥掉 userinfo 后是环回地址,应被拒");
    assert!(err.to_string().contains("受限地址"), "实际: {err}");
}

#[tokio::test]
async fn 抓拍上传_协议格式与2xx判定() {
    // 起一个本地 TCP 服务模拟采集端(回 200),用 127.0.0.1 直连绕过 SSRF(仅测协议)。
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let srv = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 1024];
        let n = s.read(&mut buf).await.unwrap();
        let req = String::from_utf8_lossy(&buf[..n]).to_string();
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        req
    });
    // http_put_jpeg 对 127.0.0.1 会被 SSRF 拒绝(安全设计),所以这里用被测函数
    // **真正生成**的请求头去打桩服务,而不是在测试里手抄一份格式串 ——
    // 手抄的那份不会随实现变化,改坏了也照样绿。
    let jpeg: &[u8] = &[0xFF, 0xD8, 0xFF, 0xD9];
    let head = build_put_request("/snap.jpg", &addr.to_string(), jpeg.len());
    let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
    c.write_all(head.as_bytes()).await.unwrap();
    c.write_all(jpeg).await.unwrap();
    let mut resp = Vec::new();
    let _ = c.read_to_end(&mut resp).await;
    assert!(String::from_utf8_lossy(&resp).contains("200"));

    let req = srv.await.unwrap();
    assert!(req.starts_with("PUT /snap.jpg HTTP/1.1"));
    assert!(req.contains("Content-Type: image/jpeg"));
    assert!(req.contains("Content-Length: 4"));
    assert!(req.contains("Connection: close"));
    // 头部与正文之间必须有空行(正文是第二次 write_all 发的,不一定和头部同一个 TCP 读到)。
    assert!(head.ends_with("\r\n\r\n"), "头部必须以空行结束");

    // 且 http_put_jpeg 对环回目标必须拒绝(SSRF 防护生效)。
    assert!(http_put_jpeg(&format!("http://{addr}/x.jpg"), jpeg)
        .await
        .is_err());
}

/// 多通道目录(FR-34):载入模板后 Catalog 查询枚举全部节点,含层级 ParentID/Parental。
#[test]
fn 多通道目录_模板与crud() {
    let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
    sim.load_catalog_template("nvr-8ch");
    let q = gb28181_protocol::manscdp::Query {
        cmd_type: "Catalog".into(),
        sn: 1,
        device_id: sim.config().device_id.to_string(),
        interval: None,
        group_id: None,
        config_type: None,
        start_time: None,
        end_time: None,
        record_type: None,
        indistinct_query: None,
        stream_number: None,
        ..Default::default()
    };
    let xml = sim.handle_query(&q).unwrap();
    // 8 视频通道 + 1 业务分组 = 9 项(根设备自引用被过滤)。
    assert!(xml.contains("<DeviceList Num=\"9\">"));
    assert!(xml.contains("<Name>NVR-8</Name>"));
    assert!(xml.contains("<Name>通道-01</Name>"));
    assert!(xml.contains("<Parental>1</Parental>")); // 业务分组
    assert!(xml.contains("<Parental>0</Parental>")); // 视频通道

    // CRUD:新增一个通道 → 树 +1;删除 → 复原。
    let node = gb28181_protocol::id_codec::CatalogNode::new(
        "34020000001320000099",
        gb28181_protocol::id_codec::CatalogNodeType::VideoChannel,
        "新增通道",
        sim.config().device_id.to_string(),
    );
    assert!(sim.upsert_channel(node.clone()));
    assert!(!sim.upsert_channel(node)); // 再次 upsert 为更新非新增
    assert!(sim.remove_channel("34020000001320000099"));
    assert!(!sim.remove_channel("34020000001320000099")); // 已删
}

/// 语音广播(FR-36):TargetID 属本设备回 Broadcast Response OK。
#[tokio::test]
async fn 语音广播_目标匹配回ok() {
    use sip_core::{Headers, Method, Request, SipMessage};
    let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform_tp.local_addr().unwrap();
    let sim = std::sync::Arc::new(DeviceSimulator::new(test_cfg(
        "127.0.0.1",
        platform_addr.port(),
    )));

    let body = format!(
        "<?xml version=\"1.0\"?><Notify><CmdType>Broadcast</CmdType><SN>1</SN>\
             <SourceID>34020000002000000001</SourceID><TargetID>{}</TargetID></Notify>",
        sim.config().device_id
    );
    let mut h = Headers::new();
    h.set("From", "<sip:34020000002000000001@3402>;tag=p1");
    h.set("To", "<sip:34020000001320000001@3402>");
    h.set("Call-ID", "bc-call-1");
    h.set("CSeq", "1 MESSAGE");
    h.set("Content-Type", "Application/MANSCDP+xml");
    let msg = Request {
        method: Method::Message,
        uri: format!("sip:{}@127.0.0.1", sim.config().device_id),
        headers: h,
        body: body.into_bytes(),
    };
    let incoming = deliver_udp_request(&device_tp, &platform_tp, msg).await;
    // 广播 Response 走独立 MESSAGE,平台按 AOR 收。
    let mut preply = platform_tp.register_inbound("34020000002000000001");
    sim.answer_inbound(&device_tp, &incoming).await.unwrap();

    let reply = tokio::time::timeout(std::time::Duration::from_secs(2), preply.recv())
        .await
        .expect("超时未收到广播应答")
        .expect("关闭");
    match reply.message {
        SipMessage::Request(r) => {
            let s = std::str::from_utf8(&r.body).unwrap();
            assert!(s.contains("<CmdType>Broadcast</CmdType>"));
            assert!(s.contains("<Result>OK</Result>"));
        }
        _ => panic!("应为独立 MESSAGE"),
    }
}

/// 扩展控制(FR-18):巡航增点→列表查询可见;精确云台→精准状态查询读回;布防→报警状态。
#[tokio::test]
async fn 扩展控制_更新状态供查询读回() {
    use gb28181_protocol::manscdp::Control;
    let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));

    // 巡航增点:轨迹 1 加预置 5(byte3=0x84 track=1 preset=5)。
    let cruise = Control {
        cmd_type: "DeviceControl".into(),
        device_id: "d".into(),
        sn: 1,
        ptz_cmd: Some("A50F0184010500".into()),
        ..Default::default()
    };
    sim.handle_control(&cruise).await.unwrap();
    assert!(sim.control_state.lock().cruise_tracks[&1].contains(&5));

    // 精确云台:pan=90 tilt=10 zoom=2。
    let precise = Control {
        cmd_type: "DeviceControl".into(),
        device_id: "d".into(),
        sn: 2,
        ptz_precise_ctrl: Some(gb28181_protocol::manscdp::PtzPreciseCtrl {
            pan: 90.0,
            tilt: 10.0,
            zoom: 2.0,
        }),
        ..Default::default()
    };
    sim.handle_control(&precise).await.unwrap();
    assert_eq!(sim.control_state.lock().precise_pose, (90.0, 10.0, 2.0));

    // 布防 → alarming=true。
    let guard = Control {
        cmd_type: "DeviceControl".into(),
        device_id: "d".into(),
        sn: 3,
        guard_cmd: Some("SetGuard".into()),
        ..Default::default()
    };
    sim.handle_control(&guard).await.unwrap();
    assert!(sim.control_state.lock().alarming);
}

#[tokio::test]
async fn 移动位置订阅_回200并周期上报位置notify() {
    use sip_core::{Method, Request};

    let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform_tp.local_addr().unwrap();

    // 设备平台地址指向 mock 平台;server_domain 为平台 AOR。
    let sim = Arc::new(DeviceSimulator::new(test_cfg(
        "127.0.0.1",
        platform_addr.port(),
    )));

    // 平台构造 MobilePosition 订阅(Interval=1)发往设备 AOR。
    let sub_xml = r#"<?xml version="1.0"?>
<Query><CmdType>MobilePosition</CmdType><SN>7</SN><DeviceID>34020000001320000001</DeviceID><Interval>1</Interval></Query>"#;
    let mut sub_headers = Headers::new();
    sub_headers.set("Call-ID", "sub-call-1");
    sub_headers.set("CSeq", "1 SUBSCRIBE");
    sub_headers.set("Event", "presence");
    let sub_req = Request {
        method: Method::Subscribe,
        uri: "sip:34020000001320000001@127.0.0.1".into(),
        headers: sub_headers,
        body: sub_xml.as_bytes().to_vec(),
    };
    // 订阅的 200 OK 回给 incoming.from,平台按 Call-ID 收。
    let mut psub = platform_tp.register("sub-call-1");
    // 位置 NOTIFY(MESSAGE)Request-URI 指向平台域,平台按 AOR 收。
    let mut pnotify = platform_tp.register_inbound("34020000002000000001");

    let incoming = deliver_udp_request(&device_tp, &platform_tp, sub_req).await;
    let answered = sim.answer_inbound(&device_tp, &incoming).await.unwrap();
    assert!(answered);

    // (1) 平台先收到 SUBSCRIBE 的 200 OK。
    let ok = tokio::time::timeout(std::time::Duration::from_secs(2), psub.recv())
        .await
        .expect("超时未收到订阅 200")
        .expect("关闭");
    match ok.message {
        SipMessage::Response(r) => assert_eq!(r.status, 200),
        _ => panic!("应先收到 200 响应"),
    }

    // (2) 周期任务的首个 tick 立即触发,平台应收到一条 MobilePosition NOTIFY。
    let notify = tokio::time::timeout(std::time::Duration::from_secs(3), pnotify.recv())
        .await
        .expect("超时未收到位置 NOTIFY")
        .expect("关闭");
    match notify.message {
        SipMessage::Request(r) => {
            assert_eq!(r.method, Method::Message);
            let body_str = std::str::from_utf8(&r.body).unwrap();
            assert!(body_str.contains("<CmdType>MobilePosition</CmdType>"));
        }
        _ => panic!("应为 MobilePosition NOTIFY MESSAGE"),
    }

    // 订阅表应登记该订阅。
    assert!(sim
        .subscriptions
        .lock()
        .await
        .contains_key("MobilePosition"));

    // 停止订阅,任务应退出(不再 panic/泄漏)。
    sim.stop_subscriptions().await;
    assert!(sim.subscriptions.lock().await.is_empty());
}

#[tokio::test]
async fn 报警订阅_回200并记录对话供对话内notify() {
    use sip_core::{Method, Request};

    let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform_tp.local_addr().unwrap();

    let sim = Arc::new(DeviceSimulator::new(test_cfg(
        "127.0.0.1",
        platform_addr.port(),
    )));

    let sub_xml = r#"<?xml version="1.0"?>
<Query><CmdType>Alarm</CmdType><SN>9</SN><DeviceID>34020000001320000001</DeviceID></Query>"#;
    let mut h = Headers::new();
    h.set("Call-ID", "alarm-sub-1");
    h.set("CSeq", "1 SUBSCRIBE");
    h.set(
        "From",
        "<sip:34020000002000000001@34020000002000000001>;tag=plat7",
    );
    h.set("To", "<sip:34020000001320000001@34020000002000000001>");
    h.set("Event", "Alarm");
    h.set("Expires", "3600");
    let sub_req = Request {
        method: Method::Subscribe,
        uri: "sip:34020000001320000001@127.0.0.1".into(),
        headers: h,
        body: sub_xml.as_bytes().to_vec(),
    };
    let mut psub = platform_tp.register("alarm-sub-1");

    let incoming = deliver_udp_request(&device_tp, &platform_tp, sub_req).await;
    assert!(sim.answer_inbound(&device_tp, &incoming).await.unwrap());

    // 平台收到订阅 200(To 带本端 tag)。
    let ok = tokio::time::timeout(std::time::Duration::from_secs(2), psub.recv())
        .await
        .expect("超时未收到订阅 200")
        .expect("关闭");
    match ok.message {
        SipMessage::Response(r) => {
            assert_eq!(r.status, 200);
            assert!(r.headers.get("To").unwrap().contains("tag="));
        }
        _ => panic!("应先收到 200"),
    }

    // 报警订阅对话已记录(后续 report_alarm 将走对话内 NOTIFY)。
    let stored = sim.alarm_dialog.lock().await.clone();
    let sub = stored.expect("应记录报警订阅对话");
    let (dialog, dst) = (sub.dialog, sub.peer);
    assert_eq!(dialog.call_id, "alarm-sub-1");
    assert_eq!(dialog.event, "Alarm");
    assert_eq!(dialog.expires, 3600);
    assert!(dialog.remote_from.contains("tag=plat7"));
    assert_eq!(dst, platform_addr);

    // 下线清理:对话应被清除。
    sim.stop_subscriptions().await;
    assert!(sim.alarm_dialog.lock().await.is_none());
}

#[tokio::test]
async fn info回放控制_调速与暂停恢复并回200() {
    use sip_core::{Method, Request};

    let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform_tp.local_addr().unwrap();

    let sim = Arc::new(DeviceSimulator::new(test_cfg(
        "127.0.0.1",
        platform_addr.port(),
    )));

    // 注入一个活跃会话(不真正推流:任务只等待停止信号)。
    let control = media_rtp::PlaybackControl::new();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let _ = stop_rx.await;
    });
    *sim.session.lock().await = Some(PushSession {
        marker: Arc::new(()),
        task,
        stop_tx,
        control: control.clone(),
        is_history: false,
        media_source: None,
        dialog: builder::InboundDialog {
            call_id: "info-call-1".into(),
            local_tag: "devtag".into(),
            remote_from: "<sip:platform@test>;tag=ptag".into(),
            peer: platform_addr,
            request_uri: "sip:34020000001320000001@127.0.0.1".into(),
        },
    });

    // 构造 INFO 请求的辅助闭包。每条用独立 CSeq 与 branch —— 真实平台就是这样,
    // 而复用同一 (branch, CSeq) 会被服务端事务层正确判定为重传。
    let make_info = |cseq: u32, body: &str| {
        let mut h = Headers::new();
        h.set("Call-ID", "info-call-1");
        h.set("CSeq", format!("{cseq} INFO"));
        h.set(
            "Via",
            format!("SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bK-info-{cseq}"),
        );
        h.set(
            "From",
            "<sip:34020000002000000001@34020000002000000001>;tag=p",
        );
        h.set(
            "To",
            "<sip:34020000001320000001@34020000002000000001>;tag=d",
        );
        Request {
            method: Method::Info,
            uri: "sip:34020000001320000001@127.0.0.1".into(),
            headers: h,
            body: body.as_bytes().to_vec(),
        }
    };
    let mut prx = platform_tp.register("info-call-1");

    // PLAY + Scale=2.0 → 倍速 2.0,未暂停。
    let inc = deliver_udp_request(
        &device_tp,
        &platform_tp,
        make_info(2, "PLAY MANSRTSP/1.0\r\nCSeq: 2\r\nScale: 2.0\r\n\r\n"),
    )
    .await;
    assert!(sim.answer_inbound(&device_tp, &inc).await.unwrap());
    assert_eq!(control.speed(), 2.0);
    assert!(!control.is_paused());
    // 平台应收到 200。
    let ok = tokio::time::timeout(std::time::Duration::from_secs(2), prx.recv())
        .await
        .expect("超时")
        .expect("关闭");
    matches!(ok.message, SipMessage::Response(r) if r.status == 200);

    // PAUSE → 暂停。
    let inc = deliver_udp_request(
        &device_tp,
        &platform_tp,
        make_info(3, "PAUSE MANSRTSP/1.0\r\nCSeq: 3\r\nPauseTime: now\r\n\r\n"),
    )
    .await;
    assert!(sim.answer_inbound(&device_tp, &inc).await.unwrap());
    assert!(control.is_paused());

    // PLAY(无 Scale)→ 恢复,倍速回 1.0。
    let inc = deliver_udp_request(
        &device_tp,
        &platform_tp,
        make_info(4, "PLAY MANSRTSP/1.0\r\nCSeq: 4\r\n\r\n"),
    )
    .await;
    assert!(sim.answer_inbound(&device_tp, &inc).await.unwrap());
    assert!(!control.is_paused());
    assert_eq!(control.speed(), 1.0);

    // 同一 (branch, CSeq) 的重传:必须只重发缓存响应,**不能**再跑一遍业务逻辑。
    control.pause();
    let retransmit = deliver_udp_request(
        &device_tp,
        &platform_tp,
        make_info(4, "PLAY MANSRTSP/1.0\r\nCSeq: 4\r\n\r\n"),
    )
    .await;
    assert!(sim.answer_inbound(&device_tp, &retransmit).await.unwrap());
    assert!(
        control.is_paused(),
        "重传的 INFO 不应被当成新命令重新执行 PLAY"
    );
}

/// 平台 200 OK 丢包后重传 INVITE 是现场常态。缺服务端事务时设备会因
/// "已有会话"回 486 Busy Here,点播直接失败——这里锁定修复后的行为:
/// 重传命中缓存的 200 OK。
#[tokio::test]
async fn invite_重传命中缓存响应而非回486() {
    let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform_tp.local_addr().unwrap();
    let device_addr = device_tp.local_addr().unwrap();

    let mut cfg = test_cfg(&platform_addr.ip().to_string(), platform_addr.port());
    // B 档轻量伪流:不依赖 ffmpeg / 文件。
    cfg.light_bitrate_kbps = Some(128);
    cfg.channels = vec![ChannelConfig {
        channel_id: DeviceId::new("34020000001320000132").unwrap(),
        name: "Camera-1".into(),
        status: "ON".into(),
    }];
    let sim = Arc::new(DeviceSimulator::new(cfg));
    sim.set_local_addr_for_test(&device_addr.ip().to_string(), device_addr.port());

    let sdp = "v=0\r\n\
o=34020000002000000001 0 0 IN IP4 127.0.0.1\r\n\
s=Play\r\n\
c=IN IP4 127.0.0.1\r\n\
t=0 0\r\n\
m=video 40000 RTP/AVP 96\r\n\
a=rtpmap:96 PS/90000\r\n\
y=0100000001\r\n";

    let make_invite = || {
        let mut h = Headers::new();
        h.set("Call-ID", "invite-retrans");
        h.set("CSeq", "1 INVITE");
        h.set("Via", "SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bK-inv-1");
        h.set(
            "From",
            "<sip:34020000002000000001@34020000002000000001>;tag=p",
        );
        h.set("To", "<sip:34020000001320000132@34020000002000000001>");
        h.set("Content-Type", "application/sdp");
        Request {
            method: Method::Invite,
            uri: "sip:34020000001320000132@127.0.0.1".into(),
            headers: h,
            body: sdp.as_bytes().to_vec(),
        }
    };

    let mut prx = platform_tp.register("invite-retrans");
    let first_invite = deliver_udp_request(&device_tp, &platform_tp, make_invite()).await;
    assert!(sim.answer_inbound(&device_tp, &first_invite).await.unwrap());

    // 收 100 Trying 与 200 OK。
    let mut statuses = Vec::new();
    for _ in 0..2 {
        let inc = tokio::time::timeout(std::time::Duration::from_secs(2), prx.recv())
            .await
            .expect("超时")
            .expect("关闭");
        if let SipMessage::Response(r) = inc.message {
            statuses.push(r.status);
        }
    }
    assert!(
        statuses.contains(&200),
        "首次 INVITE 应回 200,实际 {statuses:?}"
    );

    // 重传同一条 INVITE:应重发缓存的 200,而不是 486。
    let retransmit = deliver_udp_request(&device_tp, &platform_tp, make_invite()).await;
    assert!(sim.answer_inbound(&device_tp, &retransmit).await.unwrap());
    let inc = tokio::time::timeout(std::time::Duration::from_secs(2), prx.recv())
        .await
        .expect("超时")
        .expect("关闭");
    match inc.message {
        SipMessage::Response(r) => {
            assert_eq!(r.status, 200, "重传 INVITE 不应被回 486 Busy Here");
            assert!(!r.body.is_empty(), "缓存响应应含 SDP(与首次 200 完全一致)");
        }
        other => panic!("应为响应,实际 {other:?}"),
    }

    // 会话应仍是那一个(没有被重复建流)。
    assert!(sim.session.lock().await.is_some());
}

/// 点播 200 OK 必须带 Contact 与 10 位 y=,否则平台的 ACK 到不了设备。
///
/// 现场故障链(实测 UVP 平台 + ZLM 收流):设备应答缺 Contact → 平台(sipgo UAC)
/// 只能把 ACK 的目标算成 INVITE 的 Request-URI 域(平台自己),ACK 到不了设备 →
/// 平台判定 INVITE 失败 → 回滚关掉 ZLM 收流端口 → 设备的媒体 TCP 连接被对端关闭,
/// 前端只看到"推流失败:Broken pipe"。SIP 侧连 ACK 都收不到是这条链的指纹。
#[tokio::test]
async fn invite_200带contact与subject且y为10位() {
    let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform_tp.local_addr().unwrap();
    let device_addr = device_tp.local_addr().unwrap();

    let mut cfg = test_cfg(&platform_addr.ip().to_string(), platform_addr.port());
    // B 档轻量伪流:不依赖 ffmpeg / 文件。
    cfg.light_bitrate_kbps = Some(128);
    let sim = Arc::new(DeviceSimulator::new(cfg));
    sim.set_local_addr_for_test(&device_addr.ip().to_string(), device_addr.port());

    // 与真实平台一致:y= 是 10 位十进制(首位 0=实时),缺了首位就是另一路流。
    let sdp = "v=0\r\n\
o=34020000002000000001 0 0 IN IP4 127.0.0.1\r\n\
s=Play\r\n\
c=IN IP4 127.0.0.1\r\n\
t=0 0\r\n\
m=video 40000 RTP/AVP 96\r\n\
a=rtpmap:96 PS/90000\r\n\
y=0100000005\r\n";

    let mut h = Headers::new();
    h.set("Call-ID", "invite-contact");
    h.set("CSeq", "1 INVITE");
    h.set(
        "Via",
        "SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bK-inv-contact",
    );
    h.set(
        "From",
        "<sip:34020000002000000001@34020000002000000001>;tag=p",
    );
    h.set("To", "<sip:34020000001320000132@34020000002000000001>");
    h.set("Content-Type", "application/sdp");
    let invite = Request {
        method: Method::Invite,
        uri: "sip:34020000001320000132@127.0.0.1".into(),
        headers: h,
        body: sdp.as_bytes().to_vec(),
    };

    let mut prx = platform_tp.register("invite-contact");
    let incoming = deliver_udp_request(&device_tp, &platform_tp, invite).await;
    assert!(sim.answer_inbound(&device_tp, &incoming).await.unwrap());

    let mut ok = None;
    for _ in 0..2 {
        let inc = tokio::time::timeout(std::time::Duration::from_secs(2), prx.recv())
            .await
            .expect("超时")
            .expect("关闭");
        if let SipMessage::Response(r) = inc.message {
            if r.status == 200 {
                ok = Some(r);
                break;
            }
        }
    }
    let resp = ok.expect("应收到 200 OK");

    // RFC 3261 §12.1.1:2xx 必须带 Contact,且必须指向本端信令地址。
    let contact = resp.headers.get("Contact").expect("200 OK 缺 Contact");
    assert_eq!(
        contact.trim(),
        format!(
            "<sip:34020000001320000001@{}:{}>",
            device_addr.ip(),
            device_addr.port()
        ),
        "Contact 必须是本端可逐跳到达的地址,平台按它发 ACK"
    );

    // 附录 K:Subject 标识媒体流发送者(通道)与接收者(平台)。
    let subject = resp.headers.get("Subject").expect("200 OK 缺 Subject");
    assert!(
        subject.starts_with("34020000001320000132:0100000005,"),
        "Subject 应以 通道:10位SSRC 开头,实际 {subject}"
    );

    // 附录 F/G:y= 固定 10 位,必须与平台下发的字面量一致(不能丢首位 0)。
    let body = String::from_utf8(resp.body.clone()).expect("SDP 应为 UTF-8");
    assert!(
        body.contains("y=0100000005\r\n"),
        "y= 必须是 10 位且回显平台值,实际 SDP:\n{body}"
    );
    assert!(body.contains("m=video "), "应答应含本端媒体端口:{body}");
}

/// TCP-PASSIVE:平台 SDP 声明 `a=setup:active` 时,设备监听,平台连入后收到 RTP。
///
/// 这条链路此前只有 SDP 文本级单测,发现不了"监听端口没写进 200 OK 的 m= 行"
/// 或"a=setup 取反写反"这类问题 —— 那两种情况下平台会永远连不上,而设备侧看起来
/// 一切正常。这里按平台的做法真的连进去,并校验 RFC 4571 分帧与 RTP 头。
#[tokio::test]
async fn tcp_被动_平台连入设备收流端口并收到rtp() {
    use tokio::io::AsyncReadExt;

    let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform_tp.local_addr().unwrap();
    let device_addr = device_tp.local_addr().unwrap();

    let mut cfg = test_cfg(&platform_addr.ip().to_string(), platform_addr.port());
    cfg.light_bitrate_kbps = Some(128); // B 档轻量伪流:不依赖 ffmpeg
    let sim = Arc::new(DeviceSimulator::new(cfg));
    sim.set_local_addr_for_test(&device_addr.ip().to_string(), device_addr.port());

    // 平台是主动方(由它来连设备):m= 端口按 RFC 4145 用 discard 端口 9。
    let sdp = "v=0\r\n\
o=34020000002000000001 0 0 IN IP4 127.0.0.1\r\n\
s=Play\r\n\
c=IN IP4 127.0.0.1\r\n\
t=0 0\r\n\
m=video 9 TCP/RTP/AVP 96\r\n\
a=rtpmap:96 PS/90000\r\n\
a=setup:active\r\n\
a=connection:new\r\n\
y=0100000005\r\n";

    let mut h = Headers::new();
    h.set("Call-ID", "invite-tcp-passive");
    h.set("CSeq", "1 INVITE");
    h.set(
        "Via",
        "SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bK-inv-tcp-passive",
    );
    h.set(
        "From",
        "<sip:34020000002000000001@34020000002000000001>;tag=p",
    );
    h.set("To", "<sip:34020000001320000132@34020000002000000001>");
    h.set("Content-Type", "application/sdp");
    let invite = Request {
        method: Method::Invite,
        uri: "sip:34020000001320000132@127.0.0.1".into(),
        headers: h,
        body: sdp.as_bytes().to_vec(),
    };

    let mut prx = platform_tp.register("invite-tcp-passive");
    let incoming = deliver_udp_request(&device_tp, &platform_tp, invite).await;
    assert!(sim.answer_inbound(&device_tp, &incoming).await.unwrap());

    let mut ok = None;
    for _ in 0..2 {
        let inc = tokio::time::timeout(std::time::Duration::from_secs(2), prx.recv())
            .await
            .expect("超时")
            .expect("关闭");
        if let SipMessage::Response(r) = inc.message {
            if r.status == 200 {
                ok = Some(r);
                break;
            }
        }
    }
    let resp = ok.expect("应收到 200 OK");
    let body = String::from_utf8(resp.body.clone()).expect("SDP 应为 UTF-8");
    assert!(
        body.contains("a=setup:passive\r\n"),
        "平台 active → 设备必须回 passive,实际 SDP:\n{body}"
    );

    // 平台按应答里的端口连入:端口 0 表示媒体被禁用,平台不会来连。
    let port: u16 = body
        .lines()
        .find_map(|line| line.trim().strip_prefix("m=video "))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .expect("200 OK 的 m= 行必须带本端监听端口");
    assert_ne!(port, 0, "端口 0 表示媒体被禁用,平台不会来连");

    let mut conn = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .expect("连接设备监听端口超时")
    .expect("连接设备监听端口失败");

    // RFC 4571:2 字节大端长度 + RTP 包。
    let mut len = [0u8; 2];
    tokio::time::timeout(std::time::Duration::from_secs(5), conn.read_exact(&mut len))
        .await
        .expect("超时未收到长度前缀")
        .expect("读取长度前缀失败");
    let rtp_len = u16::from_be_bytes(len) as usize;
    assert!(rtp_len >= 12, "长度前缀应是 RTP 包长,实际 {rtp_len}");
    let mut packet = vec![0u8; rtp_len];
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        conn.read_exact(&mut packet),
    )
    .await
    .expect("超时未收到 RTP 包")
    .expect("读取 RTP 包失败");
    assert_eq!(packet[0], 0x80, "RTP V=2");
    assert_eq!(packet[1] & 0x7f, 96, "PS 流负载类型应为 96");
    assert_eq!(
        u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]),
        100000005,
        "SSRC 必须用平台下发的 y=0100000005"
    );
}

#[test]
fn 内网地址判定只用于被动模式告警() {
    for private in [
        "192.168.1.10",
        "10.0.0.5",
        "172.16.3.4",
        "127.0.0.1",
        "169.254.1.1",
    ] {
        assert!(
            is_private_or_loopback(private.parse().unwrap()),
            "{private} 应判为不可公网直达"
        );
    }
    assert!(is_private_or_loopback("::1".parse().unwrap()));
    // 公网地址不告警:平台本来就连得进来。
    assert!(!is_private_or_loopback("62.234.152.39".parse().unwrap()));
}

#[tokio::test]
async fn 未实现方法回405并带allow头() {
    let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform_tp.local_addr().unwrap();
    let sim = Arc::new(DeviceSimulator::new(test_cfg(
        &platform_addr.ip().to_string(),
        platform_addr.port(),
    )));

    let mut h = Headers::new();
    h.set("Call-ID", "publish-call");
    h.set("CSeq", "1 PUBLISH");
    h.set("Via", "SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bK-pub");
    let request = Request {
        method: Method::Publish,
        uri: "sip:34020000001320000001@127.0.0.1".into(),
        headers: h,
        body: Vec::new(),
    };
    let inc = deliver_udp_request(&device_tp, &platform_tp, request).await;
    let mut prx = platform_tp.register("publish-call");
    assert!(sim.answer_inbound(&device_tp, &inc).await.unwrap());
    let got = tokio::time::timeout(std::time::Duration::from_secs(2), prx.recv())
        .await
        .expect("超时")
        .expect("关闭");
    match got.message {
        SipMessage::Response(r) => {
            assert_eq!(r.status, 405);
            assert!(r.headers.get("Allow").unwrap().contains("INVITE"));
        }
        other => panic!("应为响应,实际 {other:?}"),
    }
}

#[test]
fn 续约周期取平台授予有效期的八成() {
    use std::time::Duration;
    assert_eq!(renew_interval(3600), Duration::from_secs(2880));
    assert_eq!(renew_interval(120), Duration::from_secs(96));
    // 很短的有效期要留出一次事务往返余量,不能贴着到期。
    assert!(renew_interval(30) < Duration::from_secs(30));
    assert!(renew_interval(10) < Duration::from_secs(10));
    assert!(renew_interval(1) >= Duration::from_secs(5));
}

#[test]
fn 目录超过_mtu_时分多条应答且_sumnum_为总数() {
    use gb28181_protocol::id_codec::{CatalogNode, CatalogNodeType};
    let sim = DeviceSimulator::new(test_cfg("1.2.3.4", 5060));
    // 造一棵足够大的目录树,单条 XML 必然超过 900 字节。
    let mut nodes = vec![CatalogNode::new(
        "34020000001320000001",
        CatalogNodeType::Device,
        "Root",
        "34020000001320000001",
    )];
    for i in 0..40u32 {
        nodes.push(CatalogNode::new(
            format!("3402000000132000{:04}", 1000 + i),
            CatalogNodeType::VideoChannel,
            format!("通道-{i}"),
            "34020000001320000001",
        ));
    }
    sim.set_catalog_tree(nodes);

    let query = gb28181_protocol::manscdp::Query {
        cmd_type: "Catalog".into(),
        sn: 7,
        device_id: "34020000001320000001".into(),
        ..Default::default()
    };
    let replies = sim.catalog_replies(&query, MAX_MANSCDP_BODY_BYTES).unwrap();
    assert!(replies.len() > 1, "40 通道的目录应被分成多条");
    for xml in &replies {
        assert!(
            xml.len() <= MAX_MANSCDP_BODY_BYTES * 2,
            "分片后单条不应远超阈值,实际 {}",
            xml.len()
        );
        assert!(xml.contains("<SumNum>40</SumNum>"), "SumNum 必须是总数");
    }
    // 各条 DeviceList 的 Num 属性之和应等于 SumNum。
    let total: usize = replies
        .iter()
        .map(|x| {
            x.split("<DeviceList Num=\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0)
        })
        .sum();
    assert_eq!(total, 40, "分片的 Num 之和应等于总数");
    // 条目 ID 不重不漏。
    let mut ids: Vec<String> = replies
        .iter()
        .flat_map(|x| {
            x.split("<DeviceID>")
                .skip(2) // 跳过根 DeviceID
                .filter_map(|s| s.split("</DeviceID>").next().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 40, "分片后条目应不重不漏");
}

#[test]
fn 假标签不能改变分派路径() {
    use gb28181_protocol::manscdp::parse_envelope;
    // 注释里塞 <Control> 不应把 Query 误判成 Control。
    let sneaky = "<?xml version=\"1.0\"?><Query><!-- <Control> --><CmdType>Catalog</CmdType>\
<SN>1</SN><DeviceID>34020000001320000001</DeviceID></Query>";
    let env = parse_envelope(sneaky).unwrap();
    assert_eq!(env.root, "Query");
    assert_eq!(env.cmd_type, "Catalog");

    // 文本节点里塞 <Control> 同样无效(会被转义)。
    let escaped = "<?xml version=\"1.0\"?><Query><CmdType>Catalog</CmdType>\
<DeviceName>&lt;Control&gt;</DeviceName></Query>";
    assert_eq!(parse_envelope(escaped).unwrap().root, "Query");

    // 真的 Control 报文能被识别。
    let ctrl = "<?xml version=\"1.0\"?><Control><CmdType>DeviceControl</CmdType>\
<SN>2</SN><DeviceID>34020000001320000001</DeviceID><RecordCmd>Record</RecordCmd></Control>";
    let env = parse_envelope(ctrl).unwrap();
    assert_eq!(env.root, "Control");
    assert_eq!(env.cmd_type, "DeviceControl");
}
