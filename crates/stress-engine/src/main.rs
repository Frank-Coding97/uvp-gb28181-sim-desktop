//! stress-engine 独立二进制入口(CLI)。
//!
//! 读取 TOML 场景文件,批量拉起虚拟设备压测上级平台,Ctrl-C 优雅停止。
//!
//! 用法:
//! ```bash
//! stress-engine <场景.toml> <设备数>
//! # 例:stress-engine examples/scenarios/linear.toml 100
//! ```

use std::process::ExitCode;

use stress_engine::{LinearScenario, Orchestrator};

#[tokio::main]
async fn main() -> ExitCode {
    // CLI 也落盘:压测跑一夜后要能回看"第几分钟开始失败"。
    // 目录取当前工作目录下的 `uvp-logs/`(CLI 没有应用数据目录的概念)。
    match common::logging::init_with_file(std::path::PathBuf::from("uvp-logs")) {
        Some(dir) => eprintln!("日志落盘目录: {}", dir.display()),
        None => eprintln!("日志目录不可用,本次仅输出到控制台"),
    }

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("用法: {} <场景.toml> <设备数>", args[0]);
        eprintln!("例:  {} examples/scenarios/linear.toml 100", args[0]);
        return ExitCode::from(2);
    }
    let scenario_path = &args[1];
    let count: usize = match args[2].parse() {
        Ok(n) => n,
        Err(_) => {
            eprintln!("设备数非法: {}", args[2]);
            return ExitCode::from(2);
        }
    };

    // 加载场景。
    let scenario = match LinearScenario::from_toml(scenario_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("加载场景失败: {e}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(path = %scenario_path, count, "加载压测场景");

    // 创建编排器。
    let orch = match Orchestrator::new(&scenario, count) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("创建编排器失败: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Ctrl-C 广播停止信号。
    let (stop_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let stop_tx2 = stop_tx.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("收到退出信号,停止所有设备");
        let _ = stop_tx2.send(());
    });

    tracing::info!("压测启动");
    // CLI 默认不做主动上报施压(0/0);后续可加 --position/--alarm 参数。
    let run_result = orch.run(stop_tx, 0, 0).await;

    // **无论成功还是失败都打印结论**。之前 run() 返回后直接 ExitCode::SUCCESS,
    // CLI 跑完一轮压测什么都不输出 —— 结果只存在于日志行里,没法当 CI 门禁用,
    // 失败分支更是连指标都不给。
    let snapshot = orch.snapshot();
    print_summary(&snapshot);
    match serde_json::to_string_pretty(&snapshot) {
        Ok(json) => println!("\n--- 指标 JSON ---\n{json}"),
        Err(e) => eprintln!("指标序列化失败: {e}"),
    }

    if let Err(e) = run_result {
        eprintln!("压测运行错误: {e}");
        return ExitCode::FAILURE;
    }
    // 本机背压过大时结论不可信,用非零退出码表达 —— 让 CI 里"压测跑完了"
    // 不等于"压测结果可用"。
    if !snapshot.results_trustworthy {
        eprintln!(
            "警告:本机出现背压(local_pressure={}),本轮结论不能当作平台承载上限",
            snapshot.local_pressure
        );
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// 打印人读摘要。字段挑的是"看完能直接下结论"的那几个。
fn print_summary(s: &stress_engine::MetricsSnapshot) {
    println!("\n===== 压测结论 =====");
    println!(
        "设备维度成功率: {:.2}%  (首次尝试 {} 台,成功 {} 次)",
        s.device_success_rate * 100.0,
        s.register_first_attempt,
        s.register_succeeded
    );
    println!(
        "在线峰值: {}  当前在线: {}",
        s.devices_online_peak, s.devices_online
    );
    println!(
        "注册延迟: p50 {} ms / p95 {} ms / p99 {} ms / max {} ms",
        s.register_latency.p50_ms,
        s.register_latency.p95_ms,
        s.register_latency.p99_ms,
        s.register_latency.max_ms
    );
    println!(
        "失败归因: 超时 {} / 平台拒绝 {}(凭据 {} · 忙 {} · 其它 {}) / 本地 {} / 其它 {}",
        s.fail_timeout,
        s.fail_rejected,
        s.fail_rejected_auth,
        s.fail_rejected_overload,
        s.fail_rejected_other,
        s.fail_local,
        s.fail_other
    );
    match s.first_failure_online {
        Some(n) => println!("失败水位: 在线 {n} 台时出现首次失败"),
        None => println!("失败水位: 全程无失败"),
    }
    if s.ramp_throttled > 0 {
        println!("爬坡减速次数: {}(本机跟不上设定速率)", s.ramp_throttled);
    }
    if !s.failure_samples.is_empty() {
        println!("失败样本(可挑一台复现):");
        for sample in s.failure_samples.iter().take(5) {
            let status = sample
                .status
                .map(|c| format!(" status={c}"))
                .unwrap_or_default();
            println!(
                "  {} kind={}{} 在线={}",
                sample.device_id, sample.kind, status, sample.online_at_failure
            );
        }
    }
    println!(
        "结论可信: {}  (本地压力 {})",
        if s.results_trustworthy { "是" } else { "否" },
        s.local_pressure
    );
    println!("====================");
}
