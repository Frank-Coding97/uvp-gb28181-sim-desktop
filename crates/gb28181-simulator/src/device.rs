//! 单设备仿真:设备配置 + 注册/心跳状态机。
//!
//! 实现 docs/10-functional/device-simulation.md §3.1(注册+Digest)、§3.2(心跳)。
//! 注册闭环:REGISTER(无鉴权)→ 平台 401 挑战 → REGISTER(带 Authorization)→ 200 OK。
//! 注册成功后按 `heartbeat_interval_secs` 周期发送 Keepalive。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use common::{DeviceId, Error, Result, Transport};
use gb28181_protocol::manscdp::Keepalive;
use sip_core::{authorization, Challenge, UdpTransport};

use crate::builder::{self, DialogIds};

/// 单个虚拟设备的静态配置。压测时由 `scenario` 按序号批量生成。
#[derive(Debug, Clone)]
pub struct DeviceConfig {
    /// 本设备国标 ID。
    pub device_id: DeviceId,
    /// SIP 认证用户名(通常等于 device_id)。
    pub username: String,
    /// SIP 认证密码。
    pub password: String,
    /// 上级平台 SIP 服务地址 host。
    pub server_host: String,
    /// 上级平台 SIP 服务端口。
    pub server_port: u16,
    /// 上级平台域(SIP domain / 服务器 ID 中心编码)。
    pub server_domain: String,
    /// 信令传输方式。
    pub transport: Transport,
    /// 心跳间隔(秒)。
    pub heartbeat_interval_secs: u64,
}

/// 设备状态(与 docs/10-functional/device-simulation.md#2、UI DTO 对齐)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    /// 离线。
    Disconnected,
    /// 注册中。
    Registering,
    /// 已注册。
    Registered,
    /// 推流中(M2)。
    InCall,
    /// 注册失败。
    Failed,
}

/// 设备仿真运行时状态机。持有配置 + 会话标识 + CSeq 计数。
pub struct DeviceSimulator {
    config: DeviceConfig,
    ids: DialogIds,
    cseq: AtomicU32,
}

impl DeviceSimulator {
    /// 用配置创建一个待运行的设备仿真实例。
    pub fn new(config: DeviceConfig) -> Self {
        Self {
            config,
            ids: DialogIds::new(),
            cseq: AtomicU32::new(1),
        }
    }

    /// 只读访问配置。
    pub fn config(&self) -> &DeviceConfig {
        &self.config
    }

    /// 本设备注册用的 Call-ID(用于在共享传输上注册接收路由)。
    pub fn call_id(&self) -> &str {
        &self.ids.call_id
    }

    fn next_cseq(&self) -> u32 {
        self.cseq.fetch_add(1, Ordering::Relaxed)
    }

    /// 执行完整注册流程(REGISTER→401→带鉴权 REGISTER→200)。
    ///
    /// `transport` 为共享 UDP 传输;`local_host/local_port` 为本端对外地址(填 Via/Contact);
    /// 成功返回 [`DeviceState::Registered`],鉴权/拒绝失败返回 [`DeviceState::Failed`] 对应错误。
    pub async fn register(
        &self,
        transport: &Arc<UdpTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<DeviceState> {
        let dst: SocketAddr = format!("{}:{}", self.config.server_host, self.config.server_port)
            .parse()
            .map_err(|_| Error::Config(format!("平台地址非法: {}:{}", self.config.server_host, self.config.server_port)))?;
        let mut rx = transport.register(self.ids.call_id.clone());
        let timing = sip_core::Timing::default();

        // 首发 REGISTER(无鉴权)。
        let cseq1 = self.next_cseq();
        let req1 = builder::register(
            &self.config, &self.ids, cseq1, local_host, local_port, None, 3600,
        );
        let resp = sip_core::client_transact(transport, dst, &req1, &mut rx, timing).await?;

        match resp.status {
            200 => Ok(DeviceState::Registered),
            401 => {
                // 解析挑战,带 Authorization 重发。
                let wa = resp
                    .headers
                    .get("WWW-Authenticate")
                    .ok_or_else(|| Error::Sip("401 缺少 WWW-Authenticate".into()))?;
                let challenge = Challenge::parse(wa)?;
                let auth = authorization(
                    &challenge,
                    &self.config.username,
                    &self.config.password,
                    "REGISTER",
                    &req1.uri,
                );
                let cseq2 = self.next_cseq();
                let req2 = builder::register(
                    &self.config, &self.ids, cseq2, local_host, local_port, Some(&auth), 3600,
                );
                let resp2 = sip_core::client_transact(transport, dst, &req2, &mut rx, timing).await?;
                if resp2.status == 200 {
                    Ok(DeviceState::Registered)
                } else {
                    Err(Error::Sip(format!("鉴权后注册被拒: {} {}", resp2.status, resp2.reason)))
                }
            }
            other => Err(Error::Sip(format!("注册被拒: {} {}", other, resp.reason))),
        }
    }

    /// 发送一次心跳(MESSAGE + Keepalive XML)。返回平台响应状态码。
    pub async fn send_keepalive(
        &self,
        transport: &Arc<UdpTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<u16> {
        let dst: SocketAddr = format!("{}:{}", self.config.server_host, self.config.server_port)
            .parse()
            .map_err(|_| Error::Config("平台地址非法".into()))?;
        let sn = self.next_cseq();
        let ka = Keepalive::ok(self.config.device_id.as_str(), sn);
        let xml = ka.to_xml()?;
        let cseq = self.next_cseq();
        let req = builder::message_xml(&self.config, &self.ids, cseq, local_host, local_port, &xml);
        let mut rx = transport.register(self.ids.call_id.clone());
        let resp = sip_core::client_transact(transport, dst, &req, &mut rx, sip_core::Timing::default()).await?;
        Ok(resp.status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sip_core::{Headers, Response, SipMessage};

    fn test_cfg(host: &str, port: u16) -> DeviceConfig {
        DeviceConfig {
            device_id: DeviceId::new("34020000001320000001").unwrap(),
            username: "34020000001320000001".into(),
            password: "12345678".into(),
            server_host: host.into(),
            server_port: port,
            server_domain: "34020000002000000001".into(),
            transport: Transport::Udp,
            heartbeat_interval_secs: 60,
        }
    }

    /// mock 平台:收到首个 REGISTER 回 401(带挑战),第二个回 200。
    async fn mock_platform_register(platform: Arc<UdpTransport>, device_addr: SocketAddr, call_id: String) {
        let mut prx = platform.register(call_id.clone());
        let mut seen = 0;
        while let Some(inc) = prx.recv().await {
            if let SipMessage::Request(req) = inc.message {
                let cseq = req.headers.cseq().unwrap_or("1 REGISTER").to_string();
                let mut h = Headers::new();
                h.set("Call-ID", call_id.clone());
                h.set("CSeq", cseq);
                let resp = if seen == 0 {
                    h.set("WWW-Authenticate", r#"Digest realm="3402000000", nonce="abc123""#);
                    Response { status: 401, reason: "Unauthorized".into(), headers: h, body: Vec::new() }
                } else {
                    Response { status: 200, reason: "OK".into(), headers: h, body: Vec::new() }
                };
                seen += 1;
                let _ = platform.send_to(&SipMessage::Response(resp), device_addr).await;
            }
        }
    }

    #[tokio::test]
    async fn 注册闭环_401后带鉴权成功() {
        let device_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();
        let device_addr = device_tp.local_addr().unwrap();

        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", platform_addr.port()));
        tokio::spawn(mock_platform_register(platform_tp.clone(), device_addr, sim.call_id().to_string()));

        let state = sim
            .register(&device_tp, "127.0.0.1", device_addr.port())
            .await
            .unwrap();
        assert_eq!(state, DeviceState::Registered);
    }
}
