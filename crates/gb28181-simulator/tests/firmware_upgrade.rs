//! 固件升级本地联调：HTTP 测试包 + SIP MESSAGE/REGISTER 往返。
//!
//! 测试使用本模拟器约定的 JSON 包：
//! `{format, manufacturer, model, firmware, payload, sha256}`。它只验证下载
//! 和包完整性，不执行 payload，也不替换测试进程本身。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::{DeviceId, GbVersion, Transport};
use gb28181_simulator::{builder, ChannelConfig, DeviceConfig, DeviceInfo, DeviceSimulator};
use serde_json::json;
use sha2::{Digest, Sha256};
use sip_core::{Headers, Incoming, Method, Request, SipMessage, UdpTransport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::{self, UnboundedReceiver};

const DEVICE_ID: &str = "34020000001320000001";
const SERVER_ID: &str = "34020000002000000991";
const SERVER_DOMAIN: &str = "3402000000";
const MANUFACTURER: &str = "UVP";
const MODEL: &str = "Desktop-Sim";

fn test_cfg(platform_port: u16, version: GbVersion) -> DeviceConfig {
    DeviceConfig {
        device_id: DeviceId::new(DEVICE_ID).unwrap(),
        username: DEVICE_ID.into(),
        password: "12345678".into(),
        server_host: "127.0.0.1".into(),
        server_port: platform_port,
        server_id: SERVER_ID.into(),
        server_domain: SERVER_DOMAIN.into(),
        transport: Transport::Udp,
        register_expires_secs: 3_600,
        heartbeat_interval_secs: 60,
        heartbeat_fail_threshold: 3,
        channels: vec![ChannelConfig {
            channel_id: DeviceId::new("34020000001320000001").unwrap(),
            name: "Camera-1".into(),
            status: "ON".into(),
        }],
        device_info: DeviceInfo {
            device_name: "Upgrade Test Device".into(),
            manufacturer: MANUFACTURER.into(),
            model: MODEL.into(),
            firmware: "0.1.0".into(),
        },
        video_source: None,
        media_profile: None,
        video_fps: 25,
        light_bitrate_kbps: None,
        gb_version: version,
        signaling_encoding: common::SignalingEncoding::Utf8,
    }
}

fn state_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "uvp-sim-upgrade-{name}-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ))
}

fn package(firmware: &str, payload: &str, valid_hash: bool) -> Vec<u8> {
    let mut digest = Sha256::new();
    digest.update(payload.as_bytes());
    let mut hash = format!("{:x}", digest.finalize());
    if !valid_hash {
        hash.replace_range(..2, if hash.starts_with("00") { "ff" } else { "00" });
    }
    serde_json::to_vec(&json!({
        "format": "uvp-simulator-firmware-v1",
        "manufacturer": MANUFACTURER,
        "model": MODEL,
        "firmware": firmware,
        "payload": payload,
        "sha256": hash,
    }))
    .unwrap()
}

/// 启动只服务一次的本地 HTTP 文件服务器，便于确认设备确实发起下载。
async fn serve_http(
    body: Vec<u8>,
    status: u16,
    delay: Duration,
) -> (SocketAddr, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let request_count = Arc::new(AtomicUsize::new(0));
    let request_count_for_task = Arc::clone(&request_count);
    let task = tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        request_count_for_task.fetch_add(1, Ordering::SeqCst);
        let mut request = vec![0u8; 4096];
        let _ = stream.read(&mut request).await;
        tokio::time::sleep(delay).await;
        let reason = if status == 200 { "OK" } else { "Found" };
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.write_all(&body).await;
    });
    (addr, task, request_count)
}

fn control_incoming(body: &str, platform_addr: SocketAddr, call_id: &str) -> Incoming {
    let mut headers = Headers::new();
    headers.set(
        "From",
        format!("<sip:{SERVER_ID}@{SERVER_DOMAIN}>;tag=platform"),
    );
    headers.set("To", format!("<sip:{DEVICE_ID}@{SERVER_DOMAIN}>"));
    headers.set("Call-ID", call_id);
    headers.set("CSeq", "1 MESSAGE");
    headers.set("Content-Type", "Application/MANSCDP+xml");
    Incoming {
        message: SipMessage::Request(Request {
            method: Method::Message,
            uri: format!("sip:{DEVICE_ID}@127.0.0.1"),
            headers,
            body: body.as_bytes().to_vec(),
        }),
        from: platform_addr,
    }
}

fn upgrade_xml(file_url: &str, firmware: &str, session_id: &str) -> String {
    format!(
        "<Control><CmdType>DeviceControl</CmdType><SN>7</SN><DeviceID>{DEVICE_ID}</DeviceID><DeviceUpgrade><Firmware>{firmware}</Firmware><FileURL>{file_url}</FileURL><Manufacturer>{MANUFACTURER}</Manufacturer><SessionID>{session_id}</SessionID></DeviceUpgrade></Control>"
    )
}

#[derive(Debug)]
enum PlatformEvent {
    Message(String),
    Register(u8),
}

/// 接收设备升级期间的所有入站请求，处理真实 Digest REGISTER，并将 MESSAGE
/// 交给测试断言。使用 AOR 入站队列而不是 Call-ID 响应队列，避免与设备业务
/// Response 共用 Call-ID 时发生路由碰撞。
fn spawn_upgrade_platform(
    platform: Arc<UdpTransport>,
    device_addr: SocketAddr,
    result_failures: u8,
) -> (
    UnboundedReceiver<PlatformEvent>,
    tokio::task::JoinHandle<()>,
) {
    let mut inbound = platform.register_inbound(SERVER_ID);
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let mut register_count = 0u8;
        let mut result_count = 0u8;
        while let Some(incoming) = inbound.recv().await {
            let SipMessage::Request(request) = incoming.message else {
                continue;
            };
            match request.method {
                Method::Register => {
                    register_count += 1;
                    let _ = events_tx.send(PlatformEvent::Register(register_count));
                    assert_eq!(request.headers.get("X-GB-Ver"), Some("3.0"));
                    let mut headers = Headers::new();
                    headers.set(
                        "Call-ID",
                        request.headers.get("Call-ID").unwrap_or_default(),
                    );
                    headers.set("CSeq", request.headers.cseq().unwrap_or("1 REGISTER"));
                    if register_count == 1 {
                        headers.set(
                            "WWW-Authenticate",
                            r#"Digest realm="3402000000", nonce="upgrade-test""#,
                        );
                    }
                    let response = sip_core::Response {
                        status: if register_count == 1 { 401 } else { 200 },
                        reason: if register_count == 1 {
                            "Unauthorized".into()
                        } else {
                            "OK".into()
                        },
                        headers,
                        body: Vec::new(),
                    };
                    platform
                        .send_to(&SipMessage::Response(response), device_addr)
                        .await
                        .unwrap();
                }
                Method::Message => {
                    let body = String::from_utf8_lossy(&request.body).into_owned();
                    let is_result = body.contains("<CmdType>DeviceUpgradeResult</CmdType>");
                    let _ = events_tx.send(PlatformEvent::Message(body));
                    if is_result {
                        result_count += 1;
                        let status = if result_count <= result_failures {
                            503
                        } else {
                            200
                        };
                        platform
                            .send_to(
                                &SipMessage::Response(builder::response_status(
                                    &request,
                                    status,
                                    if status == 503 {
                                        "Service Unavailable"
                                    } else {
                                        "OK"
                                    },
                                )),
                                incoming.from,
                            )
                            .await
                            .unwrap();
                    }
                }
                _ => {}
            }
        }
    });
    (events_rx, task)
}

async fn next_message(events: &mut UnboundedReceiver<PlatformEvent>) -> String {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("平台等待设备 MESSAGE 超时")
            .expect("平台事件通道关闭")
        {
            PlatformEvent::Message(body) => return body,
            PlatformEvent::Register(_) => {}
        }
    }
}

async fn next_register(events: &mut UnboundedReceiver<PlatformEvent>) -> u8 {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("平台等待设备 REGISTER 超时")
            .expect("平台事件通道关闭")
        {
            PlatformEvent::Register(index) => return index,
            PlatformEvent::Message(_) => {}
        }
    }
}

/// 等待设备回给平台的业务/最终 MESSAGE。业务应答是 fire-and-forget，最终结果
/// 是有事务的 MESSAGE，必须回 SIP 200 让设备任务完成。
async fn receive_messages(
    platform: Arc<UdpTransport>,
    mut inbound: tokio::sync::mpsc::UnboundedReceiver<Incoming>,
    want_result: bool,
) -> String {
    loop {
        let incoming = tokio::time::timeout(Duration::from_secs(5), inbound.recv())
            .await
            .expect("平台等待设备 MESSAGE 超时")
            .expect("平台入站路由关闭");
        let SipMessage::Request(request) = incoming.message else {
            continue;
        };
        if request.method != Method::Message {
            continue;
        }
        let body = String::from_utf8_lossy(&request.body).into_owned();
        if want_result && !body.contains("<CmdType>DeviceUpgradeResult</CmdType>") {
            continue;
        }
        if !want_result && !body.contains("<CmdType>DeviceControl</CmdType>") {
            continue;
        }
        if want_result {
            platform
                .send_to(
                    &SipMessage::Response(builder::response_ok(&request)),
                    incoming.from,
                )
                .await
                .unwrap();
        }
        return body;
    }
}

#[tokio::test]
async fn success_download_apply_reregister_and_notify_current_version() {
    let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform.local_addr().unwrap();
    let device_addr = device.local_addr().unwrap();
    let (http_addr, http_task, http_requests) = serve_http(
        package("0.2.0", "fixture-success", true),
        200,
        Duration::ZERO,
    )
    .await;
    let dir = state_dir("success");
    let sim = Arc::new(DeviceSimulator::with_upgrade_state_dir(
        test_cfg(platform_addr.port(), GbVersion::V2022),
        dir.clone(),
    ));
    sim.set_local_addr_for_test("127.0.0.1", device_addr.port());
    let (mut events, platform_task) = spawn_upgrade_platform(platform.clone(), device_addr, 1);
    let session = "success-session-0000000000000000000001";
    let body = upgrade_xml(
        &format!("http://{http_addr}/firmware.json"),
        "0.2.0",
        session,
    );
    sim.answer_inbound(
        &device,
        &control_incoming(&body, platform_addr, "upgrade-success"),
    )
    .await
    .unwrap();
    let response = next_message(&mut events).await;
    assert!(response.contains("<Result>OK</Result>"));
    assert_eq!(next_register(&mut events).await, 1);
    assert_eq!(next_register(&mut events).await, 2);
    let result = next_message(&mut events).await;
    assert!(result.contains("<UpgradeResult>OK</UpgradeResult>"));
    assert!(result.contains("<Firmware>0.2.0</Firmware>"));
    assert!(result.contains(&format!("<SessionID>{session}</SessionID>")));
    assert!(!result.contains("<Percent>") && !result.contains("<Result>1</Result>"));
    let retry_result = next_message(&mut events).await;
    assert_eq!(retry_result, result);
    assert_eq!(http_requests.load(Ordering::SeqCst), 1);
    assert_eq!(sim.current_firmware(), "0.2.0");

    let reopened = DeviceSimulator::with_upgrade_state_dir(
        test_cfg(platform_addr.port(), GbVersion::V2022),
        dir.clone(),
    );
    assert_eq!(reopened.current_firmware(), "0.2.0");
    let _ = tokio::fs::remove_dir_all(dir).await;
    http_task.abort();
    platform_task.abort();
}

#[tokio::test]
async fn corrupt_package_not_applied_and_final_error_has_reason_02() {
    let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform.local_addr().unwrap();
    let device_addr = device.local_addr().unwrap();
    let (http_addr, http_task, _http_requests) = serve_http(
        package("0.2.0", "fixture-corrupt", false),
        200,
        Duration::ZERO,
    )
    .await;
    let dir = state_dir("corrupt");
    let sim = Arc::new(DeviceSimulator::with_upgrade_state_dir(
        test_cfg(platform_addr.port(), GbVersion::V2022),
        dir.clone(),
    ));
    sim.set_local_addr_for_test("127.0.0.1", device_addr.port());
    let inbound = platform.register_inbound(SERVER_ID);
    let session = "corrupt-session-00000000000000000001";
    let body = upgrade_xml(
        &format!("http://{http_addr}/firmware.json"),
        "0.2.0",
        session,
    );
    sim.answer_inbound(
        &device,
        &control_incoming(&body, platform_addr, "upgrade-corrupt"),
    )
    .await
    .unwrap();
    let response = receive_messages(platform.clone(), inbound, false).await;
    assert!(response.contains("<Result>OK</Result>"));
    let result =
        receive_messages(platform.clone(), platform.register_inbound(SERVER_ID), true).await;
    assert!(result.contains("<UpgradeResult>ERROR</UpgradeResult>"));
    assert!(
        result.contains("<UpgradeFailedReason>02</UpgradeFailedReason>"),
        "result={result}"
    );
    assert_eq!(sim.current_firmware(), "0.1.0");
    let _ = tokio::fs::remove_dir_all(dir).await;
    http_task.abort();
}

#[tokio::test]
async fn same_session_deduplicates_and_different_session_is_busy() {
    let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform.local_addr().unwrap();
    let device_addr = device.local_addr().unwrap();
    let (http_addr, http_task, _http_requests) = serve_http(
        package("0.2.0", "fixture-dedupe", true),
        200,
        Duration::from_millis(500),
    )
    .await;
    let dir = state_dir("dedupe");
    let sim = Arc::new(DeviceSimulator::with_upgrade_state_dir(
        test_cfg(platform_addr.port(), GbVersion::V2022),
        dir.clone(),
    ));
    sim.set_local_addr_for_test("127.0.0.1", device_addr.port());
    let session = "dedupe-session-000000000000000000001";
    let body = upgrade_xml(
        &format!("http://{http_addr}/firmware.json"),
        "0.2.0",
        session,
    );

    // 首次执行：平台只需确认立即业务 Response；HTTP 服务只接受一次连接。
    let (mut events, platform_task) = spawn_upgrade_platform(platform.clone(), device_addr, 0);
    sim.answer_inbound(
        &device,
        &control_incoming(&body, platform_addr, "upgrade-dedupe-1"),
    )
    .await
    .unwrap();
    let first_response = next_message(&mut events).await;
    assert!(first_response.contains("<Result>OK</Result>"));

    // 同 SessionID/同参数在执行中只回 OK，不会创建第二个 HTTP 下载任务。
    sim.answer_inbound(
        &device,
        &control_incoming(&body, platform_addr, "upgrade-dedupe-2"),
    )
    .await
    .unwrap();
    let duplicate_response = next_message(&mut events).await;
    assert!(duplicate_response.contains("<Result>OK</Result>"));

    // 不同 SessionID 在执行期间被明确拒绝，避免并发刷写。
    let busy = upgrade_xml(
        &format!("http://{http_addr}/firmware.json"),
        "0.2.0",
        "busy-session-0000000000000000000000001",
    );
    sim.answer_inbound(
        &device,
        &control_incoming(&busy, platform_addr, "upgrade-dedupe-3"),
    )
    .await
    .unwrap();
    let busy_response = next_message(&mut events).await;
    assert!(busy_response.contains("<Result>ERROR</Result>"));

    assert_eq!(next_register(&mut events).await, 1);
    assert_eq!(next_register(&mut events).await, 2);
    let result = next_message(&mut events).await;
    assert!(result.contains("<UpgradeResult>OK</UpgradeResult>"));

    // 完成后重复请求重放结果；HTTP server 已结束，若再次下载会测试超时。
    sim.answer_inbound(
        &device,
        &control_incoming(&body, platform_addr, "upgrade-dedupe-4"),
    )
    .await
    .unwrap();
    assert!(next_message(&mut events)
        .await
        .contains("<Result>OK</Result>"));
    let replay_result = next_message(&mut events).await;
    assert!(replay_result.contains("<UpgradeResult>OK</UpgradeResult>"));
    assert_eq!(sim.current_firmware(), "0.2.0");
    let _ = tokio::fs::remove_dir_all(dir).await;
    http_task.abort();
    platform_task.abort();
}

#[tokio::test]
async fn gb2016_is_rejected_without_download_or_final_notify() {
    let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let platform_addr = platform.local_addr().unwrap();
    let device_addr = device.local_addr().unwrap();
    let dir = state_dir("2016");
    let sim = Arc::new(DeviceSimulator::with_upgrade_state_dir(
        test_cfg(platform_addr.port(), GbVersion::V2016),
        dir.clone(),
    ));
    sim.set_local_addr_for_test("127.0.0.1", device_addr.port());
    let inbound = platform.register_inbound(SERVER_ID);
    let body = upgrade_xml(
        "http://127.0.0.1:1/should-not-connect",
        "0.2.0",
        "gb2016-session-0000000000000000000001",
    );
    sim.answer_inbound(
        &device,
        &control_incoming(&body, platform_addr, "upgrade-2016"),
    )
    .await
    .unwrap();
    let response = receive_messages(platform.clone(), inbound, false).await;
    assert!(response.contains("<Result>ERROR</Result>"));
    assert_eq!(sim.current_firmware(), "0.1.0");
    let no_final = tokio::time::timeout(
        Duration::from_millis(300),
        platform.register_inbound(SERVER_ID).recv(),
    )
    .await;
    assert!(no_final.is_err(), "2016 拒绝不应发 DeviceUpgradeResult");
    let _ = tokio::fs::remove_dir_all(dir).await;
}
