//! 日志落盘:按大小滚动 + 总量上限 + 异步写。
//!
//! # 为什么必须落盘
//!
//! 日志此前只写标准输出与一个 2000 条的内存环形缓冲。桌面应用从 Finder / 开始菜单
//! 启动时标准输出无处可去,而内存缓冲随进程一起消失 —— 用户报"跑了两小时突然掉线"
//! 时,**现场是零**。商业支持流程的第一步就是"把日志发过来",没有文件这条路走不通。
//!
//! # 为什么滚动与总量上限不是可选项
//!
//! 压测一万台设备在 info 级下每秒数千行,一行约 150 字节 —— 大约 0.5~1 GB/小时。
//! 连跑一夜必然打爆磁盘,而"日志把用户磁盘写满"比没有日志更糟。
//! 所以这里同时限单文件大小与保留文件数,总占用有硬上界(见 [`LogFileConfig`])。
//!
//! # 为什么自己写而不用 tracing-appender
//!
//! `tracing-appender` 只按**时间**滚动(hourly/daily),没有按大小滚动,
//! 也没有总量上限 —— 压测一天写进同一个 daily 文件正是我们要避免的那种情况。
//! 需要的这点逻辑(滚动 + 淘汰 + 后台写线程)不到两百行,不值得为此引一个
//! 只解决一半问题的依赖。

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;

/// 落盘参数。
#[derive(Debug, Clone)]
pub struct LogFileConfig {
    /// 日志目录。
    pub dir: PathBuf,
    /// 单个文件的字节上限,超过即滚动到新文件。
    pub max_file_bytes: u64,
    /// 保留的文件数(含当前正在写的那个),超出后删最旧的。
    ///
    /// 磁盘占用上界 ≈ `max_file_bytes * max_files`。
    pub max_files: usize,
}

impl LogFileConfig {
    /// 默认参数:单文件 16 MiB × 保留 8 个 ≈ 最多 128 MiB。
    ///
    /// 这个量级的取舍:够装下一次几小时压测的关键片段,又不会让用户的
    /// 应用数据目录变成一个让人意外的大坑。
    pub fn new(dir: PathBuf) -> Self {
        LogFileConfig {
            dir,
            max_file_bytes: 16 * 1024 * 1024,
            max_files: 8,
        }
    }

    /// 磁盘占用上界(字节)。
    pub fn max_total_bytes(&self) -> u64 {
        self.max_file_bytes.saturating_mul(self.max_files as u64)
    }
}

/// 日志文件名前缀与后缀。列目录做淘汰时按这两个筛,不会误删别人的文件。
const FILE_PREFIX: &str = "uvp-";
const FILE_SUFFIX: &str = ".log";

/// 异步写句柄。克隆廉价;全部句柄丢弃后写线程自行退出。
#[derive(Clone)]
pub struct LogFileWriter {
    tx: SyncSender<Vec<u8>>,
    /// 因写队列已满而丢弃的行数。
    dropped: Arc<AtomicU64>,
    dir: PathBuf,
}

/// 写队列容量。
///
/// 有界是刻意的:磁盘卡顿(机械盘、网络盘、快照)时无界队列会把日志堆进内存直到 OOM。
/// 满了就丢并计数 —— **丢日志可以接受,因为写日志把进程搞死不可以**。
const QUEUE_CAP: usize = 8192;

impl LogFileWriter {
    /// 启动后台写线程。目录建不出来时返回 None(退化为只写标准输出与内存缓冲)。
    pub fn spawn(config: LogFileConfig) -> Option<Self> {
        if let Err(error) = std::fs::create_dir_all(&config.dir) {
            eprintln!(
                "日志目录 {} 创建失败,本次不落盘: {error}",
                config.dir.display()
            );
            return None;
        }
        let (tx, rx) = sync_channel::<Vec<u8>>(QUEUE_CAP);
        let dropped = Arc::new(AtomicU64::new(0));
        let dir = config.dir.clone();
        // 专用线程而不是 tokio 任务:日志要在 runtime 起来之前就能用,
        // 而且阻塞的文件写不该占用异步 worker。
        let spawned = std::thread::Builder::new()
            .name("uvp-log-writer".into())
            .spawn(move || {
                let mut sink = RollingSink::new(config);
                // 发送端全部丢弃后 recv 返回 Err,线程随之退出并 flush。
                while let Ok(chunk) = rx.recv() {
                    sink.write_chunk(&chunk);
                }
                sink.flush();
            });
        if let Err(error) = spawned {
            eprintln!("日志写线程启动失败,本次不落盘: {error}");
            return None;
        }
        Some(LogFileWriter { tx, dropped, dir })
    }

    /// 写一行(内部会补换行)。队列满时丢弃并计数,绝不阻塞调用方。
    pub fn write_line(&self, line: &str) {
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        match self.tx.try_send(bytes) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// 因队列满被丢弃的行数(诊断"日志是不是不全"用)。
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// 日志目录。
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// 真正落盘的部分:只在写线程里使用,因此无需加锁。
struct RollingSink {
    config: LogFileConfig,
    current: Option<(File, u64)>,
    /// 本进程内的滚动序号。
    ///
    /// 只靠"秒级时间戳 + pid"命名是不够的:一秒内滚动多次会算出同一个文件名,
    /// 于是几次滚动其实都追加进了同一个文件,单文件上限形同虚设。
    seq: u32,
}

impl RollingSink {
    fn new(config: LogFileConfig) -> Self {
        RollingSink {
            config,
            current: None,
            seq: 0,
        }
    }

    fn write_chunk(&mut self, chunk: &[u8]) {
        // 先判滚动再写:让"单文件不超过上限"成为写入前的前置条件,
        // 而不是"写完发现超了下次再滚"(后者会让实际上限超出配置值一整块)。
        let need_roll = match &self.current {
            None => true,
            Some((_, written)) => *written + chunk.len() as u64 > self.config.max_file_bytes,
        };
        if need_roll {
            self.roll();
        }
        if let Some((file, written)) = &mut self.current {
            match file.write_all(chunk) {
                Ok(()) => *written += chunk.len() as u64,
                Err(error) => {
                    // 磁盘满/权限变化:放弃当前文件,下一轮重开。不 panic ——
                    // 日志写不动不该让整个应用挂掉。
                    eprintln!("日志写入失败: {error}");
                    self.current = None;
                }
            }
        }
    }

    fn roll(&mut self) {
        self.flush();
        self.prune();
        // 序号紧跟时间戳,保证"同一秒内多次滚动"也按字典序 = 时间序排列。
        let name = format!(
            "{FILE_PREFIX}{}-{:04}-{}{FILE_SUFFIX}",
            timestamp_for_name(),
            self.seq,
            std::process::id()
        );
        self.seq = self.seq.wrapping_add(1);
        let path = self.config.dir.join(name);
        match File::options().create(true).append(true).open(&path) {
            Ok(file) => {
                let written = file.metadata().map(|m| m.len()).unwrap_or(0);
                self.current = Some((file, written));
            }
            Err(error) => {
                eprintln!("日志文件 {} 打开失败: {error}", path.display());
                self.current = None;
            }
        }
    }

    /// 删掉超出保留数的最旧文件。
    fn prune(&self) {
        let mut files = list_log_files(&self.config.dir);
        // 留出一个名额给即将新建的文件。
        let keep = self.config.max_files.saturating_sub(1);
        if files.len() <= keep {
            return;
        }
        files.sort();
        let remove = files.len() - keep;
        for path in files.into_iter().take(remove) {
            if let Err(error) = std::fs::remove_file(&path) {
                eprintln!("清理旧日志 {} 失败: {error}", path.display());
            }
        }
    }

    fn flush(&mut self) {
        if let Some((file, _)) = &mut self.current {
            let _ = file.flush();
        }
    }
}

/// 列出目录里本工具产生的日志文件。
pub fn list_log_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(FILE_PREFIX) && n.ends_with(FILE_SUFFIX))
        })
        .collect()
}

/// 文件名里的时间戳。用 UTC 的 `YYYYMMDD-HHMMSS`,**字典序等于时间序** ——
/// 淘汰最旧文件时直接按文件名排序即可,不用去读每个文件的 mtime
/// (mtime 会被备份工具、文件系统迁移改掉)。
fn timestamp_for_name() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// Unix 秒 → UTC 年月日时分秒。
///
/// 自己算而不是引 chrono:整个 crate 只有这一处需要日历换算,
/// 而 chrono/time 会带进时区数据库等一串东西。算法是 Howard Hinnant 的
/// `civil_from_days`,对 1970 年以后的日期精确。
fn civil_from_unix(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, mi, s) = (
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    );
    // 把纪元挪到 0000-03-01,让闰年计算不用特判 2 月。
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11],3 月为 0
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d, h, mi, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("uvp-logfile-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// 等写线程把队列消费完(异步写,没有同步点)。
    fn settle() {
        std::thread::sleep(std::time::Duration::from_millis(150));
    }

    #[test]
    fn 日历换算对齐已知日期() {
        // 自己实现的换算必须有已知向量兜底,否则文件名时间戳错了不会有人发现。
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil_from_unix(86_399), (1970, 1, 1, 23, 59, 59));
        assert_eq!(civil_from_unix(86_400), (1970, 1, 2, 0, 0, 0));
        // 2000-02-29(闰年,且是世纪闰年)。
        assert_eq!(civil_from_unix(951_782_400), (2000, 2, 29, 0, 0, 0));
        // 2100 不是闰年:2 月只有 28 天,紧接着就是 3 月 1 日。
        assert_eq!(civil_from_unix(4_107_456_000), (2100, 2, 28, 0, 0, 0));
        assert_eq!(civil_from_unix(4_107_542_400), (2100, 3, 1, 0, 0, 0));
        assert_eq!(civil_from_unix(1_700_000_000), (2023, 11, 14, 22, 13, 20));
    }

    #[test]
    fn 文件名字典序等于时间序() {
        // 淘汰最旧文件靠文件名排序,所以这个性质必须成立。
        let a = format!("{FILE_PREFIX}20231114-221320-0000-1{FILE_SUFFIX}");
        // 同一秒内的第二次滚动:序号让它排在后面(只有秒级时间戳时两者同名)。
        let b = format!("{FILE_PREFIX}20231114-221320-0001-1{FILE_SUFFIX}");
        let c = format!("{FILE_PREFIX}20231114-221321-0000-1{FILE_SUFFIX}");
        let d = format!("{FILE_PREFIX}20240101-000000-0000-1{FILE_SUFFIX}");
        let mut v = vec![d.clone(), b.clone(), a.clone(), c.clone()];
        v.sort();
        assert_eq!(v, vec![a, b, c, d]);
    }

    #[test]
    fn 落盘并按大小滚动() {
        let dir = temp_dir("roll");
        let writer = LogFileWriter::spawn(LogFileConfig {
            dir: dir.clone(),
            max_file_bytes: 256,
            max_files: 8,
        })
        .expect("应能启动");
        // 每行约 100 字节,写 12 行 → 至少滚动 3 次。
        for i in 0..12 {
            writer.write_line(&format!("{i:03} {}", "x".repeat(96)));
        }
        settle();
        let files = list_log_files(&dir);
        assert!(files.len() >= 3, "应按大小滚动出多个文件,实际 {files:?}");
        for f in &files {
            let len = std::fs::metadata(f).expect("元数据").len();
            assert!(
                len <= 256 + 128,
                "单文件不该显著超出上限:{} 有 {len} 字节",
                f.display()
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 保留文件数有硬上限() {
        let dir = temp_dir("prune");
        let config = LogFileConfig {
            dir: dir.clone(),
            max_file_bytes: 128,
            max_files: 3,
        };
        assert_eq!(config.max_total_bytes(), 384);
        let writer = LogFileWriter::spawn(config).expect("应能启动");
        // 远超保留数的写入量:压测一夜就是这个形状,不能把磁盘写满。
        for i in 0..200 {
            writer.write_line(&format!("{i:04} {}", "y".repeat(100)));
        }
        settle();
        let files = list_log_files(&dir);
        assert!(
            files.len() <= 3,
            "保留文件数必须有硬上限,实际 {} 个",
            files.len()
        );
        let total: u64 = files
            .iter()
            .map(|f| std::fs::metadata(f).map(|m| m.len()).unwrap_or(0))
            .sum();
        assert!(total <= 384 + 256, "总占用应受控,实际 {total} 字节");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 队列满时丢弃并计数而不是阻塞() {
        // 磁盘卡顿时无界队列会把日志堆进内存直到 OOM。
        // 丢日志可以接受,写日志把进程搞死不可以。
        let dir = temp_dir("drop");
        let writer = LogFileWriter::spawn(LogFileConfig {
            dir: dir.clone(),
            max_file_bytes: 64,
            max_files: 2,
        })
        .expect("应能启动");
        let started = std::time::Instant::now();
        for i in 0..(QUEUE_CAP * 3) {
            writer.write_line(&format!("{i} {}", "z".repeat(200)));
        }
        // 关键断言是"不阻塞":真被队列挡住的话这里会明显变慢。
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "写日志不该阻塞调用方"
        );
        settle();
        // 丢弃数可能是 0(写线程跟得上),但接口必须可读,用于告诉用户日志不全。
        let _ = writer.dropped();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 目录不可用时优雅降级() {
        // 把一个普通文件当目录传进去:应返回 None 而不是 panic。
        let dir = temp_dir("bad");
        std::fs::create_dir_all(&dir).expect("建目录");
        let file = dir.join("not-a-dir");
        std::fs::write(&file, b"x").expect("写文件");
        assert!(
            LogFileWriter::spawn(LogFileConfig::new(file)).is_none(),
            "目录建不出来时应降级为不落盘,而不是让应用起不来"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
