use std::net::SocketAddr;
use std::sync::Arc;

use common::{Error, Result};
use sip_core::SipTransport;

use super::*;

/// 最小 HTTP PUT 上传 JPEG(抓拍上传用)。仅支持 http://(https 需 TLS,本工具不引入重依赖)。
///
/// 含 SSRF 防护:**先解析出真实目标 IP 再判定**,并直接连到已校验的地址,
/// 避免"校验用域名、连接再解析一次"的 DNS rebinding 窗口。
/// 用裸 TCP 手写请求,避免给压测链路(依赖本 crate)引入 HTTP 客户端。
pub(crate) async fn http_put_jpeg(url: &str, body: &[u8]) -> Result<()> {
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
pub(crate) async fn resolve_safe_target(host: &str, port: u16) -> Result<SocketAddr> {
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
pub(crate) fn build_put_request(path: &str, host: &str, body_len: usize) -> String {
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
pub(crate) fn reject_header_unsafe(what: &str, value: &str) -> Result<()> {
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
pub(crate) fn reject_unsafe_ip(ip: std::net::IpAddr) -> Result<()> {
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
pub(crate) fn embedded_v4_unsafe(hi: u16, lo: u16) -> bool {
    let v4 = std::net::Ipv4Addr::from(((hi as u32) << 16) | lo as u32);
    reject_unsafe_ip(std::net::IpAddr::V4(v4)).is_err()
}

impl DeviceSimulator {
    /// 抓拍上传(GB-2022):串行拍 N 张,每张 HTTP PUT 上传后发完成 NOTIFY。
    /// 模拟设备无摄像头,上传一段占位 JPEG 字节。
    pub(crate) async fn run_snapshot_upload(
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
}
