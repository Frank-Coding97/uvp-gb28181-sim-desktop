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
    /// Call-ID → 发起事务的设备的响应投递端。
    routes: Arc<DashMap<String, mpsc::UnboundedSender<Incoming>>>,
    /// 设备 AOR(device_id)→ 该设备的入站请求投递端。
    aor_routes: Arc<DashMap<String, mpsc::UnboundedSender<Incoming>>>,
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
            aor_routes: Arc::new(DashMap::new()),
        });
        transport.clone().spawn_recv_loop();
        Ok(transport)
    }

    /// 本地实际绑定地址(端口由系统分配时用于回填 Contact/Via)。
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.socket.local_addr().map_err(Error::Io)
    }

    /// 为某 Call-ID 注册响应接收队列(客户端事务用)。同一 Call-ID 重复注册覆盖。
    pub fn register(&self, call_id: impl Into<String>) -> mpsc::UnboundedReceiver<Incoming> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.routes.insert(call_id.into(), tx);
        rx
    }

    /// 为某设备 AOR(device_id)注册入站请求队列(接收平台主动发来的 OPTIONS/查询)。
    pub fn register_inbound(&self, aor: impl Into<String>) -> mpsc::UnboundedReceiver<Incoming> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.aor_routes.insert(aor.into(), tx);
        rx
    }

    /// 注销某 Call-ID 的响应路由(事务结束时可调)。
    pub fn unregister(&self, call_id: &str) {
        self.routes.remove(call_id);
    }

    /// 注销某设备 AOR 的入站路由(设备下线时调)。
    pub fn unregister_inbound(&self, aor: &str) {
        self.aor_routes.remove(aor);
    }

    /// 发送一条 SIP 消息到目标地址。
    pub async fn send_to(&self, msg: &SipMessage, dst: SocketAddr) -> Result<()> {
        let bytes = msg.to_bytes();
        self.socket.send_to(&bytes, dst).await.map_err(Error::Io)?;
        Ok(())
    }

    /// 启动后台接收循环:收包 → 解析 → 分发。
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

    /// 分发:响应按 Call-ID 路由到事务队列;若 Call-ID 无匹配且是请求,
    /// 则按 Request-URI 的 AOR(user 部分)路由到设备入站队列。
    fn dispatch(&self, incoming: Incoming) {
        // 先按 Call-ID 尝试(响应、以及在对话内的请求)。
        let call_id = match &incoming.message {
            SipMessage::Request(r) => r.headers.call_id(),
            SipMessage::Response(r) => r.headers.call_id(),
        };
        if let Some(id) = call_id {
            if let Some(tx) = self.routes.get(id) {
                let _ = tx.send(incoming);
                return;
            }
        }
        // 未匹配 Call-ID:入站请求按 AOR 路由。
        if let SipMessage::Request(req) = &incoming.message {
            if let Some(aor) = uri_user(&req.uri) {
                if let Some(tx) = self.aor_routes.get(aor) {
                    let _ = tx.send(incoming);
                    return;
                }
                tracing::debug!(aor=%aor, "无匹配 AOR 入站路由,丢弃请求");
                return;
            }
        }
        tracing::debug!("消息无匹配路由,丢弃");
    }
}

/// 从 SIP URI(如 `sip:34020000001320000001@host:5060`)提取 user 部分(AOR)。
fn uri_user(uri: &str) -> Option<&str> {
    let after_scheme = uri.strip_prefix("sip:").unwrap_or(uri);
    after_scheme.split('@').next().filter(|s| !s.is_empty())
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

    #[tokio::test]
    async fn 入站请求按_aor_路由() {
        use crate::message::{Method, Request};

        let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let device_addr = device.local_addr().unwrap();

        // 设备按自身 AOR(device_id)注册入站队列。
        let mut inbound = device.register_inbound("34020000001320000001");

        // 平台发来 OPTIONS(新 Call-ID,Request-URI 指向该设备 AOR)。
        let mut headers = Headers::new();
        headers.set("Call-ID", "fresh-call-from-platform");
        headers.set("CSeq", "1 OPTIONS");
        let req = SipMessage::Request(Request {
            method: Method::Options,
            uri: "sip:34020000001320000001@127.0.0.1:5060".into(),
            headers,
            body: Vec::new(),
        });
        platform.send_to(&req, device_addr).await.unwrap();

        let got = tokio::time::timeout(std::time::Duration::from_secs(2), inbound.recv())
            .await
            .expect("超时未收到")
            .expect("队列关闭");
        match got.message {
            SipMessage::Request(r) => assert_eq!(r.method, Method::Options),
            _ => panic!("应为请求"),
        }
    }

    #[test]
    fn uri_user_提取() {
        assert_eq!(uri_user("sip:34020000001320000001@h:5060"), Some("34020000001320000001"));
        assert_eq!(uri_user("34020000001320000001@h"), Some("34020000001320000001"));
        assert_eq!(uri_user("sip:@h"), None);
    }
}
