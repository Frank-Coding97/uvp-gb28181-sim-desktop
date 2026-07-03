//! 时间工具:UNIX 纪元秒 ↔ ISO8601 字符串,以及网络校时偏移。
//!
//! GB28181 网络校时:上级平台在 REGISTER 200 OK 的 `Date` 头携带平台时间,
//! 下级设备据此校准本地时钟。本模块不改动操作系统时钟(模拟器无权/不应改),
//! 而是记录“平台时间 − 本地时间”的偏移,生成 MANSCDP 时间戳时统一叠加,
//! 使设备上报的时间(心跳/报警/位置等)与平台对齐。

use std::sync::atomic::{AtomicI64, Ordering};

/// 全局校时偏移(秒):平台时间 − 本地时间。默认 0(未校时)。
static CLOCK_OFFSET_SECS: AtomicI64 = AtomicI64::new(0);

/// 设置校时偏移(秒)。由注册流程解析平台 Date 后调用。
pub fn set_offset_secs(offset: i64) {
    CLOCK_OFFSET_SECS.store(offset, Ordering::Relaxed);
}

/// 当前校时偏移(秒)。
pub fn offset_secs() -> i64 {
    CLOCK_OFFSET_SECS.load(Ordering::Relaxed)
}

/// 本地当前 UNIX 纪元秒。
fn local_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
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
pub fn sync_from_date_header(date: &str) -> Option<i64> {
    let platform = parse_iso8601(date)?;
    let offset = platform - local_epoch_secs();
    set_offset_secs(offset);
    Some(offset)
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

    #[test]
    fn 纪元与iso往返() {
        // 2026-07-04T00:23:58 → epoch → 回原串。
        let e = parse_iso8601("2026-07-04T00:23:58").unwrap();
        assert_eq!(epoch_to_iso8601(e), "2026-07-04T00:23:58");
    }

    #[test]
    fn 已知纪元值() {
        // 1970-01-01T00:00:00 = 0。
        assert_eq!(parse_iso8601("1970-01-01T00:00:00"), Some(0));
        // 2000-01-01T00:00:00 = 946684800。
        assert_eq!(parse_iso8601("2000-01-01T00:00:00"), Some(946_684_800));
        assert_eq!(epoch_to_iso8601(946_684_800), "2000-01-01T00:00:00");
    }

    #[test]
    fn 带毫秒后缀可解析() {
        assert_eq!(
            parse_iso8601("2026-07-04T00:23:58.186"),
            parse_iso8601("2026-07-04T00:23:58")
        );
    }

    #[test]
    fn 校时偏移生效() {
        // 造一个“未来 1 小时”的平台时间,偏移应约为 +3600(允许 ±2s 执行抖动)。
        let future = epoch_to_iso8601(local_epoch_secs() + 3600);
        let off = sync_from_date_header(&future).unwrap();
        assert!((3598..=3602).contains(&off), "offset={off}");
        set_offset_secs(0); // 复位,避免污染其它测试。
    }

    #[test]
    fn 非法输入返回none() {
        assert!(parse_iso8601("").is_none());
        assert!(parse_iso8601("2026-13-01T00:00:00").is_none());
        assert!(parse_iso8601("garbage").is_none());
    }

    #[test]
    fn 多字节字符不panic() {
        // 含多字节 UTF-8(19 字节处可能落在字符中间)不应 panic,返回 None。
        assert!(parse_iso8601("2026-07-04T00:23:5中文乱码尾巴").is_none());
        assert!(parse_iso8601("中文中文中文").is_none());
    }
}
