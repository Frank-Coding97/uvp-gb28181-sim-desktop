//! 报警单：字段模型、取值预检与 Alarm Notify 组装。
//!
//! 放在协议 crate 而不是模拟器里，是因为它决定"发出去的 XML 长什么样"，
//! 属于协议契约的一部分；桌面端也能用同一份结构做 XML 预览，保证预览与实际
//! 发出的报文同源。

use crate::manscdp::AlarmNotify;
use common::{Error, Result};

/// 一次报警上报的可变字段（桌面端「报警单管理」页直接映射）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AlarmReport {
    /// 报警级别 1-4(1 最高)。
    pub priority: u8,
    /// 报警方式 1-7(1 电话/2 设备/3 短信/4 GPS/5 视频/6 器件/7 人工)。
    pub method: u8,
    /// 报警类型(1 视频丢失/2 设备防拆/3 存储满/4 设备故障/5 其他)。None 表示不输出 `<Info>`。
    #[serde(default)]
    pub alarm_type: Option<u32>,
    /// 报警类型参数(可选补充说明)。
    #[serde(default)]
    pub alarm_type_param: Option<String>,
    /// 报警描述。
    #[serde(default)]
    pub description: Option<String>,
    /// 经度(可选,-180~180)。
    #[serde(default)]
    pub longitude: Option<f64>,
    /// 纬度(可选,-90~90)。
    #[serde(default)]
    pub latitude: Option<f64>,
    /// 报警时间相对现在的分钟数(0=现在,正数=往前挪)。只改时间戳,不延迟发送。
    #[serde(default)]
    pub minutes_ago: i64,
}

impl Default for AlarmReport {
    fn default() -> Self {
        Self::video("移动侦测报警")
    }
}

impl AlarmReport {
    /// 默认视频侦测报警单(与既有 `report_alarm` 行为一致)。
    pub fn video(description: impl Into<String>) -> Self {
        Self {
            priority: 1,
            method: 5,
            alarm_type: None,
            alarm_type_param: None,
            description: Some(description.into()),
            longitude: None,
            latitude: None,
            minutes_ago: 0,
        }
    }

    /// 预检字段取值。桌面端在发送前也会先调一次，避免把明显非法的值发到平台。
    pub fn validate(&self) -> Result<()> {
        if !(1..=4).contains(&self.priority) {
            return Err(Error::Gb28181(format!(
                "报警级别必须是 1-4，当前 {}",
                self.priority
            )));
        }
        if !(1..=7).contains(&self.method) {
            return Err(Error::Gb28181(format!(
                "报警方式必须是 1-7，当前 {}",
                self.method
            )));
        }
        if let Some(alarm_type) = self.alarm_type {
            if !(1..=5).contains(&alarm_type) {
                return Err(Error::Gb28181(format!(
                    "报警类型必须是 1-5，当前 {alarm_type}"
                )));
            }
        }
        if let Some(longitude) = self.longitude {
            if longitude.is_nan() || !(-180.0..=180.0).contains(&longitude) {
                return Err(Error::Gb28181(format!(
                    "经度必须在 -180~180，当前 {longitude}"
                )));
            }
        }
        if let Some(latitude) = self.latitude {
            if latitude.is_nan() || !(-90.0..=90.0).contains(&latitude) {
                return Err(Error::Gb28181(format!(
                    "纬度必须在 -90~90，当前 {latitude}"
                )));
            }
        }
        if !(0..=10_080).contains(&self.minutes_ago) {
            return Err(Error::Gb28181(format!(
                "报警时间偏移只能在 0~10080 分钟(7 天)，当前 {}",
                self.minutes_ago
            )));
        }
        Ok(())
    }

    /// 组装 Alarm Notify。先预检，非法字段不会走到序列化。
    pub fn to_notify(&self, device_id: impl Into<String>, sn: u32) -> Result<AlarmNotify> {
        self.validate()?;
        Ok(AlarmNotify::custom(
            device_id,
            sn,
            alarm_time(self.minutes_ago),
            self.priority,
            self.method,
            self.description.clone(),
            self.longitude.map(format_coord),
            self.latitude.map(format_coord),
            self.alarm_type,
            self.alarm_type_param.clone(),
        ))
    }
}

/// 报警时间:校时后的时间往前挪 `minutes_ago` 分钟。
pub fn alarm_time(minutes_ago: i64) -> String {
    let synced = common::clock::synced_epoch_secs();
    common::clock::epoch_to_iso8601(synced - minutes_ago.max(0) * 60)
}

/// 经纬度格式化:最多 6 位小数并去掉末尾 0。
///
/// 用字符串进 XML 而不是直接序列化 f64，是为了避免 `1.0` 变成 `1`、以及 `-0`
/// 这类平台侧解析告警；同时保持"输入多少就发多少"的可核对性。
pub fn format_coord(value: f64) -> String {
    let text = format!("{value:.6}");
    let trimmed = if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.')
    } else {
        text.as_str()
    };
    if trimmed == "-0" || trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_out_of_range_fields() {
        let mut report = AlarmReport::video("x");
        report.priority = 0;
        assert!(report.validate().is_err(), "级别 0 应被拒绝");

        let mut report = AlarmReport::video("x");
        report.method = 8;
        assert!(report.validate().is_err(), "方式 8 应被拒绝");

        let mut report = AlarmReport::video("x");
        report.alarm_type = Some(9);
        assert!(report.validate().is_err(), "类型 9 应被拒绝");

        let mut report = AlarmReport::video("x");
        report.longitude = Some(200.0);
        assert!(report.validate().is_err(), "经度 200 应被拒绝");

        let mut report = AlarmReport::video("x");
        report.latitude = Some(-91.0);
        assert!(report.validate().is_err(), "纬度 -91 应被拒绝");

        let mut report = AlarmReport::video("x");
        report.minutes_ago = -5;
        assert!(report.validate().is_err(), "负数分钟应被拒绝");
    }

    #[test]
    fn formats_coord_without_float_noise() {
        assert_eq!(format_coord(116.397128), "116.397128");
        assert_eq!(format_coord(39.9), "39.9");
        assert_eq!(format_coord(1.0), "1");
        assert_eq!(format_coord(-0.0), "0");
        assert_eq!(format_coord(0.0), "0");
        // 超长小数截到 6 位，平台按 1e-6 度解析足够。
        assert_eq!(format_coord(116.3971284999), "116.397128");
    }

    #[test]
    fn notify_carries_all_requested_fields() {
        let report = AlarmReport {
            priority: 3,
            method: 2,
            alarm_type: Some(2),
            alarm_type_param: Some("外壳".into()),
            description: Some("设备防拆".into()),
            longitude: Some(116.397128),
            latitude: Some(39.916527),
            minutes_ago: 0,
        };
        let notify = report.to_notify("34020000001320000001", 5).expect("组装");
        assert_eq!(notify.priority, 3);
        assert_eq!(notify.method, 2);
        assert_eq!(notify.longitude.as_deref(), Some("116.397128"));
        assert_eq!(notify.latitude.as_deref(), Some("39.916527"));
        assert_eq!(notify.info.as_ref().map(|info| info.alarm_type), Some(2));
        let xml = notify.to_xml().expect("序列化");
        assert!(xml.contains("<Info><AlarmType>2</AlarmType>"), "实际: {xml}");
    }

    #[test]
    fn invalid_report_never_reaches_xml() {
        let mut report = AlarmReport::video("x");
        report.priority = 9;
        assert!(report.to_notify("3402", 1).is_err());
    }

    /// "N 分钟前" 必须把时间戳往前挪，而不是延迟发送。
    #[test]
    fn minutes_ago_moves_timestamp_backwards() {
        let now = alarm_time(0);
        let earlier = alarm_time(30);
        assert_ne!(now, earlier);
        let parse = |value: &str| common::clock::parse_iso8601(value).expect("可解析");
        assert_eq!(parse(&now) - parse(&earlier), 30 * 60);
    }
}
