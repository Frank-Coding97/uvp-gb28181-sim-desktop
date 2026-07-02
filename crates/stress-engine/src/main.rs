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
    common::logging::init();

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
    if let Err(e) = orch.run(stop_tx).await {
        eprintln!("压测运行错误: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
