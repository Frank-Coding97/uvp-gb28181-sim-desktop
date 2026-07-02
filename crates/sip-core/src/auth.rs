//! SIP Digest 认证(MD5)。
//!
//! 实现 docs/40-protocol/sip.md#digest:解析 401 的 `WWW-Authenticate` 挑战,
//! 计算 `response = MD5(HA1:nonce:HA2)`,构造 `Authorization` 头值。
//! GB28181 用 MD5、qop 通常缺省(不带 qop 的经典算法),此处按经典 RFC 2617 实现。

use md5::{Digest, Md5};

use common::{Error, Result};

/// 从 `WWW-Authenticate` 头解析出的 Digest 挑战。
#[derive(Debug, Clone)]
pub struct Challenge {
    /// 认证域。
    pub realm: String,
    /// 服务端随机数。
    pub nonce: String,
}

impl Challenge {
    /// 解析 `WWW-Authenticate: Digest realm="...", nonce="...", ...` 的值部分
    /// (传入 `Digest ...` 整串或去掉 scheme 均可)。
    pub fn parse(header_value: &str) -> Result<Self> {
        let body = header_value
            .trim()
            .strip_prefix("Digest")
            .unwrap_or(header_value)
            .trim();
        let realm = extract_param(body, "realm")
            .ok_or_else(|| Error::Sip("WWW-Authenticate 缺少 realm".into()))?;
        let nonce = extract_param(body, "nonce")
            .ok_or_else(|| Error::Sip("WWW-Authenticate 缺少 nonce".into()))?;
        Ok(Challenge { realm, nonce })
    }
}

/// 计算 Digest 并生成 `Authorization` 头的值。
///
/// - `method`:SIP 方法名(如 REGISTER)
/// - `uri`:Request-URI(与请求行一致)
///
/// 返回形如 `Digest username="...", realm="...", nonce="...", uri="...", response="..."`。
pub fn authorization(
    challenge: &Challenge,
    username: &str,
    password: &str,
    method: &str,
    uri: &str,
) -> String {
    let ha1 = md5_hex(&format!("{}:{}:{}", username, challenge.realm, password));
    let ha2 = md5_hex(&format!("{}:{}", method, uri));
    let response = md5_hex(&format!("{}:{}:{}", ha1, challenge.nonce, ha2));
    format!(
        "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{}\", algorithm=MD5",
        username, challenge.realm, challenge.nonce, uri, response
    )
}

/// 计算字符串的 MD5,返回 32 位小写十六进制。
fn md5_hex(input: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    let mut s = String::with_capacity(32);
    for b in digest {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// 从 `key1="v1", key2=v2` 形式中提取参数值(支持带引号或不带引号)。
fn extract_param(s: &str, key: &str) -> Option<String> {
    // 逐个查找 `key=`,确保是完整键(前面是边界)。
    let mut search_from = 0;
    while let Some(pos) = s[search_from..].find(key) {
        let abs = search_from + pos;
        let before_ok = abs == 0 || !s.as_bytes()[abs - 1].is_ascii_alphanumeric();
        let after = &s[abs + key.len()..];
        let after_trim = after.trim_start();
        if before_ok && after_trim.starts_with('=') {
            let val = after_trim[1..].trim_start();
            if let Some(rest) = val.strip_prefix('"') {
                // 带引号:取到下一个引号。
                if let Some(end) = rest.find('"') {
                    return Some(rest[..end].to_string());
                }
            } else {
                // 不带引号:取到逗号或结尾。
                let end = val.find(',').unwrap_or(val.len());
                return Some(val[..end].trim().to_string());
            }
        }
        search_from = abs + key.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析挑战() {
        let h = r#"Digest realm="3402000000", nonce="abcdef123456", algorithm=MD5"#;
        let c = Challenge::parse(h).unwrap();
        assert_eq!(c.realm, "3402000000");
        assert_eq!(c.nonce, "abcdef123456");
    }

    #[test]
    fn digest_对拍已知向量() {
        // 经典 RFC 2617 示例向量。
        // HA1 = MD5("Mufasa:testrealm@host.com:Circle Of Life")
        // HA2 = MD5("GET:/dir/index.html")
        // response = MD5(HA1:dcd98b7102dd2f0e8b11d0f600bfb0c093:HA2)
        let challenge = Challenge {
            realm: "testrealm@host.com".into(),
            nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093".into(),
        };
        let auth = authorization(&challenge, "Mufasa", "Circle Of Life", "GET", "/dir/index.html");
        assert!(auth.contains("response=\"670fd8c2df070c60b045671b8b24ff02\""));
    }

    #[test]
    fn 提取不带引号参数() {
        assert_eq!(extract_param("algorithm=MD5, x=1", "algorithm"), Some("MD5".into()));
    }
}
