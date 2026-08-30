//! 凭据容器:不进日志、离开作用域时擦除内存。
//!
//! # 为什么需要一个专门的类型
//!
//! `DeviceConfig.password` 原来是裸 `String`,而 `DeviceConfig` 派生了 `Debug`。
//! 今天没人写 `tracing::debug!(?config)`,但那是一行代码的距离 —— 一旦有人写了,
//! SIP 密码就会同时落进:标准输出、日志文件、内存环形缓冲、以及前端能拉取的
//! `get_recent_logs`。凭据泄露路径不该依赖"所有人都记得不要打印这个字段"。
//!
//! [`Secret`] 把这件事变成类型问题:
//! - `Debug` / `Display` 只输出 `Secret(<已隐藏>)`,派生 `Debug` 的外层结构自动安全;
//! - 序列化会**报错**而不是静默写出明文 —— 场景 TOML / 压测报告导出路径上,
//!   "忘了脱敏"从运行期事故变成编译期之外的显式失败;
//! - `Drop` 时用 `zeroize` 擦掉底层字节。读取必须显式调 [`Secret::expose`],
//!   于是审计"谁读了密码"只需要 grep 一个方法名。
//!
//! # 这个类型不解决什么
//!
//! 它不防"进程被完整调试"或"内存被换出到未加密的 swap"。擦除只把
//! *存活时间* 从"进程生命周期"缩短到"变量作用域",降低 core dump 与
//! 堆复用后残留的暴露面。真要防前者需要操作系统层的手段(禁 core dump、mlock)。

use zeroize::Zeroize;

/// 一个不进日志、不被序列化、离开作用域即擦除的字符串凭据。
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

/// `Debug` / `Display` 统一输出这个占位符。
const MASK: &str = "Secret(<已隐藏>)";

impl Secret {
    /// 用明文构造。调用点应尽量靠近凭据的来源(钥匙串读取、用户输入)。
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// 显式读出明文。
    ///
    /// 名字刻意刺眼:`grep expose_secret` 就能列出全部读取点,
    /// 而 `config.password` 那种写法在审计时和普通字段毫无区别。
    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    /// 是否为空(用于"未配置密码"的判定,不需要读出明文)。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 明文长度(校验用,同样不需要读出明文)。
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Secret(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Secret(value.to_string())
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(MASK)
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(MASK)
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // zeroize 用 volatile 写 + 编译器屏障,不会被优化掉 ——
        // 手写 `for b in bytes { *b = 0 }` 在 release 下可能被整段删除。
        self.0.zeroize();
    }
}

impl serde::Serialize for Secret {
    /// **恒失败**。凭据不该出现在任何导出物里(场景 TOML、压测报告、诊断包)。
    ///
    /// 之前的做法是"序列化前先把字段改成 `***`",那依赖每个导出路径都记得这么做。
    /// 让序列化本身失败,漏改的那条路径会立刻报错而不是静默泄露。
    fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom(
            "凭据不允许被序列化:导出前请显式替换为占位符",
        ))
    }
}

impl<'de> serde::Deserialize<'de> for Secret {
    /// 允许反序列化:CLI 的场景 TOML 里可以写明文密码(见 `scenario` 的文档注释),
    /// 桌面端则一律从 OS 钥匙串取。**读得进来但写不出去**是有意的不对称。
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Secret)
    }
}

/// 导出物里代替凭据的占位符。
pub const REDACTED_PLACEHOLDER: &str = "***";

/// 给字段用的"序列化成占位符"适配器:`#[serde(serialize_with = "...")]`。
///
/// 与 [`Secret`] 自身"序列化即报错"的默认行为配套:默认拦住所有导出,
/// 而**确实需要出现在导出物里的那个字段**显式声明"我这里写占位符"。
/// 这比"导出前记得把字段改成 `***`"强 —— 后者依赖每条导出路径的作者都记得。
pub fn serialize_redacted<S: serde::Serializer>(
    _secret: &Secret,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(REDACTED_PLACEHOLDER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug与display都不泄露明文() {
        let s = Secret::new("12345678");
        assert_eq!(format!("{s:?}"), MASK);
        assert_eq!(format!("{s}"), MASK);
        assert!(!format!("{s:?}").contains("12345678"));
    }

    #[test]
    fn 派生debug的外层结构自动安全() {
        // 这是这个类型存在的核心理由:外层结构派生 Debug 时不需要任何额外注意。
        #[derive(Debug)]
        #[allow(dead_code)] // 字段只经 Debug 输出使用
        struct Config {
            user: String,
            password: Secret,
        }
        let c = Config {
            user: "34020000001320000001".into(),
            password: Secret::new("s3cr3t"),
        };
        let dumped = format!("{c:?}");
        assert!(
            dumped.contains("34020000001320000001"),
            "非凭据字段照常输出"
        );
        assert!(
            !dumped.contains("s3cr3t"),
            "凭据不该出现在 Debug 里: {dumped}"
        );
    }

    #[test]
    fn 序列化恒失败而不是静默写出明文() {
        let s = Secret::new("12345678");
        let err = serde_json::to_string(&s).expect_err("序列化必须失败");
        assert!(err.to_string().contains("不允许被序列化"), "实际: {err}");

        // 外层结构里带 Secret 时整体序列化也失败 —— 漏改的导出路径会立刻暴露。
        #[derive(serde::Serialize)]
        struct Exported {
            name: String,
            password: Secret,
        }
        assert!(serde_json::to_string(&Exported {
            name: "run-1".into(),
            password: Secret::new("x"),
        })
        .is_err());
    }

    #[test]
    fn 可以反序列化进来() {
        // CLI 场景 TOML 允许明文密码;桌面端走钥匙串。读得进写不出是有意的不对称。
        let s: Secret = serde_json::from_str("\"abc\"").expect("应能反序列化");
        assert_eq!(s.expose_secret(), "abc");
    }

    #[test]
    fn 基本访问器不需要读出明文() {
        assert!(Secret::default().is_empty());
        assert!(Secret::new("").is_empty());
        let s = Secret::new("1234");
        assert!(!s.is_empty());
        assert_eq!(s.len(), 4);
        assert_eq!(s.expose_secret(), "1234");
    }

    #[test]
    fn 字段级占位符序列化() {
        #[derive(serde::Serialize)]
        struct Scenario {
            name: String,
            #[serde(serialize_with = "serialize_redacted")]
            password: Secret,
        }
        let out = serde_json::to_string(&Scenario {
            name: "run-1".into(),
            password: Secret::new("real-password"),
        })
        .expect("显式声明占位符后应能序列化");
        assert!(!out.contains("real-password"));
        assert!(out.contains(REDACTED_PLACEHOLDER));
    }

    #[test]
    fn 相等性与克隆保持语义() {
        let a = Secret::new("same");
        let b = a.clone();
        assert_eq!(a, b);
        assert_ne!(a, Secret::new("other"));
    }
}
