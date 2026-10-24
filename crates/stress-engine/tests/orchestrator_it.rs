//! M3 集成测试:编排器批量拉起设备,对 mock 平台完成注册。
//!
//! 验证 stress-engine 的核心闭环:场景生成 N 设备 → 编排器共享传输批量注册 →
//! mock 平台收到 N 个 REGISTER 并回 200。

use sip_core::SipTransport;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use scenario::{DeviceInfoTemplate, LinearScenario, MediaProfile};
use sip_core::{Headers, Response, SipMessage, UdpTransport};
use stress_engine::Orchestrator;

/// 起一个 mock 平台:对任意 REGISTER 直接回 200(简化,不走 401)。
/// 返回 (transport, 端口, 收到的 REGISTER 数, 首个设备来源地址)。
///
/// 首个来源地址即某个共享传输分片的 socket —— 集成测试用它定位设备的收流端口
/// (例如向它灌注垃圾报文制造本地压力,验证结论可信度翻转)。
async fn spawn_mock_platform() -> (
    Arc<UdpTransport>,
    u16,
    Arc<AtomicU32>,
    Arc<Mutex<Option<SocketAddr>>>,
) {
    let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
    let port = platform.local_addr().unwrap().port();
    let counter = Arc::new(AtomicU32::new(0));
    let first_from = Arc::new(Mutex::new(None));

    // 平台需要按各设备 Call-ID 接收 REGISTER;这里用通配接收:
    // 由于 transport 按 Call-ID 路由,mock 平台改用裸 socket 更简单。
    // 但为复用现有 transport,采用 register_inbound 对平台域 AOR 收 REGISTER。
    let plat = platform.clone();
    let cnt = counter.clone();
    let first = first_from.clone();
    // REGISTER 的 Request-URI = sip:<domain>@host:port,AOR = domain。
    let mut inbound = platform.register_inbound("34020000002000000001");
    tokio::spawn(async move {
        while let Some(inc) = inbound.recv().await {
            if let SipMessage::Request(req) = &inc.message {
                cnt.fetch_add(1, Ordering::Relaxed);
                {
                    let mut guard = first.lock().unwrap();
                    if guard.is_none() {
                        *guard = Some(inc.from);
                    }
                }
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

    (platform, port, counter, first_from)
}

fn scenario_for(port: u16) -> LinearScenario {
    scenario_for_ramp(port, 0)
}

fn scenario_for_ramp(port: u16, ramp_per_second: u32) -> LinearScenario {
    LinearScenario {
        base_device_id: "34020000001320000001".into(),
        password: "12345678".into(),
        server_host: "127.0.0.1".into(),
        server_port: port,
        server_domain: "34020000002000000001".into(),
        inbound_identity_mode: common::InboundIdentityMode::Compatible,
        transport: common::Transport::Udp,
        heartbeat_interval_secs: 3600,
        register_expires_secs: 3600,
        register_retry_max_secs: 10,
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
        ramp_per_second,
        gb_version: common::GbVersion::V2022,
    }
}

#[tokio::test]
async fn 编排器批量注册_平台收到全部() {
    let (_platform, port, counter, _first_from) = spawn_mock_platform().await;

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
    tokio::time::timeout(std::time::Duration::from_secs(3), run)
        .await
        .expect("编排器应在停止后 3 秒内完成")
        .expect("编排器任务不应 panic")
        .expect("主动停止应正常完成");

    // 平台应至少收到 N 个 REGISTER(可能因重传更多)。
    assert!(
        seen >= N as u32,
        "平台仅收到 {seen} 个 REGISTER,期望 >= {N}"
    );
}

/// 爬坡中途收到停止:应优雅退出(Ok),且没有把剩余设备继续拉起。
///
/// 覆盖 scheduler 的爬坡等待 select 分支:停止信号必须打断 `sleep_until`,
/// 否则停止后仍会按节拍继续创建新设备。
#[tokio::test]
async fn 爬坡中途停止_优雅完成且不继续拉起设备() {
    let (_platform, port, counter, _first) = spawn_mock_platform().await;

    // 20 台/s × 30 台 = 1.5s 爬完;300ms 停止时最多拉起约 6 台。
    const N: usize = 30;
    let sc = scenario_for_ramp(port, 20);
    let orch = Arc::new(Orchestrator::new(&sc, N).unwrap());

    let (stop_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let stop_for_run = stop_tx.clone();
    let orch_for_run = Arc::clone(&orch);
    let run = tokio::spawn(async move { orch_for_run.run(stop_for_run, 0, 0).await });

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let _ = stop_tx.send(());
    tokio::time::timeout(std::time::Duration::from_secs(5), run)
        .await
        .expect("爬坡中停止也应在 5 秒内优雅完成")
        .expect("编排器任务不应 panic")
        .expect("主动停止应返回 Ok");

    let seen = counter.load(Ordering::Relaxed);
    assert!(
        seen < N as u32,
        "停止后不应继续拉起设备:平台已见 {seen} 次 REGISTER,目标 {N}"
    );
}

/// 健康环境(平台秒回 200、本机无压力)下整轮爬坡:
/// 不应误触背压节流,结论可信位保持 true,全部设备注册成功。
#[tokio::test]
async fn 健康爬坡全程不触发背压且结论可信() {
    let (_platform, port, counter, _first) = spawn_mock_platform().await;

    const N: usize = 20;
    let sc = scenario_for_ramp(port, N as u32); // 一秒内爬完,至少跨一个判定窗口
    let orch = Arc::new(Orchestrator::new(&sc, N).unwrap());

    let (stop_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let stop_for_run = stop_tx.clone();
    let orch_for_run = Arc::clone(&orch);
    let run = tokio::spawn(async move { orch_for_run.run(stop_for_run, 0, 0).await });

    // 等爬坡完成 + 注册往返。
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let _ = stop_tx.send(());
    tokio::time::timeout(std::time::Duration::from_secs(5), run)
        .await
        .expect("停止后应 5 秒内完成")
        .expect("编排器任务不应 panic")
        .expect("主动停止应返回 Ok");

    assert!(
        counter.load(Ordering::Relaxed) >= N as u32,
        "平台应收到全部 {N} 个 REGISTER"
    );
    let throttled = orch.metrics.ramp_throttled.load(Ordering::Relaxed);
    assert_eq!(throttled, 0, "健康环境不得误报背压节流");
    let snap = orch.snapshot();
    assert!(
        snap.results_trustworthy,
        "无本地压力时结论应可信(local_pressure={})",
        snap.local_pressure
    );
    assert!(snap.register_succeeded >= N as u64, "全部设备注册成功");
}

/// 向设备共享传输分片灌注垃圾报文:传输层限流/解析失败必须进入本地压力,
/// 并把快照的 `results_trustworthy` 翻转为 false —— 这是"压测结论可信度"
/// 链路(传输计数 → local_pressure → 可信位)的端到端验证。
#[tokio::test]
async fn 传输层被灌垃圾后本地压力翻转结论可信度() {
    let (_platform, port, counter, first_from) = spawn_mock_platform().await;

    const N: usize = 2;
    let sc = scenario_for(port);
    let orch = Arc::new(Orchestrator::new(&sc, N).unwrap());

    let (stop_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let stop_for_run = stop_tx.clone();
    let orch_for_run = Arc::clone(&orch);
    let run = tokio::spawn(async move { orch_for_run.run(stop_for_run, 0, 0).await });

    // 等首个设备 REGISTER 到达,拿到它所在分片的收流地址。
    let shard_addr = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if let Some(addr) = *first_from.lock().unwrap() {
                break addr;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "3 秒内应收到首个 REGISTER(counter={})",
                counter.load(Ordering::Relaxed)
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    };

    // 从同 IP 的裸 socket 灌 40 个近 MTU 垃圾包:突发令牌 400、按 MTU 等价字节计费,
    // 约 8~9 个大包即可耗尽 —— 必然触发 rate_limited 或 parse_errors。
    let junk = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let payload = vec![0u8; 1300];
    for _ in 0..40 {
        junk.send_to(&payload, shard_addr).await.unwrap();
    }

    // 本地压力应在短时间内可见。
    let flipped = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let t = orch.transport_stats();
            if t.rate_limited + t.parse_errors > 0 {
                break true;
            }
            if std::time::Instant::now() > deadline {
                break false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    };
    assert!(
        flipped,
        "垃圾报文应触发传输层限流或解析失败计数(rate_limited={}, parse_errors={})",
        orch.transport_stats().rate_limited,
        orch.transport_stats().parse_errors
    );

    let snap = orch.snapshot();
    assert!(snap.local_pressure > 0, "本地压力必须累计");
    assert!(
        !snap.results_trustworthy,
        "本地压力非 0 时结论必须翻转为不可信"
    );

    // 本地压力不影响优雅停止。
    let _ = stop_tx.send(());
    tokio::time::timeout(std::time::Duration::from_secs(5), run)
        .await
        .expect("停止后应 5 秒内完成")
        .expect("编排器任务不应 panic")
        .expect("本地压力下主动停止仍应返回 Ok");
}
