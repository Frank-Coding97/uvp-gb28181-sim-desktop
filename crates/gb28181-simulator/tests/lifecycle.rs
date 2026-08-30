//! 生命周期收敛回归:设备下线后**路由表与 socket 都必须真的归零**。
//!
//! 这两条以前都不成立,而且都没有任何办法从外部证明:
//!
//! 1. `run()` 用 `register_inbound_many` 注册了设备 ID + 全部通道 ID + 目录树里的
//!    视频通道,下线时却只 `unregister_inbound(device_id)` —— 通道条目永久留在
//!    路由表里。反复启停一台 16 通道 NVR,每轮多留一批。
//! 2. `spawn_recv_loop` 把 `Arc<Self>` move 进"只在出错时才 break"的循环,
//!    引用计数永远 ≥ 1,于是传输对象永不 drop、socket 永不关闭、任务永不退出。
//!    压测按 64 台设备一个 socket 分片,一万台设备每跑一轮泄漏 150 多个 fd。
//!
//! 现在这两条各有一个断言钉住。

use std::sync::Arc;
use std::time::Duration;

use common::{DeviceId, GbVersion, Transport};
use gb28181_protocol::id_codec::{CatalogNode, CatalogNodeType};
use gb28181_simulator::{ChannelConfig, DeviceConfig, DeviceInfo, DeviceSimulator};
use sip_core::{SipTransport, UdpTransport};

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
        // 注册会失败(没有真平台),把重试间隔压到最小免得测试等太久。
        register_retry_max_secs: 1,
        channels: vec![
            ChannelConfig {
                channel_id: DeviceId::new("34020000001320000011").unwrap(),
                name: "Ch-1".into(),
                status: "ON".into(),
            },
            ChannelConfig {
                channel_id: DeviceId::new("34020000001320000012").unwrap(),
                name: "Ch-2".into(),
                status: "ON".into(),
            },
        ],
        device_info: DeviceInfo {
            device_name: "生命周期测试设备".into(),
            manufacturer: "UVP".into(),
            model: "Sim".into(),
            firmware: "0.1".into(),
        },
        video_source: None,
        video_fps: 25,
        light_bitrate_kbps: Some(128),
        gb_version: GbVersion::V2022,
        signaling_encoding: common::SignalingEncoding::Gb18030,
    }
}

/// 等条件成立,超时即失败。轮询而不是固定 sleep:CI 上机器慢的时候固定睡眠会假失败。
async fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if cond() {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "超时等待: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn 设备下线后全部入站路由被注销() {
    let transport = UdpTransport::bind("127.0.0.1:0").await.expect("绑定");
    let local = transport.local_addr().expect("本地地址");
    let sim = Arc::new(DeviceSimulator::new(cfg(local.port())));
    // 目录树里再放两个视频通道 —— 它们也会被注册进 AOR 路由。
    sim.set_catalog_tree(vec![
        CatalogNode::new(DEVICE_ID, CatalogNodeType::Device, "本设备", DEVICE_ID),
        CatalogNode::new(
            "34020000001320000021",
            CatalogNodeType::VideoChannel,
            "树通道 1",
            DEVICE_ID,
        ),
        CatalogNode::new(
            "34020000001320000022",
            CatalogNodeType::VideoChannel,
            "树通道 2",
            DEVICE_ID,
        ),
    ]);

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let tp: Arc<dyn SipTransport> = transport.clone();
    let run = tokio::spawn({
        let sim = Arc::clone(&sim);
        async move {
            sim.run(tp, "127.0.0.1".into(), local.port(), async {
                let _ = stop_rx.await;
            })
            .await;
        }
    });

    // 注册路由是 run() 开头就做的:等到 AOR 条目出现。
    // 设备 ID + 2 个扁平通道 + 2 个目录树通道 = 5 条。
    wait_for("入站路由注册完成", || {
        transport.route_counts().1 >= 5
    })
    .await;
    assert_eq!(
        transport.route_counts().1,
        5,
        "应注册设备 ID + 扁平通道 + 目录树视频通道"
    );

    let _ = stop_tx.send(());
    tokio::time::timeout(Duration::from_secs(15), run)
        .await
        .expect("run() 应及时返回")
        .expect("run() 不应 panic");

    let (txn_routes, aor_routes) = transport.route_counts();
    assert_eq!(
        aor_routes, 0,
        "下线后不该残留任何 AOR 路由(之前只注销了设备 ID,通道条目永久残留)"
    );
    assert_eq!(txn_routes, 0, "下线后不该残留事务路由");
}

#[tokio::test]
async fn 传输关停后端口真的释放() {
    // 与 sip-core 里那条单测互补:这里走完整的"起设备 → 停设备 → 丢传输"路径,
    // 确认设备侧持有的 Arc 也不会把 socket 钉死。
    let transport = UdpTransport::bind("127.0.0.1:0").await.expect("绑定");
    let local = transport.local_addr().expect("本地地址");
    let sim = Arc::new(DeviceSimulator::new(cfg(local.port())));

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let tp: Arc<dyn SipTransport> = transport.clone();
    let run = tokio::spawn(async move {
        sim.run(tp, "127.0.0.1".into(), local.port(), async {
            let _ = stop_rx.await;
        })
        .await;
    });
    let _ = stop_tx.send(());
    tokio::time::timeout(Duration::from_secs(15), run)
        .await
        .expect("run() 应及时返回")
        .expect("run() 不应 panic");

    transport.shutdown();
    drop(transport);

    let mut rebound = false;
    for _ in 0..200 {
        if tokio::net::UdpSocket::bind(local).await.is_ok() {
            rebound = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        rebound,
        "设备停止 + 传输关停后原端口应可重新绑定,否则 fd 在泄漏"
    );
}
