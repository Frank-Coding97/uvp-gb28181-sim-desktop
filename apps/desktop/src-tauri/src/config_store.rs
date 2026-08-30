use std::{
    collections::HashSet,
    fmt,
    fs::{self, OpenOptions},
    io::Write,
    net::IpAddr,
    path::PathBuf,
};

use common::{DeviceId, GbVersion, SignalingEncoding, Transport};
use serde::{Deserialize, Serialize};

pub const CONFIG_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Result<DesktopConfigV1, String> {
        if !self.path.exists() {
            return Ok(DesktopConfigV1::default());
        }
        let bytes = fs::read(&self.path).map_err(|error| {
            format!("读取配置失败 {}: {error}", self.path.display())
        })?;
        let config: DesktopConfigV1 = serde_json::from_slice(&bytes).map_err(|error| {
            format!("配置 JSON 损坏 {}: {error}", self.path.display())
        })?;
        config
            .validate()
            .map_err(|error| format!("配置校验失败 {}: {error}", self.path.display()))?;
        Ok(config)
    }

    pub fn save(&self, config: &DesktopConfigV1) -> Result<DesktopConfigV1, String> {
        config
            .validate()
            .map_err(|error| format!("配置校验失败: {error}"))?;
        let bytes = serde_json::to_vec_pretty(config)
            .map_err(|error| format!("配置序列化失败: {error}"))?;
        self.atomic_write(&bytes)?;
        Ok(config.clone())
    }

    pub fn reset(&self) -> Result<DesktopConfigV1, String> {
        self.save(&DesktopConfigV1::default())
    }

    fn atomic_write(&self, bytes: &[u8]) -> Result<(), String> {
        let parent = self.path.parent().ok_or_else(|| {
            format!("配置路径缺少父目录: {}", self.path.display())
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            format!("创建配置目录失败 {}: {error}", parent.display())
        })?;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("desktop-config-v1.json");
        let temp_path = parent.join(format!(
            ".{file_name}.tmp-{}-{nonce}",
            std::process::id()
        ));

        let write_result = (|| -> Result<(), String> {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp_path)
                .map_err(|error| {
                    format!("创建配置临时文件失败 {}: {error}", temp_path.display())
                })?;
            file.write_all(bytes).map_err(|error| {
                format!("写入配置临时文件失败 {}: {error}", temp_path.display())
            })?;
            file.sync_all().map_err(|error| {
                format!("同步配置临时文件失败 {}: {error}", temp_path.display())
            })?;
            fs::rename(&temp_path, &self.path).map_err(|error| {
                format!(
                    "原子替换配置失败 {} -> {}: {error}",
                    temp_path.display(),
                    self.path.display()
                )
            })?;
            #[cfg(unix)]
            {
                let directory = fs::File::open(parent).map_err(|error| {
                    format!("打开配置目录失败 {}: {error}", parent.display())
                })?;
                directory.sync_all().map_err(|error| {
                    format!("同步配置目录失败 {}: {error}", parent.display())
                })?;
            }
            Ok(())
        })();

        if write_result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        write_result
    }
}

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

pub struct ResolvedStartConfig {
    pub profile: PlatformProfileConfig,
    pub device: DeviceSettings,
    pub network: NetworkSettings,
    pub password: String,
    pub video_source: Option<String>,
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

    pub fn resolve_start(&self, mut input: StartDeviceInput) -> Result<ResolvedStartConfig, String> {
        self.validate()?;
        if input.password.trim().is_empty() {
            return Err("SIP 认证密码不能为空".into());
        }
        let profile = self
            .profiles
            .iter()
            .find(|profile| profile.id == input.profile_id)
            .cloned()
            .ok_or_else(|| format!("平台档案不存在: {}", input.profile_id))?;
        input.video_source = input
            .video_source
            .take()
            .map(|source| source.trim().to_string())
            .filter(|source| !source.is_empty());
        Ok(ResolvedStartConfig {
            profile,
            device: self.device.clone(),
            network: self.network.clone(),
            password: input.password,
            video_source: input.video_source,
            catalog_template: input.catalog_template.trim().to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{GbVersion, SignalingEncoding, Transport};

    fn temp_config_path(name: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!("uvp-desktop-config-test-{}-{nonce}", std::process::id()))
            .join(name)
    }

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

    #[test]
    fn 配置文件round_trip且目录内无临时残留() {
        let path = temp_config_path("desktop-config-v1.json");
        let store = ConfigStore::new(path.clone());
        let mut config = DesktopConfigV1::default();
        config.device.device_name = "Round Trip".into();

        store.save(&config).unwrap();
        assert_eq!(store.load().unwrap(), config);
        let names = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["desktop-config-v1.json"]);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn 不存在返回默认值但不主动落盘() {
        let path = temp_config_path("desktop-config-v1.json");
        let store = ConfigStore::new(path.clone());
        assert_eq!(store.load().unwrap(), DesktopConfigV1::default());
        assert!(!path.exists());
    }

    #[test]
    fn 损坏json报路径且不覆盖原文件() {
        let path = temp_config_path("desktop-config-v1.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{broken-json").unwrap();
        let store = ConfigStore::new(path.clone());

        let error = store.load().unwrap_err();
        assert!(error.contains(path.to_string_lossy().as_ref()));
        assert_eq!(std::fs::read(&path).unwrap(), b"{broken-json");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn 无效保存不破坏旧文件且显式reset可恢复() {
        let path = temp_config_path("desktop-config-v1.json");
        let store = ConfigStore::new(path.clone());
        let mut original = DesktopConfigV1::default();
        original.device.device_name = "Keep Me".into();
        store.save(&original).unwrap();

        let mut invalid = original.clone();
        invalid.device.register_expires_secs = 1;
        assert!(store.save(&invalid).is_err());
        assert_eq!(store.load().unwrap(), original);

        assert_eq!(store.reset().unwrap(), DesktopConfigV1::default());
        assert_eq!(store.load().unwrap(), DesktopConfigV1::default());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn 启动输入只补充会话字段并解析已保存档案() {
        let config = DesktopConfigV1::default();
        let resolved = config
            .resolve_start(StartDeviceInput {
                profile_id: "local".into(),
                password: "session-secret".into(),
                video_source: Some("  live:0  ".into()),
                catalog_template: " single ".into(),
            })
            .unwrap();
        assert_eq!(resolved.profile.server_id, "34020000002000000001");
        assert_eq!(resolved.profile.server_domain, "3402000000");
        assert_eq!(resolved.password, "session-secret");
        assert_eq!(resolved.video_source.as_deref(), Some("live:0"));
        assert_eq!(resolved.catalog_template, "single");
    }

    #[test]
    fn 启动拒绝未知档案和空密码() {
        let config = DesktopConfigV1::default();
        let unknown = config.resolve_start(StartDeviceInput {
            profile_id: "missing".into(),
            password: "x".into(),
            video_source: None,
            catalog_template: String::new(),
        });
        assert!(unknown.err().unwrap().contains("平台档案不存在"));

        let empty_password = config.resolve_start(StartDeviceInput {
            profile_id: "local".into(),
            password: "  ".into(),
            video_source: None,
            catalog_template: String::new(),
        });
        assert!(empty_password.err().unwrap().contains("密码不能为空"));
    }
}
