package sip

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"time"

	"github.com/emiago/sipgo"
	"github.com/emiago/sipgo/sip"
	"github.com/icholy/digest"
)

// ClientConfig 是 Client 初始化参数。
type ClientConfig struct {
	// Username 是 Digest 挑战应答用户名 (通常等于设备 ID)。
	Username string

	// Password 是 Digest 密码。
	Password string

	// Tracer 可选:非 nil 时,Client 在收发 SIP 报文前后调 Tracer.Emit,
	// 让上层 (M2 stdio IPC) 把报文事件推到前端 SIP Trace 面板。
	Tracer TracePublisher
}

// Client 封装 sipgo.Client,提供带 Digest 挑战应答的高层 Do 方法。
type Client struct {
	transport *Transport
	client    *sipgo.Client
	username  string
	password  string
	tracer    TracePublisher
}

// NewClient 基于 Transport 创建 sipgo Client。
//
// 关键选项(GB28181 场景必备):
//   - WithClientHostname: Contact/Via 头写本机对外可达 IP,平台按此地址回包。
//     默认走 sipgo 自己的 DNS/接口检测,GB28181 场景下经常猜错(比如猜到 lo)。
//   - WithClientPort: 固定本机 UDP 端口(通常 5060),让平台把响应发回可预期端口。
//     不设的话 sipgo 用临时端口,某些 GB28181 平台仍把响应发到 5060 就丢包。
func NewClient(transport *Transport, cfg ClientConfig) *Client {
	opts := []sipgo.ClientOption{}
	if h := transport.LocalHost(); h != "" {
		opts = append(opts, sipgo.WithClientHostname(h))
	}
	if p := transport.LocalPort(); p > 0 {
		opts = append(opts, sipgo.WithClientPort(p))
	}
	client, err := sipgo.NewClient(transport.UserAgent(), opts...)
	if err != nil {
		// sipgo NewClient 只在 UA 为 nil 时失败,transport 已经保证 UA 非 nil,
		// 这里用 panic 而非返错误 (库前置条件违反,不是运行时错误)。
		panic(fmt.Sprintf("sipgo NewClient: %v", err))
	}
	return &Client{
		transport: transport,
		client:    client,
		username:  cfg.Username,
		password:  cfg.Password,
		tracer:    cfg.Tracer,
	}
}

// emitTrace 若 tracer 已配置,推一条 TraceEvent。补齐 Summary / CSeq / Call-ID / Timestamp。
// tracer 为 nil 时零成本(inline check),M1 路径不受影响。
func (c *Client) emitTrace(dir string, method string, statusCode int, peer string, raw []byte) {
	if c.tracer == nil {
		return
	}
	c.tracer.Emit(TraceEvent{
		Timestamp:  time.Now(),
		Direction:  dir,
		Method:     method,
		StatusCode: statusCode,
		CSeq:       ExtractCSeq(raw),
		CallID:     ExtractCallID(raw),
		Peer:       peer,
		Summary:    SummarizeSIPBytes(raw),
		RawBytes:   raw,
		RawText:    string(raw),
	})
}

// Sentinel errors。上层用 errors.Is 判定,不字符串匹配。
var (
	ErrTimeout           = errors.New("sip: request timeout")
	ErrPlatformRejected  = errors.New("sip: platform rejected")
	ErrProtocol          = errors.New("sip: protocol error")
	ErrOther             = errors.New("sip: other")
)

// Do 发送请求并自动处理 401 挑战应答。
//
// 有 tracer 时在 outbound 发送前 / inbound 收到后各推一条 TraceEvent,
// 让前端 SIP Trace 面板看到完整的 4 报文 (>>> REGISTER / <<< 401 / >>> AUTH / <<< 200)。
func (c *Client) Do(ctx context.Context, req *sip.Request) (*sip.Response, error) {
	rawReq := []byte(req.String())
	peer := req.Destination()
	if peer == "" {
		peer = req.Recipient.HostPort()
	}
	slog.Debug("[trace] >>> outbound",
		"method", req.Method, "uri", req.Recipient.String(),
		"raw", string(rawReq))
	c.emitTrace(DirectionOutbound, req.Method.String(), 0, peer, rawReq)

	resp, err := c.client.Do(ctx, req)
	if err != nil {
		if errors.Is(err, context.DeadlineExceeded) || errors.Is(err, context.Canceled) {
			return nil, fmt.Errorf("%w: %v", ErrTimeout, err)
		}
		return nil, fmt.Errorf("%w: %v", ErrOther, err)
	}

	rawResp := []byte(resp.String())
	slog.Debug("[trace] <<< inbound",
		"status", resp.StatusCode, "reason", resp.Reason,
		"raw", string(rawResp))
	// inbound 的 method 沿用请求 method(方便前端配对),status_code 是响应码
	c.emitTrace(DirectionInbound, req.Method.String(), int(resp.StatusCode), peer, rawResp)

	if resp.StatusCode == 401 {
		return c.doDigestChallenge(ctx, req, resp)
	}

	if resp.StatusCode >= 400 {
		return resp, fmt.Errorf("%w: %d %s", ErrPlatformRejected, resp.StatusCode, resp.Reason)
	}

	return resp, nil
}

// doDigestChallenge 处理 401 挑战:解析 WWW-Authenticate → 构造 Authorization → 重发。
//
// **不用 sipgo 内建 DoDigestAuth**,原因:
//
// M1 真机联调 WVP-Pro v2.7.4 发现: sipgo 内部把 Options.URI 设成 req.Recipient.Addr(),
// 格式是 `<user>@<host>[:port]` **不带 "sip:" 前缀** (虽然 Uri.Addr() 方法会拼 scheme,
// 但传给 DoDigestAuth 前的某个环节丢失了 —— sipgo v1.4.0 源码待确认具体位置)。
// GB28181 平台期望 Authorization 头里 `uri="sip:..."` 带前缀对齐 Request-URI,
// 否则 Digest response hash 校验失败,平台静默丢包 (WVP 不返 403,直接超时)。
// M1 commit ea1796b (2026-07-27) 定位到该问题。
//
// 修法: 手工用 icholy/digest 库,Options.URI 显式取 req.Recipient.Addr() (已含 sip:)。
// 未来若 sipgo 修复该 quirk,可切回 DoDigestAuth (需回归 WVP-Pro + LiveGBS 验证)。
func (c *Client) doDigestChallenge(
	ctx context.Context,
	req *sip.Request,
	challenge *sip.Response,
) (*sip.Response, error) {
	wwwAuth := challenge.GetHeader("WWW-Authenticate")
	if wwwAuth == nil {
		return nil, fmt.Errorf("%w: 401 without WWW-Authenticate header", ErrProtocol)
	}
	chal, err := digest.ParseChallenge(wwwAuth.Value())
	if err != nil {
		return nil, fmt.Errorf("%w: parse challenge %q: %v", ErrProtocol, wwwAuth.Value(), err)
	}
	// sipgo 里有句 "Fix lower case algorithm although not supported by rfc",照抄
	chal.Algorithm = sip.ASCIIToUpper(chal.Algorithm)

	// Recipient.Addr() 已经含 "sip:" 前缀 (Uri.Addr() 内部拼 scheme)
	digestURI := req.Recipient.Addr()

	cred, err := digest.Digest(chal, digest.Options{
		Method:   req.Method.String(),
		URI:      digestURI,
		Username: c.username,
		Password: c.password,
	})
	if err != nil {
		return nil, fmt.Errorf("%w: compute digest: %v", ErrProtocol, err)
	}

	// 把 Authorization 头装到原 req 上;CSeq 递增 + Via 重建交给 sipgo TransactionRequest
	req.RemoveHeader("Authorization")
	req.AppendHeader(sip.NewHeader("Authorization", cred.String()))

	// 手工 CSeq++ (对齐 sipgo digestTransactionRequest 的行为)
	if cseq := req.CSeq(); cseq != nil {
		cseq.SeqNo++
	}
	req.RemoveHeader("Via")

	rawRetry := []byte(req.String())
	peer := req.Destination()
	if peer == "" {
		peer = req.Recipient.HostPort()
	}
	slog.Debug("[trace] >>> outbound (digest retry)",
		"method", req.Method, "uri", req.Recipient.String(),
		"digest_uri", digestURI,
		"raw", string(rawRetry))
	c.emitTrace(DirectionOutbound, req.Method.String(), 0, peer, rawRetry)

	// 走标准 TransactionRequest,sipgo 会自动补 Via
	tx, err := c.client.TransactionRequest(ctx, req, sipgo.ClientRequestAddVia)
	if err != nil {
		if errors.Is(err, context.DeadlineExceeded) || errors.Is(err, context.Canceled) {
			return nil, fmt.Errorf("%w: %v (digest retry stage)", ErrTimeout, err)
		}
		return nil, fmt.Errorf("%w: transaction: %v", ErrOther, err)
	}
	defer tx.Terminate()

	for {
		select {
		case resp := <-tx.Responses():
			if resp.IsProvisional() {
				continue
			}
			rawResp := []byte(resp.String())
			slog.Debug("[trace] <<< inbound (digest retry)",
				"status", resp.StatusCode, "reason", resp.Reason,
				"raw", string(rawResp))
			c.emitTrace(DirectionInbound, req.Method.String(), int(resp.StatusCode), peer, rawResp)
			if resp.StatusCode >= 400 {
				return resp, fmt.Errorf("%w: %d %s (鉴权后)", ErrPlatformRejected, resp.StatusCode, resp.Reason)
			}
			return resp, nil
		case <-tx.Done():
			return nil, fmt.Errorf("%w: transaction done: %v", ErrOther, tx.Err())
		case <-ctx.Done():
			return nil, fmt.Errorf("%w: %v (digest retry stage)", ErrTimeout, ctx.Err())
		}
	}
}

// SipgoClient 暴露原始 sipgo.Client 给需要底层能力的调用者 (谨慎使用)。
func (c *Client) SipgoClient() *sipgo.Client {
	return c.client
}
