use std::{collections::HashSet, fmt, net::IpAddr};

use common::{DeviceId, GbVersion, SignalingEncoding, Transport};
use serde::{Deserialize, Serialize};

pub const CONFIG_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DesktopConfigV1 {
    pub schema_version: u32,
    pub active_profile_id: String,
    pub profiles: Vec<PlatformProfileConfig>,
    pub device: DeviceSettings,
    pub network: NetworkSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlatformProfileConfig {
    pub id: String,
    pub name: String,
    pub server_host: String,
    pub server_port: u16,
    pub server_id: String,
    pub server_domain: String,
    pub transport: Transport,
    pub gb_version: GbVersion,
    pub signaling_encoding: SignalingEncoding,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceSettings {
    pub device_id: String,
    pub device_name: String,
    pub manufacturer: String,
    pub model: String,
    pub firmware: String,
    pub channel_name: String,
    pub register_expires_secs: u32,
    pub heartbeat_interval_secs: u64,
    pub heartbeat_fail_threshold: u32,
    pub video_fps: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BindMode {
    #[default]
    Auto,
    Specific,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkSettings {
    pub bind_mode: BindMode,
    pub bind_address: String,
    pub sip_trace: bool,
}

#[derive(Clone, Deserialize)]
pub struct StartDeviceInput {
    pub profile_id: String,
    pub password: String,
    #[serde(default)]
    pub video_source: Option<String>,
    #[serde(default)]
    pub catalog_template: String,
}

impl fmt::Debug for StartDeviceInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StartDeviceInput")
            .field("profile_id", &self.profile_id)
            .field("password", &"<redacted>")
            .field("video_source", &self.video_source)
            .field("catalog_template", &self.catalog_template)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CapabilitySnapshot {
    pub signaling_udp: bool,
    pub signaling_tcp: bool,
}

impl Default for CapabilitySnapshot {
    fn default() -> Self {
        Self {
            signaling_udp: true,
            signaling_tcp: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EffectiveDeviceConfig {
    pub profile: PlatformProfileConfig,
    pub device: DeviceSettings,
    pub network: NetworkSettings,
    pub local_host: String,
    pub local_port: u16,
    pub video_source: Option<String>,
    pub catalog_template: String,
    pub capabilities: CapabilitySnapshot,
}

impl Default for DesktopConfigV1 {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            active_profile_id: "local".into(),
            profiles: vec![PlatformProfileConfig {
                id: "local".into(),
                name: "本地示例".into(),
                server_host: "127.0.0.1".into(),
                server_port: 5060,
                server_id: "34020000002000000001".into(),
                server_domain: "3402000000".into(),
                transport: Transport::Udp,
                gb_version: GbVersion::V2022,
                signaling_encoding: SignalingEncoding::Gb18030,
            }],
            device: DeviceSettings {
                device_id: "35020000001310000001".into(),
                device_name: "UVP-Sim-Desktop".into(),
                manufacturer: "UVP".into(),
                model: "Desktop-Sim".into(),
                firmware: "0.1.2".into(),
                channel_name: "Camera-1".into(),
                register_expires_secs: 86_400,
                heartbeat_interval_secs: 60,
                heartbeat_fail_threshold: 3,
                video_fps: 30,
            },
            network: NetworkSettings {
                bind_mode: BindMode::Auto,
                bind_address: String::new(),
                sip_trace: true,
            },
        }
    }
}

impl DesktopConfigV1 {
    pub fn active_profile(&self) -> Option<&PlatformProfileConfig> {
        self.profiles
            .iter()
            .find(|profile| profile.id == self.active_profile_id)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(format!(
                "不支持的配置版本: {}（当前仅支持 {}）",
                self.schema_version, CONFIG_SCHEMA_VERSION
            ));
        }
        if self.profiles.is_empty() {
            return Err("至少需要一个平台档案".into());
        }
        let mut profile_ids = HashSet::new();
        for profile in &self.profiles {
            if profile.id.trim().is_empty() || !profile_ids.insert(profile.id.as_str()) {
                return Err("平台档案 ID 不能为空且不能重复".into());
            }
            if profile.name.trim().is_empty() {
                return Err(format!("平台档案 {} 的名称不能为空", profile.id));
            }
            if profile.server_host.parse::<IpAddr>().is_err() || profile.server_port == 0 {
                return Err(format!("平台档案 {} 的平台 IP/端口无效", profile.id));
            }
            DeviceId::new(profile.server_id.clone())
                .map_err(|_| format!("平台档案 {} 的平台 ID 必须是 20 位数字", profile.id))?;
            if profile.server_domain.len() != 10
                || !profile
                    .server_domain
                    .bytes()
                    .all(|byte| byte.is_ascii_digit())
            {
                return Err(format!("平台档案 {} 的 SIP 域必须是 10 位数字", profile.id));
            }
        }
        if self.active_profile().is_none() {
            return Err(format!(
                "活动平台档案不存在: {}",
                self.active_profile_id
            ));
        }

        DeviceId::new(self.device.device_id.clone())
            .map_err(|_| "设备 ID 必须是 20 位数字".to_string())?;
        if self.device.device_name.trim().is_empty()
            || self.device.manufacturer.trim().is_empty()
            || self.device.model.trim().is_empty()
            || self.device.firmware.trim().is_empty()
            || self.device.channel_name.trim().is_empty()
        {
            return Err("设备名称、厂商、型号、固件和通道名称不能为空".into());
        }
        if self.device.register_expires_secs < 3_600 {
            return Err("注册有效期不得短于 3600 秒".into());
        }
        if self.device.heartbeat_interval_secs == 0 {
            return Err("心跳间隔必须大于 0 秒".into());
        }
        if self.device.heartbeat_fail_threshold == 0 {
            return Err("连续心跳失败阈值必须大于 0".into());
        }
        if !(1..=120).contains(&self.device.video_fps) {
            return Err("视频帧率必须在 1..=120 之间".into());
        }
        if self.network.bind_mode == BindMode::Specific
            && self.network.bind_address.parse::<IpAddr>().is_err()
        {
            return Err("指定绑定地址必须是有效 IP".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{GbVersion, SignalingEncoding, Transport};

    #[test]
    fn 默认配置通过校验且不包含秘密字段() {
        let config = DesktopConfigV1::default();
        config.validate().unwrap();

        let json = serde_json::to_string(&config).unwrap().to_ascii_lowercase();
        assert!(!json.contains("password"));
        assert!(!json.contains("authorization"));
        assert_eq!(config.schema_version, 1);
        assert_eq!(config.device.register_expires_secs, 86_400);
    }

    #[test]
    fn 平台id与域分别保留() {
        let mut config = DesktopConfigV1::default();
        let profile = &mut config.profiles[0];
        profile.server_id = "34020000002000000001".into();
        profile.server_domain = "3402000000".into();

        config.validate().unwrap();
        let profile = &config.profiles[0];
        assert_ne!(profile.server_id, profile.server_domain);
    }

    #[test]
    fn 非法身份注册周期和网络地址被拒绝() {
        let mut invalid_id = DesktopConfigV1::default();
        invalid_id.profiles[0].server_id = "123".into();
        assert!(invalid_id.validate().unwrap_err().contains("平台 ID"));

        let mut invalid_expires = DesktopConfigV1::default();
        invalid_expires.device.register_expires_secs = 3_599;
        assert!(invalid_expires.validate().unwrap_err().contains("3600"));

        let mut invalid_bind = DesktopConfigV1::default();
        invalid_bind.network.bind_mode = BindMode::Specific;
        invalid_bind.network.bind_address = "not-an-ip".into();
        assert!(invalid_bind.validate().unwrap_err().contains("绑定地址"));
    }

    #[test]
    fn tcp档案可保存但能力快照诚实() {
        let mut config = DesktopConfigV1::default();
        config.profiles[0].transport = Transport::Tcp;
        config.validate().unwrap();

        let capabilities = CapabilitySnapshot::default();
        assert!(capabilities.signaling_udp);
        assert!(!capabilities.signaling_tcp);
    }

    #[test]
    fn 会话启动输入debug不泄露密码() {
        let input = StartDeviceInput {
            profile_id: "local".into(),
            password: "top-secret".into(),
            video_source: None,
            catalog_template: String::new(),
        };
        let debug = format!("{input:?}");
        assert!(!debug.contains("top-secret"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn dto保留类型化版本和编码() {
        let config = DesktopConfigV1::default();
        assert_eq!(config.profiles[0].gb_version, GbVersion::V2022);
        assert_eq!(
            config.profiles[0].signaling_encoding,
            SignalingEncoding::Gb18030
        );
    }
}
