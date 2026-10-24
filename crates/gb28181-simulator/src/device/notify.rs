use std::net::SocketAddr;
use std::sync::Arc;

use common::Result;
use sip_core::SipTransport;

use crate::builder::{self};

use super::*;

impl DeviceSimulator {
    /// 主动上报一条视频侦测报警(FR-9)。设备 → 平台 MESSAGE + Alarm Notify XML。
    /// `description` 为报警描述;返回平台响应状态码。
    ///
    /// 默认按"运动目标检测"(A.2.5.3 视频报警类型 2)上报。
    pub async fn report_alarm(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
        description: &str,
    ) -> Result<u16> {
        self.report_alarm_typed(
            transport,
            local_host,
            local_port,
            description,
            gb28181_protocol::manscdp::VideoAlarmType::MotionDetection,
        )
        .await
    }

    /// 主动上报一条指定类型的视频报警。
    ///
    /// 存在报警订阅时会先过一遍订阅筛选条件(A.2.4.6):平台只订阅一级警情或特定
    /// 报警方式/类型时,不满足的报警不推送(返回 0 表示"按订阅条件被过滤")。
    pub async fn report_alarm_typed(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
        description: &str,
        alarm_type: gb28181_protocol::manscdp::VideoAlarmType,
    ) -> Result<u16> {
        self.report_alarm_full(
            transport,
            local_host,
            local_port,
            description,
            alarm_type,
            None,
        )
        .await
    }

    /// 主动上报一条指定类型与优先级的视频报警。
    pub async fn report_alarm_full(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
        description: &str,
        alarm_type: gb28181_protocol::manscdp::VideoAlarmType,
        priority: Option<u8>,
    ) -> Result<u16> {
        let sn = self.next_cseq();
        // 报警时间用校时后的 ISO8601(与平台时间对齐,见 common::clock)。
        let time = common::clock::synced_iso8601();
        let position = Some(*self.position.lock());
        let mut alarm = gb28181_protocol::manscdp::AlarmNotify::video_typed(
            self.config.device_id.as_str(),
            sn,
            time,
            description,
            alarm_type,
            position,
        );
        if let Some(p) = priority {
            alarm.priority = p;
        }
        let xml = alarm.to_xml()?;

        // 若存在报警订阅对话:在对话内以 SIP NOTIFY 上报(GB28181 订阅语义);
        // 否则(如手动触发、平台未订阅)退化为独立 MESSAGE(全新 Call-ID)。
        let subscription = self.alarm_dialog.lock().await.clone();
        if let Some(AlarmSubscription {
            dialog,
            peer: sub_dst,
            filter,
        }) = subscription
        {
            // 订阅筛选:不满足条件就不推(之前完全忽略订阅条件,全推)。
            if !filter.accepts(alarm.priority, alarm.method, Some(alarm_type as u32)) {
                tracing::info!(
                    priority = alarm.priority,
                    method = alarm.method,
                    alarm_type = alarm_type as u32,
                    "报警不满足平台订阅筛选条件,不上报"
                );
                return Ok(0);
            }
            let cseq = self.next_cseq();
            let req = builder::notify_in_dialog(
                &self.config,
                &dialog,
                cseq,
                local_host,
                local_port,
                &xml,
                transport.protocol(),
            );
            let (mut rx, _route) = sip_core::RouteGuard::register(transport, &dialog.call_id);
            let result = sip_core::client_transact(
                transport.as_ref(),
                sub_dst,
                &req,
                &mut rx,
                sip_core::Timing::default(),
            )
            .await;
            return Ok(result?.status);
        }

        self.send_message_xml(transport, local_host, local_port, &xml)
            .await
    }

    /// 主动上报实时视音频回传通知(VideoUploadNotify,GB28181-2022 A.2.5.8)。
    /// 设备 → 平台独立 MESSAGE,携带当前位置。
    ///
    /// **fire-and-forget**:该通知为告知性,部分平台(如 WVP)不支持此 CmdType 会
    /// 直接丢弃不回 200,故不等待响应,发出即返回,避免无谓的事务重传阻塞。
    pub async fn report_video_upload(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<()> {
        // A.2.5.8 是 2022 新增的通知。附录 I:协议版本更高的一方应避免向对方发送
        // 不能识别的消息 —— 平台声明自己是 2016(或没声明)时就不该发过去。
        if !self.peer_supports_2022() {
            tracing::debug!(
                peer = ?self.peer_protocol_version(),
                "跳过实时视音频回传通知:对端未声明支持 GB/T 28181-2022"
            );
            return Ok(());
        }
        let sn = self.next_cseq();
        let time = common::clock::synced_iso8601();
        let pos = Some(*self.position.lock());
        let notify = gb28181_protocol::manscdp::VideoUploadNotify::new(
            self.config.device_id.as_str(),
            sn,
            time,
            pos,
        );
        let xml = notify.to_xml()?;
        self.send_message_xml_oneshot(transport, local_host, local_port, &xml)
            .await
    }

    /// 主动上报一条移动位置(GPS,MobilePosition NOTIFY)。设备 → 平台。
    /// 位置订阅场景下由周期任务调用;也可手动触发。返回平台响应状态码。
    pub async fn report_position(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
        longitude: f64,
        latitude: f64,
    ) -> Result<u16> {
        // 记录当前坐标,供位置订阅的周期 NOTIFY 复用。
        *self.position.lock() = (longitude, latitude);
        let sn = self.next_cseq();
        // 位置上报时间用校时后的 ISO8601(与平台时间对齐)。
        let time = common::clock::synced_iso8601();
        let notify = gb28181_protocol::manscdp::MobilePositionNotify::new(
            self.config.device_id.as_str(),
            sn,
            time,
            longitude,
            latitude,
        );
        let xml = notify.to_xml()?;
        self.send_message_xml(transport, local_host, local_port, &xml)
            .await
    }

    /// 在订阅对话内向平台回发目录变更通知(Catalog Notify)。
    ///
    /// 平台 `SUBSCRIBE + Catalog` 后调用:把本设备全部通道以 `Event=ON` 上报,
    /// 使平台目录树同步。走对话内 SIP NOTIFY(沿用订阅 Call-ID + 反转 From/To +
    /// Event/Subscription-State 头),`dst` 为平台来源地址。
    pub(crate) async fn notify_catalog(
        &self,
        transport: &Arc<dyn SipTransport>,
        dst: SocketAddr,
        dialog: &builder::NotifyDialog,
        sn: u32,
    ) -> Result<u16> {
        use gb28181_protocol::manscdp::{CatalogNotify, CatalogNotifyItem, CatalogSnapshot};
        // 全量目录:多通道树优先,否则扁平通道。全量后刷新快照基线供后续增量 diff。
        let publishable = self.catalog_publishable();
        let items: Vec<CatalogNotifyItem> = publishable
            .iter()
            .map(|(id, name, status)| CatalogNotifyItem {
                device_id: id.clone(),
                name: name.clone(),
                event: "ON".into(),
                status: status.clone(),
            })
            .collect();
        *self.catalog_snapshot.lock() = CatalogSnapshot::from_items(publishable.iter().cloned());
        let notify = CatalogNotify::new(self.config.device_id.as_str(), sn, items);
        let xml = notify.to_xml()?;

        let cseq = self.next_cseq();
        let req = builder::notify_in_dialog(
            &self.config,
            dialog,
            cseq,
            &self.local_host(),
            self.local_port(),
            &xml,
            transport.protocol(),
        );
        // 对话内 NOTIFY 的响应按订阅 Call-ID 回来;注册该路由收 200,收完注销。
        let (mut rx, _route) = sip_core::RouteGuard::register(transport, &dialog.call_id);
        let result = sip_core::client_transact(
            transport.as_ref(),
            dst,
            &req,
            &mut rx,
            sip_core::Timing::default(),
        )
        .await;
        Ok(result?.status)
    }

    /// 目录变更后推送增量 NOTIFY(FR-32/34)。
    ///
    /// 计算当前目录相对上次快照的 diff(ADD/DEL/UPDATE/ON/OFF),若有变更且存在活跃目录订阅,
    /// 在订阅对话内发一条只含变更项的 Catalog NOTIFY,并刷新快照基线。无订阅/无变更时不发。
    /// CRUD(upsert_channel/remove_channel)或状态切换后调用。
    pub async fn notify_catalog_changed(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
    ) -> Result<()> {
        use gb28181_protocol::manscdp::CatalogSnapshot;
        let next = CatalogSnapshot::from_items(self.catalog_publishable());
        let changes = {
            let snap = self.catalog_snapshot.lock();
            snap.diff(&next)
        };
        if changes.is_empty() {
            return Ok(());
        }
        // 刷新基线(无论有无订阅,快照都应跟上当前目录)。
        *self.catalog_snapshot.lock() = next;

        // 附录 N:攒够 100 条或等满 1 分钟再通知。没到阈值就先缓冲,
        // 由 `flush_catalog_changes`(CRUD 后与心跳节拍上各调一次)兜底发出。
        let (ready, schedule) = {
            let mut pending = self.pending_catalog_changes.lock();
            pending.merge(changes);
            let ready = pending.should_flush();
            // 只在还没安排过延迟任务时安排一次。
            let schedule = !ready && !pending.flush_scheduled;
            if schedule {
                pending.flush_scheduled = true;
            }
            (ready, schedule)
        };
        if ready {
            return self.flush_catalog_changes(transport).await;
        }
        if schedule {
            // 事件驱动而不是全局轮询:没有变更时零开销,压测万级设备也不会
            // 因为一个"目录变更定时器"多出上万次无谓唤醒。
            let dev = Arc::clone(self);
            let tp = Arc::clone(transport);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(CATALOG_NOTIFY_DEBOUNCE).await;
                    let ready = {
                        let pending = dev.pending_catalog_changes.lock();
                        if pending.items.is_empty() {
                            return; // 已被别的路径发走
                        }
                        pending.should_flush()
                    };
                    if ready {
                        if let Err(error) = dev.flush_catalog_changes(&tp).await {
                            tracing::warn!(%error, "目录变更批量通知发送失败");
                        }
                        return;
                    }
                }
            });
        }
        Ok(())
    }

    /// 把缓冲的目录变更立刻发出(附录 N 的批量通知落地)。
    ///
    /// 无目录订阅时只清空缓冲不发 —— 平台会在下次全量查询时同步。
    pub async fn flush_catalog_changes(&self, transport: &Arc<dyn SipTransport>) -> Result<()> {
        use gb28181_protocol::manscdp::CatalogNotify;
        let changes = self.pending_catalog_changes.lock().take();
        if changes.is_empty() {
            return Ok(());
        }
        let dialog = self.catalog_dialog.lock().await.clone();
        let Some((dialog, dst)) = dialog else {
            return Ok(()); // 无目录订阅,平台会在下次全量查询时同步
        };
        let count = changes.len();
        let sn = self.next_cseq();
        let notify = CatalogNotify::new(self.config.device_id.as_str(), sn, changes);
        let xml = notify.to_xml()?;
        tracing::info!(count, "目录变更批量通知(附录 N)");
        let cseq = self.next_cseq();
        let req = builder::notify_in_dialog(
            &self.config,
            &dialog,
            cseq,
            &self.local_host(),
            self.local_port(),
            &xml,
            transport.protocol(),
        );
        let (mut rx, _route) = sip_core::RouteGuard::register(transport, &dialog.call_id);
        let result = sip_core::client_transact(
            transport.as_ref(),
            dst,
            &req,
            &mut rx,
            sip_core::Timing::default(),
        )
        .await;
        result?;
        Ok(())
    }

    /// 启动(或重建)移动位置订阅的周期上报任务。
    ///
    /// 平台下发 `SUBSCRIBE + MobilePosition`(携 Interval)后调用:按 `interval` 秒
    /// 周期把当前坐标以 MobilePosition NOTIFY 上报给平台,直到订阅被替换或设备下线。
    /// `interval` 缺省/为 0 时取 5 秒兜底。同一 CmdType 再次订阅会替换旧任务。
    pub(crate) async fn start_position_subscription(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        interval: Option<u64>,
        expires: u64,
    ) {
        let secs = interval.filter(|s| *s > 0).unwrap_or(5);
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();

        let sim = Arc::clone(self);
        let tp = Arc::clone(transport);
        let local_host = self.local_host();
        let local_port = self.local_port();
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(secs));
            // 订阅到期定时器:之前只把 expires 写进头,从不真的到期,
            // 于是平台早就忘了这个订阅,设备还在无限上报。
            let expiry = tokio::time::sleep(std::time::Duration::from_secs(expires));
            tokio::pin!(expiry);
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break, // 订阅被替换/取消。
                    _ = &mut expiry => {
                        tracing::info!(expires, "移动位置订阅到期,停止周期上报");
                        sim.observer.on_event(common::DeviceEvent::SubscriptionChanged {
                            kind: "MobilePosition".into(),
                            active: false,
                            notify_count: 0,
                        });
                        break;
                    }
                    _ = ticker.tick() => {
                        let (lon, lat) = *sim.position.lock();
                        if let Err(e) = sim
                            .report_position(&tp, &local_host, local_port, lon, lat)
                            .await
                        {
                            tracing::warn!(error=%e, "位置订阅周期上报失败");
                        }
                    }
                }
            }
        });

        // 插入订阅表(替换旧任务:旧 stop_tx 被 drop → 旧任务在下次 select 时退出)。
        let mut subs = self.subscriptions.lock().await;
        subs.insert("MobilePosition".to_string(), Subscription { task, stop_tx });
        tracing::info!(interval = secs, expires, "移动位置订阅已建立,开始周期上报");
    }

    /// 为无周期任务的订阅(Catalog / Alarm)挂一个到期定时器。
    ///
    /// 到期后清理对话并发一条 `Subscription-State: terminated` 的收尾 NOTIFY,
    /// 让平台知道订阅结束而不是"设备突然不吭声了"。
    pub(crate) async fn arm_subscription_expiry(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        kind: &str,
        dialog: builder::NotifyDialog,
        dst: SocketAddr,
        expires: u64,
    ) {
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let sim = Arc::clone(self);
        let tp = Arc::clone(transport);
        let kind_owned = kind.to_string();
        let task = tokio::spawn(async move {
            let expiry = tokio::time::sleep(std::time::Duration::from_secs(expires));
            tokio::pin!(expiry);
            tokio::select! {
                _ = &mut stop_rx => {}
                _ = &mut expiry => {
                    tracing::info!(kind = %kind_owned, expires, "订阅到期");
                    sim.terminate_subscription(&tp, &kind_owned, &dialog, dst).await;
                }
            }
        });
        let mut subs = self.subscriptions.lock().await;
        subs.insert(kind.to_string(), Subscription { task, stop_tx });
        self.observer
            .on_event(common::DeviceEvent::SubscriptionChanged {
                kind: kind.to_string(),
                active: true,
                notify_count: 0,
            });
    }

    /// 终止一个订阅:清对话、停周期任务,并发 `Subscription-State: terminated` NOTIFY。
    ///
    /// 由平台 `SUBSCRIBE Expires: 0`(退订)或到期定时器触发。
    pub(crate) async fn terminate_subscription(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        kind: &str,
        dialog: &builder::NotifyDialog,
        dst: SocketAddr,
    ) {
        // 先摘掉周期任务,避免收尾 NOTIFY 之后还继续上报。
        // 注意:不能在持有 subscriptions 锁时 await 发送,否则与订阅建立路径互相排队。
        let removed = self.subscriptions.lock().await.remove(kind);
        if let Some(sub) = removed {
            let _ = sub.stop_tx.send(());
        }
        match kind {
            "Catalog" => *self.catalog_dialog.lock().await = None,
            "Alarm" => *self.alarm_dialog.lock().await = None,
            _ => {}
        }
        self.observer
            .on_event(common::DeviceEvent::SubscriptionChanged {
                kind: kind.to_string(),
                active: false,
                notify_count: 0,
            });
        // 对话内订阅才需要收尾 NOTIFY(MobilePosition/PTZPosition 走独立 MESSAGE)。
        if matches!(kind, "Catalog" | "Alarm") {
            if let Err(e) = self.notify_terminated(transport, dialog, dst).await {
                tracing::warn!(kind = %kind, error = %e, "订阅终止 NOTIFY 发送失败");
            }
        }
        tracing::info!(kind = %kind, "订阅已终止");
    }

    /// 发一条 `Subscription-State: terminated` 的空 NOTIFY 作为订阅收尾。
    pub(crate) async fn notify_terminated(
        &self,
        transport: &Arc<dyn SipTransport>,
        dialog: &builder::NotifyDialog,
        dst: SocketAddr,
    ) -> Result<()> {
        let cseq = self.next_cseq();
        let mut req = builder::notify_in_dialog(
            &self.config,
            dialog,
            cseq,
            &self.local_host(),
            self.local_port(),
            "",
            transport.protocol(),
        );
        req.headers
            .set("Subscription-State", "terminated;reason=timeout");
        req.body.clear();
        // fire-and-forget:平台可能已经忘了这个订阅并回 481,没必要为此重传。
        transport
            .send_to(&sip_core::SipMessage::Request(req), dst)
            .await
    }

    /// 启动(或重建)PTZ 精准位置事件订阅的周期上报任务(GB28181-2022 §9.11.2)。
    ///
    /// 平台 `SUBSCRIBE + PTZPosition`(携 Interval)后调用:按 interval 秒周期发
    /// PTZ 精准状态 NOTIFY(独立 MESSAGE,复用 PTZPosition 应答体)。真实设备应在姿态
    /// 变化时触发;模拟器按周期上报当前 precise_pose。
    pub(crate) async fn start_ptz_position_subscription(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        interval: Option<u64>,
        expires: u64,
    ) {
        let secs = interval.filter(|s| *s > 0).unwrap_or(5);
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();

        let sim = Arc::clone(self);
        let tp = Arc::clone(transport);
        let local_host = self.local_host();
        let local_port = self.local_port();
        let notify_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counter = notify_count.clone();
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(secs));
            let expiry = tokio::time::sleep(std::time::Duration::from_secs(expires));
            tokio::pin!(expiry);
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    _ = &mut expiry => {
                        tracing::info!(expires, "PTZ 精准位置订阅到期,停止周期上报");
                        sim.observer.on_event(common::DeviceEvent::SubscriptionChanged {
                            kind: "PTZPosition".into(),
                            active: false,
                            notify_count: 0,
                        });
                        break;
                    }
                    _ = ticker.tick() => {
                        if let Err(e) = sim
                            .report_ptz_position(&tp, &local_host, local_port)
                            .await
                        {
                            tracing::warn!(error=%e, "PTZ 精准位置订阅周期上报失败");
                        } else {
                            let n = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                            sim.observer.on_event(common::DeviceEvent::SubscriptionChanged {
                                kind: "PTZPosition".into(),
                                active: true,
                                notify_count: n,
                            });
                        }
                    }
                }
            }
        });

        let mut subs = self.subscriptions.lock().await;
        subs.insert("PTZPosition".to_string(), Subscription { task, stop_tx });
        self.observer
            .on_event(common::DeviceEvent::SubscriptionChanged {
                kind: "PTZPosition".into(),
                active: true,
                notify_count: 0,
            });
        tracing::info!(interval = secs, "PTZ 精准位置订阅已建立,开始周期上报");
    }

    /// 主动上报一条 PTZ 精准状态 NOTIFY(GB28181-2022 §9.11.2)。
    /// 复用 PTZPosition 应答体(Pan/Tilt/Zoom),独立 MESSAGE fire-and-forget。
    pub async fn report_ptz_position(
        &self,
        transport: &Arc<dyn SipTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<()> {
        let sn = self.next_cseq();
        let (pan, tilt, zoom) = self.control_state.lock().precise_pose;
        let notify = gb28181_protocol::manscdp::PtzPreciseStatusResponse::new(
            self.config.device_id.as_str(),
            sn,
            pan,
            tilt,
            zoom,
        );
        let xml = notify.to_xml()?;
        self.send_message_xml_oneshot(transport, local_host, local_port, &xml)
            .await
    }

    /// 停止全部活跃订阅(设备下线/重注册时调,避免残留周期任务)。
    pub(crate) async fn stop_subscriptions(&self) {
        // 先把全部订阅摘出来再逐个等待:持锁 await 会挡住入站请求处理。
        let drained: Vec<Subscription> = {
            let mut subs = self.subscriptions.lock().await;
            subs.drain().map(|(_, sub)| sub).collect()
        };
        for sub in drained {
            let _ = sub.stop_tx.send(());
            await_task("周期 NOTIFY 订阅", sub.task).await;
        }
        *self.alarm_dialog.lock().await = None;
        *self.catalog_dialog.lock().await = None;
    }
}
