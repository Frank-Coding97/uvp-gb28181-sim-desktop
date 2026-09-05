//! 一键导出诊断包。
//!
//! # 为什么需要
//!
//! 商业支持流程的第一步是"把现场发过来"。此前用户能导出的只有压测报告
//! (指标 + 时序),不含日志、不含运行环境 —— 而绝大多数现场问题(注册不上、
//! 收不到流、跑一半掉线)靠指标是看不出来的,真正有用的是日志加上
//! "这台机器的 fd 限额是多少""ffmpeg 在不在""版本号到底是哪个"。
//!
//! # 包里有什么
//!
//! ```text
//! uvp-diagnostics-<纳秒>-<计数>.zip
//! ├── README.txt          说明 + 隐私声明
//! ├── environment.json    工具版本 / OS / CPU 核数 / fd 限额 / 日志目录 / ffmpeg 可用性
//! └── logs/uvp-*.log      滚动日志文件(全部保留副本)
//! ```
//!
//! # 隐私
//!
//! 包里**不含** SIP 密码:凭据存 OS 钥匙串,类型是 `common::Secret`(序列化即报错),
//! 而日志里从来不打印密码字段。SIP 报文追踪的鉴权头在推给界面之前已经脱敏。
//! 包里**含有**平台地址、设备编号、本机路径与用户名(路径里可能带),
//! 这些是排障必需的,`README.txt` 里向用户明确说明。

use std::io::Write;
use std::path::{Path, PathBuf};

/// 运行环境快照。
#[derive(serde::Serialize)]
pub struct Environment {
    /// 工具版本(单一来源:workspace 的 `Cargo.toml`)。
    pub version: String,
    /// 目标三元组(编译期确定,用于确认用户下的是哪个产物)。
    pub target: String,
    /// 构建类型。
    pub profile: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
    /// 可用 CPU 并行度。
    pub cpu_parallelism: usize,
    /// 进程可打开的文件描述符软上限。
    ///
    /// 这一项直接决定"一万台设备能不能跑起来":压测按 64 台一个 socket 分片,
    /// 加上媒体端口与文件句柄,fd 不够的表现是"注册到某个数量就大面积失败",
    /// 极易被误判成平台承载不住。
    pub fd_limit: Option<u64>,
    /// fd 限额的原文。`unlimited` 是合法答案,但不是一个数字 ——
    /// 只留 `Option<u64>` 的话"无上限"和"读不出来"会被混成同一个 None。
    pub fd_limit_raw: String,
    /// 日志目录;None 表示本次运行没有落盘。
    pub log_dir: Option<String>,
    /// 因写队列满被丢弃的日志行数(非 0 说明日志文件不完整)。
    pub log_lines_dropped: u64,
    /// 临时产物目录。
    pub work_dir: String,
    /// ffmpeg 是否可用(实时采集与容器转封装依赖它)。
    pub ffmpeg_available: bool,
    /// 应用托管报告/诊断产物的默认自动清理策略。
    pub artifact_retention: crate::artifacts::RetentionPolicy,
    /// 导出时刻(Unix 秒)。
    pub exported_at_epoch_secs: u64,
}

impl Environment {
    /// 采集当前环境。
    pub fn collect() -> Self {
        Environment {
            version: env!("CARGO_PKG_VERSION").to_string(),
            target: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            cpu_parallelism: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(0),
            fd_limit: fd_soft_limit(),
            fd_limit_raw: fd_soft_limit_raw(),
            log_dir: common::logging::log_dir().map(|p| p.display().to_string()),
            log_lines_dropped: common::logging::file_dropped(),
            work_dir: media_rtp::work_dir().display().to_string(),
            ffmpeg_available: media_rtp::ffmpeg_bin().is_some(),
            artifact_retention: crate::artifacts::RetentionPolicy::default(),
            exported_at_epoch_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }
}

/// 进程的 fd 软上限。
///
/// 用 `getrlimit(RLIMIT_NOFILE)` 而不是 `sh -c "ulimit -n"`:后者在 macOS 上会报
/// `unlimited`(与内核实际给进程的软上限不符),而且要起一个子进程。
/// `RLIM_INFINITY` 时返回 None,由 [`fd_soft_limit_raw`] 给出可读原文。
#[cfg(unix)]
// `rlim_t` 在主流目标上就是 u64,但并非所有 Unix 都如此;保留 `into()` 换取可移植性,
// 代价是 clippy 在 u64 目标上把它判成多余转换。
#[allow(clippy::useless_conversion)]
fn fd_soft_limit() -> Option<u64> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: 传入合法的 rlimit 指针,getrlimit 只写这一块内存。
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) };
    if rc != 0 || lim.rlim_cur == libc::RLIM_INFINITY {
        return None;
    }
    Some(lim.rlim_cur.into())
}

#[cfg(not(unix))]
fn fd_soft_limit() -> Option<u64> {
    None
}

/// fd 软上限的可读原文。
#[cfg(unix)]
fn fd_soft_limit_raw() -> String {
    match fd_soft_limit() {
        Some(n) => n.to_string(),
        None => {
            let mut lim = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: 同上。
            let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) };
            if rc == 0 && lim.rlim_cur == libc::RLIM_INFINITY {
                "unlimited".into()
            } else {
                "unknown".into()
            }
        }
    }
}

#[cfg(not(unix))]
fn fd_soft_limit_raw() -> String {
    // Windows 没有等价的进程级 fd 限额概念(句柄上限由系统资源决定)。
    "n/a (windows)".into()
}

/// 诊断包的说明文件。
fn readme(env: &Environment) -> String {
    format!(
        "UVP GB28181 桌面端 诊断包\n\
         ================================\n\n\
         版本: {version} ({profile}, {os}/{arch})\n\
         导出时刻(Unix 秒): {at}\n\n\
         内容\n\
         ----\n\
         environment.json  运行环境快照(版本 / CPU / fd 限额 / ffmpeg 可用性)\n\
         logs/             滚动日志文件\n\n\
         隐私说明\n\
         --------\n\
         不包含: SIP 密码(存于操作系统钥匙串,且日志从不打印该字段);\n\
         \x20        SIP 报文里的鉴权头(推给界面前已脱敏)。\n\
         包含:   上级平台地址、设备编号、本机文件路径(可能含用户名)、\n\
         \x20        以及日志里出现的其它联调参数。这些是排障必需的信息,\n\
         \x20        请只发送给你信任的技术支持渠道。\n",
        version = env.version,
        profile = env.profile,
        os = env.os,
        arch = env.arch,
        at = env.exported_at_epoch_secs,
    )
}

/// 把诊断信息打成一个 zip,返回文件路径。
///
/// `out_dir` 为输出目录(与压测报告同一个位置)。日志文件读取失败时**跳过该文件继续**
/// —— 少一个日志文件的诊断包也远比"导出失败什么都没有"有用。
pub fn export(
    out_dir: &Path,
    preserve: &std::collections::HashSet<PathBuf>,
) -> Result<PathBuf, String> {
    crate::artifacts::create_secure_dir(out_dir)
        .map_err(|error| format!("创建导出目录失败: {error}"))?;
    let pre_health = crate::artifacts::cleanup(
        out_dir,
        crate::artifacts::RetentionPolicy::default(),
        preserve,
    );
    if !pre_health.healthy {
        tracing::warn!(?pre_health, "诊断导出前产物清理未完全健康");
    }

    let env = Environment::collect();
    let (nanos, counter) = crate::artifacts::unique_timestamp_counter();
    let path = out_dir.join(format!("uvp-diagnostics-{nanos}-{counter}.zip"));
    let mut staged = crate::artifacts::AtomicFile::new(&path)
        .map_err(|error| format!("创建诊断包 staging 失败: {error}"))?;
    let mut zip = zip::ZipWriter::new(staged.file_mut());
    let options: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let add = |zip: &mut zip::ZipWriter<&mut std::fs::File>, name: &str, bytes: &[u8]| {
        zip.start_file(name, options)
            .and_then(|_| zip.write_all(bytes).map_err(Into::into))
            .map_err(|error| format!("写入 {name} 失败: {error}"))
    };

    add(&mut zip, "README.txt", readme(&env).as_bytes())?;
    let env_json =
        serde_json::to_vec_pretty(&env).map_err(|error| format!("环境快照序列化失败: {error}"))?;
    add(&mut zip, "environment.json", &env_json)?;

    let mut logs_added = 0usize;
    if let Some(dir) = common::logging::log_dir() {
        let mut files = common::logfile::list_log_files(&dir);
        files.sort();
        for log in files {
            let Some(name) = log.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            match std::fs::read(&log) {
                Ok(bytes) => {
                    add(&mut zip, &format!("logs/{name}"), &bytes)?;
                    logs_added += 1;
                }
                Err(error) => {
                    tracing::debug!(%error, file = %log.display(), "诊断包跳过一个日志文件");
                }
            }
        }
    }

    zip.finish()
        .map_err(|error| format!("诊断包收尾失败: {error}"))?;
    let published = staged
        .publish()
        .map_err(|error| format!("原子发布诊断包失败: {error}"))?;
    if let Some(warning) = published.durability_warning() {
        tracing::warn!(warning, path = %published.path().display(), "诊断包持久性降级");
    }
    let path = published.path().to_path_buf();

    let mut preserved = preserve.clone();
    preserved.insert(path.clone());
    let health = crate::artifacts::cleanup(
        out_dir,
        crate::artifacts::RetentionPolicy::default(),
        &preserved,
    );
    if !health.healthy {
        tracing::warn!(?health, "诊断导出后产物清理未完全健康");
    }
    // published guard 一直保留到 preserve 清理交接完成，避免并发 cleanup 删除 final。
    drop(published);
    tracing::info!(path = %path.display(), logs_added, "已导出诊断包");
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "uvp-diag-{label}-{}",
            crate::artifacts::unique_suffix()
        ))
    }

    #[test]
    fn 环境快照包含排障必需项() {
        let env = Environment::collect();
        assert!(!env.version.is_empty(), "版本号不能为空,否则报障时对不上");
        assert!(env.cpu_parallelism > 0);
        assert!(matches!(env.profile, "debug" | "release"));
        assert!(
            env.exported_at_epoch_secs > 1_700_000_000,
            "时间戳应是现实时间"
        );
        assert!(!env.work_dir.is_empty());
        // fd 限额直接决定万级压测能不能跑起来,必须有个明确答案 ——
        // 数字、unlimited、或明确的 unknown,不能是空字符串。
        assert!(!env.fd_limit_raw.is_empty());
        #[cfg(unix)]
        assert!(
            env.fd_limit.is_some() || env.fd_limit_raw == "unlimited",
            "类 Unix 上应给出数字或 unlimited,实际 {}",
            env.fd_limit_raw
        );
    }

    #[test]
    fn 说明文件写清了隐私边界() {
        // 这段文案是产品承诺的一部分:用户据此判断能不能把包发出去。
        let text = readme(&Environment::collect());
        assert!(text.contains("不包含"));
        assert!(text.contains("钥匙串"), "必须说明密码为什么不在包里");
        assert!(text.contains("脱敏"), "必须说明鉴权头已脱敏");
        assert!(text.contains("包含"), "也必须说明包里确实有什么");
    }

    #[test]
    fn 导出产出可打开的zip且含必需条目() {
        let dir = test_dir("export");
        let _ = std::fs::remove_dir_all(&dir);
        let path = export(&dir, &std::collections::HashSet::new()).expect("导出应成功");
        assert!(path.exists() && path.extension().is_some_and(|e| e == "zip"));

        // 真正打开读一遍:只断言"文件存在"会漏掉写坏的 zip。
        let file = std::fs::File::open(&path).expect("打开");
        let mut archive = zip::ZipArchive::new(file).expect("应是合法 zip");
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).expect("条目").name().to_string())
            .collect();
        assert!(names.contains(&"README.txt".to_string()), "实际: {names:?}");
        assert!(names.contains(&"environment.json".to_string()));

        // environment.json 必须是能解析的 JSON(而不是恰好写了些字节)。
        let mut entry = archive.by_name("environment.json").expect("环境快照");
        let mut json = String::new();
        std::io::Read::read_to_string(&mut entry, &mut json).expect("读取");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("应是合法 JSON");
        assert!(parsed.get("version").is_some());
        assert!(parsed.get("fd_limit").is_some());
        assert!(parsed.get("artifact_retention").is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 连续导出使用唯一名称且不残留staging() {
        let dir = test_dir("unique");
        let preserve = std::collections::HashSet::new();
        let first = export(&dir, &preserve).expect("第一次导出");
        let second = export(&dir, &preserve).expect("第二次导出");
        assert_ne!(first, second, "同秒连续导出不得覆盖");
        assert!(first.exists() && second.exists());
        let temp_remains = std::fs::read_dir(&dir)
            .expect("读目录")
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().starts_with(".uvp-"));
        assert!(!temp_remains, "成功发布后不应残留 staging");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 目录不可写时报错而不panic() {
        // 传一个普通文件当输出目录。
        let dir = test_dir("badpath");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建目录");
        let file = dir.join("occupied");
        std::fs::write(&file, b"x").expect("写文件");
        assert!(export(&file, &std::collections::HashSet::new()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
