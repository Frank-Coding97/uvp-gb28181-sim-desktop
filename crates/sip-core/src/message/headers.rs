//! SIP 头字段集合。
//!
//! 保序存储(Vec)以尽量贴近原始报文顺序;提供大小写不敏感的按名读写。
//! 只对 GB28181 用到的头提供便捷访问器,其余头原样保留。

/// 头集合:有序的 (名, 值) 列表。名大小写不敏感,值原样保留。
#[derive(Debug, Clone, Default)]
pub struct Headers {
    entries: Vec<(String, String)>,
}

impl Headers {
    /// 空集合。
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一个头(允许同名多值,如多个 Via)。
    pub fn append(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.entries.push((name.into(), value.into()));
    }

    /// 设置头:若已存在同名(首个)则覆盖,否则追加。用于单值头。
    pub fn set(&mut self, name: &str, value: impl Into<String>) {
        if let Some(e) = self
            .entries
            .iter_mut()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
        {
            e.1 = value.into();
        } else {
            self.entries.push((name.to_string(), value.into()));
        }
    }

    /// 取首个同名头的值。
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// 取全部同名头(如多个 Via)。
    pub fn get_all(&self, name: &str) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// 写入序列化缓冲。Content-Length 由调用方传入的 body 长度覆盖/补齐,
    /// 保证与实际 body 一致(避免解析-序列化往返时长度失真)。
    pub fn write_into(&self, out: &mut Vec<u8>, body_len: usize) {
        let mut wrote_len = false;
        for (n, v) in &self.entries {
            if n.eq_ignore_ascii_case("Content-Length") {
                out.extend_from_slice(format!("Content-Length: {body_len}\r\n").as_bytes());
                wrote_len = true;
            } else {
                out.extend_from_slice(format!("{n}: {v}\r\n").as_bytes());
            }
        }
        if !wrote_len {
            out.extend_from_slice(format!("Content-Length: {body_len}\r\n").as_bytes());
        }
    }

    // ---- GB28181 常用头便捷访问器 ----

    /// Call-ID。
    pub fn call_id(&self) -> Option<&str> {
        self.get("Call-ID")
    }

    /// CSeq 原始值(如 "1 REGISTER")。
    pub fn cseq(&self) -> Option<&str> {
        self.get("CSeq")
    }

    /// 解析 CSeq 的序号部分。
    pub fn cseq_number(&self) -> Option<u32> {
        self.cseq()
            .and_then(|s| s.split_whitespace().next())
            .and_then(|n| n.parse().ok())
    }

    /// 解析 CSeq 的方法部分(响应匹配需要,RFC 3261 §17.1.3)。
    pub fn cseq_method(&self) -> Option<&str> {
        self.cseq().and_then(|s| s.split_whitespace().nth(1))
    }

    /// 首个 Via 的 `branch` 参数——事务标识(RFC 3261 §8.1.1.7)。
    pub fn via_branch(&self) -> Option<&str> {
        param_value(self.get("Via")?, "branch")
    }

    /// From 头 URI 的 user 部分。
    ///
    /// 支持带/不带 display-name 的 name-addr 形式；严格按尖括号边界选 URI，避免在
    /// display-name 中搜索字符串造成身份混淆。语法不完整、无 userinfo 或重复分隔符时
    /// 返回 None，由上层按 400 Bad Request 处理。
    pub fn from_uri_user(&self) -> Option<&str> {
        name_addr_uri_user(self.get("From")?)
    }

    /// From 头的 `tag` 参数。
    pub fn from_tag(&self) -> Option<&str> {
        param_value(self.get("From")?, "tag")
    }

    /// To 头的 `tag` 参数。
    pub fn to_tag(&self) -> Option<&str> {
        param_value(self.get("To")?, "tag")
    }

    /// Expires 头(秒)。注册续约与订阅到期都要以平台授予值为准。
    pub fn expires(&self) -> Option<u64> {
        self.get("Expires")?.trim().parse().ok()
    }

    /// Contact 头里的 `expires` 参数(平台可能在 Contact 而非 Expires 头里授予时长)。
    pub fn contact_expires(&self) -> Option<u64> {
        param_value(self.get("Contact")?, "expires")?.parse().ok()
    }
}

/// 解析 SIP name-addr/addr-spec 并返回 URI user，拒绝含糊或截断输入。
fn name_addr_uri_user(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let uri = if let Some(open) = value.find('<') {
        let prefix = &value[..open];
        if prefix.contains('>') || prefix.contains('<') {
            return None;
        }
        let rest = &value[open + 1..];
        let close = rest.find('>')?;
        let uri = rest[..close].trim();
        let suffix = rest[close + 1..].trim();
        if uri.is_empty()
            || uri.contains(['<', '>'])
            || (!suffix.is_empty() && !suffix.starts_with(';'))
        {
            return None;
        }
        uri
    } else {
        if value.contains(['<', '>']) {
            return None;
        }
        value.split(';').next()?.trim()
    };

    let scheme_end = uri.find(':')?;
    let scheme = &uri[..scheme_end];
    if !scheme.eq_ignore_ascii_case("sip") && !scheme.eq_ignore_ascii_case("sips") {
        return None;
    }
    let authority = &uri[scheme_end + 1..];
    let (userinfo, host) = authority.split_once('@')?;
    if host.trim().is_empty() || host.contains('@') {
        return None;
    }
    let user = userinfo.split(':').next()?.trim();
    if user.is_empty()
        || user
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || matches!(ch, '<' | '>' | '"' | ','))
    {
        return None;
    }
    Some(user)
}

/// 从形如 `<sip:a@b>;tag=xyz;q=0.5` 的头值里取参数值。
///
/// 只按 `;` 切分并做大小写不敏感的名字比较;值两侧的引号会被去掉。
/// 刻意不做完整 URI 解析——SIP 参数名不会出现在 quoted-string 之外的歧义位置。
fn param_value<'a>(value: &'a str, name: &str) -> Option<&'a str> {
    value.split(';').skip(1).find_map(|part| {
        let (k, v) = part.split_once('=')?;
        if k.trim().eq_ignore_ascii_case(name) {
            Some(v.trim().trim_matches('"'))
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 大小写不敏感读写() {
        let mut h = Headers::new();
        h.set("Call-ID", "x@h");
        assert_eq!(h.get("call-id"), Some("x@h"));
        h.set("CALL-ID", "y@h"); // 覆盖
        assert_eq!(h.get("Call-ID"), Some("y@h"));
    }

    #[test]
    fn 多值头() {
        let mut h = Headers::new();
        h.append("Via", "v1");
        h.append("Via", "v2");
        assert_eq!(h.get_all("Via"), vec!["v1", "v2"]);
    }

    #[test]
    fn cseq_序号解析() {
        let mut h = Headers::new();
        h.set("CSeq", "42 REGISTER");
        assert_eq!(h.cseq_number(), Some(42));
        assert_eq!(h.cseq_method(), Some("REGISTER"));
    }

    #[test]
    fn via_branch_与_tag_解析() {
        let mut h = Headers::new();
        h.append(
            "Via",
            "SIP/2.0/UDP 1.2.3.4:5060;rport;branch=z9hG4bK-abc123",
        );
        h.append("Via", "SIP/2.0/UDP 5.6.7.8:5060;branch=z9hG4bK-outer");
        // 只取首个 Via —— 事务标识是最靠上那一跳的 branch。
        assert_eq!(h.via_branch(), Some("z9hG4bK-abc123"));

        h.set("From", "<sip:34020000001320000001@3402.gov>;tag=devtag");
        h.set("To", "<sip:34020000002000000001@3402.gov>;tag=\"srvtag\"");
        assert_eq!(h.from_tag(), Some("devtag"));
        assert_eq!(h.to_tag(), Some("srvtag"), "带引号的值应去引号");

        h.set("To", "<sip:34020000002000000001@3402.gov>");
        assert_eq!(h.to_tag(), None, "无 tag 参数应返回 None");
    }

    #[test]
    fn from_uri_user_严格按_name_addr_解析() {
        let mut h = Headers::new();
        h.set(
            "From",
            "\"伪装 sip:evil@attacker\" <SIP:34020000002000000001@3402.gov>;tag=p",
        );
        assert_eq!(h.from_uri_user(), Some("34020000002000000001"));

        h.set("From", "sip:platform@example.test;tag=p");
        assert_eq!(h.from_uri_user(), Some("platform"));

        for malformed in [
            "Platform <sip:platform@example.test",
            "Platform sip:platform@example.test>",
            "<http://platform@example.test>",
            "<sip:@example.test>",
            "<sip:platform@>",
            "<sip:platform@example.test> trailing",
            "<sip:platform@example.test@evil>",
        ] {
            h.set("From", malformed);
            assert_eq!(h.from_uri_user(), None, "应拒绝: {malformed}");
        }
    }

    #[test]
    fn expires_解析优先头再看_contact_参数() {
        let mut h = Headers::new();
        h.set("Expires", "1800");
        assert_eq!(h.expires(), Some(1800));

        let mut h2 = Headers::new();
        h2.set("Contact", "<sip:dev@1.2.3.4:5060>;expires=600");
        assert_eq!(h2.expires(), None);
        assert_eq!(h2.contact_expires(), Some(600));
    }
}
