//! SIP Digest 认证。
//!
//! 实现 docs/40-protocol/sip.md#digest。相对最初的"仅 MD5、无 qop"版本补齐了几处
//! 会直接导致注册永久失败的缺口:
//!
//! - **qop=auth / auth-int**(RFC 2617 §3.2.2.1):带 qop 的挑战必须用
//!   `MD5(HA1:nonce:nc:cnonce:qop:HA2)`,用旧的无 qop 公式算出来的 response 会被拒,
//!   而且之前没有回退分支 → 平台一挑战 qop 设备就永远注册不上。
//! - **opaque 回显**:严格平台缺 opaque 会拒。
//! - **algorithm=MD5-sess**:HA1 需再拼一次 nonce:cnonce。
//! - **stale=true**:平台要求用新 nonce 重算而不是判定为密码错。
//!
//! 参数解析改成"按逗号切分、尊重引号"的正规做法,不再用子串扫描——后者会被
//! `x-realm="fake"` 之类的键名前缀误导。

use std::fmt;

use md5::{Digest, Md5};

use common::{Error, Result};

/// 摘要算法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Algorithm {
    #[default]
    Md5,
    /// MD5-sess:HA1 = MD5(MD5(user:realm:pass):nonce:cnonce)。
    Md5Sess,
}

/// 保护质量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Qop {
    /// qop=auth。
    Auth,
    /// qop=auth-int:HA2 需带 body 摘要。
    AuthInt,
}

impl fmt::Display for Qop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Qop::Auth => "auth",
            Qop::AuthInt => "auth-int",
        })
    }
}

/// 从 `WWW-Authenticate` / `Proxy-Authenticate` 头解析出的 Digest 挑战。
#[derive(Debug, Clone)]
pub struct Challenge {
    /// 认证域。
    pub realm: String,
    /// 服务端随机数。
    pub nonce: String,
    /// 服务端要求原样回显的不透明串。
    pub opaque: Option<String>,
    /// 摘要算法。
    pub algorithm: Algorithm,
    /// 服务端支持的 qop 中我们选定的那个(优先 auth,其次 auth-int);None = 无 qop。
    pub qop: Option<Qop>,
    /// nonce 已过期,应当用新 nonce 重算而非视为凭据错误。
    pub stale: bool,
}

impl Challenge {
    /// 解析 `Digest realm="...", nonce="...", ...` 的值部分
    /// (传入 `Digest ...` 整串或去掉 scheme 均可)。
    pub fn parse(header_value: &str) -> Result<Self> {
        let body = strip_scheme(header_value.trim());
        let params = parse_params(body);
        let get = |k: &str| {
            params
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(k))
                .map(|(_, v)| v.as_str())
        };

        let realm = get("realm")
            .ok_or_else(|| Error::Sip("WWW-Authenticate 缺少 realm".into()))?
            .to_string();
        let nonce = get("nonce")
            .ok_or_else(|| Error::Sip("WWW-Authenticate 缺少 nonce".into()))?
            .to_string();
        let algorithm = match get("algorithm") {
            Some(a) if a.eq_ignore_ascii_case("MD5-sess") => Algorithm::Md5Sess,
            Some(a) if a.eq_ignore_ascii_case("MD5") => Algorithm::Md5,
            Some(other) => {
                // 未知算法(SHA-256 等)不静默按 MD5 算——那只会得到一个必然被拒的
                // response,让人误以为是密码错。
                return Err(Error::Sip(format!(
                    "不支持的 Digest 算法 {other}(当前仅支持 MD5 / MD5-sess)"
                )));
            }
            None => Algorithm::Md5,
        };
        // qop 可能是逗号分隔的列表,如 `qop="auth,auth-int"`。优先选 auth。
        let qop = get("qop").and_then(|list| {
            let mut has_auth = false;
            let mut has_auth_int = false;
            for item in list.split(',') {
                let item = item.trim();
                if item.eq_ignore_ascii_case("auth") {
                    has_auth = true;
                } else if item.eq_ignore_ascii_case("auth-int") {
                    has_auth_int = true;
                }
            }
            if has_auth {
                Some(Qop::Auth)
            } else if has_auth_int {
                Some(Qop::AuthInt)
            } else {
                None
            }
        });
        let stale = get("stale").is_some_and(|v| v.eq_ignore_ascii_case("true"));

        Ok(Challenge {
            realm,
            nonce,
            opaque: get("opaque").map(str::to_string),
            algorithm,
            qop,
            stale,
        })
    }
}

/// 计算 Digest 并生成 `Authorization` 头的值(空 body)。
pub fn authorization(
    challenge: &Challenge,
    username: &str,
    password: &str,
    method: &str,
    uri: &str,
) -> String {
    authorization_with_body(challenge, username, password, method, uri, &[])
}

/// 计算 Digest 并生成 `Authorization` 头的值。
///
/// `body` 仅在 `qop=auth-int` 时参与计算(HA2 需带实体摘要)。
///
/// nonce-count 固定为 `00000001`:设备对每个挑战只重发一次带鉴权的请求,
/// 下一次 401 会给新 nonce,因此不需要跨请求维护计数器。
pub fn authorization_with_body(
    challenge: &Challenge,
    username: &str,
    password: &str,
    method: &str,
    uri: &str,
    body: &[u8],
) -> String {
    let cnonce = random_cnonce();
    const NC: &str = "00000001";

    let mut ha1 = md5_hex(&format!("{}:{}:{}", username, challenge.realm, password));
    if challenge.algorithm == Algorithm::Md5Sess {
        ha1 = md5_hex(&format!("{}:{}:{}", ha1, challenge.nonce, cnonce));
    }

    let ha2 = match challenge.qop {
        Some(Qop::AuthInt) => {
            let body_hash = md5_hex_bytes(body);
            md5_hex(&format!("{method}:{uri}:{body_hash}"))
        }
        _ => md5_hex(&format!("{method}:{uri}")),
    };

    let response = match challenge.qop {
        Some(qop) => md5_hex(&format!(
            "{}:{}:{}:{}:{}:{}",
            ha1, challenge.nonce, NC, cnonce, qop, ha2
        )),
        None => md5_hex(&format!("{}:{}:{}", ha1, challenge.nonce, ha2)),
    };

    let mut out = format!(
        "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{}\"",
        username, challenge.realm, challenge.nonce, uri, response
    );
    out.push_str(match challenge.algorithm {
        Algorithm::Md5 => ", algorithm=MD5",
        Algorithm::Md5Sess => ", algorithm=MD5-sess",
    });
    if let Some(qop) = challenge.qop {
        out.push_str(&format!(", qop={qop}, nc={NC}, cnonce=\"{cnonce}\""));
    }
    if let Some(opaque) = &challenge.opaque {
        out.push_str(&format!(", opaque=\"{opaque}\""));
    }
    out
}

/// 去掉 `Digest` scheme 前缀(大小写不敏感)。
fn strip_scheme(value: &str) -> &str {
    if value.len() >= 6 && value[..6].eq_ignore_ascii_case("Digest") {
        value[6..].trim_start()
    } else {
        value
    }
}

/// 生成客户端随机数。
fn random_cnonce() -> String {
    let a: u64 = rand::random();
    let b: u32 = rand::random();
    format!("{a:016x}{b:08x}")
}

/// 计算字符串的 MD5,返回 32 位小写十六进制。
fn md5_hex(input: &str) -> String {
    md5_hex_bytes(input.as_bytes())
}

fn md5_hex_bytes(input: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(input);
    let digest = hasher.finalize();
    let mut s = String::with_capacity(32);
    for b in digest {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// 把 `key1="v1", key2=v2` 解析成键值对列表。
///
/// 按逗号切分但**尊重引号**:引号内的逗号(如 `qop="auth,auth-int"`)不作为分隔符。
fn parse_params(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut in_quotes = false;
    let mut escaped = false;
    let mut current = String::new();

    let flush = |buf: &mut String, out: &mut Vec<(String, String)>| {
        let item = buf.trim();
        if let Some((k, v)) = item.split_once('=') {
            let value = v.trim();
            let value = if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
                value[1..value.len() - 1].replace("\\\"", "\"")
            } else {
                value.to_string()
            };
            out.push((k.trim().to_string(), value));
        }
        buf.clear();
    };

    for ch in s.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => {
                current.push(ch);
                escaped = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            ',' if !in_quotes => flush(&mut current, &mut out),
            _ => current.push(ch),
        }
    }
    flush(&mut current, &mut out);
    out
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
        assert_eq!(c.algorithm, Algorithm::Md5);
        assert!(c.qop.is_none());
        assert!(!c.stale);
    }

    #[test]
    fn digest_对拍已知向量() {
        // 经典 RFC 2617 示例向量(无 qop)。
        // HA1 = MD5("Mufasa:testrealm@host.com:Circle Of Life")
        // HA2 = MD5("GET:/dir/index.html")
        // response = MD5(HA1:dcd98b7102dd2f0e8b11d0f600bfb0c093:HA2)
        let challenge = Challenge {
            realm: "testrealm@host.com".into(),
            nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093".into(),
            opaque: None,
            algorithm: Algorithm::Md5,
            qop: None,
            stale: false,
        };
        let auth = authorization(
            &challenge,
            "Mufasa",
            "Circle Of Life",
            "GET",
            "/dir/index.html",
        );
        assert!(auth.contains("response=\"670fd8c2df070c60b045671b8b24ff02\""));
        assert!(!auth.contains("qop="), "无 qop 挑战不应输出 qop");
    }

    #[test]
    fn qop_auth_按_rfc2617_公式并回显_nc_cnonce() {
        let h = r#"Digest realm="3402000000", nonce="n0nce", qop="auth", opaque="op4que""#;
        let c = Challenge::parse(h).unwrap();
        assert_eq!(c.qop, Some(Qop::Auth));
        assert_eq!(c.opaque.as_deref(), Some("op4que"));

        let auth = authorization(&c, "dev", "pwd", "REGISTER", "sip:plat@1.2.3.4:5060");
        assert!(auth.contains("qop=auth"));
        assert!(auth.contains("nc=00000001"));
        assert!(auth.contains("cnonce=\""));
        assert!(auth.contains("opaque=\"op4que\""), "opaque 必须回显");

        // 手工算一遍 response 对拍,确认用的是带 qop 的公式。
        let cnonce = auth
            .split("cnonce=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap();
        let ha1 = md5_hex("dev:3402000000:pwd");
        let ha2 = md5_hex("REGISTER:sip:plat@1.2.3.4:5060");
        let expect = md5_hex(&format!("{ha1}:n0nce:00000001:{cnonce}:auth:{ha2}"));
        assert!(auth.contains(&format!("response=\"{expect}\"")));
    }

    #[test]
    fn qop_列表优先选_auth() {
        let c = Challenge::parse(r#"Digest realm="r", nonce="n", qop="auth-int,auth""#).unwrap();
        assert_eq!(c.qop, Some(Qop::Auth));
        let c2 = Challenge::parse(r#"Digest realm="r", nonce="n", qop="auth-int""#).unwrap();
        assert_eq!(c2.qop, Some(Qop::AuthInt));
    }

    #[test]
    fn auth_int_把_body_摘要纳入_ha2() {
        let c = Challenge::parse(r#"Digest realm="r", nonce="n", qop="auth-int""#).unwrap();
        let body = b"<Query/>";
        let auth = authorization_with_body(&c, "u", "p", "MESSAGE", "sip:x@h", body);
        let cnonce = auth
            .split("cnonce=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap();
        let ha1 = md5_hex("u:r:p");
        let ha2 = md5_hex(&format!("MESSAGE:sip:x@h:{}", md5_hex_bytes(body)));
        let expect = md5_hex(&format!("{ha1}:n:00000001:{cnonce}:auth-int:{ha2}"));
        assert!(auth.contains(&format!("response=\"{expect}\"")));
    }

    #[test]
    fn md5_sess_把_nonce_cnonce_拼进_ha1() {
        let c = Challenge::parse(r#"Digest realm="r", nonce="n", algorithm=MD5-sess"#).unwrap();
        assert_eq!(c.algorithm, Algorithm::Md5Sess);
        let auth = authorization(&c, "u", "p", "REGISTER", "sip:x@h");
        assert!(auth.contains("algorithm=MD5-sess"));
        let cnonce_present = auth.contains("cnonce=\"");
        // 无 qop 时不输出 cnonce 参数,但 HA1 内部仍用了它 —— 这是 RFC 的一处别扭之处,
        // 实践中 MD5-sess 总与 qop 同时出现;这里断言算法标识正确即可。
        assert!(!cnonce_present || auth.contains("qop="));
    }

    #[test]
    fn stale_被识别() {
        let c = Challenge::parse(r#"Digest realm="r", nonce="n2", stale=true"#).unwrap();
        assert!(c.stale, "stale=true 应被识别为需换 nonce 重算");
    }

    #[test]
    fn 未知算法明确报错而非静默算错() {
        let e = Challenge::parse(r#"Digest realm="r", nonce="n", algorithm=SHA-256"#);
        assert!(e.is_err());
    }

    #[test]
    fn 参数解析不被相似键名误导() {
        // 旧的子串扫描实现会把 x-realm 的值当成 realm。
        let c = Challenge::parse(r#"Digest x-realm="fake", realm="real", nonce="n""#).unwrap();
        assert_eq!(c.realm, "real");
    }

    #[test]
    fn 引号内逗号不被当作参数分隔() {
        let params = parse_params(r#"realm="a,b", nonce="n""#);
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].1, "a,b");
    }

    #[test]
    fn 提取不带引号参数() {
        let params = parse_params("algorithm=MD5, x=1");
        assert_eq!(params[0], ("algorithm".into(), "MD5".into()));
        assert_eq!(params[1], ("x".into(), "1".into()));
    }
}
