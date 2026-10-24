use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use common::{Error, Result};
use gb28181_protocol::manscdp::Keepalive;
use sip_core::{authorization, Challenge, SipTransport};

use crate::builder::{self, DialogIds};

use super::*;

impl DeviceSimulator {
    /// 执行完整注册流程(REGISTER→401→带鉴权 REGISTER→200)。
    ///
    /// `transport` 为共享 UDP 传输;`local_host/local_port` 为本端对外地址(填 Via/Contact);
    /// 成功返回 [`DeviceState::Registered`],鉴权/拒绝失败返回 [`DeviceState::Failed`] 对应错误。
    pub async fn register(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<DeviceState> {
        // 首次尝试与重试分开计:把重试算进"尝试"会让成功率变成尝试维度,
        // 长跑后被稀释到看不出真实上线比例。
        let retry = self.attempted_once.swap(true, Ordering::Relaxed);
        self.register_with_retry_flag(transport, local_host, local_port, retry)
            .await
    }

    pub(crate) async fn register_with_retry_flag(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
        retry: bool,
    ) -> Result<DeviceState> {
        use common::{DeviceEvent, FailureKind};
        self.observer
            .on_event(DeviceEvent::RegisterAttempt { retry });
        let started = std::time::Instant::now();
        let result = self.register_inner(transport, local_host, local_port).await;
        match &result {
            Ok(_) => {
                self.observer.on_event(DeviceEvent::RegisterSuccess {
                    rtt_ms: started.elapsed().as_millis() as u64,
                });
                self.set_online(true);
            }
            Err(e) => {
                // 按**错误变体**归因,不再匹配错误文案。
                // **本地发送失败单独一类**:本机 ENOBUFS/socket 错误绝不能算成
                // "平台承载不住",否则压测结论直接失真。
                let kind = match e {
                    Error::Io(_) => FailureKind::Local,
                    Error::Timeout(_) => FailureKind::Timeout,
                    Error::Rejected { status, .. } => FailureKind::Rejected { status: *status },
                    _ => FailureKind::Other,
                };
                self.observer.on_event(DeviceEvent::RegisterFailure {
                    kind,
                    device_id: self.config.device_id.as_str().to_string(),
                });
            }
        }
        result
    }

    /// 注册核心逻辑(不含事件上报)。
    ///
    /// 相对最初实现补齐了两处:
    /// - **尊重平台授予的 Expires**:请求写配置值,但续约周期以 200 OK 里平台给的为准。
    ///   之前硬编码 3600 并按 2880s 续约,平台只给 120s 时会被判掉线。
    /// - **stale nonce 重试**:平台回 `stale=true` 时用新 nonce 再算一次,而不是当成密码错。
    pub(crate) async fn register_inner(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<DeviceState> {
        let dst = self.platform_addr()?;
        // 路由用 RAII 守卫:注册期间被停止打断时也不会在路由表里留下孤条目。
        let (mut rx, _route) = sip_core::RouteGuard::register(transport, &self.ids.call_id);
        let timing = sip_core::Timing::default();
        let want_expires = self.config.register_expires_secs.max(1);

        // 首发 REGISTER(无鉴权)。
        let cseq1 = self.next_cseq();
        let req1 = builder::register(
            &self.config,
            &self.ids,
            cseq1,
            local_host,
            local_port,
            None,
            want_expires,
            transport.protocol(),
        );
        let resp =
            sip_core::client_transact(transport.as_ref(), dst, &req1, &mut rx, timing).await?;

        match resp.status {
            200 => {
                self.accept_registration(&resp, want_expires);
                Ok(DeviceState::Registered)
            }
            // 重定向服务器也可能不鉴权直接回 302。
            302 => {
                let target = resp
                    .headers
                    .get("Contact")
                    .and_then(contact_addr)
                    .ok_or_else(|| Error::Sip("302 重定向缺少可用的 Contact 地址".into()))?;
                tracing::info!(%target, "注册被重定向(302),改向新服务器注册");
                self.register_redirected(transport, local_host, local_port, target)
                    .await
            }
            401 => {
                // 解析挑战,带 Authorization 重发。最多重试两轮(第二轮用于 stale nonce)。
                let mut challenge_header = resp
                    .headers
                    .get("WWW-Authenticate")
                    .ok_or_else(|| Error::Sip("401 缺少 WWW-Authenticate".into()))?
                    .to_string();
                for attempt in 0..2 {
                    let challenge = Challenge::parse(&challenge_header)?;
                    let auth = authorization(
                        &challenge,
                        &self.config.username,
                        self.config.password.expose_secret(),
                        "REGISTER",
                        &req1.uri,
                    );
                    let cseq = self.next_cseq();
                    let req = builder::register(
                        &self.config,
                        &self.ids,
                        cseq,
                        local_host,
                        local_port,
                        Some(&auth),
                        want_expires,
                        transport.protocol(),
                    );
                    let resp2 =
                        sip_core::client_transact(transport.as_ref(), dst, &req, &mut rx, timing)
                            .await?;
                    match resp2.status {
                        200 => {
                            // 网络校时:用平台 200 OK 的 Date 头校准本地时钟偏移(FR)。
                            if let Some(date) = resp2.headers.get("Date") {
                                if let Some(off) = common::clock::sync_from_date_header(date) {
                                    tracing::info!(platform_date = %date, offset_secs = off, "网络校时完成");
                                }
                            }
                            self.accept_registration(&resp2, want_expires);
                            return Ok(DeviceState::Registered);
                        }
                        401 if attempt == 0 => {
                            // 平台可能因 nonce 过期(stale)再挑战一次:换新 nonce 重算。
                            let next = resp2
                                .headers
                                .get("WWW-Authenticate")
                                .ok_or_else(|| {
                                    Error::Sip("注册被拒: 401 重挑战但缺少 WWW-Authenticate".into())
                                })?
                                .to_string();
                            let stale = Challenge::parse(&next).map(|c| c.stale).unwrap_or(false);
                            if !stale && next == challenge_header {
                                return Err(Error::rejected(
                                    401,
                                    "Unauthorized(nonce 未更新,通常是密码或用户名错)",
                                ));
                            }
                            tracing::info!(stale, "平台再次挑战,用新 nonce 重算鉴权");
                            challenge_header = next;
                        }
                        // §9.1.2.3 注册重定向:重定向服务器在鉴权后回 302,
                        // Contact 里给出真正的 SIP 服务器地址,设备应改向它注册。
                        // 之前 302 被当成"注册被拒",接重定向服务器的平台永远注册不上。
                        302 => {
                            let target = resp2
                                .headers
                                .get("Contact")
                                .and_then(contact_addr)
                                .ok_or_else(|| {
                                    Error::Sip("302 重定向缺少可用的 Contact 地址".into())
                                })?;
                            tracing::info!(%target, "注册被重定向(302),改向新服务器注册");
                            return self
                                .register_redirected(transport, local_host, local_port, target)
                                .await;
                        }
                        other => {
                            return Err(Error::rejected(
                                other,
                                format!("{}(鉴权后注册)", resp2.reason),
                            ))
                        }
                    }
                }
                Err(Error::rejected(
                    401,
                    "Unauthorized(反复 401,鉴权始终不被接受)",
                ))
            }
            other => Err(Error::rejected(other, format!("{}(注册)", resp.reason))),
        }
    }

    /// 按 302 重定向给出的新地址重新注册一轮(§9.1.2.3)。
    ///
    /// 只重定向一次:再收到 302 视为重定向环路并报错,避免无限跳转。
    pub(crate) async fn register_redirected(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
        target: SocketAddr,
    ) -> Result<DeviceState> {
        let want_expires = self.config.register_expires_secs.max(1);
        // 重定向后的注册用独立 Call-ID,避免与原事务路由冲突。
        let ids = DialogIds::new();
        let (mut rx, _route) = sip_core::RouteGuard::register(transport, &ids.call_id);
        let timing = sip_core::Timing::default();
        let result = async {
            let cseq = self.next_cseq();
            let req = builder::register(
                &self.config,
                &ids,
                cseq,
                local_host,
                local_port,
                None,
                want_expires,
                transport.protocol(),
            );
            let resp = sip_core::client_transact(transport.as_ref(), target, &req, &mut rx, timing)
                .await?;
            match resp.status {
                200 => {
                    self.accept_registration(&resp, want_expires);
                    Ok(DeviceState::Registered)
                }
                401 => {
                    let wa = resp
                        .headers
                        .get("WWW-Authenticate")
                        .ok_or_else(|| Error::Sip("重定向后 401 缺少 WWW-Authenticate".into()))?;
                    let challenge = Challenge::parse(wa)?;
                    let auth = authorization(
                        &challenge,
                        &self.config.username,
                        self.config.password.expose_secret(),
                        "REGISTER",
                        &req.uri,
                    );
                    let cseq2 = self.next_cseq();
                    let req2 = builder::register(
                        &self.config,
                        &ids,
                        cseq2,
                        local_host,
                        local_port,
                        Some(&auth),
                        want_expires,
                        transport.protocol(),
                    );
                    let resp2 = sip_core::client_transact(
                        transport.as_ref(),
                        target,
                        &req2,
                        &mut rx,
                        timing,
                    )
                    .await?;
                    if resp2.status == 200 {
                        if let Some(date) = resp2.headers.get("Date") {
                            if let Some(off) = common::clock::sync_from_date_header(date) {
                                tracing::info!(offset_secs = off, "网络校时完成(重定向后)");
                            }
                        }
                        self.accept_registration(&resp2, want_expires);
                        Ok(DeviceState::Registered)
                    } else {
                        Err(Error::rejected(
                            resp2.status,
                            format!("{}(重定向后鉴权注册)", resp2.reason),
                        ))
                    }
                }
                302 => Err(Error::Sip("注册重定向环路(连续两次 302),放弃".into())),
                other => Err(Error::rejected(
                    other,
                    format!("{}(重定向后注册)", resp.reason),
                )),
            }
        }
        .await;
        result
    }

    /// 记录平台在 200 OK 里授予的注册有效期。
    ///
    /// 优先取 Expires 头,其次取 Contact 的 `expires` 参数,都没有才退回请求值
    /// (RFC 3261 §10.2.4:服务端未回 Expires 时视为接受请求值)。
    pub(crate) fn accept_registration(&self, resp: &sip_core::Response, requested: u32) {
        let granted = resp
            .headers
            .expires()
            .or_else(|| resp.headers.contact_expires())
            .map(|v| v.min(u32::MAX as u64) as u32)
            .filter(|v| *v > 0)
            .unwrap_or(requested);
        let previous = self.granted_expires.swap(granted, Ordering::Relaxed);
        if previous != granted {
            tracing::info!(requested, granted, "平台授予注册有效期(续约周期以此为准)");
        }
        // 附录 I:从注册响应的 X-GB-Ver 学到平台支持的协议版本。
        let declared = resp.headers.get("X-GB-Ver");
        let parsed = declared.and_then(common::GbVersion::from_x_gb_ver);
        *self.peer_version.lock() = parsed;
        match (declared, parsed) {
            (Some(raw), Some(version)) => {
                tracing::info!(x_gb_ver = %raw, peer = %version, "平台声明的协议版本")
            }
            (Some(raw), None) => {
                tracing::warn!(x_gb_ver = %raw, "平台声明了无法识别的协议版本,按保守方式交互")
            }
            // 大量在网平台不发这个头,不值得每次注册都告警。
            (None, _) => tracing::debug!("平台未声明 X-GB-Ver"),
        }
    }

    /// 对端是否确定支持 GB/T 28181-2022 的新增消息(附录 I)。
    ///
    /// 平台没声明 `X-GB-Ver` 时返回 false:附录 I 要求高版本一方避免发对方不认识的消息,
    /// "不知道"就该按不支持处理。本机自己配成 2016 时同样返回 false。
    pub fn peer_supports_2022(&self) -> bool {
        self.config.gb_version.is_2022()
            && matches!(*self.peer_version.lock(), Some(common::GbVersion::V2022))
    }

    /// 平台声明的协议版本(附录 I);None 表示未声明或无法识别。
    pub fn peer_protocol_version(&self) -> Option<common::GbVersion> {
        *self.peer_version.lock()
    }

    /// 平台当前授予的注册有效期(秒)。未注册成功前为配置的请求值。
    pub fn granted_expires(&self) -> u32 {
        let v = self.granted_expires.load(Ordering::Relaxed);
        if v == 0 {
            self.config.register_expires_secs.max(1)
        } else {
            v
        }
    }

    /// 解析平台信令地址。
    pub(crate) fn platform_addr(&self) -> Result<SocketAddr> {
        format!("{}:{}", self.config.server_host, self.config.server_port)
            .parse()
            .map_err(|_| {
                Error::Config(format!(
                    "平台地址非法: {}:{}",
                    self.config.server_host, self.config.server_port
                ))
            })
    }

    /// 向平台注销(GB28181-2022 §9.1.2.2):发 REGISTER Expires=0。
    ///
    /// 设备下线时主动注销,平台立即置离线,无需等心跳超时。与注册同样处理 401 鉴权挑战。
    /// 尽力而为:失败仅记录警告(下线流程不因注销失败而阻塞)。
    pub async fn unregister(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<()> {
        let dst: SocketAddr = format!("{}:{}", self.config.server_host, self.config.server_port)
            .parse()
            .map_err(|_| Error::Config("平台地址非法".into()))?;
        // 用独立 Call-ID,避免与运行中的注册路由冲突。
        let ids = DialogIds::new();
        let (mut rx, _route) = sip_core::RouteGuard::register(transport, &ids.call_id);
        let timing = sip_core::Timing::default();

        // 首发 REGISTER Expires=0(无鉴权)。
        let cseq1 = self.next_cseq();
        let req1 = builder::register(
            &self.config,
            &ids,
            cseq1,
            local_host,
            local_port,
            None,
            0,
            transport.protocol(),
        );
        let result = async {
            let resp =
                sip_core::client_transact(transport.as_ref(), dst, &req1, &mut rx, timing).await?;
            match resp.status {
                200 => Ok(()),
                401 => {
                    let wa = resp
                        .headers
                        .get("WWW-Authenticate")
                        .ok_or_else(|| Error::Sip("注销 401 缺少 WWW-Authenticate".into()))?;
                    let challenge = Challenge::parse(wa)?;
                    let auth = authorization(
                        &challenge,
                        &self.config.username,
                        self.config.password.expose_secret(),
                        "REGISTER",
                        &req1.uri,
                    );
                    let cseq2 = self.next_cseq();
                    let req2 = builder::register(
                        &self.config,
                        &ids,
                        cseq2,
                        local_host,
                        local_port,
                        Some(&auth),
                        0,
                        transport.protocol(),
                    );
                    let resp2 =
                        sip_core::client_transact(transport.as_ref(), dst, &req2, &mut rx, timing)
                            .await?;
                    if resp2.status == 200 {
                        Ok(())
                    } else {
                        Err(Error::Sip(format!("注销被拒: {}", resp2.status)))
                    }
                }
                other => Err(Error::Sip(format!("注销被拒: {other}"))),
            }
        }
        .await;
        result
    }

    /// 发送一次心跳(MESSAGE + Keepalive XML)。返回平台响应状态码。
    pub async fn send_keepalive(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<u16> {
        let sn = self.next_cseq();
        let ka = Keepalive::ok(self.config.device_id.as_str(), sn);
        let xml = ka.to_xml()?;
        let started = std::time::Instant::now();
        let status = self
            .send_message_xml(transport, local_host, local_port, &xml)
            .await?;
        if status == 200 {
            self.observer.on_event(common::DeviceEvent::HeartbeatOk {
                rtt_ms: started.elapsed().as_millis() as u64,
            });
        } else {
            self.observer.on_event(common::DeviceEvent::HeartbeatFail);
        }
        Ok(status)
    }
}
