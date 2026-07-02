//! SIP 传输层。
//!
//! 实现 docs/30-crates/sip-core.md §transport。M1 提供 UDP 传输,采用
//! **共享 socket + 按 Call-ID 路由分发** 的模型:一个 socket 承载多台虚拟设备的
//! 收发,收到的响应按 Call-ID 投递到对应设备的接收队列。这是压测万级设备的关键
//! (避免每设备一个 socket 耗尽端口/FD,见 docs/10-functional/stress-testing.md#42)。

use std::net::SocketAddr;
use std::sync::Arc;

use dashmap::DashMap;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use common::{Error, Result};

use crate::message::SipMessage;

/// 一条收到的消息 + 来源地址。
#[derive(Debug)]
pub struct Incoming {
    /// 解析后的 SIP 消息。
    pub message: SipMessage,
    /// 来源地址。
    pub from: SocketAddr,
}

/// 共享 UDP 传输。多设备复用一个 socket,按 Call-ID 分发收到的消息。
///
/// 用法:
/// 1. [`UdpTransport::bind`] 绑定本地端口并启动接收循环。
/// 2. 每台设备用其 Call-ID 调 [`UdpTransport::register`] 拿到接收端 `Receiver`。
/// 3. 发送用 [`UdpTransport::send_to`];接收循环把消息按 Call-ID 投递到对应队列。
pub struct UdpTransport {
    socket: Arc<UdpSocket>,
    /// Call-ID → 该设备的消息投递发送端。
    routes: Arc<DashMap<String, mpsc::UnboundedSender<Incoming>>>,
}

impl UdpTransport {
    /// 绑定本地地址(如 "0.0.0.0:0" 由系统分配端口),启动后台接收循环。
    pub async fn bind(local: &str) -> Result<Arc<Self>> {
        let socket = UdpSocket::bind(local)
            .await
            .map_err(Error::Io)?;
        let transport = Arc::new(UdpTransport {
            socket: Arc::new(socket),
            routes: Arc::new(DashMap::new()),
        });
        transport.clone().spawn_recv_loop();
        Ok(transport)
    }

    /// 本地实际绑定地址(端口由系统分配时用于回填 Contact/Via)。
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.socket.local_addr().map_err(Error::Io)
    }

    /// 为某 Call-ID 注册接收队列,返回接收端。同一 Call-ID 重复注册会覆盖旧队列。
    pub fn register(&self, call_id: impl Into<String>) -> mpsc::UnboundedReceiver<Incoming> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.routes.insert(call_id.into(), tx);
        rx
    }

    /// 注销某 Call-ID 的路由(设备下线时调)。
    pub fn unregister(&self, call_id: &str) {
        self.routes.remove(call_id);
    }

    /// 发送一条 SIP 消息到目标地址。
    pub async fn send_to(&self, msg: &SipMessage, dst: SocketAddr) -> Result<()> {
        let bytes = msg.to_bytes();
        self.socket.send_to(&bytes, dst).await.map_err(Error::Io)?;
        Ok(())
    }

    /// 启动后台接收循环:收包 → 解析 → 按 Call-ID 投递。
    fn spawn_recv_loop(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            loop {
                match self.socket.recv_from(&mut buf).await {
                    Ok((n, from)) => {
                        match SipMessage::parse(&buf[..n]) {
                            Ok(message) => self.dispatch(Incoming { message, from }),
                            Err(e) => tracing::warn!(%from, error=%e, "SIP 报文解析失败,丢弃"),
                        }
                    }
                    Err(e) => {
                        tracing::error!(error=%e, "UDP 接收错误,接收循环退出");
                        break;
                    }
                }
            }
        });
    }

    /// 按消息的 Call-ID 投递到对应设备队列;无路由则丢弃并告警。
    fn dispatch(&self, incoming: Incoming) {
        let call_id = match &incoming.message {
            SipMessage::Request(r) => r.headers.call_id(),
            SipMessage::Response(r) => r.headers.call_id(),
        };
        match call_id {
            Some(id) => {
                if let Some(tx) = self.routes.get(id) {
                    let _ = tx.send(incoming);
                } else {
                    tracing::debug!(call_id=%id, "无匹配 Call-ID 路由,丢弃");
                }
            }
            None => tracing::warn!("消息缺少 Call-ID,无法路由"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Headers, Response};

    #[tokio::test]
    async fn 共享_socket_按_callid_路由() {
        // 起两个 transport 模拟"设备端"与"平台端"。
        let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let device_addr = device.local_addr().unwrap();

        // 设备为 Call-ID "call-A" 注册接收队列。
        let mut rx = device.register("call-A");

        // 平台发一条带该 Call-ID 的 200 OK 给设备。
        let mut headers = Headers::new();
        headers.set("Call-ID", "call-A");
        headers.set("CSeq", "1 REGISTER");
        let resp = SipMessage::Response(Response {
            status: 200,
            reason: "OK".into(),
            headers,
            body: Vec::new(),
        });
        platform.send_to(&resp, device_addr).await.unwrap();

        // 设备应在 call-A 队列收到它。
        let got = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("超时未收到")
            .expect("队列关闭");
        match got.message {
            SipMessage::Response(r) => assert_eq!(r.status, 200),
            _ => panic!("应为响应"),
        }
    }
}
