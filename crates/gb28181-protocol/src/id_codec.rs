//! 国标 20 位 ID 编解码。
//!
//! 实现 GB/T 28181-2022 **附录 E 表 E.1**:20 位 = 中心编码(8) + 行业编码(2) +
//! 类型编码(3) + **网络标识(1)** + 序号(6)。压测批量生成设备 ID 时按序号递增(FR-20)。
//!
//! 注意第 14 位是**网络标识**而不是序号的一部分(0/1-公安视频传输网、2-行业专网、
//! 3-政法信息网、4-公安移动信息网、5-公安信息网、6-电子政务外网、7-互联网等公共网络、
//! 8-专线、9-预留)。把它并进序号在序号小于 100 万时看不出问题,一旦超过就会静默
//! 改写网络标识,得到一个语义完全不同的 ID。

use common::{DeviceId, Error, Result};

/// 拆解后的国标 ID 各字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdParts {
    /// 中心/区域编码(8 位)。
    pub center: String,
    /// 行业编码(2 位)。
    pub industry: String,
    /// 设备类型编码(3 位),如 132=网络摄像机、215=业务分组、216=虚拟组织。
    pub type_code: String,
    /// 网络标识(1 位,附录 E 表 E.1)。
    pub network_id: u8,
    /// 序号(6 位)。
    pub serial: u32,
}

impl IdParts {
    /// 从 20 位 ID 拆解。
    pub fn parse(id: &DeviceId) -> Result<Self> {
        let s = id.as_str();
        // DeviceId 构造已保证 20 位纯数字,这里直接切片。
        let network_id = s[13..14]
            .parse::<u8>()
            .map_err(|_| Error::Gb28181(format!("网络标识段非法: {s}")))?;
        let serial = s[14..20]
            .parse::<u32>()
            .map_err(|_| Error::Gb28181(format!("序号段非法: {s}")))?;
        Ok(IdParts {
            center: s[0..8].to_string(),
            industry: s[8..10].to_string(),
            type_code: s[10..13].to_string(),
            network_id,
            serial,
        })
    }

    /// 组装回 20 位 ID。
    pub fn to_id(&self) -> Result<DeviceId> {
        if self.serial > MAX_SERIAL {
            return Err(Error::Gb28181(format!(
                "序号 {} 超过附录 E 的 6 位上限 {MAX_SERIAL}",
                self.serial
            )));
        }
        DeviceId::new(format!(
            "{}{}{}{}{:06}",
            self.center, self.industry, self.type_code, self.network_id, self.serial
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

/// 序号段(6 位)的最大值,见附录 E 表 E.1。
pub const MAX_SERIAL: u32 = 999_999;

/// 目录节点类型(20 位国标 ID 第 11-13 位类型码 + 是否为目录)。
///
/// 类型码取值按 **GB/T 28181-2022 附录 E 表 E.1**。这里曾沿用上游 Android 项目的
/// 137/138 表示业务分组/虚拟组织 —— 但表 E.1 里 137 是**语音输出设备**、138 是
/// **移动传输设备**,业务分组与虚拟组织分别是 **215 / 216**(平台设备段 200~299)。
/// 用错类型码的后果是平台把业务分组解析成一台语音输出设备。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogNodeType {
    /// 设备(根):DVR 编码 111,parental=1。
    Device,
    /// 业务分组:215,parental=1。
    BusinessGroup,
    /// 虚拟组织:216,parental=1。
    VirtualOrg,
    /// 视频通道:网络摄像机(IPC)编码 132,parental=0。
    VideoChannel,
    /// 报警通道:报警输入设备编码 134,parental=0。
    AlarmChannel,
}

impl CatalogNodeType {
    /// 类型码(ID 第 11-13 位),取值见附录 E 表 E.1。
    pub fn type_code(&self) -> &'static str {
        match self {
            CatalogNodeType::Device => "111",
            CatalogNodeType::BusinessGroup => "215",
            CatalogNodeType::VirtualOrg => "216",
            CatalogNodeType::VideoChannel => "132",
            CatalogNodeType::AlarmChannel => "134",
        }
    }

    /// 该类型在附录 E 表 E.1 里的中文名(诊断与界面提示用)。
    pub fn type_code_name(&self) -> &'static str {
        match self {
            CatalogNodeType::Device => "DVR",
            CatalogNodeType::BusinessGroup => "业务分组",
            CatalogNodeType::VirtualOrg => "虚拟组织",
            CatalogNodeType::VideoChannel => "网络摄像机(IPC)",
            CatalogNodeType::AlarmChannel => "报警输入设备",
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

/// 生成子节点 ID:domain 前 10 位(中心8+行业2)+ 类型码(3)+ 网络标识(1)+ 序号(6)= 20 位。
///
/// 网络标识固定取 0(公安视频传输网),序号超过 6 位时取模回绕 —— 宁可序号重复,
/// 也不能让溢出去改写网络标识那一位。
pub fn gen_child_id(domain: &str, node_type: CatalogNodeType, seq: u32) -> String {
    gen_child_id_on_network(domain, node_type, 0, seq)
}

/// 同 [`gen_child_id`],但显式指定网络标识(附录 E 表 E.1 第 14 位)。
pub fn gen_child_id_on_network(
    domain: &str,
    node_type: CatalogNodeType,
    network_id: u8,
    seq: u32,
) -> String {
    let prefix: String = domain.chars().take(10).collect();
    let prefix = format!("{prefix:0<10}");
    let network_id = network_id % 10;
    format!(
        "{prefix}{}{network_id}{:06}",
        node_type.type_code(),
        seq % (MAX_SERIAL + 1)
    )
}

/// 一个目录节点。层级靠 parent_id(根节点 parent_id==id)。
///
/// 含经纬度(f64)故不能 `Eq`。
#[derive(Debug, Clone, PartialEq)]
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
    /// 安装地址(A.2.1.9 `Address`)。为 None 时由设备侧填默认值。
    ///
    /// 逐通道可填:一台 NVR 下的 16 路摄像机装在不同位置,
    /// 全部报同一个地址会让平台的地图/清单失去意义。
    pub address: Option<String>,
    /// 经度(A.2.1.9 `Longitude`)。为 None 时取设备当前位置。
    pub longitude: Option<f64>,
    /// 纬度(A.2.1.9 `Latitude`)。为 None 时取设备当前位置。
    pub latitude: Option<f64>,
    /// 摄像机类型(`Info/PTZType`):1 球机、2 半球、3 固定枪机、4 遥控枪机。
    ///
    /// 平台按这个值决定要不要给这一路显示云台操作面板。
    pub ptz_type: Option<u8>,
    /// 摄像机采集部位类型(`Info/CapturePositionType`,**附录 O** 的 7 位层次码)。
    pub capture_position_type: Option<String>,
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
            address: None,
            longitude: None,
            latitude: None,
            ptz_type: None,
            capture_position_type: None,
        }
    }

    /// 带行政区划。
    pub fn with_civil_code(mut self, civil_code: impl Into<String>) -> Self {
        self.civil_code = Some(civil_code.into());
        self
    }

    /// 带安装地址(A.2.1.9 `Address`)。
    pub fn with_address(mut self, address: impl Into<String>) -> Self {
        self.address = Some(address.into());
        self
    }

    /// 带独立经纬度(A.2.1.9 `Longitude`/`Latitude`)。
    pub fn with_position(mut self, longitude: f64, latitude: f64) -> Self {
        self.longitude = Some(longitude);
        self.latitude = Some(latitude);
        self
    }
}

/// 目录查询的返回范围(GB/T 28181-2022 **附录 J**)。
///
/// 标准按被查询对象的类型规定了不同的返回范围,而不是"一律返回整棵树":
///
/// - a) 按**行政区域编码**查询:返回该行政区域目录项及属于此行政区域下的行政区域、设备目录项;
/// - b) 按**系统编码**查询:返回本级和下级系统的系统、行政区划、业务分组、虚拟组织、设备目录项;
/// - c) 按**设备编码**查询:返回该设备目录项及设备下属的设备目录项;
/// - d) 按**业务分组**查询:返回该业务分组目录项及属于此业务分组下的虚拟组织、设备目录项;
/// - e) 按**虚拟组织**查询:返回该虚拟组织目录项及属于此虚拟组织下的虚拟组织、设备目录项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogQueryScope {
    /// 查的是本设备(相当于附录 J 的 b):整棵树。
    WholeTree,
    /// 查的是行政区域编码(2/4/6/8 位):按 CivilCode 前缀匹配。
    CivilCode,
    /// 查的是树里某个节点:该节点 + 其后代。
    Subtree,
    /// 查的对象不在本设备的目录里。
    Unknown,
}

/// 判断一个查询 ID 是否是行政区域编码(附录 E:2/4/6/8 位)。
pub fn is_civil_code(id: &str) -> bool {
    matches!(id.len(), 2 | 4 | 6 | 8) && id.bytes().all(|b| b.is_ascii_digit())
}

/// 按附录 J 计算目录查询应返回的节点集合。
///
/// `root_id` 是本设备(根节点)的 ID。返回的顺序与 `tree` 保持一致 ——
/// 附录 H.2.1 要求平台信息列表的第一项是本级平台,保持原序最省心。
///
/// 查询对象不在目录里时返回空集合:配合附录 M 的"未查询出结果时 SumNum 取 0
/// 且不携带记录列表",平台能明确知道"这里没有你要的东西",
/// 而不是收到一棵它没请求的整树。
pub fn catalog_query_scope<'a>(
    tree: &'a [CatalogNode],
    query_id: &str,
    root_id: &str,
) -> (CatalogQueryScope, Vec<&'a CatalogNode>) {
    // b) 按系统/本设备编码查询:整棵树。
    if query_id == root_id || query_id.is_empty() {
        return (
            CatalogQueryScope::WholeTree,
            tree.iter().filter(|n| n.id != n.parent_id).collect(),
        );
    }

    // a) 按行政区域编码查询:CivilCode 前缀匹配。
    if is_civil_code(query_id) {
        let matched: Vec<&CatalogNode> = tree
            .iter()
            .filter(|n| n.id != n.parent_id)
            .filter(|n| {
                n.civil_code
                    .as_deref()
                    .is_some_and(|code| code.starts_with(query_id))
            })
            .collect();
        return (CatalogQueryScope::CivilCode, matched);
    }

    // c/d/e) 按设备/业务分组/虚拟组织编码查询:该节点 + 其后代。
    if tree.iter().any(|n| n.id == query_id) {
        let mut wanted: Vec<&CatalogNode> = Vec::new();
        let mut frontier = vec![query_id.to_string()];
        // 逐层展开,层数上限取节点数,避免配置成环时死循环。
        for _ in 0..tree.len().max(1) {
            if frontier.is_empty() {
                break;
            }
            let mut next = Vec::new();
            for id in &frontier {
                if let Some(node) = tree.iter().find(|n| &n.id == id) {
                    // 根节点自引用不作为目录项输出。
                    if node.id != node.parent_id && !wanted.iter().any(|w| w.id == node.id) {
                        wanted.push(node);
                    }
                }
                for child in tree.iter().filter(|n| &n.parent_id == id && n.id != *id) {
                    next.push(child.id.clone());
                }
            }
            frontier = next;
        }
        return (CatalogQueryScope::Subtree, wanted);
    }

    (CatalogQueryScope::Unknown, Vec::new())
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
        // 附录 E 表 E.1:业务分组 215、虚拟组织 216(平台设备段 200~299)。
        // 曾错用 137/138 —— 那两个是语音输出设备与移动传输设备。
        assert_eq!(CatalogNodeType::BusinessGroup.type_code(), "215");
        assert_eq!(CatalogNodeType::VirtualOrg.type_code(), "216");
        assert_eq!(
            gen_child_id("3402000000", CatalogNodeType::BusinessGroup, 1),
            "34020000002150000001"
        );
        assert_eq!(CatalogNodeType::VirtualOrg.parental(), 1);
        assert_eq!(CatalogNodeType::VideoChannel.parental(), 0);
    }

    #[test]
    fn 网络标识独立于序号() {
        // 第 14 位是网络标识,不是序号的高位。
        let id = gen_child_id_on_network("3402000000", CatalogNodeType::VideoChannel, 2, 7);
        assert_eq!(id, "34020000001322000007");
        let parts = IdParts::parse(&DeviceId::new(&id).unwrap()).unwrap();
        assert_eq!(parts.network_id, 2);
        assert_eq!(parts.serial, 7);
        assert_eq!(parts.type_code, "132");
        // 序号超 6 位时回绕,绝不改写网络标识那一位。
        let wrapped =
            gen_child_id_on_network("3402000000", CatalogNodeType::VideoChannel, 3, 1_000_001);
        assert_eq!(&wrapped[13..14], "3", "网络标识必须保持 3");
        assert_eq!(&wrapped[14..20], "000001");
        // 组装时超限直接报错,而不是静默截断。
        let over = IdParts {
            center: "34020000".into(),
            industry: "00".into(),
            type_code: "132".into(),
            network_id: 0,
            serial: 1_000_000,
        };
        assert!(over.to_id().is_err());
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
    fn 附录j_按系统编码查询返回整棵树() {
        let dom = "3402000000";
        let root = "34020000001110000001";
        let tree = catalog_template("nvr-8ch", root, "Cam", dom);
        let (scope, items) = catalog_query_scope(&tree, root, root);
        assert_eq!(scope, CatalogQueryScope::WholeTree);
        // 根节点自引用不作为目录项输出:9 = 1 业务分组 + 8 通道。
        assert_eq!(items.len(), 9);
        // 空 DeviceID 按整树处理(部分平台不填)。
        let (scope, items) = catalog_query_scope(&tree, "", root);
        assert_eq!(scope, CatalogQueryScope::WholeTree);
        assert_eq!(items.len(), 9);
    }

    #[test]
    fn 附录j_按业务分组查询只返回其下() {
        let dom = "3402000000";
        let root = "34020000001110000001";
        let tree = catalog_template("large-16ch", root, "Cam", dom);
        // 室内分组:自身 + 8 个通道;室外分组与挂在根上的报警通道都不该出现。
        let indoor = gen_child_id(dom, CatalogNodeType::BusinessGroup, 1);
        let (scope, items) = catalog_query_scope(&tree, &indoor, root);
        assert_eq!(scope, CatalogQueryScope::Subtree);
        assert_eq!(items.len(), 9, "业务分组自身 + 8 通道");
        assert!(items.iter().any(|n| n.id == indoor));
        assert!(
            items
                .iter()
                .all(|n| n.node_type != CatalogNodeType::AlarmChannel),
            "挂在根上的报警通道不属于这个分组"
        );
    }

    #[test]
    fn 附录j_按虚拟组织与设备编码查询() {
        let dom = "3402000000";
        let root = "34020000001110000001";
        let tree = catalog_template("civil-3x2", root, "Cam", dom);
        // e) 虚拟组织:自身 + 其下 2 个通道。
        let org = gen_child_id(dom, CatalogNodeType::VirtualOrg, 1);
        let (scope, items) = catalog_query_scope(&tree, &org, root);
        assert_eq!(scope, CatalogQueryScope::Subtree);
        assert_eq!(items.len(), 3);
        // c) 设备(叶子通道):只有自己。
        let channel = gen_child_id(dom, CatalogNodeType::VideoChannel, 1);
        let (scope, items) = catalog_query_scope(&tree, &channel, root);
        assert_eq!(scope, CatalogQueryScope::Subtree);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, channel);
    }

    #[test]
    fn 附录j_按行政区域编码前缀匹配() {
        let dom = "3402000000";
        let root = "34020000001110000001";
        let tree = catalog_template("civil-3x2", root, "Cam", dom);
        // 浦东 310115:该虚拟组织 + 其下 2 个通道都带这个 CivilCode。
        let (scope, items) = catalog_query_scope(&tree, "310115", root);
        assert_eq!(scope, CatalogQueryScope::CivilCode);
        assert_eq!(items.len(), 3);
        // 上海市 3101(4 位):三个区都命中 → 3 组织 + 6 通道。
        let (_, items) = catalog_query_scope(&tree, "3101", root);
        assert_eq!(items.len(), 9);
        // 不存在的区划返回空。
        let (_, items) = catalog_query_scope(&tree, "440300", root);
        assert!(items.is_empty());
    }

    #[test]
    fn 附录j_未知查询对象返回空集合() {
        let dom = "3402000000";
        let root = "34020000001110000001";
        let tree = catalog_template("single", root, "Cam", dom);
        let (scope, items) = catalog_query_scope(&tree, "34020000001329999999", root);
        assert_eq!(scope, CatalogQueryScope::Unknown);
        assert!(
            items.is_empty(),
            "查询对象不在目录里就该回空(配合附录 M 的 SumNum=0),而不是回整棵树"
        );
    }

    #[test]
    fn 行政区域编码识别位数() {
        assert!(is_civil_code("11"));
        assert!(is_civil_code("1101"));
        assert!(is_civil_code("110101"));
        assert!(is_civil_code("11010101"));
        assert!(!is_civil_code("110"), "3 位不是合法区划码");
        assert!(!is_civil_code("34020000001320000001"), "20 位是设备 ID");
        assert!(!is_civil_code("11a1"));
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
