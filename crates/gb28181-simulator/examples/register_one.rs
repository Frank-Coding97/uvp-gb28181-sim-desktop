//! 单设备注册示例(M1 联调用)。
//!
//! 从环境变量读取平台与设备参数,注册到上级平台并持续心跳,验证设备在平台上线。
//!
//! 用法示例(配合本机 WVP-Pro):
//! ```bash
//! SERVER_HOST=127.0.0.1 SERVER_PORT=5060 \
//! SERVER_DOMAIN=34020000002000000001 \
//! DEVICE_ID=34020000001320000001 PASSWORD=12345678 \
//! cargo run -p gb28181-simulator --example register_one
//! ```

use std::net::UdpSocket as StdUdp;
use std::sync::Arc;

use common::{DeviceId, Transport};
use gb28181_simulator::{DeviceConfig, DeviceSimulator};
use sip_core::UdpTransport;

/// 读环境变量,缺省用给定默认值。
fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// 通过"向平台地址发起一次 UDP connect"发现本机对外 IP(不真正发包)。
fn discover_local_ip(server: &str) -> String {
    StdUdp::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect(server)?;
            Ok(s.local_addr()?.ip().to_string())
        })
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

#[tokio::main]
async fn main() {
    common::logging::init();

    let server_host = env_or("SERVER_HOST", "127.0.0.1");
    let server_port: u16 = env_or("SERVER_PORT", "5060").parse().expect("SERVER_PORT 非法");
    let server_domain = env_or("SERVER_DOMAIN", "34020000002000000001");
    let device_id_str = env_or("DEVICE_ID", "34020000001320000001");
    let password = env_or("PASSWORD", "12345678");

    let device_id = DeviceId::new(device_id_str.clone()).expect("DEVICE_ID 非法(需 20 位数字)");
    let cfg = DeviceConfig {
        device_id: device_id.clone(),
        username: device_id_str.clone(),
        password,
        server_host: server_host.clone(),
        server_port,
        server_domain,
        transport: Transport::Udp,
        heartbeat_interval_secs: 60,
    };

    // 本端地址:发现对外 IP + 绑定随机端口的共享传输。
    let local_ip = discover_local_ip(&format!("{server_host}:{server_port}"));
    let transport = UdpTransport::bind("0.0.0.0:0").await.expect("绑定 UDP 失败");
    let local_port = transport.local_addr().expect("取本地端口失败").port();
    tracing::info!(%local_ip, local_port, "本端信令地址");

    let sim = Arc::new(DeviceSimulator::new(cfg));

    // Ctrl-C 触发优雅退出。
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("收到退出信号");
    };

    tracing::info!(device=%device_id, server=%format!("{server_host}:{server_port}"), "开始注册");
    sim.run(transport, local_ip, local_port, shutdown).await;
    tracing::info!("设备已停止");
}
