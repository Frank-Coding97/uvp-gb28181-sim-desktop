use ipc_error::{IpcError, IpcResult};

use std::sync::Arc;

use common::DeviceId;

use super::*;

/// 目录节点 DTO(前端多通道管理 ⇄ 引擎 CatalogNode)。
#[derive(ts_rs::TS)]
#[ts(export, export_to = "../../src/types/generated/")]
#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub(crate) struct ChannelNodeDto {
    id: String,
    /// 类型:Device/BusinessGroup/VirtualOrg/VideoChannel/AlarmChannel。
    node_type: String,
    name: String,
    parent_id: String,
    #[serde(default)]
    civil_code: Option<String>,
    #[serde(default = "default_on")]
    status: String,
    // ── 以下是 A.2.1.9 的逐通道可选字段(留空即用设备级默认值)──
    /// 安装地址(`Address`)。一台 NVR 下各路摄像机装在不同位置,
    /// 全报同一个地址会让平台的地图与清单失去意义。
    #[serde(default)]
    address: Option<String>,
    /// 经度(`Longitude`)。留空取设备当前位置。
    #[serde(default)]
    longitude: Option<f64>,
    /// 纬度(`Latitude`)。留空取设备当前位置。
    #[serde(default)]
    latitude: Option<f64>,
    /// 摄像机类型(`Info/PTZType`):1 球机 / 2 半球 / 3 固定枪机 / 4 遥控枪机。
    /// 平台按它决定是否给这一路显示云台面板。
    #[serde(default)]
    ptz_type: Option<u8>,
    /// 采集部位类型(`Info/CapturePositionType`,附录 O 的 7 位层次码)。
    #[serde(default)]
    capture_position_type: Option<String>,
}

pub(crate) fn default_on() -> String {
    "ON".into()
}

/// 解析节点类型。**未知值报错而不是静默落到 VideoChannel** —— 静默兜底会让
/// 前端拼错的类型带着错误的 Parental 值被 NOTIFY 给平台。
pub(crate) fn node_type_from_str(
    s: &str,
) -> Result<gb28181_protocol::id_codec::CatalogNodeType, String> {
    use gb28181_protocol::id_codec::CatalogNodeType::*;
    Ok(match s {
        "BusinessGroup" => BusinessGroup,
        "VirtualOrg" => VirtualOrg,
        "AlarmChannel" => AlarmChannel,
        "Device" => Device,
        "VideoChannel" => VideoChannel,
        other => return Err(format!("未知的目录节点类型: {other}")),
    })
}

/// 校验目录节点:ID 必须是合法国标编号,类型必须已知,状态只能 ON/OFF。
///
/// 之前 `id`/`parent_id` 是裸 String、完全不过 `DeviceId::new`,非法编号会直接
/// 进目录树并被 Catalog NOTIFY 发给平台。
pub(crate) fn validate_channel_node(
    dto: ChannelNodeDto,
) -> Result<gb28181_protocol::id_codec::CatalogNode, String> {
    let id = DeviceId::new(dto.id.trim().to_string()).map_err(|e| format!("通道 ID 非法: {e}"))?;
    let parent = dto.parent_id.trim();
    if parent.is_empty() {
        return Err("父节点 ID 不能为空".into());
    }
    DeviceId::new(parent.to_string()).map_err(|e| format!("父节点 ID 非法: {e}"))?;
    let node_type = node_type_from_str(dto.node_type.trim())?;
    let name = dto.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err("通道名称长度必须在 1..=64 个字符之间".into());
    }
    let status = dto.status.trim().to_uppercase();
    if status != "ON" && status != "OFF" {
        return Err("通道状态只能是 ON 或 OFF".into());
    }
    if let Some(code) = dto.civil_code.as_deref() {
        let code = code.trim();
        let looks_valid =
            code.is_empty() || (code.len() <= 8 && code.bytes().all(|b| b.is_ascii_digit()));
        if !looks_valid {
            return Err("行政区划编码必须是 1..=8 位数字".into());
        }
    }

    let mut node = gb28181_protocol::id_codec::CatalogNode::new(
        id.as_str().to_string(),
        node_type,
        name.to_string(),
        parent.to_string(),
    );
    node.civil_code = dto
        .civil_code
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty());
    node.status = status;

    // 逐通道扩展字段:空串等于"不填",不能写成 Some("")——
    // 那会让目录项里出现一个空的 <Address/>,平台侧显示成空白而不是默认值。
    node.address = dto
        .address
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty());
    if let Some(lon) = dto.longitude {
        if !(-180.0..=180.0).contains(&lon) {
            return Err("经度必须在 -180..=180 之间".into());
        }
    }
    if let Some(lat) = dto.latitude {
        if !(-90.0..=90.0).contains(&lat) {
            return Err("纬度必须在 -90..=90 之间".into());
        }
    }
    // 经纬度要么都填要么都不填:只填一个会让平台把另一半当 0 度,
    // 于是这一路通道被钉在几内亚湾。
    match (dto.longitude, dto.latitude) {
        (Some(lon), Some(lat)) => {
            node.longitude = Some(lon);
            node.latitude = Some(lat);
        }
        (None, None) => {}
        _ => return Err("经度与纬度必须同时填写或同时留空".into()),
    }
    if let Some(ptz) = dto.ptz_type {
        if !(1..=4).contains(&ptz) {
            return Err("摄像机类型只能是 1(球机)/2(半球)/3(固定枪机)/4(遥控枪机)".into());
        }
        node.ptz_type = Some(ptz);
    }
    if let Some(code) = dto.capture_position_type.as_deref() {
        let code = code.trim();
        if !code.is_empty() {
            if !gb28181_protocol::manscdp::capture_position::is_valid(code) {
                return Err(
                    "采集部位类型必须是附录 O 的 7 位数字层次码,如 0020100(城市道路)".into(),
                );
            }
            node.capture_position_type = Some(code.to_string());
        }
    }
    Ok(node)
}

pub(crate) fn node_type_to_str(t: gb28181_protocol::id_codec::CatalogNodeType) -> &'static str {
    use gb28181_protocol::id_codec::CatalogNodeType::*;
    match t {
        Device => "Device",
        BusinessGroup => "BusinessGroup",
        VirtualOrg => "VirtualOrg",
        VideoChannel => "VideoChannel",
        AlarmChannel => "AlarmChannel",
    }
}

impl From<&gb28181_protocol::id_codec::CatalogNode> for ChannelNodeDto {
    fn from(n: &gb28181_protocol::id_codec::CatalogNode) -> Self {
        ChannelNodeDto {
            id: n.id.clone(),
            node_type: node_type_to_str(n.node_type).into(),
            name: n.name.clone(),
            parent_id: n.parent_id.clone(),
            civil_code: n.civil_code.clone(),
            status: n.status.clone(),
            address: n.address.clone(),
            longitude: n.longitude,
            latitude: n.latitude,
            ptz_type: n.ptz_type,
            capture_position_type: n.capture_position_type.clone(),
        }
    }
}

/// 载入内置目录模板(single/nvr-8ch/civil-3x2/large-16ch),返回加载后的节点列表(FR-34)。
#[tauri::command]
pub(crate) async fn load_catalog_template(
    template: String,
    state: tauri::State<'_, AppState>,
) -> IpcResult<Vec<ChannelNodeDto>> {
    const TEMPLATES: [&str; 4] = ["single", "nvr-8ch", "civil-3x2", "large-16ch"];
    if !TEMPLATES.contains(&template.as_str()) {
        return Err(IpcError::config(format!(
            "未知的目录模板 {template},可选:{}",
            TEMPLATES.join(" / ")
        )));
    }
    let guard = state.device.lock().await;
    let h = guard
        .as_ref()
        .ok_or_else(|| IpcError::device_not_running("设备未启动,请先在单设备页启动设备"))?;
    h.sim.load_catalog_template(&template);
    // 增量 NOTIFY 推送是副作用,后台发送不阻塞 UI 返回(平台不及时回 200 时事务会退避重传数秒)。
    spawn_catalog_notify(h);
    Ok(h.sim.catalog_tree().iter().map(Into::into).collect())
}

/// 获取当前目录树节点(FR-34)。
#[tauri::command]
pub(crate) async fn get_catalog_tree(
    state: tauri::State<'_, AppState>,
) -> IpcResult<Vec<ChannelNodeDto>> {
    let guard = state.device.lock().await;
    let h = guard
        .as_ref()
        .ok_or_else(|| IpcError::device_not_running("设备未启动"))?;
    Ok(h.sim.catalog_tree().iter().map(Into::into).collect())
}

/// 新增/更新一个目录通道节点,触发增量 NOTIFY(FR-34)。
#[tauri::command]
pub(crate) async fn upsert_channel(
    node: ChannelNodeDto,
    state: tauri::State<'_, AppState>,
) -> IpcResult<String> {
    let node = validate_channel_node(node)?;
    let guard = state.device.lock().await;
    let h = guard
        .as_ref()
        .ok_or_else(|| IpcError::device_not_running("设备未启动"))?;
    let added = h.sim.upsert_channel(node);
    spawn_catalog_notify(h);
    Ok(if added {
        "已新增通道".into()
    } else {
        "已更新通道".into()
    })
}

/// 删除一个目录通道节点,触发增量 NOTIFY(FR-34)。
#[tauri::command]
pub(crate) async fn remove_channel(
    id: String,
    state: tauri::State<'_, AppState>,
) -> IpcResult<String> {
    let guard = state.device.lock().await;
    let h = guard
        .as_ref()
        .ok_or_else(|| IpcError::device_not_running("设备未启动"))?;
    if h.sim.remove_channel(&id) {
        spawn_catalog_notify(h);
        Ok("已删除通道".into())
    } else {
        Err("通道不存在".into())
    }
}

/// 后台推送目录增量 NOTIFY(fire-and-forget)。
///
/// NOTIFY 是通知平台的副作用,不该阻塞 IPC 返回:目录订阅存在但平台不及时回 200 时,
/// SIP 客户端事务会按 T1 退避重传,最坏阻塞约 4 秒。放后台 spawn 让 UI 立即拿到目录树。
pub(crate) fn spawn_catalog_notify(h: &DeviceHandle) {
    let sim = Arc::clone(&h.sim);
    let transport = Arc::clone(&h.transport);
    tokio::spawn(async move {
        if let Err(e) = sim.notify_catalog_changed(&transport).await {
            tracing::warn!(error = %e, "目录增量 NOTIFY 推送失败");
        }
    });
}
