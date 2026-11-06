//! PTZ 六字段快照（GB/T 28181-2022 附录 A.2.6.15）。
//!
//! `PTZPosition` 查询应答与通知的报文体一共六项：三轴姿态 `Pan` / `Tilt` / `Zoom`，
//! 加三项光学量 `HorizontalFieldAngle` / `VerticalFieldAngle` / `MaxViewDistance`。
//! 规范里六项都是 `minOccurs="0"`，但只要设备能给出值就应该全报 —— 平台侧靠光学三量
//! 换算实际视场范围，缺项会让覆盖面积算不出来。
//!
//! 光学三量**不是独立读数**，而是变焦倍数的函数：变焦越大视场越窄、可视距离越远。
//! 因此这里只保留一组基准常量 + 一个换算函数，任何上报路径都从这里取，
//! 避免「报 3 倍变焦却带 1 倍的视场角」这种自相矛盾的报文。

/// 1 倍变焦时的水平视场角（度）。
pub const BASE_HORIZONTAL_FIELD_ANGLE_DEG: f32 = 60.0;
/// 1 倍变焦时的垂直视场角（度）。
pub const BASE_VERTICAL_FIELD_ANGLE_DEG: f32 = 36.0;
/// 1 倍变焦时的最大可视距离（米）。
pub const BASE_MAX_VIEW_DISTANCE_M: f32 = 300.0;

/// 由变焦倍数换算出的光学三量。`zoom` 非法（NaN/小于 1）时按 1 倍处理。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PtzOptics {
    pub horizontal_field_angle_deg: f32,
    pub vertical_field_angle_deg: f32,
    pub max_view_distance_m: f32,
}

impl PtzOptics {
    /// 按变焦倍数换算。倍数下限钳到 1（规范要求 `Zoom >= 1.00`）。
    pub fn from_zoom(zoom: f32) -> Self {
        let effective_zoom = if zoom.is_finite() { zoom.max(1.0) } else { 1.0 };
        PtzOptics {
            horizontal_field_angle_deg: BASE_HORIZONTAL_FIELD_ANGLE_DEG / effective_zoom,
            vertical_field_angle_deg: BASE_VERTICAL_FIELD_ANGLE_DEG / effective_zoom,
            max_view_distance_m: BASE_MAX_VIEW_DISTANCE_M * effective_zoom,
        }
    }
}

/// 一次完整的六字段姿态快照。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PtzPosition {
    pub pan: f32,
    pub tilt: f32,
    pub zoom: f32,
}

impl PtzPosition {
    pub fn new(pan: f32, tilt: f32, zoom: f32) -> Self {
        PtzPosition { pan, tilt, zoom }
    }

    /// 当前变焦下的光学三量。
    pub fn optics(&self) -> PtzOptics {
        PtzOptics::from_zoom(self.zoom)
    }
}

/// 报文统一用两位小数（与 `Pan` / `Tilt` / `Zoom` 的格式化口径一致）。
///
/// 负零会被归一成 `0.00`：`-0.00` 在报文里是合法浮点但读起来像负数，
/// 平台侧做「是否居中」判断时容易被误判。
pub(crate) fn format_two(value: f32) -> String {
    if !value.is_finite() {
        return "0.00".to_string();
    }
    let value = if value == 0.0 { 0.0 } else { value };
    format!("{value:.2}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 一倍变焦时视场角与可视距离取基准值() {
        let optics = PtzOptics::from_zoom(1.0);
        assert_eq!(optics.horizontal_field_angle_deg, 60.0);
        assert_eq!(optics.vertical_field_angle_deg, 36.0);
        assert_eq!(optics.max_view_distance_m, 300.0);
    }

    #[test]
    fn 三倍变焦把视场角除以倍数并把可视距离乘以倍数() {
        let optics = PtzOptics::from_zoom(3.0);
        assert_eq!(optics.horizontal_field_angle_deg, 20.0);
        assert_eq!(optics.vertical_field_angle_deg, 12.0);
        assert_eq!(optics.max_view_distance_m, 900.0);
    }

    #[test]
    fn 倍数小于一下限钳到一倍() {
        // 小数变焦在协议里非法（Zoom >= 1.00），但它会出现在固件回读的脏数据里，
        // 钳住比让视场角膨胀到 120 度安全。
        let optics = PtzOptics::from_zoom(0.4);
        assert_eq!(optics.horizontal_field_angle_deg, 60.0);
        assert_eq!(optics.max_view_distance_m, 300.0);
    }

    #[test]
    fn 非有限倍数退化为基准值() {
        assert_eq!(PtzOptics::from_zoom(f32::NAN), PtzOptics::from_zoom(1.0));
        assert_eq!(PtzOptics::from_zoom(f32::INFINITY), PtzOptics::from_zoom(1.0));
    }

    #[test]
    fn 姿态快照的光学量与变焦一致() {
        let position = PtzPosition::new(45.0, -10.0, 4.0);
        let optics = position.optics();
        assert_eq!(optics.horizontal_field_angle_deg, 15.0);
        assert_eq!(optics.vertical_field_angle_deg, 9.0);
        assert_eq!(optics.max_view_distance_m, 1200.0);
    }

    #[test]
    fn 两位小数格式化对非有限值与负零兜底() {
        assert_eq!(format_two(17.137), "17.14");
        assert_eq!(format_two(-0.0), "0.00");
        assert_eq!(format_two(f32::NAN), "0.00");
    }
}
