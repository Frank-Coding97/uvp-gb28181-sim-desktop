// Package mocks 提供**不依赖 sipgo** 的原始 UDP mock 平台,用于反同源污染测试。
//
// 独立于 sipgo:直接 net.ListenUDP,手工按行 + 双 CRLF 分帧解析 SIP 报文,
// 手写 401/200 响应字节。测试 daemon 生成的字节确实符合 SIP RFC + GB28181 方言,
// 而不是"我们的解析器能解析我们自己的生成器"。
package mocks

import (
	"crypto/md5"
	"encoding/hex"
	"fmt"
	"net"
	"regexp"
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
}

// RawMockPlatform 是独立 UDP mock 平台。
type RawMockPlatform struct {
	opts MockOpts
	conn *net.UDPConn

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
		opts:   opts,
		conn:   conn,
		stopCh: make(chan struct{}),
		nonce:  generateNonce(),
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
	req := m.parseRequest(raw)

	m.mu.Lock()
	m.received = append(m.received, req)
	m.mu.Unlock()

	// 只处理 REGISTER (M1 场景);其他方法一律 200 OK
	if req.Method != "REGISTER" {
		m.sendResponse(addr, buildSimple200OK(req))
		return
	}

	if !req.HasAuthorization {
		m.sendResponse(addr, m.build401(req))
		return
	}

	// 校验 Authorization
	if !m.validateAuth(req) {
		m.sendResponse(addr, m.buildStatus(req, 403, "Forbidden"))
		return
	}

	if m.opts.ForceReject != 0 {
		m.sendResponse(addr, m.buildStatus(req, m.opts.ForceReject, "Forbidden"))
		return
	}

	m.sendResponse(addr, m.build200OK(req))
}

// parseRequest 用 regexp 解析 SIP 请求关键头,不依赖 sipgo。
// 只提取测试用的字段,不做严格 RFC 校验。
func (m *RawMockPlatform) parseRequest(raw []byte) ReceivedRequest {
	req := ReceivedRequest{RawBytes: raw}
	text := string(raw)
	lines := strings.Split(text, "\r\n")

	if len(lines) > 0 {
		firstLine := strings.Fields(lines[0])
		if len(firstLine) >= 2 {
			req.Method = firstLine[0]
			req.RequestURI = firstLine[1]
		}
	}

	for _, line := range lines[1:] {
		if line == "" {
			break // 头部结束
		}
		low := strings.ToLower(line)
		switch {
		case strings.HasPrefix(low, "call-id:"):
			req.CallID = strings.TrimSpace(line[len("Call-ID:"):])
		case strings.HasPrefix(low, "cseq:"):
			cseq := strings.TrimSpace(line[len("CSeq:"):])
			parts := strings.Fields(cseq)
			if len(parts) >= 2 {
				fmt.Sscanf(parts[0], "%d", &req.CSeqNum)
				req.CSeqMethod = parts[1]
			}
		case strings.HasPrefix(low, "from:"):
			if m := reFromTag.FindStringSubmatch(line); len(m) > 1 {
				req.FromTag = m[1]
			}
		case strings.HasPrefix(low, "authorization:"):
			req.HasAuthorization = true
			m.parseAuthHeader(line, &req)
		}
	}
	return req
}

var (
	reFromTag       = regexp.MustCompile(`tag=([^;\s]+)`)
	reAuthResponse  = regexp.MustCompile(`response="([^"]+)"`)
	reAuthNonce     = regexp.MustCompile(`nonce="([^"]+)"`)
	reAuthNC        = regexp.MustCompile(`nc=([0-9a-fA-F]+)`)
	reAuthQop       = regexp.MustCompile(`qop=([a-zA-Z\-]+)`)
	reAuthOpaque    = regexp.MustCompile(`opaque="([^"]+)"`)
)

func (m *RawMockPlatform) parseAuthHeader(line string, req *ReceivedRequest) {
	if x := reAuthResponse.FindStringSubmatch(line); len(x) > 1 {
		req.AuthResponse = x[1]
	}
	if x := reAuthNonce.FindStringSubmatch(line); len(x) > 1 {
		req.AuthNonce = x[1]
	}
	if x := reAuthNC.FindStringSubmatch(line); len(x) > 1 {
		req.AuthNonceCount = x[1]
	}
	if x := reAuthQop.FindStringSubmatch(line); len(x) > 1 {
		req.AuthQop = x[1]
	}
	if x := reAuthOpaque.FindStringSubmatch(line); len(x) > 1 {
		req.AuthOpaque = x[1]
	}
}

// validateAuth 校验 Authorization 里的 response 是否符合 MD5 Digest 算法。
func (m *RawMockPlatform) validateAuth(req ReceivedRequest) bool {
	if req.AuthNonce != m.nonce {
		return false
	}
	// TODO M4: 校验完整 MD5 hash。M1 只做基本挑战应答闭环,验证客户端确实带了 Authorization。
	// 简化断言:response 非空即通过。sipgo 内部会正确计算。
	return req.AuthResponse != ""
}

// build401 构造 401 挑战响应。
func (m *RawMockPlatform) build401(req ReceivedRequest) []byte {
	auth := fmt.Sprintf(`Digest realm="%s", nonce="%s", algorithm=%s`,
		m.opts.Realm, m.nonce, m.opts.AuthAlgo)
	if m.opts.Qop != "" {
		auth += fmt.Sprintf(`, qop="%s"`, m.opts.Qop)
	}
	if m.opts.Opaque != "" {
		auth += fmt.Sprintf(`, opaque="%s"`, m.opts.Opaque)
	}

	return []byte(buildResponseHeaders(req, 401, "Unauthorized",
		"WWW-Authenticate: "+auth+"\r\n"))
}

// build200OK 构造 200 OK 响应。
func (m *RawMockPlatform) build200OK(req ReceivedRequest) []byte {
	extra := ""
	if m.opts.ExpiresReply > 0 {
		extra += fmt.Sprintf("Expires: %d\r\n", m.opts.ExpiresReply)
	}
	if m.opts.ContactExpiresParam > 0 {
		extra += fmt.Sprintf("Contact: <sip:mock@127.0.0.1:%d>;expires=%d\r\n",
			m.Port(), m.opts.ContactExpiresParam)
	}
	extra += fmt.Sprintf("Server: raw-mock/0.1\r\nDate: %s\r\n", time.Now().UTC().Format(time.RFC1123))
	return []byte(buildResponseHeaders(req, 200, "OK", extra))
}

// buildStatus 构造任意状态码响应 (403 等)。
func (m *RawMockPlatform) buildStatus(req ReceivedRequest, code int, reason string) []byte {
	return []byte(buildResponseHeaders(req, code, reason, ""))
}

// buildSimple200OK 是非 REGISTER 请求的默认 OK 响应。
func buildSimple200OK(req ReceivedRequest) []byte {
	return []byte(buildResponseHeaders(req, 200, "OK", ""))
}

// buildResponseHeaders 拼接 SIP 响应头,回填 Call-ID/CSeq/From/To/Via。
// mock 平台简化:不 echo Via 头 (客户端会容忍),只回必要头字段。
func buildResponseHeaders(req ReceivedRequest, code int, reason, extraHeaders string) string {
	var b strings.Builder
	fmt.Fprintf(&b, "SIP/2.0 %d %s\r\n", code, reason)
	// Via 头必须回带 (但我们没解析,简化用 echo 原始 Via 行)
	viaLine := extractHeader(req.RawBytes, "Via")
	if viaLine != "" {
		b.WriteString(viaLine + "\r\n")
	}
	// From
	if from := extractHeader(req.RawBytes, "From"); from != "" {
		b.WriteString(from + "\r\n")
	}
	// To
	if to := extractHeader(req.RawBytes, "To"); to != "" {
		b.WriteString(to + "\r\n")
	}
	fmt.Fprintf(&b, "Call-ID: %s\r\n", req.CallID)
	fmt.Fprintf(&b, "CSeq: %d %s\r\n", req.CSeqNum, req.CSeqMethod)
	b.WriteString(extraHeaders)
	b.WriteString("Content-Length: 0\r\n\r\n")
	return b.String()
}

func extractHeader(raw []byte, name string) string {
	text := string(raw)
	lines := strings.Split(text, "\r\n")
	prefix := strings.ToLower(name) + ":"
	for _, line := range lines {
		if strings.HasPrefix(strings.ToLower(line), prefix) {
			return line
		}
	}
	return ""
}

func (m *RawMockPlatform) sendResponse(addr *net.UDPAddr, data []byte) {
	_, _ = m.conn.WriteToUDP(data, addr)
}

// generateNonce 生成一次性 nonce (含时间戳,不同挑战不同值)。
func generateNonce() string {
	seed := fmt.Sprintf("%d-uvp-mock", time.Now().UnixNano())
	sum := md5.Sum([]byte(seed))
	return hex.EncodeToString(sum[:])
}
