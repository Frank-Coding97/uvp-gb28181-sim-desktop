package sip

import (
	"testing"
)

// TestNewTransport_UDP 覆盖默认 UDP 传输初始化。
func TestNewTransport_UDP(t *testing.T) {
	tp, err := NewTransport(TransportConfig{Protocol: "udp"})
	if err != nil {
		t.Fatalf("NewTransport(udp) err: %v", err)
	}
	defer tp.Close()
	if tp.Protocol() != "udp" {
		t.Errorf("Protocol() want udp, got %q", tp.Protocol())
	}
	if tp.UserAgent() == nil {
		t.Error("UserAgent() should not be nil")
	}
}

// TestNewTransport_TCP 验证 M4 起 TCP 传输解锁 (原 M1 拒绝分支已删)。
//
// sipgo v1.4.0 内建 TCP 支持:UA 层不区分 UDP/TCP,Client 层 Do 会根据
// SetDestination + Request-URI 的 transport 参数自动选 socket 类型。
// Transport 结构体保存 protocol 供 client 层构造 Contact/URI 时使用。
func TestNewTransport_TCP(t *testing.T) {
	tp, err := NewTransport(TransportConfig{Protocol: "tcp"})
	if err != nil {
		t.Fatalf("NewTransport(tcp) err: %v (M4 应该解锁 TCP)", err)
	}
	defer tp.Close()
	if tp.Protocol() != "tcp" {
		t.Errorf("Protocol() want tcp, got %q", tp.Protocol())
	}
	if tp.UserAgent() == nil {
		t.Error("UserAgent() should not be nil")
	}
}

// TestNewTransport_DefaultUDP 空 Protocol 默认走 UDP。
func TestNewTransport_DefaultUDP(t *testing.T) {
	tp, err := NewTransport(TransportConfig{})
	if err != nil {
		t.Fatalf("NewTransport(default) err: %v", err)
	}
	defer tp.Close()
	if tp.Protocol() != "udp" {
		t.Errorf("empty protocol should default to udp, got %q", tp.Protocol())
	}
}

// TestNewTransport_CaseInsensitive 大小写不敏感。
func TestNewTransport_CaseInsensitive(t *testing.T) {
	tp, err := NewTransport(TransportConfig{Protocol: "TCP"})
	if err != nil {
		t.Fatalf("NewTransport(TCP) err: %v", err)
	}
	defer tp.Close()
	if tp.Protocol() != "tcp" {
		t.Errorf("Protocol() want tcp (lowered), got %q", tp.Protocol())
	}
}

// TestNewTransport_UnsupportedProtocol 非 udp/tcp 应报错。
func TestNewTransport_UnsupportedProtocol(t *testing.T) {
	_, err := NewTransport(TransportConfig{Protocol: "tls"})
	if err == nil {
		t.Fatal("NewTransport(tls) should fail, TLS 归后续 spec")
	}
}
