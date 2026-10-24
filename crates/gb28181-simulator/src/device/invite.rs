use std::net::SocketAddr;
use std::sync::Arc;

use common::{Error, Result};
use sip_core::SipTransport;

use crate::builder::{self};

use super::*;

impl DeviceSimulator {
    /// TCP-PASSIVE 时把"平台该连哪个地址"打进日志,并在地址不可公网直达时告警。
    ///
    /// 这条链路失败时现场只有"平台播放失败",而设备侧连一行"在等谁连进来"都没有;
    /// 内网地址更是必须点名 —— 那不是配置错误,是 NAT 拓扑上根本连不通。
    pub(crate) fn log_passive_listener(&self, local_ip: std::net::IpAddr, port: u16) {
        tracing::info!(
            %local_ip, port,
            "TCP-PASSIVE:设备监听媒体端口,等平台连入(200 OK 的 SDP 已回填该地址)"
        );
        if is_private_or_loopback(local_ip) {
            tracing::warn!(
                %local_ip, port,
                "TCP-PASSIVE:该地址是内网/环回地址,平台在 NAT 后连不进来;需设备公网可达或做端口映射"
            );
        }
    }

    /// INVITE 200 OK 的 Contact 值(RFC 3261 §12.1.1)。
    ///
    /// 平台的 ACK 按应答里的 Contact 定目的地(§13.2.2.4)。缺了它,平台只能退回
    /// 用 INVITE 的 Request-URI —— 那是 `sip:<通道ID>@<平台域>`,于是 ACK 被发往
    /// 平台自己而不是设备;平台随后判定点播失败并回滚(关 ZLM 收流端口),设备侧
    /// 表现为媒体 TCP 连接被对端关闭(Broken pipe),SIP 侧连 ACK 都收不到。
    pub(crate) fn invite_contact(&self) -> String {
        format!(
            "<sip:{}@{}:{}>",
            self.config.device_id,
            self.local_host(),
            self.local_port()
        )
    }

    /// INVITE 200 OK 的 Subject(GB/T 28181 附录 K/L):
    /// `媒体流发送者ID:发送方媒体流序列号,媒体流接收者ID:接收方媒体流序列号`。
    ///
    /// 发送方序列号用 SSRC 的 10 位十进制写法(实时流首位 0),与 SDP 的 `y=` 同源,
    /// 平台据此把这条码流与点播会话关联起来。
    pub(crate) fn invite_subject(
        &self,
        channel: &str,
        ssrc: u32,
        req: &sip_core::Request,
    ) -> String {
        let platform_id = req
            .headers
            .from_uri_user()
            .unwrap_or(self.config.server_domain.as_str());
        sip_core::Subject::realtime(channel, ssrc, platform_id, 0).to_string()
    }

    /// 处理会话内 INFO(MANSRTSP 回放控制):按体首行 PLAY/PAUSE 调整推流。
    ///
    /// 体形如 `PLAY MANSRTSP/1.0\r\nCSeq: n\r\nScale: 2.0\r\n\r\n`(倍速/恢复)
    /// 或 `PAUSE MANSRTSP/1.0\r\nCSeq: n\r\nPauseTime: now\r\n\r\n`(暂停)。
    /// 无活跃会话时也回 200(避免平台重传)。
    pub(crate) async fn handle_info(
        &self,
        transport: &Arc<dyn SipTransport>,
        req: &sip_core::Request,
        from: SocketAddr,
        token: &sip_core::TxToken,
    ) -> Result<bool> {
        if let Ok(body) = std::str::from_utf8(&req.body) {
            let head = body.trim_start();
            let guard = self.session.lock().await;
            if let Some(session) = guard.as_ref() {
                if head.starts_with("PAUSE") {
                    session.control.pause();
                    tracing::info!("回放控制:暂停");
                } else if head.starts_with("PLAY") {
                    // 取 Scale 行(倍速);缺省视为 1.0(恢复正常播放)。
                    let scale = body
                        .lines()
                        .find_map(|l| {
                            l.split_once(':')
                                .filter(|(k, _)| k.trim().eq_ignore_ascii_case("Scale"))
                                .and_then(|(_, v)| v.trim().parse::<f32>().ok())
                        })
                        .unwrap_or(1.0);
                    // 取 Range 行(拖动定位,§9.8):`Range: npt=<start>-<end>`。
                    // start 为回放起点秒偏移;按 INVITE t= 时间段折算千分比位置。
                    if let Some(permille) = Self::parse_range_permille(body) {
                        session.control.seek_permille(permille);
                        tracing::info!(permille, "回放控制:拖动定位");
                    }
                    session.control.set_speed(scale);
                    session.control.resume();
                    tracing::info!(scale, "回放控制:播放/倍速");
                }
            }
        }
        self.respond(transport, from, token, builder::response_ok(req))
            .await?;
        Ok(true)
    }

    /// 解析 MANSRTSP PLAY 的 `Range: npt=<start>-<end>` 头,折算为流内千分比位置(0..=1000)。
    ///
    /// GB28181 回放拖动:npt 为回放起点秒偏移。模拟器循环推短文件,无真实录像时间轴,
    /// 故把 start 对一个名义 3600s 时间窗取模折算千分比(保证拖动可见地改变起播位置)。
    /// 返回 None 表示无 Range 或无法解析。
    pub(crate) fn parse_range_permille(body: &str) -> Option<u32> {
        let range = body.lines().find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.trim().eq_ignore_ascii_case("Range"))
                .map(|(_, v)| v.trim().to_string())
        })?;
        // 形如 "npt=10-" / "npt=10-20" / "npt=now-"。取 start。
        let npt = range.split_once('=').map(|(_, v)| v).unwrap_or(&range);
        let start = npt.split('-').next()?.trim();
        if start.eq_ignore_ascii_case("now") {
            return None; // now 表示从当前继续,不定位。
        }
        let secs: f64 = start.parse().ok()?;
        // 名义 3600s 窗口取模 → 千分比。
        let permille = ((secs.rem_euclid(3600.0) / 3600.0) * 1000.0) as u32;
        Some(permille.min(1000))
    }

    /// 当前推流会话的媒体遥测快照(发送统计 + 平台 RTCP 反馈)。
    ///
    /// 无活跃推流会话时返回 None;会话刚建立、发送通道还没就绪时也返回 None。
    pub async fn media_telemetry(&self) -> Option<MediaTelemetrySnapshot> {
        let guard = self.session.lock().await;
        let session = guard.as_ref()?;
        let telemetry = session.control.telemetry()?;
        let (sent_packets, sent_bytes, sent_rtcp, send_errors) = telemetry.stats.full_snapshot();
        let (capture_resync_bytes, capture_dropped_frames) = telemetry.capture_drops();
        Some(MediaTelemetrySnapshot {
            is_history: session.is_history,
            media_source: session.media_source.clone(),
            local_rtcp_port: telemetry.local_rtcp_port,
            sent_packets,
            sent_bytes,
            sent_rtcp,
            send_errors,
            capture_dropped_frames,
            capture_resync_bytes,
            // age 的原点必须是发送通道建立时刻(与 RtcpFeedback 内部同一刻度)。
            feedback: telemetry
                .feedback
                .snapshot(telemetry.clock_hz, telemetry.elapsed_ms()),
        })
    }

    /// 处理 INVITE:解析平台 SDP,启动推流,回 200 OK(带本端 SDP)。
    pub(crate) async fn handle_invite(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        req: &sip_core::Request,
        from: SocketAddr,
        token: &sip_core::TxToken,
    ) -> Result<bool> {
        use std::net::IpAddr;

        // 解析平台 SDP(提取 RTP 目标地址与端口)。
        let body_str = std::str::from_utf8(&req.body)
            .map_err(|_| Error::Sip("INVITE body 非 UTF-8".into()))?;
        let platform_sdp = sip_core::SessionDescription::parse(body_str)?;
        // 打完整 SDP,便于诊断 TCP 方向(active/passive)/端口/SSRC 等推流问题。
        tracing::info!(sdp = %body_str.replace('\r', "").replace('\n', " | "), "INVITE:平台完整 SDP");
        tracing::info!(proto=%platform_sdp.media.proto, "INVITE SDP 媒体协议");
        let rtp_host: IpAddr = platform_sdp
            .connection
            .addr
            .parse()
            .map_err(|_| Error::Sip("SDP c= 地址非法".into()))?;
        let rtp_port = platform_sdp.media.port;
        let rtp_dst = SocketAddr::new(rtp_host, rtp_port);

        // SSRC 必须用平台 SDP 的 y= 行指定值(GB28181),平台据此识别本条流。
        // 缺失时退化为设备 ID hash。
        let ssrc = platform_sdp.media.ssrc.unwrap_or_else(|| {
            self.config
                .device_id
                .as_str()
                .bytes()
                .fold(0u32, |a, b| a.wrapping_add(b as u32))
        });
        tracing::info!(%rtp_dst, ssrc, "INVITE:开始向平台推流");

        // 附录 K c):用 Subject 的"媒体流发送者 ID:发送方媒体流序列号"作媒体源标识,
        // 判断自己是否已在发同一路码流。**已在发就应释放旧链路、按新请求重建**,
        // 而不是回 486 —— 早先"有会话就 486 Busy Here"的实现会让平台重启后
        // 永远拉不到流:旧会话在设备侧还挂着,新请求一律被拒。
        let subject = req
            .headers
            .get("Subject")
            .and_then(sip_core::Subject::parse);
        let media_source = subject.as_ref().map(|s| s.media_source());
        {
            let mut guard = self.session.lock().await;
            match guard.as_ref() {
                None => {}
                Some(active) => {
                    let same_source = match (&media_source, &active.media_source) {
                        // Subject 都在且媒体源标识相同 → 同一路码流的重新请求。
                        (Some(new), Some(old)) => new == old,
                        // 任一侧没有 Subject:退化为"同一通道的重新点播即视为同源"。
                        // 平台不带 Subject 时(不合标准但现网存在)也要能恢复,
                        // 否则一次丢包就把这个通道永久锁死。
                        _ => true,
                    };
                    if same_source {
                        tracing::info!(
                            ?media_source,
                            "附录 K c):同一媒体源的重新请求,释放旧链路后按新请求重建"
                        );
                        let old = guard.take();
                        drop(guard);
                        if let Some(old) = old {
                            let _ = old.stop_tx.send(());
                        }
                        self.observer.on_event(common::DeviceEvent::StreamStop);
                    } else {
                        // 真的是另一路码流:模拟器同时只维持一路,按 SIP 语义回 486。
                        drop(guard);
                        tracing::info!(?media_source, "已有其它媒体源的会话,回 486 Busy Here");
                        self.respond(
                            transport,
                            from,
                            token,
                            builder::response_status(req, 486, "Busy Here"),
                        )
                        .await?;
                        return Ok(true);
                    }
                }
            }
        }
        // 实时设备启动可能涉及 TCC 授权和最多 5 秒首帧等待,先发 100 防止
        // 平台在准备期间把事务误判为无响应。
        let trying = sip_core::SipMessage::Response(builder::response_status(req, 100, "Trying"));
        transport.send_to(&trying, from).await?;

        // 传输模式:平台 SDP 的 m= proto 含 "TCP" 则走 RTP over TCP。
        // 主被动角色按 `a=setup` 取反(RFC 4145):平台 active → 设备 passive。
        let use_tcp = platform_sdp.is_tcp();
        let local_setup = platform_sdp.local_tcp_setup();
        let local_passive = use_tcp && local_setup == sip_core::Setup::Passive;

        // 语音对讲的上行方向:平台只请求 `m=audio`(§9.12.2 a))。
        // 这条流按附录 C.2.4 走**裸 G.711 而非 PS**,和视频点播是两套封装,
        // 继续沿用视频路径会把音频塞进 PS,平台侧解不出来。
        if platform_sdp.is_audio_only() {
            return self
                .handle_audio_invite(
                    transport,
                    req,
                    from,
                    token,
                    rtp_dst,
                    ssrc,
                    use_tcp,
                    local_setup,
                    local_passive,
                    media_source,
                )
                .await;
        }

        // 按配置选择视频源:实时采集(live:) > 文件(C档) > 轻量伪流(B档) > 不推流(A档)。
        let fps = self.config.video_fps;
        let source: Option<Box<dyn media_rtp::VideoSource>> = if let Some(ref path) =
            self.config.video_source
        {
            let path = path.clone();
            // 视频源构建是**同步阻塞**的(ffmpeg 转封装、摄像头授权、最长 5 秒首帧),
            // 直接在异步上下文里调用会占住 runtime 工作线程。
            let built =
                tokio::task::spawn_blocking(move || -> Result<Box<dyn media_rtp::VideoSource>> {
                    if path.starts_with("live:") {
                        // 实时源由 LiveSource 严格解析完整 URI;legacy live:0 明确映射主屏幕。
                        media_rtp::LiveSource::capture(&path, fps)
                            .map(|s| Box::new(s) as Box<dyn media_rtp::VideoSource>)
                            .map_err(|e| Error::Media(format!("实时采集失败: {e}")))
                    } else {
                        // 含音频轨的文件走音视频复合流(from_path_av);裸流/无音频自动退化为纯视频。
                        media_rtp::FileSource::from_path_av(&path, fps)
                            .map(|s| Box::new(s) as Box<dyn media_rtp::VideoSource>)
                            .map_err(|e| Error::Media(format!("加载视频源失败: {e}")))
                    }
                })
                .await
                .map_err(|e| Error::Media(format!("视频源构建任务异常: {e}")))?;
            match built {
                Ok(s) => Some(s),
                Err(e) => {
                    // 采集/加载失败发生在点播(INVITE)这条异步流程里,不经 start_device
                    // 返回值。必须推 RuntimeError 事件,否则前端一片安静、只见收流超时。
                    self.observer.on_event(common::DeviceEvent::RuntimeError {
                        scope: "capture".into(),
                        message: e.to_string(),
                    });
                    self.respond(
                        transport,
                        from,
                        token,
                        builder::response_status(req, 480, "Temporarily Unavailable"),
                    )
                    .await?;
                    return Ok(true);
                }
            }
        } else {
            self.config
                .light_bitrate_kbps
                .map(|kbps| Box::new(media_rtp::LightSource::new(kbps, fps)) as _)
        };

        // 预先绑定本端媒体端口,使 200 OK 的 SDP 能写**真实端口**
        // (SDP 里 m= 端口 0 表示该媒体被禁用,之前恒填 0)。
        let prepared = if source.is_some() {
            match media_rtp::PreparedMedia::prepare(rtp_dst, use_tcp, local_passive).await {
                Ok(p) => Some(p),
                Err(e) => {
                    self.observer.on_event(common::DeviceEvent::RuntimeError {
                        scope: "stream".into(),
                        message: format!("本端媒体端口绑定失败: {e}"),
                    });
                    self.respond(
                        transport,
                        from,
                        token,
                        builder::response_status(req, 500, "Server Internal Error"),
                    )
                    .await?;
                    return Ok(true);
                }
            }
        } else {
            None
        };
        let local_rtp_port = prepared.as_ref().map(|p| p.local_port()).unwrap_or(0);

        // 先构造并发送 200 OK(带本端 SDP,SSRC 回显平台值)。
        // TCP-PASSIVE 下平台收到 200 后才开始监听,故推流必须在 200 之后再连接。
        let local_ip: std::net::IpAddr = self
            .local_host()
            .parse()
            .unwrap_or_else(|_| transport.local_addr().map(|a| a.ip()).unwrap_or(rtp_host));
        if use_tcp && local_passive {
            self.log_passive_listener(local_ip, local_rtp_port);
        }
        // o= 用户名填被点播的通道 ID(取 INVITE Request-URI 的 user 部分)。
        let channel = req
            .uri
            .strip_prefix("sip:")
            .and_then(|s| s.split('@').next())
            .unwrap_or(self.config.device_id.as_str());
        let local_sdp = sip_core::SessionDescription::new_device_response_with_setup(
            channel,
            local_ip,
            local_rtp_port,
            ssrc,
            use_tcp,
            use_tcp.then_some(local_setup),
        );
        let sdp_body = local_sdp.to_string();

        let mut resp = builder::response_ok_invite(
            req,
            &self.invite_contact(),
            Some(&self.invite_subject(channel, ssrc, req)),
        );
        resp.headers.set("Content-Type", "application/sdp");
        resp.body = sdp_body.as_bytes().to_vec();
        // 会话内主动发 BYE 需要本端 To-tag:必须取自实际发出的那条 200 OK,
        // 自己另生成一个的话平台会认不出这条对话。
        let local_tag = resp
            .headers
            .to_tag()
            .map(|t| t.to_string())
            .unwrap_or_default();
        let dialog = builder::InboundDialog {
            call_id: req.headers.get("Call-ID").unwrap_or_default().trim().into(),
            local_tag,
            remote_from: req.headers.get("From").unwrap_or_default().trim().into(),
            peer: from,
            request_uri: req.uri.clone(),
        };
        self.respond(transport, from, token, resp).await?;

        // 200 OK 已发,启动推流(TCP 模式给平台一点时间建监听 + 收 ACK)。
        // 带回放控制:回放时平台可经会话内 INFO 调整倍速/暂停,直播则保持 1.0x。
        if let (Some(source), Some(prepared)) = (source, prepared) {
            let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
            let (start_tx, start_rx) = tokio::sync::oneshot::channel();
            let control = media_rtp::PlaybackControl::new();
            // 录像下载会话(s=Download + a=downloadspeed:N):按 N 倍速推流。
            if platform_sdp.is_download() {
                if let Some(spd) = platform_sdp.download_speed {
                    control.set_speed(spd as f32);
                    tracing::info!(speed = spd, "INVITE:录像下载模式,按倍速推流");
                }
            }
            let control_task = Arc::clone(&control);
            let simulator = Arc::clone(self);
            let transport_task = Arc::clone(transport);
            let dialog_task = dialog.clone();
            let marker = Arc::new(());
            let task_marker = Arc::clone(&marker);
            let task = tokio::spawn(async move {
                // 确保 PushSession 已写入槽位后再允许任务结束并清理，避免启动失败竞态。
                if start_rx.await.is_err() {
                    return;
                }
                if use_tcp && !local_passive {
                    // TCP-ACTIVE:给平台一点时间在收到 200 后建好监听。
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                }
                let result = media_rtp::push_stream_prepared(
                    prepared,
                    source,
                    rtp_dst,
                    ssrc,
                    fps,
                    control_task,
                    async {
                        let _ = stop_rx.await;
                    },
                )
                .await;
                if let Err(error) = &result {
                    tracing::warn!(%error, "推流结束(错误)");
                    simulator
                        .observer
                        .on_event(common::DeviceEvent::RuntimeError {
                            scope: "stream".into(),
                            message: error.to_string(),
                        });
                    // 附录 K a):监测到媒体流丢失应释放链路,并**通过会话内 BYE 通知**对端。
                    // 只发事件不发 BYE 的话,平台侧那条链路会一直挂着等流。
                    simulator
                        .notify_media_loss(&transport_task, &dialog_task)
                        .await;
                }
                let mut session = simulator.session.lock().await;
                let owns_slot = session
                    .as_ref()
                    .is_some_and(|active| Arc::ptr_eq(&active.marker, &task_marker));
                if owns_slot {
                    *session = None;
                }
                drop(session);
                if owns_slot {
                    simulator.observer.on_event(common::DeviceEvent::StreamStop);
                }
            });
            *self.session.lock().await = Some(PushSession {
                marker,
                task,
                stop_tx,
                control,
                is_history: platform_sdp.is_playback() || platform_sdp.is_download(),
                media_source,
                dialog,
            });
            self.observer.on_event(common::DeviceEvent::StreamStart);
            let _ = start_tx.send(());
        }
        let mode = if platform_sdp.is_download() {
            "下载"
        } else if platform_sdp.is_playback() {
            "回放"
        } else {
            "点播"
        };
        self.observer
            .on_event(common::DeviceEvent::PlatformCommand {
                kind: "invite".into(),
                summary: format!("平台{mode} INVITE → 200 OK,开始推流"),
            });
        Ok(true)
    }

    /// 处理**纯音频** INVITE:语音对讲的设备→平台上行(§9.12.2 a) + 附录 C.2.4)。
    ///
    /// 与视频点播的区别:SDP 回 `m=audio ... 8` + `a=rtpmap:8 PCMA/8000`,
    /// 媒体走裸 G.711A RTP(PT=8),不做 PS 封装。
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn handle_audio_invite(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        req: &sip_core::Request,
        from: SocketAddr,
        token: &sip_core::TxToken,
        rtp_dst: SocketAddr,
        ssrc: u32,
        use_tcp: bool,
        local_setup: sip_core::Setup,
        local_passive: bool,
        media_source: Option<String>,
    ) -> Result<bool> {
        // 音源:复用已配置的媒体源(文件音轨 / 摄像头麦克风)。没有音源时
        // push_audio_stream 会发 A-law 静音并在统计里如实标注。
        let fps = self.config.video_fps;
        let source: Option<Box<dyn media_rtp::VideoSource>> = if let Some(ref path) =
            self.config.video_source
        {
            let path = path.clone();
            let built =
                tokio::task::spawn_blocking(move || -> Result<Box<dyn media_rtp::VideoSource>> {
                    if path.starts_with("live:") {
                        media_rtp::LiveSource::capture(&path, fps)
                            .map(|s| Box::new(s) as Box<dyn media_rtp::VideoSource>)
                            .map_err(|e| Error::Media(format!("实时采集失败: {e}")))
                    } else {
                        media_rtp::FileSource::from_path_av(&path, fps)
                            .map(|s| Box::new(s) as Box<dyn media_rtp::VideoSource>)
                            .map_err(|e| Error::Media(format!("加载音频源失败: {e}")))
                    }
                })
                .await
                .map_err(|e| Error::Media(format!("音频源构建任务异常: {e}")))?;
            match built {
                Ok(s) => Some(s),
                Err(e) => {
                    // 音源起不来不该让对讲整体失败:仍建流发静音,但把原因报上去。
                    tracing::warn!(error = %e, "对讲上行音源构建失败,将发送静音");
                    self.observer.on_event(common::DeviceEvent::RuntimeError {
                        scope: "capture".into(),
                        message: format!("对讲音源不可用,已发送静音: {e}"),
                    });
                    None
                }
            }
        } else {
            None
        };

        let prepared =
            match media_rtp::PreparedMedia::prepare(rtp_dst, use_tcp, local_passive).await {
                Ok(p) => p,
                Err(e) => {
                    self.observer.on_event(common::DeviceEvent::RuntimeError {
                        scope: "stream".into(),
                        message: format!("对讲媒体端口绑定失败: {e}"),
                    });
                    self.respond(
                        transport,
                        from,
                        token,
                        builder::response_status(req, 500, "Server Internal Error"),
                    )
                    .await?;
                    return Ok(true);
                }
            };
        let local_rtp_port = prepared.local_port();
        let local_ip: std::net::IpAddr = self.local_host().parse().unwrap_or_else(|_| {
            transport
                .local_addr()
                .map(|a| a.ip())
                .unwrap_or(rtp_dst.ip())
        });
        if use_tcp && local_passive {
            self.log_passive_listener(local_ip, local_rtp_port);
        }
        let channel = req
            .uri
            .strip_prefix("sip:")
            .and_then(|s| s.split('@').next())
            .unwrap_or(self.config.device_id.as_str());
        let local_sdp = sip_core::SessionDescription::new_device_audio_response(
            channel,
            local_ip,
            local_rtp_port,
            ssrc,
            use_tcp,
            use_tcp.then_some(local_setup),
        );

        let mut resp = builder::response_ok_invite(
            req,
            &self.invite_contact(),
            Some(&self.invite_subject(channel, ssrc, req)),
        );
        resp.headers.set("Content-Type", "application/sdp");
        resp.body = local_sdp.to_string().as_bytes().to_vec();
        let local_tag = resp
            .headers
            .to_tag()
            .map(|t| t.to_string())
            .unwrap_or_default();
        let dialog = builder::InboundDialog {
            call_id: req.headers.get("Call-ID").unwrap_or_default().trim().into(),
            local_tag,
            remote_from: req.headers.get("From").unwrap_or_default().trim().into(),
            peer: from,
            request_uri: req.uri.clone(),
        };
        self.respond(transport, from, token, resp).await?;

        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let simulator = Arc::clone(self);
        let transport_task = Arc::clone(transport);
        let dialog_task = dialog.clone();
        let marker = Arc::new(());
        let task_marker = Arc::clone(&marker);
        let task = tokio::spawn(async move {
            if start_rx.await.is_err() {
                return;
            }
            if use_tcp && !local_passive {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
            let result = media_rtp::push_audio_stream(prepared, source, rtp_dst, ssrc, async {
                let _ = stop_rx.await;
            })
            .await;
            match &result {
                Ok(summary) => {
                    if summary.silence_only() {
                        // 说清楚"通道通了、但没有真实音源",避免误判为对讲正常。
                        simulator
                            .observer
                            .on_event(common::DeviceEvent::PlatformCommand {
                                kind: "invite".into(),
                                summary: format!(
                                    "对讲上行结束:全程静音({} 包),设备未配置音源",
                                    summary.silence_packets
                                ),
                            });
                    }
                    if let Some(error) = &summary.source_error {
                        simulator
                            .observer
                            .on_event(common::DeviceEvent::RuntimeError {
                                scope: "stream".into(),
                                message: error.clone(),
                            });
                        simulator
                            .notify_media_loss(&transport_task, &dialog_task)
                            .await;
                    }
                }
                Err(error) => {
                    simulator
                        .observer
                        .on_event(common::DeviceEvent::RuntimeError {
                            scope: "stream".into(),
                            message: error.to_string(),
                        });
                    simulator
                        .notify_media_loss(&transport_task, &dialog_task)
                        .await;
                }
            }
            let mut session = simulator.session.lock().await;
            let owns_slot = session
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(&active.marker, &task_marker));
            if owns_slot {
                *session = None;
            }
            drop(session);
            if owns_slot {
                simulator.observer.on_event(common::DeviceEvent::StreamStop);
            }
        });
        *self.session.lock().await = Some(PushSession {
            marker,
            task,
            stop_tx,
            control: media_rtp::PlaybackControl::new(),
            is_history: false,
            media_source,
            dialog,
        });
        self.observer.on_event(common::DeviceEvent::StreamStart);
        let _ = start_tx.send(());
        self.observer
            .on_event(common::DeviceEvent::PlatformCommand {
                kind: "invite".into(),
                summary: format!(
                    "平台对讲 INVITE(纯音频)→ 200 OK,本端 {local_rtp_port} 端口发 G.711A"
                ),
            });
        Ok(true)
    }

    /// 附录 K a):媒体流丢失后释放链路并通过**会话内 BYE** 通知平台。
    ///
    /// 设备是被叫,From/To 相对原 INVITE 反转(见 [`builder::bye_inbound`])。
    pub(crate) async fn notify_media_loss(
        &self,
        transport: &Arc<dyn SipTransport>,
        dialog: &builder::InboundDialog,
    ) {
        let bye = builder::bye_inbound(
            &self.config,
            dialog,
            self.next_cseq(),
            &self.local_host(),
            self.local_port(),
            transport.protocol(),
        );
        match transport
            .send_to(&sip_core::SipMessage::Request(bye), dialog.peer)
            .await
        {
            Ok(()) => tracing::info!(
                call_id = %dialog.call_id,
                "附录 K a):媒体流丢失,已发会话内 BYE 通知平台释放链路"
            ),
            Err(error) => tracing::warn!(%error, "媒体流丢失通知 BYE 发送失败"),
        }
    }

    /// 处理 BYE:停止推流或语音广播收流,回 200 OK。回放/下载会话结束后补发 MediaStatus 121。
    pub(crate) async fn handle_bye(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        req: &sip_core::Request,
        from: SocketAddr,
        token: &sip_core::TxToken,
    ) -> Result<bool> {
        // 先看这条 BYE 是不是在拆语音广播会话(§9.12.1 信令 17/18):
        // 广播的 Call-ID 是设备自己生成的,和点播会话不是同一条对话,不能混着停。
        let call_id = req.headers.get("Call-ID").unwrap_or_default().trim();
        if self.stop_broadcast_if_matches(call_id).await {
            self.respond(transport, from, token, builder::response_ok(req))
                .await?;
            self.observer
                .on_event(common::DeviceEvent::PlatformCommand {
                    kind: "broadcast".into(),
                    summary: "平台 BYE 结束语音广播".into(),
                });
            return Ok(true);
        }

        let (was_history, had_session) = self.stop_active_session().await;
        if had_session {
            self.observer.on_event(common::DeviceEvent::StreamStop);
        }

        self.respond(transport, from, token, builder::response_ok(req))
            .await?;

        // 回放/下载会话结束:补发 MediaStatus 121(历史媒体文件发送结束)。
        if was_history {
            let (h, p) = (self.local_host(), self.local_port());
            let sn = self.next_cseq();
            if let Ok(xml) = gb28181_protocol::manscdp::MediaStatusNotify::finished(
                self.config.device_id.as_str(),
                sn,
            )
            .to_xml()
            {
                let _ = self.send_message_xml(transport, &h, p, &xml).await;
            }
            tracing::info!("回放/下载结束,发 MediaStatus 121");
        }
        Ok(true)
    }

    /// 停掉当前活跃会话,返回 (是否历史会话, 之前是否有会话)。
    ///
    /// 单独抽出来是为了**不在持锁期间 await 网络发送**:BYE/CANCEL 都只需要瞬时
    /// 摘掉会话槽位,后续应答与通知在锁外做。
    pub(crate) async fn stop_active_session(&self) -> (bool, bool) {
        let mut sess_guard = self.session.lock().await;
        let was_history = sess_guard.as_ref().map(|s| s.is_history).unwrap_or(false);
        let had_session = if let Some(session) = sess_guard.take() {
            let _ = session.stop_tx.send(()); // 触发停流
            true
        } else {
            false
        };
        (was_history, had_session)
    }

    /// 设备下线时释放所有媒体链路并**通知平台**(附录 K b))。
    ///
    /// 标准要求:监测到媒体流接收方/发送方故障或离线时应主动释放媒体链路。设备自己
    /// 主动下线就是"发送方离线"这一种,只停本地推流是不够的 —— 平台那条链路会一直
    /// 挂着等流,直到它自己的丢流监测超时。
    pub(crate) async fn release_media_on_shutdown(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
    ) {
        // 点播/回放会话:设备是被叫,发反转的会话内 BYE。
        let taken = {
            let mut guard = self.session.lock().await;
            guard.take().map(|session| {
                let _ = session.stop_tx.send(());
                (session.dialog, session.task)
            })
        };
        if let Some((dialog, task)) = taken {
            // 先等推流任务真的停下再发 BYE:否则 BYE 之后还可能飞出几个 RTP 包,
            // 平台侧会看到"已挂断的会话仍在收流"。
            await_task("推流会话", task).await;
            self.observer.on_event(common::DeviceEvent::StreamStop);
            self.notify_media_loss(transport, &dialog).await;
        }
        // 语音广播:设备是主叫,发正向的会话内 BYE。
        self.stop_broadcast(transport, "设备下线").await;
    }

    /// 处理 CANCEL(RFC 3261 §9.2)。
    ///
    /// 正确行为是**两条响应**:对 CANCEL 本身回 200 OK,同时对被取消的 INVITE 回
    /// 487 Request Terminated。之前只回 200,平台会一直等 INVITE 的最终响应,
    /// 表现为"点播取消后平台界面卡在加载中"。
    pub(crate) async fn handle_cancel(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        req: &sip_core::Request,
        from: SocketAddr,
        token: &sip_core::TxToken,
    ) -> Result<bool> {
        let (_, had_session) = self.stop_active_session().await;
        if had_session {
            self.observer.on_event(common::DeviceEvent::StreamStop);
        }

        // 1) 对 CANCEL 回 200 OK。
        self.respond(transport, from, token, builder::response_ok(req))
            .await?;

        // 2) 对原 INVITE 回 487。CANCEL 与被取消的 INVITE 共享 Via branch / CSeq 序号,
        //    只是 CSeq 方法不同,据此构造 487 的事务标识头。
        let mut terminated = builder::response_status(req, 487, "Request Terminated");
        if let Some(cseq_num) = req.headers.cseq_number() {
            terminated.headers.set("CSeq", format!("{cseq_num} INVITE"));
        }
        transport
            .send_to(&sip_core::SipMessage::Response(terminated), from)
            .await?;
        tracing::info!("CANCEL:已回 200 并对原 INVITE 回 487");
        Ok(true)
    }
}
