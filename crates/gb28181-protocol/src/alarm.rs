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
    /// 报警类型。取值随报警方式变化(方式 2: 1-5 / 方式 5: 1-13 / 方式 6: 1-2，
    /// 其余方式标准不允许携带)。None 表示不输出 `<Info>`。
    /// 名称表见 [`alarm_type_names`]，与平台展示文案同口径。
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

/// 报警方式取值与名称(GB/T 28181-2016 A.2.5.3)。
pub const ALARM_METHODS: &[(u8, &str)] = &[
    (1, "电话"),
    (2, "设备"),
    (3, "短信"),
    (4, "GPS"),
    (5, "视频"),
    (6, "器件故障"),
    (7, "人工"),
];

/// 报警方式 2(设备报警)的类型表。
const ALARM_TYPES_DEVICE: &[(u32, &str)] = &[
    (1, "视频丢失报警"),
    (2, "设备防拆报警"),
    (3, "存储设备磁盘满报警"),
    (4, "设备高温报警"),
    (5, "设备低温报警"),
];

/// 报警方式 5(视频报警)的类型表；13 是 2022 版新增的图像遮挡。
const ALARM_TYPES_VIDEO: &[(u32, &str)] = &[
    (1, "人工视频报警"),
    (2, "运动目标检测报警"),
    (3, "遗留物检测报警"),
    (4, "物体移除检测报警"),
    (5, "绊线检测报警"),
    (6, "入侵检测报警"),
    (7, "逆行检测报警"),
    (8, "徘徊检测报警"),
    (9, "流量统计报警"),
    (10, "密度检测报警"),
    (11, "视频异常检测报警"),
    (12, "快速移动报警"),
    (13, "图像遮挡报警（2022）"),
];

/// 报警方式 6(器件故障)的类型表。
const ALARM_TYPES_FAULT: &[(u32, &str)] = &[(1, "存储设备磁盘故障报警"), (2, "存储设备风扇故障报警")];

/// 某报警方式的类型名称表。`None` = 标准未给该方式定义类型，不允许携带 `<AlarmType>`。
///
/// 这份表是「发什么」的唯一依据：协议层校验、桌面端下拉、平台展示都按它对齐，
/// 避免再出现「方式 7 + 类型 5」这种两边都查不到的组合。
pub fn alarm_type_names(method: u8) -> Option<&'static [(u32, &'static str)]> {
    match method {
        2 => Some(ALARM_TYPES_DEVICE),
        5 => Some(ALARM_TYPES_VIDEO),
        6 => Some(ALARM_TYPES_FAULT),
        _ => None,
    }
}

/// 某报警方式允许的类型取值范围 `(min, max)`；`None` = 不允许携带类型。
pub fn alarm_type_range(method: u8) -> Option<(u32, u32)> {
    let names = alarm_type_names(method)?;
    let min = names.first()?.0;
    let max = names.last()?.0;
    Some((min, max))
}

/// 报警方式名称(未知取值原样返回数字)。
pub fn alarm_method_name(method: u8) -> String {
    ALARM_METHODS
        .iter()
        .find(|(value, _)| *value == method)
        .map(|(_, name)| (*name).to_string())
        .unwrap_or_else(|| method.to_string())
}

/// 某报警方式下某个类型的名称；方式无类型表或取值不在表中时返回 `None`。
pub fn alarm_type_name(method: u8, alarm_type: u32) -> Option<&'static str> {
    alarm_type_names(method)?
        .iter()
        .find(|(value, _)| *value == alarm_type)
        .map(|(_, name)| *name)
}

impl Default for AlarmReport {
    fn default() -> Self {
        Self::video("移动侦测报警")
    }
}

impl AlarmReport {
    /// 默认视频侦测报警单(与既有 `report_alarm` 行为一致)。
    ///
    /// 带类型 2(运动目标检测):没有 `<Info>` 的平台只能显示「未知」，
    /// 而视频报警的可选类型里最贴近「移动侦测」的就是 2。
    pub fn video(description: impl Into<String>) -> Self {
        Self {
            priority: 1,
            method: 5,
            alarm_type: Some(2),
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
            match alarm_type_range(self.method) {
                None => {
                    return Err(Error::Gb28181(format!(
                        "报警方式 {} 不允许携带报警类型(标准只给方式 2/5/6 定义了类型)",
                        self.method
                    )));
                }
                Some((min, max)) => {
                    if !(min..=max).contains(&alarm_type) {
                        return Err(Error::Gb28181(format!(
                            "报警方式 {} 的报警类型必须是 {min}-{max}，当前 {alarm_type}",
                            self.method
                        )));
                    }
                }
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
        report.alarm_type = Some(14);
        assert!(report.validate().is_err(), "视频报警类型 14 应被拒绝");

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

    /// 类型合法区间随报警方式变化：2→1-5、5→1-13、6→1-2，其余方式不允许带类型。
    #[test]
    fn alarm_type_range_follows_method() {
        assert_eq!(alarm_type_range(2), Some((1, 5)));
        assert_eq!(alarm_type_range(5), Some((1, 13)));
        assert_eq!(alarm_type_range(6), Some((1, 2)));
        for method in [1, 3, 4, 7, 8] {
            assert_eq!(alarm_type_range(method), None, "方式 {method} 不应有类型表");
        }

        let with = |method: u8, alarm_type: u32| {
            let mut report = AlarmReport::video("x");
            report.method = method;
            report.alarm_type = Some(alarm_type);
            report.validate()
        };
        assert!(with(2, 5).is_ok(), "方式 2 上限 5 应通过");
        assert!(with(2, 6).is_err(), "方式 2 的 6 应被拒绝");
        assert!(with(5, 13).is_ok(), "方式 5 的 2022 类型 13 应通过");
        assert!(with(6, 2).is_ok(), "方式 6 的上限 2 应通过");
        assert!(with(6, 4).is_err(), "方式 6 的 4 应被拒绝");
        assert!(with(7, 5).is_err(), "方式 7 不允许携带类型");

        // 不带类型时，任何方式都合法（方式 7 就是这种形态）。
        let mut report = AlarmReport::video("x");
        report.method = 7;
        report.alarm_type = None;
        assert!(report.validate().is_ok());
    }

    /// 默认视频报警要带「运动目标检测」，否则平台只能显示「未知」。
    #[test]
    fn default_video_report_carries_motion_type() {
        let report = AlarmReport::video("移动侦测报警");
        assert_eq!(report.alarm_type, Some(2));
        let xml = report
            .to_notify("34020000001320000001", 1)
            .expect("组装")
            .to_xml()
            .expect("序列化");
        assert!(xml.contains("<Info><AlarmType>2</AlarmType></Info>"), "实际: {xml}");
    }

    /// 类型名称表要覆盖标准里的每一个取值，避免 UI 出现空标签。
    #[test]
    fn every_standard_type_has_a_name() {
        for (method, count) in [(2u8, 5usize), (5, 13), (6, 2)] {
            let names = alarm_type_names(method).expect("应有类型表");
            assert_eq!(names.len(), count, "方式 {method} 类型数量不符");
            for (index, (value, name)) in names.iter().enumerate() {
                assert_eq!(*value, index as u32 + 1, "方式 {method} 类型取值应连续");
                assert!(!name.is_empty(), "方式 {method} 类型 {value} 缺名称");
                assert_eq!(alarm_type_name(method, *value), Some(*name));
            }
        }
        assert_eq!(alarm_type_name(7, 5), None);
        assert_eq!(alarm_method_name(6), "器件故障");
        assert_eq!(alarm_method_name(9), "9");
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
