//! 设备安装位置与摄像头类型的唯一真值表（Rust 侧）。
//!
//! 配置存稳定英文 id，界面与 MANSCDP 报文用中文标签。前端同名单在
//! `src/device-metadata.ts`，由 `device-metadata-contract.test.mjs` 逐项核对。
//! 空串表示「未设置」：此时目录 `<Address>` 与设备信息 `<DeviceType>` 都不输出，
//! 与加这两个字段之前的报文逐字节一致。

/// (配置 id, 界面/报文标签)。
pub const LOCATION_OPTIONS: &[(&str, &str)] = &[
    ("main_gate", "园区大门"),
    ("lobby", "大厅"),
    ("corridor", "走廊"),
    ("elevator", "电梯"),
    ("parking", "停车场"),
    ("server_room", "机房"),
    ("perimeter", "周界围墙"),
    ("main_road", "主干道"),
    ("other", "其他"),
];

pub const CAMERA_TYPE_OPTIONS: &[(&str, &str)] = &[
    ("bullet", "固定枪机"),
    ("dome", "半球"),
    ("ptz_dome", "球机(PTZ)"),
    ("pan_tilt", "云台"),
    ("panoramic", "全景"),
    ("checkpoint", "卡口电警"),
    ("thermal", "热成像"),
    ("other", "其他"),
];

fn label(options: &'static [(&'static str, &'static str)], id: &str) -> Option<&'static str> {
    options
        .iter()
        .find(|(candidate, _)| *candidate == id)
        .map(|(_, label)| *label)
}

/// 目录 Item 的 `<Address>` 值；未知 id 返回 None，绝不把非法值拼进 XML。
pub fn location_label(id: &str) -> Option<&'static str> {
    label(LOCATION_OPTIONS, id)
}

/// DeviceInfo 应答的 `<DeviceType>` 值；未知 id 返回 None。
pub fn camera_type_label(id: &str) -> Option<&'static str> {
    label(CAMERA_TYPE_OPTIONS, id)
}

fn validate(options: &'static [(&'static str, &'static str)], field: &str, id: &str) -> Result<(), String> {
    if id.is_empty() || label(options, id).is_some() {
        Ok(())
    } else {
        Err(format!("{field} 取值不在可选项内: {id}"))
    }
}

pub fn validate_location(id: &str) -> Result<(), String> {
    validate(LOCATION_OPTIONS, "安装位置", id)
}

pub fn validate_camera_type(id: &str) -> Result<(), String> {
    validate(CAMERA_TYPE_OPTIONS, "摄像头类型", id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 未设置时不出标签() {
        assert_eq!(location_label(""), None);
        assert_eq!(camera_type_label(""), None);
        assert!(validate_location("").is_ok());
        assert!(validate_camera_type("").is_ok());
    }

    #[test]
    fn 已知选项映射到中文标签() {
        assert_eq!(location_label("parking"), Some("停车场"));
        assert_eq!(camera_type_label("ptz_dome"), Some("球机(PTZ)"));
    }

    #[test]
    fn 未知取值被拒绝且不产生标签() {
        assert_eq!(location_label("<script>"), None);
        assert!(validate_location("<script>").is_err());
        assert!(validate_camera_type("unknown").is_err());
    }
}
