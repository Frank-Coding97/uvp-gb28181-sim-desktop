use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use common::{Error, Result};
use sip_core::SipTransport;

use crate::builder::{self};

use super::*;

impl DeviceSimulator {
    /// 处理语音广播(§9.12.1 信令 1~4):校验 TargetID,回 Broadcast Response,
    /// 接受则异步走完信令 5~16(设备主动 INVITE → 收流)。
    pub(crate) async fn handle_broadcast(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        bc: &gb28181_protocol::manscdp::BroadcastNotify,
        from: SocketAddr,
    ) -> Result<()> {
        // TargetID 须为本设备或其某通道。
        let owned = bc.target_id == self.config.device_id.as_str()
            || self
                .config
                .channels
                .iter()
                .any(|c| c.channel_id.as_str() == bc.target_id);
        let resp = if owned {
            gb28181_protocol::manscdp::BroadcastResponse::ok(&bc.target_id, bc.sn)
        } else {
            gb28181_protocol::manscdp::BroadcastResponse::error(
                self.config.device_id.as_str(),
                bc.sn,
                "target mismatch",
            )
        };
        // 先回 200 OK(入站已回),再以独立 MESSAGE 发 Broadcast Response(信令 3)。
        let xml = resp.to_xml()?;
        self.send_reply_message(transport, from, &xml).await?;

        if !owned {
            tracing::warn!(target = %bc.target_id, "语音广播 TargetID 不属本设备,拒绝");
            return Ok(());
        }

        // 信令 5 起是一次完整的 UAC 事务 + 收流,放后台跑:不能占住入站分派循环,
        // 否则这段时间里平台的其它命令(含本次广播的 200 OK)都进不来。
        let dev = Arc::clone(self);
        let tp = Arc::clone(transport);
        let source_id = bc.source_id.clone();
        let target_id = bc.target_id.clone();
        tokio::spawn(async move {
            if let Err(error) = dev.start_broadcast(&tp, &source_id, &target_id, from).await {
                tracing::warn!(%error, "语音广播建流失败");
                dev.observer.on_event(common::DeviceEvent::RuntimeError {
                    scope: "broadcast".into(),
                    message: error.to_string(),
                });
            }
        });
        Ok(())
    }

    /// 语音广播的设备侧主叫流程(§9.12.1 图27 信令 5、14、15)。
    ///
    /// 顺序很关键:**先绑收流端口,再发 INVITE**。SDP 的 `m=audio <port>` 必须是真实端口,
    /// 之前的实现填 0 —— 等于告诉平台"往 0 端口发",链路永远建不起来。
    pub(crate) async fn start_broadcast(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        source_id: &str,
        target_id: &str,
        peer: SocketAddr,
    ) -> Result<()> {
        // 同一时刻只维持一路广播:平台重复下发广播通知时,先拆掉旧的再建新的。
        self.stop_broadcast(transport, "新的广播请求").await;

        let local_ip: std::net::IpAddr = self
            .local_host()
            .parse()
            .unwrap_or_else(|_| transport.local_addr().map(|a| a.ip()).unwrap_or(peer.ip()));
        let receiver = media_rtp::AudioReceiver::bind(local_ip).await?;
        let local_port = receiver.local_port()?;
        let stats = receiver.stats();

        let ssrc = rand::random::<u32>() & 0x00FF_FFFF;
        let offer = sip_core::build_broadcast_offer(target_id, local_ip, local_port, ssrc, false);

        // 附录 L:媒体流发送者是平台侧的语音流发送者(SourceID),接收者是本设备(TargetID)。
        let seq = self.media_seq.fetch_add(1, Ordering::Relaxed);
        let subject = sip_core::Subject::realtime(source_id, seq, target_id, seq);

        let remote_uri = format!("sip:{}@{}", source_id, self.config.server_domain);
        let dialog = builder::OutboundDialog::new(remote_uri, self.next_cseq());
        let (mut rx, _route) = sip_core::RouteGuard::register(transport, &dialog.call_id);
        let req = builder::invite(
            &self.config,
            &dialog,
            &self.local_host(),
            self.local_port(),
            &subject,
            &offer,
            transport.protocol(),
        );
        tracing::info!(%source_id, %target_id, local_port, subject = %subject,
            "语音广播:发出 INVITE(信令 5)");

        let resp = sip_core::client_transact(
            transport.as_ref(),
            peer,
            &req,
            &mut rx,
            sip_core::Timing::default(),
        )
        .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                return Err(e);
            }
        };
        if resp.status != 200 {
            return Err(Error::Sip(format!(
                "语音广播 INVITE 被拒:{} {}",
                resp.status, resp.reason
            )));
        }

        // 学到平台的 To-tag 后才能发合法的 ACK / 会话内 BYE。
        let mut dialog = dialog;
        dialog.remote_tag = resp.headers.to_tag().map(|t| t.to_string());
        let ack = builder::ack(
            &self.config,
            &dialog,
            &self.local_host(),
            self.local_port(),
            transport.protocol(),
        );
        transport
            .send_to(&sip_core::SipMessage::Request(ack), peer)
            .await?;

        // 平台 200 OK 的 SDP 只用于诊断:UDP 下我们是被动收流方,不需要连对端。
        if let Ok(text) = std::str::from_utf8(&resp.body) {
            if let Ok(sdp) = sip_core::SessionDescription::parse(text) {
                tracing::info!(
                    sender = %sdp.connection.addr, port = sdp.media.port,
                    ssrc = ?sdp.media.ssrc, "语音广播:平台应答 SDP"
                );
            }
        }

        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let dev = Arc::clone(self);
        let watch_stats = Arc::clone(&stats);
        let task = tokio::spawn(async move {
            let result = receiver
                .run(async {
                    let _ = stop_rx.await;
                })
                .await;
            if let Err(error) = result {
                tracing::warn!(%error, "语音广播收流异常结束");
            }
            let snap = watch_stats.snapshot();
            tracing::info!(
                packets = snap.packets,
                bytes = snap.payload_bytes,
                lost = snap.lost,
                "语音广播收流结束"
            );
            dev.observer.on_event(common::DeviceEvent::PlatformCommand {
                kind: "broadcast".into(),
                summary: format!(
                    "语音广播结束:收 {} 包 / {} 字节,丢 {}",
                    snap.packets, snap.payload_bytes, snap.lost
                ),
            });
        });

        let call_id = dialog.call_id.clone();
        *self.broadcast.lock().await = Some(BroadcastSession {
            dialog,
            peer,
            source_id: source_id.to_string(),
            local_port,
            stats,
            stop_tx,
            task,
        });
        self.observer
            .on_event(common::DeviceEvent::PlatformCommand {
                kind: "broadcast".into(),
                summary: format!("语音广播建流成功:本端 {local_port} 端口收 G.711 音频"),
            });
        tracing::info!(%call_id, local_port, "语音广播:ACK 已发,开始收音频");
        Ok(())
    }

    /// 结束语音广播会话。`reason` 非空时发会话内 BYE 通知平台。
    ///
    /// 返回是否确实结束了一路会话。
    pub(crate) async fn stop_broadcast(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        reason: &str,
    ) -> bool {
        let Some(session) = self.broadcast.lock().await.take() else {
            return false;
        };
        let _ = session.stop_tx.send(());
        // 等收流任务真的退出再发 BYE:否则 BYE 之后收流端口还在,
        // 平台如果继续推几个包会打到一个"已经不该存在"的端口上。
        await_task("语音广播收流", session.task).await;
        transport.unregister(&session.dialog.call_id);
        // 设备主动挂断要发 BYE(附录 K a):否则平台侧那条媒体链路一直挂着。
        let bye = builder::bye_outbound(
            &self.config,
            &session.dialog,
            self.next_cseq(),
            &self.local_host(),
            self.local_port(),
            transport.protocol(),
        );
        if let Err(error) = transport
            .send_to(&sip_core::SipMessage::Request(bye), session.peer)
            .await
        {
            tracing::warn!(%error, "语音广播 BYE 发送失败");
        }
        tracing::info!(reason, source = %session.source_id, "语音广播已结束并发出 BYE");
        true
    }

    /// 当前语音广播会话快照(供前端展示收流统计与电平)。
    pub async fn broadcast_snapshot(&self) -> Option<BroadcastSnapshot> {
        let guard = self.broadcast.lock().await;
        guard.as_ref().map(|s| BroadcastSnapshot {
            source_id: s.source_id.clone(),
            local_port: s.local_port,
            call_id: s.dialog.call_id.clone(),
            recv: s.stats.snapshot(),
        })
    }

    /// 设备侧主动挂断语音广播(界面上的"挂断"按钮)。
    ///
    /// 会按附录 K a) 发会话内 BYE 通知平台释放链路。返回是否确实挂断了一路会话。
    pub async fn hangup_broadcast(self: &Arc<Self>, transport: &Arc<dyn SipTransport>) -> bool {
        let stopped = self.stop_broadcast(transport, "设备侧主动挂断").await;
        if stopped {
            self.observer
                .on_event(common::DeviceEvent::PlatformCommand {
                    kind: "broadcast".into(),
                    summary: "设备主动挂断语音广播并已发 BYE".into(),
                });
        }
        stopped
    }

    /// 平台发来的 BYE 是否属于当前广播会话;是则结束收流并返回 true。
    pub(crate) async fn stop_broadcast_if_matches(&self, call_id: &str) -> bool {
        let mut guard = self.broadcast.lock().await;
        let matches = guard.as_ref().is_some_and(|s| s.dialog.call_id == call_id);
        if matches {
            if let Some(session) = guard.take() {
                let _ = session.stop_tx.send(());
                await_task("语音广播收流", session.task).await;
                tracing::info!(%call_id, "平台 BYE 结束语音广播会话");
            }
        }
        matches
    }
}
