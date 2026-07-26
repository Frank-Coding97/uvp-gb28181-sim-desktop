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
    /// 爬坡速率:每秒拉起多少台(0 = 一次性)。
    ramp_per_second: u32,
}

impl Orchestrator {
    /// 用场景批量创建设备实例(含共享 Metrics 注入)。
    pub fn new(scenario: &dyn Scenario, count: usize) -> Result<Self> {
        let configs = scenario.generate(count)?;
        let ramp_per_second = scenario.ramp_per_second();
        let metrics = Arc::new(Metrics::default());
        let devices: Vec<Arc<DeviceSimulator>> = configs
            .into_iter()
            .map(|cfg| {
                let m: Arc<dyn common::DeviceObserver> = Arc::clone(&metrics) as _;
                Arc::new(DeviceSimulator::with_observer(cfg, m))
            })
            .collect();
        tracing::info!(
            device_count = devices.len(),
            ramp_per_second,
            "编排器已创建设备实例"
        );
        Ok(Orchestrator {
            devices,
            metrics,
            ramp_per_second,
        })
    }

    /// 启动所有设备的 run 任务(注册+心跳+入站应答)。
    /// 从 `stop` 广播接收停止信号:发送一次即令所有设备优雅停止。
    ///
    /// `position_secs`/`alarm_secs` > 0 时,每台设备额外周期主动上报定位/报警(施压平台);
    /// 0 表示不上报。
    pub async fn run(
        &self,
        stop: tokio::sync::broadcast::Sender<()>,
        position_secs: u64,
        alarm_secs: u64,
    ) -> Result<()> {
        // 单个共享 transport(所有设备复用一个 UDP socket)。bind 已返回 Arc。
        let transport = sip_core::UdpTransport::bind("0.0.0.0:0").await?;
        let local_addr = transport.local_addr()?;
        let local_port = local_addr.port();
        // 简化:用本地 IP(实际应用需 discover_local_ip)。
        let local_host = local_addr.ip().to_string();
        tracing::info!(%local_host, local_port, "共享 SIP 传输已绑定");

        // 爬坡:每拉起 ramp_per_second 台后等 1 秒,避免瞬时注册风暴(FR-21)。
        // ramp=0 表示一次性全拉起。
        let ramp = self.ramp_per_second;

        // 批量启动设备 run 任务。
        let mut handles = Vec::new();
        for (i, dev) in self.devices.iter().enumerate() {
            // 爬坡节流:每满一批(ramp 台)暂停 1 秒。
            if ramp > 0 && i > 0 && (i as u32) % ramp == 0 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
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

            // 主动上报施压:为该设备挂一个周期任务(定位/报警),随 stop 退出。
            if position_secs > 0 || alarm_secs > 0 {
                let dev_rep = dev.clone();
                let tp_rep = Arc::clone(&transport);
                let host_rep = local_host.clone();
                let metrics = Arc::clone(&self.metrics);
                let mut stop_rep = stop.subscribe();
                handles.push(tokio::spawn(async move {
                    Self::report_loop(
                        dev_rep,
                        tp_rep,
                        host_rep,
                        local_port,
                        position_secs,
                        alarm_secs,
                        metrics,
                        &mut stop_rep,
                    )
                    .await;
                }));
            }
        }

        tracing::info!(task_count = handles.len(), "所有设备 run 任务已启动");

        // 等待所有任务完成(stop 触发后自然退出)。
        for h in handles {
            let _ = h.await;
        }
        tracing::info!("所有设备已停止");
        Ok(())
    }

    /// 单设备的周期主动上报循环(定位/报警),用于压测施压。随 stop 广播退出。
    #[allow(clippy::too_many_arguments)]
    async fn report_loop(
        dev: Arc<DeviceSimulator>,
        transport: Arc<sip_core::UdpTransport>,
        local_host: String,
        local_port: u16,
        position_secs: u64,
        alarm_secs: u64,
        metrics: Arc<Metrics>,
        stop: &mut tokio::sync::broadcast::Receiver<()>,
    ) {
        // 用较小的 tick 轮询,各自按间隔触发;间隔为 0 的类型不触发。
        let mut pos_acc = 0u64;
        let mut alarm_acc = 0u64;
        let tick = std::time::Duration::from_secs(1);
        loop {
            tokio::select! {
                _ = stop.recv() => break,
                _ = tokio::time::sleep(tick) => {
                    if position_secs > 0 {
                        pos_acc += 1;
                        if pos_acc >= position_secs {
                            pos_acc = 0;
                            // 北京附近固定坐标(压测只关心上报承载,不关心真实轨迹)。
                            if dev.report_position(&transport, &local_host, local_port, 116.397, 39.908)
                                .await.map(|c| c == 200).unwrap_or(false)
                            {
                                metrics.on_position_reported();
                            }
                        }
                    }
                    if alarm_secs > 0 {
                        alarm_acc += 1;
                        if alarm_acc >= alarm_secs {
                            alarm_acc = 0;
                            if dev.report_alarm(&transport, &local_host, local_port, "压测报警")
                                .await.map(|c| c == 200).unwrap_or(false)
                            {
                                metrics.on_alarm_reported();
                            }
                        }
                    }
                }
            }
        }
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
            server_id: String::new(),
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
            bitrate_kbps: 512,
            active_ratio: 1.0,
            ramp_per_second: 0,
            gb_version: common::GbVersion::V2022,
        }
    }

    #[test]
    fn 编排器创建设备() {
        let sc = test_scenario();
        let orch = Orchestrator::new(&sc, 5).unwrap();
        assert_eq!(orch.device_count(), 5);
    }
}
