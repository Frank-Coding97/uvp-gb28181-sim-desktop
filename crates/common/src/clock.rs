//! 时间工具:UNIX 纪元秒 ↔ ISO8601 字符串,以及网络校时偏移。
//!
//! GB28181 网络校时:上级平台在 REGISTER 200 OK 的 `Date` 头携带平台时间,
//! 下级设备据此校准本地时钟。本模块不改动操作系统时钟(模拟器无权/不应改),
//! 而是记录“平台时间 − 本地时间”的偏移,生成 MANSCDP 时间戳时统一叠加,
//! 使设备上报的时间(心跳/报警/位置等)与平台对齐。
//!
//! 校时来源有两条:
//! - `SipDate`:平台在 REGISTER 200 OK 里下发的 `Date` 头(默认来源);
//! - `Ntp`:用户显式配置的 NTP/SNTP 服务器(优先级更高)。
//!
//! 优先级规则与移动端一致:**NTP 生效期间不覆盖偏移**;NTP 查询失败时回退到
//! 最近一次 SIP Date 偏移,而不是把偏移清零(否则设备时间会突然跳回本地时钟)。

use std::sync::{
    atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering},
    Mutex,
};

use serde::Serialize;

/// 校时来源。
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeSyncSource {
    /// 尚未校时。
    None = 0,
    /// 平台 REGISTER 200 OK 的 `Date` 头。
    SipDate = 1,
    /// 用户配置的 NTP/SNTP 服务器。
    Ntp = 2,
}

impl TimeSyncSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SipDate => "sip_date",
            Self::Ntp => "ntp",
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::SipDate,
            2 => Self::Ntp,
            _ => Self::None,
        }
    }
}

/// 对外暴露的校时状态快照(前端「设备校时」页直接消费)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClockSnapshot {
    /// 当前生效偏移(秒):平台时间 − 本地时间。
    pub offset_secs: i64,
    /// 当前生效来源。
    pub source: TimeSyncSource,
    /// 来源凭证明细:原始 `Date` 头,或 `NTP host:port`。
    pub detail: String,
    /// 最近一次成功校时的本地时刻(毫秒;0 表示从未成功)。
    pub synced_at_ms: i64,
    /// 最近一次 SIP Date 偏移(秒),NTP 失败时作为回退值。
    pub sip_offset_secs: i64,
    /// 是否已经收到过可解析的 SIP Date。
    pub sip_synced: bool,
    /// NTP 当前是否处于生效状态。
    pub ntp_active: bool,
    /// 最近一次校时失败原因(NTP 查询失败时写入)。
    pub last_error: Option<String>,
}

/// 全局校时偏移(秒):平台时间 − 本地时间。默认 0(未校时)。
static CLOCK_OFFSET_SECS: AtomicI64 = AtomicI64::new(0);
/// 最近一次 SIP Date 偏移,供 NTP 失败时回退。
static SIP_OFFSET_SECS: AtomicI64 = AtomicI64::new(0);
static SIP_SYNCED: AtomicBool = AtomicBool::new(false);
/// NTP 是否正在生效(生效期间 SIP Date 只记录、不覆盖)。
static NTP_ACTIVE: AtomicBool = AtomicBool::new(false);
static SOURCE: AtomicU8 = AtomicU8::new(0);
static SYNCED_AT_MS: AtomicI64 = AtomicI64::new(0);
static DETAIL: Mutex<String> = Mutex::new(String::new());
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// 设置校时偏移(秒)。由注册流程解析平台 Date 后调用。
///
/// 底层写入,不改变来源标记;需要完整语义请用 [`sync_from_date_header`]。
pub fn set_offset_secs(offset: i64) {
    CLOCK_OFFSET_SECS.store(offset, Ordering::Relaxed);
}

/// 当前校时偏移(秒)。
pub fn offset_secs() -> i64 {
    CLOCK_OFFSET_SECS.load(Ordering::Relaxed)
}

/// 本地当前 UNIX 纪元秒。
fn local_epoch_secs() -> i64 {
    local_epoch_ms() / 1000
}

/// 本地当前 UNIX 纪元毫秒。
pub fn local_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 校时后的当前 UNIX 纪元秒(本地 + 偏移)。
pub fn synced_epoch_secs() -> i64 {
    local_epoch_secs() + offset_secs()
}

/// 校时后的当前时间,格式化为不含时区的 ISO8601(`YYYY-MM-DDThh:mm:ss`)。
/// GB28181 MANSCDP 时间字段用此格式。
pub fn synced_iso8601() -> String {
    epoch_to_iso8601(synced_epoch_secs())
}

/// 把 UNIX 纪元秒转成 `YYYY-MM-DDThh:mm:ss`(UTC 历法计算,不带时区后缀)。
///
/// 注:这里按“纪元秒对应的挂钟时间”直接换算。偏移已在 synced_epoch_secs 叠加,
/// 使数值与平台 Date(平台本地时间)对齐,故此处不再做时区转换。
pub fn epoch_to_iso8601(epoch_secs: i64) -> String {
    let days = epoch_secs.div_euclid(86_400);
    let secs_of_day = epoch_secs.rem_euclid(86_400);
    let (hh, mm, ss) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}")
}

/// 解析平台 `Date` 头为 UNIX 纪元秒。接受 `YYYY-MM-DDThh:mm:ss[.fff]` 形式
/// (GB28181/WVP 常用,无时区)。解析失败返回 None。
pub fn parse_iso8601(s: &str) -> Option<i64> {
    let s = s.trim();
    // 切掉毫秒/时区尾巴,只取到秒。按字符边界安全截取,避免多字节 UTF-8 越界 panic。
    let core = s.get(..19).unwrap_or(s);
    let (date, time) = core.split_once('T')?;
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let mo: i64 = dp.next()?.parse().ok()?;
    let d: i64 = dp.next()?.parse().ok()?;
    let mut tp = time.split(':');
    let hh: i64 = tp.next()?.parse().ok()?;
    let mm: i64 = tp.next()?.parse().ok()?;
    let ss: i64 = tp.next()?.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// 用平台 Date 头校准偏移:offset = 平台纪元秒 − 本地纪元秒。成功返回偏移秒。
///
/// NTP 生效期间只记录 SIP 偏移作为回退值,不覆盖当前生效偏移 —— 否则每条
/// 注册/续约的 200 OK 都会把用户显式选择的 NTP 校时结果冲掉。
pub fn sync_from_date_header(date: &str) -> Option<i64> {
    let platform = parse_iso8601(date)?;
    let offset = platform - local_epoch_secs();
    SIP_OFFSET_SECS.store(offset, Ordering::Relaxed);
    SIP_SYNCED.store(true, Ordering::Release);
    if NTP_ACTIVE.load(Ordering::Acquire) {
        return Some(offset);
    }
    CLOCK_OFFSET_SECS.store(offset, Ordering::Relaxed);
    SOURCE.store(TimeSyncSource::SipDate as u8, Ordering::Release);
    SYNCED_AT_MS.store(local_epoch_ms(), Ordering::Relaxed);
    set_detail(format!("Date: {}", date.trim()));
    clear_error();
    Some(offset)
}

/// 应用一次 NTP 校时结果。`offset_secs` 为“平台时间 − 本地时间”。
pub fn apply_ntp(offset_secs: i64, server: &str, round_trip_ms: i64) {
    NTP_ACTIVE.store(true, Ordering::Release);
    CLOCK_OFFSET_SECS.store(offset_secs, Ordering::Relaxed);
    SOURCE.store(TimeSyncSource::Ntp as u8, Ordering::Release);
    SYNCED_AT_MS.store(local_epoch_ms(), Ordering::Relaxed);
    set_detail(format!("NTP {server} · RTT {round_trip_ms}ms"));
    clear_error();
}

/// NTP 查询失败:回退到最近一次 SIP Date 偏移,并记录失败原因。
///
/// 没有可回退值时保持偏移不变(而不是清零)——清零会让设备时间突然跳回
/// 未校时的本地时钟,平台侧看到的是时间轴断裂。
pub fn ntp_failed(reason: &str) {
    NTP_ACTIVE.store(false, Ordering::Release);
    if SIP_SYNCED.load(Ordering::Acquire) {
        CLOCK_OFFSET_SECS.store(SIP_OFFSET_SECS.load(Ordering::Relaxed), Ordering::Relaxed);
        SOURCE.store(TimeSyncSource::SipDate as u8, Ordering::Release);
        set_detail("NTP 失败，已回退 SIP Date".to_string());
    } else {
        SOURCE.store(TimeSyncSource::None as u8, Ordering::Release);
        set_detail("NTP 失败，暂无 SIP Date 可回退".to_string());
    }
    set_error(reason.to_string());
}

/// 关闭 NTP 生效标记并恢复 SIP Date 偏移(用户在界面上关掉 NTP 时调用)。
pub fn clear_ntp() {
    NTP_ACTIVE.store(false, Ordering::Release);
    if SIP_SYNCED.load(Ordering::Acquire) {
        CLOCK_OFFSET_SECS.store(SIP_OFFSET_SECS.load(Ordering::Relaxed), Ordering::Relaxed);
        SOURCE.store(TimeSyncSource::SipDate as u8, Ordering::Release);
        set_detail("已关闭 NTP，按 SIP Date 校时".to_string());
    } else {
        SOURCE.store(TimeSyncSource::None as u8, Ordering::Release);
        set_detail(String::new());
    }
    clear_error();
}

/// 当前校时状态快照。
pub fn snapshot() -> ClockSnapshot {
    ClockSnapshot {
        offset_secs: offset_secs(),
        source: TimeSyncSource::from_u8(SOURCE.load(Ordering::Acquire)),
        detail: DETAIL.lock().map(|value| value.clone()).unwrap_or_default(),
        synced_at_ms: SYNCED_AT_MS.load(Ordering::Relaxed),
        sip_offset_secs: SIP_OFFSET_SECS.load(Ordering::Relaxed),
        sip_synced: SIP_SYNCED.load(Ordering::Acquire),
        ntp_active: NTP_ACTIVE.load(Ordering::Acquire),
        last_error: LAST_ERROR.lock().ok().and_then(|value| value.clone()),
    }
}

/// 本地与平台当前时间(秒),供前端展示;偏移已叠加到平台时间上。
pub fn now_pair() -> (i64, i64) {
    let local = local_epoch_secs();
    (local, local + offset_secs())
}

fn set_detail(value: String) {
    if let Ok(mut detail) = DETAIL.lock() {
        *detail = value;
    }
}

fn set_error(value: String) {
    if let Ok(mut error) = LAST_ERROR.lock() {
        *error = Some(value);
    }
}

fn clear_error() {
    if let Ok(mut error) = LAST_ERROR.lock() {
        *error = None;
    }
}

/// 测试专用:把所有全局状态复位到“从未校时”。
#[doc(hidden)]
pub fn reset_for_test() {
    CLOCK_OFFSET_SECS.store(0, Ordering::Relaxed);
    SIP_OFFSET_SECS.store(0, Ordering::Relaxed);
    SIP_SYNCED.store(false, Ordering::Release);
    NTP_ACTIVE.store(false, Ordering::Release);
    SOURCE.store(0, Ordering::Release);
    SYNCED_AT_MS.store(0, Ordering::Relaxed);
    set_detail(String::new());
    clear_error();
}

// ── 民用历法 ↔ 纪元天数(Howard Hinnant 算法,公历,支持负年天数)──

/// 公历 (y, m, d) → 距 1970-01-01 的天数。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// 距 1970-01-01 的天数 → 公历 (y, m, d)。
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全局状态是进程级单例,并发测试必须串行,否则互相看到对方的偏移。
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_for_test();
        guard
    }

    #[test]
    fn 纪元与iso往返() {
        let _guard = serial();
        // 2026-07-04T00:23:58 → epoch → 回原串。
        let e = parse_iso8601("2026-07-04T00:23:58").unwrap();
        assert_eq!(epoch_to_iso8601(e), "2026-07-04T00:23:58");
    }

    #[test]
    fn 已知纪元值() {
        let _guard = serial();
        // 1970-01-01T00:00:00 = 0。
        assert_eq!(parse_iso8601("1970-01-01T00:00:00"), Some(0));
        // 2000-01-01T00:00:00 = 946684800。
        assert_eq!(parse_iso8601("2000-01-01T00:00:00"), Some(946_684_800));
        assert_eq!(epoch_to_iso8601(946_684_800), "2000-01-01T00:00:00");
    }

    #[test]
    fn 带毫秒后缀可解析() {
        let _guard = serial();
        assert_eq!(
            parse_iso8601("2026-07-04T00:23:58.186"),
            parse_iso8601("2026-07-04T00:23:58")
        );
    }

    #[test]
    fn 校时偏移生效() {
        let _guard = serial();
        // 造一个“未来 1 小时”的平台时间,偏移应约为 +3600(允许 ±2s 执行抖动)。
        let future = epoch_to_iso8601(local_epoch_secs() + 3600);
        let off = sync_from_date_header(&future).unwrap();
        assert!((3598..=3602).contains(&off), "offset={off}");
        let state = snapshot();
        assert_eq!(state.source, TimeSyncSource::SipDate);
        assert!(state.sip_synced);
        assert!(state.detail.starts_with("Date: "));
        assert!(state.synced_at_ms > 0);
    }

    #[test]
    fn ntp生效期间sip_date不覆盖偏移() {
        let _guard = serial();
        apply_ntp(120, "ntp.example.com:123", 8);
        let future = epoch_to_iso8601(local_epoch_secs() + 3600);
        sync_from_date_header(&future);
        let state = snapshot();
        assert_eq!(state.offset_secs, 120, "NTP 偏移不应被 SIP Date 冲掉");
        assert_eq!(state.source, TimeSyncSource::Ntp);
        assert!(state.ntp_active);
        // SIP 偏移仍被记录,用于 NTP 失败时回退。
        assert!((3598..=3602).contains(&state.sip_offset_secs));
    }

    #[test]
    fn ntp失败回退sip_date() {
        let _guard = serial();
        let future = epoch_to_iso8601(local_epoch_secs() + 600);
        sync_from_date_header(&future);
        apply_ntp(30, "ntp.example.com:123", 12);
        ntp_failed("timeout");
        let state = snapshot();
        assert_eq!(state.source, TimeSyncSource::SipDate);
        assert!(!state.ntp_active);
        assert!((598..=602).contains(&state.offset_secs));
        assert_eq!(state.last_error.as_deref(), Some("timeout"));
    }

    #[test]
    fn 无sip可回退时不清零偏移() {
        let _guard = serial();
        apply_ntp(45, "ntp.example.com:123", 5);
        ntp_failed("dns failure");
        let state = snapshot();
        assert_eq!(state.offset_secs, 45, "没有回退值时保持原偏移");
        assert_eq!(state.source, TimeSyncSource::None);
        assert!(!state.sip_synced);
    }

    #[test]
    fn 关闭ntp恢复sip偏移() {
        let _guard = serial();
        let future = epoch_to_iso8601(local_epoch_secs() + 300);
        sync_from_date_header(&future);
        apply_ntp(10, "ntp.example.com:123", 5);
        clear_ntp();
        let state = snapshot();
        assert!(!state.ntp_active);
        assert!((298..=302).contains(&state.offset_secs));
        assert_eq!(state.last_error, None);
    }

    #[test]
    fn 非法输入返回none() {
        let _guard = serial();
        assert!(parse_iso8601("").is_none());
        assert!(parse_iso8601("2026-13-01T00:00:00").is_none());
        assert!(parse_iso8601("garbage").is_none());
    }

    #[test]
    fn 多字节字符不panic() {
        let _guard = serial();
        // 含多字节 UTF-8(19 字节处可能落在字符中间)不应 panic,返回 None。
        assert!(parse_iso8601("2026-07-04T00:23:5中文乱码尾巴").is_none());
        assert!(parse_iso8601("中文中文中文").is_none());
    }
}
