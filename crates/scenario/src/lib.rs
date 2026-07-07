//! 压测场景:批量生成设备配置的策略。
//!
//! 实现 docs/10-functional/stress-testing.md §1:从基础配置生成 N 个设备,
//! ID 递增,分档设置媒体强度。M3 实现 LinearScenario(连续 ID),支持 TOML 配置。

use common::{DeviceId, GbVersion, Result, Transport};
use gb28181_simulator::{ChannelConfig, DeviceConfig, DeviceInfo};
use serde::{Deserialize, Serialize};

/// 场景:批量生成设备配置的策略。
pub trait Scenario {
    /// 生成 `count` 个设备配置。
    fn generate(&self, count: usize) -> Result<Vec<DeviceConfig>>;

    /// 爬坡速率:每秒拉起多少台设备(0 = 一次性全拉起)。默认 0。
    fn ramp_per_second(&self) -> u32 {
        0
    }
}

/// 线性场景:基础配置 + 连续递增 ID。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinearScenario {
    /// 基础设备 ID(起点,20 位数字)。
    pub base_device_id: String,
    /// SIP 认证密码(所有设备共享)。
    pub password: String,
    /// 平台 host。
    pub server_host: String,
    /// 平台端口。
    pub server_port: u16,
    /// 平台域。
    pub server_domain: String,
    /// 传输方式。
    #[serde(default = "default_transport")]
    pub transport: Transport,
    /// 心跳间隔(秒)。
    #[serde(default = "default_heartbeat")]
    pub heartbeat_interval_secs: u64,
    /// 每设备通道数。
    #[serde(default = "default_channels")]
    pub channels_per_device: usize,
    /// 设备信息模板。
    pub device_info: DeviceInfoTemplate,
    /// 媒体档(A/B/C)。
    #[serde(default)]
    pub media_profile: MediaProfile,
    /// 视频源文件路径(C 档用)。
    pub video_source: Option<String>,
    /// 推流帧率。
    #[serde(default = "default_fps")]
    pub video_fps: u32,
    /// 目标码率(kbps),B 档轻量伪流用;C 档由文件决定,可忽略。
    #[serde(default = "default_bitrate")]
    pub bitrate_kbps: u32,
    /// 真推流设备占比(0.0~1.0,FR-24)。仅前 count×ratio 台带媒体源,其余只维持信令。
    /// 默认 1.0(全部推流)。A 档忽略此项(本就不推流)。
    #[serde(default = "default_active_ratio")]
    pub active_ratio: f32,
    /// 爬坡速率:每秒拉起多少台设备(0 = 一次性全拉起)。避免瞬时注册风暴(FR-21)。
    #[serde(default)]
    pub ramp_per_second: u32,
    /// GB28181 协议版本(2022 默认)。
    #[serde(default)]
    pub gb_version: GbVersion,
}

fn default_transport() -> Transport {
    Transport::Udp
}
fn default_heartbeat() -> u64 {
    60
}
fn default_channels() -> usize {
    1
}
fn default_fps() -> u32 {
    25
}
fn default_bitrate() -> u32 {
    512
}
fn default_active_ratio() -> f32 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfoTemplate {
    pub device_name: String,
    pub manufacturer: String,
    pub model: String,
    pub firmware: String,
}

/// 媒体档(对应压测强度,docs/10-functional/stress-testing.md#2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MediaProfile {
    /// A 档:空媒体,不推流(仅信令)。默认档。
    #[default]
    A,
    /// B 档:轻量伪包(待实现)。
    B,
    /// C 档:真实 H.264 文件循环。
    C,
}

impl Scenario for LinearScenario {
    fn generate(&self, count: usize) -> Result<Vec<DeviceConfig>> {
        let _base = DeviceId::new(&self.base_device_id)?;
        // 20 位 ID 超出 u64 范围(~1.8e19),用 u128 承载递增。
        let base_num: u128 = self
            .base_device_id
            .parse()
            .map_err(|_| common::Error::Gb28181("基础设备 ID 非纯数字".into()))?;

        // FR-24:仅前 active_count 台带媒体源真推流,其余只维持信令。
        let ratio = self.active_ratio.clamp(0.0, 1.0);
        let active_count = (count as f32 * ratio).ceil() as usize;

        let mut configs = Vec::with_capacity(count);
        for i in 0..count {
            let is_active = i < active_count;
            let device_num = base_num + i as u128;
            let device_id_str = format!("{:020}", device_num);
            let device_id = DeviceId::new(&device_id_str)?;

            // 通道 ID:设备 ID 基础上改后 3 位为 132/133/...
            let channels: Vec<ChannelConfig> = (0..self.channels_per_device)
                .map(|ch_idx| {
                    let channel_id_str = format!("{}{:03}", &device_id_str[..17], 132 + ch_idx);
                    ChannelConfig {
                        channel_id: DeviceId::new(channel_id_str).unwrap(),
                        name: format!("Camera-{}", ch_idx + 1),
                        status: "ON".into(),
                    }
                })
                .collect();

            let video_source = match self.media_profile {
                MediaProfile::C if is_active => self.video_source.clone(),
                _ => None,
            };
            // B 档用轻量伪流(按码率合成);A/C 档不用。仅活跃设备推流(FR-24)。
            let light_bitrate_kbps = match self.media_profile {
                MediaProfile::B if is_active => Some(self.bitrate_kbps),
                _ => None,
            };

            configs.push(DeviceConfig {
                device_id,
                username: device_id_str.clone(),
                password: self.password.clone(),
                server_host: self.server_host.clone(),
                server_port: self.server_port,
                server_domain: self.server_domain.clone(),
                transport: self.transport,
                heartbeat_interval_secs: self.heartbeat_interval_secs,
                channels,
                device_info: DeviceInfo {
                    device_name: format!("{}-{}", self.device_info.device_name, i),
                    manufacturer: self.device_info.manufacturer.clone(),
                    model: self.device_info.model.clone(),
                    firmware: self.device_info.firmware.clone(),
                },
                video_source,
                video_fps: self.video_fps,
                light_bitrate_kbps,
                gb_version: self.gb_version,
                // 压测默认 GB18030(国标 §6.10);中文通道名不乱码。
                signaling_encoding: common::SignalingEncoding::default(),
            });
        }
        Ok(configs)
    }

    fn ramp_per_second(&self) -> u32 {
        self.ramp_per_second
    }
}

impl LinearScenario {
    /// 从 TOML 文件路径加载场景。
    pub fn from_toml(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(common::Error::Io)?;
        toml::from_str(&content).map_err(|e| common::Error::Config(format!("TOML 解析失败: {e}")))
    }

    /// 从 TOML 字符串解析场景（桌面端 IPC 调用时使用）。
    pub fn from_toml_str(text: &str) -> Result<Self> {
        toml::from_str(text).map_err(|e| common::Error::Config(format!("TOML 解析失败: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 线性场景生成_id_递增() {
        let sc = LinearScenario {
            base_device_id: "34020000001320000001".into(),
            password: "12345678".into(),
            server_host: "1.2.3.4".into(),
            server_port: 5060,
            server_domain: "34020000002000000001".into(),
            transport: Transport::Udp,
            heartbeat_interval_secs: 60,
            channels_per_device: 1,
            device_info: DeviceInfoTemplate {
                device_name: "Dev".into(),
                manufacturer: "UVP".into(),
                model: "Sim".into(),
                firmware: "0.1".into(),
            },
            media_profile: MediaProfile::A,
            video_source: None,
            video_fps: 25,
            bitrate_kbps: 512,
            active_ratio: 1.0,
            ramp_per_second: 0,
            gb_version: GbVersion::V2022,
        };

        let cfgs = sc.generate(3).unwrap();
        assert_eq!(cfgs.len(), 3);
        assert_eq!(cfgs[0].device_id.as_str(), "34020000001320000001");
        assert_eq!(cfgs[1].device_id.as_str(), "34020000001320000002");
        assert_eq!(cfgs[2].device_id.as_str(), "34020000001320000003");
        assert_eq!(cfgs[0].channels.len(), 1);
        assert_eq!(
            cfgs[0].channels[0].channel_id.as_str(),
            "34020000001320000132"
        );
    }

    #[test]
    fn c档带视频源() {
        let mut sc = LinearScenario {
            base_device_id: "34020000001320000001".into(),
            password: "pwd".into(),
            server_host: "1.2.3.4".into(),
            server_port: 5060,
            server_domain: "34020000002000000001".into(),
            transport: Transport::Udp,
            heartbeat_interval_secs: 60,
            channels_per_device: 1,
            device_info: DeviceInfoTemplate {
                device_name: "D".into(),
                manufacturer: "U".into(),
                model: "S".into(),
                firmware: "0.1".into(),
            },
            media_profile: MediaProfile::C,
            video_source: Some("/tmp/test.h264".into()),
            video_fps: 25,
            bitrate_kbps: 512,
            active_ratio: 1.0,
            ramp_per_second: 0,
            gb_version: GbVersion::V2022,
        };

        let cfgs = sc.generate(1).unwrap();
        assert_eq!(cfgs[0].video_source, Some("/tmp/test.h264".into()));

        sc.media_profile = MediaProfile::A;
        let cfgs_a = sc.generate(1).unwrap();
        assert_eq!(cfgs_a[0].video_source, None);
    }

    #[test]
    fn active_ratio_仅部分设备推流() {
        // C 档 + 30% 采样:10 台里前 ceil(10*0.3)=3 台带视频源,其余 None。
        let sc = LinearScenario {
            base_device_id: "34020000001320000001".into(),
            password: "p".into(),
            server_host: "1.2.3.4".into(),
            server_port: 5060,
            server_domain: "34020000002000000001".into(),
            transport: Transport::Udp,
            heartbeat_interval_secs: 60,
            channels_per_device: 1,
            device_info: DeviceInfoTemplate {
                device_name: "D".into(),
                manufacturer: "U".into(),
                model: "S".into(),
                firmware: "0.1".into(),
            },
            media_profile: MediaProfile::C,
            video_source: Some("/tmp/t.h264".into()),
            video_fps: 25,
            bitrate_kbps: 512,
            active_ratio: 0.3,
            ramp_per_second: 0,
            gb_version: GbVersion::V2022,
        };
        let cfgs = sc.generate(10).unwrap();
        let active = cfgs.iter().filter(|c| c.video_source.is_some()).count();
        assert_eq!(active, 3, "10 台 @ 30% 应有 3 台推流");
        assert!(cfgs[0].video_source.is_some());
        assert!(cfgs[3].video_source.is_none());
    }
}
