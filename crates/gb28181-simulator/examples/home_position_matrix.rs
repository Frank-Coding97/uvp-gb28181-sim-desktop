//! HomePosition T13.1 矩阵 runner。
//!
//! 当前仓库没有可携带 UVP token/SIP 配置的 CI 网络环境，因此只有显式传入
//! `--offline` 才生成离线 fixture。默认真实路径缺少环境或 runner 未接入时会
//! 非零退出且不写报告。接入真实平台时应把 `fixed_cases()` 映射到
//! DeviceSimulator 网络会话，不要把平台状态机复制进协议 crate。

use gb28181_simulator::home_position::{MatrixReport, HOME_POSITION_SCENARIO_IDS};
use std::env;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

#[derive(Debug)]
struct Args {
    base_url: String,
    token_env: String,
    platform_sip: String,
    sip_config_file: Option<PathBuf>,
    matrix: String,
    report: PathBuf,
    offline: bool,
}

fn main() {
    match run() {
        Ok(()) => {}
        Err(err) => {
            eprintln!("home_position_matrix failed: {err}");
            std::process::exit(2);
        }
    }
}

fn run() -> Result<(), String> {
    let Some(args) = parse_args()? else {
        return Ok(());
    };
    if args.matrix != "home-position-v1" {
        return Err(format!(
            "unsupported --matrix {}; expected home-position-v1",
            args.matrix
        ));
    }
    ensure_loopback_url(&args.base_url)?;
    ensure_loopback_endpoint(&args.platform_sip)?;
    if let Some(path) = &args.sip_config_file {
        validate_sip_config(path)?;
    }

    if !args.offline {
        require_real_runner_environment(&args)?;
        let config_path = args.sip_config_file.clone().unwrap_or_else(|| {
            PathBuf::from(
                env::var("UVP_HOME_POS_SIP_CONFIG_FILE")
                    .expect("require_real_runner_environment checked this variable"),
            )
        });
        validate_sip_config(&config_path)?;
        return Err(
            "真实 HTTP/SIP runner 尚未接入；未生成报告。请保留 T13.1 blocked，使用 --offline 仅验证 schema fixture".into(),
        );
    }

    // 离线 fixture 只用于 schema/profile 单测与本地开发，不能作为 T13.1 网络证据。
    // token 名称可配置，但永远不读取/打印 token 值，也不写入报告。
    let report = MatrixReport::offline("offline-fixture", "offline-fixture");
    report.write_json(&args.report)?;

    for (index, case) in report.cases.iter().enumerate() {
        let operation_id = case
            .operation_ids
            .first()
            .map(String::as_str)
            .unwrap_or("unknown");
        println!(
            "device=fixture-device channel=fixture-channel transport={} scenario={} SN={} Call-ID=fixture-call-{} CSeq={} operationId={} finalStatus={} sipSummary=offline-fixture",
            case.transport.as_str(),
            case.id,
            index + 1,
            index + 1,
            index + 1,
            operation_id,
            case.result,
        );
    }
    println!(
        "matrix=home-position-v1 cases={} report={} mode=offline-fixture (not real HTTP/SIP evidence)",
        HOME_POSITION_SCENARIO_IDS.len(),
        args.report.display()
    );
    Ok(())
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = env::args().skip(1);
    let mut parsed = Args {
        base_url: "http://127.0.0.1:8080".into(),
        token_env: "UVP_HOME_POS_TOKEN".into(),
        platform_sip: "127.0.0.1:5060".into(),
        sip_config_file: None,
        matrix: "home-position-v1".into(),
        report: PathBuf::from("matrix.json"),
        offline: false,
    };

    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            print_help();
            return Ok(None);
        }
        let value = |name: &str, args: &mut std::iter::Skip<env::Args>| {
            args.next()
                .ok_or_else(|| format!("{name} requires a value"))
        };
        match arg.as_str() {
            "--base-url" => parsed.base_url = value("--base-url", &mut args)?,
            "--token-env" => parsed.token_env = value("--token-env", &mut args)?,
            "--platform-sip" => parsed.platform_sip = value("--platform-sip", &mut args)?,
            "--sip-config-file" => {
                parsed.sip_config_file = Some(PathBuf::from(value("--sip-config-file", &mut args)?))
            }
            "--matrix" => parsed.matrix = value("--matrix", &mut args)?,
            "--report" => parsed.report = PathBuf::from(value("--report", &mut args)?),
            "--offline" => parsed.offline = true,
            unknown => return Err(format!("unknown argument {unknown}; use --help")),
        }
    }
    Ok(Some(parsed))
}

fn print_help() {
    println!(
        "home_position_matrix [options]\n\
         --base-url URL             loopback UVP base URL (default http://127.0.0.1:8080)\n\
         --token-env NAME           token env var name; value is never logged (default UVP_HOME_POS_TOKEN)\n\
         --platform-sip HOST:PORT   loopback SIP endpoint (default 127.0.0.1:5060)\n\
         --sip-config-file PATH     optional SIP config; existing file must be mode 0600\n\
         --matrix NAME              home-position-v1 (default)\n\
         --report PATH              JSON fixture path (default matrix.json)\n\
         --offline                  explicitly generate offline fixture; never T13.1 network evidence"
    );
}

fn require_real_runner_environment(args: &Args) -> Result<(), String> {
    let required = [
        "UVP_HOME_POS_BASE_URL",
        "UVP_HOME_POS_TOKEN",
        "UVP_HOME_POS_SIP_ADDR",
        "UVP_HOME_POS_SIP_CONFIG_FILE",
    ];
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|name| {
            env::var(name)
                .ok()
                .map(|value| value.trim().is_empty())
                .unwrap_or(true)
        })
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "真实 runner 缺少环境变量 {:?}；未生成报告。离线 fixture 必须显式使用 --offline",
            missing
        ));
    }
    // 仍要求 CLI 先做 loopback 校验,避免后续接入真实 HTTP/SIP 时绕过安全门禁。
    ensure_loopback_url(&env::var("UVP_HOME_POS_BASE_URL").unwrap())?;
    ensure_loopback_endpoint(&env::var("UVP_HOME_POS_SIP_ADDR").unwrap())?;
    if args.token_env != "UVP_HOME_POS_TOKEN" {
        return Err("真实 runner 只允许从 UVP_HOME_POS_TOKEN 读取 token，禁止改名".into());
    }
    Ok(())
}

fn ensure_loopback_url(value: &str) -> Result<(), String> {
    let rest = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))
        .ok_or_else(|| "--base-url must use http:// or https://".to_string())?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority_host(authority)?;
    if !is_loopback_host(host) {
        return Err(format!("--base-url 必须是 loopback 地址，拒绝 {host}"));
    }
    Ok(())
}

fn ensure_loopback_endpoint(value: &str) -> Result<(), String> {
    let rest = value
        .strip_prefix("udp://")
        .or_else(|| value.strip_prefix("tcp://"))
        .or_else(|| value.strip_prefix("sip://"))
        .unwrap_or(value);
    let authority = rest.split('/').next().unwrap_or_default();
    let (host_part, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, port) = bracketed
            .split_once(']')
            .ok_or_else(|| "--platform-sip IPv6 地址缺少 ]".to_string())?;
        let port = port
            .strip_prefix(':')
            .ok_or_else(|| "--platform-sip 缺少端口".to_string())?;
        (host, port)
    } else {
        authority
            .rsplit_once(':')
            .ok_or_else(|| "--platform-sip 必须为 HOST:PORT".to_string())?
    };
    let port: u16 = port
        .parse()
        .map_err(|_| "--platform-sip 端口非法".to_string())?;
    if port == 0 {
        return Err("--platform-sip 端口不能为 0".into());
    }
    if !is_loopback_host(host_part) {
        return Err(format!(
            "--platform-sip 必须是 loopback 地址，拒绝 {host_part}"
        ));
    }
    Ok(())
}

fn authority_host(authority: &str) -> Result<&str, String> {
    if authority.is_empty() || authority.contains('@') {
        return Err("URL authority 非法".into());
    }
    if let Some(host) = authority.strip_prefix('[') {
        return host
            .split_once(']')
            .map(|(host, _)| host)
            .ok_or_else(|| "URL IPv6 地址缺少 ]".into());
    }
    Ok(authority
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(authority))
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

fn validate_sip_config(path: &Path) -> Result<(), String> {
    let metadata =
        fs::metadata(path).map_err(|err| format!("读取 SIP 配置失败 {}: {err}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("SIP 配置不是普通文件: {}", path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o777;
        if mode != 0o600 {
            return Err(format!(
                "SIP 配置权限必须为 0600，实际 {:o}: {}",
                mode,
                path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_gate_accepts_local_http_and_sip() {
        ensure_loopback_url("http://127.0.0.1:8080/api").unwrap();
        ensure_loopback_url("https://[::1]:8443").unwrap();
        ensure_loopback_endpoint("udp://localhost:5060").unwrap();
        ensure_loopback_endpoint("[::1]:5060").unwrap();
    }

    #[test]
    fn loopback_gate_rejects_remote_http_and_sip() {
        assert!(ensure_loopback_url("http://192.168.1.20:8080").is_err());
        assert!(ensure_loopback_endpoint("10.0.0.2:5060").is_err());
        assert!(ensure_loopback_endpoint("sip://example.test:5060").is_err());
    }
}
