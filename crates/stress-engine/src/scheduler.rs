//! 压测调度器(编排器):批量拉起设备 + 生命周期管理 + 指标聚合。

use std::sync::Arc;

use common::{Error, Result};
use gb28181_simulator::DeviceSimulator;
use scenario::Scenario;
use sip_core::SipTransport;

use crate::metrics::Metrics;

/// 每个共享 UDP socket 承载的设备数上限。
///
/// 为什么要分片:原来 10k 设备全挤在**一个** socket + **一个**接收循环上,
/// 注册风暴时内核接收缓冲一溢出就丢包,而丢包会被上层统计成"事务超时",
/// 于是本机瓶颈被误读成平台过载 —— 压测结论直接失真。
/// 分片同时把接收循环并行化,也更接近"很多台设备各有自己五元组"的真实拓扑。
const DEVICES_PER_SOCKET: usize = 64;

/// 爬坡减速的最大倍数(闭环背压)。
///
/// 16 倍(如 200/s → 12/s)已经足够慢到让本机喘过来;再慢就该判定
/// "本机根本跑不动这个规模",继续等下去只是把结论推迟。
const MAX_GAP_MULTIPLIER: u32 = 16;

/// 一个传输分片:共享 socket + 本端地址。
struct TransportShard {
    transport: Arc<dyn SipTransport>,
    local_host: String,
    local_port: u16,
}

/// 等待全部设备/上报子任务。JoinError 在非主动停止时向上聚合，且先停止再 drain。
async fn await_workers(
    mut handles: tokio::task::JoinSet<()>,
    stop: &tokio::sync::broadcast::Sender<()>,
    mut completion_stop: tokio::sync::broadcast::Receiver<()>,
    mut externally_stopping: bool,
) -> Result<()> {
    let mut worker_errors = Vec::new();
    while !handles.is_empty() {
        tokio::select! {
            stop_result = completion_stop.recv(), if !externally_stopping => {
                if !matches!(
                    stop_result,
                    Err(tokio::sync::broadcast::error::RecvError::Closed)
                ) {
                    externally_stopping = true;
                }
            }
            result = handles.join_next() => {
                let Some(result) = result else { break };
                if let Err(error) = result {
                    // 取消只有在主动停止已线性化时才是预期；panic 永远是运行异常，
                    // 不能被稍后到达的 stop 掩盖。
                    if !externally_stopping {
                        externally_stopping = !matches!(
                            completion_stop.try_recv(),
                            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
                                | Err(tokio::sync::broadcast::error::TryRecvError::Closed)
                        );
                    }
                    if error.is_cancelled() && externally_stopping {
                        tracing::warn!(%error, "主动停止期间压测设备任务被取消");
                    } else {
                        tracing::error!(%error, "压测设备任务异常退出，正在停止其余任务");
                        worker_errors.push(error.to_string());
                        let _ = stop.send(());
                    }
                }
            }
        }
    }
    tracing::info!("所有设备已停止");
    if worker_errors.is_empty() {
        Ok(())
    } else {
        Err(Error::Io(std::io::Error::other(format!(
            "{} 个压测设备子任务异常: {}",
            worker_errors.len(),
            worker_errors.join("; ")
        ))))
    }
}

/// 压测编排器:持有场景、批量设备、共享指标。
pub struct Orchestrator {
    devices: Vec<Arc<DeviceSimulator>>,
    /// 所有设备共享的原子指标,可随时 snapshot 读取。
    pub metrics: Arc<Metrics>,
    /// 爬坡速率:每秒拉起多少台(0 = 一次性)。
    ramp_per_second: u32,
    /// 用于探测本机对平台可达地址的目标端点。
    server_host: String,
    server_port: u16,
    /// 信令传输方式(UDP / TCP)。决定建哪种共享传输,并据此填 Via。
    transport_kind: common::Transport,
    /// 运行中的传输分片统计句柄(供快照聚合本地压力指标)。
    transport_stats: parking_lot::Mutex<Vec<Arc<sip_core::TransportStats>>>,
}

impl Orchestrator {
    /// 用场景批量创建设备实例(含共享 Metrics 注入)。
    pub fn new(scenario: &dyn Scenario, count: usize) -> Result<Self> {
        if !(1..=10_000).contains(&count) {
            return Err(common::Error::Config(
                "压测设备数量必须在 1..=10000 之间".into(),
            ));
        }
        let configs = scenario.generate(count)?;
        if configs.len() != count {
            return Err(common::Error::Config(format!(
                "场景生成设备数异常:期望 {count},实际 {}",
                configs.len()
            )));
        }
        let first = configs
            .first()
            .ok_or_else(|| common::Error::Config("场景未生成任何设备".into()))?;
        let server_host = first.server_host.clone();
        let server_port = first.server_port;
        let transport_kind = first.transport;
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
            server_host,
            server_port,
            transport_kind,
            transport_stats: parking_lot::Mutex::new(Vec::new()),
        })
    }

    /// 当前累计的本地压力(队列丢弃 + 发送失败 + 解析失败 + 本地归因失败)。
    ///
    /// 与 `MetricsSnapshot::local_pressure` 同一个定义,但这里避免构造整个快照
    /// (爬坡窗口每秒一次,而快照会克隆失败样本 Vec)。
    fn local_pressure(&self) -> u64 {
        let t = self.transport_stats();
        t.dropped_queue_full
            + t.send_errors
            + t.parse_errors
            + t.rate_limited
            + t.anti_amplification_blocked
            + self
                .metrics
                .fail_local
                .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 聚合全部传输分片的统计。
    pub fn transport_stats(&self) -> sip_core::TransportStatsSnapshot {
        let guard = self.transport_stats.lock();
        let mut total = sip_core::TransportStatsSnapshot::default();
        for stats in guard.iter() {
            total.merge(&stats.snapshot());
        }
        total
    }

    /// 指标快照(含传输层本地压力,用于判断结论是否可信)。
    pub fn snapshot(&self) -> crate::metrics::MetricsSnapshot {
        self.metrics
            .snapshot_with_transport(Some(self.transport_stats()))
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
        let stop_rx = stop.subscribe();
        self.run_with_stop_receiver(stop, stop_rx, position_secs, alarm_secs)
            .await
    }

    /// 使用调用方预先创建的停止接收器运行。
    ///
    /// 桌面端在发布“运行中”状态前创建接收器，从而保证用户启动后立即点击停止时，
    /// 广播信号也不会因调度任务尚未首次轮询而丢失。
    pub async fn run_with_stop_receiver(
        &self,
        stop: tokio::sync::broadcast::Sender<()>,
        mut ramp_stop: tokio::sync::broadcast::Receiver<()>,
        position_secs: u64,
        alarm_secs: u64,
    ) -> Result<()> {
        // 该 receiver 从入口即存在，用于准确区分“外部主动停止”和 worker 异常后内部停机。
        let completion_stop = stop.subscribe();
        let mut externally_stopping = false;
        // 当前设备发送路径只接受字面平台 IP；在创建任何 transport/task 前失败，
        // 绝不因解析失败而退化成关闭来源过滤。
        let platform_ip = self
            .server_host
            .parse::<std::net::IpAddr>()
            .map_err(|_| Error::Config("平台地址必须是字面 IP（当前不支持域名）".into()))?;
        let platform_addr = std::net::SocketAddr::new(platform_ip, self.server_port);

        // 不能把 bind 得到的 0.0.0.0 写进 Via/Contact。通过连接目标平台探测
        // 操作系统实际选择的出口地址(UDP connect 不会发送数据)。
        let probe = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
        probe.connect(platform_addr).await?;
        let local_host = probe.local_addr()?.ip().to_string();

        // 按 DEVICES_PER_SOCKET 分片建共享传输,避免单 socket + 单接收循环成为
        // 隐形瓶颈(见常量注释)。
        let shard_count = self.devices.len().div_ceil(DEVICES_PER_SOCKET).max(1);
        let mut shards: Vec<TransportShard> = Vec::with_capacity(shard_count);
        let mut shard_stats = Vec::with_capacity(shard_count);
        for _ in 0..shard_count {
            // 可信来源与 Router 同时构造，在 UDP recv/TCP accept loop 启动前生效。
            let transport: Arc<dyn SipTransport> = match self.transport_kind {
                common::Transport::Udp => {
                    sip_core::UdpTransport::bind_with_trusted_ips("0.0.0.0:0", vec![platform_ip])
                        .await?
                }
                common::Transport::Tcp => {
                    sip_core::TcpTransport::bind_with_trusted_ips("0.0.0.0:0", vec![platform_ip])
                        .await?
                }
            };
            let local_port = transport.local_addr()?.port();
            shard_stats.push(transport.stats());
            shards.push(TransportShard {
                transport,
                local_host: local_host.clone(),
                local_port,
            });
        }
        // 锁只在同步区间持有:parking_lot 的 guard 不是 Send,不能跨 await。
        {
            let mut stats_guard = self.transport_stats.lock();
            *stats_guard = shard_stats;
        }
        tracing::info!(
            %local_host,
            shard_count,
            devices_per_socket = DEVICES_PER_SOCKET,
            transport = %self.transport_kind,
            "共享 SIP 传输已分片绑定"
        );

        // 爬坡:按 1s/ramp 的**均匀间隔**逐台发放,而不是"攒一批后突发"。
        // 批量突发会在批内瞬时打满,既不像真实设备上线过程,也容易自己把自己打挂。
        let ramp = self.ramp_per_second;
        let base_gap = (ramp > 0).then(|| {
            std::time::Duration::from_nanos((1_000_000_000f64 / ramp as f64).max(1.0) as u64)
        });
        let mut next_slot = tokio::time::Instant::now();

        // ── 闭环背压 ──
        //
        // 爬坡原来是**开环**的:完全由时间驱动,不读任何反馈。本机一旦开始丢包
        // (socket 缓冲不够、fd 紧张、CPU 打满),它仍按原速继续拉设备,
        // 于是失败里混进了本机瓶颈,而 `results_trustworthy` 只能**事后**告诉你
        // "这轮不可信" —— 一小时压测白跑。
        //
        // 现在每个爬坡窗口结束时看一次本地压力增量:涨了就把间隔翻倍(上限
        // [`MAX_GAP_MULTIPLIER`]),没涨就逐步收回。这是控制"加压速率"而不是
        // "目标台数",所以不会偷偷少起设备,只会起得慢一些。
        let mut gap_multiplier: u32 = 1;
        let mut pressure_at_window = self.local_pressure();

        // 批量启动设备 run 任务。
        let mut handles = tokio::task::JoinSet::new();
        for (i, dev) in self.devices.iter().enumerate() {
            // 爬坡等待必须可被停止打断,否则停止后仍会继续创建新设备,且晚订阅者
            // 收不到之前已经发送过的 broadcast 信号。
            if let Some(gap) = base_gap {
                next_slot += gap * gap_multiplier;
                tokio::select! {
                    _ = ramp_stop.recv() => {
                        externally_stopping = true;
                        tracing::info!(started = i, "爬坡阶段收到停止信号");
                        break;
                    }
                    _ = tokio::time::sleep_until(next_slot) => {}
                }
                // 每上线 ramp 台(≈ 一个爬坡窗口)输出一次进度并做一次背压判定。
                if ramp > 0 && (i as u32 + 1) % ramp == 0 {
                    let online = self
                        .metrics
                        .devices_online
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let pressure = self.local_pressure();
                    let delta = pressure.saturating_sub(pressure_at_window);
                    pressure_at_window = pressure;

                    if delta > 0 {
                        // 本机在这一窗口里丢过东西:减速。
                        let before = gap_multiplier;
                        gap_multiplier = (gap_multiplier * 2).min(MAX_GAP_MULTIPLIER);
                        if gap_multiplier != before {
                            self.metrics.on_ramp_throttled();
                        }
                        tracing::warn!(
                            started = i + 1,
                            online,
                            local_pressure_delta = delta,
                            effective_ramp_per_sec = ramp / gap_multiplier.max(1),
                            "本机出现背压(丢包/发送失败),爬坡减速 ——                              此时的失败不能算作平台承载上限"
                        );
                    } else if gap_multiplier > 1 {
                        // 压力停止增长:逐步收回速率,而不是一次跳回原速
                        // (一次跳回容易立刻再次触发,变成振荡)。
                        gap_multiplier = (gap_multiplier / 2).max(1);
                        tracing::info!(
                            started = i + 1,
                            effective_ramp_per_sec = ramp / gap_multiplier.max(1),
                            "本机压力缓解,爬坡恢复提速"
                        );
                    }

                    tracing::info!(
                        started = i + 1,
                        target = self.devices.len(),
                        online,
                        "爬坡进度"
                    );
                }
            }
            // 先为本轮所有潜在子任务预建 receiver，再检查全局停止状态：
            // stop 若发生在订阅前会被 ramp_stop 捕获，发生在订阅后则由子任务 receiver 捕获。
            let mut stop_rx = stop.subscribe();
            let stop_rep = (position_secs > 0 || alarm_secs > 0).then(|| stop.subscribe());
            match ramp_stop.try_recv() {
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {}
                _ => {
                    externally_stopping = true;
                    tracing::info!(started = i, "启动设备前收到停止信号");
                    break;
                }
            }

            let shard = &shards[i / DEVICES_PER_SOCKET];
            let dev_clone = dev.clone();
            let tp = Arc::clone(&shard.transport);
            let host = shard.local_host.clone();
            let port = shard.local_port;
            handles.spawn(async move {
                dev_clone
                    .run(tp, host, port, async move {
                        let _ = stop_rx.recv().await;
                    })
                    .await;
            });

            // 主动上报施压:为该设备挂一个周期任务(定位/报警),随 stop 退出。
            if let Some(mut stop_rep) = stop_rep {
                let dev_rep = dev.clone();
                let tp_rep = Arc::clone(&shard.transport);
                let host_rep = shard.local_host.clone();
                let port_rep = shard.local_port;
                let metrics = Arc::clone(&self.metrics);
                handles.spawn(async move {
                    Self::report_loop(
                        dev_rep,
                        tp_rep,
                        host_rep,
                        port_rep,
                        position_secs,
                        alarm_secs,
                        metrics,
                        &mut stop_rep,
                    )
                    .await;
                });
            }
        }

        tracing::info!(task_count = handles.len(), "设备任务已启动");

        // 按完成顺序 drain，任何非主动停止下的 panic/cancel 都触发全局 stop，
        // 但仍等待所有其余任务退出，避免 detach。
        await_workers(handles, &stop, completion_stop, externally_stopping).await
    }

    /// 单设备的周期主动上报循环(定位/报警),用于压测施压。随 stop 广播退出。
    #[allow(clippy::too_many_arguments)]
    async fn report_loop(
        dev: Arc<DeviceSimulator>,
        transport: Arc<dyn SipTransport>,
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
            inbound_identity_mode: common::InboundIdentityMode::Compatible,
            transport: Transport::Udp,
            heartbeat_interval_secs: 3600, // 长心跳避免测试中发送
            register_expires_secs: 3600,
            register_retry_max_secs: 10,
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

    #[tokio::test]
    async fn 非主动停止的worker_panic向上返回错误并drain其余任务() {
        let (stop, _) = tokio::sync::broadcast::channel(1);
        let completion_stop = stop.subscribe();
        let mut child_stop = stop.subscribe();
        let mut workers = tokio::task::JoinSet::new();
        workers.spawn(async { panic!("boom") });
        workers.spawn(async move {
            let _ = child_stop.recv().await;
        });

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            await_workers(workers, &stop, completion_stop, false),
        )
        .await
        .expect("异常后应停止并 drain 全部 worker");
        assert!(result.is_err(), "非主动停止的 panic 不得降级为成功");
    }

    #[tokio::test]
    async fn 主动停止期间的worker取消不误报运行失败() {
        let (stop, _) = tokio::sync::broadcast::channel(1);
        let completion_stop = stop.subscribe();
        let mut workers = tokio::task::JoinSet::new();
        let abort = workers.spawn(std::future::pending::<()>());

        let _ = stop.send(());
        abort.abort();
        let result = await_workers(workers, &stop, completion_stop, false).await;
        assert!(result.is_ok(), "先发生的主动停止应准确识别后续取消");
    }

    #[tokio::test]
    async fn panic先于stop时后到停止不得掩盖异常() {
        let (stop, _) = tokio::sync::broadcast::channel(1);
        let completion_stop = stop.subscribe();
        let mut workers = tokio::task::JoinSet::new();
        workers.spawn(async { panic!("panic-first") });
        tokio::task::yield_now().await;
        let _ = stop.send(());

        let result = await_workers(workers, &stop, completion_stop, false).await;
        assert!(
            result.is_err(),
            "后到的 stop 不得把已发生 panic 改判为主动停止"
        );
    }
}
