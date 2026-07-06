//! 命令注入测试:模拟平台向运行中的设备发各类 MANSCDP 命令,验证应答。
//! 覆盖所有查询(A.2.4)与控制/配置(A.2.3)命令的分派与响应,走真实 UDP 传输。
use std::sync::Arc;
use std::time::Duration;

use common::{DeviceId, GbVersion, Transport};
use gb28181_simulator::{ChannelConfig, DeviceConfig, DeviceInfo, DeviceSimulator};
use sip_core::{Headers, Incoming, Method, Request, SipMessage, UdpTransport};

fn cfg() -> DeviceConfig {
    DeviceConfig {
        device_id: DeviceId::new("34020000001110000001").unwrap(),
        username: "34020000001110000001".into(),
        password: "12345".into(),
        server_host: "127.0.0.1".into(),
        server_port: 5060,
        server_domain: "3402000000".into(),
        transport: Transport::Udp,
        gb_version: GbVersion::V2022,
        heartbeat_interval_secs: 60,
        channels: vec![ChannelConfig {
            channel_id: DeviceId::new("34020000001320000001").unwrap(),
            name: "Ch1".into(),
            status: "ON".into(),
        }],
        device_info: DeviceInfo {
            device_name: "InjectTest".into(),
            manufacturer: "UVP".into(),
            model: "Sim".into(),
            firmware: "1.0".into(),
        },
        video_source: None,
        video_fps: 25,
        light_bitrate_kbps: None,
    }
}

/// 构造一条平台→设备的 MESSAGE(含指定 XML 体)。
fn message(from_tag: &str, call_id: &str, body: &str) -> SipMessage {
    let mut h = Headers::new();
    h.set(
        "From",
        format!("<sip:34020000002000000001@3402>;tag={from_tag}"),
    );
    h.set("To", "<sip:34020000001110000001@3402>");
    h.set("Call-ID", call_id);
    h.set("CSeq", "1 MESSAGE");
    h.set("Content-Type", "Application/MANSCDP+xml");
    SipMessage::Request(Request {
        method: Method::Message,
        uri: "sip:34020000001110000001@127.0.0.1".into(),
        headers: h,
        body: body.as_bytes().to_vec(),
    })
}

/// 注入一条命令,收集平台侧收到的应答 MESSAGE 体(独立 MESSAGE 路径)。
async fn inject_and_collect(
    sim: &Arc<DeviceSimulator>,
    device_tp: &Arc<UdpTransport>,
    platform_tp: &Arc<UdpTransport>,
    platform_addr: std::net::SocketAddr,
    call_id: &str,
    body: &str,
) -> Vec<String> {
    let mut reply = platform_tp.register_inbound("3402000000");
    let incoming = Incoming {
        message: message("p1", call_id, body),
        from: platform_addr,
    };
    sim.answer_inbound(device_tp, &incoming).await.unwrap();
    let mut bodies = Vec::new();
    // 应答为独立 MESSAGE(入 AOR 路由);等首条应答即可,500ms 兜底超时。
    if let Ok(Some(inc)) = tokio::time::timeout(Duration::from_millis(500), reply.recv()).await {
        if let SipMessage::Request(r) = inc.message {
            bodies.push(String::from_utf8_lossy(&r.body).to_string());
        }
    }
    platform_tp.unregister_inbound("3402000000");
    bodies
}

#[tokio::test]
async fn 全部查询命令注入应答() {
    let sim = Arc::new(DeviceSimulator::new(cfg()));
    let device_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform_tp.local_addr().unwrap();
    // 让设备知道本端地址(应答 From 填充用)。
    sim.set_local_addr_for_test("127.0.0.1", device_tp.local_addr().unwrap().port());

    let dev = "34020000001110000001";
    let queries = [
        ("Catalog", format!("<Query><CmdType>Catalog</CmdType><SN>1</SN><DeviceID>{dev}</DeviceID></Query>")),
        ("DeviceInfo", format!("<Query><CmdType>DeviceInfo</CmdType><SN>2</SN><DeviceID>{dev}</DeviceID></Query>")),
        ("DeviceStatus", format!("<Query><CmdType>DeviceStatus</CmdType><SN>3</SN><DeviceID>{dev}</DeviceID></Query>")),
        ("RecordInfo", format!("<Query><CmdType>RecordInfo</CmdType><SN>4</SN><DeviceID>{dev}</DeviceID><StartTime>2026-07-06T00:00:00</StartTime><EndTime>2026-07-06T23:59:59</EndTime></Query>")),
        ("ConfigDownload", format!("<Query><CmdType>ConfigDownload</CmdType><SN>5</SN><DeviceID>{dev}</DeviceID><ConfigType>BasicParam/VideoParamOpt/VideoParamAttribute</ConfigType></Query>")),
        ("PresetQuery", format!("<Query><CmdType>PresetQuery</CmdType><SN>6</SN><DeviceID>{dev}</DeviceID></Query>")),
        ("HomePositionQuery", format!("<Query><CmdType>HomePositionQuery</CmdType><SN>7</SN><DeviceID>{dev}</DeviceID></Query>")),
        ("SDCardStatus", format!("<Query><CmdType>SDCardStatus</CmdType><SN>8</SN><DeviceID>{dev}</DeviceID></Query>")),
        ("CruiseTrackListQuery", format!("<Query><CmdType>CruiseTrackListQuery</CmdType><SN>9</SN><DeviceID>{dev}</DeviceID></Query>")),
        ("PTZPosition", format!("<Query><CmdType>PTZPosition</CmdType><SN>10</SN><DeviceID>{dev}</DeviceID></Query>")),
    ];

    for (i, (name, body)) in queries.iter().enumerate() {
        let call_id = format!("q-{i}");
        let bodies = inject_and_collect(
            &sim,
            &device_tp,
            &platform_tp,
            platform_addr,
            &call_id,
            body,
        )
        .await;
        let joined = bodies.join("\n");
        assert!(
            joined.contains(&format!("<CmdType>{name}</CmdType>")),
            "查询 {name} 未收到对应 CmdType 应答,实收: {joined}"
        );
        eprintln!("✅ {name} 应答正确");
    }
}

#[tokio::test]
async fn 设备配置控制注入应答() {
    let sim = Arc::new(DeviceSimulator::new(cfg()));
    let device_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform_tp.local_addr().unwrap();
    sim.set_local_addr_for_test("127.0.0.1", device_tp.local_addr().unwrap().port());
    let dev = "34020000001110000001";

    // DeviceConfig 修改 → 应答 CmdType=DeviceConfig(A.2.6.8)。
    let body = format!("<Control><CmdType>DeviceConfig</CmdType><SN>1</SN><DeviceID>{dev}</DeviceID><BasicParam><Name>NewName</Name></BasicParam></Control>");
    let bodies = inject_and_collect(
        &sim,
        &device_tp,
        &platform_tp,
        platform_addr,
        "cfg-1",
        &body,
    )
    .await;
    let joined = bodies.join("\n");
    assert!(
        joined.contains("<CmdType>DeviceConfig</CmdType>"),
        "DeviceConfig 应答 CmdType 错误: {joined}"
    );
    assert!(joined.contains("<Result>OK</Result>"));
    eprintln!("✅ DeviceConfig 应答 CmdType=DeviceConfig");

    // DeviceControl(录像) → 应答 CmdType=DeviceControl。
    let body2 = format!("<Control><CmdType>DeviceControl</CmdType><SN>2</SN><DeviceID>{dev}</DeviceID><RecordCmd>Record</RecordCmd></Control>");
    let bodies2 = inject_and_collect(
        &sim,
        &device_tp,
        &platform_tp,
        platform_addr,
        "ctrl-1",
        &body2,
    )
    .await;
    let joined2 = bodies2.join("\n");
    assert!(
        joined2.contains("<CmdType>DeviceControl</CmdType>"),
        "DeviceControl 应答错误: {joined2}"
    );
    eprintln!("✅ DeviceControl 应答 CmdType=DeviceControl");
}
