//! SIP 事务层。
//!
//! 实现 docs/30-crates/sip-core.md §transaction,分两半:
//!
//! - **客户端事务** [`client_transact`]:发请求 → 等响应,UDP 下按 T1 指数退避重传。
//!   按 RFC 3261 §17.1 的关键行为:收到 1xx 临时响应后 INVITE 停止重传、非 INVITE
//!   把重传间隔钉在 T2;总超时按 Timer B/F = 64×T1;响应匹配看 branch + CSeq 方法。
//! - **服务端事务** [`ServerTransactions`]:对入站请求按 (branch, CSeq, method) 去重,
//!   缓存最终响应用于重传应答。缺了这一层会出现一个很具体的互操作 bug:平台的
//!   200 OK 丢包后重传 INVITE,设备因"已有会话"直接回 486 Busy Here,点播失败。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::time::timeout;

use common::{Error, Result};

use crate::message::{Method, Request, Response, SipMessage};
use crate::transport::{Incoming, SipTransport};

/// 事务定时参数。默认贴近 RFC 3261(T1=500ms),测试可调小。
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// 初始重传间隔(T1)。
    pub t1: Duration,
    /// 重传间隔上限(T2)。
    pub t2: Duration,
    /// 最大重传次数(不含首发)。
    pub max_retransmits: u32,
    /// 事务总超时(Timer B / Timer F)。RFC 3261 规定为 64×T1。
    ///
    /// 之前的实现只有"重传 6 次、间隔上限 4s"≈15.5s,比规范的 32s 短一半,
    /// 会把慢平台误判成超时失败,直接污染压测结论。
    pub total_timeout: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        let t1 = Duration::from_millis(500);
        Timing {
            t1,
            t2: Duration::from_secs(4),
            max_retransmits: 10,
            total_timeout: t1 * 64,
        }
    }
}

impl Timing {
    /// 按给定 T1 推导一套自洽的定时参数(T2 = 8×T1,Timer B/F = 64×T1)。
    pub fn from_t1(t1: Duration) -> Self {
        Timing {
            t1,
            t2: t1 * 8,
            max_retransmits: 10,
            total_timeout: t1 * 64,
        }
    }
}

/// 客户端事务结果:携最终响应(1xx 临时响应会被跳过,直到 2xx-6xx)。
pub type TxResult = Result<Response>;

/// 执行一次客户端事务:发送 `request` 到 `dst`,在 `rx`(该 Call-ID 的接收队列)
/// 上等待匹配响应。
///
/// 重传策略(RFC 3261 §17.1.1.2 / §17.1.2.2):
/// - 未收到任何响应:间隔 T1 → 2×T1 → …,非 INVITE 上限 T2,INVITE 不设上限。
/// - 收到 1xx 后:INVITE **停止重传**(等 Timer B),非 INVITE 间隔固定为 T2。
/// - 达到 `total_timeout` 或重传上限:返回超时错误(对应压测失败归因的 `timeout`)。
///
/// 匹配规则:优先按 Via branch,退回按 CSeq 序号 + 方法名。
pub async fn client_transact(
    transport: &dyn SipTransport,
    dst: SocketAddr,
    request: &Request,
    rx: &mut mpsc::Receiver<Incoming>,
    timing: Timing,
) -> TxResult {
    let matcher = ResponseMatcher::of(request);
    let is_invite = request.method == Method::Invite;
    let msg = SipMessage::Request(request.clone());

    // 首发。
    transport.send_to(&msg, dst).await?;

    let started = Instant::now();
    let mut interval = timing.t1;
    let mut retransmits = 0u32;
    let mut provisional_seen = false;

    loop {
        let elapsed = started.elapsed();
        if elapsed >= timing.total_timeout {
            return Err(timeout_error(request, retransmits, timing.total_timeout));
        }
        let remaining = timing.total_timeout - elapsed;
        // 收到 1xx 后 INVITE 不再重传:直接一路等到 Timer B。
        let wait = if provisional_seen && is_invite {
            remaining
        } else {
            interval.min(remaining)
        };

        match timeout(wait, wait_matching(rx, &matcher)).await {
            Ok(Some(Matched::Final(resp))) => return Ok(resp),
            Ok(Some(Matched::Provisional)) => {
                if !provisional_seen {
                    provisional_seen = true;
                    // 非 INVITE:临时响应后间隔钉在 T2(§17.1.2.2)。
                    if !is_invite {
                        interval = timing.t2;
                    }
                }
            }
            Ok(None) => return Err(Error::Sip("事务接收队列已关闭".into())),
            Err(_) => {
                // 本轮等待到点。
                if provisional_seen && is_invite {
                    return Err(timeout_error(request, retransmits, timing.total_timeout));
                }
                if retransmits >= timing.max_retransmits {
                    return Err(timeout_error(request, retransmits, timing.total_timeout));
                }
                if started.elapsed() >= timing.total_timeout {
                    return Err(timeout_error(request, retransmits, timing.total_timeout));
                }
                transport.send_to(&msg, dst).await?;
                retransmits += 1;
                interval = if is_invite {
                    interval * 2
                } else {
                    (interval * 2).min(timing.t2)
                };
            }
        }
    }
}

fn timeout_error(request: &Request, retransmits: u32, total: Duration) -> Error {
    // 用 `Error::Timeout` 而不是 `Error::Sip`:压测的失败归因据此分类,
    // 之前靠 `msg.contains("超时")` 匹配文案,改一次措辞就会静默归错类。
    Error::Timeout(format!(
        "{} 在 {:.1}s 内无最终响应(重传 {} 次)",
        request.method,
        total.as_secs_f32(),
        retransmits
    ))
}

/// 响应匹配依据。
struct ResponseMatcher {
    branch: Option<String>,
    cseq: Option<u32>,
    method: &'static str,
}

impl ResponseMatcher {
    fn of(request: &Request) -> Self {
        ResponseMatcher {
            branch: request.headers.via_branch().map(str::to_string),
            cseq: request.headers.cseq_number(),
            method: request.method.as_str(),
        }
    }

    /// RFC 3261 §17.1.3:响应属于本事务当且仅当 Via branch 相同且 CSeq 方法相同。
    /// 平台不回显 branch 时退化为 CSeq 序号 + 方法匹配。
    fn matches(&self, resp: &Response) -> bool {
        if let (Some(expect), Some(got)) = (self.branch.as_deref(), resp.headers.via_branch()) {
            if expect != got {
                return false;
            }
        }
        if let (Some(expect), Some(got)) = (self.cseq, resp.headers.cseq_number()) {
            if expect != got {
                return false;
            }
        }
        if let Some(got) = resp.headers.cseq_method() {
            if !got.eq_ignore_ascii_case(self.method) {
                return false;
            }
        }
        true
    }
}

enum Matched {
    Provisional,
    Final(Response),
}

/// 从队列取消息,返回首个匹配的响应。1xx 单独上报(调用方据此调整重传)。
async fn wait_matching(
    rx: &mut mpsc::Receiver<Incoming>,
    matcher: &ResponseMatcher,
) -> Option<Matched> {
    loop {
        let incoming = rx.recv().await?;
        if let SipMessage::Response(resp) = incoming.message {
            if !matcher.matches(&resp) {
                continue;
            }
            return Some(if resp.status >= 200 {
                Matched::Final(resp)
            } else {
                Matched::Provisional
            });
        }
        // 请求不属于客户端事务范围。传输层已改为"请求优先按 AOR 路由",
        // 正常不会走到这里;真走到了也只能忽略,由 AOR 队列侧负责。
    }
}

// ── 服务端事务 ────────────────────────────────────────────

/// 服务端事务标识:Via branch + CSeq 序号 + 方法。
///
/// branch 缺失(极老实现)时退化用 Call-ID 顶替,保证仍有去重能力。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TxKey {
    branch: String,
    cseq: u32,
    method: &'static str,
}

impl TxKey {
    fn of(request: &Request) -> Self {
        let branch = request
            .headers
            .via_branch()
            .or_else(|| request.headers.call_id())
            .unwrap_or("")
            .to_string();
        TxKey {
            branch,
            cseq: request.headers.cseq_number().unwrap_or(0),
            method: request.method.as_str(),
        }
    }
}

struct ServerTxState {
    /// 最终响应;None 表示仍在处理中。
    response: Option<Response>,
    /// 用于按 Timer J(64×T1)清理。
    touched: Instant,
}

/// 入站请求的处理决策。
pub enum Admit {
    /// 新事务,调用方应正常处理,并在产出最终响应后调 [`ServerTransactions::complete`]。
    Proceed(TxToken),
    /// 请求重传,且已有缓存的最终响应 —— 直接重发它,**不要**重新执行业务逻辑。
    Retransmit(Response),
    /// 请求重传,但上一轮仍在处理中 —— 吸收掉(RFC 3261 §17.2.1 的行为)。
    Absorb,
}

/// [`Admit::Proceed`] 的凭据,用于回填最终响应。
pub struct TxToken(TxKey);

/// 服务端事务表。按 (branch, CSeq, method) 去重入站请求并缓存最终响应。
pub struct ServerTransactions {
    map: Mutex<HashMap<TxKey, ServerTxState>>,
    /// 事务完成后保留响应的时长(Timer J = 64×T1)。
    linger: Duration,
    /// 表容量上限,防止被伪造 branch 的洪泛打爆内存。
    capacity: usize,
}

impl Default for ServerTransactions {
    fn default() -> Self {
        Self::new(Duration::from_millis(500) * 64, 4096)
    }
}

impl ServerTransactions {
    pub fn new(linger: Duration, capacity: usize) -> Self {
        ServerTransactions {
            map: Mutex::new(HashMap::new()),
            linger,
            capacity,
        }
    }

    /// 判定一条入站请求该如何处理。
    ///
    /// ACK 不构成独立事务(它确认的是 INVITE 的 2xx),因此始终 `Proceed`,
    /// 由上层自行幂等处理。
    pub fn admit(&self, request: &Request) -> Admit {
        if request.method == Method::Ack {
            return Admit::Proceed(TxToken(TxKey::of(request)));
        }
        let key = TxKey::of(request);
        let mut map = self.map.lock();
        self.purge_expired(&mut map);

        if let Some(state) = map.get_mut(&key) {
            state.touched = Instant::now();
            return match &state.response {
                Some(resp) => Admit::Retransmit(resp.clone()),
                None => Admit::Absorb,
            };
        }

        // 容量保护:满了先丢最旧的已完成事务,再不行就不记账(退化成无去重但不 OOM)。
        if map.len() >= self.capacity {
            self.evict_oldest(&mut map);
            if map.len() >= self.capacity {
                tracing::warn!(
                    capacity = self.capacity,
                    "服务端事务表已满,本条请求不参与去重"
                );
                return Admit::Proceed(TxToken(key));
            }
        }

        map.insert(
            key.clone(),
            ServerTxState {
                response: None,
                touched: Instant::now(),
            },
        );
        Admit::Proceed(TxToken(key))
    }

    /// 记录最终响应,供后续重传直接命中。1xx 不算最终响应,不记录。
    pub fn complete(&self, token: &TxToken, response: &Response) {
        if response.status < 200 {
            return;
        }
        let mut map = self.map.lock();
        if let Some(state) = map.get_mut(&token.0) {
            state.response = Some(response.clone());
            state.touched = Instant::now();
        }
    }

    /// 放弃一个事务(处理过程中出错、决定不回响应时调用),避免后续重传被永久 Absorb。
    pub fn abandon(&self, token: &TxToken) {
        self.map.lock().remove(&token.0);
    }

    /// 当前表内事务数(诊断/测试用)。
    pub fn len(&self) -> usize {
        self.map.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn purge_expired(&self, map: &mut HashMap<TxKey, ServerTxState>) {
        let linger = self.linger;
        let now = Instant::now();
        map.retain(|_, state| now.duration_since(state.touched) < linger);
    }

    fn evict_oldest(&self, map: &mut HashMap<TxKey, ServerTxState>) {
        let oldest = map
            .iter()
            .filter(|(_, s)| s.response.is_some())
            .min_by_key(|(_, s)| s.touched)
            .map(|(k, _)| k.clone());
        if let Some(key) = oldest {
            map.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Headers, Method};
    use crate::transport::UdpTransport;

    fn make_request(call_id: &str, cseq: u32) -> Request {
        let mut headers = Headers::new();
        headers.set("Call-ID", call_id);
        headers.set("CSeq", format!("{cseq} REGISTER"));
        headers.set(
            "Via",
            format!("SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bK-{call_id}-{cseq}"),
        );
        Request {
            method: Method::Register,
            uri: "sip:platform@127.0.0.1".into(),
            headers,
            body: Vec::new(),
        }
    }

    fn response_for(req: &Request, status: u16) -> SipMessage {
        let mut h = Headers::new();
        h.set("Call-ID", req.headers.call_id().unwrap_or("x"));
        h.set("CSeq", req.headers.cseq().unwrap_or("1 REGISTER"));
        if let Some(via) = req.headers.get("Via") {
            h.set("Via", via);
        }
        SipMessage::Response(Response {
            status,
            reason: if status == 200 {
                "OK".into()
            } else {
                "Trying".into()
            },
            headers: h,
            body: Vec::new(),
        })
    }

    #[tokio::test]
    async fn 收到_200_即结束事务() {
        let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let dst = platform.local_addr().unwrap();
        let device_addr = device.local_addr().unwrap();

        let mut rx = device.register("call-1");

        // 平台侧:收到请求后回 200(带同 Call-ID/CSeq/Via)。
        let mut prx = platform.register("call-1");
        let platform2 = platform.clone();
        tokio::spawn(async move {
            if let Some(inc) = prx.recv().await {
                if let SipMessage::Request(req) = inc.message {
                    let _ = platform2
                        .send_to(&response_for(&req, 200), device_addr)
                        .await;
                }
            }
        });

        let req = make_request("call-1", 1);
        let resp = client_transact(device.as_ref(), dst, &req, &mut rx, Timing::default())
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
            total_timeout: Duration::from_millis(200),
        };
        let r = client_transact(device.as_ref(), dst, &req, &mut rx, timing).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn branch_不匹配的响应被忽略() {
        let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let dst = platform.local_addr().unwrap();
        let device_addr = device.local_addr().unwrap();
        let mut rx = device.register("call-b");

        let mut prx = platform.register("call-b");
        let platform2 = platform.clone();
        tokio::spawn(async move {
            if let Some(inc) = prx.recv().await {
                if let SipMessage::Request(req) = inc.message {
                    // 先发一条 branch 被篡改的 200,再发正确的那条。
                    let mut wrong = match response_for(&req, 200) {
                        SipMessage::Response(r) => r,
                        _ => unreachable!(),
                    };
                    wrong
                        .headers
                        .set("Via", "SIP/2.0/UDP h;branch=z9hG4bK-other");
                    wrong.reason = "Wrong".into();
                    let _ = platform2
                        .send_to(&SipMessage::Response(wrong), device_addr)
                        .await;
                    let _ = platform2
                        .send_to(&response_for(&req, 200), device_addr)
                        .await;
                }
            }
        });

        let req = make_request("call-b", 1);
        let resp = client_transact(device.as_ref(), dst, &req, &mut rx, Timing::default())
            .await
            .unwrap();
        assert_eq!(resp.reason, "OK", "应跳过 branch 不匹配的响应");
    }

    #[tokio::test]
    async fn 临时响应不结束事务但被记录() {
        let device = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let dst = platform.local_addr().unwrap();
        let device_addr = device.local_addr().unwrap();
        let mut rx = device.register("call-p");

        let mut prx = platform.register("call-p");
        let platform2 = platform.clone();
        tokio::spawn(async move {
            if let Some(inc) = prx.recv().await {
                if let SipMessage::Request(req) = inc.message {
                    let _ = platform2
                        .send_to(&response_for(&req, 100), device_addr)
                        .await;
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    let _ = platform2
                        .send_to(&response_for(&req, 200), device_addr)
                        .await;
                }
            }
        });

        let req = make_request("call-p", 1);
        let resp = client_transact(
            device.as_ref(),
            dst,
            &req,
            &mut rx,
            Timing::from_t1(Duration::from_millis(30)),
        )
        .await
        .unwrap();
        assert_eq!(resp.status, 200);
    }

    #[test]
    fn 服务端事务对重传返回缓存响应() {
        let txns = ServerTransactions::new(Duration::from_secs(5), 64);
        let req = make_request("srv-1", 1);

        // 首次:新事务。
        let token = match txns.admit(&req) {
            Admit::Proceed(t) => t,
            _ => panic!("首次应为新事务"),
        };
        // 处理中的重传应被吸收,而不是再跑一遍业务逻辑。
        assert!(matches!(txns.admit(&req), Admit::Absorb));

        let resp = match response_for(&req, 200) {
            SipMessage::Response(r) => r,
            _ => unreachable!(),
        };
        txns.complete(&token, &resp);

        // 完成后的重传应拿到缓存响应。
        match txns.admit(&req) {
            Admit::Retransmit(cached) => assert_eq!(cached.status, 200),
            _ => panic!("重传应命中缓存响应"),
        }

        // 不同 branch 的同名请求是新事务。
        let other = make_request("srv-1", 2);
        assert!(matches!(txns.admit(&other), Admit::Proceed(_)));
    }

    #[test]
    fn 服务端事务在_linger_后过期() {
        let txns = ServerTransactions::new(Duration::from_millis(30), 64);
        let req = make_request("srv-2", 1);
        let token = match txns.admit(&req) {
            Admit::Proceed(t) => t,
            _ => panic!(),
        };
        let resp = match response_for(&req, 200) {
            SipMessage::Response(r) => r,
            _ => unreachable!(),
        };
        txns.complete(&token, &resp);
        std::thread::sleep(Duration::from_millis(60));
        // 过期后视为新事务(平台隔很久重用同一 branch 属异常,按新请求处理更安全)。
        assert!(matches!(txns.admit(&req), Admit::Proceed(_)));
    }

    #[test]
    fn ack_不占事务槽位() {
        let txns = ServerTransactions::new(Duration::from_secs(5), 64);
        let mut ack = make_request("srv-3", 1);
        ack.method = Method::Ack;
        assert!(matches!(txns.admit(&ack), Admit::Proceed(_)));
        assert!(matches!(txns.admit(&ack), Admit::Proceed(_)));
        assert!(txns.is_empty(), "ACK 不应在事务表里留条目");
    }

    #[test]
    fn abandon_让后续重传重新处理() {
        let txns = ServerTransactions::new(Duration::from_secs(5), 64);
        let req = make_request("srv-4", 1);
        let token = match txns.admit(&req) {
            Admit::Proceed(t) => t,
            _ => panic!(),
        };
        txns.abandon(&token);
        assert!(matches!(txns.admit(&req), Admit::Proceed(_)));
    }
}
