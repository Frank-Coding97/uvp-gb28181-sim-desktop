// Package mocks 提供**不依赖 sipgo** 的原始 UDP mock 平台,用于反同源污染测试。
//
// 独立于 sipgo:直接 net.ListenUDP,手工按行 + 双 CRLF 分帧解析 SIP 报文,
// 手写 401/200 响应字节。测试 daemon 生成的字节确实符合 SIP RFC + GB28181 方言,
// 而不是"我们的解析器能解析我们自己的生成器"。
//
// P2-4 fix: 解析/构造逻辑已提取到 raw_common.go,UDP/TCP 复用。
package mocks

import (
	"net"
	"strings"
	"sync"
	"time"
)

// MockOpts 是 mock 平台初始化选项。
type MockOpts struct {
	// Realm 是平台域,通常等于 10 位域 ID。
	Realm string

	// Password 是设备端应该配置的密码,用于校验 Authorization 头。
	Password string

	// AuthAlgo 是 Digest 算法:"MD5" 或 "MD5-sess"。默认 MD5。
	AuthAlgo string

	// Qop 是 Digest quality of protection:"" (RFC 2069 无 qop) 或 "auth"。
	Qop string

	// Opaque 是 401 挑战里的 opaque 值,平台可选返回,客户端应原样回带。
	Opaque string

	// ExpiresReply 是 200 OK 响应里 Expires 头的值 (0 表示不返回该头)。
	ExpiresReply int

	// ContactExpiresParam 是 200 OK Contact 头 expires 参数 (0 表示不设)。
	ContactExpiresParam int

	// ForceReject 若非 0,鉴权通过后仍返回该状态码 (测试 403 场景)。
	ForceReject int
}

// ReceivedRequest 记录一次收到的 SIP 请求关键字段,供测试断言。
type ReceivedRequest struct {
	Method            string
	RequestURI        string
	CallID            string
	CSeqNum           int
	CSeqMethod        string
	FromTag           string
	HasAuthorization  bool
	AuthResponse      string // Authorization 头里的 response= 值
	AuthNonce         string
	AuthNonceCount    string
	AuthQop           string
	AuthOpaque        string
	RawBytes          []byte
	ConnectionID      string // TCP 场景:连接唯一标识 (UDP 场景留空)
}

// RawMockPlatform 是独立 UDP mock 平台。
type RawMockPlatform struct {
	opts    MockOpts
	conn    *net.UDPConn
	parser  *RequestParser
	builder *ResponseBuilder

	mu       sync.Mutex
	received []ReceivedRequest
	nonce    string
	stopCh   chan struct{}
	stopped  bool
	wg       sync.WaitGroup
}

// StartRawMockPlatform 启动 mock 平台监听随机 UDP 端口。
func StartRawMockPlatform(opts MockOpts) (*RawMockPlatform, error) {
	if opts.AuthAlgo == "" {
		opts.AuthAlgo = "MD5"
	}

	addr, err := net.ResolveUDPAddr("udp", "127.0.0.1:0")
	if err != nil {
		return nil, err
	}
	conn, err := net.ListenUDP("udp", addr)
	if err != nil {
		return nil, err
	}

	m := &RawMockPlatform{
		opts:    opts,
		conn:    conn,
		parser:  &RequestParser{},
		stopCh:  make(chan struct{}),
		nonce:   generateNonce(),
	}
	m.builder = &ResponseBuilder{
		Opts: opts,
		Port: m.Port(),
	}
	m.wg.Add(1)
	go m.serve()
	return m, nil
}

// Addr 返回 mock 平台监听地址。
func (m *RawMockPlatform) Addr() *net.UDPAddr {
	return m.conn.LocalAddr().(*net.UDPAddr)
}

// Host 返回 IP 字符串。
func (m *RawMockPlatform) Host() string {
	return m.Addr().IP.String()
}

// Port 返回端口。
func (m *RawMockPlatform) Port() int {
	return m.Addr().Port
}

// Stop 关闭 mock 平台。
func (m *RawMockPlatform) Stop() {
	m.mu.Lock()
	if m.stopped {
		m.mu.Unlock()
		return
	}
	m.stopped = true
	close(m.stopCh)
	m.mu.Unlock()

	_ = m.conn.Close()
	m.wg.Wait()
}

// Received 返回累计收到的请求副本。
func (m *RawMockPlatform) Received() []ReceivedRequest {
	m.mu.Lock()
	defer m.mu.Unlock()
	out := make([]ReceivedRequest, len(m.received))
	copy(out, m.received)
	return out
}

// serve 主收发循环。
func (m *RawMockPlatform) serve() {
	defer m.wg.Done()
	buf := make([]byte, 65535)
	for {
		select {
		case <-m.stopCh:
			return
		default:
		}

		_ = m.conn.SetReadDeadline(time.Now().Add(100 * time.Millisecond))
		n, addr, err := m.conn.ReadFromUDP(buf)
		if err != nil {
			// 超时或 conn 关闭
			if strings.Contains(err.Error(), "closed") {
				return
			}
			continue
		}
		raw := make([]byte, n)
		copy(raw, buf[:n])
		m.handle(raw, addr)
	}
}

// handle 处理一条 SIP 请求。
func (m *RawMockPlatform) handle(raw []byte, addr *net.UDPAddr) {
	req := m.parser.Parse(raw)

	m.mu.Lock()
	m.received = append(m.received, req)
	m.mu.Unlock()

	// 只处理 REGISTER (M1 场景);其他方法一律 200 OK
	if req.Method != "REGISTER" {
		m.sendResponse(addr, BuildSimple200OK(req))
		return
	}

	if !req.HasAuthorization {
		m.sendResponse(addr, m.builder.Build401(req, m.nonce))
		return
	}

	// 校验 Authorization
	if !ValidateAuthBasic(req, m.nonce) {
		m.sendResponse(addr, m.builder.BuildStatus(req, 403, "Forbidden"))
		return
	}

	if m.opts.ForceReject != 0 {
		m.sendResponse(addr, m.builder.BuildStatus(req, m.opts.ForceReject, "Forbidden"))
		return
	}

	m.sendResponse(addr, m.builder.Build200OK(req))
}

func (m *RawMockPlatform) sendResponse(addr *net.UDPAddr, data []byte) {
	_, _ = m.conn.WriteToUDP(data, addr)
}
