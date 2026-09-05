//! 单设备仿真:设备配置 + 注册/心跳状态机。
//!
//! 实现 docs/10-functional/device-simulation.md §3.1(注册+Digest)、§3.2(心跳)。
//! 注册闭环:REGISTER(无鉴权)→ 平台 401 挑战 → REGISTER(带 Authorization)→ 200 OK。
//! 注册成功后按 `heartbeat_interval_secs` 周期发送 Keepalive。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::{DeviceId, Error, Result, Transport};
use gb28181_protocol::manscdp::Keepalive;
use sip_core::{authorization, Challenge, SipTransport};

use crate::builder::{self, DialogIds};

/// 设备初始内置的五个具名预置位(可被平台的预置位设置/删除命令修改)。
fn default_presets() -> std::collections::BTreeMap<u8, String> {
    [
        (1, "大门入口"),
        (2, "停车场"),
        (3, "接待大厅"),
        (4, "东侧通道"),
        (5, "西侧通道"),
    ]
    .into_iter()
    .map(|(id, name)| (id, name.to_string()))
    .collect()
}

/// 最小 HTTP PUT 上传 JPEG(抓拍上传用)。仅支持 http://(https 需 TLS,本工具不引入重依赖)。
///
/// 含 SSRF 防护:**先解析出真实目标 IP 再判定**,并直接连到已校验的地址,
/// 避免"校验用域名、连接再解析一次"的 DNS rebinding 窗口。
/// 用裸 TCP 手写请求,避免给压测链路(依赖本 crate)引入 HTTP 客户端。
async fn http_put_jpeg(url: &str, body: &[u8]) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| Error::Gb28181("抓拍上传仅支持 http:// URL".into()))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    // URL 里可能带 userinfo(user:pass@host),authority 要先剥掉它再切端口,
    // 否则 `a@evil.com:80` 会被当成 host=`a@evil.com`。
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>()
                .map_err(|_| Error::Gb28181("上传 URL 端口非法".into()))?,
        ),
        None => (authority.to_string(), 80),
    };
    // ── 请求走私防护 ──
    //
    // `path`/`host` 来自平台下发的 `UploadURL`(A.2.3.1.9 SnapShotConfig),而
    // quick-xml 会把 `&#13;&#10;` 解成真实的 CR LF。若原样插进请求行与 Host 头,
    // 恶意平台就能在一次 PUT 里追加任意请求头、甚至第二个完整请求,
    // 对上传目标(往往是内网可达的代理/缓存)做 HTTP 请求走私。
    reject_header_unsafe("上传 URL 路径", path)?;
    reject_header_unsafe("上传 URL 主机", &host)?;
    // 端口白名单:即使目标被限定为公网 IP,任意端口仍构成一个公网端口扫描器
    // (2xx / 连接失败 / 超时 三种结果可区分)。只放行常规 HTTP 端口。
    if !UPLOAD_ALLOWED_PORTS.contains(&port) {
        return Err(Error::Gb28181(format!(
            "上传目标端口 {port} 不在允许范围({}),已拒绝",
            UPLOAD_ALLOWED_PORTS
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(" / ")
        )));
    }
    // 解析 + 校验:得到一个确定安全的目标地址,后续只用它。
    let target = resolve_safe_target(&host, port).await?;

    let req = build_put_request(path, &host, body.len());
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect(target),
    )
    .await
    .map_err(|_| Error::Gb28181("上传连接超时".into()))?
    .map_err(|e| Error::Gb28181(format!("上传连接失败: {e}")))?;
    stream
        .write_all(req.as_bytes())
        .await
        .map_err(|e| Error::Gb28181(format!("上传写入失败: {e}")))?;
    stream
        .write_all(body)
        .await
        .map_err(|e| Error::Gb28181(format!("上传写入失败: {e}")))?;

    // 响应只用来读状态行,**读满上限就停**:恶意上传目标 5 秒内足够灌进几百 MB,
    // 压测上万设备同时上传会把这个放大成整机内存耗尽。8 KiB 装得下任何状态行 + 头部。
    let mut resp = vec![0u8; MAX_UPLOAD_RESPONSE_BYTES];
    let read = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut resp))
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or(0);
    resp.truncate(read);
    // 解析状态行首行 "HTTP/1.1 2xx"。
    let head = String::from_utf8_lossy(&resp);
    let ok = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .map(|c| (200..300).contains(&c))
        .unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err(Error::Gb28181(format!(
            "上传响应非 2xx: {}",
            head.lines().next().unwrap_or("")
        )))
    }
}

/// 把 host(字面 IP 或域名)解析成**已通过 SSRF 校验**的具体地址。
///
/// 域名会被真正解析,其**全部**解析结果都必须安全——只要有一条指向内网就整体拒绝,
/// 否则攻击者可以用一个同时解析到公网与 169.254.169.254 的域名绕过检查。
async fn resolve_safe_target(host: &str, port: u16) -> Result<SocketAddr> {
    use std::net::IpAddr;

    if let Ok(ip) = host.parse::<IpAddr>() {
        reject_unsafe_ip(ip)?;
        return Ok(SocketAddr::new(ip, port));
    }

    let host_owned = host.to_string();
    let addrs = tokio::task::spawn_blocking(move || {
        use std::net::ToSocketAddrs;
        (host_owned.as_str(), port)
            .to_socket_addrs()
            .map(|it| it.collect::<Vec<_>>())
    })
    .await
    .map_err(|e| Error::Gb28181(format!("上传目标解析任务异常: {e}")))?
    .map_err(|e| Error::Gb28181(format!("上传目标 {host} 解析失败: {e}")))?;

    if addrs.is_empty() {
        return Err(Error::Gb28181(format!("上传目标 {host} 无解析结果")));
    }
    for addr in &addrs {
        reject_unsafe_ip(addr.ip())?;
    }
    Ok(addrs[0])
}

/// 组装抓拍上传的 HTTP PUT 请求头。
///
/// 单独抽出来是为了能真的被测到 —— 之前测试里手抄了一份一模一样的格式串,
/// 于是"实现改了但测试还绿"这种事完全可能发生。
/// 调用方必须先用 [`reject_header_unsafe`] 校验过 `path`/`host`。
fn build_put_request(path: &str, host: &str, body_len: usize) -> String {
    format!(
        "PUT {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: image/jpeg\r\n\
         Content-Length: {body_len}\r\nConnection: close\r\n\r\n"
    )
}

/// 抓拍上传允许的目标端口。
///
/// 443 也列进来是为了给"平台配了 https 端口但写了 http"这种常见配置错误一个
/// 明确的连接失败而不是"端口被拒",诊断更直接。
pub const UPLOAD_ALLOWED_PORTS: &[u16] = &[80, 443, 8080, 8081, 8000, 8443];

/// 上传响应最多读这么多字节(只需要状态行)。
pub const MAX_UPLOAD_RESPONSE_BYTES: usize = 8 * 1024;

/// 拒绝会破坏 HTTP 报文结构的字符(CR/LF/NUL/空格)。
///
/// 这些字符出现在从 URL 切出来的 host 或 path 里,只可能是注入尝试 ——
/// 合法路径里的空格应当已被 percent-encode。
fn reject_header_unsafe(what: &str, value: &str) -> Result<()> {
    if let Some(bad) = value
        .chars()
        .find(|c| matches!(c, '\r' | '\n' | '\0' | ' ' | '\t'))
    {
        return Err(Error::Gb28181(format!(
            "{what}含非法字符 {:?},疑似 HTTP 请求注入,已拒绝",
            bad
        )));
    }
    Ok(())
}

/// 拒绝 SSRF 高危目标(环回/链路本地/私网/组播/未指定/唯一本地/文档段)。
fn reject_unsafe_ip(ip: std::net::IpAddr) -> Result<()> {
    use std::net::IpAddr;
    let bad = match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_link_local() // 含 169.254.169.254 云元数据
                || v4.is_private()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                // 100.64.0.0/10 运营商级 NAT
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
                // 0.0.0.0/8 与 240.0.0.0/4 保留段
                || v4.octets()[0] == 0
                || v4.octets()[0] >= 240
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                // fc00::/7 唯一本地地址
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // fe80::/10 链路本地(fec0::/10 站点本地已废弃但仍可能被路由,一并拦)
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || (v6.segments()[0] & 0xffc0) == 0xfec0
                // ::ffff:0:0/96 IPv4 映射地址:按其 IPv4 判定
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| reject_unsafe_ip(IpAddr::V4(v4)).is_err())
                // 下面三段都能把一个内网 IPv4 包在 IPv6 地址里,必须拆开按 IPv4 判定,
                // 否则 `::7f00:1` / `2002:7f00:1::` 这类写法可以直接绕过上面所有规则。
                //
                // ::/96 IPv4-**兼容**地址(与上面的"映射"是两回事:前缀是 96 个 0)
                || (v6.segments()[..5].iter().all(|s| *s == 0)
                    && v6.segments()[5] == 0
                    && embedded_v4_unsafe(v6.segments()[6], v6.segments()[7]))
                // 2002::/16 6to4:第 2~3 段就是被封装的 IPv4
                || (v6.segments()[0] == 0x2002
                    && embedded_v4_unsafe(v6.segments()[1], v6.segments()[2]))
                // 2001:0::/32 Teredo:第 5~6 段是取反后的客户端 IPv4
                || (v6.segments()[0] == 0x2001
                    && v6.segments()[1] == 0
                    && embedded_v4_unsafe(!v6.segments()[4], !v6.segments()[5]))
                // 64:ff9b::/96 NAT64:末两段是被翻译的 IPv4
                || (v6.segments()[0] == 0x0064
                    && v6.segments()[1] == 0xff9b
                    && embedded_v4_unsafe(v6.segments()[6], v6.segments()[7]))
        }
    };
    if bad {
        return Err(Error::Gb28181(format!("上传目标 {ip} 属受限地址,已拒绝")));
    }
    Ok(())
}

/// 把 IPv6 里内嵌的两段还原成 IPv4 后判定是否受限。
///
/// 6to4 / Teredo / NAT64 / IPv4-兼容地址都是"把一个 IPv4 塞进 IPv6"的形式,
/// 只按 IPv6 规则看的话它们全是普通的全球单播地址。
fn embedded_v4_unsafe(hi: u16, lo: u16) -> bool {
    let v4 = std::net::Ipv4Addr::from(((hi as u32) << 16) | lo as u32);
    reject_unsafe_ip(std::net::IpAddr::V4(v4)).is_err()
}

/// 单个虚拟设备的静态配置。压测时由 `scenario` 按序号批量生成。
#[derive(Debug, Clone)]
pub struct DeviceConfig {
    /// 本设备国标 ID。
    pub device_id: DeviceId,
    /// SIP 认证用户名(通常等于 device_id)。
    pub username: String,
    /// SIP 认证密码。
    ///
    /// 用 [`common::Secret`] 而不是 `String`:本结构派生了 `Debug`,
    /// 裸 `String` 的话任何一处 `tracing::debug!(?config)` 都会把密码写进
    /// 标准输出、日志文件、内存环形缓冲以及前端可拉取的 `get_recent_logs`。
    pub password: common::Secret,
    /// 上级平台 SIP 服务地址 host。
    pub server_host: String,
    /// 上级平台 SIP 服务端口。
    pub server_port: u16,
    /// 上级平台域(SIP domain / 服务器 ID 中心编码)。
    pub server_domain: String,
    /// 入站 SIP 请求的平台身份校验模式。
    pub inbound_identity_mode: common::InboundIdentityMode,
    /// 信令传输方式。
    pub transport: Transport,
    /// 心跳间隔(秒)。
    pub heartbeat_interval_secs: u64,
    /// 通道列表(目录查询应答用)。
    pub channels: Vec<ChannelConfig>,
    /// 设备信息(DeviceInfo 查询应答用)。
    pub device_info: DeviceInfo,
    /// 视频源文件路径(H.264 Annex B,C 档);None 且无 light_bitrate 时不推流(A 档)。
    pub video_source: Option<String>,
    /// 推流帧率(fps)。
    pub video_fps: u32,
    /// B 档轻量伪流码率(kbps);Some 且无 video_source 时用 LightSource。
    pub light_bitrate_kbps: Option<u32>,
    /// GB28181 协议版本(影响 MANSCDP 应答字段集,2022 默认)。
    pub gb_version: common::GbVersion,
    /// 注册有效期(秒),写入 REGISTER 的 Expires 头。
    ///
    /// 注意这只是**请求值**:平台可以在 200 OK 里授予更短的时长,续约必须以
    /// 平台授予值为准(见 [`DeviceSimulator::granted_expires`])。
    ///
    /// GB/T 28181-2022 §9.1.1 d):缺省宜为 86400s,且**不应短于 3600s**。
    pub register_expires_secs: u32,
    /// 注册失败后的重试间隔上限(秒)。
    ///
    /// §9.1.1 c) 规定"与上一次注册时间间隔应可调,一般情况下不应短于 60s"。
    /// 单设备联调默认 60s 以符合标准;**压测场景会刻意调小**(否则爬坡毫无意义),
    /// 这是有意识的偏离,不是漏实现。
    pub register_retry_max_secs: u64,
    /// 信令字符集编码(§6.10:GB18030 默认;部分平台用 UTF-8)。
    /// 决定 MANSCDP XML 体的字节编码与声明,修复中文乱码。
    pub signaling_encoding: common::SignalingEncoding,
}

/// 默认注册有效期(秒)。§9.1.1 d) 的下限是 3600s,缺省宜 86400s;
/// 这里取下限 3600s 以便联调时更快观察到续约行为。
pub const DEFAULT_REGISTER_EXPIRES: u32 = 3600;

/// 默认注册重试间隔上限(秒),对齐 §9.1.1 c) 的"不应短于 60s"。
pub const DEFAULT_REGISTER_RETRY_MAX_SECS: u64 = 60;

/// 通道配置(对应目录查询中的一个 Item)。
#[derive(Debug, Clone)]
pub struct ChannelConfig {
    /// 通道国标 ID(设备侧通道 ID 规则:设备 ID + 通道序号后缀)。
    pub channel_id: DeviceId,
    /// 通道名称。
    pub name: String,
    /// 状态:ON / OFF。
    pub status: String,
}

/// 设备信息(DeviceInfo 查询应答字段)。
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub device_name: String,
    pub manufacturer: String,
    pub model: String,
    pub firmware: String,
}

/// 设备状态(与 docs/10-functional/device-simulation.md#2、UI DTO 对齐)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    /// 离线。
    Disconnected,
    /// 注册中。
    Registering,
    /// 已注册。
    Registered,
    /// 推流中(M2)。
    InCall,
    /// 注册失败。
    Failed,
}

/// 设备仿真运行时状态机。持有配置 + 会话标识 + CSeq 计数 + 会话状态 + 事件观察者。
pub struct DeviceSimulator {
    config: DeviceConfig,
    ids: DialogIds,
    cseq: AtomicU32,
    /// 当前活跃的推流会话(INVITE → 推流中,BYE → 停止)。
    session: tokio::sync::Mutex<Option<PushSession>>,
    /// 事件观察者(上报注册/心跳/推流事件给压测指标)。默认 Noop。
    observer: Arc<dyn common::DeviceObserver>,
    /// 本端对外信令地址 (host, port),run() 启动时设置;应答 MESSAGE 填 Via/From 用。
    local_addr: std::sync::OnceLock<(String, u16)>,
    /// 当前位置(经度, 纬度)。移动位置订阅的周期 NOTIFY 上报此坐标;
    /// 由 report_position/set_position 更新。默认取一个内置坐标(北京)。
    position: parking_lot::Mutex<(f64, f64)>,
    /// 活跃订阅表(CmdType → 订阅句柄)。平台 SUBSCRIBE 建立,退订/下线时清理。
    subscriptions: tokio::sync::Mutex<HashMap<String, Subscription>>,
    /// 报警订阅对话(平台 `SUBSCRIBE + Alarm` 时记录:对话标识 + 平台地址 + 筛选条件)。
    /// 存在时,report_alarm 走对话内 NOTIFY;否则退化为独立 MESSAGE(如手动触发)。
    ///
    /// 筛选条件来自 A.2.4.6:平台只订阅一级警情时,四级警情不该被推上去。
    alarm_dialog: tokio::sync::Mutex<Option<AlarmSubscription>>,
    /// 目录订阅对话(平台 `SUBSCRIBE + Catalog` 时记录):CRUD 变更后据此发对话内增量 NOTIFY。
    catalog_dialog: tokio::sync::Mutex<Option<(builder::NotifyDialog, SocketAddr)>>,
    /// 预置位表(编号 → 名称)。PTZ 预置位设置/删除命令更新,PresetQuery 返回。
    presets: parking_lot::Mutex<std::collections::BTreeMap<u8, String>>,
    /// 设备控制态(布防/看守位/巡航/精准姿态)。扩展查询与控制命令共同维护(FR-17/18)。
    control_state: parking_lot::Mutex<ControlState>,
    /// 多通道目录树(FR-34)。为空时用 `config.channels` 扁平列表出目录;
    /// 非空时用此树(含业务分组/虚拟组织/报警通道层级)出目录,支持 CRUD + 增量 NOTIFY。
    catalog_tree: parking_lot::Mutex<Vec<gb28181_protocol::id_codec::CatalogNode>>,
    /// 上次推给平台的目录快照(用于计算增量 NOTIFY 的 diff)。
    catalog_snapshot: parking_lot::Mutex<gb28181_protocol::manscdp::CatalogSnapshot>,
    /// 是否已经发起过一次注册尝试(用于区分首次与重试)。
    attempted_once: std::sync::atomic::AtomicBool,
    /// 平台在注册 200 OK 里授予的有效期(秒)。0 = 尚未注册成功。
    ///
    /// 续约周期必须以此为准:平台完全可以只给 120s,而请求写的是 3600s。
    granted_expires: AtomicU32,
    /// 设备是否处于"平台视角的在线"状态。仅用于在状态**翻转**时上报事件,
    /// 让聚合方能维护"当前在线设备数"这个 gauge 而无需按设备去重。
    online: std::sync::atomic::AtomicBool,
    /// 服务端事务表:入站请求按 (branch, CSeq, method) 去重并缓存最终响应。
    ///
    /// 少了这层会出现一个很具体的现场故障:平台 200 OK 丢包后重传 INVITE,
    /// 设备因"已有会话"回 486 Busy Here,点播直接失败。
    server_txns: sip_core::ServerTransactions,
    /// 严格身份模式拒绝计数，用于抽样日志，避免可信网段内的洪泛灌爆日志。
    rejected_identity: AtomicU32,
    /// 当前语音广播会话(§9.12.1)。设备是主叫,同一时刻只维持一路。
    broadcast: tokio::sync::Mutex<Option<BroadcastSession>>,
    /// 广播/对讲媒体流序列号计数器(附录 L 的"媒体流序列号")。
    media_seq: AtomicU32,
    /// 平台经 DeviceConfig/BasicParam 改过的运行时参数(A.2.3.2.1)。
    runtime: parking_lot::Mutex<RuntimeParams>,
    /// 待发送的目录变更缓冲(附录 N 的批量通知)。
    ///
    /// 附录 N:"被订阅域宜在累计一定数量(如 100)条目录变更或者一定时间间隔
    /// (如 1 分钟)后,应将变更事件通知订阅域。" 逐条变更立刻发 NOTIFY 在批量
    /// CRUD(比如换一次 16 通道模板)时会瞬间打出十几条 NOTIFY。
    pending_catalog_changes: parking_lot::Mutex<PendingCatalogChanges>,
    /// 对端(平台)在注册响应的 `X-GB-Ver` 里声明的协议版本(附录 I)。
    ///
    /// None = 平台没声明。附录 I 要求"协议版本更高一方应避免向对方发送不能识别的消息",
    /// 所以这个值决定要不要发 2022 新增的通知(如 A.2.5.8 实时视音频回传通知)。
    peer_version: parking_lot::Mutex<Option<common::GbVersion>>,
}

/// 从 `Contact` 头值里取出可连接的 `IP:端口`(302 重定向用)。
///
/// 形如 `<sip:34020000002000000001@192.168.1.20:5060>` → `192.168.1.20:5060`。
/// 缺端口按 SIP 默认 5060;域名(非字面 IP)返回 None —— 上层会给出明确报错,
/// 而不是静默连到一个猜出来的地址。
fn contact_addr(contact: &str) -> Option<SocketAddr> {
    let inside = contact
        .split_once('<')
        .and_then(|(_, rest)| rest.split_once('>'))
        .map(|(uri, _)| uri)
        .unwrap_or(contact);
    let after_scheme = inside
        .strip_prefix("sip:")
        .or_else(|| inside.strip_prefix("sips:"))
        .unwrap_or(inside);
    // 去掉 userinfo 与 URI 参数。
    let host_port = after_scheme
        .rsplit_once('@')
        .map(|(_, hp)| hp)
        .unwrap_or(after_scheme);
    let host_port = host_port.split([';', '?']).next()?.trim();
    if let Ok(addr) = host_port.parse::<SocketAddr>() {
        return Some(addr);
    }
    // 无端口:补 SIP 默认端口 5060。
    format!("{host_port}:5060").parse().ok()
}

/// 单条 MANSCDP body 的安全上限(字节)。
///
/// 目标是让"SIP 头 + body"整体落在 1300 字节的安全 MTU 内:SIP 头(Via/From/To/
/// Call-ID/CSeq/Contact/Content-Type…)通常 300~400 字节,故 body 留 900。
const MAX_MANSCDP_BODY_BYTES: usize = 900;

/// 由注册有效期推导续约周期:取 80%,并夹在 [10s, 有效期-5s] 之间。
///
/// 平台给很短的 Expires(如 30s)时,80% 仍需留出一次事务往返的余量。
fn renew_interval(expires_secs: u32) -> std::time::Duration {
    let expires = expires_secs.max(1) as u64;
    let eighty = (expires * 80 / 100).max(10);
    let safe_ceiling = expires.saturating_sub(5).max(5);
    std::time::Duration::from_secs(eighty.min(safe_ceiling).max(5))
}

/// 设备控制态。由扩展控制命令(布防/看守位/巡航/精准云台)更新,
/// 供扩展查询(AlarmStatus/HomePositionQuery/CruiseTrack*/PTZPosition)读回。
#[derive(Debug, Clone)]
pub struct ControlState {
    /// 是否处于布防/报警态(AlarmStatus 的 DutyStatus)。
    pub alarming: bool,
    /// 是否正在录像(DeviceControl/RecordCmd 维护,DeviceStatus 的 Record 字段读回)。
    pub recording: bool,
    /// 看守位是否启用。
    pub home_enabled: bool,
    /// 是否已设看守位(HomePositionQuery 的 PresetIndex 标志)。
    pub home_set: bool,
    /// 巡航轨迹:轨迹号 → 该轨迹的预置位号列表(增点/删点维护)。
    pub cruise_tracks: std::collections::BTreeMap<u32, Vec<u32>>,
    /// 巡航参数:轨迹号 → (速度, 停留秒数)。GB/T 28181-2022 表 A.9 的 12 位值。
    pub cruise_params: std::collections::BTreeMap<u32, (u32, u32)>,
    /// 自动扫描:扫描组号 → (是否在扫描, 速度, 已设左边界, 已设右边界)。
    /// GB/T 28181-2022 表 A.10;之前 0x89/0x8A 被误当成辅助开关,扫描完全没实现。
    pub scan_groups: std::collections::BTreeMap<u32, ScanGroupState>,
    /// 最近一次精准云台姿态(pan 度, tilt 度, zoom 倍)。
    pub precise_pose: (f32, f32, f32),
}

/// 单个自动扫描组的状态(GB/T 28181-2022 表 A.10)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanGroupState {
    /// 是否正在自动扫描。
    pub scanning: bool,
    /// 扫描速度(12 位)。
    pub speed: u32,
    /// 是否已设左边界。
    pub left_set: bool,
    /// 是否已设右边界。
    pub right_set: bool,
}

impl Default for ControlState {
    fn default() -> Self {
        ControlState {
            alarming: false,
            recording: false,
            home_enabled: true,
            home_set: false,
            cruise_tracks: std::collections::BTreeMap::new(),
            cruise_params: std::collections::BTreeMap::new(),
            scan_groups: std::collections::BTreeMap::new(),
            precise_pose: (0.0, 0.0, 1.0),
        }
    }
}

/// 子任务停止后等待它真正退出的上限。
///
/// 为什么要等而不是发完停止信号就走:发信号只保证"任务下次检查时会退出",
/// 而推流任务一轮可能还在 send 一帧、收流任务可能正阻塞在 recv。
/// 不等的话"设备已下线"这句日志之后,socket 与子进程可能还活着几百毫秒 ——
/// 压测反复启停时这些残留会叠起来,而且**没有任何办法证明它收敛了**。
const TASK_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// 等一个子任务退出,超时则强制 abort。返回是否在超时前自行退出。
///
/// 超时不是错误:媒体发送可能卡在一个不响应的对端上。但它值得记一条日志 ——
/// 频繁超时说明某条停止路径没有真正响应停止信号。
async fn await_task(what: &str, task: tokio::task::JoinHandle<()>) -> bool {
    await_task_with_timeout(what, task, TASK_JOIN_TIMEOUT).await
}

async fn await_task_with_timeout(
    what: &str,
    mut task: tokio::task::JoinHandle<()>,
    timeout: Duration,
) -> bool {
    match tokio::time::timeout(timeout, &mut task).await {
        Ok(Ok(())) => true,
        Ok(Err(error)) if error.is_cancelled() => true,
        Ok(Err(error)) => {
            tracing::warn!(what, %error, "子任务异常结束");
            true
        }
        Err(_) => {
            tracing::warn!(
                what,
                secs = timeout.as_secs_f64(),
                "子任务未在超时内退出,正在强制取消"
            );
            // 丢弃 JoinHandle 只会 detach,不会取消任务。必须 abort 后再次 await，
            // 让推流 future 被 drop，进而必达 LiveSource::drop -> kill+wait FFmpeg。
            task.abort();
            match tokio::time::timeout(Duration::from_secs(1), &mut task).await {
                Ok(Err(error)) if error.is_cancelled() => {}
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(what, %error, "强制取消后的子任务异常"),
                Err(_) => tracing::error!(what, "强制取消后子任务仍未收敛"),
            }
            false
        }
    }
}

/// 活跃的推流会话(推流任务句柄 + 停止信号)。
struct PushSession {
    /// 唯一会话标记，防止已结束的旧任务清理后续新会话。
    marker: Arc<()>,
    /// push_stream 任务。下线时要 join 到它真的退出(见 `await_task`)。
    task: tokio::task::JoinHandle<()>,
    /// 停止信号发送端(drop 或 send 均可触发停流)。
    stop_tx: tokio::sync::oneshot::Sender<()>,
    /// 回放控制(倍速/暂停),供会话内 INFO 调整推流。
    control: Arc<media_rtp::PlaybackControl>,
    /// 是否为回放/下载会话(s=Playback/Download):结束时发 MediaStatus 121。
    is_history: bool,
    /// 附录 K c) 的媒体源标识(`Subject` 的"发送者 ID:发送方序列号");
    /// 平台不带 Subject 时为 None。
    media_source: Option<String>,
    /// 会话内主动发 BYE 所需的对话标识(附录 K a) 的媒体流丢失通知)。
    dialog: builder::InboundDialog,
}

/// 报警订阅:对话标识 + 平台地址 + 筛选条件(A.2.4.6)。
#[derive(Debug, Clone)]
struct AlarmSubscription {
    dialog: builder::NotifyDialog,
    peer: SocketAddr,
    filter: gb28181_protocol::manscdp::AlarmSubscribeFilter,
}

/// 一条活跃订阅(周期 NOTIFY 任务句柄 + 停止信号)。
/// 丢弃或 send 停止信号即终止周期上报任务。
struct Subscription {
    /// 周期 NOTIFY 任务。下线时要 join 到它真的退出(见 `await_task`)。
    task: tokio::task::JoinHandle<()>,
    /// 停止信号发送端。
    stop_tx: tokio::sync::oneshot::Sender<()>,
}

/// 附录 N 的批量通知条数阈值:累计这么多条变更就立刻通知。
pub const CATALOG_NOTIFY_BATCH_ITEMS: usize = 100;
/// 批量通知的去抖窗口:最后一条变更之后静默这么久就发出。
///
/// **有意偏离标准的示例值**:附录 N 举的例子是"一定时间间隔(如 1 分钟)",
/// 那个量级对生产平台的目录同步是合适的,但本项目是**联调工具** ——
/// 工程师在界面上改一个通道名,要能立刻在平台侧看到 NOTIFY,等一分钟没法排障。
/// 去抖同样满足附录 N 的本意(避免大批量变更时逐条推送),只是窗口取秒级。
pub const CATALOG_NOTIFY_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(1);
/// 批量通知的硬上限:第一条待发变更之后最多攒这么久,与附录 N 的示例值一致。
///
/// 变更持续不断(去抖一直被重置)时靠它兜底,保证不会无限期攒着不发。
pub const CATALOG_NOTIFY_MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(60);

/// 待发送的目录变更缓冲(附录 N)。
#[derive(Debug, Default)]
struct PendingCatalogChanges {
    /// 已累计的变更项(按发生顺序;同一通道的后续变更会覆盖前一条)。
    items: Vec<gb28181_protocol::manscdp::CatalogNotifyItem>,
    /// 第一条待发变更的时刻,用于硬上限判定。
    since: Option<std::time::Instant>,
    /// 最后一条变更的时刻,用于去抖判定。
    last: Option<std::time::Instant>,
    /// 是否已安排了延迟发送任务,避免每条变更都 spawn 一个。
    flush_scheduled: bool,
}

impl PendingCatalogChanges {
    /// 合并新变更。同一 DeviceID 只保留最新一条 —— 平台关心的是终态,
    /// 把"上线→下线→上线"三条都推过去只是噪声。
    fn merge(&mut self, changes: Vec<gb28181_protocol::manscdp::CatalogNotifyItem>) {
        for change in changes {
            if let Some(slot) = self
                .items
                .iter_mut()
                .find(|c| c.device_id == change.device_id)
            {
                *slot = change;
            } else {
                self.items.push(change);
            }
        }
        if self.items.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        if self.since.is_none() {
            self.since = Some(now);
        }
        self.last = Some(now);
    }

    /// 是否到了该发的时候:攒够条数、去抖窗口已过、或已到硬上限。
    fn should_flush(&self) -> bool {
        if self.items.is_empty() {
            return false;
        }
        self.items.len() >= CATALOG_NOTIFY_BATCH_ITEMS
            || self
                .last
                .is_some_and(|t| t.elapsed() >= CATALOG_NOTIFY_DEBOUNCE)
            || self
                .since
                .is_some_and(|t| t.elapsed() >= CATALOG_NOTIFY_MAX_DELAY)
    }

    /// 取走全部待发变更。
    fn take(&mut self) -> Vec<gb28181_protocol::manscdp::CatalogNotifyItem> {
        self.since = None;
        self.last = None;
        self.flush_scheduled = false;
        std::mem::take(&mut self.items)
    }
}

/// 平台可经 `DeviceConfig / BasicParam`(A.2.3.2.1)在运行中修改的参数。
///
/// 之前这些字段只在配置里、命令来了只打一行日志就回 OK —— 平台以为改成功了,
/// 设备行为却完全没变(心跳周期照旧、DeviceInfo 里还是老名字、再查配置也读不回来)。
#[derive(Debug, Clone)]
pub struct RuntimeParams {
    /// 设备名称(DeviceInfo / Catalog / ConfigDownload 都读它)。
    pub device_name: String,
    /// 注册有效期(秒)。
    pub expiration_secs: u32,
    /// 心跳间隔(秒)。
    pub heartbeat_interval_secs: u64,
    /// 心跳超时次数(连续失败多少次判掉线并重注册)。
    pub heartbeat_count: u32,
}

impl RuntimeParams {
    /// 用启动配置初始化。心跳超时次数没有独立配置项,沿用文档里的重注册阈值 3
    /// (见 docs/10-functional/device-simulation.md §3.2)。
    fn from_config(config: &DeviceConfig) -> Self {
        RuntimeParams {
            device_name: config.device_info.device_name.clone(),
            expiration_secs: config.register_expires_secs,
            heartbeat_interval_secs: config.heartbeat_interval_secs,
            heartbeat_count: DEFAULT_HEARTBEAT_COUNT,
        }
    }
}

/// 心跳连续失败多少次判定掉线并重注册。平台可经 BasicParam 的 HeartBeatCount 改。
const DEFAULT_HEARTBEAT_COUNT: u32 = 3;

/// 语音广播会话(GB/T 28181-2022 §9.12.1,设备是**语音流接收者**)。
///
/// 设备作主叫发出 INVITE、收 200 OK、发 ACK,然后在本端端口上收平台推来的
/// G.711 音频 RTP(附录 C.2.4)。平台发 BYE 或设备主动挂断时结束。
struct BroadcastSession {
    /// 出站对话(Call-ID / from-tag / 学到的平台 to-tag)。
    dialog: builder::OutboundDialog,
    /// 平台地址(ACK/BYE 的目的地)。
    peer: SocketAddr,
    /// 语音流发送者 ID(SourceID)。
    source_id: String,
    /// 本端收流端口(诊断/前端展示)。
    local_port: u16,
    /// 收流统计(包数/字节/丢包/电平/空闲时长)。
    stats: Arc<media_rtp::RecvStats>,
    /// 停止信号:发送或丢弃即结束收流任务。
    stop_tx: tokio::sync::oneshot::Sender<()>,
    /// 收流任务。下线时要 join 到它真的退出(见 `await_task`)。
    task: tokio::task::JoinHandle<()>,
}

/// 当前推流会话的媒体遥测快照(发送侧统计 + 平台 RTCP 反馈)。
///
/// 设备侧一直只能看到"我发了多少",看不到"平台收到了多少" ——
/// 现场排"画面卡但设备说在发"这类问题时,平台回的 RTCP RR 才是关键证据。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MediaTelemetrySnapshot {
    /// 会话是回放/下载(true)还是实时(false)。
    pub is_history: bool,
    /// 附录 K c) 的媒体源标识(平台带 Subject 时才有)。
    pub media_source: Option<String>,
    /// 本端 RTCP 端口;TCP 模式或未抢到 RTP+1 时为 None,此时收不到 RR。
    pub local_rtcp_port: Option<u16>,
    /// 已发送 RTP 包数。
    pub sent_packets: u64,
    /// 已发送字节数(含 RTP 头)。
    pub sent_bytes: u64,
    /// 已发送 RTCP SR 包数。
    pub sent_rtcp: u64,
    /// 本地发送失败次数。
    pub send_errors: u64,
    /// 平台回报的收流质量;从未收到 RR 时字段 `reports` 为 0。
    pub feedback: media_rtp::RtcpFeedbackSnapshot,
}

/// 语音广播会话快照(给前端面板)。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BroadcastSnapshot {
    /// 语音流发送者 ID。
    pub source_id: String,
    /// 本端收流端口。
    pub local_port: u16,
    /// 会话 Call-ID。
    pub call_id: String,
    /// 收流统计。
    pub recv: media_rtp::RecvStatsSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InboundIdentityRejection {
    status: u16,
    reason: &'static str,
    detail: &'static str,
}

impl DeviceSimulator {
    /// 用配置创建一个待运行的设备仿真实例(无观察者)。
    pub fn new(config: DeviceConfig) -> Self {
        let runtime = RuntimeParams::from_config(&config);
        Self {
            config,
            ids: DialogIds::new(),
            cseq: AtomicU32::new(1),
            session: tokio::sync::Mutex::new(None),
            observer: Arc::new(common::NoopObserver),
            local_addr: std::sync::OnceLock::new(),
            position: parking_lot::Mutex::new((116.397_428, 39.909_230)),
            subscriptions: tokio::sync::Mutex::new(HashMap::new()),
            alarm_dialog: tokio::sync::Mutex::new(None),
            catalog_dialog: tokio::sync::Mutex::new(None),
            presets: parking_lot::Mutex::new(default_presets()),
            control_state: parking_lot::Mutex::new(ControlState::default()),
            catalog_tree: parking_lot::Mutex::new(Vec::new()),
            catalog_snapshot: parking_lot::Mutex::new(gb28181_protocol::manscdp::CatalogSnapshot {
                channels: std::collections::BTreeMap::new(),
            }),
            attempted_once: std::sync::atomic::AtomicBool::new(false),
            granted_expires: AtomicU32::new(0),
            online: std::sync::atomic::AtomicBool::new(false),
            server_txns: sip_core::ServerTransactions::default(),
            rejected_identity: AtomicU32::new(0),
            broadcast: tokio::sync::Mutex::new(None),
            media_seq: AtomicU32::new(1),
            runtime: parking_lot::Mutex::new(runtime),
            peer_version: parking_lot::Mutex::new(None),
            pending_catalog_changes: parking_lot::Mutex::new(PendingCatalogChanges::default()),
        }
    }

    /// 带事件观察者创建(压测时由编排器注入共享 Metrics)。
    pub fn with_observer(config: DeviceConfig, observer: Arc<dyn common::DeviceObserver>) -> Self {
        let runtime = RuntimeParams::from_config(&config);
        Self {
            config,
            ids: DialogIds::new(),
            cseq: AtomicU32::new(1),
            session: tokio::sync::Mutex::new(None),
            observer,
            local_addr: std::sync::OnceLock::new(),
            position: parking_lot::Mutex::new((116.397_428, 39.909_230)),
            subscriptions: tokio::sync::Mutex::new(HashMap::new()),
            alarm_dialog: tokio::sync::Mutex::new(None),
            catalog_dialog: tokio::sync::Mutex::new(None),
            presets: parking_lot::Mutex::new(default_presets()),
            control_state: parking_lot::Mutex::new(ControlState::default()),
            catalog_tree: parking_lot::Mutex::new(Vec::new()),
            catalog_snapshot: parking_lot::Mutex::new(gb28181_protocol::manscdp::CatalogSnapshot {
                channels: std::collections::BTreeMap::new(),
            }),
            attempted_once: std::sync::atomic::AtomicBool::new(false),
            granted_expires: AtomicU32::new(0),
            online: std::sync::atomic::AtomicBool::new(false),
            server_txns: sip_core::ServerTransactions::default(),
            rejected_identity: AtomicU32::new(0),
            broadcast: tokio::sync::Mutex::new(None),
            media_seq: AtomicU32::new(1),
            runtime: parking_lot::Mutex::new(runtime),
            peer_version: parking_lot::Mutex::new(None),
            pending_catalog_changes: parking_lot::Mutex::new(PendingCatalogChanges::default()),
        }
    }

    /// 本端信令 host(run 启动后有效,未设时回退 0.0.0.0)。
    fn local_host(&self) -> String {
        self.local_addr
            .get()
            .map(|(h, _)| h.clone())
            .unwrap_or_else(|| "0.0.0.0".into())
    }

    /// 本端信令端口(run 启动后有效,未设时回退 0)。
    fn local_port(&self) -> u16 {
        self.local_addr.get().map(|(_, p)| *p).unwrap_or(0)
    }

    /// 只读访问配置。
    pub fn config(&self) -> &DeviceConfig {
        &self.config
    }

    /// 设置本端信令地址(仅供测试/非 run 场景手动注入应答用;run() 会自动设置)。
    #[doc(hidden)]
    pub fn set_local_addr_for_test(&self, host: &str, port: u16) {
        let _ = self.local_addr.set((host.to_string(), port));
    }

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
    fn catalog_publishable(&self) -> Vec<(String, String, String)> {
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

    /// 本设备注册用的 Call-ID(用于在共享传输上注册接收路由)。
    pub fn call_id(&self) -> &str {
        &self.ids.call_id
    }

    fn next_cseq(&self) -> u32 {
        self.cseq.fetch_add(1, Ordering::Relaxed)
    }

    /// 执行完整注册流程(REGISTER→401→带鉴权 REGISTER→200)。
    ///
    /// `transport` 为共享 UDP 传输;`local_host/local_port` 为本端对外地址(填 Via/Contact);
    /// 成功返回 [`DeviceState::Registered`],鉴权/拒绝失败返回 [`DeviceState::Failed`] 对应错误。
    pub async fn register(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<DeviceState> {
        // 首次尝试与重试分开计:把重试算进"尝试"会让成功率变成尝试维度,
        // 长跑后被稀释到看不出真实上线比例。
        let retry = self.attempted_once.swap(true, Ordering::Relaxed);
        self.register_with_retry_flag(transport, local_host, local_port, retry)
            .await
    }

    async fn register_with_retry_flag(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
        retry: bool,
    ) -> Result<DeviceState> {
        use common::{DeviceEvent, FailureKind};
        self.observer
            .on_event(DeviceEvent::RegisterAttempt { retry });
        let started = std::time::Instant::now();
        let result = self.register_inner(transport, local_host, local_port).await;
        match &result {
            Ok(_) => {
                self.observer.on_event(DeviceEvent::RegisterSuccess {
                    rtt_ms: started.elapsed().as_millis() as u64,
                });
                self.set_online(true);
            }
            Err(e) => {
                // 按**错误变体**归因,不再匹配错误文案。
                // **本地发送失败单独一类**:本机 ENOBUFS/socket 错误绝不能算成
                // "平台承载不住",否则压测结论直接失真。
                let kind = match e {
                    Error::Io(_) => FailureKind::Local,
                    Error::Timeout(_) => FailureKind::Timeout,
                    Error::Rejected { status, .. } => FailureKind::Rejected { status: *status },
                    _ => FailureKind::Other,
                };
                self.observer.on_event(DeviceEvent::RegisterFailure {
                    kind,
                    device_id: self.config.device_id.as_str().to_string(),
                });
            }
        }
        result
    }

    /// 翻转在线状态并在**真的变化时**上报事件。
    fn set_online(&self, online: bool) {
        let previous = self.online.swap(online, Ordering::Relaxed);
        if previous != online {
            self.observer
                .on_event(common::DeviceEvent::OnlineChanged { online });
        }
    }

    /// 注册核心逻辑(不含事件上报)。
    ///
    /// 相对最初实现补齐了两处:
    /// - **尊重平台授予的 Expires**:请求写配置值,但续约周期以 200 OK 里平台给的为准。
    ///   之前硬编码 3600 并按 2880s 续约,平台只给 120s 时会被判掉线。
    /// - **stale nonce 重试**:平台回 `stale=true` 时用新 nonce 再算一次,而不是当成密码错。
    async fn register_inner(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<DeviceState> {
        let dst = self.platform_addr()?;
        // 路由用 RAII 守卫:注册期间被停止打断时也不会在路由表里留下孤条目。
        let (mut rx, _route) = sip_core::RouteGuard::register(transport, &self.ids.call_id);
        let timing = sip_core::Timing::default();
        let want_expires = self.config.register_expires_secs.max(1);

        // 首发 REGISTER(无鉴权)。
        let cseq1 = self.next_cseq();
        let req1 = builder::register(
            &self.config,
            &self.ids,
            cseq1,
            local_host,
            local_port,
            None,
            want_expires,
            transport.protocol(),
        );
        let resp =
            sip_core::client_transact(transport.as_ref(), dst, &req1, &mut rx, timing).await?;

        match resp.status {
            200 => {
                self.accept_registration(&resp, want_expires);
                Ok(DeviceState::Registered)
            }
            // 重定向服务器也可能不鉴权直接回 302。
            302 => {
                let target = resp
                    .headers
                    .get("Contact")
                    .and_then(contact_addr)
                    .ok_or_else(|| Error::Sip("302 重定向缺少可用的 Contact 地址".into()))?;
                tracing::info!(%target, "注册被重定向(302),改向新服务器注册");
                self.register_redirected(transport, local_host, local_port, target)
                    .await
            }
            401 => {
                // 解析挑战,带 Authorization 重发。最多重试两轮(第二轮用于 stale nonce)。
                let mut challenge_header = resp
                    .headers
                    .get("WWW-Authenticate")
                    .ok_or_else(|| Error::Sip("401 缺少 WWW-Authenticate".into()))?
                    .to_string();
                for attempt in 0..2 {
                    let challenge = Challenge::parse(&challenge_header)?;
                    let auth = authorization(
                        &challenge,
                        &self.config.username,
                        self.config.password.expose_secret(),
                        "REGISTER",
                        &req1.uri,
                    );
                    let cseq = self.next_cseq();
                    let req = builder::register(
                        &self.config,
                        &self.ids,
                        cseq,
                        local_host,
                        local_port,
                        Some(&auth),
                        want_expires,
                        transport.protocol(),
                    );
                    let resp2 =
                        sip_core::client_transact(transport.as_ref(), dst, &req, &mut rx, timing)
                            .await?;
                    match resp2.status {
                        200 => {
                            // 网络校时:用平台 200 OK 的 Date 头校准本地时钟偏移(FR)。
                            if let Some(date) = resp2.headers.get("Date") {
                                if let Some(off) = common::clock::sync_from_date_header(date) {
                                    tracing::info!(platform_date = %date, offset_secs = off, "网络校时完成");
                                }
                            }
                            self.accept_registration(&resp2, want_expires);
                            return Ok(DeviceState::Registered);
                        }
                        401 if attempt == 0 => {
                            // 平台可能因 nonce 过期(stale)再挑战一次:换新 nonce 重算。
                            let next = resp2
                                .headers
                                .get("WWW-Authenticate")
                                .ok_or_else(|| {
                                    Error::Sip("注册被拒: 401 重挑战但缺少 WWW-Authenticate".into())
                                })?
                                .to_string();
                            let stale = Challenge::parse(&next).map(|c| c.stale).unwrap_or(false);
                            if !stale && next == challenge_header {
                                return Err(Error::rejected(
                                    401,
                                    "Unauthorized(nonce 未更新,通常是密码或用户名错)",
                                ));
                            }
                            tracing::info!(stale, "平台再次挑战,用新 nonce 重算鉴权");
                            challenge_header = next;
                        }
                        // §9.1.2.3 注册重定向:重定向服务器在鉴权后回 302,
                        // Contact 里给出真正的 SIP 服务器地址,设备应改向它注册。
                        // 之前 302 被当成"注册被拒",接重定向服务器的平台永远注册不上。
                        302 => {
                            let target = resp2
                                .headers
                                .get("Contact")
                                .and_then(contact_addr)
                                .ok_or_else(|| {
                                    Error::Sip("302 重定向缺少可用的 Contact 地址".into())
                                })?;
                            tracing::info!(%target, "注册被重定向(302),改向新服务器注册");
                            return self
                                .register_redirected(transport, local_host, local_port, target)
                                .await;
                        }
                        other => {
                            return Err(Error::rejected(
                                other,
                                format!("{}(鉴权后注册)", resp2.reason),
                            ))
                        }
                    }
                }
                Err(Error::rejected(
                    401,
                    "Unauthorized(反复 401,鉴权始终不被接受)",
                ))
            }
            other => Err(Error::rejected(other, format!("{}(注册)", resp.reason))),
        }
    }

    /// 按 302 重定向给出的新地址重新注册一轮(§9.1.2.3)。
    ///
    /// 只重定向一次:再收到 302 视为重定向环路并报错,避免无限跳转。
    async fn register_redirected(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
        target: SocketAddr,
    ) -> Result<DeviceState> {
        let want_expires = self.config.register_expires_secs.max(1);
        // 重定向后的注册用独立 Call-ID,避免与原事务路由冲突。
        let ids = DialogIds::new();
        let (mut rx, _route) = sip_core::RouteGuard::register(transport, &ids.call_id);
        let timing = sip_core::Timing::default();
        let result = async {
            let cseq = self.next_cseq();
            let req = builder::register(
                &self.config,
                &ids,
                cseq,
                local_host,
                local_port,
                None,
                want_expires,
                transport.protocol(),
            );
            let resp = sip_core::client_transact(transport.as_ref(), target, &req, &mut rx, timing)
                .await?;
            match resp.status {
                200 => {
                    self.accept_registration(&resp, want_expires);
                    Ok(DeviceState::Registered)
                }
                401 => {
                    let wa = resp
                        .headers
                        .get("WWW-Authenticate")
                        .ok_or_else(|| Error::Sip("重定向后 401 缺少 WWW-Authenticate".into()))?;
                    let challenge = Challenge::parse(wa)?;
                    let auth = authorization(
                        &challenge,
                        &self.config.username,
                        self.config.password.expose_secret(),
                        "REGISTER",
                        &req.uri,
                    );
                    let cseq2 = self.next_cseq();
                    let req2 = builder::register(
                        &self.config,
                        &ids,
                        cseq2,
                        local_host,
                        local_port,
                        Some(&auth),
                        want_expires,
                        transport.protocol(),
                    );
                    let resp2 = sip_core::client_transact(
                        transport.as_ref(),
                        target,
                        &req2,
                        &mut rx,
                        timing,
                    )
                    .await?;
                    if resp2.status == 200 {
                        if let Some(date) = resp2.headers.get("Date") {
                            if let Some(off) = common::clock::sync_from_date_header(date) {
                                tracing::info!(offset_secs = off, "网络校时完成(重定向后)");
                            }
                        }
                        self.accept_registration(&resp2, want_expires);
                        Ok(DeviceState::Registered)
                    } else {
                        Err(Error::rejected(
                            resp2.status,
                            format!("{}(重定向后鉴权注册)", resp2.reason),
                        ))
                    }
                }
                302 => Err(Error::Sip("注册重定向环路(连续两次 302),放弃".into())),
                other => Err(Error::rejected(
                    other,
                    format!("{}(重定向后注册)", resp.reason),
                )),
            }
        }
        .await;
        result
    }

    /// 记录平台在 200 OK 里授予的注册有效期。
    ///
    /// 优先取 Expires 头,其次取 Contact 的 `expires` 参数,都没有才退回请求值
    /// (RFC 3261 §10.2.4:服务端未回 Expires 时视为接受请求值)。
    fn accept_registration(&self, resp: &sip_core::Response, requested: u32) {
        let granted = resp
            .headers
            .expires()
            .or_else(|| resp.headers.contact_expires())
            .map(|v| v.min(u32::MAX as u64) as u32)
            .filter(|v| *v > 0)
            .unwrap_or(requested);
        let previous = self.granted_expires.swap(granted, Ordering::Relaxed);
        if previous != granted {
            tracing::info!(requested, granted, "平台授予注册有效期(续约周期以此为准)");
        }
        // 附录 I:从注册响应的 X-GB-Ver 学到平台支持的协议版本。
        let declared = resp.headers.get("X-GB-Ver");
        let parsed = declared.and_then(common::GbVersion::from_x_gb_ver);
        *self.peer_version.lock() = parsed;
        match (declared, parsed) {
            (Some(raw), Some(version)) => {
                tracing::info!(x_gb_ver = %raw, peer = %version, "平台声明的协议版本")
            }
            (Some(raw), None) => {
                tracing::warn!(x_gb_ver = %raw, "平台声明了无法识别的协议版本,按保守方式交互")
            }
            // 大量在网平台不发这个头,不值得每次注册都告警。
            (None, _) => tracing::debug!("平台未声明 X-GB-Ver"),
        }
    }

    /// 对端是否确定支持 GB/T 28181-2022 的新增消息(附录 I)。
    ///
    /// 平台没声明 `X-GB-Ver` 时返回 false:附录 I 要求高版本一方避免发对方不认识的消息,
    /// "不知道"就该按不支持处理。本机自己配成 2016 时同样返回 false。
    pub fn peer_supports_2022(&self) -> bool {
        self.config.gb_version.is_2022()
            && matches!(*self.peer_version.lock(), Some(common::GbVersion::V2022))
    }

    /// 平台声明的协议版本(附录 I);None 表示未声明或无法识别。
    pub fn peer_protocol_version(&self) -> Option<common::GbVersion> {
        *self.peer_version.lock()
    }

    /// 平台当前授予的注册有效期(秒)。未注册成功前为配置的请求值。
    pub fn granted_expires(&self) -> u32 {
        let v = self.granted_expires.load(Ordering::Relaxed);
        if v == 0 {
            self.config.register_expires_secs.max(1)
        } else {
            v
        }
    }

    /// 解析平台信令地址。
    fn platform_addr(&self) -> Result<SocketAddr> {
        format!("{}:{}", self.config.server_host, self.config.server_port)
            .parse()
            .map_err(|_| {
                Error::Config(format!(
                    "平台地址非法: {}:{}",
                    self.config.server_host, self.config.server_port
                ))
            })
    }

    /// 向平台注销(GB28181-2022 §9.1.2.2):发 REGISTER Expires=0。
    ///
    /// 设备下线时主动注销,平台立即置离线,无需等心跳超时。与注册同样处理 401 鉴权挑战。
    /// 尽力而为:失败仅记录警告(下线流程不因注销失败而阻塞)。
    pub async fn unregister(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<()> {
        let dst: SocketAddr = format!("{}:{}", self.config.server_host, self.config.server_port)
            .parse()
            .map_err(|_| Error::Config("平台地址非法".into()))?;
        // 用独立 Call-ID,避免与运行中的注册路由冲突。
        let ids = DialogIds::new();
        let (mut rx, _route) = sip_core::RouteGuard::register(transport, &ids.call_id);
        let timing = sip_core::Timing::default();

        // 首发 REGISTER Expires=0(无鉴权)。
        let cseq1 = self.next_cseq();
        let req1 = builder::register(
            &self.config,
            &ids,
            cseq1,
            local_host,
            local_port,
            None,
            0,
            transport.protocol(),
        );
        let result = async {
            let resp =
                sip_core::client_transact(transport.as_ref(), dst, &req1, &mut rx, timing).await?;
            match resp.status {
                200 => Ok(()),
                401 => {
                    let wa = resp
                        .headers
                        .get("WWW-Authenticate")
                        .ok_or_else(|| Error::Sip("注销 401 缺少 WWW-Authenticate".into()))?;
                    let challenge = Challenge::parse(wa)?;
                    let auth = authorization(
                        &challenge,
                        &self.config.username,
                        self.config.password.expose_secret(),
                        "REGISTER",
                        &req1.uri,
                    );
                    let cseq2 = self.next_cseq();
                    let req2 = builder::register(
                        &self.config,
                        &ids,
                        cseq2,
                        local_host,
                        local_port,
                        Some(&auth),
                        0,
                        transport.protocol(),
                    );
                    let resp2 =
                        sip_core::client_transact(transport.as_ref(), dst, &req2, &mut rx, timing)
                            .await?;
                    if resp2.status == 200 {
                        Ok(())
                    } else {
                        Err(Error::Sip(format!("注销被拒: {}", resp2.status)))
                    }
                }
                other => Err(Error::Sip(format!("注销被拒: {other}"))),
            }
        }
        .await;
        result
    }

    /// 发送一条无对话的 MANSCDP MESSAGE(心跳/报警/位置等)并等平台响应。
    ///
    /// 每次事务用**全新** [`DialogIds`](独立 Call-ID):同一设备的心跳、报警、
    /// 位置上报可能并发(周期任务 + 手动触发),若共用注册 Call-ID 会在共享传输上
    /// 互相覆盖接收路由、抢走对方的响应导致误判超时。独立 Call-ID 彻底隔离,
    /// 事务结束即注销路由。
    async fn send_message_xml(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
        xml: &str,
    ) -> Result<u16> {
        let dst: SocketAddr = format!("{}:{}", self.config.server_host, self.config.server_port)
            .parse()
            .map_err(|_| Error::Config("平台地址非法".into()))?;
        let ids = DialogIds::new();
        let cseq = self.next_cseq();
        let req = builder::message_xml(
            &self.config,
            &ids,
            cseq,
            local_host,
            local_port,
            xml,
            transport.protocol(),
        );
        let (mut rx, _route) = sip_core::RouteGuard::register(transport, &ids.call_id);
        let result = sip_core::client_transact(
            transport.as_ref(),
            dst,
            &req,
            &mut rx,
            sip_core::Timing::default(),
        )
        .await;
        Ok(result?.status)
    }

    /// 发送一条通知 MESSAGE 但**不等待响应**(fire-and-forget)。
    ///
    /// 用于纯告知性、平台可能不 ACK 的通知(如 VideoUploadNotify —— 部分平台
    /// 不支持该 CmdType 直接丢弃)。发出即返回,不因平台不响应而阻塞重传。
    async fn send_message_xml_oneshot(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
        xml: &str,
    ) -> Result<()> {
        let dst: SocketAddr = format!("{}:{}", self.config.server_host, self.config.server_port)
            .parse()
            .map_err(|_| Error::Config("平台地址非法".into()))?;
        let ids = DialogIds::new();
        let cseq = self.next_cseq();
        let req = builder::message_xml(
            &self.config,
            &ids,
            cseq,
            local_host,
            local_port,
            xml,
            transport.protocol(),
        );
        transport
            .send_to(&sip_core::SipMessage::Request(req), dst)
            .await
    }

    /// 发送一次心跳(MESSAGE + Keepalive XML)。返回平台响应状态码。
    pub async fn send_keepalive(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<u16> {
        let sn = self.next_cseq();
        let ka = Keepalive::ok(self.config.device_id.as_str(), sn);
        let xml = ka.to_xml()?;
        let started = std::time::Instant::now();
        let status = self
            .send_message_xml(transport, local_host, local_port, &xml)
            .await?;
        if status == 200 {
            self.observer.on_event(common::DeviceEvent::HeartbeatOk {
                rtt_ms: started.elapsed().as_millis() as u64,
            });
        } else {
            self.observer.on_event(common::DeviceEvent::HeartbeatFail);
        }
        Ok(status)
    }

    /// 主动上报一条视频侦测报警(FR-9)。设备 → 平台 MESSAGE + Alarm Notify XML。
    /// `description` 为报警描述;返回平台响应状态码。
    ///
    /// 默认按"运动目标检测"(A.2.5.3 视频报警类型 2)上报。
    pub async fn report_alarm(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
        description: &str,
    ) -> Result<u16> {
        self.report_alarm_typed(
            transport,
            local_host,
            local_port,
            description,
            gb28181_protocol::manscdp::VideoAlarmType::MotionDetection,
        )
        .await
    }

    /// 主动上报一条指定类型的视频报警。
    ///
    /// 存在报警订阅时会先过一遍订阅筛选条件(A.2.4.6):平台只订阅一级警情或特定
    /// 报警方式/类型时,不满足的报警不推送(返回 0 表示"按订阅条件被过滤")。
    pub async fn report_alarm_typed(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
        description: &str,
        alarm_type: gb28181_protocol::manscdp::VideoAlarmType,
    ) -> Result<u16> {
        self.report_alarm_full(
            transport,
            local_host,
            local_port,
            description,
            alarm_type,
            None,
        )
        .await
    }

    /// 主动上报一条指定类型与优先级的视频报警。
    pub async fn report_alarm_full(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
        description: &str,
        alarm_type: gb28181_protocol::manscdp::VideoAlarmType,
        priority: Option<u8>,
    ) -> Result<u16> {
        let sn = self.next_cseq();
        // 报警时间用校时后的 ISO8601(与平台时间对齐,见 common::clock)。
        let time = common::clock::synced_iso8601();
        let position = Some(*self.position.lock());
        let mut alarm = gb28181_protocol::manscdp::AlarmNotify::video_typed(
            self.config.device_id.as_str(),
            sn,
            time,
            description,
            alarm_type,
            position,
        );
        if let Some(p) = priority {
            alarm.priority = p;
        }
        let xml = alarm.to_xml()?;

        // 若存在报警订阅对话:在对话内以 SIP NOTIFY 上报(GB28181 订阅语义);
        // 否则(如手动触发、平台未订阅)退化为独立 MESSAGE(全新 Call-ID)。
        let subscription = self.alarm_dialog.lock().await.clone();
        if let Some(AlarmSubscription {
            dialog,
            peer: sub_dst,
            filter,
        }) = subscription
        {
            // 订阅筛选:不满足条件就不推(之前完全忽略订阅条件,全推)。
            if !filter.accepts(alarm.priority, alarm.method, Some(alarm_type as u32)) {
                tracing::info!(
                    priority = alarm.priority,
                    method = alarm.method,
                    alarm_type = alarm_type as u32,
                    "报警不满足平台订阅筛选条件,不上报"
                );
                return Ok(0);
            }
            let cseq = self.next_cseq();
            let req = builder::notify_in_dialog(
                &self.config,
                &dialog,
                cseq,
                local_host,
                local_port,
                &xml,
                transport.protocol(),
            );
            let (mut rx, _route) = sip_core::RouteGuard::register(transport, &dialog.call_id);
            let result = sip_core::client_transact(
                transport.as_ref(),
                sub_dst,
                &req,
                &mut rx,
                sip_core::Timing::default(),
            )
            .await;
            return Ok(result?.status);
        }

        self.send_message_xml(transport, local_host, local_port, &xml)
            .await
    }

    /// 主动上报实时视音频回传通知(VideoUploadNotify,GB28181-2022 A.2.5.8)。
    /// 设备 → 平台独立 MESSAGE,携带当前位置。
    ///
    /// **fire-and-forget**:该通知为告知性,部分平台(如 WVP)不支持此 CmdType 会
    /// 直接丢弃不回 200,故不等待响应,发出即返回,避免无谓的事务重传阻塞。
    pub async fn report_video_upload(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<()> {
        // A.2.5.8 是 2022 新增的通知。附录 I:协议版本更高的一方应避免向对方发送
        // 不能识别的消息 —— 平台声明自己是 2016(或没声明)时就不该发过去。
        if !self.peer_supports_2022() {
            tracing::debug!(
                peer = ?self.peer_protocol_version(),
                "跳过实时视音频回传通知:对端未声明支持 GB/T 28181-2022"
            );
            return Ok(());
        }
        let sn = self.next_cseq();
        let time = common::clock::synced_iso8601();
        let pos = Some(*self.position.lock());
        let notify = gb28181_protocol::manscdp::VideoUploadNotify::new(
            self.config.device_id.as_str(),
            sn,
            time,
            pos,
        );
        let xml = notify.to_xml()?;
        self.send_message_xml_oneshot(transport, local_host, local_port, &xml)
            .await
    }

    /// 主动上报一条移动位置(GPS,MobilePosition NOTIFY)。设备 → 平台。
    /// 位置订阅场景下由周期任务调用;也可手动触发。返回平台响应状态码。
    pub async fn report_position(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
        longitude: f64,
        latitude: f64,
    ) -> Result<u16> {
        // 记录当前坐标,供位置订阅的周期 NOTIFY 复用。
        *self.position.lock() = (longitude, latitude);
        let sn = self.next_cseq();
        // 位置上报时间用校时后的 ISO8601(与平台时间对齐)。
        let time = common::clock::synced_iso8601();
        let notify = gb28181_protocol::manscdp::MobilePositionNotify::new(
            self.config.device_id.as_str(),
            sn,
            time,
            longitude,
            latitude,
        );
        let xml = notify.to_xml()?;
        self.send_message_xml(transport, local_host, local_port, &xml)
            .await
    }

    /// 在订阅对话内向平台回发目录变更通知(Catalog Notify)。
    ///
    /// 平台 `SUBSCRIBE + Catalog` 后调用:把本设备全部通道以 `Event=ON` 上报,
    /// 使平台目录树同步。走对话内 SIP NOTIFY(沿用订阅 Call-ID + 反转 From/To +
    /// Event/Subscription-State 头),`dst` 为平台来源地址。
    async fn notify_catalog(
        &self,
        transport: &Arc<dyn SipTransport>,
        dst: SocketAddr,
        dialog: &builder::NotifyDialog,
        sn: u32,
    ) -> Result<u16> {
        use gb28181_protocol::manscdp::{CatalogNotify, CatalogNotifyItem, CatalogSnapshot};
        // 全量目录:多通道树优先,否则扁平通道。全量后刷新快照基线供后续增量 diff。
        let publishable = self.catalog_publishable();
        let items: Vec<CatalogNotifyItem> = publishable
            .iter()
            .map(|(id, name, status)| CatalogNotifyItem {
                device_id: id.clone(),
                name: name.clone(),
                event: "ON".into(),
                status: status.clone(),
            })
            .collect();
        *self.catalog_snapshot.lock() = CatalogSnapshot::from_items(publishable.iter().cloned());
        let notify = CatalogNotify::new(self.config.device_id.as_str(), sn, items);
        let xml = notify.to_xml()?;

        let cseq = self.next_cseq();
        let req = builder::notify_in_dialog(
            &self.config,
            dialog,
            cseq,
            &self.local_host(),
            self.local_port(),
            &xml,
            transport.protocol(),
        );
        // 对话内 NOTIFY 的响应按订阅 Call-ID 回来;注册该路由收 200,收完注销。
        let (mut rx, _route) = sip_core::RouteGuard::register(transport, &dialog.call_id);
        let result = sip_core::client_transact(
            transport.as_ref(),
            dst,
            &req,
            &mut rx,
            sip_core::Timing::default(),
        )
        .await;
        Ok(result?.status)
    }

    /// 目录变更后推送增量 NOTIFY(FR-32/34)。
    ///
    /// 计算当前目录相对上次快照的 diff(ADD/DEL/UPDATE/ON/OFF),若有变更且存在活跃目录订阅,
    /// 在订阅对话内发一条只含变更项的 Catalog NOTIFY,并刷新快照基线。无订阅/无变更时不发。
    /// CRUD(upsert_channel/remove_channel)或状态切换后调用。
    pub async fn notify_catalog_changed(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
    ) -> Result<()> {
        use gb28181_protocol::manscdp::CatalogSnapshot;
        let next = CatalogSnapshot::from_items(self.catalog_publishable());
        let changes = {
            let snap = self.catalog_snapshot.lock();
            snap.diff(&next)
        };
        if changes.is_empty() {
            return Ok(());
        }
        // 刷新基线(无论有无订阅,快照都应跟上当前目录)。
        *self.catalog_snapshot.lock() = next;

        // 附录 N:攒够 100 条或等满 1 分钟再通知。没到阈值就先缓冲,
        // 由 `flush_catalog_changes`(CRUD 后与心跳节拍上各调一次)兜底发出。
        let (ready, schedule) = {
            let mut pending = self.pending_catalog_changes.lock();
            pending.merge(changes);
            let ready = pending.should_flush();
            // 只在还没安排过延迟任务时安排一次。
            let schedule = !ready && !pending.flush_scheduled;
            if schedule {
                pending.flush_scheduled = true;
            }
            (ready, schedule)
        };
        if ready {
            return self.flush_catalog_changes(transport).await;
        }
        if schedule {
            // 事件驱动而不是全局轮询:没有变更时零开销,压测万级设备也不会
            // 因为一个"目录变更定时器"多出上万次无谓唤醒。
            let dev = Arc::clone(self);
            let tp = Arc::clone(transport);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(CATALOG_NOTIFY_DEBOUNCE).await;
                    let ready = {
                        let pending = dev.pending_catalog_changes.lock();
                        if pending.items.is_empty() {
                            return; // 已被别的路径发走
                        }
                        pending.should_flush()
                    };
                    if ready {
                        if let Err(error) = dev.flush_catalog_changes(&tp).await {
                            tracing::warn!(%error, "目录变更批量通知发送失败");
                        }
                        return;
                    }
                }
            });
        }
        Ok(())
    }

    /// 把缓冲的目录变更立刻发出(附录 N 的批量通知落地)。
    ///
    /// 无目录订阅时只清空缓冲不发 —— 平台会在下次全量查询时同步。
    pub async fn flush_catalog_changes(&self, transport: &Arc<dyn SipTransport>) -> Result<()> {
        use gb28181_protocol::manscdp::CatalogNotify;
        let changes = self.pending_catalog_changes.lock().take();
        if changes.is_empty() {
            return Ok(());
        }
        let dialog = self.catalog_dialog.lock().await.clone();
        let Some((dialog, dst)) = dialog else {
            return Ok(()); // 无目录订阅,平台会在下次全量查询时同步
        };
        let count = changes.len();
        let sn = self.next_cseq();
        let notify = CatalogNotify::new(self.config.device_id.as_str(), sn, changes);
        let xml = notify.to_xml()?;
        tracing::info!(count, "目录变更批量通知(附录 N)");
        let cseq = self.next_cseq();
        let req = builder::notify_in_dialog(
            &self.config,
            &dialog,
            cseq,
            &self.local_host(),
            self.local_port(),
            &xml,
            transport.protocol(),
        );
        let (mut rx, _route) = sip_core::RouteGuard::register(transport, &dialog.call_id);
        let result = sip_core::client_transact(
            transport.as_ref(),
            dst,
            &req,
            &mut rx,
            sip_core::Timing::default(),
        )
        .await;
        result?;
        Ok(())
    }

    /// 启动(或重建)移动位置订阅的周期上报任务。
    ///
    /// 平台下发 `SUBSCRIBE + MobilePosition`(携 Interval)后调用:按 `interval` 秒
    /// 周期把当前坐标以 MobilePosition NOTIFY 上报给平台,直到订阅被替换或设备下线。
    /// `interval` 缺省/为 0 时取 5 秒兜底。同一 CmdType 再次订阅会替换旧任务。
    async fn start_position_subscription(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        interval: Option<u64>,
        expires: u64,
    ) {
        let secs = interval.filter(|s| *s > 0).unwrap_or(5);
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();

        let sim = Arc::clone(self);
        let tp = Arc::clone(transport);
        let local_host = self.local_host();
        let local_port = self.local_port();
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(secs));
            // 订阅到期定时器:之前只把 expires 写进头,从不真的到期,
            // 于是平台早就忘了这个订阅,设备还在无限上报。
            let expiry = tokio::time::sleep(std::time::Duration::from_secs(expires));
            tokio::pin!(expiry);
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break, // 订阅被替换/取消。
                    _ = &mut expiry => {
                        tracing::info!(expires, "移动位置订阅到期,停止周期上报");
                        sim.observer.on_event(common::DeviceEvent::SubscriptionChanged {
                            kind: "MobilePosition".into(),
                            active: false,
                            notify_count: 0,
                        });
                        break;
                    }
                    _ = ticker.tick() => {
                        let (lon, lat) = *sim.position.lock();
                        if let Err(e) = sim
                            .report_position(&tp, &local_host, local_port, lon, lat)
                            .await
                        {
                            tracing::warn!(error=%e, "位置订阅周期上报失败");
                        }
                    }
                }
            }
        });

        // 插入订阅表(替换旧任务:旧 stop_tx 被 drop → 旧任务在下次 select 时退出)。
        let mut subs = self.subscriptions.lock().await;
        subs.insert("MobilePosition".to_string(), Subscription { task, stop_tx });
        tracing::info!(interval = secs, expires, "移动位置订阅已建立,开始周期上报");
    }

    /// 为无周期任务的订阅(Catalog / Alarm)挂一个到期定时器。
    ///
    /// 到期后清理对话并发一条 `Subscription-State: terminated` 的收尾 NOTIFY,
    /// 让平台知道订阅结束而不是"设备突然不吭声了"。
    async fn arm_subscription_expiry(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        kind: &str,
        dialog: builder::NotifyDialog,
        dst: SocketAddr,
        expires: u64,
    ) {
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let sim = Arc::clone(self);
        let tp = Arc::clone(transport);
        let kind_owned = kind.to_string();
        let task = tokio::spawn(async move {
            let expiry = tokio::time::sleep(std::time::Duration::from_secs(expires));
            tokio::pin!(expiry);
            tokio::select! {
                _ = &mut stop_rx => {}
                _ = &mut expiry => {
                    tracing::info!(kind = %kind_owned, expires, "订阅到期");
                    sim.terminate_subscription(&tp, &kind_owned, &dialog, dst).await;
                }
            }
        });
        let mut subs = self.subscriptions.lock().await;
        subs.insert(kind.to_string(), Subscription { task, stop_tx });
        self.observer
            .on_event(common::DeviceEvent::SubscriptionChanged {
                kind: kind.to_string(),
                active: true,
                notify_count: 0,
            });
    }

    /// 终止一个订阅:清对话、停周期任务,并发 `Subscription-State: terminated` NOTIFY。
    ///
    /// 由平台 `SUBSCRIBE Expires: 0`(退订)或到期定时器触发。
    async fn terminate_subscription(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        kind: &str,
        dialog: &builder::NotifyDialog,
        dst: SocketAddr,
    ) {
        // 先摘掉周期任务,避免收尾 NOTIFY 之后还继续上报。
        // 注意:不能在持有 subscriptions 锁时 await 发送,否则与订阅建立路径互相排队。
        let removed = self.subscriptions.lock().await.remove(kind);
        if let Some(sub) = removed {
            let _ = sub.stop_tx.send(());
        }
        match kind {
            "Catalog" => *self.catalog_dialog.lock().await = None,
            "Alarm" => *self.alarm_dialog.lock().await = None,
            _ => {}
        }
        self.observer
            .on_event(common::DeviceEvent::SubscriptionChanged {
                kind: kind.to_string(),
                active: false,
                notify_count: 0,
            });
        // 对话内订阅才需要收尾 NOTIFY(MobilePosition/PTZPosition 走独立 MESSAGE)。
        if matches!(kind, "Catalog" | "Alarm") {
            if let Err(e) = self.notify_terminated(transport, dialog, dst).await {
                tracing::warn!(kind = %kind, error = %e, "订阅终止 NOTIFY 发送失败");
            }
        }
        tracing::info!(kind = %kind, "订阅已终止");
    }

    /// 发一条 `Subscription-State: terminated` 的空 NOTIFY 作为订阅收尾。
    async fn notify_terminated(
        &self,
        transport: &Arc<dyn SipTransport>,
        dialog: &builder::NotifyDialog,
        dst: SocketAddr,
    ) -> Result<()> {
        let cseq = self.next_cseq();
        let mut req = builder::notify_in_dialog(
            &self.config,
            dialog,
            cseq,
            &self.local_host(),
            self.local_port(),
            "",
            transport.protocol(),
        );
        req.headers
            .set("Subscription-State", "terminated;reason=timeout");
        req.body.clear();
        // fire-and-forget:平台可能已经忘了这个订阅并回 481,没必要为此重传。
        transport
            .send_to(&sip_core::SipMessage::Request(req), dst)
            .await
    }

    /// 启动(或重建)PTZ 精准位置事件订阅的周期上报任务(GB28181-2022 §9.11.2)。
    ///
    /// 平台 `SUBSCRIBE + PTZPosition`(携 Interval)后调用:按 interval 秒周期发
    /// PTZ 精准状态 NOTIFY(独立 MESSAGE,复用 PTZPosition 应答体)。真实设备应在姿态
    /// 变化时触发;模拟器按周期上报当前 precise_pose。
    async fn start_ptz_position_subscription(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        interval: Option<u64>,
        expires: u64,
    ) {
        let secs = interval.filter(|s| *s > 0).unwrap_or(5);
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();

        let sim = Arc::clone(self);
        let tp = Arc::clone(transport);
        let local_host = self.local_host();
        let local_port = self.local_port();
        let notify_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counter = notify_count.clone();
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(secs));
            let expiry = tokio::time::sleep(std::time::Duration::from_secs(expires));
            tokio::pin!(expiry);
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    _ = &mut expiry => {
                        tracing::info!(expires, "PTZ 精准位置订阅到期,停止周期上报");
                        sim.observer.on_event(common::DeviceEvent::SubscriptionChanged {
                            kind: "PTZPosition".into(),
                            active: false,
                            notify_count: 0,
                        });
                        break;
                    }
                    _ = ticker.tick() => {
                        if let Err(e) = sim
                            .report_ptz_position(&tp, &local_host, local_port)
                            .await
                        {
                            tracing::warn!(error=%e, "PTZ 精准位置订阅周期上报失败");
                        } else {
                            let n = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                            sim.observer.on_event(common::DeviceEvent::SubscriptionChanged {
                                kind: "PTZPosition".into(),
                                active: true,
                                notify_count: n,
                            });
                        }
                    }
                }
            }
        });

        let mut subs = self.subscriptions.lock().await;
        subs.insert("PTZPosition".to_string(), Subscription { task, stop_tx });
        self.observer
            .on_event(common::DeviceEvent::SubscriptionChanged {
                kind: "PTZPosition".into(),
                active: true,
                notify_count: 0,
            });
        tracing::info!(interval = secs, "PTZ 精准位置订阅已建立,开始周期上报");
    }

    /// 主动上报一条 PTZ 精准状态 NOTIFY(GB28181-2022 §9.11.2)。
    /// 复用 PTZPosition 应答体(Pan/Tilt/Zoom),独立 MESSAGE fire-and-forget。
    pub async fn report_ptz_position(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<()> {
        let sn = self.next_cseq();
        let (pan, tilt, zoom) = self.control_state.lock().precise_pose;
        let notify = gb28181_protocol::manscdp::PtzPreciseStatusResponse::new(
            self.config.device_id.as_str(),
            sn,
            pan,
            tilt,
            zoom,
        );
        let xml = notify.to_xml()?;
        self.send_message_xml_oneshot(transport, local_host, local_port, &xml)
            .await
    }

    /// 停止全部活跃订阅(设备下线/重注册时调,避免残留周期任务)。
    async fn stop_subscriptions(&self) {
        // 先把全部订阅摘出来再逐个等待:持锁 await 会挡住入站请求处理。
        let drained: Vec<Subscription> = {
            let mut subs = self.subscriptions.lock().await;
            subs.drain().map(|(_, sub)| sub).collect()
        };
        for sub in drained {
            let _ = sub.stop_tx.send(());
            await_task("周期 NOTIFY 订阅", sub.task).await;
        }
        *self.alarm_dialog.lock().await = None;
        *self.catalog_dialog.lock().await = None;
    }

    /// 在任何业务分派前统一校验平台身份。
    ///
    /// Compatible 保持既有互操作；Strict 要求恰好一个可解析的 From，且 URI user
    /// 精确匹配平台域 ID。这里不把 Authorization 的存在当成认证成功：当前只实现
    /// 设备作为 UAC 的 REGISTER Digest，没有平台反向凭据与 nonce/replay verifier。
    fn validate_inbound_identity(
        &self,
        req: &sip_core::Request,
    ) -> std::result::Result<(), InboundIdentityRejection> {
        if self.config.inbound_identity_mode == common::InboundIdentityMode::Compatible {
            return Ok(());
        }
        if req.headers.get_all("From").len() != 1 {
            return Err(InboundIdentityRejection {
                status: 400,
                reason: "Bad Request",
                detail: "From 头缺失或重复",
            });
        }
        let Some(platform_id) = req.headers.from_uri_user() else {
            return Err(InboundIdentityRejection {
                status: 400,
                reason: "Bad Request",
                detail: "From URI 无法解析",
            });
        };
        if platform_id != self.config.server_domain {
            return Err(InboundIdentityRejection {
                status: 403,
                reason: "Forbidden",
                detail: "From URI user 与平台域 ID 不匹配",
            });
        }
        Ok(())
    }

    /// 处理一条入站请求(带服务端事务去重)。
    ///
    /// 去重是必须的:UDP 下平台的请求会重传,而重复执行业务逻辑会产生真实故障
    /// (最典型的是重传 INVITE 被回 486 Busy Here,点播失败)。命中已完成事务时
    /// 直接重发缓存响应;上一轮仍在处理时吸收掉。
    pub async fn answer_inbound(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        incoming: &sip_core::Incoming,
    ) -> Result<bool> {
        let sip_core::SipMessage::Request(req) = &incoming.message else {
            return Ok(false);
        };

        let token = match self.server_txns.admit(req, incoming.from) {
            sip_core::Admit::Retransmit(cached) => {
                tracing::info!(
                    method = %req.method,
                    status = cached.status,
                    "入站请求重传,重发缓存响应(不重跑业务逻辑)"
                );
                transport
                    .send_to(&sip_core::SipMessage::Response(cached), incoming.from)
                    .await?;
                return Ok(true);
            }
            sip_core::Admit::Absorb => {
                tracing::debug!(method = %req.method, "入站请求重传,上一轮仍在处理,吸收");
                return Ok(true);
            }
            sip_core::Admit::Proceed(token) => token,
        };

        if let Err(rejection) = self.validate_inbound_identity(req) {
            let count = self.rejected_identity.fetch_add(1, Ordering::Relaxed) + 1;
            if count <= 5 || count % 1000 == 0 {
                tracing::warn!(
                    method = %req.method,
                    from = %incoming.from,
                    status = rejection.status,
                    reason = rejection.detail,
                    count,
                    "严格入站身份校验拒绝 SIP 请求"
                );
            }
            // ACK 不允许产生响应（RFC 3261）；身份错误时只静默丢弃，且绝不进入业务分派。
            if req.method == sip_core::Method::Ack {
                self.server_txns.abandon(&token);
                return Ok(true);
            }
            let response = builder::response_status(req, rejection.status, rejection.reason);
            self.respond(transport, incoming.from, &token, response)
                .await?;
            return Ok(true);
        }

        let result = self
            .dispatch_inbound(transport, req, incoming.from, &token)
            .await;
        // 没有产生最终响应(出错或未处理)时释放事务槽位,否则后续重传会被永久吸收。
        if !matches!(result, Ok(true)) {
            self.server_txns.abandon(&token);
        }
        result
    }

    /// 发送最终响应并登记到服务端事务表(供重传直接命中)。
    async fn respond(
        &self,
        transport: &Arc<dyn SipTransport>,
        dst: SocketAddr,
        token: &sip_core::TxToken,
        resp: sip_core::Response,
    ) -> Result<()> {
        self.server_txns.complete(token, &resp);
        transport
            .send_to(&sip_core::SipMessage::Response(resp), dst)
            .await
    }

    /// 按方法分派入站请求。OPTIONS 回 200 并带 Allow;MESSAGE 解析 XML 查询并应答;
    /// SUBSCRIBE 回 200 并按需启动周期 NOTIFY;未实现的方法回 405。
    ///
    /// 取 `self: &Arc<Self>`(而非 `&self`):移动位置订阅需要把设备句柄交给后台
    /// 周期上报任务,任务生命周期独立于单次入站处理。
    async fn dispatch_inbound(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        req: &sip_core::Request,
        from: SocketAddr,
        token: &sip_core::TxToken,
    ) -> Result<bool> {
        let incoming_from = from;
        {
            match req.method {
                sip_core::Method::Options => {
                    // OPTIONS 应答带 Allow,便于平台探知设备支持的方法集。
                    let mut resp = builder::response_ok(req);
                    resp.headers.set("Allow", sip_core::Method::ALLOW_HEADER);
                    self.respond(transport, incoming_from, token, resp).await?;
                    Ok(true)
                }
                sip_core::Method::Message => {
                    // GB28181 规范:先对查询 MESSAGE 回空 200 OK(事务应答),
                    // 再由设备发一条独立的 MESSAGE 把应答 XML 送回平台(新事务)。
                    self.respond(transport, incoming_from, token, builder::response_ok(req))
                        .await?;

                    // body 可能是查询(Query)或控制(Control)。按**根元素**区分,
                    // 不能靠 parse 成功与否(quick-xml 忽略根名,Query 会误吞 Control)。
                    // 按信令编码解码(GB18030/UTF-8),修复中文乱码。
                    {
                        let body_str =
                            builder::decode_xml_body(&req.body, self.config.signaling_encoding);
                        let body_str = body_str.as_str();
                        // 按**真正解析出的根元素**分派,而不是 `contains("<Control>")`:
                        // 后者可被注释或文本节点里的假标签欺骗,改变设备的处理路径。
                        let envelope = gb28181_protocol::manscdp::parse_envelope(body_str)
                            .unwrap_or_else(|e| {
                                tracing::warn!(error = %e, "MANSCDP 报文无法解析根元素");
                                gb28181_protocol::manscdp::Envelope {
                                    root: String::new(),
                                    cmd_type: String::new(),
                                }
                            });
                        if envelope.root == "Control" {
                            if let Ok(ctrl) = gb28181_protocol::manscdp::Control::parse(body_str) {
                                let xml = self.handle_control(&ctrl).await?;
                                self.send_reply_message(transport, incoming_from, &xml)
                                    .await?;
                                // 平台命令时间线:上报语义摘要(控制类,含方向/变倍/预置位等细节)。
                                self.observer
                                    .on_event(common::DeviceEvent::PlatformCommand {
                                        kind: "control".into(),
                                        summary: format!(
                                            "平台控制:{} → 已应答 OK",
                                            Self::control_detail(&ctrl)
                                        ),
                                    });
                                // 抓拍/升级等需设备主动发 NOTIFY 的命令:回 200/结果后异步执行。
                                self.spawn_control_side_effects(transport, &ctrl);
                            }
                        } else if envelope.cmd_type == "Broadcast" {
                            // 语音广播:回 Broadcast Response(OK/ERROR),接受则反向发 INVITE。
                            if let Ok(bc) =
                                gb28181_protocol::manscdp::BroadcastNotify::parse(body_str)
                            {
                                self.handle_broadcast(transport, &bc, incoming_from).await?;
                                self.observer
                                    .on_event(common::DeviceEvent::PlatformCommand {
                                        kind: "broadcast".into(),
                                        summary: "平台语音广播请求 → 已应答".into(),
                                    });
                            }
                        } else if envelope.root == "Query" || envelope.root == "Notify" {
                            if let Ok(query) = gb28181_protocol::manscdp::Query::parse(body_str) {
                                let max_body = Self::max_body_for(transport.protocol());
                                if let Ok(replies) = self.handle_query_replies(&query, max_body) {
                                    // 附录 M:多条应答串行发送(收到上一条的响应再发下一条)。
                                    self.send_reply_messages_serial(
                                        transport,
                                        incoming_from,
                                        &replies,
                                    )
                                    .await?;
                                    // 平台命令时间线:上报语义摘要(查询类)。
                                    self.observer
                                        .on_event(common::DeviceEvent::PlatformCommand {
                                            kind: "query".into(),
                                            summary: self.query_summary(&query.cmd_type),
                                        });
                                }
                            }
                        } else {
                            tracing::warn!(
                                root = %envelope.root,
                                cmd_type = %envelope.cmd_type,
                                "无法识别的 MANSCDP 报文,已回 200 但不处理"
                            );
                        }
                    }
                    Ok(true)
                }
                sip_core::Method::Invite => {
                    // 解析 SDP,启动推流,回 200 OK。
                    self.handle_invite(transport, req, incoming_from, token)
                        .await
                }
                sip_core::Method::Bye => {
                    // 停止推流,回 200 OK。
                    self.handle_bye(transport, req, incoming_from, token).await
                }
                sip_core::Method::Cancel => {
                    // 取消点播(RFC 3261 §9.2):对 CANCEL 本身回 200,
                    // 同时对被取消的 INVITE 回 487 Request Terminated。
                    // 之前只回 200,平台会一直等 INVITE 的最终响应。
                    self.handle_cancel(transport, req, incoming_from, token)
                        .await
                }
                sip_core::Method::Ack => {
                    // ACK 无需响应,但标志会话已确认。
                    Ok(true)
                }
                sip_core::Method::Subscribe => {
                    // 平台订阅(Catalog/MobilePosition/Alarm):先回 200 OK 建立订阅。
                    // 用固定 to_tag,使 200 的 To-tag 与后续对话内 NOTIFY 的 From-tag 一致。
                    let local_tag = builder::rand_token("");
                    // Expires: 0 是退订(RFC 3265 §3.2.4):回 200 后终止该订阅。
                    let requested_expires = req.headers.expires().unwrap_or(3600);
                    self.respond(
                        transport,
                        incoming_from,
                        token,
                        builder::response_ok_tagged(req, &local_tag),
                    )
                    .await?;

                    {
                        let body_str =
                            builder::decode_xml_body(&req.body, self.config.signaling_encoding);
                        if let Ok(query) = gb28181_protocol::manscdp::Query::parse(&body_str) {
                            // 从 SUBSCRIBE + 本端 tag 构建订阅对话(供对话内 NOTIFY 用)。
                            let dialog = builder::NotifyDialog {
                                call_id: req.headers.call_id().unwrap_or_default().to_string(),
                                local_tag,
                                remote_from: req
                                    .headers
                                    .get("From")
                                    .unwrap_or_default()
                                    .to_string(),
                                event: req
                                    .headers
                                    .get("Event")
                                    .unwrap_or(&query.cmd_type)
                                    .to_string(),
                                expires: requested_expires.min(u32::MAX as u64) as u32,
                            };
                            // Expires: 0 → 退订。终止周期任务、清对话,并发一条
                            // Subscription-State: terminated 的收尾 NOTIFY。
                            if requested_expires == 0 {
                                self.terminate_subscription(
                                    transport,
                                    &query.cmd_type,
                                    &dialog,
                                    incoming_from,
                                )
                                .await;
                                return Ok(true);
                            }
                            match query.cmd_type.as_str() {
                                // 移动位置订阅:启动周期位置 NOTIFY(独立 MESSAGE 路径)。
                                "MobilePosition" => {
                                    self.start_position_subscription(
                                        transport,
                                        query.interval,
                                        requested_expires,
                                    )
                                    .await;
                                }
                                // 目录订阅:记录对话供 CRUD 增量 NOTIFY,并回发一条全量目录 NOTIFY。
                                "Catalog" => {
                                    *self.catalog_dialog.lock().await =
                                        Some((dialog.clone(), incoming_from));
                                    self.arm_subscription_expiry(
                                        transport,
                                        "Catalog",
                                        dialog.clone(),
                                        incoming_from,
                                        requested_expires,
                                    )
                                    .await;
                                    let sim = Arc::clone(self);
                                    let tp = Arc::clone(transport);
                                    let sn = query.sn;
                                    let dst = incoming_from;
                                    let dialog_for_notify = dialog.clone();
                                    // 独立任务发送:notify 走事务会等待平台响应,不占用入站循环。
                                    tokio::spawn(async move {
                                        if let Err(e) = sim
                                            .notify_catalog(&tp, dst, &dialog_for_notify, sn)
                                            .await
                                        {
                                            tracing::warn!(error=%e, "目录订阅通知失败");
                                        }
                                    });
                                }
                                // 报警订阅:记录对话,后续 report_alarm 走对话内 NOTIFY。
                                "Alarm" => {
                                    let filter =
                                        gb28181_protocol::manscdp::AlarmSubscribeFilter::from_query(
                                            &query,
                                        );
                                    tracing::info!(
                                        ?filter,
                                        "报警订阅已建立(按 A.2.4.6 筛选条件过滤上报)"
                                    );
                                    *self.alarm_dialog.lock().await = Some(AlarmSubscription {
                                        dialog: dialog.clone(),
                                        peer: incoming_from,
                                        filter,
                                    });
                                    self.arm_subscription_expiry(
                                        transport,
                                        "Alarm",
                                        dialog,
                                        incoming_from,
                                        requested_expires,
                                    )
                                    .await;
                                }
                                // PTZ 精准位置事件订阅(GB28181-2022 §9.11.1/9.11.2):
                                // 启动周期 PTZ 精准状态 NOTIFY(独立 MESSAGE 路径,与位置订阅一致)。
                                "PTZPosition" => {
                                    self.start_ptz_position_subscription(
                                        transport,
                                        query.interval,
                                        requested_expires,
                                    )
                                    .await;
                                }
                                _ => {}
                            }
                        }
                    }
                    Ok(true)
                }
                sip_core::Method::Info => {
                    // 会话内回放控制(MANSRTSP):PLAY(Scale=N)调速 / 恢复,PAUSE 暂停。
                    self.handle_info(transport, req, incoming_from, token).await
                }
                // REGISTER 是设备发出的,不该由设备处理;PRACK/UPDATE/REFER/PUBLISH 未实现。
                // 静默丢弃会让平台一直重传,按 RFC 3261 §8.2.1 回 405 + Allow。
                method => {
                    tracing::info!(%method, "未实现的入站方法,回 405 Method Not Allowed");
                    let mut resp = builder::response_status(req, 405, "Method Not Allowed");
                    resp.headers.set("Allow", sip_core::Method::ALLOW_HEADER);
                    self.respond(transport, incoming_from, token, resp).await?;
                    Ok(true)
                }
            }
        }
    }

    /// 处理会话内 INFO(MANSRTSP 回放控制):按体首行 PLAY/PAUSE 调整推流。
    ///
    /// 体形如 `PLAY MANSRTSP/1.0\r\nCSeq: n\r\nScale: 2.0\r\n\r\n`(倍速/恢复)
    /// 或 `PAUSE MANSRTSP/1.0\r\nCSeq: n\r\nPauseTime: now\r\n\r\n`(暂停)。
    /// 无活跃会话时也回 200(避免平台重传)。
    async fn handle_info(
        &self,
        transport: &Arc<dyn SipTransport>,
        req: &sip_core::Request,
        from: SocketAddr,
        token: &sip_core::TxToken,
    ) -> Result<bool> {
        if let Ok(body) = std::str::from_utf8(&req.body) {
            let head = body.trim_start();
            let guard = self.session.lock().await;
            if let Some(session) = guard.as_ref() {
                if head.starts_with("PAUSE") {
                    session.control.pause();
                    tracing::info!("回放控制:暂停");
                } else if head.starts_with("PLAY") {
                    // 取 Scale 行(倍速);缺省视为 1.0(恢复正常播放)。
                    let scale = body
                        .lines()
                        .find_map(|l| {
                            l.split_once(':')
                                .filter(|(k, _)| k.trim().eq_ignore_ascii_case("Scale"))
                                .and_then(|(_, v)| v.trim().parse::<f32>().ok())
                        })
                        .unwrap_or(1.0);
                    // 取 Range 行(拖动定位,§9.8):`Range: npt=<start>-<end>`。
                    // start 为回放起点秒偏移;按 INVITE t= 时间段折算千分比位置。
                    if let Some(permille) = Self::parse_range_permille(body) {
                        session.control.seek_permille(permille);
                        tracing::info!(permille, "回放控制:拖动定位");
                    }
                    session.control.set_speed(scale);
                    session.control.resume();
                    tracing::info!(scale, "回放控制:播放/倍速");
                }
            }
        }
        self.respond(transport, from, token, builder::response_ok(req))
            .await?;
        Ok(true)
    }

    /// 解析 MANSRTSP PLAY 的 `Range: npt=<start>-<end>` 头,折算为流内千分比位置(0..=1000)。
    ///
    /// GB28181 回放拖动:npt 为回放起点秒偏移。模拟器循环推短文件,无真实录像时间轴,
    /// 故把 start 对一个名义 3600s 时间窗取模折算千分比(保证拖动可见地改变起播位置)。
    /// 返回 None 表示无 Range 或无法解析。
    fn parse_range_permille(body: &str) -> Option<u32> {
        let range = body.lines().find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.trim().eq_ignore_ascii_case("Range"))
                .map(|(_, v)| v.trim().to_string())
        })?;
        // 形如 "npt=10-" / "npt=10-20" / "npt=now-"。取 start。
        let npt = range.split_once('=').map(|(_, v)| v).unwrap_or(&range);
        let start = npt.split('-').next()?.trim();
        if start.eq_ignore_ascii_case("now") {
            return None; // now 表示从当前继续,不定位。
        }
        let secs: f64 = start.parse().ok()?;
        // 名义 3600s 窗口取模 → 千分比。
        let permille = ((secs.rem_euclid(3600.0) / 3600.0) * 1000.0) as u32;
        Some(permille.min(1000))
    }

    /// 控制命令的详细人类可读摘要(供 UI 命令时间线):方向/变倍/预置位/巡航/辅助等。
    fn control_detail(ctrl: &gb28181_protocol::manscdp::Control) -> String {
        use gb28181_protocol::manscdp::{CruiseOp, PresetAction};
        // PTZ 方向/变倍。
        if let Some(m) = ctrl.ptz_motion() {
            if !m.is_stop() {
                let mut parts = Vec::new();
                if m.up {
                    parts.push("上");
                }
                if m.down {
                    parts.push("下");
                }
                if m.left {
                    parts.push("左");
                }
                if m.right {
                    parts.push("右");
                }
                if m.zoom_in {
                    parts.push("放大");
                }
                if m.zoom_out {
                    parts.push("缩小");
                }
                let dir = if parts.is_empty() {
                    "云台动作".to_string()
                } else {
                    parts.join("+")
                };
                return format!(
                    "PTZ {dir}(水平{} 垂直{} 变倍{})",
                    m.pan_speed, m.tilt_speed, m.zoom_speed
                );
            }
            return "PTZ 停止".into();
        }
        // 预置位。
        if let Some((action, idx)) = ctrl.preset_op() {
            let a = match action {
                PresetAction::Set => "设置",
                PresetAction::Call => "调用",
                PresetAction::Delete => "删除",
            };
            return format!("PTZ {a}预置位 {idx}");
        }
        // 巡航。
        if let Some(op) = ctrl.cruise_op() {
            return match op {
                CruiseOp::SetPoint { track, preset } => format!("巡航{track}增点(预置{preset})"),
                CruiseOp::DelPoint { track, preset } => format!("巡航{track}删点(预置{preset})"),
                CruiseOp::DelTrack { track } => format!("删除整条巡航{track}"),
                CruiseOp::SetSpeed { track, speed } => format!("巡航{track}设速{speed}"),
                CruiseOp::SetDwell { track, dwell } => format!("巡航{track}停留{dwell}s"),
                CruiseOp::Start { track } => {
                    if track == 0 {
                        "巡航停止".into()
                    } else {
                        format!("巡航{track}启动")
                    }
                }
            };
        }
        // 自动扫描(表 A.10)。
        if let Some(op) = ctrl.scan_op() {
            use gb28181_protocol::manscdp::ScanOp;
            return match op {
                ScanOp::Start { group } => format!("自动扫描{group}开始"),
                ScanOp::SetLeftBoundary { group } => format!("自动扫描{group}设左边界"),
                ScanOp::SetRightBoundary { group } => format!("自动扫描{group}设右边界"),
                ScanOp::SetSpeed { group, speed } => format!("自动扫描{group}设速{speed}"),
            };
        }
        // 辅助控制。
        if let Some(aux) = ctrl.aux_op() {
            return format!(
                "辅助{}{}",
                aux.function.label(),
                if aux.on { "开" } else { "关" }
            );
        }
        // FI 光圈/聚焦。
        if let Some(fi) = ctrl.fi_op() {
            return fi.label();
        }
        // 其它:录像/布防/复位/看守位/精准云台/升级/抓拍/配置等,用 kind()。
        if let Some(v) = ctrl.record_cmd.as_deref() {
            return format!("录像控制:{v}");
        }
        if let Some(g) = &ctrl.guard_cmd {
            return format!("布防/撤防:{g}");
        }
        if let Some(p) = &ctrl.ptz_precise_ctrl {
            return format!("精确云台 Pan{} Tilt{} Zoom{}", p.pan, p.tilt, p.zoom);
        }
        ctrl.kind().to_string()
    }

    /// 查询命令的人类可读摘要(供 UI 命令时间线)。
    fn query_summary(&self, cmd_type: &str) -> String {
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
    fn catalog_items(&self) -> Vec<gb28181_protocol::manscdp::CatalogItem> {
        // 不带查询对象时等价于"按本设备编码查询"(附录 J b:整棵树)。
        self.catalog_items_for(self.config.device_id.as_str())
    }

    /// 按**附录 J** 的返回范围规则生成目录项。
    ///
    /// `query_id` 是平台 `<Query>` 里的 `DeviceID`:可以是本设备编码(整树)、
    /// 行政区域编码(前缀匹配)、业务分组/虚拟组织/通道编码(该节点及其后代)。
    fn catalog_items_for(&self, query_id: &str) -> Vec<gb28181_protocol::manscdp::CatalogItem> {
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
    fn catalog_resolution_code(&self) -> String {
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
    fn catalog_encode_type_code(&self) -> String {
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
    fn business_group_of(
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
    fn handle_query_replies(
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
    fn max_body_for(protocol: common::Transport) -> usize {
        match protocol {
            common::Transport::Udp => MAX_MANSCDP_BODY_BYTES,
            common::Transport::Tcp => usize::MAX,
        }
    }

    /// 目录应答:先按整条试算,超过安全阈值才按条目数分片(SumNum 恒为总数)。
    ///
    /// 这样小目录的报文与之前完全一致,只有真会超 MTU 的大目录才分多条。
    fn catalog_replies(
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
    fn record_items_for(
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
    fn handle_query(&self, query: &gb28181_protocol::manscdp::Query) -> Result<String> {
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

    /// 处理设备控制命令(PTZ/关键帧/录像/布防/复位/重启),返回应答 XML。
    /// 语义:模拟器接受并回 Result=OK;强制关键帧会触发当前推流立即发关键帧
    ///(当前 FileSource/LightSource 本就周期性带关键帧,记录日志即可)。
    async fn handle_control(&self, ctrl: &gb28181_protocol::manscdp::Control) -> Result<String> {
        tracing::info!(kind = %ctrl.kind(), sn = ctrl.sn, "收到设备控制");
        if let Some(ptz) = &ctrl.ptz_cmd {
            // PTZ 8 字节码:识别预置位设置/调用/删除并更新预置位表;其余为方向/变倍。
            use gb28181_protocol::manscdp::PresetAction;
            match ctrl.preset_op() {
                Some((PresetAction::Set, idx)) => {
                    let name = ctrl
                        .ptz_cmd_params
                        .as_ref()
                        .and_then(|params| params.preset_name.as_deref())
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("预置位{idx}"));
                    self.presets.lock().insert(idx, name.clone());
                    tracing::info!(preset = idx, preset_name = %name, "预置位设置");
                }
                Some((PresetAction::Delete, idx)) => {
                    self.presets.lock().remove(&idx);
                    tracing::info!(preset = idx, "预置位删除");
                }
                Some((PresetAction::Call, idx)) => {
                    let name = self
                        .presets
                        .lock()
                        .get(&idx)
                        .cloned()
                        .unwrap_or_else(|| format!("预置位{idx}"));
                    tracing::info!(preset = idx, preset_name = %name, "预置位调用(转到)");
                    self.observer
                        .on_event(common::DeviceEvent::PtzPresetCall { preset: idx, name });
                }
                None => {
                    // 优先级:巡航/辅助(0x8x)→ FI 光圈聚焦(0x4x)→ 方向/变倍运动。
                    // 自动扫描(表 A.10)。0x89/0x8A 之前被错当成辅助开关(实际是
                    // 0x8C/0x8D),扫描指令等于被静默吞掉;现在单独一支。
                    if let Some(op) = ctrl.scan_op() {
                        self.apply_scan_op(op);
                    } else if let Some(op) = ctrl.cruise_op() {
                        self.apply_cruise_op(op);
                    } else if let Some(aux) = ctrl.aux_op() {
                        tracing::info!(func = aux.function.label(), on = aux.on, "辅助控制");
                    } else if let Some(fi) = ctrl.fi_op() {
                        tracing::info!(fi = %fi.label(), "FI 光圈/聚焦控制");
                    } else if let Some(m) = ctrl.ptz_motion() {
                        // 方向/变倍运动:解析后上报观察者(供 UI 云台动画)。
                        tracing::info!(ptz = %ptz, up=m.up, down=m.down, left=m.left, right=m.right,
                            zoom_in=m.zoom_in, zoom_out=m.zoom_out, "PTZ 云台运动");
                        self.observer.on_event(common::DeviceEvent::Ptz {
                            up: m.up,
                            down: m.down,
                            left: m.left,
                            right: m.right,
                            zoom_in: m.zoom_in,
                            zoom_out: m.zoom_out,
                            pan_speed: m.pan_speed,
                            tilt_speed: m.tilt_speed,
                            zoom_speed: m.zoom_speed,
                        });
                    } else {
                        tracing::info!(ptz = %ptz, "PTZ 云台控制(模拟接受)");
                    }
                }
            }
        }
        // 精确云台控制(GB-2022):记录姿态供 PTZPosition 读回。
        if let Some(p) = &ctrl.ptz_precise_ctrl {
            tracing::info!(pan = p.pan, tilt = p.tilt, zoom = p.zoom, "精确云台控制");
            self.control_state.lock().precise_pose = (p.pan, p.tilt, p.zoom);
        }
        // 目标跟踪:白名单模式外忽略,仍应答 200/OK。
        if let Some(t) = &ctrl.target_track {
            let mode = t.mode.trim();
            if matches!(mode, "Auto" | "Manual" | "Stop") {
                tracing::info!(mode, object = ?t.object_id, "目标跟踪");
            } else {
                tracing::info!(mode, "目标跟踪(模式未识别,忽略)");
            }
        }
        // 格式化 SD 卡:模拟设备无实际存储,仅记录应答。
        if ctrl.format_sd_card.is_some() {
            let disk = ctrl.disk_num.unwrap_or(0);
            tracing::info!(disk, "格式化 SD 卡(模拟接受)");
        }
        // 录像控制:更新录像态(供 DeviceStatus 的 Record 字段读回)。
        if let Some(v) = ctrl.record_cmd.as_deref() {
            let recording = v.eq_ignore_ascii_case("Record");
            self.control_state.lock().recording = recording;
            tracing::info!(record = %v, recording, "录像控制");
        }
        // 布防/撤防:更新报警值守态(供 AlarmStatus 查询读回)。
        if let Some(g) = &ctrl.guard_cmd {
            let arm = g.eq_ignore_ascii_case("SetGuard");
            self.control_state.lock().alarming = arm;
            tracing::info!(guard = %g, "布防/撤防");
        }
        // 看守位设置:更新启用/已设标志(供 HomePositionQuery 读回)。
        if let Some(hp) = &ctrl.home_position {
            {
                let mut s = self.control_state.lock();
                s.home_enabled = hp.enabled != 0;
                s.home_set = hp.enabled != 0;
            }
            tracing::info!(enabled = hp.enabled, preset = hp.preset_index, "看守位设置");
        }
        if ctrl.iframe_cmd.is_some() {
            // A.2.3.1.7:把请求交给推流循环。实时编码源(屏幕采集 H.264)会真的
            // 让编码器立刻插一个 IDR;文件循环源无法兑现,只能等自带的周期 IDR。
            let requested = match self.session.lock().await.as_ref() {
                Some(session) => {
                    session.control.request_keyframe();
                    true
                }
                None => false,
            };
            if requested {
                tracing::info!("强制关键帧:已下发到推流循环");
            } else {
                tracing::info!("强制关键帧:当前无活跃推流会话,忽略");
            }
            self.observer
                .on_event(common::DeviceEvent::PlatformCommand {
                    kind: "control".into(),
                    summary: if requested {
                        "平台请求强制关键帧:已下发到编码器".into()
                    } else {
                        "平台请求强制关键帧:当前无推流会话".to_string()
                    },
                });
        }
        // DeviceConfig 控制命令(GB28181-2022 A.2.3.2):设备配置修改。
        // 模拟器接受所有配置修改并回 OK,实际不改变设备行为(仅记录日志)。
        // CmdType=DeviceConfig 时应答须为 DeviceConfig(A.2.6.8),而非 DeviceControl。
        let is_device_config = ctrl.cmd_type == "DeviceConfig"
            || ctrl.cfg_basic_param.is_some()
            || ctrl.cfg_video_record_plan.is_some()
            || ctrl.cfg_video_alarm_record.is_some()
            || ctrl.cfg_picture_mask.is_some()
            || ctrl.cfg_frame_mirror.is_some()
            || ctrl.cfg_alarm_report.is_some()
            || ctrl.cfg_osd_config.is_some()
            || ctrl.cfg_video_param_attribute.is_some()
            || ctrl.cfg_svac_encode.is_some()
            || ctrl.cfg_svac_decode.is_some();
        if let Some(bp) = &ctrl.cfg_basic_param {
            // A.2.3.2.1:配置修改命令可只带部分字段,空串/0 表示"这一项不改"。
            let mut applied: Vec<String> = Vec::new();
            {
                let mut rt = self.runtime.lock();
                if !bp.name.trim().is_empty() && bp.name != rt.device_name {
                    applied.push(format!("Name={}", bp.name));
                    rt.device_name = bp.name.trim().to_string();
                }
                if bp.expiration > 0 && bp.expiration != rt.expiration_secs {
                    applied.push(format!("Expiration={}", bp.expiration));
                    rt.expiration_secs = bp.expiration;
                }
                if bp.heartbeat_interval > 0
                    && u64::from(bp.heartbeat_interval) != rt.heartbeat_interval_secs
                {
                    applied.push(format!("HeartBeatInterval={}", bp.heartbeat_interval));
                    rt.heartbeat_interval_secs = u64::from(bp.heartbeat_interval);
                }
                if bp.heartbeat_count > 0 && bp.heartbeat_count != rt.heartbeat_count {
                    applied.push(format!("HeartBeatCount={}", bp.heartbeat_count));
                    rt.heartbeat_count = bp.heartbeat_count;
                }
            }
            if applied.is_empty() {
                tracing::info!("设备配置:基本参数无变化");
            } else {
                tracing::info!(changed = %applied.join(", "), "设备配置:基本参数已生效");
                self.observer
                    .on_event(common::DeviceEvent::PlatformCommand {
                        kind: "control".into(),
                        summary: format!("平台改设备配置已生效:{}", applied.join(", ")),
                    });
            }
        }
        if ctrl.cfg_video_record_plan.is_some() {
            tracing::info!("设备配置:录像计划(模拟接受)");
        }
        if ctrl.cfg_video_alarm_record.is_some() {
            tracing::info!("设备配置:报警录像(模拟接受)");
        }
        if ctrl.cfg_picture_mask.is_some() {
            tracing::info!("设备配置:画面遮挡(模拟接受)");
        }
        if ctrl.cfg_frame_mirror.is_some() {
            tracing::info!("设备配置:画面翻转(模拟接受)");
        }
        if ctrl.cfg_alarm_report.is_some() {
            tracing::info!("设备配置:报警上报开关(模拟接受)");
        }
        if let Some(osd) = &ctrl.cfg_osd_config {
            let time_show = osd.time_show_flag != 0;
            let osd_show = osd.osd_show_flag != 0;
            tracing::info!(time_show, osd_show, "设备配置:前端OSD(已应用)");
            // 上报观察者:供 UI 展示"平台下发 OSD 配置、设备已应用"(国标 A.2.3.2.11)。
            self.observer.on_event(common::DeviceEvent::OsdConfig {
                time_show,
                osd_show,
            });
        }
        let resp = if is_device_config {
            gb28181_protocol::manscdp::ControlResponse::config_ok(&ctrl.device_id, ctrl.sn)
        } else {
            gb28181_protocol::manscdp::ControlResponse::ok(&ctrl.device_id, ctrl.sn)
        };
        resp.to_xml()
    }

    /// 应用自动扫描控制操作(GB/T 28181-2022 表 A.10)。
    fn apply_scan_op(&self, op: gb28181_protocol::manscdp::ScanOp) {
        use gb28181_protocol::manscdp::ScanOp;
        let mut s = self.control_state.lock();
        match op {
            ScanOp::Start { group } => {
                s.scan_groups.entry(group).or_default().scanning = true;
                tracing::info!(group, "自动扫描开始(画面自右向左)");
            }
            ScanOp::SetLeftBoundary { group } => {
                s.scan_groups.entry(group).or_default().left_set = true;
                tracing::info!(group, "设置自动扫描左边界");
            }
            ScanOp::SetRightBoundary { group } => {
                s.scan_groups.entry(group).or_default().right_set = true;
                tracing::info!(group, "设置自动扫描右边界");
            }
            ScanOp::SetSpeed { group, speed } => {
                s.scan_groups.entry(group).or_default().speed = speed;
                tracing::info!(group, speed, "设置自动扫描速度");
            }
        }
    }

    /// 应用巡航轨迹控制操作到设备控制态(供 CruiseTrack* 查询读回)。
    fn apply_cruise_op(&self, op: gb28181_protocol::manscdp::CruiseOp) {
        use gb28181_protocol::manscdp::CruiseOp;
        let mut s = self.control_state.lock();
        match op {
            CruiseOp::SetPoint { track, preset } => {
                let pts = s.cruise_tracks.entry(track).or_default();
                if !pts.contains(&preset) {
                    pts.push(preset);
                }
                tracing::info!(track, preset, "巡航增点");
            }
            CruiseOp::DelPoint { track, preset } => {
                if let Some(pts) = s.cruise_tracks.get_mut(&track) {
                    pts.retain(|&p| p != preset);
                }
                tracing::info!(track, preset, "巡航删点");
            }
            // 表 A.9 注2:字节6=00H 时删除整条巡航。
            CruiseOp::DelTrack { track } => {
                s.cruise_tracks.remove(&track);
                s.cruise_params.remove(&track);
                tracing::info!(track, "巡航删除整条轨迹");
            }
            CruiseOp::SetSpeed { track, speed } => {
                s.cruise_tracks.entry(track).or_default();
                s.cruise_params.entry(track).or_default().0 = speed;
                tracing::info!(track, speed, "巡航设速");
            }
            CruiseOp::SetDwell { track, dwell } => {
                s.cruise_tracks.entry(track).or_default();
                s.cruise_params.entry(track).or_default().1 = dwell;
                tracing::info!(track, dwell, "巡航设停留");
            }
            CruiseOp::Start { track } => {
                if track == 0 {
                    tracing::info!("巡航停止");
                } else {
                    s.cruise_tracks.entry(track).or_default();
                    tracing::info!(track, "巡航启动");
                }
            }
        }
    }

    /// 处理语音广播(§9.12.1 信令 1~4):校验 TargetID,回 Broadcast Response,
    /// 接受则异步走完信令 5~16(设备主动 INVITE → 收流)。
    async fn handle_broadcast(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        bc: &gb28181_protocol::manscdp::BroadcastNotify,
        from: SocketAddr,
    ) -> Result<()> {
        // TargetID 须为本设备或其某通道。
        let owned = bc.target_id == self.config.device_id.as_str()
            || self
                .config
                .channels
                .iter()
                .any(|c| c.channel_id.as_str() == bc.target_id);
        let resp = if owned {
            gb28181_protocol::manscdp::BroadcastResponse::ok(&bc.target_id, bc.sn)
        } else {
            gb28181_protocol::manscdp::BroadcastResponse::error(
                self.config.device_id.as_str(),
                bc.sn,
                "target mismatch",
            )
        };
        // 先回 200 OK(入站已回),再以独立 MESSAGE 发 Broadcast Response(信令 3)。
        let xml = resp.to_xml()?;
        self.send_reply_message(transport, from, &xml).await?;

        if !owned {
            tracing::warn!(target = %bc.target_id, "语音广播 TargetID 不属本设备,拒绝");
            return Ok(());
        }

        // 信令 5 起是一次完整的 UAC 事务 + 收流,放后台跑:不能占住入站分派循环,
        // 否则这段时间里平台的其它命令(含本次广播的 200 OK)都进不来。
        let dev = Arc::clone(self);
        let tp = Arc::clone(transport);
        let source_id = bc.source_id.clone();
        let target_id = bc.target_id.clone();
        tokio::spawn(async move {
            if let Err(error) = dev.start_broadcast(&tp, &source_id, &target_id, from).await {
                tracing::warn!(%error, "语音广播建流失败");
                dev.observer.on_event(common::DeviceEvent::RuntimeError {
                    scope: "broadcast".into(),
                    message: error.to_string(),
                });
            }
        });
        Ok(())
    }

    /// 语音广播的设备侧主叫流程(§9.12.1 图27 信令 5、14、15)。
    ///
    /// 顺序很关键:**先绑收流端口,再发 INVITE**。SDP 的 `m=audio <port>` 必须是真实端口,
    /// 之前的实现填 0 —— 等于告诉平台"往 0 端口发",链路永远建不起来。
    async fn start_broadcast(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        source_id: &str,
        target_id: &str,
        peer: SocketAddr,
    ) -> Result<()> {
        // 同一时刻只维持一路广播:平台重复下发广播通知时,先拆掉旧的再建新的。
        self.stop_broadcast(transport, "新的广播请求").await;

        let local_ip: std::net::IpAddr = self
            .local_host()
            .parse()
            .unwrap_or_else(|_| transport.local_addr().map(|a| a.ip()).unwrap_or(peer.ip()));
        let receiver = media_rtp::AudioReceiver::bind(local_ip).await?;
        let local_port = receiver.local_port()?;
        let stats = receiver.stats();

        let ssrc = rand::random::<u32>() & 0x00FF_FFFF;
        let offer = sip_core::build_broadcast_offer(target_id, local_ip, local_port, ssrc, false);

        // 附录 L:媒体流发送者是平台侧的语音流发送者(SourceID),接收者是本设备(TargetID)。
        let seq = self.media_seq.fetch_add(1, Ordering::Relaxed);
        let subject = sip_core::Subject::realtime(source_id, seq, target_id, seq);

        let remote_uri = format!("sip:{}@{}", source_id, self.config.server_domain);
        let dialog = builder::OutboundDialog::new(remote_uri, self.next_cseq());
        let (mut rx, _route) = sip_core::RouteGuard::register(transport, &dialog.call_id);
        let req = builder::invite(
            &self.config,
            &dialog,
            &self.local_host(),
            self.local_port(),
            &subject,
            &offer,
            transport.protocol(),
        );
        tracing::info!(%source_id, %target_id, local_port, subject = %subject,
            "语音广播:发出 INVITE(信令 5)");

        let resp = sip_core::client_transact(
            transport.as_ref(),
            peer,
            &req,
            &mut rx,
            sip_core::Timing::default(),
        )
        .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                return Err(e);
            }
        };
        if resp.status != 200 {
            return Err(Error::Sip(format!(
                "语音广播 INVITE 被拒:{} {}",
                resp.status, resp.reason
            )));
        }

        // 学到平台的 To-tag 后才能发合法的 ACK / 会话内 BYE。
        let mut dialog = dialog;
        dialog.remote_tag = resp.headers.to_tag().map(|t| t.to_string());
        let ack = builder::ack(
            &self.config,
            &dialog,
            &self.local_host(),
            self.local_port(),
            transport.protocol(),
        );
        transport
            .send_to(&sip_core::SipMessage::Request(ack), peer)
            .await?;

        // 平台 200 OK 的 SDP 只用于诊断:UDP 下我们是被动收流方,不需要连对端。
        if let Ok(text) = std::str::from_utf8(&resp.body) {
            if let Ok(sdp) = sip_core::SessionDescription::parse(text) {
                tracing::info!(
                    sender = %sdp.connection.addr, port = sdp.media.port,
                    ssrc = ?sdp.media.ssrc, "语音广播:平台应答 SDP"
                );
            }
        }

        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let dev = Arc::clone(self);
        let watch_stats = Arc::clone(&stats);
        let task = tokio::spawn(async move {
            let result = receiver
                .run(async {
                    let _ = stop_rx.await;
                })
                .await;
            if let Err(error) = result {
                tracing::warn!(%error, "语音广播收流异常结束");
            }
            let snap = watch_stats.snapshot();
            tracing::info!(
                packets = snap.packets,
                bytes = snap.payload_bytes,
                lost = snap.lost,
                "语音广播收流结束"
            );
            dev.observer.on_event(common::DeviceEvent::PlatformCommand {
                kind: "broadcast".into(),
                summary: format!(
                    "语音广播结束:收 {} 包 / {} 字节,丢 {}",
                    snap.packets, snap.payload_bytes, snap.lost
                ),
            });
        });

        let call_id = dialog.call_id.clone();
        *self.broadcast.lock().await = Some(BroadcastSession {
            dialog,
            peer,
            source_id: source_id.to_string(),
            local_port,
            stats,
            stop_tx,
            task,
        });
        self.observer
            .on_event(common::DeviceEvent::PlatformCommand {
                kind: "broadcast".into(),
                summary: format!("语音广播建流成功:本端 {local_port} 端口收 G.711 音频"),
            });
        tracing::info!(%call_id, local_port, "语音广播:ACK 已发,开始收音频");
        Ok(())
    }

    /// 结束语音广播会话。`reason` 非空时发会话内 BYE 通知平台。
    ///
    /// 返回是否确实结束了一路会话。
    async fn stop_broadcast(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        reason: &str,
    ) -> bool {
        let Some(session) = self.broadcast.lock().await.take() else {
            return false;
        };
        let _ = session.stop_tx.send(());
        // 等收流任务真的退出再发 BYE:否则 BYE 之后收流端口还在,
        // 平台如果继续推几个包会打到一个"已经不该存在"的端口上。
        await_task("语音广播收流", session.task).await;
        transport.unregister(&session.dialog.call_id);
        // 设备主动挂断要发 BYE(附录 K a):否则平台侧那条媒体链路一直挂着。
        let bye = builder::bye_outbound(
            &self.config,
            &session.dialog,
            self.next_cseq(),
            &self.local_host(),
            self.local_port(),
            transport.protocol(),
        );
        if let Err(error) = transport
            .send_to(&sip_core::SipMessage::Request(bye), session.peer)
            .await
        {
            tracing::warn!(%error, "语音广播 BYE 发送失败");
        }
        tracing::info!(reason, source = %session.source_id, "语音广播已结束并发出 BYE");
        true
    }

    /// 当前语音广播会话快照(供前端展示收流统计与电平)。
    pub async fn broadcast_snapshot(&self) -> Option<BroadcastSnapshot> {
        let guard = self.broadcast.lock().await;
        guard.as_ref().map(|s| BroadcastSnapshot {
            source_id: s.source_id.clone(),
            local_port: s.local_port,
            call_id: s.dialog.call_id.clone(),
            recv: s.stats.snapshot(),
        })
    }

    /// 当前推流会话的媒体遥测快照(发送统计 + 平台 RTCP 反馈)。
    ///
    /// 无活跃推流会话时返回 None;会话刚建立、发送通道还没就绪时也返回 None。
    pub async fn media_telemetry(&self) -> Option<MediaTelemetrySnapshot> {
        let guard = self.session.lock().await;
        let session = guard.as_ref()?;
        let telemetry = session.control.telemetry()?;
        let (sent_packets, sent_bytes, sent_rtcp, send_errors) = telemetry.stats.full_snapshot();
        Some(MediaTelemetrySnapshot {
            is_history: session.is_history,
            media_source: session.media_source.clone(),
            local_rtcp_port: telemetry.local_rtcp_port,
            sent_packets,
            sent_bytes,
            sent_rtcp,
            send_errors,
            // age 的原点必须是发送通道建立时刻(与 RtcpFeedback 内部同一刻度)。
            feedback: telemetry
                .feedback
                .snapshot(telemetry.clock_hz, telemetry.elapsed_ms()),
        })
    }

    /// 设备侧主动挂断语音广播(界面上的"挂断"按钮)。
    ///
    /// 会按附录 K a) 发会话内 BYE 通知平台释放链路。返回是否确实挂断了一路会话。
    pub async fn hangup_broadcast(self: &Arc<Self>, transport: &Arc<dyn SipTransport>) -> bool {
        let stopped = self.stop_broadcast(transport, "设备侧主动挂断").await;
        if stopped {
            self.observer
                .on_event(common::DeviceEvent::PlatformCommand {
                    kind: "broadcast".into(),
                    summary: "设备主动挂断语音广播并已发 BYE".into(),
                });
        }
        stopped
    }

    /// 平台发来的 BYE 是否属于当前广播会话;是则结束收流并返回 true。
    async fn stop_broadcast_if_matches(&self, call_id: &str) -> bool {
        let mut guard = self.broadcast.lock().await;
        let matches = guard.as_ref().is_some_and(|s| s.dialog.call_id == call_id);
        if matches {
            if let Some(session) = guard.take() {
                let _ = session.stop_tx.send(());
                await_task("语音广播收流", session.task).await;
                tracing::info!(%call_id, "平台 BYE 结束语音广播会话");
            }
        }
        matches
    }

    /// 触发需设备主动 NOTIFY/上报的控制副作用(在线升级 4 步进度、抓拍上传)。
    /// 已在入站循环里回过 200/结果,这里异步执行不阻塞应答。
    fn spawn_control_side_effects(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        ctrl: &gb28181_protocol::manscdp::Control,
    ) {
        // 在线升级:4 步进度 NOTIFY。
        if let Some(up) = &ctrl.device_upgrade {
            let dev = Arc::clone(self);
            let tp = Arc::clone(transport);
            let up = up.clone();
            tokio::spawn(async move {
                dev.run_upgrade_progress(&tp, &up).await;
            });
        }
        // 抓拍配置(GB-2022):HTTP 上传 + 完成 NOTIFY。
        if let Some(cfg) = &ctrl.snap_shot_config {
            if cfg.is_valid() {
                let dev = Arc::clone(self);
                let tp = Arc::clone(transport);
                let cfg = cfg.clone();
                tokio::spawn(async move {
                    dev.run_snapshot_upload(&tp, &cfg).await;
                });
            } else {
                tracing::warn!("SnapShotConfig 缺 SessionID/UploadURL,忽略");
            }
        }
        // 抓拍(旧路径):经 Alarm Notify 上报。
        if ctrl.snap_shot_cmd.is_some() {
            let dev = Arc::clone(self);
            let tp = Arc::clone(transport);
            tokio::spawn(async move {
                let (h, p) = (dev.local_host(), dev.local_port());
                let _ = dev.report_alarm(&tp, &h, p, "SnapShot").await;
            });
        }
    }

    /// 在线升级 4 步进度:percent [0,30,60,100],步间 1.5s,发 DeviceUpgradeResult NOTIFY。
    async fn run_upgrade_progress(
        &self,
        transport: &Arc<dyn SipTransport>,
        up: &gb28181_protocol::manscdp::DeviceUpgrade,
    ) {
        let (h, p) = (self.local_host(), self.local_port());
        for percent in [0u8, 30, 60, 100] {
            let sn = self.next_cseq() & 0xFFFF;
            let notify = gb28181_protocol::manscdp::DeviceUpgradeResultNotify::new(
                self.config.device_id.as_str(),
                sn,
                &up.session_id,
                &up.firmware,
                percent,
            );
            if let Ok(xml) = notify.to_xml() {
                let _ = self.send_message_xml(transport, &h, p, &xml).await;
            }
            tracing::info!(percent, "在线升级进度");
            let step = match percent {
                0 => 1,
                30 => 2,
                60 => 3,
                _ => 4,
            };
            self.observer.on_event(common::DeviceEvent::Progress {
                kind: "upgrade".into(),
                current: step,
                total: 4,
                percent: percent as u32,
            });
            if percent < 100 {
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            }
        }
    }

    /// 抓拍上传(GB-2022):串行拍 N 张,每张 HTTP PUT 上传后发完成 NOTIFY。
    /// 模拟设备无摄像头,上传一段占位 JPEG 字节。
    async fn run_snapshot_upload(
        &self,
        transport: &Arc<dyn SipTransport>,
        cfg: &gb28181_protocol::manscdp::SnapShotConfig,
    ) {
        let (h, p) = (self.local_host(), self.local_port());
        let num = cfg.clamped_num();
        // 占位 JPEG(SOI + EOI),真实设备为编码帧。
        let jpeg: &[u8] = &[0xFF, 0xD8, 0xFF, 0xD9];
        for idx in 1..=num {
            let ts = common::clock::synced_iso8601();
            let snap_id = format!("{}_{idx}", ts.replace([':', '-'], "").replace('.', ""));
            let url = if cfg.upload_url.ends_with('/') {
                format!("{}{snap_id}.jpg", cfg.upload_url)
            } else {
                cfg.upload_url.clone()
            };
            match http_put_jpeg(&url, jpeg).await {
                Ok(()) => {
                    let sn = self.next_cseq();
                    let notify = gb28181_protocol::manscdp::SnapShotNotify::new(
                        self.config.device_id.as_str(),
                        sn,
                        &cfg.session_id,
                        &snap_id,
                        &ts,
                        &url,
                    );
                    if let Ok(xml) = notify.to_xml() {
                        let _ = self.send_message_xml(transport, &h, p, &xml).await;
                    }
                    tracing::info!(snap_id, "抓拍上传成功");
                }
                Err(e) => tracing::warn!(error = %e, url, "抓拍上传失败"),
            }
            self.observer.on_event(common::DeviceEvent::Progress {
                kind: "snapshot".into(),
                current: idx,
                total: num,
                percent: idx * 100 / num,
            });
            if idx < num && cfg.interval > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(cfg.interval as u64)).await;
            }
        }
    }

    /// 向平台发送一条应答/通知 MESSAGE(查询应答、控制应答、位置通知等复用)。
    /// 不等响应 —— 单条应答不值得为它开一条独立 Call-ID 去等 200。
    async fn send_reply_message(
        &self,
        transport: &Arc<dyn SipTransport>,
        to: SocketAddr,
        xml: &str,
    ) -> Result<()> {
        let cseq = self.next_cseq();
        let reply = builder::message_xml(
            &self.config,
            &self.ids,
            cseq,
            &self.local_host(),
            self.local_port(),
            xml,
            transport.protocol(),
        );
        transport
            .send_to(&sip_core::SipMessage::Request(reply), to)
            .await
    }

    /// **串行**发送多条应答 MESSAGE(GB/T 28181-2022 附录 M)。
    ///
    /// 标准原文:"多条响应、通知消息发送时宜采用串行发送方式,记录发送方需收到上一条
    /// SIP Message/Notify 消息的 SIP 响应后再进行后续发送处理。"
    ///
    /// 为什么这条要求是有意义的:UDP 下一次性把十几条目录应答喷出去,平台侧的接收
    /// 缓冲很容易溢出,现象是"目录时多时少"。等上一条的 200 再发下一条,相当于用
    /// 应答做流控。
    ///
    /// 单条应答不值得为它开一条独立 Call-ID 去等响应,直接发。
    async fn send_reply_messages_serial(
        &self,
        transport: &Arc<dyn SipTransport>,
        to: SocketAddr,
        replies: &[String],
    ) -> Result<()> {
        if replies.len() <= 1 {
            if let Some(xml) = replies.first() {
                self.send_reply_message(transport, to, xml).await?;
            }
            return Ok(());
        }
        let (host, port) = (self.local_host(), self.local_port());
        for (index, xml) in replies.iter().enumerate() {
            // 用等响应的发送路径:它每条用独立 Call-ID,事务结束即注销路由。
            match self.send_message_xml(transport, &host, port, xml).await {
                Ok(status) => {
                    if !(200..300).contains(&status) {
                        tracing::warn!(
                            index,
                            total = replies.len(),
                            status,
                            "多响应传输:平台对某条应答回了非 2xx,继续发后续分片"
                        );
                    }
                }
                Err(error) => {
                    // 平台不回 200 时不能就此放弃剩余分片:目录残缺比慢更糟。
                    tracing::warn!(
                        index,
                        total = replies.len(),
                        %error,
                        "多响应传输:等上一条应答的响应失败,继续发后续分片"
                    );
                }
            }
        }
        Ok(())
    }

    /// 处理 INVITE:解析平台 SDP,启动推流,回 200 OK(带本端 SDP)。
    async fn handle_invite(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        req: &sip_core::Request,
        from: SocketAddr,
        token: &sip_core::TxToken,
    ) -> Result<bool> {
        use std::net::IpAddr;

        // 解析平台 SDP(提取 RTP 目标地址与端口)。
        let body_str = std::str::from_utf8(&req.body)
            .map_err(|_| Error::Sip("INVITE body 非 UTF-8".into()))?;
        let platform_sdp = sip_core::SessionDescription::parse(body_str)?;
        // 打完整 SDP,便于诊断 TCP 方向(active/passive)/端口/SSRC 等推流问题。
        tracing::info!(sdp = %body_str.replace('\r', "").replace('\n', " | "), "INVITE:平台完整 SDP");
        tracing::info!(proto=%platform_sdp.media.proto, "INVITE SDP 媒体协议");
        let rtp_host: IpAddr = platform_sdp
            .connection
            .addr
            .parse()
            .map_err(|_| Error::Sip("SDP c= 地址非法".into()))?;
        let rtp_port = platform_sdp.media.port;
        let rtp_dst = SocketAddr::new(rtp_host, rtp_port);

        // SSRC 必须用平台 SDP 的 y= 行指定值(GB28181),平台据此识别本条流。
        // 缺失时退化为设备 ID hash。
        let ssrc = platform_sdp.media.ssrc.unwrap_or_else(|| {
            self.config
                .device_id
                .as_str()
                .bytes()
                .fold(0u32, |a, b| a.wrapping_add(b as u32))
        });
        tracing::info!(%rtp_dst, ssrc, "INVITE:开始向平台推流");

        // 附录 K c):用 Subject 的"媒体流发送者 ID:发送方媒体流序列号"作媒体源标识,
        // 判断自己是否已在发同一路码流。**已在发就应释放旧链路、按新请求重建**,
        // 而不是回 486 —— 早先"有会话就 486 Busy Here"的实现会让平台重启后
        // 永远拉不到流:旧会话在设备侧还挂着,新请求一律被拒。
        let subject = req
            .headers
            .get("Subject")
            .and_then(sip_core::Subject::parse);
        let media_source = subject.as_ref().map(|s| s.media_source());
        {
            let mut guard = self.session.lock().await;
            match guard.as_ref() {
                None => {}
                Some(active) => {
                    let same_source = match (&media_source, &active.media_source) {
                        // Subject 都在且媒体源标识相同 → 同一路码流的重新请求。
                        (Some(new), Some(old)) => new == old,
                        // 任一侧没有 Subject:退化为"同一通道的重新点播即视为同源"。
                        // 平台不带 Subject 时(不合标准但现网存在)也要能恢复,
                        // 否则一次丢包就把这个通道永久锁死。
                        _ => true,
                    };
                    if same_source {
                        tracing::info!(
                            ?media_source,
                            "附录 K c):同一媒体源的重新请求,释放旧链路后按新请求重建"
                        );
                        let old = guard.take();
                        drop(guard);
                        if let Some(old) = old {
                            let _ = old.stop_tx.send(());
                        }
                        self.observer.on_event(common::DeviceEvent::StreamStop);
                    } else {
                        // 真的是另一路码流:模拟器同时只维持一路,按 SIP 语义回 486。
                        drop(guard);
                        tracing::info!(?media_source, "已有其它媒体源的会话,回 486 Busy Here");
                        self.respond(
                            transport,
                            from,
                            token,
                            builder::response_status(req, 486, "Busy Here"),
                        )
                        .await?;
                        return Ok(true);
                    }
                }
            }
        }
        // 实时设备启动可能涉及 TCC 授权和最多 5 秒首帧等待,先发 100 防止
        // 平台在准备期间把事务误判为无响应。
        let trying = sip_core::SipMessage::Response(builder::response_status(req, 100, "Trying"));
        transport.send_to(&trying, from).await?;

        // 传输模式:平台 SDP 的 m= proto 含 "TCP" 则走 RTP over TCP。
        // 主被动角色按 `a=setup` 取反(RFC 4145):平台 active → 设备 passive。
        let use_tcp = platform_sdp.is_tcp();
        let local_setup = platform_sdp.local_tcp_setup();
        let local_passive = use_tcp && local_setup == sip_core::Setup::Passive;

        // 语音对讲的上行方向:平台只请求 `m=audio`(§9.12.2 a))。
        // 这条流按附录 C.2.4 走**裸 G.711 而非 PS**,和视频点播是两套封装,
        // 继续沿用视频路径会把音频塞进 PS,平台侧解不出来。
        if platform_sdp.is_audio_only() {
            return self
                .handle_audio_invite(
                    transport,
                    req,
                    from,
                    token,
                    rtp_dst,
                    ssrc,
                    use_tcp,
                    local_setup,
                    local_passive,
                    media_source,
                )
                .await;
        }

        // 按配置选择视频源:实时采集(live:) > 文件(C档) > 轻量伪流(B档) > 不推流(A档)。
        let fps = self.config.video_fps;
        let source: Option<Box<dyn media_rtp::VideoSource>> = if let Some(ref path) =
            self.config.video_source
        {
            let path = path.clone();
            // 视频源构建是**同步阻塞**的(ffmpeg 转封装、摄像头授权、最长 5 秒首帧),
            // 直接在异步上下文里调用会占住 runtime 工作线程。
            let built =
                tokio::task::spawn_blocking(move || -> Result<Box<dyn media_rtp::VideoSource>> {
                    if path.starts_with("live:") {
                        // 实时源由 LiveSource 严格解析完整 URI;legacy live:0 明确映射主屏幕。
                        media_rtp::LiveSource::capture(&path, fps)
                            .map(|s| Box::new(s) as Box<dyn media_rtp::VideoSource>)
                            .map_err(|e| Error::Media(format!("实时采集失败: {e}")))
                    } else {
                        // 含音频轨的文件走音视频复合流(from_path_av);裸流/无音频自动退化为纯视频。
                        media_rtp::FileSource::from_path_av(&path, fps)
                            .map(|s| Box::new(s) as Box<dyn media_rtp::VideoSource>)
                            .map_err(|e| Error::Media(format!("加载视频源失败: {e}")))
                    }
                })
                .await
                .map_err(|e| Error::Media(format!("视频源构建任务异常: {e}")))?;
            match built {
                Ok(s) => Some(s),
                Err(e) => {
                    // 采集/加载失败发生在点播(INVITE)这条异步流程里,不经 start_device
                    // 返回值。必须推 RuntimeError 事件,否则前端一片安静、只见收流超时。
                    self.observer.on_event(common::DeviceEvent::RuntimeError {
                        scope: "capture".into(),
                        message: e.to_string(),
                    });
                    self.respond(
                        transport,
                        from,
                        token,
                        builder::response_status(req, 480, "Temporarily Unavailable"),
                    )
                    .await?;
                    return Ok(true);
                }
            }
        } else {
            self.config
                .light_bitrate_kbps
                .map(|kbps| Box::new(media_rtp::LightSource::new(kbps, fps)) as _)
        };

        // 预先绑定本端媒体端口,使 200 OK 的 SDP 能写**真实端口**
        // (SDP 里 m= 端口 0 表示该媒体被禁用,之前恒填 0)。
        let prepared = if source.is_some() {
            match media_rtp::PreparedMedia::prepare(rtp_dst, use_tcp, local_passive).await {
                Ok(p) => Some(p),
                Err(e) => {
                    self.observer.on_event(common::DeviceEvent::RuntimeError {
                        scope: "stream".into(),
                        message: format!("本端媒体端口绑定失败: {e}"),
                    });
                    self.respond(
                        transport,
                        from,
                        token,
                        builder::response_status(req, 500, "Server Internal Error"),
                    )
                    .await?;
                    return Ok(true);
                }
            }
        } else {
            None
        };
        let local_rtp_port = prepared.as_ref().map(|p| p.local_port()).unwrap_or(0);

        // 先构造并发送 200 OK(带本端 SDP,SSRC 回显平台值)。
        // TCP-PASSIVE 下平台收到 200 后才开始监听,故推流必须在 200 之后再连接。
        let local_ip: std::net::IpAddr = self
            .local_host()
            .parse()
            .unwrap_or_else(|_| transport.local_addr().map(|a| a.ip()).unwrap_or(rtp_host));
        // o= 用户名填被点播的通道 ID(取 INVITE Request-URI 的 user 部分)。
        let channel = req
            .uri
            .strip_prefix("sip:")
            .and_then(|s| s.split('@').next())
            .unwrap_or(self.config.device_id.as_str());
        let local_sdp = sip_core::SessionDescription::new_device_response_with_setup(
            channel,
            local_ip,
            local_rtp_port,
            ssrc,
            use_tcp,
            use_tcp.then_some(local_setup),
        );
        let sdp_body = local_sdp.to_string();

        let mut resp = builder::response_ok(req);
        resp.headers.set("Content-Type", "application/sdp");
        resp.body = sdp_body.as_bytes().to_vec();
        // 会话内主动发 BYE 需要本端 To-tag:必须取自实际发出的那条 200 OK,
        // 自己另生成一个的话平台会认不出这条对话。
        let local_tag = resp
            .headers
            .to_tag()
            .map(|t| t.to_string())
            .unwrap_or_default();
        let dialog = builder::InboundDialog {
            call_id: req.headers.get("Call-ID").unwrap_or_default().trim().into(),
            local_tag,
            remote_from: req.headers.get("From").unwrap_or_default().trim().into(),
            peer: from,
            request_uri: req.uri.clone(),
        };
        self.respond(transport, from, token, resp).await?;

        // 200 OK 已发,启动推流(TCP 模式给平台一点时间建监听 + 收 ACK)。
        // 带回放控制:回放时平台可经会话内 INFO 调整倍速/暂停,直播则保持 1.0x。
        if let (Some(source), Some(prepared)) = (source, prepared) {
            let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
            let (start_tx, start_rx) = tokio::sync::oneshot::channel();
            let control = media_rtp::PlaybackControl::new();
            // 录像下载会话(s=Download + a=downloadspeed:N):按 N 倍速推流。
            if platform_sdp.is_download() {
                if let Some(spd) = platform_sdp.download_speed {
                    control.set_speed(spd as f32);
                    tracing::info!(speed = spd, "INVITE:录像下载模式,按倍速推流");
                }
            }
            let control_task = Arc::clone(&control);
            let simulator = Arc::clone(self);
            let transport_task = Arc::clone(transport);
            let dialog_task = dialog.clone();
            let marker = Arc::new(());
            let task_marker = Arc::clone(&marker);
            let task = tokio::spawn(async move {
                // 确保 PushSession 已写入槽位后再允许任务结束并清理，避免启动失败竞态。
                if start_rx.await.is_err() {
                    return;
                }
                if use_tcp && !local_passive {
                    // TCP-ACTIVE:给平台一点时间在收到 200 后建好监听。
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                }
                let result = media_rtp::push_stream_prepared(
                    prepared,
                    source,
                    rtp_dst,
                    ssrc,
                    fps,
                    control_task,
                    async {
                        let _ = stop_rx.await;
                    },
                )
                .await;
                if let Err(error) = &result {
                    tracing::warn!(%error, "推流结束(错误)");
                    simulator
                        .observer
                        .on_event(common::DeviceEvent::RuntimeError {
                            scope: "stream".into(),
                            message: error.to_string(),
                        });
                    // 附录 K a):监测到媒体流丢失应释放链路,并**通过会话内 BYE 通知**对端。
                    // 只发事件不发 BYE 的话,平台侧那条链路会一直挂着等流。
                    simulator
                        .notify_media_loss(&transport_task, &dialog_task)
                        .await;
                }
                let mut session = simulator.session.lock().await;
                let owns_slot = session
                    .as_ref()
                    .is_some_and(|active| Arc::ptr_eq(&active.marker, &task_marker));
                if owns_slot {
                    *session = None;
                }
                drop(session);
                if owns_slot {
                    simulator.observer.on_event(common::DeviceEvent::StreamStop);
                }
            });
            *self.session.lock().await = Some(PushSession {
                marker,
                task,
                stop_tx,
                control,
                is_history: platform_sdp.is_playback() || platform_sdp.is_download(),
                media_source,
                dialog,
            });
            self.observer.on_event(common::DeviceEvent::StreamStart);
            let _ = start_tx.send(());
        }
        let mode = if platform_sdp.is_download() {
            "下载"
        } else if platform_sdp.is_playback() {
            "回放"
        } else {
            "点播"
        };
        self.observer
            .on_event(common::DeviceEvent::PlatformCommand {
                kind: "invite".into(),
                summary: format!("平台{mode} INVITE → 200 OK,开始推流"),
            });
        Ok(true)
    }

    /// 处理**纯音频** INVITE:语音对讲的设备→平台上行(§9.12.2 a) + 附录 C.2.4)。
    ///
    /// 与视频点播的区别:SDP 回 `m=audio ... 8` + `a=rtpmap:8 PCMA/8000`,
    /// 媒体走裸 G.711A RTP(PT=8),不做 PS 封装。
    #[allow(clippy::too_many_arguments)]
    async fn handle_audio_invite(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        req: &sip_core::Request,
        from: SocketAddr,
        token: &sip_core::TxToken,
        rtp_dst: SocketAddr,
        ssrc: u32,
        use_tcp: bool,
        local_setup: sip_core::Setup,
        local_passive: bool,
        media_source: Option<String>,
    ) -> Result<bool> {
        // 音源:复用已配置的媒体源(文件音轨 / 摄像头麦克风)。没有音源时
        // push_audio_stream 会发 A-law 静音并在统计里如实标注。
        let fps = self.config.video_fps;
        let source: Option<Box<dyn media_rtp::VideoSource>> = if let Some(ref path) =
            self.config.video_source
        {
            let path = path.clone();
            let built =
                tokio::task::spawn_blocking(move || -> Result<Box<dyn media_rtp::VideoSource>> {
                    if path.starts_with("live:") {
                        media_rtp::LiveSource::capture(&path, fps)
                            .map(|s| Box::new(s) as Box<dyn media_rtp::VideoSource>)
                            .map_err(|e| Error::Media(format!("实时采集失败: {e}")))
                    } else {
                        media_rtp::FileSource::from_path_av(&path, fps)
                            .map(|s| Box::new(s) as Box<dyn media_rtp::VideoSource>)
                            .map_err(|e| Error::Media(format!("加载音频源失败: {e}")))
                    }
                })
                .await
                .map_err(|e| Error::Media(format!("音频源构建任务异常: {e}")))?;
            match built {
                Ok(s) => Some(s),
                Err(e) => {
                    // 音源起不来不该让对讲整体失败:仍建流发静音,但把原因报上去。
                    tracing::warn!(error = %e, "对讲上行音源构建失败,将发送静音");
                    self.observer.on_event(common::DeviceEvent::RuntimeError {
                        scope: "capture".into(),
                        message: format!("对讲音源不可用,已发送静音: {e}"),
                    });
                    None
                }
            }
        } else {
            None
        };

        let prepared =
            match media_rtp::PreparedMedia::prepare(rtp_dst, use_tcp, local_passive).await {
                Ok(p) => p,
                Err(e) => {
                    self.observer.on_event(common::DeviceEvent::RuntimeError {
                        scope: "stream".into(),
                        message: format!("对讲媒体端口绑定失败: {e}"),
                    });
                    self.respond(
                        transport,
                        from,
                        token,
                        builder::response_status(req, 500, "Server Internal Error"),
                    )
                    .await?;
                    return Ok(true);
                }
            };
        let local_rtp_port = prepared.local_port();
        let local_ip: std::net::IpAddr = self.local_host().parse().unwrap_or_else(|_| {
            transport
                .local_addr()
                .map(|a| a.ip())
                .unwrap_or(rtp_dst.ip())
        });
        let channel = req
            .uri
            .strip_prefix("sip:")
            .and_then(|s| s.split('@').next())
            .unwrap_or(self.config.device_id.as_str());
        let local_sdp = sip_core::SessionDescription::new_device_audio_response(
            channel,
            local_ip,
            local_rtp_port,
            ssrc,
            use_tcp,
            use_tcp.then_some(local_setup),
        );

        let mut resp = builder::response_ok(req);
        resp.headers.set("Content-Type", "application/sdp");
        resp.body = local_sdp.to_string().as_bytes().to_vec();
        let local_tag = resp
            .headers
            .to_tag()
            .map(|t| t.to_string())
            .unwrap_or_default();
        let dialog = builder::InboundDialog {
            call_id: req.headers.get("Call-ID").unwrap_or_default().trim().into(),
            local_tag,
            remote_from: req.headers.get("From").unwrap_or_default().trim().into(),
            peer: from,
            request_uri: req.uri.clone(),
        };
        self.respond(transport, from, token, resp).await?;

        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let simulator = Arc::clone(self);
        let transport_task = Arc::clone(transport);
        let dialog_task = dialog.clone();
        let marker = Arc::new(());
        let task_marker = Arc::clone(&marker);
        let task = tokio::spawn(async move {
            if start_rx.await.is_err() {
                return;
            }
            if use_tcp && !local_passive {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
            let result = media_rtp::push_audio_stream(prepared, source, rtp_dst, ssrc, async {
                let _ = stop_rx.await;
            })
            .await;
            match &result {
                Ok(summary) => {
                    if summary.silence_only() {
                        // 说清楚"通道通了、但没有真实音源",避免误判为对讲正常。
                        simulator
                            .observer
                            .on_event(common::DeviceEvent::PlatformCommand {
                                kind: "invite".into(),
                                summary: format!(
                                    "对讲上行结束:全程静音({} 包),设备未配置音源",
                                    summary.silence_packets
                                ),
                            });
                    }
                    if let Some(error) = &summary.source_error {
                        simulator
                            .observer
                            .on_event(common::DeviceEvent::RuntimeError {
                                scope: "stream".into(),
                                message: error.clone(),
                            });
                        simulator
                            .notify_media_loss(&transport_task, &dialog_task)
                            .await;
                    }
                }
                Err(error) => {
                    simulator
                        .observer
                        .on_event(common::DeviceEvent::RuntimeError {
                            scope: "stream".into(),
                            message: error.to_string(),
                        });
                    simulator
                        .notify_media_loss(&transport_task, &dialog_task)
                        .await;
                }
            }
            let mut session = simulator.session.lock().await;
            let owns_slot = session
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(&active.marker, &task_marker));
            if owns_slot {
                *session = None;
            }
            drop(session);
            if owns_slot {
                simulator.observer.on_event(common::DeviceEvent::StreamStop);
            }
        });
        *self.session.lock().await = Some(PushSession {
            marker,
            task,
            stop_tx,
            control: media_rtp::PlaybackControl::new(),
            is_history: false,
            media_source,
            dialog,
        });
        self.observer.on_event(common::DeviceEvent::StreamStart);
        let _ = start_tx.send(());
        self.observer
            .on_event(common::DeviceEvent::PlatformCommand {
                kind: "invite".into(),
                summary: format!(
                    "平台对讲 INVITE(纯音频)→ 200 OK,本端 {local_rtp_port} 端口发 G.711A"
                ),
            });
        Ok(true)
    }

    /// 附录 K a):媒体流丢失后释放链路并通过**会话内 BYE** 通知平台。
    ///
    /// 设备是被叫,From/To 相对原 INVITE 反转(见 [`builder::bye_inbound`])。
    async fn notify_media_loss(
        &self,
        transport: &Arc<dyn SipTransport>,
        dialog: &builder::InboundDialog,
    ) {
        let bye = builder::bye_inbound(
            &self.config,
            dialog,
            self.next_cseq(),
            &self.local_host(),
            self.local_port(),
            transport.protocol(),
        );
        match transport
            .send_to(&sip_core::SipMessage::Request(bye), dialog.peer)
            .await
        {
            Ok(()) => tracing::info!(
                call_id = %dialog.call_id,
                "附录 K a):媒体流丢失,已发会话内 BYE 通知平台释放链路"
            ),
            Err(error) => tracing::warn!(%error, "媒体流丢失通知 BYE 发送失败"),
        }
    }

    /// 处理 BYE:停止推流或语音广播收流,回 200 OK。回放/下载会话结束后补发 MediaStatus 121。
    async fn handle_bye(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        req: &sip_core::Request,
        from: SocketAddr,
        token: &sip_core::TxToken,
    ) -> Result<bool> {
        // 先看这条 BYE 是不是在拆语音广播会话(§9.12.1 信令 17/18):
        // 广播的 Call-ID 是设备自己生成的,和点播会话不是同一条对话,不能混着停。
        let call_id = req.headers.get("Call-ID").unwrap_or_default().trim();
        if self.stop_broadcast_if_matches(call_id).await {
            self.respond(transport, from, token, builder::response_ok(req))
                .await?;
            self.observer
                .on_event(common::DeviceEvent::PlatformCommand {
                    kind: "broadcast".into(),
                    summary: "平台 BYE 结束语音广播".into(),
                });
            return Ok(true);
        }

        let (was_history, had_session) = self.stop_active_session().await;
        if had_session {
            self.observer.on_event(common::DeviceEvent::StreamStop);
        }

        self.respond(transport, from, token, builder::response_ok(req))
            .await?;

        // 回放/下载会话结束:补发 MediaStatus 121(历史媒体文件发送结束)。
        if was_history {
            let (h, p) = (self.local_host(), self.local_port());
            let sn = self.next_cseq();
            if let Ok(xml) = gb28181_protocol::manscdp::MediaStatusNotify::finished(
                self.config.device_id.as_str(),
                sn,
            )
            .to_xml()
            {
                let _ = self.send_message_xml(transport, &h, p, &xml).await;
            }
            tracing::info!("回放/下载结束,发 MediaStatus 121");
        }
        Ok(true)
    }

    /// 停掉当前活跃会话,返回 (是否历史会话, 之前是否有会话)。
    ///
    /// 单独抽出来是为了**不在持锁期间 await 网络发送**:BYE/CANCEL 都只需要瞬时
    /// 摘掉会话槽位,后续应答与通知在锁外做。
    async fn stop_active_session(&self) -> (bool, bool) {
        let mut sess_guard = self.session.lock().await;
        let was_history = sess_guard.as_ref().map(|s| s.is_history).unwrap_or(false);
        let had_session = if let Some(session) = sess_guard.take() {
            let _ = session.stop_tx.send(()); // 触发停流
            true
        } else {
            false
        };
        (was_history, had_session)
    }

    /// 设备下线时释放所有媒体链路并**通知平台**(附录 K b))。
    ///
    /// 标准要求:监测到媒体流接收方/发送方故障或离线时应主动释放媒体链路。设备自己
    /// 主动下线就是"发送方离线"这一种,只停本地推流是不够的 —— 平台那条链路会一直
    /// 挂着等流,直到它自己的丢流监测超时。
    async fn release_media_on_shutdown(self: &Arc<Self>, transport: &Arc<dyn SipTransport>) {
        // 点播/回放会话:设备是被叫,发反转的会话内 BYE。
        let taken = {
            let mut guard = self.session.lock().await;
            guard.take().map(|session| {
                let _ = session.stop_tx.send(());
                (session.dialog, session.task)
            })
        };
        if let Some((dialog, task)) = taken {
            // 先等推流任务真的停下再发 BYE:否则 BYE 之后还可能飞出几个 RTP 包,
            // 平台侧会看到"已挂断的会话仍在收流"。
            await_task("推流会话", task).await;
            self.observer.on_event(common::DeviceEvent::StreamStop);
            self.notify_media_loss(transport, &dialog).await;
        }
        // 语音广播:设备是主叫,发正向的会话内 BYE。
        self.stop_broadcast(transport, "设备下线").await;
    }

    /// 处理 CANCEL(RFC 3261 §9.2)。
    ///
    /// 正确行为是**两条响应**:对 CANCEL 本身回 200 OK,同时对被取消的 INVITE 回
    /// 487 Request Terminated。之前只回 200,平台会一直等 INVITE 的最终响应,
    /// 表现为"点播取消后平台界面卡在加载中"。
    async fn handle_cancel(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        req: &sip_core::Request,
        from: SocketAddr,
        token: &sip_core::TxToken,
    ) -> Result<bool> {
        let (_, had_session) = self.stop_active_session().await;
        if had_session {
            self.observer.on_event(common::DeviceEvent::StreamStop);
        }

        // 1) 对 CANCEL 回 200 OK。
        self.respond(transport, from, token, builder::response_ok(req))
            .await?;

        // 2) 对原 INVITE 回 487。CANCEL 与被取消的 INVITE 共享 Via branch / CSeq 序号,
        //    只是 CSeq 方法不同,据此构造 487 的事务标识头。
        let mut terminated = builder::response_status(req, 487, "Request Terminated");
        if let Some(cseq_num) = req.headers.cseq_number() {
            terminated.headers.set("CSeq", format!("{cseq_num} INVITE"));
        }
        transport
            .send_to(&sip_core::SipMessage::Response(terminated), from)
            .await?;
        tracing::info!("CANCEL:已回 200 并对原 INVITE 回 487");
        Ok(true)
    }

    /// 运行设备:注册 → 启动入站应答(OPTIONS)→ 周期心跳,直到 `shutdown` 完成。
    ///
    /// 心跳连续失败 `max_hb_fail` 次判定掉线,触发重注册(指数退避 backoff→2×,上限 backoff_max)。
    /// 这是设备"保持在线"的主循环(docs/10-functional/device-simulation.md §3.2/§3.3)。
    pub async fn run(
        self: Arc<Self>,
        transport: Arc<dyn SipTransport>,
        local_host: String,
        local_port: u16,
        shutdown: impl std::future::Future<Output = ()>,
    ) {
        use std::time::Duration;
        tokio::pin!(shutdown);

        // 记录本端信令地址(应答 MESSAGE / NOTIFY 构造 Via/From/Contact 用)。
        let _ = self.local_addr.set((local_host.clone(), local_port));

        // 入站请求应答任务:注册设备 ID + 全部通道 ID(点播 INVITE 的
        // Request-URI 是通道 ID,需一并注册才能收到)。
        let mut aors = vec![self.config.device_id.as_str().to_string()];
        aors.extend(
            self.config
                .channels
                .iter()
                .map(|c| c.channel_id.as_str().to_string()),
        );
        // 多通道目录树的视频通道 ID 也要注册,否则平台点播子通道的 INVITE 收不到(问题1)。
        {
            let tree = self.catalog_tree.lock();
            aors.extend(
                tree.iter()
                    .filter(|n| {
                        n.node_type == gb28181_protocol::id_codec::CatalogNodeType::VideoChannel
                    })
                    .map(|n| n.id.clone()),
            );
        }
        aors.sort();
        aors.dedup();
        // 保留一份注册清单:下线时必须**逐条**注销。
        // 之前只注销了设备 ID,通道条目永久留在路由表里 ——
        // 反复启停一台 16 通道 NVR,每轮多留一批(压测启停时更明显)。
        let registered_aors = aors.clone();
        let mut inbound = transport.register_inbound_many(aors);
        let inbound_sim = self.clone();
        let inbound_tp = transport.clone();
        let inbound_task = tokio::spawn(async move {
            while let Some(inc) = inbound.recv().await {
                if let sip_core::SipMessage::Request(r) = &inc.message {
                    tracing::info!(method=%r.method, uri=%r.uri, "收到入站请求");
                }
                if let Err(e) = inbound_sim.answer_inbound(&inbound_tp, &inc).await {
                    tracing::warn!(error=%e, "入站请求应答失败");
                }
            }
        });

        // 初次注册(失败则退避重试)。
        // §9.1.1 c):注册重试间隔可调,一般不应短于 60s。压测场景会调小。
        let backoff_base = Duration::from_secs(1);
        let backoff_max = Duration::from_secs(self.config.register_retry_max_secs.max(1));
        let mut backoff = backoff_base;
        loop {
            // **注册本身也必须能被停止打断**。之前只有"退避等待"在 select 里,
            // 而 `register()` 内部是一次完整的 SIP 客户端事务:平台不可达时会按
            // Timer A 重传直到 Timer B(约 32 秒)才返回 Err。于是用户点"停止"后
            // 界面要僵住半分钟,压测批量停止更是逐台叠加。
            let registered = tokio::select! {
                _ = &mut shutdown => None,
                r = self.register(&transport, &local_host, local_port) => Some(r),
            };
            match registered {
                // 注册成功,进入心跳主循环。
                Some(Ok(_)) => break,
                Some(Err(_)) => {}
                None => {
                    // 注册进行中被要求停止:这条早退路径以前既不注销路由也不清理,
                    // 于是"启动失败后重试"会一路累积 AOR 条目。
                    inbound_task.abort();
                    transport.unregister_inbound_many(&registered_aors);
                    return;
                }
            }
            tokio::select! {
                _ = &mut shutdown => {
                    inbound_task.abort();
                    transport.unregister_inbound_many(&registered_aors);
                    return;
                }
                _ = tokio::time::sleep(backoff) => {
                    backoff = (backoff * 2).min(backoff_max);
                }
            }
        }
        tracing::info!(device=%self.config.device_id, "注册成功");

        // 心跳主循环 + 注册续约(FR-32)。
        // 续约周期以**平台授予的 Expires** 为准(取其 80%),而不是硬编码 3600×80%。
        // 平台只给 120s 时,按 2880s 续约会被判掉线。
        let mut heartbeat_interval =
            Duration::from_secs(self.config.heartbeat_interval_secs.max(1));
        let mut current_expires = self.granted_expires();
        let mut renew_ticker = tokio::time::interval(renew_interval(current_expires));
        renew_ticker.reset(); // 跳过启动即触发
        let mut max_hb_fail;
        let mut hb_fail = 0u32;
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                _ = renew_ticker.tick() => {
                    // 到期前主动续约,避免掉线后被动重注册。
                    tracing::info!(
                        device=%self.config.device_id,
                        expires = current_expires,
                        "注册续约(平台授予有效期的 80%)"
                    );
                    let _ = self.register(&transport, &local_host, local_port).await;
                    // 平台可能在续约时改授新的有效期,跟随调整续约周期。
                    let granted = self.granted_expires();
                    if granted != current_expires {
                        current_expires = granted;
                        renew_ticker = tokio::time::interval(renew_interval(current_expires));
                        renew_ticker.reset();
                    }
                }
                _ = tokio::time::sleep(heartbeat_interval) => {
                    // 每拍重读:平台经 DeviceConfig/BasicParam 改过的心跳周期与
                    // 超时次数要真的生效,而不是沿用启动时的快照。
                    {
                        let rt = self.runtime.lock();
                        heartbeat_interval = Duration::from_secs(rt.heartbeat_interval_secs.max(1));
                        max_hb_fail = rt.heartbeat_count.max(1);
                    }
                    match self.send_keepalive(&transport, &local_host, local_port).await {
                        Ok(200) => hb_fail = 0,
                        _ => {
                            hb_fail += 1;
                            if hb_fail >= max_hb_fail {
                                tracing::warn!(device=%self.config.device_id, "心跳连续失败,重注册");
                                hb_fail = 0;
                                // 平台此时已把设备当离线,gauge 要跟上。
                                self.set_online(false);
                                let mut b = backoff_base;
                                while self.register(&transport, &local_host, local_port).await.is_err() {
                                    tokio::select! {
                                        _ = &mut shutdown => { inbound_task.abort(); return; }
                                        _ = tokio::time::sleep(b) => { b = (b * 2).min(backoff_max); }
                                    }
                                }
                                let granted = self.granted_expires();
                                if granted != current_expires {
                                    current_expires = granted;
                                    renew_ticker = tokio::time::interval(renew_interval(current_expires));
                                    renew_ticker.reset();
                                }
                            }
                        }
                    }
                }
            }
        }
        inbound_task.abort();
        self.set_online(false);
        self.stop_subscriptions().await;
        // 附录 K b):设备下线前先释放媒体链路并发会话内 BYE,别让平台干等着收流。
        self.release_media_on_shutdown(&transport).await;
        // 主动向平台注销(REGISTER Expires=0,§9.1.2.2):平台立即置离线,不等心跳超时。
        if let Err(e) = self.unregister(&transport, &local_host, local_port).await {
            tracing::warn!(device=%self.config.device_id, error=%e, "注销失败(下线仍继续)");
        } else {
            tracing::info!(device=%self.config.device_id, "已向平台注销");
        }
        transport.unregister_inbound_many(&registered_aors);
        let (txn_routes, aor_routes) = transport.route_counts();
        tracing::debug!(
            device = %self.config.device_id,
            txn_routes,
            aor_routes,
            "设备已下线,路由已注销"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sip_core::{Headers, Method, Request, Response, SipMessage, UdpTransport};

    /// 经真实 UDP 接收循环注入请求,而不是手工伪造 `Incoming`。
    ///
    /// 这样单测同时覆盖来源限流与反放大字节信用；若直接构造 `Incoming`,随后发送的
    /// Response 理应被传输层视为"没有对应入站请求"并阻断。
    async fn deliver_udp_request(
        device: &Arc<dyn SipTransport>,
        platform: &Arc<UdpTransport>,
        request: Request,
    ) -> sip_core::Incoming {
        let aor = request
            .uri
            .strip_prefix("sip:")
            .and_then(|rest| rest.split(['@', ';', '>']).next())
            .filter(|value| !value.is_empty())
            .expect("测试请求 URI 应含 AOR")
            .to_string();
        let mut inbound = device.register_inbound(&aor);
        platform
            .send_to(
                &SipMessage::Request(request),
                device.local_addr().expect("设备 UDP 地址"),
            )
            .await
            .expect("注入平台请求");
        tokio::time::timeout(std::time::Duration::from_secs(2), inbound.recv())
            .await
            .expect("设备未收到测试请求")
            .expect("设备入站路由提前关闭")
    }

    fn change(id: &str, event: &str, status: &str) -> gb28181_protocol::manscdp::CatalogNotifyItem {
        gb28181_protocol::manscdp::CatalogNotifyItem {
            device_id: id.into(),
            name: format!("Ch-{id}"),
            event: event.into(),
            status: status.into(),
        }
    }

    #[tokio::test]
    async fn 子任务超时后被真正取消而非_detach() {
        struct DropProbe(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::Release);
            }
        }

        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe = DropProbe(Arc::clone(&dropped));
        let task = tokio::spawn(async move {
            let _probe = probe;
            std::future::pending::<()>().await;
        });
        assert!(
            !await_task_with_timeout("取消测试", task, Duration::from_millis(20)).await,
            "超时路径应报告非正常完成"
        );
        assert!(
            dropped.load(std::sync::atomic::Ordering::Acquire),
            "abort 后 future 必须被 drop,资源所有权链才能收敛"
        );
    }

    #[test]
    fn 附录n_同一通道的变更只保留最新一条() {
        let mut pending = PendingCatalogChanges::default();
        pending.merge(vec![change("1", "ADD", "ON")]);
        pending.merge(vec![change("1", "OFF", "OFF")]);
        pending.merge(vec![change("1", "ON", "ON")]);
        assert_eq!(
            pending.items.len(),
            1,
            "上线→下线→上线三条推给平台只是噪声,平台关心终态"
        );
        assert_eq!(pending.items[0].event, "ON");
        // 不同通道各占一条,且保持发生顺序。
        pending.merge(vec![change("2", "ADD", "ON"), change("3", "ADD", "ON")]);
        let ids: Vec<&str> = pending.items.iter().map(|c| c.device_id.as_str()).collect();
        assert_eq!(ids, vec!["1", "2", "3"]);
    }

    #[test]
    fn 附录n_攒够条数立刻发出() {
        let mut pending = PendingCatalogChanges::default();
        // 空缓冲永远不发。
        assert!(!pending.should_flush());
        let batch: Vec<_> = (0..CATALOG_NOTIFY_BATCH_ITEMS - 1)
            .map(|i| change(&i.to_string(), "ADD", "ON"))
            .collect();
        pending.merge(batch);
        assert!(
            !pending.should_flush(),
            "差一条到阈值,且去抖窗口未过,应继续攒"
        );
        pending.merge(vec![change("999", "ADD", "ON")]);
        assert_eq!(pending.items.len(), CATALOG_NOTIFY_BATCH_ITEMS);
        assert!(pending.should_flush(), "到 100 条就该立刻通知");
    }

    #[test]
    fn 附录n_去抖窗口过后发出并清空() {
        let mut pending = PendingCatalogChanges::default();
        pending.merge(vec![change("1", "UPDATE", "ON")]);
        assert!(!pending.should_flush());
        // 把"最后一条变更"的时刻往前拨到去抖窗口之外。
        pending.last = Some(std::time::Instant::now() - CATALOG_NOTIFY_DEBOUNCE);
        assert!(pending.should_flush());
        let taken = pending.take();
        assert_eq!(taken.len(), 1);
        assert!(pending.items.is_empty());
        assert!(pending.since.is_none() && pending.last.is_none());
        assert!(
            !pending.flush_scheduled,
            "取走后要允许下一批变更重新安排延迟任务"
        );
        assert!(!pending.should_flush());
    }

    #[test]
    fn 附录n_变更不断时靠硬上限兜底() {
        let mut pending = PendingCatalogChanges::default();
        pending.merge(vec![change("1", "ADD", "ON")]);
        // 模拟"变更持续不断":第一条已等满上限,但去抖一直被最新变更重置。
        pending.since = Some(std::time::Instant::now() - CATALOG_NOTIFY_MAX_DELAY);
        pending.last = Some(std::time::Instant::now());
        assert!(pending.items.len() < CATALOG_NOTIFY_BATCH_ITEMS);
        assert!(pending.should_flush(), "去抖被反复重置也不能无限期攒着不发");
    }

    #[test]
    fn 附录n_硬上限与附录n示例值一致() {
        assert_eq!(CATALOG_NOTIFY_BATCH_ITEMS, 100);
        assert_eq!(CATALOG_NOTIFY_MAX_DELAY, std::time::Duration::from_secs(60));
        assert!(
            CATALOG_NOTIFY_DEBOUNCE < CATALOG_NOTIFY_MAX_DELAY,
            "去抖窗口必须小于硬上限,否则上限永远先到"
        );
    }

    fn test_cfg(host: &str, port: u16) -> DeviceConfig {
        DeviceConfig {
            device_id: DeviceId::new("34020000001320000001").unwrap(),
            username: "34020000001320000001".into(),
            password: "12345678".into(),
            server_host: host.into(),
            server_port: port,
            server_domain: "34020000002000000001".into(),
            inbound_identity_mode: common::InboundIdentityMode::Compatible,
            transport: Transport::Udp,
            heartbeat_interval_secs: 60,
            channels: vec![],
            device_info: DeviceInfo {
                device_name: "Test Device".into(),
                manufacturer: "UVP".into(),
                model: "Sim".into(),
                firmware: "0.1.0".into(),
            },
            video_source: None,
            video_fps: 25,
            light_bitrate_kbps: None,
            register_expires_secs: DEFAULT_REGISTER_EXPIRES,
            register_retry_max_secs: DEFAULT_REGISTER_RETRY_MAX_SECS,
            gb_version: common::GbVersion::V2022,
            signaling_encoding: common::SignalingEncoding::Gb18030,
        }
    }

    fn inbound_request(method: Method, call_id: &str, from: Option<&str>) -> Request {
        let mut headers = Headers::new();
        headers.set(
            "Via",
            format!("SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bK-{call_id}"),
        );
        if let Some(from) = from {
            headers.set("From", from);
        }
        headers.set("To", "<sip:34020000001320000001@3402>");
        headers.set("Call-ID", call_id);
        headers.set("CSeq", format!("1 {}", method.as_str()));
        Request {
            method,
            uri: "sip:34020000001320000001@127.0.0.1".into(),
            headers,
            body: Vec::new(),
        }
    }

    async fn answer_and_status(
        sim: &Arc<DeviceSimulator>,
        device: &Arc<dyn SipTransport>,
        platform: &Arc<UdpTransport>,
        request: Request,
    ) -> Option<u16> {
        let call_id = request.headers.call_id().unwrap().to_string();
        let mut responses = platform.register(&call_id);
        let incoming = deliver_udp_request(device, platform, request).await;
        sim.answer_inbound(device, &incoming).await.unwrap();
        match tokio::time::timeout(Duration::from_millis(500), responses.recv()).await {
            Ok(Some(sip_core::Incoming {
                message: SipMessage::Response(response),
                ..
            })) => Some(response.status),
            _ => None,
        }
    }

    #[tokio::test]
    async fn 入站身份模式严格校验_from_且_ack_静默拒绝() {
        let device_impl = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let device: Arc<dyn SipTransport> = device_impl;
        let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let mut cfg = test_cfg("127.0.0.1", platform.local_addr().unwrap().port());
        cfg.inbound_identity_mode = common::InboundIdentityMode::Strict;
        let sim = Arc::new(DeviceSimulator::new(cfg));

        assert_eq!(
            answer_and_status(
                &sim,
                &device,
                &platform,
                inbound_request(
                    Method::Options,
                    "strict-ok",
                    Some("<sip:34020000002000000001@3402>;tag=p")
                ),
            )
            .await,
            Some(200)
        );
        assert_eq!(
            answer_and_status(
                &sim,
                &device,
                &platform,
                inbound_request(Method::Options, "strict-missing", None),
            )
            .await,
            Some(400)
        );
        assert_eq!(
            answer_and_status(
                &sim,
                &device,
                &platform,
                inbound_request(
                    Method::Options,
                    "strict-malformed",
                    Some("Platform <sip:bad@3402")
                ),
            )
            .await,
            Some(400)
        );
        assert_eq!(
            answer_and_status(
                &sim,
                &device,
                &platform,
                inbound_request(
                    Method::Options,
                    "strict-wrong",
                    Some("<sip:attacker@3402>;tag=p")
                ),
            )
            .await,
            Some(403)
        );
        assert_eq!(
            answer_and_status(
                &sim,
                &device,
                &platform,
                inbound_request(Method::Ack, "strict-ack", Some("<sip:attacker@3402>;tag=p")),
            )
            .await,
            None,
            "身份错误的 ACK 必须静默丢弃"
        );
    }

    #[tokio::test]
    async fn 严格身份拒绝发生在控制命令副作用之前() {
        let device_impl = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let device: Arc<dyn SipTransport> = device_impl;
        let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let mut cfg = test_cfg("127.0.0.1", platform.local_addr().unwrap().port());
        cfg.inbound_identity_mode = common::InboundIdentityMode::Strict;
        let sim = Arc::new(DeviceSimulator::new(cfg));
        let mut request = inbound_request(
            Method::Message,
            "strict-no-side-effect",
            Some("<sip:attacker@3402>;tag=p"),
        );
        request.body = b"<Control><CmdType>DeviceControl</CmdType><SN>2</SN><DeviceID>34020000001320000001</DeviceID><RecordCmd>Record</RecordCmd></Control>".to_vec();

        assert_eq!(
            answer_and_status(&sim, &device, &platform, request).await,
            Some(403)
        );
        assert!(
            !sim.control_state.lock().recording,
            "错误身份 MESSAGE 不得修改设备控制状态"
        );
    }

    #[tokio::test]
    async fn 入站身份兼容模式保持旧平台互操作() {
        let device_impl = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let device: Arc<dyn SipTransport> = device_impl;
        let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let sim = Arc::new(DeviceSimulator::new(test_cfg(
            "127.0.0.1",
            platform.local_addr().unwrap().port(),
        )));
        assert_eq!(
            answer_and_status(
                &sim,
                &device,
                &platform,
                inbound_request(Method::Options, "compatible-missing", None),
            )
            .await,
            Some(200)
        );
    }

    /// mock 平台:收到首个 REGISTER 回 401(带挑战),第二个回 200。
    async fn mock_platform_register(
        platform: Arc<dyn SipTransport>,
        device_addr: SocketAddr,
        call_id: String,
    ) {
        let mut prx = platform.register(&call_id);
        let mut seen = 0;
        let mut first_via: Option<String> = None;
        let mut first_from: Option<String> = None;
        let mut first_cseq: Option<u32> = None;
        while let Some(inc) = prx.recv().await {
            if let SipMessage::Request(req) = inc.message {
                let cseq = req.headers.cseq().unwrap_or("1 REGISTER").to_string();
                let via = req
                    .headers
                    .get("Via")
                    .expect("REGISTER 缺少 Via")
                    .to_string();
                let from = req
                    .headers
                    .get("From")
                    .expect("REGISTER 缺少 From")
                    .to_string();
                let cseq_number = req.headers.cseq_number().expect("REGISTER 缺少 CSeq");
                let mut h = Headers::new();
                h.set("Call-ID", call_id.clone());
                h.set("CSeq", cseq);
                let resp = if seen == 0 {
                    assert!(req.headers.get("Authorization").is_none());
                    first_via = Some(via);
                    first_from = Some(from);
                    first_cseq = Some(cseq_number);
                    h.set(
                        "WWW-Authenticate",
                        r#"Digest realm="3402000000", nonce="abc123""#,
                    );
                    Response {
                        status: 401,
                        reason: "Unauthorized".into(),
                        headers: h,
                        body: Vec::new(),
                    }
                } else {
                    assert!(req.headers.get("Authorization").is_some());
                    assert_eq!(req.headers.get("Call-ID"), Some(call_id.as_str()));
                    assert_eq!(first_from.as_deref(), Some(from.as_str()));
                    assert_ne!(first_via.as_deref(), Some(via.as_str()));
                    assert_eq!(Some(cseq_number), first_cseq.map(|value| value + 1));
                    Response {
                        status: 200,
                        reason: "OK".into(),
                        headers: h,
                        body: Vec::new(),
                    }
                };
                seen += 1;
                let _ = platform
                    .send_to(&SipMessage::Response(resp), device_addr)
                    .await;
                if seen == 2 {
                    return;
                }
            }
        }
    }

    #[tokio::test]
    async fn 注册闭环_401后带鉴权成功() {
        let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();
        let device_addr = device_tp.local_addr().unwrap();

        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", platform_addr.port()));
        let platform_task = tokio::spawn(mock_platform_register(
            platform_tp.clone(),
            device_addr,
            sim.call_id().to_string(),
        ));

        let state = sim
            .register(&device_tp, "127.0.0.1", device_addr.port())
            .await
            .unwrap();
        assert_eq!(state, DeviceState::Registered);
        platform_task.await.unwrap();
    }

    #[tokio::test]
    async fn options_回_200() {
        use sip_core::{Method, Request};

        let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();

        let sim = Arc::new(DeviceSimulator::new(test_cfg(
            "127.0.0.1",
            platform_addr.port(),
        )));

        // 平台构造发往设备 AOR 的 OPTIONS,并注册接收响应的队列。
        let mut h = Headers::new();
        h.set("Via", "SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bKopt");
        h.set("From", "<sip:34020000002000000001@3402>;tag=p1");
        h.set("To", "<sip:34020000001320000001@3402>");
        h.set("Call-ID", "opt-call-1");
        h.set("CSeq", "1 OPTIONS");
        let options = Request {
            method: Method::Options,
            uri: format!("sip:{}@127.0.0.1", sim.config().device_id),
            headers: h,
            body: Vec::new(),
        };
        let mut presp = platform_tp.register("opt-call-1");

        // 设备侧应答一条入站请求。
        let incoming = deliver_udp_request(&device_tp, &platform_tp, options).await;
        let answered = sim.answer_inbound(&device_tp, &incoming).await.unwrap();
        assert!(answered);

        // 平台应收到 200 OK(Call-ID 回显)。
        let got = tokio::time::timeout(std::time::Duration::from_secs(2), presp.recv())
            .await
            .expect("超时")
            .expect("关闭");
        match got.message {
            SipMessage::Response(r) => {
                assert_eq!(r.status, 200);
                assert_eq!(r.headers.call_id(), Some("opt-call-1"));
            }
            _ => panic!("应为响应"),
        }
    }

    #[tokio::test]
    async fn 目录查询应答含通道() {
        use sip_core::Method;

        let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();

        let mut cfg = test_cfg("127.0.0.1", platform_addr.port());
        cfg.channels.push(ChannelConfig {
            channel_id: DeviceId::new("34020000001320000132").unwrap(),
            name: "Camera-1".into(),
            status: "ON".into(),
        });
        let sim = Arc::new(DeviceSimulator::new(cfg));

        let query_xml = r#"<?xml version="1.0"?>
<Query><CmdType>Catalog</CmdType><SN>999</SN><DeviceID>34020000001320000001</DeviceID></Query>"#;

        let mut h = Headers::new();
        h.set("Via", "SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bKcat");
        h.set("From", "<sip:34020000002000000001@3402>;tag=p2");
        h.set("To", "<sip:34020000001320000001@3402>");
        h.set("Call-ID", "cat-call-1");
        h.set("CSeq", "2 MESSAGE");
        h.set("Content-Type", "Application/MANSCDP+xml");
        let query_msg = sip_core::Request {
            method: Method::Message,
            uri: format!("sip:{}@127.0.0.1", sim.config().device_id),
            headers: h,
            body: query_xml.as_bytes().to_vec(),
        };
        let mut presp = platform_tp.register("cat-call-1");

        let incoming = deliver_udp_request(&device_tp, &platform_tp, query_msg).await;
        // 应答 MESSAGE 的 Request-URI 指向平台域,平台按 AOR 收独立应答。
        let mut preply = platform_tp.register_inbound("34020000002000000001");

        let answered = sim.answer_inbound(&device_tp, &incoming).await.unwrap();
        assert!(answered);

        // (1) 平台先收到对查询的空 200 OK(事务应答)。
        let ack = tokio::time::timeout(std::time::Duration::from_secs(2), presp.recv())
            .await
            .expect("超时")
            .expect("关闭");
        match ack.message {
            SipMessage::Response(r) => {
                assert_eq!(r.status, 200);
                assert!(r.body.is_empty(), "查询 200 OK 应为空 body");
            }
            _ => panic!("应先收到 200 响应"),
        }

        // (2) 平台再收到独立的 Catalog 应答 MESSAGE。
        let reply = tokio::time::timeout(std::time::Duration::from_secs(2), preply.recv())
            .await
            .expect("超时未收到应答 MESSAGE")
            .expect("关闭");
        match reply.message {
            SipMessage::Request(r) => {
                assert_eq!(r.method, Method::Message);
                let body_str = std::str::from_utf8(&r.body).unwrap();
                assert!(body_str.contains("<CmdType>Catalog</CmdType>"));
                assert!(body_str.contains("<SN>999</SN>"));
                assert!(body_str.contains("34020000001320000132")); // 通道 ID
                assert!(body_str.contains("<Name>Camera-1</Name>"));
            }
            _ => panic!("应为独立 MESSAGE 应答"),
        }
    }

    #[test]
    fn 目录应答按gb版本差异化字段() {
        use gb28181_protocol::manscdp::Query;

        let query = Query {
            cmd_type: "Catalog".into(),
            sn: 1,
            device_id: "35020000001310000001".into(),
            interval: None,
            group_id: None,
            config_type: None,
            start_time: None,
            end_time: None,
            record_type: None,
            indistinct_query: None,
            stream_number: None,
            ..Default::default()
        };
        let ch = ChannelConfig {
            channel_id: DeviceId::new("35020000001310000132").unwrap(),
            name: "Camera-1".into(),
            status: "ON".into(),
        };

        // 2022:输出 SecurityLevelCode。
        let mut cfg = test_cfg("127.0.0.1", 5060);
        cfg.channels.push(ch.clone());
        cfg.gb_version = common::GbVersion::V2022;
        let xml = DeviceSimulator::new(cfg).handle_query(&query).unwrap();
        assert!(
            xml.contains("<SecurityLevelCode>"),
            "2022 应含 SecurityLevelCode"
        );

        // 2016:不输出该 2022 新增字段。
        let mut cfg16 = test_cfg("127.0.0.1", 5060);
        cfg16.channels.push(ch);
        cfg16.gb_version = common::GbVersion::V2016;
        let xml16 = DeviceSimulator::new(cfg16).handle_query(&query).unwrap();
        assert!(
            !xml16.contains("SecurityLevelCode"),
            "2016 不应含 2022 新增字段"
        );
    }

    #[test]
    fn 目录项逐通道扩展字段生效() {
        use gb28181_protocol::id_codec::{CatalogNode, CatalogNodeType};
        use gb28181_protocol::manscdp::Query;

        let query = Query {
            cmd_type: "Catalog".into(),
            sn: 1,
            device_id: "34020000001320000001".into(),
            ..Default::default()
        };
        let mut cfg = test_cfg("127.0.0.1", 5060);
        cfg.gb_version = common::GbVersion::V2022;
        let sim = DeviceSimulator::new(cfg);
        let root = "34020000001320000001";
        // 两路通道:一路填满扩展字段,一路完全不填(验证默认路径没被破坏)。
        let mut custom = CatalogNode::new(
            "34020000001320000011",
            CatalogNodeType::VideoChannel,
            "东门枪机",
            root,
        );
        custom.address = Some("上海市浦东新区世纪大道 1 号东门".into());
        custom.longitude = Some(121.500_123);
        custom.latitude = Some(31.239_456);
        custom.ptz_type = Some(3);
        custom.capture_position_type =
            Some(gb28181_protocol::manscdp::capture_position::CITY_ROAD.into());
        sim.set_catalog_tree(vec![
            CatalogNode::new(root, CatalogNodeType::Device, "本设备", root),
            custom,
            CatalogNode::new(
                "34020000001320000012",
                CatalogNodeType::VideoChannel,
                "默认通道",
                root,
            ),
        ]);

        let xml = sim.handle_query(&query).unwrap();
        assert!(
            xml.contains("<Address>上海市浦东新区世纪大道 1 号东门</Address>"),
            "逐通道安装地址应出现在目录项里,实际:{xml}"
        );
        assert!(xml.contains("121.500123"), "逐通道经度应生效");
        assert!(xml.contains("31.239456"), "逐通道纬度应生效");
        assert!(
            xml.contains("<PTZType>3</PTZType>"),
            "逐通道摄像机类型应生效"
        );
        assert!(
            xml.contains("<CapturePositionType>0020100</CapturePositionType>"),
            "附录 O 采集部位类型应生效"
        );
        // 没填扩展字段的那一路仍要有必选 Address(取设备级默认),不能变成空元素。
        assert!(
            !xml.contains("<Address></Address>") && !xml.contains("<Address/>"),
            "留空不该产出空 Address 元素"
        );
    }

    #[test]
    fn 目录项采集部位类型仅2022输出() {
        use gb28181_protocol::id_codec::{CatalogNode, CatalogNodeType};
        use gb28181_protocol::manscdp::Query;

        let query = Query {
            cmd_type: "Catalog".into(),
            sn: 1,
            device_id: "34020000001320000001".into(),
            ..Default::default()
        };
        let root = "34020000001320000001";
        let mut node = CatalogNode::new(
            "34020000001320000011",
            CatalogNodeType::VideoChannel,
            "枪机",
            root,
        );
        node.capture_position_type = Some("0020100".into());
        node.ptz_type = Some(1);

        let mut cfg = test_cfg("127.0.0.1", 5060);
        cfg.gb_version = common::GbVersion::V2016;
        let sim = DeviceSimulator::new(cfg);
        sim.set_catalog_tree(vec![
            CatalogNode::new(root, CatalogNodeType::Device, "本设备", root),
            node,
        ]);
        let xml = sim.handle_query(&query).unwrap();
        assert!(
            !xml.contains("CapturePositionType"),
            "CapturePositionType 是 2022 新增字段,发给 2016 平台只会让它报解析错误"
        );
        assert!(
            xml.contains("<PTZType>1</PTZType>"),
            "PTZType 两版都有,应照常输出"
        );
    }

    #[test]
    fn 回放range解析折算千分比() {
        // npt=1800- → 1800/3600 = 500‰。
        let p = DeviceSimulator::parse_range_permille("PLAY MANSRTSP/1.0\r\nRange: npt=1800-\r\n");
        assert_eq!(p, Some(500));
        // npt=now- → 不定位。
        assert_eq!(
            DeviceSimulator::parse_range_permille("PLAY MANSRTSP/1.0\r\nRange: npt=now-\r\n"),
            None
        );
        // 无 Range → None。
        assert_eq!(
            DeviceSimulator::parse_range_permille("PLAY MANSRTSP/1.0\r\nScale: 2.0\r\n"),
            None
        );
    }

    /// OSD 配置命令(国标 A.2.3.2.11):平台下发 DeviceConfig+OSDConfig,
    /// 设备回 DeviceConfig 应答 + 上报 OsdConfig 事件(供 UI 展示已应用)。
    #[tokio::test]
    async fn osd配置命令_应答并上报事件() {
        use common::{DeviceEvent, DeviceObserver};
        use std::sync::atomic::{AtomicBool, Ordering};
        // 捕获 OsdConfig 事件的观察者。
        struct Cap {
            got: Arc<AtomicBool>,
        }
        impl DeviceObserver for Cap {
            fn on_event(&self, e: DeviceEvent) {
                if let DeviceEvent::OsdConfig {
                    time_show,
                    osd_show,
                } = e
                {
                    assert!(time_show && osd_show);
                    self.got.store(true, Ordering::SeqCst);
                }
            }
        }
        let got = Arc::new(AtomicBool::new(false));
        let sim = DeviceSimulator::with_observer(
            test_cfg("127.0.0.1", 5060),
            Arc::new(Cap { got: got.clone() }),
        );
        let ctrl = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceConfig</CmdType><SN>1</SN><DeviceID>d</DeviceID>\
             <OSDConfig><TimeShowFlag>1</TimeShowFlag><OSDShowFlag>1</OSDShowFlag></OSDConfig></Control>",
        )
        .unwrap();
        let xml = sim.handle_control(&ctrl).await.unwrap();
        // 应答 CmdType=DeviceConfig(A.2.6.8),且事件已上报。
        assert!(xml.contains("<CmdType>DeviceConfig</CmdType>"));
        assert!(got.load(Ordering::SeqCst), "应上报 OsdConfig 事件");
    }

    /// 扩展查询(FR-17):AlarmStatus/HomePositionQuery/StorageCard/CruiseTrack*/PTZPreciseStatus。
    #[test]
    fn 扩展查询应答_各cmdtype() {
        let mk = |cmd: &str, group: Option<u32>| gb28181_protocol::manscdp::Query {
            cmd_type: cmd.into(),
            sn: 1,
            device_id: "34020000001320000001".into(),
            interval: None,
            group_id: group,
            config_type: None,
            start_time: None,
            end_time: None,
            record_type: None,
            indistinct_query: None,
            stream_number: None,
            ..Default::default()
        };
        let mut cfg = test_cfg("127.0.0.1", 5060);
        cfg.channels.push(ChannelConfig {
            channel_id: DeviceId::new("34020000001320000001").unwrap(),
            name: "Ch".into(),
            status: "ON".into(),
        });
        let sim = DeviceSimulator::new(cfg);

        // 预置一条巡航轨迹 1 = [1,3],并置布防态,便于验证读回。
        {
            let mut s = sim.control_state.lock();
            s.alarming = true;
            s.home_set = true;
            s.cruise_tracks.insert(1, vec![1, 3]);
        }

        let alarm = sim.handle_query(&mk("AlarmStatus", None)).unwrap();
        assert!(alarm.contains("<CmdType>AlarmStatus</CmdType>"));
        assert!(alarm.contains("<DutyStatus>ALARM</DutyStatus>"));

        let home = sim.handle_query(&mk("HomePositionQuery", None)).unwrap();
        assert!(home.contains("<ResetTime>30</ResetTime>"));
        assert!(home.contains("<PresetIndex>1</PresetIndex>"));

        let sd = sim.handle_query(&mk("SDCardStatus", None)).unwrap();
        assert!(sd.contains("<TotalCapacity>32768</TotalCapacity>"));

        let list = sim.handle_query(&mk("CruiseTrackListQuery", None)).unwrap();
        assert!(list.contains("<TrackList Num=\"1\">"));
        assert!(list.contains("<Name>巡航 1</Name>"));

        let detail = sim.handle_query(&mk("CruiseTrackQuery", Some(1))).unwrap();
        assert!(detail.contains("<GroupID>1</GroupID>"));
        assert!(detail.contains("<PresetID>3</PresetID>"));

        let precise = sim.handle_query(&mk("PTZPosition", None)).unwrap();
        assert!(precise.contains("<Zoom>1.00</Zoom>"));
    }

    /// 测试辅助:按字面 IP 判定(域名交由 `resolve_safe_target` 在解析后判定)。
    fn ssrf_ip(host: &str) -> Result<()> {
        match host.parse::<std::net::IpAddr>() {
            Ok(ip) => reject_unsafe_ip(ip),
            Err(_) => Ok(()),
        }
    }

    #[test]
    fn ssrf_拒绝受限地址() {
        assert!(ssrf_ip("127.0.0.1").is_err());
        assert!(ssrf_ip("169.254.1.1").is_err());
        assert!(ssrf_ip("192.168.1.10").is_err());
        assert!(ssrf_ip("10.0.0.1").is_err());
        assert!(ssrf_ip("224.0.0.1").is_err());
        // 云元数据端点必须被拒(169.254.169.254 属链路本地)。
        assert!(ssrf_ip("169.254.169.254").is_err());
        // 运营商级 NAT(100.64.0.0/10)与文档段(203.0.113.0/24)同样拒绝。
        assert!(ssrf_ip("100.64.0.1").is_err());
        assert!(ssrf_ip("203.0.113.9").is_err());
        // IPv6 唯一本地与链路本地也拒绝。
        assert!(ssrf_ip("fd00::1").is_err());
        assert!(ssrf_ip("fe80::1").is_err());
        // 真正的公网 IP 放行;域名留给解析后判定(见 resolve_safe_target)。
        assert!(ssrf_ip("8.8.8.8").is_ok());
        assert!(ssrf_ip("upload.example.com").is_ok());
    }

    #[test]
    fn ssrf_ipv6内嵌的内网v4也要拦() {
        // 这几种写法都是"把一个 IPv4 塞进 IPv6",只按 IPv6 规则看的话
        // 它们全是普通全球单播地址,会被放行 —— 于是 SSRF 防护形同虚设。
        //
        // ::/96 IPv4-兼容地址:::127.0.0.1
        assert!(ssrf_ip("::7f00:1").is_err(), "IPv4 兼容地址里的环回");
        assert!(
            ssrf_ip("::c0a8:1").is_err(),
            "IPv4 兼容地址里的 192.168.0.1"
        );
        // 6to4:2002:<v4>::/48
        assert!(
            ssrf_ip("2002:a9fe:a9fe::1").is_err(),
            "6to4 包裹云元数据地址"
        );
        assert!(
            ssrf_ip("2002:c0a8:101::1").is_err(),
            "6to4 包裹 192.168.1.1"
        );
        // NAT64:64:ff9b::<v4>
        assert!(ssrf_ip("64:ff9b::a00:1").is_err(), "NAT64 包裹 10.0.0.1");
        // fec0::/10 站点本地(已废弃但仍可能被路由)
        assert!(ssrf_ip("fec0::1").is_err());
        // 反例:6to4 包裹一个公网 IPv4 应放行,别把规则做成"凡 2002:: 全拒"。
        assert!(ssrf_ip("2002:0808:0808::1").is_ok(), "6to4 包裹 8.8.8.8");
        // 普通公网 IPv6 照常放行。
        assert!(ssrf_ip("2400:3200::1").is_ok());
    }

    #[test]
    fn 上传url的crlf注入被拒绝() {
        // quick-xml 会把 &#13;&#10; 解成真实 CR LF,若原样插进请求行,
        // 恶意平台就能在一次 PUT 里追加请求头甚至第二个完整请求(HTTP 请求走私)。
        let err = reject_header_unsafe("路径", "/a\r\nX-Injected: 1")
            .expect_err("含 CRLF 的路径必须拒绝");
        assert!(err.to_string().contains("请求注入"), "实际: {err}");
        assert!(reject_header_unsafe("主机", "evil\ncom").is_err());
        assert!(
            reject_header_unsafe("路径", "/a b").is_err(),
            "裸空格会截断请求行"
        );
        assert!(reject_header_unsafe("路径", "/a\tb").is_err());
        assert!(reject_header_unsafe("路径", "/a\0b").is_err());
        // 正常路径(含 percent-encode 的空格与查询串)放行。
        assert!(reject_header_unsafe("路径", "/upload/snap%20001.jpg?sn=3").is_ok());
        assert!(reject_header_unsafe("主机", "upload.example.com").is_ok());
    }

    #[tokio::test]
    async fn 上传拒绝非常规端口与注入路径() {
        // 端口不在白名单:任意端口 = 一个公网端口扫描器。
        let err = http_put_jpeg("http://8.8.8.8:22/x.jpg", b"x")
            .await
            .expect_err("22 端口应被拒");
        assert!(err.to_string().contains("不在允许范围"), "实际: {err}");
        // 路径注入在连接之前就被拦下(所以用一个不可达地址也能测)。
        let err = http_put_jpeg("http://8.8.8.8:80/a\r\nHost: evil\r\n\r\nGET /x", b"x")
            .await
            .expect_err("CRLF 路径应被拒");
        assert!(err.to_string().contains("请求注入"), "实际: {err}");
        // userinfo 不能用来伪装 host。
        let err = http_put_jpeg("http://8.8.8.8@127.0.0.1:80/x.jpg", b"x")
            .await
            .expect_err("剥掉 userinfo 后是环回地址,应被拒");
        assert!(err.to_string().contains("受限地址"), "实际: {err}");
    }

    #[tokio::test]
    async fn 抓拍上传_协议格式与2xx判定() {
        // 起一个本地 TCP 服务模拟采集端(回 200),用 127.0.0.1 直连绕过 SSRF(仅测协议)。
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 1024];
            let n = s.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            req
        });
        // http_put_jpeg 对 127.0.0.1 会被 SSRF 拒绝(安全设计),所以这里用被测函数
        // **真正生成**的请求头去打桩服务,而不是在测试里手抄一份格式串 ——
        // 手抄的那份不会随实现变化,改坏了也照样绿。
        let jpeg: &[u8] = &[0xFF, 0xD8, 0xFF, 0xD9];
        let head = build_put_request("/snap.jpg", &addr.to_string(), jpeg.len());
        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        c.write_all(head.as_bytes()).await.unwrap();
        c.write_all(jpeg).await.unwrap();
        let mut resp = Vec::new();
        let _ = c.read_to_end(&mut resp).await;
        assert!(String::from_utf8_lossy(&resp).contains("200"));

        let req = srv.await.unwrap();
        assert!(req.starts_with("PUT /snap.jpg HTTP/1.1"));
        assert!(req.contains("Content-Type: image/jpeg"));
        assert!(req.contains("Content-Length: 4"));
        assert!(req.contains("Connection: close"));
        // 头部与正文之间必须有空行(正文是第二次 write_all 发的,不一定和头部同一个 TCP 读到)。
        assert!(head.ends_with("\r\n\r\n"), "头部必须以空行结束");

        // 且 http_put_jpeg 对环回目标必须拒绝(SSRF 防护生效)。
        assert!(http_put_jpeg(&format!("http://{addr}/x.jpg"), jpeg)
            .await
            .is_err());
    }

    /// 多通道目录(FR-34):载入模板后 Catalog 查询枚举全部节点,含层级 ParentID/Parental。
    #[test]
    fn 多通道目录_模板与crud() {
        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        sim.load_catalog_template("nvr-8ch");
        let q = gb28181_protocol::manscdp::Query {
            cmd_type: "Catalog".into(),
            sn: 1,
            device_id: sim.config().device_id.to_string(),
            interval: None,
            group_id: None,
            config_type: None,
            start_time: None,
            end_time: None,
            record_type: None,
            indistinct_query: None,
            stream_number: None,
            ..Default::default()
        };
        let xml = sim.handle_query(&q).unwrap();
        // 8 视频通道 + 1 业务分组 = 9 项(根设备自引用被过滤)。
        assert!(xml.contains("<DeviceList Num=\"9\">"));
        assert!(xml.contains("<Name>NVR-8</Name>"));
        assert!(xml.contains("<Name>通道-01</Name>"));
        assert!(xml.contains("<Parental>1</Parental>")); // 业务分组
        assert!(xml.contains("<Parental>0</Parental>")); // 视频通道

        // CRUD:新增一个通道 → 树 +1;删除 → 复原。
        let node = gb28181_protocol::id_codec::CatalogNode::new(
            "34020000001320000099",
            gb28181_protocol::id_codec::CatalogNodeType::VideoChannel,
            "新增通道",
            sim.config().device_id.to_string(),
        );
        assert!(sim.upsert_channel(node.clone()));
        assert!(!sim.upsert_channel(node)); // 再次 upsert 为更新非新增
        assert!(sim.remove_channel("34020000001320000099"));
        assert!(!sim.remove_channel("34020000001320000099")); // 已删
    }

    /// 语音广播(FR-36):TargetID 属本设备回 Broadcast Response OK。
    #[tokio::test]
    async fn 语音广播_目标匹配回ok() {
        use sip_core::{Headers, Method, Request, SipMessage};
        let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();
        let sim = std::sync::Arc::new(DeviceSimulator::new(test_cfg(
            "127.0.0.1",
            platform_addr.port(),
        )));

        let body = format!(
            "<?xml version=\"1.0\"?><Notify><CmdType>Broadcast</CmdType><SN>1</SN>\
             <SourceID>34020000002000000001</SourceID><TargetID>{}</TargetID></Notify>",
            sim.config().device_id
        );
        let mut h = Headers::new();
        h.set("From", "<sip:34020000002000000001@3402>;tag=p1");
        h.set("To", "<sip:34020000001320000001@3402>");
        h.set("Call-ID", "bc-call-1");
        h.set("CSeq", "1 MESSAGE");
        h.set("Content-Type", "Application/MANSCDP+xml");
        let msg = Request {
            method: Method::Message,
            uri: format!("sip:{}@127.0.0.1", sim.config().device_id),
            headers: h,
            body: body.into_bytes(),
        };
        let incoming = deliver_udp_request(&device_tp, &platform_tp, msg).await;
        // 广播 Response 走独立 MESSAGE,平台按 AOR 收。
        let mut preply = platform_tp.register_inbound("34020000002000000001");
        sim.answer_inbound(&device_tp, &incoming).await.unwrap();

        let reply = tokio::time::timeout(std::time::Duration::from_secs(2), preply.recv())
            .await
            .expect("超时未收到广播应答")
            .expect("关闭");
        match reply.message {
            SipMessage::Request(r) => {
                let s = std::str::from_utf8(&r.body).unwrap();
                assert!(s.contains("<CmdType>Broadcast</CmdType>"));
                assert!(s.contains("<Result>OK</Result>"));
            }
            _ => panic!("应为独立 MESSAGE"),
        }
    }

    /// 扩展控制(FR-18):巡航增点→列表查询可见;精确云台→精准状态查询读回;布防→报警状态。
    #[tokio::test]
    async fn 扩展控制_更新状态供查询读回() {
        use gb28181_protocol::manscdp::Control;
        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));

        // 巡航增点:轨迹 1 加预置 5(byte3=0x84 track=1 preset=5)。
        let cruise = Control {
            cmd_type: "DeviceControl".into(),
            device_id: "d".into(),
            sn: 1,
            ptz_cmd: Some("A50F0184010500".into()),
            ..Default::default()
        };
        sim.handle_control(&cruise).await.unwrap();
        assert!(sim.control_state.lock().cruise_tracks[&1].contains(&5));

        // 精确云台:pan=90 tilt=10 zoom=2。
        let precise = Control {
            cmd_type: "DeviceControl".into(),
            device_id: "d".into(),
            sn: 2,
            ptz_precise_ctrl: Some(gb28181_protocol::manscdp::PtzPreciseCtrl {
                pan: 90.0,
                tilt: 10.0,
                zoom: 2.0,
            }),
            ..Default::default()
        };
        sim.handle_control(&precise).await.unwrap();
        assert_eq!(sim.control_state.lock().precise_pose, (90.0, 10.0, 2.0));

        // 布防 → alarming=true。
        let guard = Control {
            cmd_type: "DeviceControl".into(),
            device_id: "d".into(),
            sn: 3,
            guard_cmd: Some("SetGuard".into()),
            ..Default::default()
        };
        sim.handle_control(&guard).await.unwrap();
        assert!(sim.control_state.lock().alarming);
    }

    #[tokio::test]
    async fn 移动位置订阅_回200并周期上报位置notify() {
        use sip_core::{Method, Request};

        let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();

        // 设备平台地址指向 mock 平台;server_domain 为平台 AOR。
        let sim = Arc::new(DeviceSimulator::new(test_cfg(
            "127.0.0.1",
            platform_addr.port(),
        )));

        // 平台构造 MobilePosition 订阅(Interval=1)发往设备 AOR。
        let sub_xml = r#"<?xml version="1.0"?>
<Query><CmdType>MobilePosition</CmdType><SN>7</SN><DeviceID>34020000001320000001</DeviceID><Interval>1</Interval></Query>"#;
        let mut sub_headers = Headers::new();
        sub_headers.set("Call-ID", "sub-call-1");
        sub_headers.set("CSeq", "1 SUBSCRIBE");
        sub_headers.set("Event", "presence");
        let sub_req = Request {
            method: Method::Subscribe,
            uri: "sip:34020000001320000001@127.0.0.1".into(),
            headers: sub_headers,
            body: sub_xml.as_bytes().to_vec(),
        };
        // 订阅的 200 OK 回给 incoming.from,平台按 Call-ID 收。
        let mut psub = platform_tp.register("sub-call-1");
        // 位置 NOTIFY(MESSAGE)Request-URI 指向平台域,平台按 AOR 收。
        let mut pnotify = platform_tp.register_inbound("34020000002000000001");

        let incoming = deliver_udp_request(&device_tp, &platform_tp, sub_req).await;
        let answered = sim.answer_inbound(&device_tp, &incoming).await.unwrap();
        assert!(answered);

        // (1) 平台先收到 SUBSCRIBE 的 200 OK。
        let ok = tokio::time::timeout(std::time::Duration::from_secs(2), psub.recv())
            .await
            .expect("超时未收到订阅 200")
            .expect("关闭");
        match ok.message {
            SipMessage::Response(r) => assert_eq!(r.status, 200),
            _ => panic!("应先收到 200 响应"),
        }

        // (2) 周期任务的首个 tick 立即触发,平台应收到一条 MobilePosition NOTIFY。
        let notify = tokio::time::timeout(std::time::Duration::from_secs(3), pnotify.recv())
            .await
            .expect("超时未收到位置 NOTIFY")
            .expect("关闭");
        match notify.message {
            SipMessage::Request(r) => {
                assert_eq!(r.method, Method::Message);
                let body_str = std::str::from_utf8(&r.body).unwrap();
                assert!(body_str.contains("<CmdType>MobilePosition</CmdType>"));
            }
            _ => panic!("应为 MobilePosition NOTIFY MESSAGE"),
        }

        // 订阅表应登记该订阅。
        assert!(sim
            .subscriptions
            .lock()
            .await
            .contains_key("MobilePosition"));

        // 停止订阅,任务应退出(不再 panic/泄漏)。
        sim.stop_subscriptions().await;
        assert!(sim.subscriptions.lock().await.is_empty());
    }

    #[tokio::test]
    async fn 报警订阅_回200并记录对话供对话内notify() {
        use sip_core::{Method, Request};

        let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();

        let sim = Arc::new(DeviceSimulator::new(test_cfg(
            "127.0.0.1",
            platform_addr.port(),
        )));

        let sub_xml = r#"<?xml version="1.0"?>
<Query><CmdType>Alarm</CmdType><SN>9</SN><DeviceID>34020000001320000001</DeviceID></Query>"#;
        let mut h = Headers::new();
        h.set("Call-ID", "alarm-sub-1");
        h.set("CSeq", "1 SUBSCRIBE");
        h.set(
            "From",
            "<sip:34020000002000000001@34020000002000000001>;tag=plat7",
        );
        h.set("To", "<sip:34020000001320000001@34020000002000000001>");
        h.set("Event", "Alarm");
        h.set("Expires", "3600");
        let sub_req = Request {
            method: Method::Subscribe,
            uri: "sip:34020000001320000001@127.0.0.1".into(),
            headers: h,
            body: sub_xml.as_bytes().to_vec(),
        };
        let mut psub = platform_tp.register("alarm-sub-1");

        let incoming = deliver_udp_request(&device_tp, &platform_tp, sub_req).await;
        assert!(sim.answer_inbound(&device_tp, &incoming).await.unwrap());

        // 平台收到订阅 200(To 带本端 tag)。
        let ok = tokio::time::timeout(std::time::Duration::from_secs(2), psub.recv())
            .await
            .expect("超时未收到订阅 200")
            .expect("关闭");
        match ok.message {
            SipMessage::Response(r) => {
                assert_eq!(r.status, 200);
                assert!(r.headers.get("To").unwrap().contains("tag="));
            }
            _ => panic!("应先收到 200"),
        }

        // 报警订阅对话已记录(后续 report_alarm 将走对话内 NOTIFY)。
        let stored = sim.alarm_dialog.lock().await.clone();
        let sub = stored.expect("应记录报警订阅对话");
        let (dialog, dst) = (sub.dialog, sub.peer);
        assert_eq!(dialog.call_id, "alarm-sub-1");
        assert_eq!(dialog.event, "Alarm");
        assert_eq!(dialog.expires, 3600);
        assert!(dialog.remote_from.contains("tag=plat7"));
        assert_eq!(dst, platform_addr);

        // 下线清理:对话应被清除。
        sim.stop_subscriptions().await;
        assert!(sim.alarm_dialog.lock().await.is_none());
    }

    #[tokio::test]
    async fn info回放控制_调速与暂停恢复并回200() {
        use sip_core::{Method, Request};

        let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();

        let sim = Arc::new(DeviceSimulator::new(test_cfg(
            "127.0.0.1",
            platform_addr.port(),
        )));

        // 注入一个活跃会话(不真正推流:任务只等待停止信号)。
        let control = media_rtp::PlaybackControl::new();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _ = stop_rx.await;
        });
        *sim.session.lock().await = Some(PushSession {
            marker: Arc::new(()),
            task,
            stop_tx,
            control: control.clone(),
            is_history: false,
            media_source: None,
            dialog: builder::InboundDialog {
                call_id: "info-call-1".into(),
                local_tag: "devtag".into(),
                remote_from: "<sip:platform@test>;tag=ptag".into(),
                peer: platform_addr,
                request_uri: "sip:34020000001320000001@127.0.0.1".into(),
            },
        });

        // 构造 INFO 请求的辅助闭包。每条用独立 CSeq 与 branch —— 真实平台就是这样,
        // 而复用同一 (branch, CSeq) 会被服务端事务层正确判定为重传。
        let make_info = |cseq: u32, body: &str| {
            let mut h = Headers::new();
            h.set("Call-ID", "info-call-1");
            h.set("CSeq", format!("{cseq} INFO"));
            h.set(
                "Via",
                format!("SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bK-info-{cseq}"),
            );
            h.set(
                "From",
                "<sip:34020000002000000001@34020000002000000001>;tag=p",
            );
            h.set(
                "To",
                "<sip:34020000001320000001@34020000002000000001>;tag=d",
            );
            Request {
                method: Method::Info,
                uri: "sip:34020000001320000001@127.0.0.1".into(),
                headers: h,
                body: body.as_bytes().to_vec(),
            }
        };
        let mut prx = platform_tp.register("info-call-1");

        // PLAY + Scale=2.0 → 倍速 2.0,未暂停。
        let inc = deliver_udp_request(
            &device_tp,
            &platform_tp,
            make_info(2, "PLAY MANSRTSP/1.0\r\nCSeq: 2\r\nScale: 2.0\r\n\r\n"),
        )
        .await;
        assert!(sim.answer_inbound(&device_tp, &inc).await.unwrap());
        assert_eq!(control.speed(), 2.0);
        assert!(!control.is_paused());
        // 平台应收到 200。
        let ok = tokio::time::timeout(std::time::Duration::from_secs(2), prx.recv())
            .await
            .expect("超时")
            .expect("关闭");
        matches!(ok.message, SipMessage::Response(r) if r.status == 200);

        // PAUSE → 暂停。
        let inc = deliver_udp_request(
            &device_tp,
            &platform_tp,
            make_info(3, "PAUSE MANSRTSP/1.0\r\nCSeq: 3\r\nPauseTime: now\r\n\r\n"),
        )
        .await;
        assert!(sim.answer_inbound(&device_tp, &inc).await.unwrap());
        assert!(control.is_paused());

        // PLAY(无 Scale)→ 恢复,倍速回 1.0。
        let inc = deliver_udp_request(
            &device_tp,
            &platform_tp,
            make_info(4, "PLAY MANSRTSP/1.0\r\nCSeq: 4\r\n\r\n"),
        )
        .await;
        assert!(sim.answer_inbound(&device_tp, &inc).await.unwrap());
        assert!(!control.is_paused());
        assert_eq!(control.speed(), 1.0);

        // 同一 (branch, CSeq) 的重传:必须只重发缓存响应,**不能**再跑一遍业务逻辑。
        control.pause();
        let retransmit = deliver_udp_request(
            &device_tp,
            &platform_tp,
            make_info(4, "PLAY MANSRTSP/1.0\r\nCSeq: 4\r\n\r\n"),
        )
        .await;
        assert!(sim.answer_inbound(&device_tp, &retransmit).await.unwrap());
        assert!(
            control.is_paused(),
            "重传的 INFO 不应被当成新命令重新执行 PLAY"
        );
    }

    /// 平台 200 OK 丢包后重传 INVITE 是现场常态。缺服务端事务时设备会因
    /// "已有会话"回 486 Busy Here,点播直接失败——这里锁定修复后的行为:
    /// 重传命中缓存的 200 OK。
    #[tokio::test]
    async fn invite_重传命中缓存响应而非回486() {
        let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();
        let device_addr = device_tp.local_addr().unwrap();

        let mut cfg = test_cfg(&platform_addr.ip().to_string(), platform_addr.port());
        // B 档轻量伪流:不依赖 ffmpeg / 文件。
        cfg.light_bitrate_kbps = Some(128);
        cfg.channels = vec![ChannelConfig {
            channel_id: DeviceId::new("34020000001320000132").unwrap(),
            name: "Camera-1".into(),
            status: "ON".into(),
        }];
        let sim = Arc::new(DeviceSimulator::new(cfg));
        sim.set_local_addr_for_test(&device_addr.ip().to_string(), device_addr.port());

        let sdp = "v=0\r\n\
o=34020000002000000001 0 0 IN IP4 127.0.0.1\r\n\
s=Play\r\n\
c=IN IP4 127.0.0.1\r\n\
t=0 0\r\n\
m=video 40000 RTP/AVP 96\r\n\
a=rtpmap:96 PS/90000\r\n\
y=0100000001\r\n";

        let make_invite = || {
            let mut h = Headers::new();
            h.set("Call-ID", "invite-retrans");
            h.set("CSeq", "1 INVITE");
            h.set("Via", "SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bK-inv-1");
            h.set(
                "From",
                "<sip:34020000002000000001@34020000002000000001>;tag=p",
            );
            h.set("To", "<sip:34020000001320000132@34020000002000000001>");
            h.set("Content-Type", "application/sdp");
            Request {
                method: Method::Invite,
                uri: "sip:34020000001320000132@127.0.0.1".into(),
                headers: h,
                body: sdp.as_bytes().to_vec(),
            }
        };

        let mut prx = platform_tp.register("invite-retrans");
        let first_invite = deliver_udp_request(&device_tp, &platform_tp, make_invite()).await;
        assert!(sim.answer_inbound(&device_tp, &first_invite).await.unwrap());

        // 收 100 Trying 与 200 OK。
        let mut statuses = Vec::new();
        for _ in 0..2 {
            let inc = tokio::time::timeout(std::time::Duration::from_secs(2), prx.recv())
                .await
                .expect("超时")
                .expect("关闭");
            if let SipMessage::Response(r) = inc.message {
                statuses.push(r.status);
            }
        }
        assert!(
            statuses.contains(&200),
            "首次 INVITE 应回 200,实际 {statuses:?}"
        );

        // 重传同一条 INVITE:应重发缓存的 200,而不是 486。
        let retransmit = deliver_udp_request(&device_tp, &platform_tp, make_invite()).await;
        assert!(sim.answer_inbound(&device_tp, &retransmit).await.unwrap());
        let inc = tokio::time::timeout(std::time::Duration::from_secs(2), prx.recv())
            .await
            .expect("超时")
            .expect("关闭");
        match inc.message {
            SipMessage::Response(r) => {
                assert_eq!(r.status, 200, "重传 INVITE 不应被回 486 Busy Here");
                assert!(!r.body.is_empty(), "缓存响应应含 SDP(与首次 200 完全一致)");
            }
            other => panic!("应为响应,实际 {other:?}"),
        }

        // 会话应仍是那一个(没有被重复建流)。
        assert!(sim.session.lock().await.is_some());
    }

    #[tokio::test]
    async fn 未实现方法回405并带allow头() {
        let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let device_tp: Arc<dyn SipTransport> = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();
        let sim = Arc::new(DeviceSimulator::new(test_cfg(
            &platform_addr.ip().to_string(),
            platform_addr.port(),
        )));

        let mut h = Headers::new();
        h.set("Call-ID", "publish-call");
        h.set("CSeq", "1 PUBLISH");
        h.set("Via", "SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bK-pub");
        let request = Request {
            method: Method::Publish,
            uri: "sip:34020000001320000001@127.0.0.1".into(),
            headers: h,
            body: Vec::new(),
        };
        let inc = deliver_udp_request(&device_tp, &platform_tp, request).await;
        let mut prx = platform_tp.register("publish-call");
        assert!(sim.answer_inbound(&device_tp, &inc).await.unwrap());
        let got = tokio::time::timeout(std::time::Duration::from_secs(2), prx.recv())
            .await
            .expect("超时")
            .expect("关闭");
        match got.message {
            SipMessage::Response(r) => {
                assert_eq!(r.status, 405);
                assert!(r.headers.get("Allow").unwrap().contains("INVITE"));
            }
            other => panic!("应为响应,实际 {other:?}"),
        }
    }

    #[test]
    fn 续约周期取平台授予有效期的八成() {
        use std::time::Duration;
        assert_eq!(renew_interval(3600), Duration::from_secs(2880));
        assert_eq!(renew_interval(120), Duration::from_secs(96));
        // 很短的有效期要留出一次事务往返余量,不能贴着到期。
        assert!(renew_interval(30) < Duration::from_secs(30));
        assert!(renew_interval(10) < Duration::from_secs(10));
        assert!(renew_interval(1) >= Duration::from_secs(5));
    }

    #[test]
    fn 目录超过_mtu_时分多条应答且_sumnum_为总数() {
        use gb28181_protocol::id_codec::{CatalogNode, CatalogNodeType};
        let sim = DeviceSimulator::new(test_cfg("1.2.3.4", 5060));
        // 造一棵足够大的目录树,单条 XML 必然超过 900 字节。
        let mut nodes = vec![CatalogNode::new(
            "34020000001320000001",
            CatalogNodeType::Device,
            "Root",
            "34020000001320000001",
        )];
        for i in 0..40u32 {
            nodes.push(CatalogNode::new(
                format!("3402000000132000{:04}", 1000 + i),
                CatalogNodeType::VideoChannel,
                format!("通道-{i}"),
                "34020000001320000001",
            ));
        }
        sim.set_catalog_tree(nodes);

        let query = gb28181_protocol::manscdp::Query {
            cmd_type: "Catalog".into(),
            sn: 7,
            device_id: "34020000001320000001".into(),
            ..Default::default()
        };
        let replies = sim.catalog_replies(&query, MAX_MANSCDP_BODY_BYTES).unwrap();
        assert!(replies.len() > 1, "40 通道的目录应被分成多条");
        for xml in &replies {
            assert!(
                xml.len() <= MAX_MANSCDP_BODY_BYTES * 2,
                "分片后单条不应远超阈值,实际 {}",
                xml.len()
            );
            assert!(xml.contains("<SumNum>40</SumNum>"), "SumNum 必须是总数");
        }
        // 各条 DeviceList 的 Num 属性之和应等于 SumNum。
        let total: usize = replies
            .iter()
            .map(|x| {
                x.split("<DeviceList Num=\"")
                    .nth(1)
                    .and_then(|s| s.split('"').next())
                    .and_then(|n| n.parse::<usize>().ok())
                    .unwrap_or(0)
            })
            .sum();
        assert_eq!(total, 40, "分片的 Num 之和应等于总数");
        // 条目 ID 不重不漏。
        let mut ids: Vec<String> = replies
            .iter()
            .flat_map(|x| {
                x.split("<DeviceID>")
                    .skip(2) // 跳过根 DeviceID
                    .filter_map(|s| s.split("</DeviceID>").next().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 40, "分片后条目应不重不漏");
    }

    #[test]
    fn 假标签不能改变分派路径() {
        use gb28181_protocol::manscdp::parse_envelope;
        // 注释里塞 <Control> 不应把 Query 误判成 Control。
        let sneaky = "<?xml version=\"1.0\"?><Query><!-- <Control> --><CmdType>Catalog</CmdType>\
<SN>1</SN><DeviceID>34020000001320000001</DeviceID></Query>";
        let env = parse_envelope(sneaky).unwrap();
        assert_eq!(env.root, "Query");
        assert_eq!(env.cmd_type, "Catalog");

        // 文本节点里塞 <Control> 同样无效(会被转义)。
        let escaped = "<?xml version=\"1.0\"?><Query><CmdType>Catalog</CmdType>\
<DeviceName>&lt;Control&gt;</DeviceName></Query>";
        assert_eq!(parse_envelope(escaped).unwrap().root, "Query");

        // 真的 Control 报文能被识别。
        let ctrl = "<?xml version=\"1.0\"?><Control><CmdType>DeviceControl</CmdType>\
<SN>2</SN><DeviceID>34020000001320000001</DeviceID><RecordCmd>Record</RecordCmd></Control>";
        let env = parse_envelope(ctrl).unwrap();
        assert_eq!(env.root, "Control");
        assert_eq!(env.cmd_type, "DeviceControl");
    }
}
