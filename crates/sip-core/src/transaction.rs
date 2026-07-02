//! SIP 事务层。
//!
//! 实现 docs/30-crates/sip-core.md §transaction。M1 提供**客户端事务**:发送请求后
//! 等待响应,UDP 下按 T1 指数退避重传,直到收到最终响应或达到重传上限。
//! 响应与请求按 CSeq 序号匹配(同一 Call-ID 队列内)。

use std::net::SocketAddr;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::timeout;

use common::{Error, Result};

use crate::message::{Request, Response, SipMessage};
use crate::transport::{Incoming, UdpTransport};

/// 事务定时参数。默认贴近 RFC 3261(T1=500ms),测试可调小。
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// 初始重传间隔(T1)。
    pub t1: Duration,
    /// 重传间隔上限(T2)。
    pub t2: Duration,
    /// 最大重传次数(不含首发)。
    pub max_retransmits: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            t1: Duration::from_millis(500),
            t2: Duration::from_secs(4),
            max_retransmits: 6,
        }
    }
}

/// 客户端事务结果:携最终响应(1xx 临时响应会被跳过,直到 2xx-6xx)。
pub type TxResult = Result<Response>;

/// 执行一次客户端事务:发送 `request` 到 `dst`,在 `rx`(该 Call-ID 的接收队列)
/// 上等待匹配响应。UDP 下超时按 t1→2×…(上限 t2)退避重传,超过上限返回超时错误
/// (对应压测失败归因的 `timeout`)。
///
/// 匹配规则:响应 CSeq 序号与请求一致。临时响应(1xx)不结束事务,继续等最终响应。
pub async fn client_transact(
    transport: &UdpTransport,
    dst: SocketAddr,
    request: &Request,
    rx: &mut mpsc::UnboundedReceiver<Incoming>,
    timing: Timing,
) -> TxResult {
    let cseq = request.headers.cseq_number();
    let msg = SipMessage::Request(request.clone());

    // 首发。
    transport.send_to(&msg, dst).await?;

    let mut interval = timing.t1;
    let mut retransmits = 0u32;

    loop {
        match timeout(interval, wait_matching(rx, cseq)).await {
            Ok(Some(resp)) => return Ok(resp),
            Ok(None) => return Err(Error::Sip("事务接收队列已关闭".into())),
            Err(_) => {
                // 超时:重传或放弃。
                if retransmits >= timing.max_retransmits {
                    return Err(Error::Sip(format!(
                        "事务超时:{} 无最终响应(重传 {} 次)",
                        request.method, retransmits
                    )));
                }
                transport.send_to(&msg, dst).await?;
                retransmits += 1;
                interval = (interval * 2).min(timing.t2);
            }
        }
    }
}

/// 从队列取消息,返回首个 CSeq 匹配的**最终**响应(跳过 1xx 与请求)。
async fn wait_matching(
    rx: &mut mpsc::UnboundedReceiver<Incoming>,
    cseq: Option<u32>,
) -> Option<Response> {
    loop {
        let incoming = rx.recv().await?;
        if let SipMessage::Response(resp) = incoming.message {
            let matches = cseq.is_none() || resp.headers.cseq_number() == cseq;
            if matches && resp.status >= 200 {
                return Some(resp);
            }
            // 1xx 或不匹配:继续等。
        }
        // 请求(平台反向发来的)不在客户端事务范围,忽略。
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Headers, Method};

    fn make_request(call_id: &str, cseq: u32) -> Request {
        let mut headers = Headers::new();
        headers.set("Call-ID", call_id);
        headers.set("CSeq", format!("{cseq} REGISTER"));
        Request {
            method: Method::Register,
            uri: "sip:platform@127.0.0.1".into(),
            headers,
            body: Vec::new(),
        }
    }

    #[tokio::test]
    async fn 收到_200_即结束事务() {
        let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let dst = platform.local_addr().unwrap();
        let device_addr = device.local_addr().unwrap();

        let mut rx = device.register("call-1");

        // 平台侧:收到请求后回 200(带同 Call-ID/CSeq)。
        let mut prx = platform.register("call-1");
        let platform2 = platform.clone();
        tokio::spawn(async move {
            if let Some(inc) = prx.recv().await {
                if let SipMessage::Request(req) = inc.message {
                    let mut h = Headers::new();
                    h.set("Call-ID", "call-1");
                    h.set("CSeq", req.headers.cseq().unwrap_or("1 REGISTER"));
                    let resp = SipMessage::Response(Response {
                        status: 200,
                        reason: "OK".into(),
                        headers: h,
                        body: Vec::new(),
                    });
                    let _ = platform2.send_to(&resp, device_addr).await;
                }
            }
        });

        let req = make_request("call-1", 1);
        let resp = client_transact(&device, dst, &req, &mut rx, Timing::default())
            .await
            .unwrap();
        assert_eq!(resp.status, 200);
    }

    #[tokio::test]
    async fn 无响应则超时() {
        let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        // 目标指向一个无人应答的本地端口。
        let dst: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let mut rx = device.register("call-x");
        let req = make_request("call-x", 1);
        // 极短定时,快速走完重传上限。
        let timing = Timing {
            t1: Duration::from_millis(5),
            t2: Duration::from_millis(10),
            max_retransmits: 2,
        };
        let r = client_transact(&device, dst, &req, &mut rx, timing).await;
        assert!(r.is_err());
    }
}
