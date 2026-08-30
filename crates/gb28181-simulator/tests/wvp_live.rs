//! WVP 真机联调测试(运行前需通过环境变量提供受控测试平台参数)。
//! `cargo test -p gb28181-simulator --test wvp_live -- --nocapture --ignored`
use std::sync::Arc;
use std::time::Duration;

use common::{DeviceId, GbVersion, Transport};
use gb28181_simulator::{ChannelConfig, DeviceConfig, DeviceInfo, DeviceSimulator};
use sip_core::UdpTransport;

fn required_env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("缺少联调环境变量 {key}"))
}

fn wvp_cfg(template_channels: Vec<ChannelConfig>) -> DeviceConfig {
    DeviceConfig {
        device_id: DeviceId::new(required_env("WVP_DEVICE_ID")).unwrap(),
        username: required_env("WVP_DEVICE_ID"),
        password: required_env("WVP_SIP_PASSWORD"),
        server_host: required_env("WVP_SERVER_HOST"),
        server_port: required_env("WVP_SERVER_PORT")
            .parse()
            .expect("WVP_SERVER_PORT 非法"),
        server_id: std::env::var("WVP_SERVER_ID")
            .unwrap_or_else(|_| required_env("WVP_SERVER_DOMAIN")),
        server_domain: required_env("WVP_SERVER_DOMAIN"),
        transport: Transport::Udp,
        gb_version: GbVersion::V2022,
        signaling_encoding: common::SignalingEncoding::Gb18030,
        register_expires_secs: 3_600,
        heartbeat_interval_secs: 30,
        heartbeat_fail_threshold: 3,
        channels: template_channels,
        device_info: DeviceInfo {
            device_name: "UVP-Sim-Desktop".into(),
            manufacturer: "UVP".into(),
            model: "Desktop-Sim".into(),
            firmware: "0.1.0".into(),
        },
        video_source: None,
        video_fps: 25,
        light_bitrate_kbps: None,
    }
}

/// 注册 + 运行 20s,让 WVP 查询目录/设备信息/各类命令,观察设备侧应答日志。
#[tokio::test]
#[ignore = "需 VPN 连通 WVP,手动 --ignored 运行"]
async fn wvp联调_注册运行并应答查询() {
    common::logging::init();
    let ch = vec![ChannelConfig {
        channel_id: DeviceId::new("35020000001310000001").unwrap(),
        name: "Camera-1".into(),
        status: "ON".into(),
    }];
    let sim = Arc::new(DeviceSimulator::new(wvp_cfg(ch)));
    let tp = UdpTransport::bind("0.0.0.0:0").await.unwrap();
    let local_host = required_env("WVP_LOCAL_HOST");
    let local_port = tp.local_addr().unwrap().port();
    eprintln!("本端 {local_host}:{local_port} → WVP 联调目标");

    let (tx, rx) = tokio::sync::oneshot::channel();
    let run = tokio::spawn(
        sim.clone()
            .run(tp.clone(), local_host.clone(), local_port, async {
                let _ = rx.await;
            }),
    );

    // 运行 8s:注册 + 心跳 + 应答 WVP 主动查询(目录/设备信息等)。
    tokio::time::sleep(Duration::from_secs(8)).await;

    // 对比:report_position(已知真机 code 200)vs report_video_upload(新增)。
    match sim
        .report_position(&tp, &local_host, local_port, 116.4, 39.9)
        .await
    {
        Ok(code) => eprintln!("MobilePosition 上报 → WVP 响应 code={code}"),
        Err(e) => eprintln!("MobilePosition 上报失败: {e:?}"),
    }
    match sim.report_alarm(&tp, &local_host, local_port, "test").await {
        Ok(code) => eprintln!("Alarm 上报 → WVP 响应 code={code}"),
        Err(e) => eprintln!("Alarm 上报失败: {e:?}"),
    }
    // 主动上报 VideoUploadNotify(FR-37 新增,fire-and-forget,不阻塞)。
    match sim.report_video_upload(&tp, &local_host, local_port).await {
        Ok(()) => eprintln!("VideoUploadNotify 已发出(fire-and-forget)"),
        Err(e) => eprintln!("VideoUploadNotify 发送失败: {e:?}"),
    }

    tokio::time::sleep(Duration::from_secs(4)).await;
    let _ = tx.send(());
    let _ = run.await;
    eprintln!("联调结束");
}
