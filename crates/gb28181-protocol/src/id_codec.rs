//! 国标 20 位 ID 编解码。
//!
//! 实现 docs/40-protocol/manscdp.md#id-编码:20 位 = 中心编码(8) + 行业(2) +
//! 类型(3) + 序号(7)。压测批量生成设备 ID 时按序号递增(FR-20)。

use common::{DeviceId, Error, Result};

/// 拆解后的国标 ID 各字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdParts {
    /// 中心/区域编码(8 位)。
    pub center: String,
    /// 行业编码(2 位)。
    pub industry: String,
    /// 设备类型编码(3 位),如 132=视频通道、200=设备。
    pub type_code: String,
    /// 序号(7 位)。
    pub serial: u32,
}

impl IdParts {
    /// 从 20 位 ID 拆解。
    pub fn parse(id: &DeviceId) -> Result<Self> {
        let s = id.as_str();
        // DeviceId 构造已保证 20 位纯数字,这里直接切片。
        let serial = s[13..20]
            .parse::<u32>()
            .map_err(|_| Error::Gb28181(format!("序号段非法: {s}")))?;
        Ok(IdParts {
            center: s[0..8].to_string(),
            industry: s[8..10].to_string(),
            type_code: s[10..13].to_string(),
            serial,
        })
    }

    /// 组装回 20 位 ID。
    pub fn to_id(&self) -> Result<DeviceId> {
        DeviceId::new(format!(
            "{}{}{}{:07}",
            self.center, self.industry, self.type_code, self.serial
        ))
    }
}

/// 批量生成设备 ID(压测用)。
///
/// 以 `prefix`(前 13 位:中心8+行业2+类型3)为基,序号从 `start_index` 起递增 `count` 个。
/// prefix 必须恰好 13 位数字,否则 [`Error::Gb28181`]。
pub fn gen_device_ids(prefix: &str, start_index: u32, count: u32) -> Result<Vec<DeviceId>> {
    if prefix.len() != 13 || !prefix.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::Gb28181(format!(
            "ID 前缀需 13 位数字(中心8+行业2+类型3),实际: {prefix}"
        )));
    }
    (start_index..start_index + count)
        .map(|i| DeviceId::new(format!("{prefix}{i:07}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 拆解与组装往返() {
        let id = DeviceId::new("34020000001320000001").unwrap();
        let parts = IdParts::parse(&id).unwrap();
        assert_eq!(parts.center, "34020000");
        assert_eq!(parts.industry, "00");
        assert_eq!(parts.type_code, "132");
        assert_eq!(parts.serial, 1);
        assert_eq!(parts.to_id().unwrap(), id);
    }

    #[test]
    fn 批量生成序号递增() {
        let ids = gen_device_ids("3402000000132", 1, 3).unwrap();
        assert_eq!(ids.len(), 3);
        assert_eq!(ids[0].as_str(), "34020000001320000001");
        assert_eq!(ids[2].as_str(), "34020000001320000003");
    }

    #[test]
    fn 非法前缀被拒() {
        assert!(gen_device_ids("123", 1, 1).is_err());
    }
}
