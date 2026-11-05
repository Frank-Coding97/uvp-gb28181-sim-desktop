//! 语音广播 / 对讲下行(平台 → 设备)。
//!
//! 流程对齐 GB/T 28181 §9.8 与移动端模拟器的实现:
//! 1. 平台下发 `Broadcast` Notify,设备回 `Broadcast Response`;
//! 2. 设备作为 UAC 向平台发反向 INVITE,`s=Broadcast`、`a=recvonly`、
//!    `m=audio <port> RTP/AVP|TCP/RTP/AVP 8 0`(按 [`AudioTransport`] 选择);
//! 3. 平台 200 OK 的 SDP answer 给出媒体地址/端口,设备回 ACK;
//! 4. 设备按 UDP / TCP 主动 / TCP 被动收 RTP,解 G.711A/U 为 PCM 交给
//!    [`BroadcastAudioSink`](桌面端由 Tauri 层转成 Web Audio 播放);
//! 5. 平台 BYE(或本端停止)结束会话,释放端口。
//!
//! ⚠️ 本模块只做下行(平台 → 设备)。设备麦克风上行(真正的双向对讲)不在此列。

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use common::{AudioTransport, DeviceEvent, DeviceObserver};
use gb28181_protocol::manscdp::BroadcastNotify;
use sip_core::{BroadcastAnswer, SipMessage, SipTransport, Timing};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{watch, Mutex};

use crate::builder::{self, BroadcastDialogIds};
use crate::device::DeviceConfig;

/// 采样率:GB28181 语音广播固定 G.711 8kHz 单声道。
pub const BROADCAST_SAMPLE_RATE: u32 = 8_000;
/// INVITE 无最终响应的等待上限(按 T1=1s、T2=2s 退避)。
const INVITE_TIMING: Timing = Timing {
    t1: Duration::from_secs(1),
    t2: Duration::from_secs(2),
    max_retransmits: 4,
};
/// TCP 主动连接平台 / TCP 被动等待平台连入的超时。
const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const TCP_ACCEPT_TIMEOUT: Duration = Duration::from_secs(10);

/// 设备侧音频输出。桌面端实现为 Tauri 事件 → 前端 Web Audio;
/// 测试/压测可注入记录型实现验证解码结果。
pub trait BroadcastAudioSink: Send + Sync {
    /// 一段 8kHz 单声道 PCM(每包通常 160 个采样点 = 20ms)。
    fn on_pcm(&self, pcm: &[i16], sample_rate: u32);
}

/// 会话结束原因(上报给 UI / 日志)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastEndReason {
    /// 平台下发 BYE。
    RemoteBye,
    /// 本端主动停止(UI 停止 / 设备下线)。
    LocalStop,
    /// INVITE 无最终响应。
    InviteTimeout,
    /// 平台拒绝 INVITE(4xx-6xx)。
    InviteRejected,
    /// answer 的 SDP 不可用。
    InvalidAnswer,
    /// 媒体端口绑定/建连/accept 失败。
    MediaError,
    /// RTP 接收链路意外结束(TCP EOF 等)。
    RxEnded,
}

impl BroadcastEndReason {
    /// 稳定字符串标识(事件上报用)。
    pub fn as_str(&self) -> &'static str {
        match self {
            BroadcastEndReason::RemoteBye => "remote_bye",
            BroadcastEndReason::LocalStop => "local_stop",
            BroadcastEndReason::InviteTimeout => "invite_timeout",
            BroadcastEndReason::InviteRejected => "invite_rejected",
            BroadcastEndReason::InvalidAnswer => "invalid_answer",
            BroadcastEndReason::MediaError => "media_error",
            BroadcastEndReason::RxEnded => "rx_ended",
        }
    }
}

struct Session {
    ids: BroadcastDialogIds,
    transport: AudioTransport,
    local_port: u16,
    remote_tag: Option<String>,
    remote_target: Option<String>,
    packets: Arc<AtomicU64>,
    bytes: Arc<AtomicU64>,
    rx_stop: Option<watch::Sender<bool>>,
    rx_task: Option<tokio::task::JoinHandle<()>>,
}

impl Session {
    fn packet_count(&self) -> u64 {
        self.packets.load(Ordering::Relaxed)
    }

    fn byte_count(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }
}

/// 一台设备的语音广播会话管理器。
pub struct BroadcastRunner {
    config: DeviceConfig,
    observer: Arc<dyn DeviceObserver>,
    audio: RwLock<Option<Arc<dyn BroadcastAudioSink>>>,
    session: Mutex<Option<Session>>,
    /// 取消正在等待 200 OK 的 INVITE(设备下线 / UI 停止时)。
    invite_cancel: Mutex<watch::Sender<bool>>,
    /// 快速忙判(避免每次 Broadcast 都抢异步锁)。
    active: AtomicBool,
}

impl BroadcastRunner {
    /// 构造(设备初始化时调用)。
    pub fn new(config: DeviceConfig, observer: Arc<dyn DeviceObserver>) -> Self {
        let (cancel_tx, _) = watch::channel(false);
        BroadcastRunner {
            config,
            observer,
            audio: RwLock::new(None),
            session: Mutex::new(None),
            invite_cancel: Mutex::new(cancel_tx),
            active: AtomicBool::new(false),
        }
    }

    /// 注入音频输出(桌面端在启动设备前注入)。
    pub fn set_audio_sink(&self, sink: Arc<dyn BroadcastAudioSink>) {
        if let Ok(mut guard) = self.audio.write() {
            *guard = Some(sink);
        }
    }

    /// 当前是否有活跃广播会话。
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    /// 处理平台 Broadcast 通知:接受则发起反向 INVITE。返回 false 表示设备正忙。
    ///
    /// 调用方负责先回 `Broadcast Response`,这里只负责媒体面。
    pub fn start(
        self: &Arc<Self>,
        transport: Arc<SipTransport>,
        local_host: String,
        local_port: u16,
        notify: BroadcastNotify,
    ) -> bool {
        if self.active.swap(true, Ordering::AcqRel) {
            return false;
        }
        let runner = Arc::clone(self);
        tokio::spawn(async move {
            runner
                .run_invite(transport, local_host, local_port, notify)
                .await;
        });
        true
    }

    /// 平台 BYE:命中本设备广播对话则结束并返回 true(调用方回 200 OK)。
    pub async fn handle_bye(&self, request: &sip_core::Request) -> bool {
        let call_id = match request.headers.call_id() {
            Some(id) => id.to_string(),
            None => return false,
        };
        let matches = {
            let guard = self.session.lock().await;
            guard
                .as_ref()
                .map(|session| session.ids.call_id == call_id)
                .unwrap_or(false)
        };
        if !matches {
            return false;
        }
        self.finish_end(BroadcastEndReason::RemoteBye, None, None, None, false)
            .await;
        true
    }

    /// 主动停止(UI 停止 / 设备下线):取消未决 INVITE,并尽力向平台发 BYE。
    pub async fn stop(
        &self,
        transport: Arc<SipTransport>,
        dst: SocketAddr,
        local_host: String,
        local_port: u16,
    ) {
        {
            let sender = self.invite_cancel.lock().await;
            let _ = sender.send_replace(true);
        }
        self.finish_end(
            BroadcastEndReason::LocalStop,
            Some(transport),
            Some(dst),
            Some((local_host, local_port)),
            true,
        )
        .await;
    }

    // ── 内部实现 ─────────────────────────────────────────

    async fn run_invite(
        self: Arc<Self>,
        transport: Arc<SipTransport>,
        local_host: String,
        local_port: u16,
        notify: BroadcastNotify,
    ) {
        let mode = self.config.audio_transport;
        let endpoint = match MediaEndpoint::bind(mode).await {
            Ok(endpoint) => endpoint,
            Err(error) => {
                tracing::warn!(%error, "语音广播媒体端口绑定失败");
                self.fail(BroadcastEndReason::MediaError).await;
                return;
            }
        };

        let local_ip = match resolve_local_ip(&local_host, &transport) {
            Some(ip) => ip,
            None => {
                tracing::warn!(local_host, "语音广播缺少可用的本端媒体地址");
                self.fail(BroadcastEndReason::MediaError).await;
                return;
            }
        };

        let ids = BroadcastDialogIds::new(&notify.target_id, &notify.source_id);
        let media_port = endpoint.port;
        let sdp = sip_core::build_broadcast_offer(
            self.config.device_id.as_str(),
            local_ip,
            media_port,
            ids.ssrc,
            mode,
        );

        let packets = Arc::new(AtomicU64::new(0));
        let bytes = Arc::new(AtomicU64::new(0));
        {
            let mut slot = self.session.lock().await;
            if slot.is_some() {
                self.active.store(false, Ordering::Release);
                return;
            }
            *slot = Some(Session {
                ids: ids.clone(),
                transport: mode,
                local_port: media_port,
                remote_tag: None,
                remote_target: None,
                packets: Arc::clone(&packets),
                bytes: Arc::clone(&bytes),
                rx_stop: None,
                rx_task: None,
            });
        }

        let dst: SocketAddr =
            match format!("{}:{}", self.config.server_host, self.config.server_port).parse() {
                Ok(addr) => addr,
                Err(_) => {
                    self.fail(BroadcastEndReason::MediaError).await;
                    return;
                }
            };
        let request = builder::invite_broadcast(&self.config, &ids, &sdp, &local_host, local_port);

        let mut cancel = {
            let sender = self.invite_cancel.lock().await;
            let _ = sender.send_replace(false);
            sender.subscribe()
        };

        let mut rx = transport.register(ids.call_id.clone());
        let response = tokio::select! {
            biased;
            _ = wait_cancelled(&mut cancel) => None,
            result = sip_core::client_transact(&transport, dst, &request, &mut rx, INVITE_TIMING) => Some(result),
        };
        // 事务结束即注销 Call-ID 路由:之后的对话内 BYE 要按 Request-URI 落到设备入站队列。
        transport.unregister(&ids.call_id);

        let response = match response {
            None => {
                tracing::info!(device = %self.config.device_id, "语音广播 INVITE 被本端取消");
                self.finish_end(BroadcastEndReason::LocalStop, None, None, None, false)
                    .await;
                return;
            }
            Some(Ok(response)) => response,
            Some(Err(error)) => {
                tracing::warn!(%error, "语音广播 INVITE 无最终响应");
                self.finish_end(
                    BroadcastEndReason::InviteTimeout,
                    None,
                    None,
                    None,
                    false,
                )
                .await;
                return;
            }
        };

        if !(200..300).contains(&response.status) {
            tracing::warn!(status = response.status, "语音广播 INVITE 被平台拒绝");
            self.finish_end(
                BroadcastEndReason::InviteRejected,
                None,
                None,
                None,
                false,
            )
            .await;
            return;
        }

        let remote_tag = header_tag(&response.headers, "To");
        let remote_target = contact_uri(&response.headers);
        let Some(remote_tag) = remote_tag else {
            tracing::warn!("语音广播 200 OK 缺少 To-tag");
            self.finish_end(BroadcastEndReason::InvalidAnswer, None, None, None, false)
                .await;
            return;
        };

        // 先 ACK:即便 answer 不可用,也要按 RFC 3261 结束 2xx 事务再 BYE。
        let ack = builder::ack_broadcast(
            &self.config,
            &ids,
            &remote_tag,
            remote_target.as_deref(),
            &local_host,
            local_port,
        );
        if let Err(error) = transport
            .send_to(&SipMessage::Request(ack), dst)
            .await
        {
            tracing::warn!(%error, "语音广播 ACK 发送失败");
        }

        let answer = match sip_core::parse_broadcast_answer(&String::from_utf8_lossy(&response.body))
        {
            Ok(answer) => answer,
            Err(error) => {
                tracing::warn!(%error, "语音广播应答 SDP 不可用");
                self.finish_end(
                    BroadcastEndReason::InvalidAnswer,
                    Some(Arc::clone(&transport)),
                    Some(dst),
                    Some((local_host, local_port)),
                    true,
                )
                .await;
                return;
            }
        };

        let receiver = match endpoint
            .into_receiver(&answer, mode)
            .await
        {
            Ok(receiver) => receiver,
            Err(error) => {
                tracing::warn!(%error, "语音广播媒体链路建立失败");
                self.finish_end(
                    BroadcastEndReason::MediaError,
                    Some(Arc::clone(&transport)),
                    Some(dst),
                    Some((local_host, local_port)),
                    true,
                )
                .await;
                return;
            }
        };

        let (rx_stop_tx, rx_stop_rx) = watch::channel(false);
        {
            let mut slot = self.session.lock().await;
            let Some(session) = slot.as_mut() else {
                self.active.store(false, Ordering::Release);
                return;
            };
            if session.ids.call_id != ids.call_id {
                return;
            }
            session.remote_tag = Some(remote_tag.clone());
            session.remote_target = remote_target.clone();
            session.rx_stop = Some(rx_stop_tx);
        }

        let codec_name = answer.codec_name();
        let remote_media = format!("{}:{}", answer.remote_ip, answer.remote_port);
        let runner = Arc::clone(&self);
        let rx_transport = Arc::clone(&transport);
        let rx_packets = Arc::clone(&packets);
        let rx_bytes = Arc::clone(&bytes);
        let rx_local_host = local_host.clone();
        let task = tokio::spawn(async move {
            runner
                .rx_loop(
                    receiver,
                    answer,
                    rx_transport,
                    dst,
                    rx_local_host,
                    local_port,
                    rx_packets,
                    rx_bytes,
                    rx_stop_rx,
                )
                .await;
        });
        if let Some(session) = self.session.lock().await.as_mut() {
            session.rx_task = Some(task);
        }

        self.observer.on_event(DeviceEvent::BroadcastStarted {
            channel_id: notify.target_id.clone(),
            source_id: notify.source_id.clone(),
            transport: mode.as_str().to_string(),
            local_port: media_port,
        });
        self.observer.on_event(DeviceEvent::PlatformCommand {
            kind: "broadcast".into(),
            summary: format!(
                "语音广播建立 ← {} · {} · 本地端口 {} · {}",
                notify.source_id,
                mode,
                media_port,
                codec_name
            ),
        });
        tracing::info!(
            device = %self.config.device_id,
            channel = %notify.target_id,
            mode = %mode,
            remote = %remote_media,
            codec = codec_name,
            "语音广播已建立"
        );
    }

    #[allow(clippy::too_many_arguments)]
    async fn rx_loop(
        self: Arc<Self>,
        mut receiver: Receiver,
        answer: BroadcastAnswer,
        transport: Arc<SipTransport>,
        dst: SocketAddr,
        local_host: String,
        local_port: u16,
        packets: Arc<AtomicU64>,
        bytes: Arc<AtomicU64>,
        mut stop: watch::Receiver<bool>,
    ) {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut locked_ssrc: Option<u32> = None;
        let mut last_packets = 0u64;

        loop {
            tokio::select! {
                biased;
                _ = wait_cancelled(&mut stop) => break,
                _ = ticker.tick() => {
                    let count = packets.load(Ordering::Relaxed);
                    if count != last_packets {
                        last_packets = count;
                        self.observer.on_event(DeviceEvent::BroadcastProgress {
                            packets: count,
                            bytes: bytes.load(Ordering::Relaxed),
                        });
                    }
                }
                packet = receiver.next_packet() => {
                    match packet {
                        Ok(Some(packet)) => {
                            if let Some(rtp) = RtpPacket::parse(&packet) {
                                if locked_ssrc.is_none() {
                                    locked_ssrc = Some(rtp.ssrc);
                                }
                                if locked_ssrc != Some(rtp.ssrc) {
                                    continue;
                                }
                                self.push_audio(&rtp, &answer, &packets, &bytes);
                            }
                        }
                        Ok(None) => {
                            tracing::info!("语音广播 RTP 链路结束(EOF)");
                            break;
                        }
                        Err(error) => {
                            if !*stop.borrow() {
                                tracing::warn!(%error, "语音广播 RTP 接收错误");
                            }
                            break;
                        }
                    }
                }
            }
        }

        // 非本端停止的意外结束,要通知平台并落终态。
        let unexpected = !*stop.borrow();
        self.finish_end(
            if unexpected {
                BroadcastEndReason::RxEnded
            } else {
                BroadcastEndReason::LocalStop
            },
            Some(transport),
            Some(dst),
            Some((local_host, local_port)),
            unexpected,
        )
        .await;
    }

    fn push_audio(
        &self,
        rtp: &RtpPacket<'_>,
        answer: &BroadcastAnswer,
        packets: &AtomicU64,
        bytes: &AtomicU64,
    ) {
        let pcm: Vec<i16> = match answer.payload_type {
            8 => rtp.payload.iter().map(|byte| decode_alaw(*byte)).collect(),
            0 => rtp.payload.iter().map(|byte| decode_ulaw(*byte)).collect(),
            _ => return,
        };
        packets.fetch_add(1, Ordering::Relaxed);
        bytes.fetch_add(rtp.payload.len() as u64, Ordering::Relaxed);
        if let Ok(guard) = self.audio.read() {
            if let Some(sink) = guard.as_ref() {
                sink.on_pcm(&pcm, BROADCAST_SAMPLE_RATE);
            }
        }
    }

    /// 清空会话并上报结束(`send_bye` 时尽力通知平台)。
    async fn finish_end(
        &self,
        reason: BroadcastEndReason,
        transport: Option<Arc<SipTransport>>,
        dst: Option<SocketAddr>,
        local: Option<(String, u16)>,
        send_bye: bool,
    ) {
        let session = {
            let mut guard = self.session.lock().await;
            guard.take()
        };
        let Some(session) = session else {
            return;
        };
        self.active.store(false, Ordering::Release);

        if let Some(stop) = &session.rx_stop {
            let _ = stop.send(true);
        }
        if let Some(task) = &session.rx_task {
            task.abort();
        }

        if send_bye {
            if let (Some(tag), Some(transport), Some(dst), Some((host, port))) =
                (session.remote_tag.as_deref(), transport.as_ref(), dst, local.as_ref())
            {
                let bye = builder::bye_broadcast(
                    &self.config,
                    &session.ids,
                    tag,
                    session.remote_target.as_deref(),
                    host,
                    *port,
                );
                if let Err(error) = transport
                    .send_to(&SipMessage::Request(bye), dst)
                    .await
                {
                    tracing::warn!(%error, "语音广播 BYE 发送失败");
                }
            }
        }

        tracing::info!(
            device = %self.config.device_id,
            reason = reason.as_str(),
            mode = %session.transport,
            local_port = session.local_port,
            packets = session.packet_count(),
            bytes = session.byte_count(),
            "语音广播结束"
        );
        self.observer.on_event(DeviceEvent::BroadcastEnded {
            reason: reason.as_str().to_string(),
            packets: session.packet_count(),
            bytes: session.byte_count(),
        });
    }

    /// 失败路径(尚未登记会话时也能上报结束)。
    async fn fail(&self, reason: BroadcastEndReason) {
        self.active.store(false, Ordering::Release);
        self.observer.on_event(DeviceEvent::BroadcastEnded {
            reason: reason.as_str().to_string(),
            packets: 0,
            bytes: 0,
        });
    }
}

/// 绑定媒体端口。TCP 主动模式也绑定一个临时监听:
/// 平台侧 `ParseBroadcastOffer` 拒绝 `m=audio 0`,需要一个非零端口占位;
/// 该端口不会被使用(连接由本端主动发起),拿到 answer 后即释放。
struct MediaEndpoint {
    port: u16,
    udp: Option<UdpSocket>,
    listener: Option<TcpListener>,
}

impl MediaEndpoint {
    async fn bind(mode: AudioTransport) -> std::io::Result<Self> {
        match mode {
            AudioTransport::Udp => {
                let socket = UdpSocket::bind("0.0.0.0:0").await?;
                let port = socket.local_addr()?.port();
                Ok(MediaEndpoint {
                    port,
                    udp: Some(socket),
                    listener: None,
                })
            }
            AudioTransport::TcpActive | AudioTransport::TcpPassive => {
                let listener = TcpListener::bind("0.0.0.0:0").await?;
                let port = listener.local_addr()?.port();
                Ok(MediaEndpoint {
                    port,
                    udp: None,
                    listener: Some(listener),
                })
            }
        }
    }

    async fn into_receiver(
        self,
        answer: &BroadcastAnswer,
        mode: AudioTransport,
    ) -> std::io::Result<Receiver> {
        match mode {
            AudioTransport::Udp => Ok(Receiver::Udp {
                socket: self.udp.expect("UDP 模式必须已绑定 socket"),
            }),
            AudioTransport::TcpActive => {
                let stream = tokio::time::timeout(
                    TCP_CONNECT_TIMEOUT,
                    TcpStream::connect((answer.remote_ip, answer.remote_port)),
                )
                .await
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "TCP 主动连接平台超时"))??;
                Ok(Receiver::Tcp(stream))
            }
            AudioTransport::TcpPassive => {
                let listener = self.listener.expect("TCP 被动模式必须已绑定监听");
                let (stream, _) = tokio::time::timeout(TCP_ACCEPT_TIMEOUT, listener.accept())
                    .await
                    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "等待平台 TCP 连入超时"))??;
                Ok(Receiver::Tcp(stream))
            }
        }
    }
}

enum Receiver {
    Udp { socket: UdpSocket },
    Tcp(TcpStream),
}

impl Receiver {
    /// 取下一帧 RTP(TCP 按 RFC 4571 两字节大端长度解帧)。
    async fn next_packet(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        match self {
            Receiver::Udp { socket } => {
                let mut buffer = vec![0u8; 2048];
                let (size, _) = socket.recv_from(&mut buffer).await?;
                buffer.truncate(size);
                Ok(Some(buffer))
            }
            Receiver::Tcp(stream) => read_tcp_frame(stream).await,
        }
    }
}

async fn read_tcp_frame(stream: &mut TcpStream) -> std::io::Result<Option<Vec<u8>>> {
    let mut length = [0u8; 2];
    match stream.read_exact(&mut length).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let size = u16::from_be_bytes(length) as usize;
    if size == 0 {
        return Ok(None);
    }
    let mut frame = vec![0u8; size];
    stream.read_exact(&mut frame).await?;
    Ok(Some(frame))
}

struct RtpPacket<'a> {
    /// 序号(暂用于测试/后续丢包统计)。
    #[allow(dead_code)]
    seq: u16,
    ssrc: u32,
    payload: &'a [u8],
}

impl<'a> RtpPacket<'a> {
    fn parse(packet: &'a [u8]) -> Option<Self> {
        if packet.len() < 12 || packet[0] >> 6 != 2 {
            return None;
        }
        let csrc_count = (packet[0] & 0x0F) as usize;
        let padding = packet[0] & 0x20 != 0;
        let seq = u16::from_be_bytes([packet[2], packet[3]]);
        let ssrc = u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]);
        let start = 12 + csrc_count * 4;
        let mut end = packet.len();
        if padding {
            let pad = *packet.last()? as usize;
            if pad == 0 || start + pad > end {
                return None;
            }
            end -= pad;
        }
        if start >= end {
            return None;
        }
        Some(RtpPacket {
            seq,
            ssrc,
            payload: &packet[start..end],
        })
    }
}

/// G.711 A-law → 16-bit PCM(ITU-T G.711)。
pub fn decode_alaw(byte: u8) -> i16 {
    let value = byte ^ 0x55;
    let mut sample = ((value & 0x0F) as i32) << 4;
    let segment = ((value & 0x70) >> 4) as i32;
    match segment {
        0 => sample += 8,
        1 => sample += 0x108,
        _ => {
            sample += 0x108;
            sample <<= segment - 1;
        }
    }
    if value & 0x80 != 0 {
        sample as i16
    } else {
        (-sample) as i16
    }
}

/// G.711 μ-law → 16-bit PCM(部分平台用 PCMU)。
pub fn decode_ulaw(byte: u8) -> i16 {
    let value = !byte as i32;
    let sign = value & 0x80;
    let exponent = (value >> 4) & 0x07;
    let mantissa = value & 0x0F;
    let mut sample = ((mantissa << 3) + 0x84) << exponent;
    sample -= 0x84;
    if sign != 0 {
        (-sample) as i16
    } else {
        sample as i16
    }
}

/// 从 From/To 头里取 tag 值。
fn header_tag(headers: &sip_core::Headers, name: &str) -> Option<String> {
    let value = headers.get(name)?;
    let index = value.find("tag=")?;
    let rest = &value[index + 4..];
    let end = rest
        .find(|c: char| c == ';' || c == ',' || c.is_whitespace())
        .unwrap_or(rest.len());
    let tag = rest[..end].trim();
    if tag.is_empty() {
        None
    } else {
        Some(tag.to_string())
    }
}

/// 取 Contact 头里的 URI(`<sip:...>` 内层)。
fn contact_uri(headers: &sip_core::Headers) -> Option<String> {
    let value = headers.get("Contact")?;
    if let (Some(start), Some(end)) = (value.find('<'), value.find('>')) {
        if end > start + 1 {
            return Some(value[start + 1..end].trim().to_string());
        }
    }
    Some(value.split(';').next()?.trim().to_string())
}

/// 本端媒体地址:优先信令实际绑定的 host,未指定时退回 UDP socket 的本地地址。
fn resolve_local_ip(local_host: &str, transport: &SipTransport) -> Option<IpAddr> {
    if let Ok(ip) = local_host.parse::<IpAddr>() {
        if !ip.is_unspecified() {
            return Some(ip);
        }
    }
    transport
        .local_addr()
        .ok()
        .map(|addr| addr.ip())
        .filter(|ip| !ip.is_unspecified())
}

async fn wait_cancelled(stop: &mut watch::Receiver<bool>) {
    if *stop.borrow() {
        return;
    }
    let _ = stop.changed().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alaw_silence_and_sign() {
        // A-law 静音 0xD5 → +8,0x55 → -8(LSB 抖动)。
        assert_eq!(decode_alaw(0xD5), 8);
        assert_eq!(decode_alaw(0x55), -8);
        // 满幅。
        assert_eq!(decode_alaw(0xAA), 32256);
        assert_eq!(decode_alaw(0x2A), -32256);
    }

    #[test]
    fn ulaw_silence_and_sign() {
        assert_eq!(decode_ulaw(0xFF), 0);
        assert_eq!(decode_ulaw(0x7F), 0);
        assert_eq!(decode_ulaw(0x80), 32124);
        assert_eq!(decode_ulaw(0x00), -32124);
    }

    #[test]
    fn rtp_parse_strips_header_and_padding() {
        let mut packet = vec![0x80, 8, 0x00, 0x2A, 0, 0, 0, 0, 0, 0, 0, 1];
        packet.extend_from_slice(&[0xD5, 0xD5]);
        let rtp = RtpPacket::parse(&packet).unwrap();
        assert_eq!(rtp.seq, 42);
        assert_eq!(rtp.ssrc, 1);
        assert_eq!(rtp.payload, &[0xD5, 0xD5]);

        // 带 padding:最后一个字节是 padding 长度。
        let mut padded = vec![0xA0, 8, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2];
        padded.extend_from_slice(&[0xD5, 0, 0, 2]);
        let rtp = RtpPacket::parse(&padded).unwrap();
        assert_eq!(rtp.payload, &[0xD5, 0]);

        // 非 RTP / 头部不完整。
        assert!(RtpPacket::parse(&[0x40, 0, 0]).is_none());
    }

    #[test]
    fn header_tag_and_contact_parse() {
        let mut headers = sip_core::Headers::new();
        headers.set("To", "<sip:34020000002000000001@1.2.3.4:5060>;tag=abc123;foo=bar");
        headers.set("Contact", "<sip:34020000002000000001@1.2.3.4:5060>");
        assert_eq!(header_tag(&headers, "To").as_deref(), Some("abc123"));
        assert_eq!(
            contact_uri(&headers).as_deref(),
            Some("sip:34020000002000000001@1.2.3.4:5060")
        );
        assert_eq!(header_tag(&headers, "From"), None);
    }
}
