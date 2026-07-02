//! 单设备仿真:设备配置 + 状态机骨架。

use common::{DeviceId, Transport};

/// 单个虚拟设备的静态配置。压测时由 `scenario` 按序号批量生成。
#[derive(Debug, Clone)]
pub struct DeviceConfig {
    /// 本设备国标 ID。
    pub device_id: DeviceId,
    /// SIP 认证用户名(通常等于 device_id)。
    pub username: String,
    /// SIP 认证密码。
    pub password: String,
    /// 上级平台 SIP 服务地址 host:port。
    pub server_host: String,
    pub server_port: u16,
    /// 上级平台域(SIP domain / 服务器 ID 中心编码)。
    pub server_domain: String,
    /// 信令传输方式。
    pub transport: Transport,
    /// 心跳间隔(秒)。
    pub heartbeat_interval_secs: u64,
}

/// 设备仿真运行时状态机。
///
/// M0 仅持有配置。M1 实现:注册(Digest)、心跳保活、OPTIONS 应答、
/// 目录/设备信息查询应答;M2 接 INVITE→媒体推流。
pub struct DeviceSimulator {
    config: DeviceConfig,
}

impl DeviceSimulator {
    /// 用配置创建一个待运行的设备仿真实例。
    pub fn new(config: DeviceConfig) -> Self {
        Self { config }
    }

    /// 只读访问配置。
    pub fn config(&self) -> &DeviceConfig {
        &self.config
    }
}
