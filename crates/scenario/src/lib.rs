//! 压测场景模型。
//!
//! 与 `docs/gb28181.md` 的 YAML 场景一一对应。引擎解析成本模块的强类型结构后驱动调度。
//! M0 定义结构 + YAML 解析,调度逻辑在 `stress-engine`。

use common::{Error, Result};
use serde::{Deserialize, Serialize};

/// 一次压测场景的完整定义。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    /// 场景名称。
    pub name: String,
    /// 上级平台连接信息。
    pub platform: Platform,
    /// 虚拟设备批量生成规则。
    pub devices: Devices,
    /// 注册计划。
    pub register: RegisterPlan,
    /// 心跳计划。
    pub heartbeat: HeartbeatPlan,
    /// 媒体推流计划。
    pub media: MediaPlan,
    /// 压测持续时长(秒)。
    #[serde(default)]
    pub duration_seconds: u64,
}

/// 上级平台连接信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Platform {
    pub host: String,
    pub port: u16,
    /// 平台域(20 位平台 ID 或其中心编码)。
    pub domain: String,
}

/// 虚拟设备批量生成规则。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Devices {
    /// 设备总数。
    pub count: u32,
    /// ID 前缀(后若干位由 start_index 递增补齐到 20 位)。
    pub id_prefix: String,
    /// 起始序号。
    pub start_index: u32,
    /// 统一认证密码。
    pub password: String,
}

/// 注册计划。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterPlan {
    pub enabled: bool,
    /// 爬坡速率:每秒拉起多少台,避免瞬时注册风暴。
    #[serde(default)]
    pub rate_per_second: u32,
}

/// 心跳计划。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatPlan {
    pub enabled: bool,
    #[serde(default)]
    pub interval_seconds: u64,
}

/// 媒体推流强度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaMode {
    /// A 档:空媒体,不推流。
    None,
    /// B 档:轻量伪 PS 包。
    Light,
    /// C 档:真实 H.264/H.265 文件循环。
    Real,
}

/// 媒体推流计划。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaPlan {
    pub enabled: bool,
    pub mode: MediaMode,
    /// 真推流设备占比(0.0~1.0),其余只维持信令。
    #[serde(default)]
    pub active_ratio: f32,
    #[serde(default)]
    pub bitrate_kbps: u32,
    /// 真实码流文件路径(mode=real 时用)。
    #[serde(default)]
    pub file: String,
}

impl Scenario {
    /// 从 YAML 文本解析场景。
    pub fn from_yaml(text: &str) -> Result<Self> {
        // 文档里场景包在 `scenario:` 顶层键下,这里剥掉外层。
        #[derive(Deserialize)]
        struct Wrapper {
            scenario: Scenario,
        }
        serde_yaml::from_str::<Wrapper>(text)
            .map(|w| w.scenario)
            .map_err(|e| Error::Config(format!("场景 YAML 解析失败: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析示例场景() {
        let yaml = r#"
scenario:
  name: "test"
  platform:
    host: "192.168.1.100"
    port: 5060
    domain: "34020000002000000001"
  devices:
    count: 100
    id_prefix: "34020000001320000"
    start_index: 1
    password: "12345678"
  register:
    enabled: true
    rate_per_second: 50
  heartbeat:
    enabled: true
    interval_seconds: 60
  media:
    enabled: false
    mode: none
  duration_seconds: 600
"#;
        let s = Scenario::from_yaml(yaml).unwrap();
        assert_eq!(s.devices.count, 100);
        assert_eq!(s.register.rate_per_second, 50);
        assert_eq!(s.media.mode, MediaMode::None);
    }
}
