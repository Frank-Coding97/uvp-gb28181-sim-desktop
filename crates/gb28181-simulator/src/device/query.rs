use common::{Error, Result};

use super::*;

impl DeviceSimulator {
    /// 载入一个内置目录模板(single/nvr-8ch/civil-3x2/large-16ch),替换现有目录树(FR-34)。
    pub fn load_catalog_template(&self, template: &str) {
        let tree = gb28181_protocol::id_codec::catalog_template(
            template,
            self.config.device_id.as_str(),
            &self.config.device_info.device_name,
            &self.config.server_domain,
        );
        *self.catalog_tree.lock() = tree;
    }

    /// 直接设置目录树节点(FR-34)。首节点应为根设备。
    pub fn set_catalog_tree(&self, nodes: Vec<gb28181_protocol::id_codec::CatalogNode>) {
        *self.catalog_tree.lock() = nodes;
    }

    /// 当前目录树快照(空表示用 config.channels 扁平列表)。
    pub fn catalog_tree(&self) -> Vec<gb28181_protocol::id_codec::CatalogNode> {
        self.catalog_tree.lock().clone()
    }

    /// 新增/更新一个目录通道节点(按 id 覆盖)。返回是否为新增。
    pub fn upsert_channel(&self, node: gb28181_protocol::id_codec::CatalogNode) -> bool {
        let mut tree = self.catalog_tree.lock();
        if let Some(existing) = tree.iter_mut().find(|n| n.id == node.id) {
            *existing = node;
            false
        } else {
            tree.push(node);
            true
        }
    }

    /// 删除一个目录通道节点(按 id)。返回是否删除了节点。
    pub fn remove_channel(&self, id: &str) -> bool {
        let mut tree = self.catalog_tree.lock();
        let before = tree.len();
        tree.retain(|n| n.id != id);
        tree.len() != before
    }

    /// 目录节点(树非空时)或扁平通道(树空时)映射为 (id, name, status) 快照三元组,
    /// 供目录应答与增量 diff 复用。根设备节点(parent==self)不作为通道项。
    pub(crate) fn catalog_publishable(&self) -> Vec<(String, String, String)> {
        let tree = self.catalog_tree.lock();
        if tree.is_empty() {
            self.config
                .channels
                .iter()
                .map(|c| {
                    (
                        c.channel_id.as_str().to_string(),
                        c.name.clone(),
                        c.status.clone(),
                    )
                })
                .collect()
        } else {
            tree.iter()
                .filter(|n| n.id != n.parent_id) // 过滤根设备自引用
                .map(|n| (n.id.clone(), n.name.clone(), n.status.clone()))
                .collect()
        }
    }

    /// 查询命令的人类可读摘要(供 UI 命令时间线)。
    pub(crate) fn query_summary(&self, cmd_type: &str) -> String {
        let ch_count = {
            let tree = self.catalog_tree.lock();
            if tree.is_empty() {
                self.config.channels.len()
            } else {
                tree.iter()
                    .filter(|n| {
                        n.node_type == gb28181_protocol::id_codec::CatalogNodeType::VideoChannel
                    })
                    .count()
            }
        };
        match cmd_type {
            "Catalog" => format!("查询目录 → 回 {ch_count} 通道"),
            "DeviceInfo" => "查询设备信息 → 已应答".into(),
            "DeviceStatus" => "查询设备状态 → 在线".into(),
            "RecordInfo" => "检索录像 → 回录像列表".into(),
            "ConfigDownload" => "查询设备配置 → 已应答".into(),
            "PresetQuery" => "查询预置位 → 回预置位列表".into(),
            "HomePositionQuery" => "查询看守位 → 已应答".into(),
            "SDCardStatus" => "查询存储卡状态 → 已应答".into(),
            "CruiseTrackListQuery" => "查询巡航轨迹列表 → 已应答".into(),
            "CruiseTrackQuery" => "查询巡航轨迹 → 已应答".into(),
            "PTZPosition" => "查询 PTZ 精准状态 → 已应答".into(),
            other => format!("查询 {other} → 已应答"),
        }
    }

    /// 目录项列表(附录 A.2.1.9 + 附录 J)。
    ///
    /// 两条路径:配置里的扁平通道(单通道兼容)、或多通道目录树(FR-34)。
    /// 必选字段一律补齐 —— 缺 `RegisterWay`/`Secrecy`/`Address` 这类必选元素时,
    /// 严格校验 schema 的平台会拒收整条应答,现象是"平台一直同步不到目录"。
    pub(crate) fn catalog_items(&self) -> Vec<gb28181_protocol::manscdp::CatalogItem> {
        // 不带查询对象时等价于"按本设备编码查询"(附录 J b:整棵树)。
        self.catalog_items_for(self.config.device_id.as_str())
    }

    /// 按**附录 J** 的返回范围规则生成目录项。
    ///
    /// `query_id` 是平台 `<Query>` 里的 `DeviceID`:可以是本设备编码(整树)、
    /// 行政区域编码(前缀匹配)、业务分组/虚拟组织/通道编码(该节点及其后代)。
    pub(crate) fn catalog_items_for(
        &self,
        query_id: &str,
    ) -> Vec<gb28181_protocol::manscdp::CatalogItem> {
        use gb28181_protocol::id_codec::CatalogNodeType;
        use gb28181_protocol::manscdp::{CatalogItem, CatalogItemInfo};

        let is_2022 = self.config.gb_version.is_2022();
        let (longitude, latitude) = *self.position.lock();
        let manufacturer = self.config.device_info.manufacturer.clone();
        let model = self.config.device_info.model.clone();
        let owner = self.runtime.lock().device_name.clone();
        // Info 里的分辨率/编码格式按实际媒体能力填,不是随手编的常量。
        let info_template = CatalogItemInfo::camera(
            self.catalog_resolution_code(),
            self.catalog_encode_type_code(),
        );

        /// 逐节点可覆写的目录项字段(界面上"目录项扩展"那一组)。
        ///
        /// 全为 None 时行为与之前完全一致:地址取默认、经纬度取设备位置、
        /// Info 用按媒体能力推出的模板。
        #[derive(Default)]
        struct ItemOverrides<'a> {
            address: Option<&'a str>,
            longitude: Option<f64>,
            latitude: Option<f64>,
            ptz_type: Option<u8>,
            capture_position_type: Option<&'a str>,
        }

        // 设备/通道项的公共装配:必选字段 + 版本差异字段 + Info。
        let device_item = |id: &str,
                           name: &str,
                           parent: &str,
                           status: &str,
                           civil: Option<String>,
                           parental: u8,
                           with_info: bool,
                           over: ItemOverrides<'_>| {
            let mut item = CatalogItem::device(id, name, &manufacturer, &model, parent, status)
                .with_civil_code(civil)
                .with_parental(parental)
                .with_position(
                    over.longitude.unwrap_or(longitude),
                    over.latitude.unwrap_or(latitude),
                );
            if let Some(address) = over.address.filter(|a| !a.is_empty()) {
                item = item.with_address(address);
            }
            if is_2022 {
                item = item.with_gb2022_fields("A");
            } else {
                // 2016 的 Owner 是必选;2022 已删除该字段。
                item = item.with_gb2016_fields(&owner);
            }
            if with_info {
                let mut info = if is_2022 {
                    info_template.clone()
                } else {
                    info_template.to_gb2016()
                };
                if let Some(ptz) = over.ptz_type {
                    info.ptz_type = Some(ptz);
                }
                // 采集部位类型是 2022 新增的 Info 字段,给 2016 平台发它没有意义。
                if is_2022 {
                    if let Some(code) = over
                        .capture_position_type
                        .filter(|c| gb28181_protocol::manscdp::capture_position::is_valid(c))
                    {
                        info.capture_position_type = Some(code.to_string());
                    }
                }
                item = item.with_info(info);
            }
            item
        };

        let tree = self.catalog_tree.lock();
        if tree.is_empty() {
            // 扁平通道(单通道兼容路径)。查的是本设备就给全部通道,
            // 查具体通道就只给那一条(附录 J c)。
            let root = self.config.device_id.as_str();
            self.config
                .channels
                .iter()
                .filter(|ch| {
                    query_id.is_empty() || query_id == root || query_id == ch.channel_id.as_str()
                })
                .map(|ch| {
                    device_item(
                        &ch.channel_id.to_string(),
                        &ch.name,
                        root,
                        &ch.status,
                        None,
                        0,
                        true,
                        ItemOverrides::default(),
                    )
                })
                .collect()
        } else {
            // 多通道目录树:按附录 J 的规则圈定返回范围。
            let (scope, nodes) = gb28181_protocol::id_codec::catalog_query_scope(
                &tree,
                query_id,
                self.config.device_id.as_str(),
            );
            if scope == gb28181_protocol::id_codec::CatalogQueryScope::Unknown {
                tracing::warn!(
                    query_id,
                    "目录查询的对象不在本设备目录里,按附录 M 回 SumNum=0 空应答"
                );
            }
            nodes
                .into_iter()
                .map(|n| match n.node_type {
                    // 附录 J:行政区划/业务分组/虚拟组织等目录节点"各字段应按照示例固定填写",
                    // 不带 Manufacturer/Model/Address 这些设备专属字段。
                    CatalogNodeType::BusinessGroup => CatalogItem::folder(&n.id, &n.name)
                        .with_civil_code(n.civil_code.clone())
                        .with_parent(&n.parent_id),
                    CatalogNodeType::VirtualOrg => {
                        let mut item = CatalogItem::folder(&n.id, &n.name)
                            .with_civil_code(n.civil_code.clone())
                            .with_parent(&n.parent_id);
                        // 虚拟组织要声明所属业务分组(2022 新增字段)。
                        if is_2022 {
                            if let Some(group) = self.business_group_of(&tree, &n.parent_id) {
                                item.business_group_id = Some(group);
                            }
                        }
                        item
                    }
                    _ => device_item(
                        &n.id,
                        &n.name,
                        &n.parent_id,
                        &n.status,
                        n.civil_code.clone(),
                        n.node_type.parental(),
                        n.node_type == CatalogNodeType::VideoChannel,
                        ItemOverrides {
                            address: n.address.as_deref(),
                            longitude: n.longitude,
                            latitude: n.latitude,
                            ptz_type: n.ptz_type,
                            capture_position_type: n.capture_position_type.as_deref(),
                        },
                    ),
                })
                .collect()
        }
    }

    /// 目录项 Info 里的分辨率代码(附录 G `f=` 的分辨率取值)。
    ///
    /// 取当前实时源配置的分辨率;标准枚举里有的用枚举值,没有的按 `WxH` 表示。
    pub(crate) fn catalog_resolution_code(&self) -> String {
        let source = self.config.video_source.as_deref().unwrap_or("");
        let profile = media_rtp::LiveSourceSpec::parse(source)
            .ok()
            .map(|spec| match spec {
                media_rtp::LiveSourceSpec::Camera { profile, .. }
                | media_rtp::LiveSourceSpec::Screen { profile, .. } => profile,
            });
        match profile {
            Some(p) => match (p.width, p.height) {
                (1280, 720) => "5".into(),  // 720P
                (1920, 1080) => "6".into(), // 1080P/I
                (352, 288) => "2".into(),   // CIF
                (176, 144) => "1".into(),   // QCIF
                (w, h) => format!("{w}x{h}"),
            },
            // 无实时源(文件/伪流)时不谎报具体分辨率,按最常见的 720P 声明能力。
            None => "5".into(),
        }
    }

    /// 目录项 Info 里的视频编码格式代码(附录 G `f=`:2-H.264、5-H.265)。
    pub(crate) fn catalog_encode_type_code(&self) -> String {
        let source = self.config.video_source.as_deref().unwrap_or("");
        let is_h265 = media_rtp::LiveSourceSpec::parse(source)
            .ok()
            .map(|spec| match spec {
                media_rtp::LiveSourceSpec::Camera { profile, .. }
                | media_rtp::LiveSourceSpec::Screen { profile, .. } => {
                    profile.codec == media_rtp::LiveVideoCodec::H265
                }
            })
            .unwrap_or(false)
            || source.ends_with(".h265")
            || source.ends_with(".hevc");
        if is_h265 {
            "5".into()
        } else {
            "2".into()
        }
    }

    /// 沿着父链找到虚拟组织所属的业务分组 ID(附录 J 的 `BusinessGroupID`)。
    pub(crate) fn business_group_of(
        &self,
        tree: &[gb28181_protocol::id_codec::CatalogNode],
        parent_id: &str,
    ) -> Option<String> {
        use gb28181_protocol::id_codec::CatalogNodeType;
        let mut cursor = parent_id.to_string();
        // 最多上溯树的深度次,避免配置成环时死循环。
        for _ in 0..tree.len().max(1) {
            let node = tree.iter().find(|n| n.id == cursor)?;
            if node.node_type == CatalogNodeType::BusinessGroup {
                return Some(node.id.clone());
            }
            if node.parent_id == node.id {
                return None;
            }
            cursor = node.parent_id.clone();
        }
        None
    }

    /// 处理 Query,返回**一条或多条**应答 XML。
    ///
    /// 只有目录可能需要多条:单条 UDP MESSAGE 装整棵目录树容易超 MTU 被静默截断。
    /// 其余查询体量固定,恒为一条。
    pub(crate) fn handle_query_replies(
        &self,
        query: &gb28181_protocol::manscdp::Query,
        max_body: usize,
    ) -> Result<Vec<String>> {
        if query.cmd_type == "Catalog" {
            return self.catalog_replies(query, max_body);
        }
        Ok(vec![self.handle_query(query)?])
    }

    /// 单条 MANSCDP body 的上限:UDP 受 MTU 约束需要分片,TCP 是流式传输不需要。
    pub(crate) fn max_body_for(protocol: common::Transport) -> usize {
        match protocol {
            common::Transport::Udp => MAX_MANSCDP_BODY_BYTES,
            common::Transport::Tcp => usize::MAX,
        }
    }

    /// 目录应答:先按整条试算,超过安全阈值才按条目数分片(SumNum 恒为总数)。
    ///
    /// 这样小目录的报文与之前完全一致,只有真会超 MTU 的大目录才分多条。
    pub(crate) fn catalog_replies(
        &self,
        query: &gb28181_protocol::manscdp::Query,
        max_body: usize,
    ) -> Result<Vec<String>> {
        use gb28181_protocol::manscdp::CatalogResponse;
        // 附录 J:返回范围取决于被查询对象,不是一律整棵树。
        let items = self.catalog_items_for(&query.device_id);
        let single = CatalogResponse::new(&query.device_id, query.sn, items.clone()).to_xml()?;
        if single.len() <= max_body || items.is_empty() {
            return Ok(vec![single]);
        }
        // 按平均条目大小估算每条能装多少个,再留一档余量(GB18030 中文会略微膨胀)。
        let per_message = ((max_body * items.len()) / single.len()).max(1);
        let total = items.len();
        let parts = CatalogResponse::split(&query.device_id, query.sn, items, per_message);
        tracing::info!(
            total,
            per_message,
            messages = parts.len(),
            "目录较大,分多条 MESSAGE 应答(SumNum 为总数)"
        );
        parts.iter().map(|p| p.to_xml()).collect()
    }

    /// 按录像检索请求(A.2.4.5)的时间区间生成录像段。
    ///
    /// 模拟器没有真实录像,但**必须遵守请求的时间范围** —— 否则平台时间轴上
    /// 永远只有一段固定录像,回放定位无从验证。这里按整点切段(每段 1 小时,
    /// 最多 24 段),并按 `Type` 过滤(time/alarm/manual/all)。
    pub(crate) fn record_items_for(
        &self,
        query: &gb28181_protocol::manscdp::Query,
    ) -> Vec<gb28181_protocol::manscdp::RecordItem> {
        use gb28181_protocol::manscdp::RecordItem;
        const MAX_SEGMENTS: usize = 24;

        let wanted = query.record_type.as_deref().unwrap_or("all").trim();
        // 只请求 alarm/manual 时模拟器没有对应录像,返回空列表(SumNum=0)。
        if !wanted.is_empty() && !wanted.eq_ignore_ascii_case("all") && wanted != "time" {
            return Vec::new();
        }

        let (Some(start), Some(end)) = (
            query
                .start_time
                .as_deref()
                .and_then(common::clock::parse_iso8601),
            query
                .end_time
                .as_deref()
                .and_then(common::clock::parse_iso8601),
        ) else {
            // 平台没给时间区间(不合规但现实存在):退回单段占位。
            return vec![RecordItem::timed(
                &query.device_id,
                "record",
                common::clock::synced_iso8601(),
                common::clock::synced_iso8601(),
            )];
        };
        if end <= start {
            return Vec::new();
        }

        const HOUR: i64 = 3600;
        let mut items = Vec::new();
        // 从起始时间所在整点开始切段。
        let mut seg_start = start - start.rem_euclid(HOUR);
        if seg_start < start {
            seg_start = start;
        }
        while seg_start < end && items.len() < MAX_SEGMENTS {
            let seg_end = (seg_start - seg_start.rem_euclid(HOUR) + HOUR).min(end);
            if seg_end <= seg_start {
                break;
            }
            items.push(RecordItem::timed(
                &query.device_id,
                format!("record-{}", items.len() + 1),
                common::clock::epoch_to_iso8601(seg_start),
                common::clock::epoch_to_iso8601(seg_end),
            ));
            seg_start = seg_end;
        }
        items
    }

    /// 处理 Query,返回应答 XML(目录只返回首条,完整分片见 [`Self::handle_query_replies`])。
    pub(crate) fn handle_query(&self, query: &gb28181_protocol::manscdp::Query) -> Result<String> {
        use gb28181_protocol::manscdp::*;
        match query.cmd_type.as_str() {
            "Catalog" => {
                let items = self.catalog_items();
                CatalogResponse::new(&query.device_id, query.sn, items).to_xml()
            }
            "DeviceInfo" => {
                let resp = DeviceInfoResponse {
                    cmd_type: "DeviceInfo".into(),
                    sn: query.sn,
                    device_id: query.device_id.clone(),
                    result: "OK".into(),
                    // 名字读运行时值:平台改过 BasicParam/Name 后再查 DeviceInfo
                    // 必须看到新名字,否则"改了没生效"发现不了。
                    device_name: self.runtime.lock().device_name.clone(),
                    manufacturer: self.config.device_info.manufacturer.clone(),
                    model: self.config.device_info.model.clone(),
                    firmware: self.config.device_info.firmware.clone(),
                    channel: self.config.channels.len() as u32,
                };
                resp.to_xml()
            }
            "DeviceStatus" => {
                // A.2.6.6 的可选字段平台是会读的:DeviceTime 用于显示设备时钟/判断是否
                // 需要校时,Alarmstatus 用于报警值守面板。我们本就做了网络校时,
                // 报一个真实时间几乎零成本。
                let (recording, alarming) = {
                    let st = self.control_state.lock();
                    (st.recording, st.alarming)
                };
                let alarm_channels: Vec<String> = self
                    .config
                    .channels
                    .iter()
                    .map(|c| c.channel_id.to_string())
                    .collect();
                DeviceStatusResponse::online(
                    &query.device_id,
                    query.sn,
                    Some(common::clock::synced_iso8601()),
                    recording,
                    &alarm_channels,
                    alarming,
                )
                .to_xml()
            }
            "RecordInfo" => {
                // A.2.4.5:StartTime / EndTime 是**必选**检索条件。之前无论平台查哪一天
                // 都返回同一段硬编码录像("2026-07-03T10:00"),平台侧时间轴对不上。
                // 现在按请求区间切成整点段返回。
                let items = self.record_items_for(query);
                RecordInfoResponse::with_name(
                    &query.device_id,
                    self.runtime.lock().device_name.clone(),
                    query.sn,
                    items,
                )
                .to_xml()
            }
            "ConfigDownload" => {
                // 设备配置查询:按 ConfigType 返回基本参数(BasicParam)和/或视频参数(VideoParamOpt)。
                // 读的是**运行时参数**:平台刚用 DeviceConfig 改过的值必须能查回来,
                // 否则"改了没生效"根本无法从平台侧发现。
                let rt = self.runtime.lock().clone();
                let resp = ConfigDownloadResponse::by_type(
                    &query.device_id,
                    query.sn,
                    query.config_type.as_deref().unwrap_or("BasicParam"),
                    rt.device_name,
                    rt.expiration_secs,
                    rt.heartbeat_interval_secs as u32,
                    rt.heartbeat_count,
                    "1920*1080",
                );
                resp.to_xml()
            }
            "PresetQuery" => {
                // 预置位查询:返回当前预置位表(可被 PTZ 预置位设置/删除命令动态修改)。
                let items: Vec<PresetItem> = self
                    .presets
                    .lock()
                    .iter()
                    .map(|(id, name)| PresetItem {
                        preset_id: *id as u32,
                        preset_name: name.clone(),
                    })
                    .collect();
                let resp = PresetQueryResponse::new(&query.device_id, query.sn, items);
                resp.to_xml()
            }
            "AlarmStatus" => {
                // 报警状态查询(FR-17):GB-2022 每报警通道一项 DutyStatus,GB-2016 用 NotNumber。
                let alarming = self.control_state.lock().alarming;
                // 报警通道:取设备各通道 ID(模拟器无独立报警通道时用查询目标 ID 兜底)。
                let channels: Vec<String> = if self.config.channels.is_empty() {
                    vec![query.device_id.clone()]
                } else {
                    self.config
                        .channels
                        .iter()
                        .map(|c| c.channel_id.to_string())
                        .collect()
                };
                let resp = AlarmStatusResponse::new(
                    &query.device_id,
                    query.sn,
                    &channels,
                    alarming,
                    self.config.gb_version.is_2022(),
                );
                resp.to_xml()
            }
            "HomePositionQuery" => {
                // 看守位查询(FR-17):ResetTime 固定 30,PresetIndex 为"有无看守位"标志。
                let (enabled, has_home) = {
                    let s = self.control_state.lock();
                    (s.home_enabled, s.home_set)
                };
                HomePositionQueryResponse::new(&query.device_id, query.sn, enabled, has_home)
                    .to_xml()
            }
            "SDCardStatus" => {
                // 存储卡状态查询(FR-17):模拟单张 32G 卡余 24G。
                StorageCardStatusResponse::mock(&query.device_id, query.sn).to_xml()
            }
            "CruiseTrackListQuery" => {
                // 巡航轨迹列表查询(FR-17):返回已配置的轨迹号。
                let tracks: Vec<u32> = self
                    .control_state
                    .lock()
                    .cruise_tracks
                    .keys()
                    .copied()
                    .collect();
                CruiseTrackListResponse::new(&query.device_id, query.sn, &tracks).to_xml()
            }
            "CruiseTrackQuery" => {
                // 巡航轨迹详情查询(FR-17):读 GroupID(缺省 1),返回该轨迹预置点(Speed/DwellTime 固定 5/3)。
                let group = query.group_id.unwrap_or(1);
                let presets: Vec<u32> = self
                    .control_state
                    .lock()
                    .cruise_tracks
                    .get(&group)
                    .cloned()
                    .unwrap_or_default();
                CruiseTrackQueryResponse::new(&query.device_id, query.sn, group, &presets).to_xml()
            }
            "PTZPosition" => {
                // PTZ 精准状态查询(FR-17,GB-2022):返回最近精准云台姿态。
                let (pan, tilt, zoom) = self.control_state.lock().precise_pose;
                PtzPreciseStatusResponse::new(&query.device_id, query.sn, pan, tilt, zoom).to_xml()
            }
            _ => Err(Error::Gb28181(format!(
                "未实现的查询类型: {}",
                query.cmd_type
            ))),
        }
    }
}
