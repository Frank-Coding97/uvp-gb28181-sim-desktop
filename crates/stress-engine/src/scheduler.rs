//! 压测调度器(编排器):批量拉起设备 + 生命周期管理 + 指标聚合。

use std::sync::Arc;

use common::Result;
use gb28181_simulator::DeviceSimulator;
use scenario::Scenario;

use crate::metrics::Metrics;

/// 压测编排器:持有场景、批量设备、共享指标。
pub struct Orchestrator {
    devices: Vec<Arc<DeviceSimulator>>,
    /// 所有设备共享的原子指标,可随时 snapshot 读取。
    pub metrics: Arc<Metrics>,
}

impl Orchestrator {
    /// 用场景批量创建设备实例(含共享 Metrics 注入)。
    pub fn new(scenario: &dyn Scenario, count: usize) -> Result<Self> {
        let configs = scenario.generate(count)?;
        let metrics = Arc::new(Metrics::default());
        let devices: Vec<Arc<DeviceSimulator>> = configs
            .into_iter()
            .map(|cfg| {
                let m: Arc<dyn common::DeviceObserver> = Arc::clone(&metrics) as _;
                Arc::new(DeviceSimulator::with_observer(cfg, m))
            })
            .collect();
        tracing::info!(device_count = devices.len(), "编排器已创建设备实例");
        Ok(Orchestrator { devices, metrics })
    }

    /// 启动所有设备的 run 任务(注册+心跳+入站应答)。
    /// 从 `stop` 广播接收停止信号:发送一次即令所有设备优雅停止。
    pub async fn run(&self, stop: tokio::sync::broadcast::Sender<()>) -> Result<()> {
        // 单个共享 transport(所有设备复用一个 UDP socket)。bind 已返回 Arc。
        let transport = sip_core::UdpTransport::bind("0.0.0.0:0").await?;
        let local_addr = transport.local_addr()?;
        let local_port = local_addr.port();
        // 简化:用本地 IP(实际应用需 discover_local_ip)。
        let local_host = local_addr.ip().to_string();
        tracing::info!(%local_host, local_port, "共享 SIP 传输已绑定");

        // 批量启动设备 run 任务。
        let mut handles = Vec::new();
        for dev in &self.devices {
            let dev_clone = dev.clone();
            let tp = Arc::clone(&transport);
            let host = local_host.clone();
            let mut stop_rx = stop.subscribe();
            let handle = tokio::spawn(async move {
                dev_clone
                    .run(tp, host, local_port, async move {
                        let _ = stop_rx.recv().await;
                    })
                    .await;
            });
            handles.push(handle);
        }

        tracing::info!(task_count = handles.len(), "所有设备 run 任务已启动");

        // 等待所有任务完成(stop 触发后自然退出)。
        for h in handles {
            let _ = h.await;
        }
        tracing::info!("所有设备已停止");
        Ok(())
    }

    /// 设备数量。
    pub fn device_count(&self) -> usize {
        self.devices.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::Transport;
    use scenario::{DeviceInfoTemplate, LinearScenario, MediaProfile};

    fn test_scenario() -> LinearScenario {
        LinearScenario {
            base_device_id: "34020000001320000001".into(),
            password: "12345678".into(),
            server_host: "127.0.0.1".into(),
            server_port: 15060,
            server_domain: "34020000002000000001".into(),
            transport: Transport::Udp,
            heartbeat_interval_secs: 3600, // 长心跳避免测试中发送
            channels_per_device: 1,
            device_info: DeviceInfoTemplate {
                device_name: "TestDev".into(),
                manufacturer: "UVP".into(),
                model: "Sim".into(),
                firmware: "0.1".into(),
            },
            media_profile: MediaProfile::A,
            video_source: None,
            video_fps: 25,
        }
    }

    #[test]
    fn 编排器创建设备() {
        let sc = test_scenario();
        let orch = Orchestrator::new(&sc, 5).unwrap();
        assert_eq!(orch.device_count(), 5);
    }
}
