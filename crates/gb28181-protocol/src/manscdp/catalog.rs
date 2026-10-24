use common::Result;
use serde::{Deserialize, Serialize};

use super::*;

/// 目录项(GB/T 28181-2022 附录 A.2.1.9 `itemType`)。
///
/// 字段顺序严格按标准的 `<sequence>` 排列。2016 与 2022 的字段集有差异
/// (2016 有 `Owner`/`SafetyWay`/证书四项,2022 有 `SecurityLevelCode`/`BusinessGroupID`),
/// 这里取两版的**并集**并按"2016 独有项插在 2022 顺序的对应位置"排布,
/// 于是同一个结构体在任一版本下按需置 None 后,输出顺序对两版都合法。
///
/// 含 f64 故不能 derive `Eq`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Item")]
pub struct CatalogItem {
    /// 通道国标 ID。
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    /// 通道名称。
    #[serde(rename = "Name")]
    pub name: String,
    /// 厂商(当为设备时必选)。
    #[serde(rename = "Manufacturer", skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// 型号(当为设备时必选)。
    #[serde(rename = "Model", skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 设备归属(**2016 必选**;2022 已删除该字段)。
    #[serde(rename = "Owner", skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// 行政区域,可为 2/4/6/8 位(必选)。
    #[serde(rename = "CivilCode", skip_serializing_if = "Option::is_none")]
    pub civil_code: Option<String>,
    /// 警区(可选)。
    #[serde(rename = "Block", skip_serializing_if = "Option::is_none")]
    pub block: Option<String>,
    /// 安装地址(当为设备时必选)。
    #[serde(rename = "Address", skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// 是否有子设备(必选):1-有,0-没有。
    #[serde(rename = "Parental", skip_serializing_if = "Option::is_none")]
    pub parental: Option<u8>,
    /// 父设备/区域/系统 ID(必选)。可多值,用半角 `/` 分隔。
    #[serde(rename = "ParentID", skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// 信令安全模式(**2016 可选**,缺省 0;2022 已删除)。
    #[serde(rename = "SafetyWay", skip_serializing_if = "Option::is_none")]
    pub safety_way: Option<u8>,
    /// 注册方式(必选,缺省 1):1-RFC 3261 认证注册;2-口令双向认证;3/4-数字证书(高安全级别)。
    #[serde(rename = "RegisterWay", skip_serializing_if = "Option::is_none")]
    pub register_way: Option<u8>,
    /// 证书序列号(**2016**,有证书的设备必选)。
    #[serde(rename = "CertNum", skip_serializing_if = "Option::is_none")]
    pub cert_num: Option<String>,
    /// 证书有效标识(**2016**,有证书的设备必选,缺省 0)。
    #[serde(rename = "Certifiable", skip_serializing_if = "Option::is_none")]
    pub certifiable: Option<u8>,
    /// 证书无效原因码(**2016**,有证书且证书无效时必选)。
    #[serde(rename = "ErrCode", skip_serializing_if = "Option::is_none")]
    pub err_code: Option<u32>,
    /// 证书终止有效期(**2016**,有证书的设备必选)。
    #[serde(rename = "EndTime", skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    /// 摄像机安全能力等级代码(**2022 新增**,可选):A/B/C 对应 GB 35114 三级。
    #[serde(rename = "SecurityLevelCode", skip_serializing_if = "Option::is_none")]
    pub security_level_code: Option<String>,
    /// 保密属性(必选,缺省 0):0-不涉密,1-涉密。
    #[serde(rename = "Secrecy", skip_serializing_if = "Option::is_none")]
    pub secrecy: Option<u8>,
    /// 设备 IPv4/IPv6 地址(可选)。
    #[serde(rename = "IPAddress", skip_serializing_if = "Option::is_none")]
    pub ip_address: Option<String>,
    /// 设备端口(可选)。
    #[serde(rename = "Port", skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    // 注:A.2.1.9 还定义了可选的 `Password`(设备口令)。**本实现永不输出** ——
    // 把设备口令写进目录应答等于把凭据广播给链路上的每一跳,收益为零。
    /// 设备状态(必选):ON / OFF。
    #[serde(rename = "Status")]
    pub status: String,
    /// 经度(WGS-84;2022 下一类、二类视频监控点必选)。
    #[serde(rename = "Longitude", skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    /// 纬度(WGS-84;2022 下一类、二类视频监控点必选)。
    #[serde(rename = "Latitude", skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    /// 虚拟组织所属的业务分组 ID(**2022 新增**,可选)。
    #[serde(rename = "BusinessGroupID", skip_serializing_if = "Option::is_none")]
    pub business_group_id: Option<String>,
    /// 摄像机扩展信息(可选)。
    #[serde(rename = "Info", skip_serializing_if = "Option::is_none")]
    pub info: Option<CatalogItemInfo>,
}

/// 目录项的 `Info` 扩展块(GB/T 28181-2022 附录 A.2.1.9)。
///
/// 字段顺序按标准的 `<sequence>` 排列 —— XSD 的 sequence 是**有序**的,
/// 顺序错了严格校验的平台会拒收整条目录应答。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename = "Info")]
pub struct CatalogItemInfo {
    /// 摄像机结构类型:1-球机;2-半球;3-固定枪机;4-遥控枪机;5-遥控半球;
    /// 6-多目设备的全景/拼接通道;7-多目设备的分割通道。
    #[serde(rename = "PTZType", skip_serializing_if = "Option::is_none")]
    pub ptz_type: Option<u8>,
    /// 光电成像类型:1-可见光;2-热成像;3-雷达;4-X光;5-深度光场;9-其他。可多值用 `/` 分隔。
    #[serde(
        rename = "PhotoelectricImagingType",
        skip_serializing_if = "Option::is_none"
    )]
    pub photoelectric_imaging_type: Option<String>,
    /// 采集部位类型,取值见附录 O(7 位层次码)。
    #[serde(
        rename = "CapturePositionType",
        skip_serializing_if = "Option::is_none"
    )]
    pub capture_position_type: Option<String>,
    /// 安装位置室外/室内:1-室外;2-室内(缺省 1)。
    #[serde(rename = "RoomType", skip_serializing_if = "Option::is_none")]
    pub room_type: Option<u8>,
    /// 补光属性:1-无补光;2-红外;3-白光;4-激光;9-其他(缺省 1)。
    #[serde(rename = "SupplyLightType", skip_serializing_if = "Option::is_none")]
    pub supply_light_type: Option<u8>,
    /// 监视方位(光轴方向):1-东…8-西北。固定摄像机或设了看守位的摄像机可选。
    #[serde(rename = "DirectionType", skip_serializing_if = "Option::is_none")]
    pub direction_type: Option<u8>,
    /// 支持的分辨率,取值见附录 G 的 `f=` 规定。可多值用 `/` 分隔。
    #[serde(rename = "Resolution", skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// 支持的码流编号列表,如 `0/1/2`(0=主码流)。
    #[serde(rename = "StreamNumberList", skip_serializing_if = "Option::is_none")]
    pub stream_number_list: Option<String>,
    /// 支持的下载倍速,如 `1/2/4`。
    #[serde(rename = "DownloadSpeed", skip_serializing_if = "Option::is_none")]
    pub download_speed: Option<String>,
    /// 空域编码能力:0-不支持;1/2/3-增强层数。
    #[serde(
        rename = "SVCSpaceSupportMode",
        skip_serializing_if = "Option::is_none"
    )]
    pub svc_space_support_mode: Option<u8>,
    /// 时域编码能力:0-不支持;1/2/3-增强层数。
    #[serde(rename = "SVCTimeSupportMode", skip_serializing_if = "Option::is_none")]
    pub svc_time_support_mode: Option<u8>,
    /// SSVC 增强层与基本层比例能力,如 `4:3/2:1`。
    #[serde(
        rename = "SSVCRatioSupportList",
        skip_serializing_if = "Option::is_none"
    )]
    pub ssvc_ratio_support_list: Option<String>,
    /// 移动采集设备类型(仅移动采集设备适用):1-机器人载;2-执法记录仪;3-单兵;
    /// 4-车载;5-无人机载;9-其他。
    #[serde(rename = "MobileDeviceType", skip_serializing_if = "Option::is_none")]
    pub mobile_device_type: Option<u8>,
    /// 水平视场角(度,0 < x ≤ 360)。
    #[serde(
        rename = "HorizontalFieldAngle",
        skip_serializing_if = "Option::is_none"
    )]
    pub horizontal_field_angle: Option<f64>,
    /// 竖直视场角(度,0 < x ≤ 360)。
    #[serde(rename = "VerticalFieldAngle", skip_serializing_if = "Option::is_none")]
    pub vertical_field_angle: Option<f64>,
    /// 可视距离(米)。
    #[serde(rename = "MaxViewDistance", skip_serializing_if = "Option::is_none")]
    pub max_view_distance: Option<f64>,
    /// 基层组织编码(**必选**,非基层建设时为 `000000`),规则见附录 E.3。
    #[serde(rename = "GrassrootsCode", skip_serializing_if = "Option::is_none")]
    pub grassroots_code: Option<String>,
    /// 监控点位类型(当为摄像机时必选):1-一类;2-二类;3-三类;9-其他。
    #[serde(rename = "PointType", skip_serializing_if = "Option::is_none")]
    pub point_type: Option<u8>,
    /// 点位俗称。
    #[serde(rename = "PointCommonName", skip_serializing_if = "Option::is_none")]
    pub point_common_name: Option<String>,
    /// 设备 MAC 地址,`XX-XX-XX-XX-XX-XX` 格式。
    #[serde(rename = "MAC", skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    /// 卡口功能类型:01-人脸;02-人员;03-机动车;04-非机动车;05-物品;99-其他。可多值。
    #[serde(rename = "FunctionType", skip_serializing_if = "Option::is_none")]
    pub function_type: Option<String>,
    /// 视频编码格式,取值见附录 G 的 `f=` 规定(2-H.264、5-H.265)。
    #[serde(rename = "EncodeType", skip_serializing_if = "Option::is_none")]
    pub encode_type: Option<String>,
    /// 安装使用时间(一类视频监控点必选)。
    #[serde(rename = "InstallTime", skip_serializing_if = "Option::is_none")]
    pub install_time: Option<String>,
    /// 所属管理单位名称。
    #[serde(rename = "ManagementUnit", skip_serializing_if = "Option::is_none")]
    pub management_unit: Option<String>,
    /// 管理单位联系方式(可多值用 `/` 分隔;一类视频监控点必填)。
    #[serde(rename = "ContactInfo", skip_serializing_if = "Option::is_none")]
    pub contact_info: Option<String>,
    /// 录像保存天数(一类视频监控点必填)。
    #[serde(rename = "RecordSaveDays", skip_serializing_if = "Option::is_none")]
    pub record_save_days: Option<u32>,
    /// 国民经济行业分类代码(GB/T 4754 第 5 章)。
    #[serde(
        rename = "IndustrialClassification",
        skip_serializing_if = "Option::is_none"
    )]
    pub industrial_classification: Option<String>,
}

/// 附录 M:每条响应/通知消息携带的记录条数上限。
///
/// 超过这个量级标准建议改用 TCP 传输(见附录 M 与 §附录 D)。
pub const MAX_ITEMS_PER_MESSAGE: usize = 10_000;

/// 附录 E 表 E.1 的注册方式取值:符合 IETF RFC 3261 的认证注册模式。
pub const REGISTER_WAY_RFC3261: u8 = 1;
/// 附录 A.2.1.9 保密属性:不涉密。
pub const SECRECY_PUBLIC: u8 = 0;
/// 附录 A.2.1.9 基层组织编码:非基层建设时的固定取值。
pub const GRASSROOTS_CODE_NONE: &str = "000000";

/// 附录 O 的摄像机采集部位类型代码(7 位层次码:大类 3 + 中类 2 + 小类 2)。
///
/// 只收录本模拟器默认会用到的几类;完整表见标准附录 O 表 O.1。小类默认 `00`。
pub mod capture_position {
    /// 001 公共活动露天广场 / 00103 商业服务露天广场。
    pub const COMMERCIAL_SQUARE: &str = "0010300";
    /// 002 通行区域 / 00201 城市主要路段、路口、立交桥等。
    pub const CITY_ROAD: &str = "0020100";
    /// 003 出入口卡口 / 00301 高速公路主要出入口、卡口、检查站等。
    pub const HIGHWAY_CHECKPOINT: &str = "0030100";
    /// 004 大型桥梁隧道 / 00401 大型桥梁主要通行区域。
    pub const BRIDGE: &str = "0040100";

    /// 校验是否为合法的 7 位采集部位类型代码。
    ///
    /// 只校验结构(7 位数字、大类与中类顺序码不为全 0),不内置整张表 ——
    /// 表 O.1 会随行业规定扩充,把它写死在代码里反而会拒掉合法的新代码。
    pub fn is_valid(code: &str) -> bool {
        code.len() == 7
            && code.bytes().all(|b| b.is_ascii_digit())
            && &code[0..3] != "000"
            && &code[3..5] != "00"
    }
}

impl CatalogItem {
    /// 构造一个**目录节点项**(区域/业务分组/虚拟组织):按附录 J 只填 ID 与名称等少量字段。
    ///
    /// 附录 J 明确"行政区划、系统、业务分组、虚拟组织目录项各字段应按照示例固定填写",
    /// 所以这些节点不该带 Manufacturer/Model/Address 这些设备专属字段。
    pub fn folder(device_id: impl Into<String>, name: impl Into<String>) -> Self {
        CatalogItem {
            device_id: device_id.into(),
            name: name.into(),
            manufacturer: None,
            model: None,
            owner: None,
            civil_code: None,
            block: None,
            address: None,
            parental: Some(1),
            parent_id: None,
            safety_way: None,
            register_way: None,
            cert_num: None,
            certifiable: None,
            err_code: None,
            end_time: None,
            security_level_code: None,
            secrecy: None,
            ip_address: None,
            port: None,
            status: "ON".into(),
            longitude: None,
            latitude: None,
            business_group_id: None,
            info: None,
        }
    }

    /// 构造一个**设备/通道目录项**,补齐 A.2.1.9 的全部必选字段。
    ///
    /// 必选字段给默认值而不是留空:严格校验 schema 的平台会因为缺 `RegisterWay`/`Secrecy`
    /// 这类必选元素拒收整条应答,现象是"平台一直同步不到目录"却看不出原因。
    pub fn device(
        device_id: impl Into<String>,
        name: impl Into<String>,
        manufacturer: impl Into<String>,
        model: impl Into<String>,
        parent_id: impl Into<String>,
        status: impl Into<String>,
    ) -> Self {
        CatalogItem {
            device_id: device_id.into(),
            name: name.into(),
            manufacturer: Some(manufacturer.into()),
            model: Some(model.into()),
            owner: None,
            civil_code: None,
            block: None,
            address: Some("Simulated".into()),
            parental: Some(0),
            parent_id: Some(parent_id.into()),
            safety_way: None,
            register_way: Some(REGISTER_WAY_RFC3261),
            cert_num: None,
            certifiable: None,
            err_code: None,
            end_time: None,
            security_level_code: None,
            secrecy: Some(SECRECY_PUBLIC),
            ip_address: None,
            port: None,
            status: status.into(),
            longitude: None,
            latitude: None,
            business_group_id: None,
            info: None,
        }
    }

    /// 链式设置父节点 ID。
    pub fn with_parent(mut self, parent_id: impl Into<String>) -> Self {
        self.parent_id = Some(parent_id.into());
        self
    }

    /// 链式设置行政区划。
    pub fn with_civil_code(mut self, civil_code: Option<String>) -> Self {
        self.civil_code = civil_code;
        self
    }

    /// 链式设置安装地址。
    pub fn with_address(mut self, address: impl Into<String>) -> Self {
        self.address = Some(address.into());
        self
    }

    /// 链式设置是否有子设备。
    pub fn with_parental(mut self, parental: u8) -> Self {
        self.parental = Some(parental);
        self
    }

    /// 链式设置经纬度(WGS-84)。2022 下一类/二类监控点必选。
    pub fn with_position(mut self, longitude: f64, latitude: f64) -> Self {
        self.longitude = Some(longitude);
        self.latitude = Some(latitude);
        self
    }

    /// 链式设置本端地址(可选字段 IPAddress/Port)。
    pub fn with_endpoint(mut self, ip: impl Into<String>, port: u16) -> Self {
        self.ip_address = Some(ip.into());
        self.port = Some(port);
        self
    }

    /// 链式设置 `Info` 扩展块。
    pub fn with_info(mut self, info: CatalogItemInfo) -> Self {
        self.info = Some(info);
        self
    }

    /// 应用 **2016 版**独有的必选/可选字段(`Owner` 必选)。
    pub fn with_gb2016_fields(mut self, owner: impl Into<String>) -> Self {
        self.owner = Some(owner.into());
        self.safety_way = Some(0); // 缺省 0:不采用信令安全模式
        self
    }

    /// 应用 **2022 版**独有字段(安全能力等级)。
    pub fn with_gb2022_fields(mut self, security_level_code: impl Into<String>) -> Self {
        self.security_level_code = Some(security_level_code.into());
        self
    }
}

impl CatalogItemInfo {
    /// 一台**摄像机通道**的默认扩展信息。
    ///
    /// `GrassrootsCode` 与 `PointType` 在标准里是必选(当为摄像机时),所以必须有值;
    /// 其余按模拟器的真实能力填 —— 比如 `DownloadSpeed` 写 `1/2/4` 是因为下载会话
    /// 真的支持这三档倍速,不是随手编的。
    pub fn camera(resolution: impl Into<String>, encode_type: impl Into<String>) -> Self {
        CatalogItemInfo {
            ptz_type: Some(1), // 球机:本模拟器有完整 PTZ 与预置位仿真
            photoelectric_imaging_type: Some("1".into()), // 可见光
            capture_position_type: Some(capture_position::CITY_ROAD.into()),
            room_type: Some(1),         // 室外
            supply_light_type: Some(2), // 红外补光
            direction_type: None,       // 球机方位随 PTZ 变,不声明固定方位
            resolution: Some(resolution.into()),
            stream_number_list: Some("0".into()), // 只有主码流
            download_speed: Some("1/2/4".into()),
            svc_space_support_mode: Some(0),
            svc_time_support_mode: Some(0),
            ssvc_ratio_support_list: None,
            mobile_device_type: None, // 非移动采集设备
            horizontal_field_angle: None,
            vertical_field_angle: None,
            max_view_distance: None,
            grassroots_code: Some(GRASSROOTS_CODE_NONE.into()),
            point_type: Some(2), // 二类视频监控点(带经纬度)
            point_common_name: None,
            mac: None,
            function_type: None,
            encode_type: Some(encode_type.into()),
            install_time: None,
            management_unit: None,
            contact_info: None,
            record_save_days: None,
            industrial_classification: None,
        }
    }

    /// 只保留 **2016** 版 `Info` 里确定存在的字段。
    ///
    /// 2016 的 `Info` 远小于 2022(2022 新增了采集部位/补光/视场角/点位类型等一大批),
    /// 把 2022 的字段原样发给 2016 平台会带来一堆它不认识的元素。
    /// 这里保守只留 `PTZType` —— 那是 2016 原文里能确认的项。
    pub fn to_gb2016(&self) -> Self {
        CatalogItemInfo {
            ptz_type: self.ptz_type,
            ..Default::default()
        }
    }
}

/// 目录查询应答(设备 → 平台)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Response")]
pub struct CatalogResponse {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    /// 设备(根)ID。
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    /// 通道总数。
    #[serde(rename = "SumNum")]
    pub sum_num: u32,
    /// 通道列表容器。
    ///
    /// **附录 M**:未查询出结果时 `SumNum` 取 0 **且不携带记录列表**,
    /// 所以这里是 `Option` —— 空结果时整个 `<DeviceList>` 元素都不输出,
    /// 而不是输出一个 `<DeviceList Num="0"/>`。
    #[serde(rename = "DeviceList", skip_serializing_if = "Option::is_none")]
    pub device_list: Option<DeviceList>,
}

/// 目录通道列表容器(带 Num 属性)。含经纬度(f64)故不能 derive `Eq`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceList {
    /// 本次携带的通道数。
    #[serde(rename = "@Num")]
    pub num: u32,
    /// 通道项。
    #[serde(rename = "Item", default)]
    pub items: Vec<CatalogItem>,
}

impl CatalogResponse {
    /// 用设备 ID、SN、通道列表构造应答。
    pub fn new(device_id: impl Into<String>, sn: u32, items: Vec<CatalogItem>) -> Self {
        let num = items.len() as u32;
        CatalogResponse {
            cmd_type: "Catalog".into(),
            sn,
            device_id: device_id.into(),
            sum_num: num,
            device_list: (!items.is_empty()).then_some(DeviceList { num, items }),
        }
    }

    /// 把目录拆成多条应答(GB/T 28181 §A.2.6.4:`SumNum` 为总数,每条的 `Num`
    /// 为本条携带的条目数)。
    ///
    /// 为什么必须分片:单条 UDP MESSAGE 装整棵 NVR 目录树很容易超过 MTU,
    /// 被中间设备静默截断,现象是"平台一直同步不到目录"。
    /// `per_message` 为每条最多携带的条目数(0 视为 1)。
    pub fn split(
        device_id: impl Into<String>,
        sn: u32,
        items: Vec<CatalogItem>,
        per_message: usize,
    ) -> Vec<Self> {
        let device_id = device_id.into();
        let sum_num = items.len() as u32;
        // 附录 M:每条响应携带记录上限 10000 条。
        let chunk = per_message.clamp(1, MAX_ITEMS_PER_MESSAGE);
        if items.is_empty() {
            // 附录 M:未查询出结果时 SumNum 取 0 且不携带记录列表。
            return vec![CatalogResponse {
                cmd_type: "Catalog".into(),
                sn,
                device_id,
                sum_num: 0,
                device_list: None,
            }];
        }
        items
            .chunks(chunk)
            .map(|part| CatalogResponse {
                cmd_type: "Catalog".into(),
                sn,
                device_id: device_id.clone(),
                sum_num,
                device_list: Some(DeviceList {
                    num: part.len() as u32,
                    items: part.to_vec(),
                }),
            })
            .collect()
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
    }
}

/// 目录订阅通知项:在目录项基础上多一个 `Event` 字段(ON/OFF/ADD/DEL/UPDATE)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Item")]
pub struct CatalogNotifyItem {
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "Name")]
    pub name: String,
    /// 变更事件:ON(上线)/OFF(离线)/ADD/DEL/UPDATE。
    #[serde(rename = "Event")]
    pub event: String,
    #[serde(rename = "Status")]
    pub status: String,
}

/// 目录订阅通知列表容器。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogNotifyList {
    #[serde(rename = "@Num")]
    pub num: u32,
    #[serde(rename = "Item", default)]
    pub items: Vec<CatalogNotifyItem>,
}

/// 目录订阅变更通知(设备 → 平台,`<Notify>` 根元素)。
///
/// 平台 `SUBSCRIBE + Catalog` 后,设备用本通知上报目录项的上线/离线/增删。
/// 与 [`CatalogResponse`] 的区别:根元素是 `Notify`(非 `Response`),每项带 `Event`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Notify")]
pub struct CatalogNotify {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "SumNum")]
    pub sum_num: u32,
    #[serde(rename = "DeviceList")]
    pub device_list: CatalogNotifyList,
}

impl CatalogNotify {
    /// 用设备 ID、SN、变更项列表构造目录通知。
    pub fn new(device_id: impl Into<String>, sn: u32, items: Vec<CatalogNotifyItem>) -> Self {
        let num = items.len() as u32;
        CatalogNotify {
            cmd_type: "Catalog".into(),
            sn,
            device_id: device_id.into(),
            sum_num: num,
            device_list: CatalogNotifyList { num, items },
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
    }
}

/// 目录通道快照(用于增量 NOTIFY diff)。仅保留 diff 所需字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSnapshot {
    /// 通道 ID → (名称, 状态)。
    pub channels: std::collections::BTreeMap<String, (String, String)>,
}

impl CatalogSnapshot {
    /// 从 (id, name, status) 三元组构造快照。
    pub fn from_items<I, S>(items: I) -> Self
    where
        I: IntoIterator<Item = (S, S, S)>,
        S: Into<String>,
    {
        let channels = items
            .into_iter()
            .map(|(id, name, status)| (id.into(), (name.into(), status.into())))
            .collect();
        CatalogSnapshot { channels }
    }

    /// 计算从 `self`(旧)到 `next`(新)的目录变更项。
    ///
    /// - 新增通道 → Event=ADD
    /// - 删除通道 → Event=DEL
    /// - 名称变化 → Event=UPDATE
    /// - 仅状态变化 → Event=ON(变 ON)/ OFF(变非 ON)
    ///
    /// 无变化返回空 Vec(调用方据此决定不发 NOTIFY)。
    pub fn diff(&self, next: &CatalogSnapshot) -> Vec<CatalogNotifyItem> {
        let mut items = Vec::new();
        // 新增 / 变更。
        for (id, (name, status)) in &next.channels {
            match self.channels.get(id) {
                None => items.push(CatalogNotifyItem {
                    device_id: id.clone(),
                    name: name.clone(),
                    event: "ADD".into(),
                    status: status.clone(),
                }),
                Some((old_name, old_status)) => {
                    if old_name != name {
                        items.push(CatalogNotifyItem {
                            device_id: id.clone(),
                            name: name.clone(),
                            event: "UPDATE".into(),
                            status: status.clone(),
                        });
                    } else if old_status != status {
                        let on = status.eq_ignore_ascii_case("ON");
                        items.push(CatalogNotifyItem {
                            device_id: id.clone(),
                            name: name.clone(),
                            event: if on { "ON" } else { "OFF" }.into(),
                            status: status.clone(),
                        });
                    }
                }
            }
        }
        // 删除。
        for (id, (name, _)) in &self.channels {
            if !next.channels.contains_key(id) {
                items.push(CatalogNotifyItem {
                    device_id: id.clone(),
                    name: name.clone(),
                    event: "DEL".into(),
                    status: "OFF".into(),
                });
            }
        }
        items
    }
}

// ─────────────────────────────────────────────────────────────────────────
// FR-17 扩展查询应答(M6·P1)。骨架据上游 uvp-gb28181-sim 真实实现核对,
// 均以独立 MESSAGE 发回 `<Response>`(与 Catalog/ConfigDownload 同路径)。
// ─────────────────────────────────────────────────────────────────────────
