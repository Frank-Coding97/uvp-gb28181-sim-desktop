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

/// 目录节点类型(20 位国标 ID 第 11-13 位类型码 + 是否为目录)。
/// 据上游 uvp-gb28181-sim CatalogNodeType 核对。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogNodeType {
    /// 设备(根),typeCode 111,parental=1。
    Device,
    /// 业务分组,typeCode 137,parental=1。
    BusinessGroup,
    /// 虚拟组织(行政区划),typeCode 138,parental=1。
    VirtualOrg,
    /// 视频通道,typeCode 132,parental=0。
    VideoChannel,
    /// 报警通道,typeCode 134,parental=0。
    AlarmChannel,
}

impl CatalogNodeType {
    /// 类型码(ID 第 11-13 位)。
    pub fn type_code(&self) -> &'static str {
        match self {
            CatalogNodeType::Device => "111",
            CatalogNodeType::BusinessGroup => "137",
            CatalogNodeType::VirtualOrg => "138",
            CatalogNodeType::VideoChannel => "132",
            CatalogNodeType::AlarmChannel => "134",
        }
    }

    /// 是否为目录节点(有子)。1=目录,0=叶子通道。
    pub fn parental(&self) -> u8 {
        match self {
            CatalogNodeType::Device
            | CatalogNodeType::BusinessGroup
            | CatalogNodeType::VirtualOrg => 1,
            CatalogNodeType::VideoChannel | CatalogNodeType::AlarmChannel => 0,
        }
    }
}

/// 生成子节点 ID:domain 前 10 位 + 类型码(3)+ 序号(7 位补零)= 20 位。
/// 与上游 IdEncoder.genChildId 一致。
pub fn gen_child_id(domain: &str, node_type: CatalogNodeType, seq: u32) -> String {
    let prefix: String = domain.chars().take(10).collect();
    let prefix = format!("{prefix:0<10}");
    format!("{prefix}{}{:07}", node_type.type_code(), seq)
}

/// 一个目录节点。层级靠 parent_id(根节点 parent_id==id)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogNode {
    /// 20 位国标 ID。
    pub id: String,
    /// 节点类型。
    pub node_type: CatalogNodeType,
    /// 名称。
    pub name: String,
    /// 父节点 ID。
    pub parent_id: String,
    /// 行政区划(仅虚拟组织/其下通道携带)。
    pub civil_code: Option<String>,
    /// 状态:ON / OFF。
    pub status: String,
}

impl CatalogNode {
    /// 便捷构造一个 ON 状态节点。
    pub fn new(
        id: impl Into<String>,
        node_type: CatalogNodeType,
        name: impl Into<String>,
        parent_id: impl Into<String>,
    ) -> Self {
        CatalogNode {
            id: id.into(),
            node_type,
            name: name.into(),
            parent_id: parent_id.into(),
            civil_code: None,
            status: "ON".into(),
        }
    }

    /// 带行政区划。
    pub fn with_civil_code(mut self, civil_code: impl Into<String>) -> Self {
        self.civil_code = Some(civil_code.into());
        self
    }
}

/// 内置目录模板。据上游 CatalogTreeStore.templates 核对(single/nvr-8ch/civil-3x2/large-16ch)。
///
/// - `single`:根设备 + 1 视频通道 + 1 报警通道。
/// - `nvr-8ch`:根 + 业务分组 NVR-8 + 8 视频通道。
/// - `civil-3x2`:根 + 3 虚拟组织(浦东/黄浦/徐汇)各 2 通道,演示 CivilCode。
/// - `large-16ch`:根 + 室内/室外 2 分组各 8 通道 + 1 报警通道挂根。
///
/// 返回节点列表(首个为根设备);`domain` 取前 10 位作 ID 前缀。
pub fn catalog_template(
    template: &str,
    device_id: &str,
    device_name: &str,
    domain: &str,
) -> Vec<CatalogNode> {
    let root = || CatalogNode::new(device_id, CatalogNodeType::Device, device_name, device_id);
    let vid = |seq: u32| gen_child_id(domain, CatalogNodeType::VideoChannel, seq);
    let alarm = |seq: u32| gen_child_id(domain, CatalogNodeType::AlarmChannel, seq);
    let group = |seq: u32| gen_child_id(domain, CatalogNodeType::BusinessGroup, seq);
    let org = |seq: u32| gen_child_id(domain, CatalogNodeType::VirtualOrg, seq);

    match template {
        "nvr-8ch" => {
            let mut v = vec![root()];
            let g = group(1);
            v.push(CatalogNode::new(
                &g,
                CatalogNodeType::BusinessGroup,
                "NVR-8",
                device_id,
            ));
            for i in 1..=8 {
                v.push(CatalogNode::new(
                    vid(i),
                    CatalogNodeType::VideoChannel,
                    format!("通道-{i:02}"),
                    &g,
                ));
            }
            v
        }
        "civil-3x2" => {
            let mut v = vec![root()];
            let areas = [("浦东", "310115"), ("黄浦", "310101"), ("徐汇", "310104")];
            let mut ch_seq = 1u32;
            for (idx, (area, civil)) in areas.iter().enumerate() {
                let o = org(idx as u32 + 1);
                v.push(
                    CatalogNode::new(
                        &o,
                        CatalogNodeType::VirtualOrg,
                        format!("{area}区"),
                        device_id,
                    )
                    .with_civil_code(*civil),
                );
                for k in 1..=2 {
                    v.push(
                        CatalogNode::new(
                            vid(ch_seq),
                            CatalogNodeType::VideoChannel,
                            format!("{area}-{k}号"),
                            &o,
                        )
                        .with_civil_code(*civil),
                    );
                    ch_seq += 1;
                }
            }
            v
        }
        "large-16ch" => {
            let mut v = vec![root()];
            let indoor = group(1);
            let outdoor = group(2);
            v.push(CatalogNode::new(
                &indoor,
                CatalogNodeType::BusinessGroup,
                "室内监控",
                device_id,
            ));
            v.push(CatalogNode::new(
                &outdoor,
                CatalogNodeType::BusinessGroup,
                "室外监控",
                device_id,
            ));
            let mut ch_seq = 1u32;
            for i in 1..=8 {
                v.push(CatalogNode::new(
                    vid(ch_seq),
                    CatalogNodeType::VideoChannel,
                    format!("室内-{i:02}"),
                    &indoor,
                ));
                ch_seq += 1;
            }
            for i in 1..=8 {
                v.push(CatalogNode::new(
                    vid(ch_seq),
                    CatalogNodeType::VideoChannel,
                    format!("室外-{i:02}"),
                    &outdoor,
                ));
                ch_seq += 1;
            }
            v.push(CatalogNode::new(
                alarm(1),
                CatalogNodeType::AlarmChannel,
                "总报警",
                device_id,
            ));
            v
        }
        // "single" 及未知:默认单设备 + 1 视频 + 1 报警。
        _ => vec![
            root(),
            CatalogNode::new(
                vid(1),
                CatalogNodeType::VideoChannel,
                format!("{device_name}-通道1"),
                device_id,
            ),
            CatalogNode::new(
                alarm(1),
                CatalogNodeType::AlarmChannel,
                format!("{device_name}-报警"),
                device_id,
            ),
        ],
    }
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
    fn 子节点id组成() {
        assert_eq!(
            gen_child_id("3402000000", CatalogNodeType::VideoChannel, 1),
            "34020000001320000001"
        );
        assert_eq!(
            gen_child_id("3402000000", CatalogNodeType::AlarmChannel, 1),
            "34020000001340000001"
        );
        assert_eq!(CatalogNodeType::BusinessGroup.type_code(), "137");
        assert_eq!(CatalogNodeType::VirtualOrg.parental(), 1);
        assert_eq!(CatalogNodeType::VideoChannel.parental(), 0);
    }

    #[test]
    fn 模板节点数与结构() {
        let dom = "3402000000";
        let single = catalog_template("single", "34020000001110000001", "Cam", dom);
        assert_eq!(single.len(), 3); // 根 + 视频 + 报警
        assert_eq!(single[0].node_type, CatalogNodeType::Device);

        let nvr = catalog_template("nvr-8ch", "34020000001110000001", "Cam", dom);
        assert_eq!(nvr.len(), 10); // 根 + 分组 + 8 通道
        assert_eq!(nvr[1].node_type, CatalogNodeType::BusinessGroup);
        assert!(
            nvr.iter()
                .filter(|n| n.node_type == CatalogNodeType::VideoChannel)
                .count()
                == 8
        );

        let civil = catalog_template("civil-3x2", "34020000001110000001", "Cam", dom);
        assert_eq!(civil.len(), 10); // 根 + 3 组织 + 6 通道
        assert_eq!(civil[1].civil_code.as_deref(), Some("310115"));

        let large = catalog_template("large-16ch", "34020000001110000001", "Cam", dom);
        assert_eq!(large.len(), 20); // 根 + 2 分组 + 16 通道 + 1 报警
        assert!(large
            .iter()
            .any(|n| n.node_type == CatalogNodeType::AlarmChannel));
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
