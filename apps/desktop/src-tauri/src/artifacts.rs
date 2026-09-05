//! 应用托管产物（压测时序、报告和诊断包）的安全创建与保留策略。

use serde::Serialize;
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(0);
static ACTIVE_ARTIFACTS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

fn active_artifacts() -> &'static Mutex<HashSet<PathBuf>> {
    ACTIVE_ARTIFACTS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn lock_active_artifacts() -> std::sync::MutexGuard<'static, HashSet<PathBuf>> {
    active_artifacts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 活跃产物注册令牌。令牌存活期间 cleanup 不得删除其当前路径；Drop 自动解除。
///
/// publish 在 registry 锁内完成 rename 并把保护从 staging 原子转移到 final，
/// 因而调用方不需要、也不允许手工 unregister。
#[derive(Debug)]
pub(crate) struct ActiveArtifactGuard {
    path: Mutex<Option<PathBuf>>,
}

#[cfg(test)]
pub(crate) fn is_registered_active(path: &Path) -> bool {
    lock_active_artifacts().contains(path)
}

impl ActiveArtifactGuard {
    fn new_registered(path: &Path) -> io::Result<(File, Self)> {
        let mut active = lock_active_artifacts();
        let file = create_new_private(path)?;
        active.insert(path.to_path_buf());
        Ok((
            file,
            Self {
                path: Mutex::new(Some(path.to_path_buf())),
            },
        ))
    }

    pub(crate) fn publish_to(&self, final_path: &Path) -> io::Result<PublishHealth> {
        let mut path = self
            .path
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staging = path
            .as_ref()
            .ok_or_else(|| io::Error::other("活跃产物令牌已释放"))?;
        if staging.parent() != final_path.parent() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "staging 与 final 必须位于同一目录",
            ));
        }

        let mut active = lock_active_artifacts();
        let health = publish_new(staging, final_path)?;
        active.remove(staging);
        active.insert(final_path.to_path_buf());
        *path = Some(final_path.to_path_buf());
        Ok(health)
    }
}

impl Drop for ActiveArtifactGuard {
    fn drop(&mut self) {
        let path = self
            .path
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(path) = path {
            lock_active_artifacts().remove(&path);
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PublishHealth {
    pub durability_warning: Option<String>,
}

/// 已发布且仍受活跃注册保护的文件。调用方加入 preserve/状态交接后再 Drop。
#[derive(Debug)]
pub(crate) struct PublishedArtifact {
    path: PathBuf,
    health: PublishHealth,
    _guard: ActiveArtifactGuard,
}

impl PublishedArtifact {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn durability_warning(&self) -> Option<&str> {
        self.health.durability_warning.as_deref()
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct RetentionPolicy {
    pub max_age_secs: u64,
    pub max_files: usize,
    pub max_total_bytes: u64,
    pub stale_temp_secs: u64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            max_age_secs: 30 * 24 * 60 * 60,
            max_files: 100,
            max_total_bytes: 1024 * 1024 * 1024,
            stale_temp_secs: 24 * 60 * 60,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CleanupHealth {
    pub healthy: bool,
    pub scanned: usize,
    pub removed: usize,
    pub failed_removals: usize,
    pub retained_files: usize,
    pub retained_bytes: u64,
    pub retained_temp_files: usize,
    pub retained_temp_bytes: u64,
    pub metadata_failures: usize,
    pub unknown_files: usize,
    pub skipped_symlinks: usize,
    pub preserved_files: usize,
    pub warnings: Vec<String>,
}

impl CleanupHealth {
    fn warn(&mut self, message: String) {
        tracing::warn!(message, "应用托管产物清理遇到可恢复错误");
        self.warnings.push(message);
    }

    fn finish(&mut self, policy: RetentionPolicy) {
        self.healthy = self.warnings.is_empty()
            && self.retained_files <= policy.max_files
            && self.retained_bytes <= policy.max_total_bytes;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArtifactKind {
    Managed,
    Temp,
}

#[derive(Debug)]
struct Candidate {
    path: PathBuf,
    modified: Option<SystemTime>,
    bytes: u64,
    preserved: bool,
}

pub fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let counter = UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{nanos}-{counter}", std::process::id())
}

pub fn unique_timestamp_counter() -> (u128, u64) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let counter = UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed);
    (nanos, counter)
}

pub fn create_secure_dir(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} 不是安全的普通目录", path.display()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        let mode = std::fs::symlink_metadata(path)?.permissions().mode() & 0o777;
        if mode != 0o700 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} 权限不是 0700（实际 {mode:o}）", path.display()),
            ));
        }
    }
    Ok(())
}

pub(crate) fn create_new_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let mode = file.metadata()?.permissions().mode() & 0o777;
        if mode != 0o600 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} 权限不是 0600（实际 {mode:o}）", path.display()),
            ));
        }
    }
    Ok(file)
}

/// 在与 cleanup 共用的注册表锁内创建并登记活跃时序，避免扫描竞态。
pub(crate) fn create_active_series(path: &Path) -> io::Result<(File, ActiveArtifactGuard)> {
    ActiveArtifactGuard::new_registered(path)
}

fn sync_parent(path: &Path) -> io::Result<()> {
    #[cfg(test)]
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("report-fsync-fail-"))
    {
        return Err(io::Error::other("测试注入父目录同步失败"));
    }
    #[cfg(unix)]
    {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "产物路径缺少父目录"))?;
        File::open(parent)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(target_os = "macos")]
fn rename_new(staging: &Path, final_path: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let source = std::ffi::CString::new(staging.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "staging 路径含 NUL"))?;
    let target = std::ffi::CString::new(final_path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "final 路径含 NUL"))?;
    // SAFETY: 两个 CString 在调用期间有效，RENAME_EXCL 保证目标存在时不覆盖。
    let result = unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn rename_new(staging: &Path, final_path: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let source = std::ffi::CString::new(staging.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "staging 路径含 NUL"))?;
    let target = std::ffi::CString::new(final_path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "final 路径含 NUL"))?;
    // SAFETY: 指针在 syscall 期间有效；flag=1 是 renameat2 的 RENAME_NOREPLACE。
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            1u32,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn rename_new(staging: &Path, final_path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileW(existing: *const u16, new: *const u16) -> i32;
    }
    let source = staging
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target = final_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: 两个 NUL 结尾缓冲区在调用期间有效；MoveFileW 不替换已有目标。
    let result = unsafe { MoveFileW(source.as_ptr(), target.as_ptr()) };
    if result != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn rename_new(staging: &Path, final_path: &Path) -> io::Result<()> {
    // 其它 Unix 缺少统一 no-replace rename API；link+unlink 仍保持原子可见且不覆盖。
    std::fs::hard_link(staging, final_path)?;
    std::fs::remove_file(staging)
}

/// 以“不覆盖”的方式原子 rename 同目录 staging 文件。
///
/// rename 成功即表示 final 已完整可见。后续父目录 fsync 失败只降低掉电持久性，
/// 不能再返回“未发布”，否则调用方会把真实存在的 final 当成 staging 失败处理。
fn publish_new(staging: &Path, final_path: &Path) -> io::Result<PublishHealth> {
    if final_path.try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("目标已存在: {}", final_path.display()),
        ));
    }
    rename_new(staging, final_path)?;
    let durability_warning = sync_parent(final_path).err().map(|error| {
        let warning = format!(
            "{} 已完整发布，但父目录同步失败，掉电后目录项持久性无法保证: {error}",
            final_path.display()
        );
        tracing::warn!(
            %error,
            final_path = %final_path.display(),
            "产物已发布，但父目录同步失败"
        );
        warning
    });
    Ok(PublishHealth { durability_warning })
}

/// 同目录 staging 文件。Drop 会尽力清理尚未发布的 `.uvp-*.tmp`。
pub struct AtomicFile {
    final_path: PathBuf,
    temp_path: PathBuf,
    file: Option<File>,
    active: Option<ActiveArtifactGuard>,
}

impl AtomicFile {
    pub fn new(final_path: &Path) -> io::Result<Self> {
        let parent = final_path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "产物路径缺少父目录"))?;
        create_secure_dir(parent)?;
        if final_path.try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("目标已存在: {}", final_path.display()),
            ));
        }
        let temp_path = parent.join(format!(".uvp-{}.tmp", unique_suffix()));
        let (file, active) = ActiveArtifactGuard::new_registered(&temp_path)?;
        Ok(Self {
            final_path: final_path.to_path_buf(),
            temp_path,
            file: Some(file),
            active: Some(active),
        })
    }

    pub fn file_mut(&mut self) -> &mut File {
        self.file.as_mut().expect("AtomicFile 发布后不可再写")
    }

    pub(crate) fn publish(mut self) -> io::Result<PublishedArtifact> {
        let mut file = self.file.take().expect("AtomicFile 只能发布一次");
        file.flush()?;
        file.sync_all()?;
        drop(file);
        let active = self.active.take().expect("AtomicFile 活跃令牌缺失");
        let health = active.publish_to(&self.final_path)?;
        Ok(PublishedArtifact {
            path: self.final_path.clone(),
            health,
            _guard: active,
        })
    }
}

impl Drop for AtomicFile {
    fn drop(&mut self) {
        if self.temp_path.exists() {
            if let Err(error) = std::fs::remove_file(&self.temp_path) {
                tracing::warn!(
                    %error,
                    path = %self.temp_path.display(),
                    "清理未发布 staging 失败，将由 stale-temp 策略重试"
                );
            }
        }
    }
}

pub(crate) fn atomic_write_new(final_path: &Path, bytes: &[u8]) -> io::Result<PublishedArtifact> {
    let mut staged = AtomicFile::new(final_path)?;
    staged.file_mut().write_all(bytes)?;
    staged.publish()
}

fn all_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn numbered_stem(value: &str, prefix: &str, parts: usize) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.split('-').count() == parts && rest.split('-').all(all_digits))
}

fn classify_name(name: &str) -> Option<ArtifactKind> {
    if let Some(stem) = name
        .strip_prefix(".uvp-")
        .and_then(|v| v.strip_suffix(".tmp"))
    {
        return (stem.split('-').count() == 3 && stem.split('-').all(all_digits))
            .then_some(ArtifactKind::Temp);
    }

    if let Some(stem) = name.strip_suffix(".jsonl.inprogress") {
        return (numbered_stem(stem, "series-run", 3) || numbered_stem(stem, "series-run", 2))
            .then_some(ArtifactKind::Managed);
    }
    if let Some(stem) = name.strip_suffix(".jsonl") {
        return (numbered_stem(stem, "series-run", 3) || numbered_stem(stem, "series-run", 2))
            .then_some(ArtifactKind::Managed);
    }
    if let Some(stem) = name.strip_suffix(".json") {
        return (numbered_stem(stem, "report-", 2) || numbered_stem(stem, "report-", 1))
            .then_some(ArtifactKind::Managed);
    }
    if let Some(stem) = name
        .strip_prefix("uvp-diagnostics-")
        .and_then(|v| v.strip_suffix(".zip"))
    {
        return (stem.split('-').count() <= 2 && stem.split('-').all(all_digits))
            .then_some(ArtifactKind::Managed);
    }
    None
}

fn old_enough(modified: SystemTime, now: SystemTime, age: Duration) -> bool {
    now.duration_since(modified)
        .is_ok_and(|elapsed| elapsed >= age)
}

fn remove_candidate(candidate: &Candidate, health: &mut CleanupHealth) -> bool {
    match std::fs::remove_file(&candidate.path) {
        Ok(()) => {
            health.removed += 1;
            true
        }
        Err(error) => {
            health.failed_removals += 1;
            health.warn(format!("删除 {} 失败: {error}", candidate.path.display()));
            false
        }
    }
}

/// 清理应用严格命名的托管产物。任何单项错误都只进入 health/warn。
pub fn cleanup(dir: &Path, policy: RetentionPolicy, preserve: &HashSet<PathBuf>) -> CleanupHealth {
    let mut health = CleanupHealth::default();
    if let Err(error) = create_secure_dir(dir) {
        health.warn(format!("准备产物目录 {} 失败: {error}", dir.display()));
        health.finish(policy);
        return health;
    }

    // 持锁覆盖整个扫描与删除阶段；所有 staging 创建和 final 发布也用同一锁。
    let active = lock_active_artifacts();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            health.warn(format!("读取产物目录 {} 失败: {error}", dir.display()));
            health.finish(policy);
            return health;
        }
    };
    let now = SystemTime::now();
    let mut managed = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                health.warn(format!("读取产物目录项失败: {error}"));
                continue;
            }
        };
        health.scanned += 1;
        let path = entry.path();
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                health.metadata_failures += 1;
                health.warn(format!("读取 {} 元数据失败: {error}", path.display()));
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            health.skipped_symlinks += 1;
            continue;
        }
        if !metadata.is_file() {
            health.unknown_files += 1;
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            health.unknown_files += 1;
            continue;
        };
        let Some(kind) = classify_name(name) else {
            health.unknown_files += 1;
            continue;
        };
        let modified = match metadata.modified() {
            Ok(modified) => Some(modified),
            Err(error) => {
                health.metadata_failures += 1;
                health.warn(format!("读取 {} 修改时间失败: {error}", path.display()));
                None
            }
        };
        let bytes = metadata.len();
        let protected = preserve.contains(&path) || active.contains(&path);
        if protected {
            health.preserved_files += 1;
        }
        // 无修改时间时无法安全排序/判龄；保留它，但仍计入 managed 数量和字节。
        let preserved = protected || modified.is_none();
        if kind == ArtifactKind::Temp {
            let stale = modified.is_some_and(|modified| {
                old_enough(modified, now, Duration::from_secs(policy.stale_temp_secs))
            });
            let candidate = Candidate {
                path,
                modified,
                bytes,
                preserved,
            };
            if !preserved && stale && remove_candidate(&candidate, &mut health) {
                continue;
            }
            health.retained_temp_files += 1;
            health.retained_temp_bytes = health.retained_temp_bytes.saturating_add(bytes);
            continue;
        }
        managed.push(Candidate {
            path,
            modified,
            bytes,
            preserved,
        });
    }

    // 修改时间不可读的 managed 文件仍进入 retained 统计，且不会参与年龄删除；
    // 这样 health 不会静默低报实际可测的文件数和字节数。
    managed.sort_by_key(|candidate| candidate.modified.unwrap_or(UNIX_EPOCH));
    let max_age = Duration::from_secs(policy.max_age_secs);
    let mut retained = Vec::with_capacity(managed.len());
    for candidate in managed {
        let expired = candidate
            .modified
            .is_some_and(|modified| old_enough(modified, now, max_age));
        if !candidate.preserved && expired {
            if !remove_candidate(&candidate, &mut health) {
                retained.push(candidate);
            }
        } else {
            retained.push(candidate);
        }
    }

    let mut retained_files = retained.len();
    let mut retained_bytes = retained
        .iter()
        .map(|candidate| candidate.bytes)
        .sum::<u64>();
    for candidate in &retained {
        if retained_files <= policy.max_files && retained_bytes <= policy.max_total_bytes {
            break;
        }
        if candidate.preserved {
            continue;
        }
        if remove_candidate(candidate, &mut health) {
            retained_files = retained_files.saturating_sub(1);
            retained_bytes = retained_bytes.saturating_sub(candidate.bytes);
        }
    }
    health.retained_files = retained_files;
    health.retained_bytes = retained_bytes;
    if retained_files > policy.max_files || retained_bytes > policy.max_total_bytes {
        health.warn("保留文件使产物目录仍超过策略上限".to_string());
    }
    drop(active);
    health.finish(policy);
    health
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("uvp-artifacts-{label}-{}", unique_suffix()))
    }

    fn write_managed(dir: &Path, name: &str, bytes: usize) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, vec![b'x'; bytes]).expect("写测试产物");
        path
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut result = std::fs::read_dir(dir)
            .expect("读目录")
            .map(|entry| {
                entry
                    .expect("目录项")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        result.sort();
        result
    }

    #[test]
    fn atomic_write_success_and_conflict_does_not_overwrite() {
        let dir = test_dir("atomic");
        create_secure_dir(&dir).expect("建目录");
        let final_path = dir.join("report-1-1.json");
        atomic_write_new(&final_path, b"first").expect("首次写入");
        assert_eq!(std::fs::read(&final_path).expect("读取"), b"first");
        let error = atomic_write_new(&final_path, b"second").expect_err("冲突应失败");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&final_path).expect("读取"), b"first");
        assert!(!names(&dir).iter().any(|name| name.starts_with(".uvp-")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn atomic_write_failure_leaves_no_final_or_temp() {
        let dir = test_dir("atomic-fail");
        create_secure_dir(&dir).expect("建目录");
        let final_path = dir.join("report-1-1.json");
        {
            let mut staged = AtomicFile::new(&final_path).expect("建 staging");
            staged.file_mut().write_all(b"partial").expect("写部分");
            // 模拟序列化/生成过程失败：未调用 publish，Drop 必须清理 staging。
        }
        assert!(!final_path.exists());
        assert!(names(&dir).is_empty(), "不应残留 temp");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn atomic_staging和final在preserve交接前都受guard保护() {
        let dir = test_dir("atomic-active");
        create_secure_dir(&dir).expect("建目录");
        let final_path = dir.join("report-3-3.json");
        let mut staged = AtomicFile::new(&final_path).expect("建 staging");
        staged.file_mut().write_all(b"complete").expect("写内容");
        let policy = RetentionPolicy {
            max_age_secs: 0,
            max_files: 0,
            max_total_bytes: 0,
            stale_temp_secs: 0,
        };
        let health = cleanup(&dir, policy, &HashSet::new());
        assert_eq!(health.preserved_files, 1);
        assert_eq!(health.retained_temp_files, 1);

        let published = staged.publish().expect("发布");
        cleanup(&dir, policy, &HashSet::new());
        assert!(final_path.exists(), "published guard 存活时 final 不得被删");
        drop(published);
        cleanup(&dir, policy, &HashSet::new());
        assert!(!final_path.exists(), "交接完成释放 guard 后恢复 retention");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rename成功后目录同步失败仍明确返回已发布() {
        let dir = test_dir("fsync-warning");
        create_secure_dir(&dir).expect("建目录");
        let final_path = dir.join("report-fsync-fail-1.json");
        let published = atomic_write_new(&final_path, b"complete").expect("rename 后应视为已发布");
        assert!(final_path.exists());
        assert_eq!(std::fs::read(&final_path).expect("读 final"), b"complete");
        assert!(published.durability_warning().is_some());
        drop(published);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cleanup_health准确记录删除失败和保留temp() {
        let dir = test_dir("cleanup-health");
        create_secure_dir(&dir).expect("建目录");
        write_managed(&dir, ".uvp-1-2-3.tmp", 7);
        let policy = RetentionPolicy {
            max_age_secs: u64::MAX,
            max_files: 100,
            max_total_bytes: 1024,
            stale_temp_secs: u64::MAX,
        };
        let health = cleanup(&dir, policy, &HashSet::new());
        assert_eq!(health.retained_temp_files, 1);
        assert_eq!(health.retained_temp_bytes, 7);

        let missing = Candidate {
            path: dir.join("report-404-404.json"),
            modified: Some(SystemTime::now()),
            bytes: 11,
            preserved: false,
        };
        let mut failed = CleanupHealth::default();
        assert!(!remove_candidate(&missing, &mut failed));
        assert_eq!(failed.failed_removals, 1);
        assert_eq!(failed.removed, 0);
        assert!(!failed.warnings.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cleanup_never_deletes_registered_active_series() {
        let dir = test_dir("active");
        create_secure_dir(&dir).expect("建目录");
        let path = dir.join("series-run9-1-1.jsonl.inprogress");
        let (file, active) = create_active_series(&path).expect("创建并登记活跃时序");
        drop(file);
        let policy = RetentionPolicy {
            max_age_secs: 0,
            max_files: 0,
            max_total_bytes: 0,
            stale_temp_secs: 0,
        };
        let health = cleanup(&dir, policy, &HashSet::new());
        assert!(path.exists(), "活跃 series 即使超过全部上限也不得删除");
        assert_eq!(health.preserved_files, 1);
        drop(active);
        cleanup(&dir, policy, &HashSet::new());
        assert!(!path.exists(), "退出活跃注册表后应恢复普通 retention");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cleanup_enforces_count_bytes_and_preserve() {
        let dir = test_dir("limits");
        create_secure_dir(&dir).expect("建目录");
        let preserved = write_managed(&dir, "series-run1-1-1.jsonl", 8);
        write_managed(&dir, "report-1-1.json", 8);
        write_managed(&dir, "report-2-2.json", 8);
        let policy = RetentionPolicy {
            max_age_secs: u64::MAX,
            max_files: 2,
            max_total_bytes: 16,
            stale_temp_secs: u64::MAX,
        };
        let health = cleanup(&dir, policy, &HashSet::from([preserved.clone()]));
        assert!(preserved.exists(), "preserve 文件不得删除");
        assert_eq!(health.retained_files, 2);
        assert_eq!(health.retained_bytes, 16);
        assert_eq!(health.removed, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cleanup_deletes_old_files_but_not_unknown_or_symlink() {
        let dir = test_dir("age-unknown");
        create_secure_dir(&dir).expect("建目录");
        let managed = write_managed(&dir, "report-1.json", 1);
        let unknown = write_managed(&dir, "notes.txt", 1);
        let policy = RetentionPolicy {
            max_age_secs: 0,
            max_files: 100,
            max_total_bytes: 1024,
            stale_temp_secs: u64::MAX,
        };
        #[cfg(unix)]
        let link = {
            use std::os::unix::fs::symlink;
            let link = dir.join("report-9.json");
            symlink(&unknown, &link).expect("建 symlink");
            Some(link)
        };
        #[cfg(not(unix))]
        let link: Option<PathBuf> = None;
        let health = cleanup(&dir, policy, &HashSet::new());
        assert!(!managed.exists());
        assert!(unknown.exists());
        if let Some(link) = link {
            assert!(link.symlink_metadata().is_ok(), "symlink 不得跟随或删除");
            assert_eq!(health.skipped_symlinks, 1);
        }
        assert!(health.unknown_files >= 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cleanup_removes_stale_temp_before_limits() {
        let dir = test_dir("stale");
        create_secure_dir(&dir).expect("建目录");
        let stale = write_managed(&dir, ".uvp-1-2-3.tmp", 4);
        let recent = write_managed(&dir, ".uvp-1-2-4.tmp", 4);
        std::thread::sleep(Duration::from_millis(2));
        let policy = RetentionPolicy {
            max_age_secs: u64::MAX,
            max_files: 100,
            max_total_bytes: 1024,
            stale_temp_secs: 0,
        };
        let health = cleanup(&dir, policy, &HashSet::new());
        assert!(!stale.exists());
        assert!(!recent.exists());
        assert_eq!(health.removed, 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn secure_dir_and_file_permissions_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = test_dir("permissions");
        std::fs::create_dir_all(&dir).expect("建宽松目录");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("设权限");
        create_secure_dir(&dir).expect("收紧目录");
        assert_eq!(
            std::fs::metadata(&dir)
                .expect("元数据")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let final_path = dir.join("report-1-1.json");
        atomic_write_new(&final_path, b"private").expect("写文件");
        assert_eq!(
            std::fs::metadata(&final_path)
                .expect("元数据")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
