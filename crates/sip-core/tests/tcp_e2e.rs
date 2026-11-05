//! SIP over TCP 端到端验证:真连接、真分帧、共享连接多设备。
//!
//! 这三个用例分别锁定 TCP 传输的三个关键承诺:
//! 1. `tcp_注册闭环` —— 单设备事务能走通(协议正确性);
//! 2. `tcp_多设备共享单连接` —— 一条 TCP 连接承载多台设备,平台侧只 accept 一次
//!    (压测规模的前提);
//! 3. `tcp_响应沿入站连接回发` —— 平台从哪条连接发来请求,设备就在哪条回响应
//!    (RFC 3261 §18.2.2,多连接下才不会串线)。

use std::net::SocketAddr;
use std::time::Duration;

use sip_core::{
    client_transact, FrameDecoder, Headers, Method, Request, Response, SipMessage, SipTransport,
    Timing,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// 短事务预算,让失败用例快速暴露而不是等满 19.5 秒。
fn fast_timing() -> Timing {
    Timing {
        t1: Duration::from_millis(50),
        t2: Duration::from_millis(100),
        max_retransmits: 2,
    }
}

fn register_request(device: &str, call_id: &str, cseq: u32) -> Request {
    let mut headers = Headers::new();
    headers.append("Via", "SIP/2.0/TCP 127.0.0.1:5060;rport;branch=z9hG4bKtest");
    headers.append("From", format!("<sip:{device}@3402000000>;tag=t1"));
    headers.append("To", format!("<sip:{device}@3402000000>"));
    headers.set("Call-ID", call_id);
    headers.set("CSeq", format!("{cseq} REGISTER"));
    headers.set("Contact", format!("<sip:{device}@127.0.0.1:5060>"));
    headers.set("Max-Forwards", "70");
    Request {
        method: Method::Register,
        uri: "sip:34020000002000000001@3402000000".into(),
        headers,
        body: Vec::new(),
    }
}

/// 按 SIP 规则回显请求头构造 200 OK。
fn ok_response(request: &Request) -> Response {
    let mut headers = Headers::new();
    for name in ["Via", "From", "To", "Call-ID", "CSeq"] {
        for value in request.headers.get_all(name) {
            headers.append(name, value);
        }
    }
    Response {
        status: 200,
        reason: "OK".into(),
        headers,
        body: Vec::new(),
    }
}

/// 从连接上读出一条完整 SIP 消息。
async fn read_message(stream: &mut TcpStream, decoder: &mut FrameDecoder) -> SipMessage {
    let mut buf = [0u8; 8192];
    loop {
        if let Some(frame) = decoder.next_frame().expect("分帧失败") {
            return SipMessage::parse(&frame).expect("解析失败");
        }
        let n = stream.read(&mut buf).await.expect("读失败");
        assert!(n > 0, "连接在收到完整报文前被关闭");
        decoder.extend(&buf[..n]);
    }
}

#[tokio::test]
async fn tcp_注册闭环() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut decoder = FrameDecoder::new();
        let message = read_message(&mut stream, &mut decoder).await;
        let SipMessage::Request(request) = message else {
            panic!("平台侧应收到请求");
        };
        assert_eq!(request.method, Method::Register);
        let resp = SipMessage::Response(ok_response(&request));
        stream.write_all(&resp.to_bytes()).await.unwrap();
        // 保持连接,避免客户端在事务完成前看到 EOF。
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let transport = SipTransport::connect_tcp(&addr.to_string(), 1)
        .await
        .expect("TCP 连接应建立");
    assert!(transport.is_reliable(), "TCP 应被识别为可靠传输");

    let mut rx = transport.register("call-1");
    let request = register_request("34020000001320000001", "call-1", 1);
    let response = client_transact(&transport, addr, &request, &mut rx, fast_timing())
        .await
        .expect("注册事务应收到 200");
    assert_eq!(response.status, 200);

    server.await.unwrap();
}

#[tokio::test]
async fn tcp_多设备共享单连接() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        // 连接池设为 1:平台侧必须只 accept 到一条连接。
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut decoder = FrameDecoder::new();
        for _ in 0..2 {
            let message = read_message(&mut stream, &mut decoder).await;
            let SipMessage::Request(request) = message else {
                panic!("平台侧应收到请求");
            };
            let resp = SipMessage::Response(ok_response(&request));
            stream.write_all(&resp.to_bytes()).await.unwrap();
        }
        // 第二条连接的 accept 必须超时 —— 证明两设备共用了一条连接。
        let extra = tokio::time::timeout(Duration::from_millis(400), listener.accept()).await;
        assert!(extra.is_err(), "共享连接模型下不应出现第二条连接");
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let transport = SipTransport::connect_tcp(&addr.to_string(), 1)
        .await
        .expect("TCP 连接应建立");

    let mut rx_a = transport.register("call-a");
    let mut rx_b = transport.register("call-b");
    let req_a = register_request("34020000001320000001", "call-a", 1);
    let req_b = register_request("34020000001320000002", "call-b", 1);

    // 并发发起两笔事务:同一条连接上交错收发,按 Call-ID 各自路由回对应设备。
    let tp_a = transport.clone();
    let tp_b = transport.clone();
    let task_a = tokio::spawn(async move {
        client_transact(&tp_a, addr, &req_a, &mut rx_a, fast_timing()).await
    });
    let task_b = tokio::spawn(async move {
        client_transact(&tp_b, addr, &req_b, &mut rx_b, fast_timing()).await
    });

    let (a, b) = (task_a.await.unwrap(), task_b.await.unwrap());
    assert_eq!(a.expect("设备 A 应收到 200").status, 200);
    assert_eq!(b.expect("设备 B 应收到 200").status, 200);

    server.await.unwrap();
}

#[tokio::test]
async fn tcp_响应沿入站连接回发() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // 平台侧:接受两条连接(连接池设为 2),从第二条下发 OPTIONS。
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        let (mut second, _) = listener.accept().await.unwrap();

        let mut headers = Headers::new();
        headers.append("Via", "SIP/2.0/TCP 127.0.0.1:5060;branch=z9hG4bKopt");
        headers.append("From", "<sip:34020000002000000001@3402000000>;tag=p1");
        headers.append("To", "<sip:dev-opt@3402000000>");
        headers.set("Call-ID", "opt-call");
        headers.set("CSeq", "1 OPTIONS");
        let options = Request {
            method: Method::Options,
            uri: "sip:dev-opt@3402000000".into(),
            headers,
            body: Vec::new(),
        };
        second
            .write_all(&SipMessage::Request(options).to_bytes())
            .await
            .unwrap();

        // 响应必须出现在 second(入站连接),而不是 first。
        let mut decoder = FrameDecoder::new();
        let reply = read_message(&mut second, &mut decoder).await;
        match reply {
            SipMessage::Response(r) => assert_eq!(r.status, 200),
            _ => panic!("设备应回响应"),
        }

        // first 上不应出现任何报文。
        let mut stray = [0u8; 64];
        let leaked = tokio::time::timeout(Duration::from_millis(400), first.read(&mut stray)).await;
        match leaked {
            Err(_) => {}
            Ok(Ok(0)) => {}
            Ok(Ok(n)) => panic!("响应串到了非入站连接,收到 {n} 字节"),
            Ok(Err(e)) => panic!("读 first 失败: {e}"),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    });

    let transport = SipTransport::connect_tcp(&addr.to_string(), 2)
        .await
        .expect("TCP 连接池应建立");
    let mut inbound = transport.register_inbound("dev-opt");

    let incoming = tokio::time::timeout(Duration::from_secs(2), inbound.recv())
        .await
        .expect("应收到平台 OPTIONS")
        .expect("入站队列关闭");
    let SipMessage::Request(request) = &incoming.message else {
        panic!("应为请求");
    };
    assert_eq!(request.method, Method::Options);

    let reply = SipMessage::Response(ok_response(request));
    transport
        .send_to(&reply, incoming.from)
        .await
        .expect("回 200 应成功");

    server.await.unwrap();
}

#[tokio::test]
async fn tcp_连接失败快速报错() {
    // 指向一个已监听但不 accept 的地址仍会连上;这里用一个不可达端口验证快速失败。
    let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let started = std::time::Instant::now();
    let result = SipTransport::connect_tcp(&addr.to_string(), 1).await;
    assert!(result.is_err(), "不可达端口应返回错误");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "失败应在 3 秒左右返回,实际 {:?}",
        started.elapsed()
    );
}
