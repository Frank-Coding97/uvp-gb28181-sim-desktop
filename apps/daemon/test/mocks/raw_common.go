// raw_common.go 提取 UDP/TCP mock 平台的共享解析与构造逻辑。
//
// P2-4 fix: TCP mock 复制了 UDP mock 80% 的代码(parseRequest / build401 / build200OK),
// 只有 socket 层不同。抽出共享逻辑避免重复,降低维护成本。
package mocks

import (
	"crypto/md5"
	"encoding/hex"
	"fmt"
	"regexp"
	"strconv"
	"strings"
	"time"
)

// RequestParser 解析 SIP 请求关键字段(独立于 sipgo)。
type RequestParser struct{}

// Parse 用 regexp 解析 SIP 请求,提取测试用字段(不做严格 RFC 校验)。
func (p *RequestParser) Parse(raw []byte) ReceivedRequest {
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

var (
	reFromTag      = regexp.MustCompile(`tag=([^;\s]+)`)
	reAuthResponse = regexp.MustCompile(`response="([^"]+)"`)
	reAuthNonce    = regexp.MustCompile(`nonce="([^"]+)"`)
	reAuthNC       = regexp.MustCompile(`nc=([0-9a-fA-F]+)`)
	reAuthQop      = regexp.MustCompile(`qop=([a-zA-Z\-]+)`)
	reAuthOpaque   = regexp.MustCompile(`opaque="([^"]+)"`)
)

// parseAuthLine 从 Authorization 头一行解析各子字段。
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

// ResponseBuilder 构造 SIP 响应(401 / 200 OK / 任意状态码)。
type ResponseBuilder struct {
	Opts MockOpts
	Port int // 平台监听端口(用于 Contact 头)
}

// Build401 构造 401 Unauthorized 挑战响应。
func (b *ResponseBuilder) Build401(req ReceivedRequest, nonce string) []byte {
	auth := fmt.Sprintf(`Digest realm="%s", nonce="%s", algorithm=%s`,
		b.Opts.Realm, nonce, b.Opts.AuthAlgo)
	if b.Opts.Qop != "" {
		auth += fmt.Sprintf(`, qop="%s"`, b.Opts.Qop)
	}
	if b.Opts.Opaque != "" {
		auth += fmt.Sprintf(`, opaque="%s"`, b.Opts.Opaque)
	}
	return []byte(buildResponseHeaders(req, 401, "Unauthorized",
		"WWW-Authenticate: "+auth+"\r\n"))
}

// Build200OK 构造 200 OK 响应。
func (b *ResponseBuilder) Build200OK(req ReceivedRequest) []byte {
	extra := ""
	if b.Opts.ExpiresReply > 0 {
		extra += fmt.Sprintf("Expires: %d\r\n", b.Opts.ExpiresReply)
	}
	if b.Opts.ContactExpiresParam > 0 {
		extra += fmt.Sprintf("Contact: <sip:mock@127.0.0.1:%d>;expires=%d\r\n",
			b.Port, b.Opts.ContactExpiresParam)
	}
	extra += fmt.Sprintf("Server: raw-mock-common/0.1\r\nDate: %s\r\n",
		time.Now().UTC().Format(time.RFC1123))
	return []byte(buildResponseHeaders(req, 200, "OK", extra))
}

// BuildStatus 构造任意状态码响应(403 / 其他)。
func (b *ResponseBuilder) BuildStatus(req ReceivedRequest, code int, reason string) []byte {
	return []byte(buildResponseHeaders(req, code, reason, ""))
}

// BuildSimple200OK 是非 REGISTER 请求的默认 OK 响应。
func BuildSimple200OK(req ReceivedRequest) []byte {
	return []byte(buildResponseHeaders(req, 200, "OK", ""))
}

// buildResponseHeaders 拼接 SIP 响应头,回填 Call-ID/CSeq/From/To/Via。
// mock 平台简化:不 echo Via 头(客户端会容忍),只回必要头字段。
func buildResponseHeaders(req ReceivedRequest, code int, reason, extraHeaders string) string {
	var b strings.Builder
	fmt.Fprintf(&b, "SIP/2.0 %d %s\r\n", code, reason)
	// Via 头必须回带(但我们没解析,简化用 echo 原始 Via 行)
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

// generateNonce 生成一次性 nonce(含时间戳,不同挑战不同值)。
func generateNonce() string {
	seed := fmt.Sprintf("%d-uvp-mock", time.Now().UnixNano())
	sum := md5.Sum([]byte(seed))
	return hex.EncodeToString(sum[:])
}

// ValidateAuthBasic 最小校验 Authorization(只检查 nonce 与 response 非空)。
func ValidateAuthBasic(req ReceivedRequest, expectedNonce string) bool {
	if req.AuthNonce != expectedNonce {
		return false
	}
	return req.AuthResponse != ""
}
