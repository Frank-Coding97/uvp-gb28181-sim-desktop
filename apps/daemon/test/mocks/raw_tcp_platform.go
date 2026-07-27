// raw_tcp_platform.go 是 raw_udp_platform.go 的 TCP 版本。
//
// 独立于 sipgo:直接 net.Listen("tcp"),按 SIP RFC 3261 §7.5 严格分帧
// (headers CRLF-CRLF 结束 + Content-Length 头指定 body 长度),
// 手写 401/200 响应字节。反同源污染:测试 daemon 生成的字节确实合规,
// 而不是"我们的解析器能解析我们自己的生成器"。
//
// 与 UDP 版共享 MockOpts / ReceivedRequest / 报文构造 / Digest 校验逻辑。
package mocks

import (
	"bufio"
	"bytes"
	"errors"
	"io"
	"net"
	"strconv"
	"strings"
	"sync"
	"time"
)

// RawTCPMockPlatform 是独立 TCP mock 平台。
type RawTCPMockPlatform struct {
	opts     MockOpts
	listener net.Listener

	mu       sync.Mutex
	received []ReceivedRequest
	nonce    string
	stopCh   chan struct{}
	stopped  bool
	wg       sync.WaitGroup

	// 已接受的连接,Stop 时统一 Close 让 goroutine 退出
	connMu sync.Mutex
	conns  []net.Conn
}

// StartRawTCPMockPlatform 启动 mock 平台监听随机 TCP 端口。
func StartRawTCPMockPlatform(opts MockOpts) (*RawTCPMockPlatform, error) {
	if opts.AuthAlgo == "" {
		opts.AuthAlgo = "MD5"
	}

	l, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return nil, err
	}

	m := &RawTCPMockPlatform{
		opts:     opts,
		listener: l,
		stopCh:   make(chan struct{}),
		nonce:    generateNonce(),
	}
	m.wg.Add(1)
	go m.acceptLoop()
	return m, nil
}

// Addr 返回 mock 平台监听地址 (TCP)。
func (m *RawTCPMockPlatform) Addr() *net.TCPAddr {
	return m.listener.Addr().(*net.TCPAddr)
}

// Host 返回 IP 字符串。
func (m *RawTCPMockPlatform) Host() string {
	return m.Addr().IP.String()
}

// Port 返回端口。
func (m *RawTCPMockPlatform) Port() int {
	return m.Addr().Port
}

// Stop 关闭 mock 平台。
func (m *RawTCPMockPlatform) Stop() {
	m.mu.Lock()
	if m.stopped {
		m.mu.Unlock()
		return
	}
	m.stopped = true
	close(m.stopCh)
	m.mu.Unlock()

	_ = m.listener.Close()

	// 关掉所有活动连接,让 handleConn 的 ReadRequest 立刻返回 err 退出
	m.connMu.Lock()
	for _, c := range m.conns {
		_ = c.Close()
	}
	m.connMu.Unlock()

	m.wg.Wait()
}

// Received 返回累计收到的请求副本。
func (m *RawTCPMockPlatform) Received() []ReceivedRequest {
	m.mu.Lock()
	defer m.mu.Unlock()
	out := make([]ReceivedRequest, len(m.received))
	copy(out, m.received)
	return out
}

// acceptLoop 主 Accept 循环。
func (m *RawTCPMockPlatform) acceptLoop() {
	defer m.wg.Done()
	for {
		conn, err := m.listener.Accept()
		if err != nil {
			select {
			case <-m.stopCh:
				return
			default:
			}
			// 非 stop 场景的 accept 错误 (通常是 listener 已关),退出
			return
		}
		m.connMu.Lock()
		m.conns = append(m.conns, conn)
		m.connMu.Unlock()

		m.wg.Add(1)
		go m.handleConn(conn)
	}
}

// handleConn 处理单条 TCP 连接:循环读 SIP 请求 → 分帧 → 响应。
//
// SIP over TCP 分帧 (RFC 3261 §7.5):
//   - headers 以 CRLF-CRLF 结束
//   - body 长度由 Content-Length 头指定 (REGISTER 通常 0)
//   - 一条连接可承载多个请求/响应 (persistent connection)
func (m *RawTCPMockPlatform) handleConn(conn net.Conn) {
	defer m.wg.Done()
	defer conn.Close()

	// 连接唯一标识 (用 RemoteAddr 作 ID,测试断言用)
	connID := conn.RemoteAddr().String()

	reader := bufio.NewReader(conn)

	for {
		select {
		case <-m.stopCh:
			return
		default:
		}

		// 短超时确保 Stop 后能及时退出
		_ = conn.SetReadDeadline(time.Now().Add(500 * time.Millisecond))

		raw, err := readSIPMessage(reader)
		if err != nil {
			if errors.Is(err, io.EOF) {
				return
			}
			// 超时看 stopCh,再决定退不退
			var nerr net.Error
			if errors.As(err, &nerr) && nerr.Timeout() {
				continue
			}
			return
		}

		req := parseRequestGeneric(raw)
		req.ConnectionID = connID // 标记连接 ID

		m.mu.Lock()
		m.received = append(m.received, req)
		m.mu.Unlock()

		// 只处理 REGISTER;其他方法一律 200 OK
		if req.Method != "REGISTER" {
			_, _ = conn.Write(buildSimple200OK(req))
			continue
		}

		if !req.HasAuthorization {
			_, _ = conn.Write(m.build401TCP(req))
			continue
		}

		if !m.validateAuthTCP(req) {
			_, _ = conn.Write(m.buildStatusTCP(req, 403, "Forbidden"))
			continue
		}

		if m.opts.ForceReject != 0 {
			_, _ = conn.Write(m.buildStatusTCP(req, m.opts.ForceReject, "Forbidden"))
			continue
		}

		_, _ = conn.Write(m.build200OKTCP(req))
	}
}

// readSIPMessage 严格按 SIP over TCP 分帧规则读取一条完整消息。
//
// 步骤:
//  1. 逐行读 headers 直到空行 (CRLF)
//  2. 从 headers 中找 Content-Length
//  3. 读 Content-Length 字节的 body
//  4. 拼装原始字节返回
func readSIPMessage(reader *bufio.Reader) ([]byte, error) {
	var headerBuf bytes.Buffer
	contentLength := 0

	for {
		line, err := reader.ReadString('\n')
		if err != nil {
			return nil, err
		}
		headerBuf.WriteString(line)

		// 空行 (仅 \r\n) 表示 headers 结束
		trimmed := strings.TrimRight(line, "\r\n")
		if trimmed == "" {
			break
		}

		// 提取 Content-Length (case-insensitive)
		low := strings.ToLower(trimmed)
		if strings.HasPrefix(low, "content-length:") ||
			strings.HasPrefix(low, "l:") { // SIP 短头
			var valStr string
			if strings.HasPrefix(low, "content-length:") {
				valStr = strings.TrimSpace(trimmed[len("Content-Length:"):])
			} else {
				valStr = strings.TrimSpace(trimmed[len("l:"):])
			}
			n, err := strconv.Atoi(valStr)
			if err == nil && n >= 0 {
				contentLength = n
			}
		}
	}

	if contentLength > 0 {
		body := make([]byte, contentLength)
		if _, err := io.ReadFull(reader, body); err != nil {
			return nil, err
		}
		headerBuf.Write(body)
	}

	return headerBuf.Bytes(), nil
}

// parseRequestGeneric 复用 UDP 版的 regexp 解析逻辑。
func parseRequestGeneric(raw []byte) ReceivedRequest {
	// 与 RawMockPlatform.parseRequest 逻辑一致,抽成独立函数便于 TCP 复用
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
				n, _ := strconv.Atoi(parts[0])
				req.CSeqNum = n
				req.CSeqMethod = parts[1]
			}
		case strings.HasPrefix(low, "from:"):
			if m := reFromTag.FindStringSubmatch(line); len(m) > 1 {
				req.FromTag = m[1]
			}
		case strings.HasPrefix(low, "authorization:"):
			req.HasAuthorization = true
			parseAuthLine(line, &req)
		}
	}
	return req
}

// parseAuthLine 从 Authorization 头一行解析各子字段。
// 与 RawMockPlatform.parseAuthHeader 逻辑一致,提为包内函数 (TCP 也用)。
func parseAuthLine(line string, req *ReceivedRequest) {
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

// validateAuthTCP 与 UDP 版一致的最小校验逻辑。
func (m *RawTCPMockPlatform) validateAuthTCP(req ReceivedRequest) bool {
	if req.AuthNonce != m.nonce {
		return false
	}
	return req.AuthResponse != ""
}

// build401TCP 构造 401 挑战响应。共用 UDP 版的头拼装 (buildResponseHeaders)。
func (m *RawTCPMockPlatform) build401TCP(req ReceivedRequest) []byte {
	auth := `Digest realm="` + m.opts.Realm + `", nonce="` + m.nonce + `", algorithm=` + m.opts.AuthAlgo
	if m.opts.Qop != "" {
		auth += `, qop="` + m.opts.Qop + `"`
	}
	if m.opts.Opaque != "" {
		auth += `, opaque="` + m.opts.Opaque + `"`
	}
	return []byte(buildResponseHeaders(req, 401, "Unauthorized",
		"WWW-Authenticate: "+auth+"\r\n"))
}

// build200OKTCP 构造 200 OK。
func (m *RawTCPMockPlatform) build200OKTCP(req ReceivedRequest) []byte {
	var extra strings.Builder
	if m.opts.ExpiresReply > 0 {
		extra.WriteString("Expires: ")
		extra.WriteString(strconv.Itoa(m.opts.ExpiresReply))
		extra.WriteString("\r\n")
	}
	if m.opts.ContactExpiresParam > 0 {
		extra.WriteString("Contact: <sip:mock@127.0.0.1:")
		extra.WriteString(strconv.Itoa(m.Port()))
		extra.WriteString(">;expires=")
		extra.WriteString(strconv.Itoa(m.opts.ContactExpiresParam))
		extra.WriteString("\r\n")
	}
	extra.WriteString("Server: raw-mock-tcp/0.1\r\nDate: ")
	extra.WriteString(time.Now().UTC().Format(time.RFC1123))
	extra.WriteString("\r\n")
	return []byte(buildResponseHeaders(req, 200, "OK", extra.String()))
}

// buildStatusTCP 通用状态码响应。
func (m *RawTCPMockPlatform) buildStatusTCP(req ReceivedRequest, code int, reason string) []byte {
	return []byte(buildResponseHeaders(req, code, reason, ""))
}
