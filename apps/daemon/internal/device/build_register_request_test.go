package device

import (
	"strings"
	"testing"
)

// TestBuildRegisterRequest_UDP_NoTransportParam:
// UDP 传输时 Request-URI 与 Contact 头都不带 transport 参数,
// 与 M1/M2/M3 时代原有报文格式保持字节级一致 (向后兼容 WVP-Pro 现网)。
func TestBuildRegisterRequest_UDP_NoTransportParam(t *testing.T) {
	cfg := newTestConfig()
	cfg.Transport = "udp"
	session := newRawTestSession(&mockSipClient{}, cfg)

	req, err := session.buildRegisterRequest()
	if err != nil {
		t.Fatalf("buildRegisterRequest err: %v", err)
	}

	// Request-URI 不应含 transport 参数
	if got, ok := req.Recipient.UriParams.Get("transport"); ok {
		t.Errorf("UDP: Request-URI 不应带 transport 参数, got %q", got)
	}

	// Contact 头字符串不应含 transport
	rawReq := req.String()
	if strings.Contains(strings.ToLower(rawReq), "transport=") {
		t.Errorf("UDP: 完整报文不应含 transport 参数\nraw:\n%s", rawReq)
	}
}

// TestBuildRegisterRequest_TCP_HasTransportParam:
// TCP 传输时 Request-URI 与 Contact 头都必须带 ;transport=tcp,
// GB28181 平台 (WVP-Pro / LiveGBS) 靠这个参数识别设备端期望的传输协议,
// 平台侧路由响应回来时才会选正确的 socket 类型。
func TestBuildRegisterRequest_TCP_HasTransportParam(t *testing.T) {
	cfg := newTestConfig()
	cfg.Transport = "tcp"
	session := newRawTestSession(&mockSipClient{}, cfg)

	req, err := session.buildRegisterRequest()
	if err != nil {
		t.Fatalf("buildRegisterRequest err: %v", err)
	}

	// Request-URI 必须带 transport=tcp
	got, ok := req.Recipient.UriParams.Get("transport")
	if !ok {
		t.Fatalf("TCP: Request-URI 应带 transport=tcp 参数, 实际 params=%v",
			req.Recipient.UriParams)
	}
	if got != "tcp" {
		t.Errorf("TCP: Request-URI transport 参数 want tcp, got %q", got)
	}

	// Contact 头也必须带 transport=tcp (让平台响应走同一 socket)
	contact := req.Contact()
	if contact == nil {
		t.Fatal("Contact 头未生成")
	}
	tp, ok := contact.Address.UriParams.Get("transport")
	if !ok {
		t.Fatalf("TCP: Contact URI 应带 transport=tcp, 实际 params=%v",
			contact.Address.UriParams)
	}
	if tp != "tcp" {
		t.Errorf("TCP: Contact transport 参数 want tcp, got %q", tp)
	}

	// 完整报文含 "transport=tcp"
	rawReq := req.String()
	if !strings.Contains(strings.ToLower(rawReq), "transport=tcp") {
		t.Errorf("TCP: 完整报文应含 transport=tcp\nraw:\n%s", rawReq)
	}
}

// TestBuildRegisterRequest_TCP_DestinationUnchanged:
// TCP 场景下 SetDestination 仍然指向 ServerHost:ServerPort,
// 让 sipgo 内部按此地址建 TCP 连接 (URI host 是 SIP 域名/域 ID 不能 DNS)。
func TestBuildRegisterRequest_TCP_DestinationUnchanged(t *testing.T) {
	cfg := newTestConfig()
	cfg.Transport = "tcp"
	cfg.ServerHost = "10.0.0.5"
	cfg.ServerPort = 5060
	session := newRawTestSession(&mockSipClient{}, cfg)

	req, err := session.buildRegisterRequest()
	if err != nil {
		t.Fatalf("buildRegisterRequest err: %v", err)
	}

	// sipgo Destination() 返回 SetDestination 指定的值
	if got := req.Destination(); got != "10.0.0.5:5060" {
		t.Errorf("Destination want 10.0.0.5:5060, got %q", got)
	}
}
