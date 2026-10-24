use super::*;

#[test]
fn 心跳序列化含关键字段() {
    let ka = Keepalive::ok("34020000001320000001", 5);
    let xml = ka.to_xml().unwrap();
    assert!(xml.contains("<CmdType>Keepalive</CmdType>"));
    assert!(xml.contains("<SN>5</SN>"));
    assert!(xml.contains("<DeviceID>34020000001320000001</DeviceID>"));
    assert!(xml.contains("<Status>OK</Status>"));
}

#[test]
fn 心跳解析往返() {
    let ka = Keepalive::ok("34020000001320000001", 9);
    let xml = ka.to_xml().unwrap();
    let back = parse_keepalive(&xml).unwrap();
    assert_eq!(back, ka);
}

#[test]
fn 解析目录查询() {
    let xml = "<?xml version=\"1.0\"?>\n<Query>\
            <CmdType>Catalog</CmdType><SN>17</SN>\
            <DeviceID>34020000001320000001</DeviceID></Query>";
    let q = Query::parse(xml).unwrap();
    assert_eq!(q.cmd_type, "Catalog");
    assert_eq!(q.sn, 17);
    assert_eq!(q.device_id, "34020000001320000001");
}

#[test]
fn 附录m_空结果不携带记录列表() {
    // 附录 M 原文:"目录查询、文件查询未查询出结果情况下返回响应中 SumNum 应取值为 0,
    // 且不携带记录列表。" 输出一个 <DeviceList Num="0"/> 不符合这条。
    let empty = CatalogResponse::split("34020000001110000001", 17430, Vec::new(), 5);
    assert_eq!(empty.len(), 1);
    let xml = empty[0].to_xml().unwrap();
    assert!(xml.contains("<SumNum>0</SumNum>"));
    assert!(!xml.contains("DeviceList"), "空结果不应携带记录列表:{xml}");

    let empty_records = RecordInfoResponse::new("34020000001110000001", 17430, Vec::new());
    let xml = empty_records.to_xml().unwrap();
    assert!(xml.contains("<SumNum>0</SumNum>"));
    assert!(!xml.contains("RecordList"), "空结果不应携带记录列表:{xml}");

    // 非空时照常携带,且 Num 属性正确。
    let one = CatalogResponse::new(
        "34020000001110000001",
        1,
        vec![CatalogItem::folder("34020000002150000001", "组")],
    );
    let xml = one.to_xml().unwrap();
    assert!(xml.contains("Num=\"1\""));
}

#[test]
fn 附录m_每条应答记录数不超过上限() {
    // 附录 M:每条响应携带记录上限 10000 条;传入更大的分片值应被夹住。
    let items: Vec<CatalogItem> = (0..3)
        .map(|i| CatalogItem::folder(format!("3402000000215000000{i}"), "组"))
        .collect();
    let parts = CatalogResponse::split("d", 1, items, usize::MAX);
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].sum_num, 3);
    assert_eq!(parts[0].device_list.as_ref().unwrap().num, 3);
    assert_eq!(MAX_ITEMS_PER_MESSAGE, 10_000, "附录 M 规定的上限");
}

#[test]
fn 附录m_多条应答的sn与总数一致() {
    // 附录 M:各响应消息的 SN 值需与请求消息相同;SumNum 是总数而非本条条数。
    let items: Vec<CatalogItem> = (0..5)
        .map(|i| CatalogItem::folder(format!("3402000000215000000{i}"), "组"))
        .collect();
    let parts = CatalogResponse::split("d", 4242, items, 2);
    assert_eq!(parts.len(), 3); // 2 + 2 + 1
    for part in &parts {
        assert_eq!(part.sn, 4242, "每条应答的 SN 必须与请求一致");
        assert_eq!(part.sum_num, 5, "SumNum 是总数");
    }
    let nums: Vec<u32> = parts
        .iter()
        .map(|p| p.device_list.as_ref().unwrap().num)
        .collect();
    assert_eq!(nums, vec![2, 2, 1]);
}

#[test]
fn 目录项必选字段齐全且顺序符合schema() {
    // A.2.1.9 的 <sequence> 是有序的:顺序错了严格校验的平台会拒收整条应答。
    let item = CatalogItem::device(
        "34020000001320000002",
        "通道1",
        "UVP",
        "Sim",
        "34020000001110000001",
        "ON",
    )
    .with_civil_code(Some("340200".into()))
    .with_position(116.397, 39.909)
    .with_gb2022_fields("A")
    .with_info(CatalogItemInfo::camera("5", "2"));
    let xml = CatalogResponse::new("34020000001110000001", 1, vec![item])
        .to_xml()
        .unwrap();

    // 必选字段一个都不能少。
    for required in [
        "<DeviceID>",
        "<Name>",
        "<Manufacturer>",
        "<Model>",
        "<CivilCode>",
        "<Address>",
        "<Parental>",
        "<ParentID>",
        "<RegisterWay>1</RegisterWay>",
        "<Secrecy>0</Secrecy>",
        "<Status>ON</Status>",
        "<Longitude>",
        "<Latitude>",
        // Info 里当为摄像机时的必选项。
        "<GrassrootsCode>000000</GrassrootsCode>",
        "<PointType>2</PointType>",
    ] {
        assert!(xml.contains(required), "缺必选字段 {required}:{xml}");
    }

    // 设备口令绝不输出:把凭据写进目录应答等于广播给链路上每一跳。
    assert!(!xml.contains("<Password>"), "目录应答不应携带设备口令");

    // 关键顺序:RegisterWay → SecurityLevelCode → Secrecy → Status → Longitude。
    let pos = |tag: &str| xml.find(tag).unwrap_or_else(|| panic!("缺 {tag}"));
    assert!(pos("<RegisterWay>") < pos("<SecurityLevelCode>"));
    assert!(pos("<SecurityLevelCode>") < pos("<Secrecy>"));
    assert!(pos("<Secrecy>") < pos("<Status>"));
    assert!(pos("<Status>") < pos("<Longitude>"));
    assert!(pos("<Longitude>") < pos("<Latitude>"));
    assert!(pos("<Latitude>") < pos("<Info>"));
    // Info 内部顺序:PTZType → CapturePositionType → Resolution → GrassrootsCode → PointType。
    assert!(pos("<PTZType>") < pos("<CapturePositionType>"));
    assert!(pos("<CapturePositionType>") < pos("<Resolution>"));
    assert!(pos("<Resolution>") < pos("<GrassrootsCode>"));
    assert!(pos("<GrassrootsCode>") < pos("<PointType>"));
}

#[test]
fn 目录项按gb版本差异化必选字段() {
    // 2016 的 Owner 是必选,2022 删除了它;SecurityLevelCode 反之。
    let item2016 = CatalogItem::device("d", "n", "m", "mo", "p", "ON").with_gb2016_fields("某单位");
    let xml = CatalogResponse::new("p", 1, vec![item2016])
        .to_xml()
        .unwrap();
    assert!(xml.contains("<Owner>"), "2016 必选 Owner:{xml}");
    assert!(!xml.contains("<SecurityLevelCode>"));
    // Owner 必须排在 CivilCode 之前(2016 的 sequence 顺序)。
    assert!(xml.find("<Owner>").unwrap() < xml.find("<Address>").unwrap());

    let item2022 = CatalogItem::device("d", "n", "m", "mo", "p", "ON").with_gb2022_fields("B");
    let xml = CatalogResponse::new("p", 1, vec![item2022])
        .to_xml()
        .unwrap();
    assert!(!xml.contains("<Owner>"), "2022 不应输出 Owner:{xml}");
    assert!(xml.contains("<SecurityLevelCode>B</SecurityLevelCode>"));
}

#[test]
fn 目录节点项不带设备专属字段() {
    // 附录 J:业务分组/虚拟组织等目录节点各字段按示例固定填写,
    // 带上 Manufacturer/Address 这些设备字段反而不符合示例。
    let folder =
        CatalogItem::folder("34020000002150000001", "业务分组").with_parent("34020000001110000001");
    let xml = CatalogResponse::new("34020000001110000001", 1, vec![folder])
        .to_xml()
        .unwrap();
    assert!(xml.contains("<Parental>1</Parental>"));
    assert!(!xml.contains("<Manufacturer>"));
    assert!(!xml.contains("<Address>"));
    assert!(!xml.contains("<Info>"));
}

#[test]
fn 采集部位类型代码校验结构() {
    use capture_position::*;
    assert!(is_valid(CITY_ROAD));
    assert!(is_valid(HIGHWAY_CHECKPOINT));
    assert!(is_valid(BRIDGE));
    assert!(is_valid(COMMERCIAL_SQUARE));
    // 7 位数字、大类与中类顺序码不为全 0。
    assert!(!is_valid("002010"), "位数不足应拒");
    assert!(!is_valid("00201000"), "位数超出应拒");
    assert!(!is_valid("0000100"), "大类全 0 应拒");
    assert!(!is_valid("0020000"), "中类全 0 应拒");
    assert!(!is_valid("00201aa"), "非数字应拒");
}

#[test]
fn 目录应答含通道与数量() {
    let items = vec![CatalogItem::device(
        "34020000001320000002",
        "通道1",
        "UVP",
        "Sim",
        "34020000001320000001",
        "ON",
    )
    .with_gb2022_fields("A")
    .with_endpoint("10.0.0.2", 5060)];
    let resp = CatalogResponse::new("34020000001320000001", 17, items);
    assert_eq!(resp.sum_num, 1);
    let xml = resp.to_xml().unwrap();
    assert!(xml.contains("<CmdType>Catalog</CmdType>"));
    assert!(xml.contains("<SecurityLevelCode>A</SecurityLevelCode>"));
    assert!(xml.contains("<IPAddress>10.0.0.2</IPAddress>"));
    assert!(xml.contains("<SumNum>1</SumNum>"));
    assert!(xml.contains("Num=\"1\""));
    assert!(xml.contains("<DeviceID>34020000001320000002</DeviceID>"));
    assert!(xml.contains("<Status>ON</Status>"));
}

#[test]
fn 设备信息应答字段() {
    let resp = DeviceInfoResponse {
        cmd_type: "DeviceInfo".into(),
        sn: 3,
        device_id: "34020000001320000001".into(),
        result: "OK".into(),
        device_name: "UVP-Sim".into(),
        manufacturer: "UVP".into(),
        model: "Desktop".into(),
        firmware: "0.1.0".into(),
        channel: 1,
    };
    let xml = resp.to_xml().unwrap();
    assert!(xml.contains("<CmdType>DeviceInfo</CmdType>"));
    assert!(xml.contains("<Manufacturer>UVP</Manufacturer>"));
    assert!(xml.contains("<Channel>1</Channel>"));
}

#[test]
fn 设备状态应答字段() {
    // A.2.6.6:除 Online/Status 外,平台还会读 DeviceTime / Encode / Record /
    // Alarmstatus —— 这些之前完全不输出。
    let resp = DeviceStatusResponse::online(
        "34020000001320000001",
        4,
        Some("2026-07-03T11:22:33".into()),
        true,
        &["34020000001340000001".to_string()],
        true,
    );
    let xml = resp.to_xml().unwrap();
    assert!(xml.contains("<Online>ONLINE</Online>"));
    assert!(xml.contains("<Status>OK</Status>"));
    assert!(xml.contains("<DeviceTime>2026-07-03T11:22:33</DeviceTime>"));
    assert!(xml.contains("<Encode>ON</Encode>"));
    assert!(xml.contains("<Record>ON</Record>"));
    assert!(xml.contains("Num=\"1\""));
    assert!(xml.contains("<DutyStatus>ALARM</DutyStatus>"));

    // 未布防时是 OFFDUTY;无报警通道时整段省略。
    let calm = DeviceStatusResponse::online("d", 5, None, false, &[], false);
    let calm_xml = calm.to_xml().unwrap();
    assert!(calm_xml.contains("<Record>OFF</Record>"));
    assert!(!calm_xml.contains("DeviceTime"));
    assert!(!calm_xml.contains("Alarmstatus"));
}

#[test]
fn 报警订阅筛选条件解析与判定() {
    let mk = |xml: &str| Query::parse(xml).unwrap();
    // 订阅一级到二级警情、只要设备报警与视频报警。
    let q = mk(
        r#"<?xml version="1.0"?><Query><CmdType>Alarm</CmdType><SN>1</SN>
<DeviceID>34020000001320000001</DeviceID>
<StartAlarmPriority>1</StartAlarmPriority><EndAlarmPriority>2</EndAlarmPriority>
<AlarmMethod>2/5</AlarmMethod></Query>"#,
    );
    let f = AlarmSubscribeFilter::from_query(&q);
    assert_eq!(f.start_priority, Some(1));
    assert_eq!(f.end_priority, Some(2));
    assert_eq!(f.methods, vec![2, 5]);
    assert!(f.accepts(1, 5, None), "一级视频报警应被接受");
    assert!(!f.accepts(3, 5, None), "三级超出订阅级别范围");
    assert!(!f.accepts(1, 4, None), "GPS 报警不在订阅方式内");

    // 级别 0 与方式 0 都表示"全部" → 不限。
    let all = mk(
        r#"<?xml version="1.0"?><Query><CmdType>Alarm</CmdType><SN>2</SN>
<DeviceID>d</DeviceID><StartAlarmPriority>0</StartAlarmPriority>
<AlarmMethod>0</AlarmMethod></Query>"#,
    );
    let fa = AlarmSubscribeFilter::from_query(&all);
    assert_eq!(fa, AlarmSubscribeFilter::default());
    assert!(fa.accepts(4, 7, Some(13)));

    // 限定类型:不匹配的类型被拒;报警未带类型时保守放行。
    let typed = mk(
        r#"<?xml version="1.0"?><Query><CmdType>Alarm</CmdType><SN>3</SN>
<DeviceID>d</DeviceID><AlarmMethod>5</AlarmMethod><AlarmType>2/6</AlarmType></Query>"#,
    );
    let ft = AlarmSubscribeFilter::from_query(&typed);
    assert_eq!(ft.types, vec![2, 6]);
    assert!(ft.accepts(1, 5, Some(6)));
    assert!(!ft.accepts(1, 5, Some(11)));
    assert!(ft.accepts(1, 5, None));

    // 无筛选条件的订阅接受一切。
    let bare = mk(
        r#"<?xml version="1.0"?><Query><CmdType>Alarm</CmdType><SN>4</SN>
<DeviceID>d</DeviceID></Query>"#,
    );
    assert!(AlarmSubscribeFilter::from_query(&bare).accepts(4, 7, Some(1)));
}

#[test]
fn 报警通知含字段() {
    let a = AlarmNotify::video("35020000001310000132", 7, "2026-07-03T11:00:00", "移动侦测");
    let xml = a.to_xml().unwrap();
    assert!(xml.contains("<CmdType>Alarm</CmdType>"));
    assert!(xml.contains("<AlarmMethod>5</AlarmMethod>"));
    assert!(xml.contains("<AlarmDescription>移动侦测</AlarmDescription>"));
    assert!(xml.contains("<DeviceID>35020000001310000132</DeviceID>"));
    // A.2.5.3 的 Info/AlarmType:平台靠它区分具体的视频报警类型。
    assert!(
        xml.contains("<AlarmType>2</AlarmType>"),
        "视频报警必须带 AlarmType,否则平台只能显示未知类型"
    );
}

#[test]
fn 报警通知_类型与经纬度() {
    let intrusion = AlarmNotify::video_typed(
        "d",
        1,
        "2026-07-03T11:00:00",
        "入侵",
        VideoAlarmType::Intrusion,
        Some((116.4, 39.9)),
    );
    let xml = intrusion.to_xml().unwrap();
    assert!(xml.contains("<AlarmType>6</AlarmType>"));
    assert!(xml.contains("<Longitude>116.4</Longitude>"));
    assert!(xml.contains("<Latitude>39.9</Latitude>"));

    // GPS 报警:AlarmMethod=4,带经纬度、不带 Info。
    let gps = AlarmNotify::gps("d", 2, "2026-07-03T11:00:00", 1.5, 2.5);
    let gps_xml = gps.to_xml().unwrap();
    assert!(gps_xml.contains("<AlarmMethod>4</AlarmMethod>"));
    assert!(gps_xml.contains("<Longitude>1.5</Longitude>"));
    assert!(!gps_xml.contains("<AlarmType>"));
}

#[test]
fn 录像列表应答含段() {
    let items = vec![RecordItem::timed(
        "35020000001310000132",
        "rec1",
        "2026-07-03T10:00:00",
        "2026-07-03T10:05:00",
    )];
    let resp = RecordInfoResponse::new("35020000001310000132", 12, items);
    assert_eq!(resp.sum_num, 1);
    let xml = resp.to_xml().unwrap();
    assert!(xml.contains("<CmdType>RecordInfo</CmdType>"));
    assert!(xml.contains("Num=\"1\""));
    assert!(xml.contains("<StartTime>2026-07-03T10:00:00</StartTime>"));
    assert!(xml.contains("<Type>time</Type>"));
    // A.2.1.10 里 Secrecy 是必选,之前没输出会被严格校验的平台拒收。
    assert!(xml.contains("<Secrecy>0</Secrecy>"));
}

#[test]
fn 配置查询应答含基本参数() {
    let resp =
        ConfigDownloadResponse::basic("35020000001310000001", 21, "UVP-Sim-Desktop", 3600, 60, 3);
    let xml = resp.to_xml().unwrap();
    assert!(xml.contains("<CmdType>ConfigDownload</CmdType>"));
    assert!(xml.contains("<SN>21</SN>"));
    assert!(xml.contains("<Result>OK</Result>"));
    assert!(xml.contains("<BasicParam>"));
    assert!(xml.contains("<Name>UVP-Sim-Desktop</Name>"));
    assert!(xml.contains("<Expiration>3600</Expiration>"));
    assert!(xml.contains("<HeartBeatInterval>60</HeartBeatInterval>"));
    assert!(xml.contains("<HeartBeatCount>3</HeartBeatCount>"));
}

#[test]
fn 预置位查询应答含列表() {
    let resp = PresetQueryResponse::new(
        "35020000001310000001",
        88,
        vec![
            PresetItem {
                preset_id: 1,
                preset_name: "大门".into(),
            },
            PresetItem {
                preset_id: 2,
                preset_name: "停车场".into(),
            },
        ],
    );
    let xml = resp.to_xml().unwrap();
    assert!(xml.contains("<CmdType>PresetQuery</CmdType>"));
    assert!(xml.contains("<PresetList Num=\"2\">"));
    assert!(xml.contains("<PresetID>1</PresetID>"));
    assert!(xml.contains("<PresetName>大门</PresetName>"));
    assert!(xml.contains("<PresetID>2</PresetID>"));
}

#[test]
fn 目录订阅通知含事件字段() {
    let notify = CatalogNotify::new(
        "35020000001310000001",
        5,
        vec![CatalogNotifyItem {
            device_id: "35020000001310000001".into(),
            name: "Camera-1".into(),
            event: "ON".into(),
            status: "ON".into(),
        }],
    );
    let xml = notify.to_xml().unwrap();
    assert!(xml.contains("<Notify>"));
    assert!(xml.contains("<CmdType>Catalog</CmdType>"));
    assert!(xml.contains("<SumNum>1</SumNum>"));
    assert!(xml.contains("<DeviceList Num=\"1\">"));
    assert!(xml.contains("<Event>ON</Event>"));
}

#[test]
fn 解析ptz控制并识别类型() {
    let xml = "<?xml version=\"1.0\"?><Control><CmdType>DeviceControl</CmdType>\
            <SN>5</SN><DeviceID>35020000001310000132</DeviceID>\
            <PTZCmd>A50F01000000FF</PTZCmd></Control>";
    let c = Control::parse(xml).unwrap();
    assert_eq!(c.cmd_type, "DeviceControl");
    assert_eq!(c.sn, 5);
    assert_eq!(c.ptz_cmd.as_deref(), Some("A50F01000000FF"));
    assert_eq!(c.kind(), "PTZ 云台控制");
    assert_eq!(c.preset_op(), None); // 非预置位指令
}

#[test]
fn 解析预置位设置调用删除() {
    // 字节3=指令码(81 设置/82 调用/83 删除),字节5=预置位号(0x03)。
    let mk = |cmd: &str| {
        Control::parse(&format!(
            "<?xml version=\"1.0\"?><Control><CmdType>DeviceControl</CmdType>\
                 <SN>1</SN><DeviceID>d</DeviceID><PTZCmd>{cmd}</PTZCmd></Control>"
        ))
        .unwrap()
    };
    use PresetAction::*;
    // 实测 WVP 布局:字节4=0x01,字节5=预置位号。
    assert_eq!(mk("A50F01810103003B").preset_op(), Some((Set, 3)));
    assert_eq!(mk("A50F01820103003C").preset_op(), Some((Call, 3)));
    assert_eq!(mk("A50F01830103003D").preset_op(), Some((Delete, 3)));
    // WVP 真实调用码 presetId=7。
    assert_eq!(
        mk("A50F018201070 3F".replace(' ', "").as_str()).preset_op(),
        Some((Call, 7))
    );
}

#[test]
fn 解析ptz方向变倍_实测wvp码() {
    // 实测 WVP 下发码(horizonSpeed/verticalSpeed=150=0x96):
    let mk = |cmd: &str| {
        Control::parse(&format!(
            "<?xml version=\"1.0\"?><Control><CmdType>DeviceControl</CmdType>\
                 <SN>1</SN><DeviceID>d</DeviceID><PTZCmd>{cmd}</PTZCmd></Control>"
        ))
        .unwrap()
    };
    // 实测 WVP:right=0x01 left=0x02 down=0x04 up=0x08(带标签抓包核对)。
    assert!(mk("A50F0100000000B5").ptz_motion().unwrap().is_stop()); // 停止
    let right = mk("A50F0101969610F2").ptz_motion().unwrap();
    assert!(right.right && !right.left && right.pan_speed == 0x96);
    let left = mk("A50F0102969610F3").ptz_motion().unwrap();
    assert!(left.left && !left.right);
    let down = mk("A50F0104969610F5").ptz_motion().unwrap();
    assert!(down.down && !down.up);
    let up = mk("A50F0108969610F9").ptz_motion().unwrap();
    assert!(up.up && !up.down);
    assert!(mk("A50F011096961001").ptz_motion().unwrap().zoom_in);
    assert!(mk("A50F012096961011").ptz_motion().unwrap().zoom_out);
    // 预置位码不算运动。
    assert_eq!(mk("A50F01810300EA").ptz_motion(), None);
}

#[test]
fn 解析看守位设置() {
    let xml = "<?xml version=\"1.0\"?><Control><CmdType>DeviceControl</CmdType>\
            <SN>7</SN><DeviceID>d</DeviceID>\
            <HomePosition><Enabled>1</Enabled><ResetTime>60</ResetTime><PresetIndex>2</PresetIndex></HomePosition></Control>";
    let c = Control::parse(xml).unwrap();
    assert_eq!(c.kind(), "看守位设置");
    let hp = c.home_position.unwrap();
    assert_eq!(hp.enabled, 1);
    assert_eq!(hp.reset_time, 60);
    assert_eq!(hp.preset_index, 2);
}

#[test]
fn 解析强制关键帧() {
    let xml = "<Control><CmdType>DeviceControl</CmdType><SN>1</SN>\
            <DeviceID>x</DeviceID><IFameCmd>Send</IFameCmd></Control>";
    let c = Control::parse(xml).unwrap();
    assert_eq!(c.kind(), "强制关键帧");
    let resp = ControlResponse::ok("x", 1).to_xml().unwrap();
    assert!(resp.contains("<Result>OK</Result>"));
}

#[test]
fn 移动位置通知含经纬度() {
    let p = MobilePositionNotify::new(
        "35020000001310000001",
        3,
        "2026-07-03T11:00:00",
        116.397,
        39.908,
    );
    let xml = p.to_xml().unwrap();
    assert!(xml.contains("<CmdType>MobilePosition</CmdType>"));
    assert!(xml.contains("<Longitude>116.397</Longitude>"));
    assert!(xml.contains("<Latitude>39.908</Latitude>"));
}

#[test]
fn 报警状态应答_2022用item_2016用notnumber() {
    let ch = vec!["35020000001340000001".to_string()];
    let x22 = AlarmStatusResponse::new("dev", 1, &ch, true, true)
        .to_xml()
        .unwrap();
    assert!(x22.contains("<CmdType>AlarmStatus</CmdType>"));
    assert!(x22.contains("<Num>1</Num>"));
    assert!(x22.contains("<DutyStatus>ALARM</DutyStatus>"));
    assert!(!x22.contains("NotNumber"));

    let x16 = AlarmStatusResponse::new("dev", 1, &ch, false, false)
        .to_xml()
        .unwrap();
    assert!(x16.contains("<NotNumber>0</NotNumber>"));
    assert!(!x16.contains("<Item>"));
}

#[test]
fn 看守位查询应答_resettime固定30() {
    let xml = HomePositionQueryResponse::new("dev", 2, true, true)
        .to_xml()
        .unwrap();
    assert!(xml.contains("<CmdType>HomePositionQuery</CmdType>"));
    assert!(xml.contains("<Enabled>1</Enabled>"));
    assert!(xml.contains("<ResetTime>30</ResetTime>"));
    assert!(xml.contains("<PresetIndex>1</PresetIndex>"));
}

#[test]
fn 存储卡状态应答_单张32g() {
    let xml = StorageCardStatusResponse::mock("dev", 3).to_xml().unwrap();
    assert!(xml.contains("<CmdType>SDCardStatus</CmdType>"));
    assert!(xml.contains("<StorageList Num=\"1\">"));
    assert!(xml.contains("<TotalCapacity>32768</TotalCapacity>"));
    assert!(xml.contains("<RemainingSpace>24576</RemainingSpace>"));
}

#[test]
fn 巡航列表应答_名称与空表() {
    let xml = CruiseTrackListResponse::new("dev", 4, &[1, 2])
        .to_xml()
        .unwrap();
    assert!(xml.contains("<TrackList Num=\"2\">"));
    assert!(xml.contains("<Name>巡航 1</Name>"));
    let empty = CruiseTrackListResponse::new("dev", 4, &[])
        .to_xml()
        .unwrap();
    assert!(empty.contains("<TrackList Num=\"0\"/>"));
}

#[test]
fn 巡航详情应答_speed与dwell固定() {
    let xml = CruiseTrackQueryResponse::new("dev", 5, 1, &[7, 8])
        .to_xml()
        .unwrap();
    assert!(xml.contains("<GroupID>1</GroupID>"));
    assert!(xml.contains("<PresetID>7</PresetID>"));
    assert!(xml.contains("<Speed>5</Speed>"));
    assert!(xml.contains("<DwellTime>3</DwellTime>"));
}

#[test]
fn 解析巡航辅助聚焦字节码() {
    let mk = |code: &str| Control {
        cmd_type: "DeviceControl".into(),
        ptz_cmd: Some(code.into()),
        ..Default::default()
    };
    // 巡航增点:byte3=0x84, track=1, preset=3 → A50F01 84 01 03 00 <cksum>
    assert_eq!(
        mk("A50F0184010300").cruise_op(),
        Some(CruiseOp::SetPoint {
            track: 1,
            preset: 3
        })
    );
    // 巡航启动:byte3=0x88, track=2
    assert_eq!(
        mk("A50F0188020000").cruise_op(),
        Some(CruiseOp::Start { track: 2 })
    );
    // 表 A.9 注2:0x85 且字节6=00H 删除整条巡航,非 00H 才是删单点。
    assert_eq!(
        mk("A50F0185030000").cruise_op(),
        Some(CruiseOp::DelTrack { track: 3 })
    );
    assert_eq!(
        mk("A50F0185030500").cruise_op(),
        Some(CruiseOp::DelPoint {
            track: 3,
            preset: 5
        })
    );
    // 巡航速度/停留是 12 位:字节6 低 8 位 + 字节7 高 4 位。
    // 速度 = 0x34 | (0x2 << 8) = 0x234 = 564。
    assert_eq!(
        mk("A50F0186013420").cruise_op(),
        Some(CruiseOp::SetSpeed {
            track: 1,
            speed: 0x234
        })
    );
    assert_eq!(
        mk("A50F01870A0F10").cruise_op(),
        Some(CruiseOp::SetDwell {
            track: 0x0A,
            dwell: 0x10F
        })
    );

    // 辅助开关是 0x8C(开)/0x8D(关)——表 A.11,2016 与 2022 一致。
    let aux = mk("A50F018C010000").aux_op().unwrap();
    assert!(aux.on && aux.function == AuxFunction::Wiper);
    let aux2 = mk("A50F018D020000").aux_op().unwrap();
    assert!(!aux2.on && aux2.function == AuxFunction::InfraredLight);
    // 0x89/0x8A **不是**辅助开关(这是之前的实现错误),而是自动扫描(表 A.10)。
    assert!(
        mk("A50F0189010000").aux_op().is_none(),
        "0x89 是自动扫描,不能当辅助开关"
    );
    assert!(mk("A50F018A010000").aux_op().is_none());
    assert_eq!(
        mk("A50F0189010000").scan_op(),
        Some(ScanOp::Start { group: 1 })
    );
    assert_eq!(
        mk("A50F0189010100").scan_op(),
        Some(ScanOp::SetLeftBoundary { group: 1 })
    );
    assert_eq!(
        mk("A50F0189010200").scan_op(),
        Some(ScanOp::SetRightBoundary { group: 1 })
    );
    // 扫描速度同样是 12 位。
    assert_eq!(
        mk("A50F018A020530").scan_op(),
        Some(ScanOp::SetSpeed {
            group: 2,
            speed: 0x305
        })
    );
    // 辅助开关码不应被当成扫描。
    assert!(mk("A50F018C010000").scan_op().is_none());
    // FI 指令(表 A.6):0x42 聚焦近、0x41 聚焦远、0x48 光圈缩小、0x44 光圈放大。
    let near = mk("A50F0142000000").fi_op().unwrap();
    assert!(near.focus_near && !near.focus_far && !near.iris_open && !near.iris_close);
    let far = mk("A50F0141000000").fi_op().unwrap();
    assert!(far.focus_far && !far.focus_near);
    let iris_close = mk("A50F0148000000").fi_op().unwrap();
    assert!(iris_close.iris_close && !iris_close.iris_open);
    let iris_open = mk("A50F0144000000").fi_op().unwrap();
    assert!(iris_open.iris_open);
    // 0x40 停止 FI(全 false)。
    assert!(mk("A50F0140000000").fi_op().unwrap().is_stop());
    // 方向命令(0x08 上)不应误判为 FI/巡航;FI(0x42)不应误判为方向运动。
    assert!(mk("A50F0108320000").cruise_op().is_none());
    assert!(mk("A50F0108320000").fi_op().is_none());
    assert!(
        mk("A50F0142000000").ptz_motion().is_none(),
        "FI 不应被当方向运动"
    );
}

#[test]
fn 语音广播_解析与应答() {
    let xml = r#"<?xml version="1.0"?>
<Notify><CmdType>Broadcast</CmdType><SN>7</SN><SourceID>34020000002000000001</SourceID><TargetID>34020000001320000001</TargetID></Notify>"#;
    let b = BroadcastNotify::parse(xml).unwrap();
    assert_eq!(b.source_id, "34020000002000000001");
    assert_eq!(b.target_id, "34020000001320000001");

    let ok = BroadcastResponse::ok("34020000001320000001", 7)
        .to_xml()
        .unwrap();
    assert!(ok.contains("<CmdType>Broadcast</CmdType>"));
    assert!(ok.contains("<Result>OK</Result>"));
    assert!(!ok.contains("<Reason>"));

    let err = BroadcastResponse::error("d", 7, "busy").to_xml().unwrap();
    assert!(err.contains("<Result>ERROR</Result>"));
    assert!(err.contains("<Reason>busy</Reason>"));
}

#[test]
fn 目录快照diff_增删改与状态() {
    let old = CatalogSnapshot::from_items([
        ("ch1", "相机1", "ON"),
        ("ch2", "相机2", "ON"),
        ("ch3", "相机3", "ON"),
    ]);
    let next = CatalogSnapshot::from_items([
        ("ch1", "相机1", "ON"),   // 不变
        ("ch2", "相机2改", "ON"), // 改名 → UPDATE
        ("ch3", "相机3", "OFF"),  // 状态变 → OFF
        ("ch4", "相机4", "ON"),   // 新增 → ADD
    ]);
    let d = old.diff(&next);
    // ch2 UPDATE, ch3 OFF, ch4 ADD, ch1 无。删除:无。共 3 项。
    assert_eq!(d.len(), 3);
    let by_id = |id: &str| {
        d.iter()
            .find(|i| i.device_id == id)
            .map(|i| i.event.as_str())
    };
    assert_eq!(by_id("ch2"), Some("UPDATE"));
    assert_eq!(by_id("ch3"), Some("OFF"));
    assert_eq!(by_id("ch4"), Some("ADD"));
    assert_eq!(by_id("ch1"), None);

    // 删除通道 → DEL。
    let d2 = old.diff(&CatalogSnapshot::from_items([("ch1", "相机1", "ON")]));
    assert!(d2.iter().any(|i| i.device_id == "ch2" && i.event == "DEL"));
    // 无变化 → 空。
    assert!(old.diff(&old).is_empty());
}

#[test]
fn 升级进度与抓拍完成通知() {
    let n0 = DeviceUpgradeResultNotify::new("d", 1, "s1", "v2.0", 0)
        .to_xml()
        .unwrap();
    assert!(n0.contains("<CmdType>DeviceUpgradeResult</CmdType>"));
    assert!(n0.contains("<Result>0</Result>") && n0.contains("<Percent>0</Percent>"));
    let n100 = DeviceUpgradeResultNotify::new("d", 2, "s1", "v2.0", 100)
        .to_xml()
        .unwrap();
    assert!(n100.contains("<Result>1</Result>"));

    let snap = SnapShotNotify::new("d", 3, "sess", "20260704T120000_1", "t", "http://x/a.jpg")
        .to_xml()
        .unwrap();
    assert!(snap.contains("<SubCmd>SnapShot</SubCmd>"));
    assert!(snap.contains("<SnapShotID>20260704T120000_1</SnapShotID>"));

    let media = MediaStatusNotify::finished("d", 4).to_xml().unwrap();
    assert!(media.contains("<CmdType>MediaStatus</CmdType>"));
    assert!(media.contains("<NotifyType>121</NotifyType>"));
}

#[test]
fn 解析抓拍配置与升级() {
    let xml = r#"<?xml version="1.0"?>
<Control><CmdType>DeviceControl</CmdType><SN>1</SN><DeviceID>d</DeviceID>
<SnapShotConfig><SessionID>s1</SessionID><UploadURL>http://h/</UploadURL><SnapNum>20</SnapNum><Interval>2</Interval></SnapShotConfig></Control>"#;
    let c = Control::parse(xml).unwrap();
    assert_eq!(c.kind(), "抓拍配置");
    let cfg = c.snap_shot_config.unwrap();
    assert!(cfg.is_valid());
    assert_eq!(cfg.clamped_num(), 10); // 20 钳制到 10

    let xml2 = r#"<?xml version="1.0"?>
<Control><CmdType>DeviceControl</CmdType><SN>2</SN><DeviceID>d</DeviceID>
<DeviceUpgrade><Firmware>v2</Firmware><SessionID>u1</SessionID><FileURL>http://f/x.bin</FileURL></DeviceUpgrade></Control>"#;
    let c2 = Control::parse(xml2).unwrap();
    assert_eq!(c2.kind(), "在线升级");
    assert_eq!(c2.device_upgrade.unwrap().firmware, "v2");
}

#[test]
fn 解析精确云台与目标跟踪() {
    let xml = r#"<?xml version="1.0"?>
<Control><CmdType>DeviceControl</CmdType><SN>1</SN><DeviceID>d</DeviceID>
<PTZPreciseCtrl><Pan>123.45</Pan><Tilt>-15.0</Tilt><Zoom>3.5</Zoom></PTZPreciseCtrl></Control>"#;
    let c = Control::parse(xml).unwrap();
    assert_eq!(c.kind(), "精确云台控制");
    let p = c.ptz_precise_ctrl.clone().unwrap();
    assert!((p.pan - 123.45).abs() < 0.01 && (p.zoom - 3.5).abs() < 0.01);

    let xml2 = r#"<?xml version="1.0"?>
<Control><CmdType>DeviceControl</CmdType><SN>2</SN><DeviceID>d</DeviceID>
<TargetTrack><Mode>Auto</Mode><Speed>10</Speed></TargetTrack></Control>"#;
    let c2 = Control::parse(xml2).unwrap();
    assert_eq!(c2.kind(), "目标跟踪");
    assert_eq!(c2.target_track.unwrap().mode, "Auto");

    let xml3 = r#"<?xml version="1.0"?>
<Control><CmdType>DeviceControl</CmdType><SN>3</SN><DeviceID>d</DeviceID>
<FormatSDCard>1</FormatSDCard><DiskNum>0</DiskNum></Control>"#;
    let c3 = Control::parse(xml3).unwrap();
    assert_eq!(c3.kind(), "格式化SD卡");
}

#[test]
fn 配置查询_videoparamopt与组合() {
    // 仅 VideoParamOpt。
    let v =
        ConfigDownloadResponse::by_type("dev", 1, "VideoParamOpt", "cam", 3600, 60, 3, "1920*1080")
            .to_xml()
            .unwrap();
    assert!(v.contains("<VideoParamOpt>"));
    assert!(v.contains("<DownloadSpeed>1/2/4</DownloadSpeed>"));
    assert!(v.contains("<Resolution>1920*1080</Resolution>"));
    assert!(!v.contains("<BasicParam>"));
    // 组合 BasicParam/VideoParamOpt。
    let both = ConfigDownloadResponse::by_type(
        "dev",
        1,
        "BasicParam/VideoParamOpt",
        "cam",
        3600,
        60,
        3,
        "1920*1080",
    )
    .to_xml()
    .unwrap();
    assert!(both.contains("<BasicParam>") && both.contains("<VideoParamOpt>"));
}

#[test]
fn ptz精准状态应答_两位小数() {
    let xml = PtzPreciseStatusResponse::new("dev", 6, 123.456, -15.0, 3.5)
        .to_xml()
        .unwrap();
    assert!(xml.contains("<Pan>123.46</Pan>"));
    assert!(xml.contains("<Tilt>-15.00</Tilt>"));
    assert!(xml.contains("<Zoom>3.50</Zoom>"));
}

#[test]
fn 设备配置应答cmdtype为deviceconfig() {
    // A.2.6.8:DeviceConfig 应答 CmdType 须为 DeviceConfig,区别于 DeviceControl。
    let xml = ControlResponse::config_ok("dev", 7).to_xml().unwrap();
    assert!(xml.contains("<CmdType>DeviceConfig</CmdType>"));
    assert!(xml.contains("<Result>OK</Result>"));
    let ctrl = ControlResponse::ok("dev", 7).to_xml().unwrap();
    assert!(ctrl.contains("<CmdType>DeviceControl</CmdType>"));
}

#[test]
fn 实时视音频回传通知() {
    // A.2.5.8 VideoUploadNotify:带经纬度。
    let xml = VideoUploadNotify::new("dev", 1, "2026-07-05T14:00:00", Some((116.4, 39.9)))
        .to_xml()
        .unwrap();
    assert!(xml.contains("<CmdType>VideoUploadNotify</CmdType>"));
    assert!(xml.contains("<Time>2026-07-05T14:00:00</Time>"));
    assert!(xml.contains("<Longitude>116.4</Longitude>"));
    // 无位置时不含经纬度。
    let x2 = VideoUploadNotify::new("dev", 2, "t", None)
        .to_xml()
        .unwrap();
    assert!(!x2.contains("Longitude"));
}

#[test]
fn 强制关键帧兼容两种拼写() {
    // 2016 拼写 IFameCmd 与 2022 拼写 IFrameCmd 都应解析到 iframe_cmd。
    let x2016 = r#"<?xml version="1.0"?><Control><CmdType>DeviceControl</CmdType>
<SN>1</SN><DeviceID>d</DeviceID><IFameCmd>Send</IFameCmd></Control>"#;
    let x2022 = r#"<?xml version="1.0"?><Control><CmdType>DeviceControl</CmdType>
<SN>1</SN><DeviceID>d</DeviceID><IFrameCmd>Send</IFrameCmd></Control>"#;
    assert_eq!(
        Control::parse(x2016).unwrap().iframe_cmd.as_deref(),
        Some("Send")
    );
    assert_eq!(
        Control::parse(x2022).unwrap().iframe_cmd.as_deref(),
        Some("Send")
    );
}

#[test]
fn 录像检索应答含必选name() {
    // A.2.6.7:RecordInfo 应答含必选 Name。
    let xml = RecordInfoResponse::with_name("dev", "前门相机", 1, vec![])
        .to_xml()
        .unwrap();
    assert!(xml.contains("<Name>前门相机</Name>"));
    assert!(xml.contains("<CmdType>RecordInfo</CmdType>"));
}

#[test]
fn 配置查询_视频参数属性与svac() {
    // A.2.3.2.5 VideoParamAttribute 查询返回码流参数。
    let xml = ConfigDownloadResponse::by_type(
        "dev",
        1,
        "VideoParamAttribute",
        "cam",
        3600,
        60,
        3,
        "1920*1080",
    )
    .to_xml()
    .unwrap();
    assert!(xml.contains("<VideoParamAttribute"));
    assert!(xml.contains("<StreamNumber>0</StreamNumber>"));
    // SVAC 编码查询返回占位块。
    let svac = ConfigDownloadResponse::by_type(
        "dev",
        1,
        "SVACEncodeConfig",
        "cam",
        3600,
        60,
        3,
        "1920*1080",
    )
    .to_xml()
    .unwrap();
    assert!(svac.contains("<SVACEncodeConfig>"));
    assert!(svac.contains("<Result>OK</Result>"));
}
