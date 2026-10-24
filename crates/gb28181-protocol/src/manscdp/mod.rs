//! MANSCDP XML 消息编解码。
//!
//! 实现 docs/40-protocol/manscdp.md。M1 覆盖 Keepalive(心跳 Notify);
//! M2 补充 Catalog / DeviceInfo / DeviceStatus 查询解析与应答。用 quick-xml + serde。
mod catalog;
mod control;
mod notify;
mod response;
pub use catalog::*;
pub use control::*;
pub use notify::*;
pub use response::*;

use common::{Error, Result};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

/// XML 声明前缀(GB28181 惯例用 GB2312 编码声明,内容为 ASCII 安全)。
const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"GB2312\"?>\n";

/// 把一个 MANSCDP 结构序列化为带 XML 声明的完整报文。
/// 各 Response/Notify 的 `to_xml()` 统一委托此函数,消除重复模板。
fn manscdp_to_xml<T: Serialize>(value: &T) -> Result<String> {
    let body = quick_xml::se::to_string(value)
        .map_err(|e| Error::Gb28181(format!("MANSCDP 序列化失败: {e}")))?;
    Ok(format!("{XML_DECL}{body}"))
}

/// 去掉 `<?xml ...?>` 声明,返回其后的元素部分。
fn strip_xml_decl(xml: &str) -> &str {
    match xml.find("?>") {
        Some(pos) => xml[pos + 2..].trim_start(),
        None => xml.trim_start(),
    }
}

/// MANSCDP 报文信封:根元素名 + CmdType。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// 根元素名:Control / Query / Notify / Response。
    pub root: String,
    /// `<CmdType>` 文本;缺失时为空串。
    pub cmd_type: String,
}

/// 真正**解析**出根元素与 CmdType,而不是用 `body.contains("<Control>")` 猜。
///
/// 为什么重要:子串判定可以被伪造 —— 在 Query 的注释或文本节点里塞一个
/// `<Control>` 就能改变设备的分派路径。这里用 quick-xml 逐事件读,注释、
/// 处理指令、CDATA 与文本节点都不会被误当成元素。
pub fn parse_envelope(xml: &str) -> Result<Envelope> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let config = reader.config_mut();
    config.trim_text(true);

    let mut root: Option<String> = None;
    let mut in_cmd_type = false;
    let mut cmd_type = String::new();
    let mut buf_depth = 0usize;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                buf_depth += 1;
                if root.is_none() {
                    root = Some(name);
                } else if buf_depth == 2 && name == "CmdType" {
                    in_cmd_type = true;
                }
            }
            Ok(Event::Text(t)) if in_cmd_type => {
                // quick-xml 0.41 起 `Text` 事件不再自动解实体(实体引用会作为
                // 独立的 `GeneralRef` 事件抛出),`unescape()` 也随之移除。
                // 这里读的是 `CmdType`,取值是 `Catalog`/`DeviceControl` 这类
                // 固定枚举词,不可能出现 XML 实体,所以 `xml10_content()`
                // (解码 + 换行规范化)与旧行为等价。
                cmd_type = t.xml10_content().unwrap_or_default().trim().to_string();
            }
            Ok(Event::End(_)) => {
                in_cmd_type = false;
                buf_depth = buf_depth.saturating_sub(1);
            }
            Ok(Event::Empty(e)) => {
                if root.is_none() {
                    root = Some(String::from_utf8_lossy(e.local_name().as_ref()).to_string());
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(Error::Gb28181(format!("MANSCDP XML 解析失败: {e}"))),
        }
    }

    match root {
        Some(root) => Ok(Envelope { root, cmd_type }),
        None => Err(Error::Gb28181("MANSCDP 报文无根元素".into())),
    }
}

/// 平台 → 设备的查询请求(Query),用于提取 CmdType 与 SN 以决定如何应答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename = "Query")]
pub struct Query {
    /// 命令类型:Catalog / DeviceInfo / DeviceStatus / RecordInfo 等。
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    /// 序列号,应答须回显。
    #[serde(rename = "SN")]
    pub sn: u32,
    /// 目标设备/通道 ID。
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    /// 订阅周期(秒)。仅移动位置订阅(SUBSCRIBE + MobilePosition)携带,
    /// 指示设备按此间隔周期上报位置 NOTIFY;普通查询不含此字段(None)。
    #[serde(rename = "Interval", default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<u64>,
    /// 巡航轨迹号。仅巡航轨迹详情查询(CruiseTrackQuery)携带;缺省视为 1。
    /// GB28181-2022 附录 A.2.4.12 字段名为 Number。
    #[serde(rename = "Number", default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<u32>,
    /// 配置类型(斜杠分隔可组合)。仅 ConfigDownload 查询携带,如 BasicParam / VideoParamOpt。
    #[serde(
        rename = "ConfigType",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub config_type: Option<String>,
    // ── RecordInfo 查询扩展字段(GB28181-2022 A.2.4.5) ──
    /// 录像检索起始时间。
    #[serde(rename = "StartTime", default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    /// 录像检索终止时间。
    #[serde(rename = "EndTime", default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    // ── 报警订阅筛选条件(GB28181-2022 A.2.4.6) ──
    //
    // 之前完全不解析,于是"只订阅一级警情"和"订阅全部"对设备是一样的,
    // 设备会把所有报警都推上去。
    /// 报警起始级别:0 全部;1~4 一至四级警情。
    #[serde(
        rename = "StartAlarmPriority",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub start_alarm_priority: Option<String>,
    /// 报警终止级别。
    #[serde(
        rename = "EndAlarmPriority",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub end_alarm_priority: Option<String>,
    /// 报警方式条件:0 全部;1 电话/2 设备/3 短信/4 GPS/5 视频/6 设备故障/7 其他。
    /// 可组合,如 `1/2`。
    #[serde(
        rename = "AlarmMethod",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub alarm_method: Option<String>,
    /// 报警类型条件(含义取决于 AlarmMethod)。
    #[serde(rename = "AlarmType", default, skip_serializing_if = "Option::is_none")]
    pub alarm_type: Option<String>,
    /// 报警发生起始时间。
    #[serde(
        rename = "StartAlarmTime",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub start_alarm_time: Option<String>,
    /// 报警发生终止时间。
    #[serde(
        rename = "EndAlarmTime",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub end_alarm_time: Option<String>,
    /// 录像产生类型:time/alarm/manual/all。
    #[serde(rename = "Type", default, skip_serializing_if = "Option::is_none")]
    pub record_type: Option<String>,
    /// 模糊查询:0=不模糊,1=模糊查询(同时中心+前端检索)。
    #[serde(
        rename = "IndistinctQuery",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub indistinct_query: Option<String>,
    /// 码流编号:0=主码流,1=子码流1,2=子码流2 等。
    #[serde(
        rename = "StreamNumber",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub stream_number: Option<u32>,
}

impl Query {
    /// 从 XML 解析查询。非 Query 根元素则返回错误。
    pub fn parse(xml: &str) -> Result<Self> {
        quick_xml::de::from_str(strip_xml_decl(xml))
            .map_err(|e| Error::Gb28181(format!("Query 解析失败: {e}")))
    }
}

/// 语音广播通知(平台 → 设备,CmdType=Broadcast)。
///
/// 平台请求把某音源(SourceID)广播到设备(TargetID)。设备回 Broadcast Response,
/// 若接受则**由设备向平台发起 INVITE**(设备为主叫 UAC),SDP `a=recvonly` 收 G.711A。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Notify")]
pub struct BroadcastNotify {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    /// 音源 ID(平台侧广播源)。
    #[serde(rename = "SourceID")]
    pub source_id: String,
    /// 目标 ID(应为本设备/通道 ID)。
    #[serde(rename = "TargetID")]
    pub target_id: String,
}

impl BroadcastNotify {
    /// 从 XML 解析广播通知。
    pub fn parse(xml: &str) -> Result<Self> {
        quick_xml::de::from_str(strip_xml_decl(xml))
            .map_err(|e| Error::Gb28181(format!("Broadcast 解析失败: {e}")))
    }
}

/// 语音广播应答(设备 → 平台)。Result=OK 接受、ERROR 拒绝(带 Reason)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Response")]
pub struct BroadcastResponse {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "Result")]
    pub result: String,
    /// 拒绝原因(仅 ERROR 时);如 busy / target mismatch。
    #[serde(rename = "Reason", skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl BroadcastResponse {
    /// 接受广播(Result=OK)。
    pub fn ok(device_id: impl Into<String>, sn: u32) -> Self {
        BroadcastResponse {
            cmd_type: "Broadcast".into(),
            sn,
            device_id: device_id.into(),
            result: "OK".into(),
            reason: None,
        }
    }

    /// 拒绝广播(Result=ERROR + Reason)。
    pub fn error(device_id: impl Into<String>, sn: u32, reason: impl Into<String>) -> Self {
        BroadcastResponse {
            cmd_type: "Broadcast".into(),
            sn,
            device_id: device_id.into(),
            result: "ERROR".into(),
            reason: Some(reason.into()),
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
    }
}

/// 报警订阅筛选条件(A.2.4.6 解析结果)。
///
/// 设备据此决定"哪些报警要推给平台":只订阅一级警情时不该把四级警情也推上去。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AlarmSubscribeFilter {
    /// 起始级别(含);None = 不限。
    pub start_priority: Option<u8>,
    /// 终止级别(含);None = 不限。
    pub end_priority: Option<u8>,
    /// 允许的报警方式集合;空 = 不限。
    pub methods: Vec<u8>,
    /// 允许的报警类型集合;空 = 不限。
    pub types: Vec<u32>,
}

impl AlarmSubscribeFilter {
    /// 从报警订阅请求解析筛选条件。
    pub fn from_query(query: &Query) -> Self {
        // 级别 "0" 表示全部 → 视为不限。
        let priority = |v: &Option<String>| -> Option<u8> {
            v.as_deref()?.trim().parse::<u8>().ok().filter(|p| *p != 0)
        };
        // 方式/类型可组合,如 "1/2";含 0 视为全部。
        let split = |v: &Option<String>| -> Vec<u32> {
            let Some(text) = v.as_deref() else {
                return Vec::new();
            };
            let parsed: Vec<u32> = text
                .split(['/', ',', ' '])
                .filter_map(|p| p.trim().parse::<u32>().ok())
                .collect();
            if parsed.contains(&0) {
                Vec::new()
            } else {
                parsed
            }
        };
        AlarmSubscribeFilter {
            start_priority: priority(&query.start_alarm_priority),
            end_priority: priority(&query.end_alarm_priority),
            methods: split(&query.alarm_method)
                .into_iter()
                .map(|v| v as u8)
                .collect(),
            types: split(&query.alarm_type),
        }
    }

    /// 一条报警是否应当按此订阅上报。
    pub fn accepts(&self, priority: u8, method: u8, alarm_type: Option<u32>) -> bool {
        if let Some(start) = self.start_priority {
            if priority < start {
                return false;
            }
        }
        if let Some(end) = self.end_priority {
            if priority > end {
                return false;
            }
        }
        if !self.methods.is_empty() && !self.methods.contains(&method) {
            return false;
        }
        if !self.types.is_empty() {
            match alarm_type {
                Some(t) if self.types.contains(&t) => {}
                // 订阅限定了类型但报警没带类型 → 无法判定,保守放行。
                None => {}
                _ => return false,
            }
        }
        true
    }
}
