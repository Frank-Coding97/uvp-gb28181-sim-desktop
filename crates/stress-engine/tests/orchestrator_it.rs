//! M3 集成测试:编排器批量拉起设备,对 mock 平台完成注册。
//!
//! 验证 stress-engine 的核心闭环:场景生成 N 设备 → 编排器共享传输批量注册 →
//! mock 平台收到 N 个 REGISTER 并回 200。

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use scenario::{DeviceInfoTemplate, LinearScenario, MediaProfile};
use sip_core::{Headers, Response, SipMessage, UdpTransport};
use stress_engine::Orchestrator;

/// 起一个 mock 平台:对任意 REGISTER 直接回 200(简化,不走 401)。
/// 返回已收到的不同设备数计数句柄。
async fn spawn_mock_platform() -> (Arc<UdpTransport>, u16, Arc<AtomicU32>) {
    let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let port = platform.local_addr().unwrap().port();
    let counter = Arc::new(AtomicU32::new(0));

    // 平台需要按各设备 Call-ID 接收 REGISTER;这里用通配接收:
    // 由于 transport 按 Call-ID 路由,mock 平台改用裸 socket 更简单。
    // 但为复用现有 transport,采用 register_inbound 对平台域 AOR 收 REGISTER。
    let plat = platform.clone();
    let cnt = counter.clone();
    // REGISTER 的 Request-URI = sip:<domain>@host:port,AOR = domain。
    let mut inbound = platform.register_inbound("34020000002000000001");
    tokio::spawn(async move {
        while let Some(inc) = inbound.recv().await {
            if let SipMessage::Request(req) = &inc.message {
                cnt.fetch_add(1, Ordering::Relaxed);
                let mut h = Headers::new();
                if let Some(v) = req.headers.get("Via") {
                    h.append("Via", v.to_string());
                }
                if let Some(f) = req.headers.get("From") {
                    h.append("From", f.to_string());
                }
                if let Some(t) = req.headers.get("To") {
                    h.append("To", format!("{t};tag=plat"));
                }
                if let Some(c) = req.headers.call_id() {
                    h.append("Call-ID", c.to_string());
                }
                if let Some(cs) = req.headers.cseq() {
                    h.append("CSeq", cs.to_string());
                }
                let resp = SipMessage::Response(Response {
                    status: 200,
                    reason: "OK".into(),
                    headers: h,
                    body: Vec::new(),
                });
                let _ = plat.send_to(&resp, inc.from).await;
            }
        }
    });

    (platform, port, counter)
}

fn scenario_for(port: u16) -> LinearScenario {
    LinearScenario {
        base_device_id: "34020000001320000001".into(),
        password: "12345678".into(),
        server_host: "127.0.0.1".into(),
        server_port: port,
        server_id: "34020000002000000001".into(),
        server_domain: "3402000000".into(),
        transport: common::Transport::Udp,
        heartbeat_interval_secs: 3600,
        channels_per_device: 1,
        device_info: DeviceInfoTemplate {
            device_name: "IT".into(),
            manufacturer: "UVP".into(),
            model: "Sim".into(),
            firmware: "0.1".into(),
        },
        media_profile: MediaProfile::A,
        video_source: None,
        video_fps: 25,
        bitrate_kbps: 512,
        active_ratio: 1.0,
        ramp_per_second: 0,
        gb_version: common::GbVersion::V2022,
    }
}

#[tokio::test]
async fn 编排器批量注册_平台收到全部() {
    let (_platform, port, counter) = spawn_mock_platform().await;

    const N: usize = 8;
    let sc = scenario_for(port);
    let orch = Orchestrator::new(&sc, N).unwrap();
    assert_eq!(orch.device_count(), N);

    // 启动编排器,运行片刻后停止。
    let (stop_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let stop_for_run = stop_tx.clone();
    let run = tokio::spawn(async move { orch.run(stop_for_run, 0, 0).await });

    // 给设备时间完成注册。
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    let seen = counter.load(Ordering::Relaxed);

    // 停止并等待退出。
    let _ = stop_tx.send(());
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), run).await;

    // 平台应至少收到 N 个 REGISTER(可能因重传更多)。
    assert!(
        seen >= N as u32,
        "平台仅收到 {seen} 个 REGISTER,期望 >= {N}"
    );
}
