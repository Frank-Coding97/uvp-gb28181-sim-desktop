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
    }
}
