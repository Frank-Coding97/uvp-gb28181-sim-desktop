// Package sip: MESSAGE 入站处理器 (v2 M5 目录查询三件套).
//
// v4 关键决策:
//   - S-H1: 未注册回 403 Forbidden (非 481)
//   - S-B2: 200 OK 由 sip.NewResponseFromRequest 自动加 To-tag
//   - P-H5: Content-Type MIME 大小写不敏感 + 参数剥离
//   - P-H4: 逐片等严格 200 OK (SendStatus 枚举)
//   - N-B5: 严格 statusCode == 200 才算成功, 1xx 继续等
//   - N-H3: Via 由本层生成 (sipgo v1.4.0 Client 不自动补 Via)
package sip

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"mime"
	"strings"
	"time"

	sipgo "github.com/emiago/sipgo"
	"github.com/emiago/sipgo/sip"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/pkg/manscdp"
)

const (
	// ResponseTimeout 每片 Response MESSAGE 等 200 OK 上限.
	ResponseTimeout = 5 * time.Second
	// ManscdpMIME 标准 MANSCDP Content-Type.
	ManscdpMIME = "application/manscdp+xml"
)

// SessionState 设备注册状态 (供 handler 门禁判断).
type SessionState int

const (
	StateDisconnected SessionState = iota
	StateRegistering
	StateRegistered
	StateInCall
	StateFailed
)

// Session 供 MessageHandler 引用的最小 session 接口.
// 实际由 pkg/device 或 internal/device 的 RegistrationSession 实现.
type Session interface {
	State() SessionState
	Context() context.Context
	DeviceID() string
	FromTag() string
	NextCSeq() uint32
	PlatformURI() sip.Uri
	DeviceURI() sip.Uri
	Transport() string
	LocalHost() string
	LocalPort() int
}

// QueryHandler 抽象 pkg/device.QueryHandler 供测试 mock.
type QueryHandler interface {
	HandleCatalogQuery(ctx context.Context, q *manscdp.Query) ([][]byte, error)
	HandleDeviceInfoQuery(ctx context.Context, q *manscdp.Query) ([]byte, error)
	HandleDeviceStatusQuery(ctx context.Context, q *manscdp.Query) ([]byte, error)
}

// SendStatus 事务发送结果 (R3-H3: 区分成功/超时/被拒).
type SendStatus int

const (
	SendStatusOK       SendStatus = iota // 平台严格 200 OK 确认
	SendStatusTimeout                    // 等 200 OK 超时
	SendStatusRejected                   // 收到非 200 最终响应
	SendStatusTxError                    // 事务层错误
	SendStatusCtxDone                    // ctx 取消
)

func (s SendStatus) String() string {
	switch s {
	case SendStatusOK:
		return "ok"
	case SendStatusTimeout:
		return "timeout"
	case SendStatusRejected:
		return "rejected"
	case SendStatusTxError:
		return "tx_error"
	case SendStatusCtxDone:
		return "ctx_done"
	default:
		return "unknown"
	}
}

// MessageHandler 入站 MESSAGE + 出站 Response 编排.
type MessageHandler struct {
	client       *sipgo.Client
	session      Session
	queryHandler QueryHandler
	log          *slog.Logger
}

// NewMessageHandler 创建 handler.
// client 为 nil 时 sendResponseMessage 返 SendStatusTxError (测试 mock 场景).
func NewMessageHandler(
	client *sipgo.Client,
	session Session,
	qh QueryHandler,
	log *slog.Logger,
) *MessageHandler {
	if log == nil {
		log = slog.Default()
	}
	return &MessageHandler{
		client:       client,
		session:      session,
		queryHandler: qh,
		log:          log,
	}
}

// OnMessage 入站 MESSAGE 处理 (注册到 sipgo.Server.OnMessage 的回调).
func (h *MessageHandler) OnMessage(req *sip.Request, tx sip.ServerTransaction) {
	// S-H1: 未注册回 403 Forbidden
	if h.session.State() != StateRegistered {
		h.log.Warn("MESSAGE rejected: not registered")
		resp := sip.NewResponseFromRequest(req, 403, "Not Registered", nil)
		_ = tx.Respond(resp)
		return
	}

	// S-B2: 200 OK, sip.NewResponseFromRequest 自动加 To-tag
	okResp := sip.NewResponseFromRequest(req, 200, "OK", nil)
	if err := tx.Respond(okResp); err != nil {
		h.log.Error("respond 200 failed", "err", err)
		return
	}

	// P-H5: Content-Type MIME 解析 (大小写不敏感 + 剥离参数)
	ctHeaders := req.GetHeaders("Content-Type")
	if len(ctHeaders) == 0 {
		h.log.Warn("MESSAGE missing Content-Type")
		return
	}
	mediaType, _, err := mime.ParseMediaType(ctHeaders[0].Value())
	if err != nil {
		h.log.Warn("parse Content-Type failed", "err", err)
		return
	}
	if !strings.EqualFold(mediaType, ManscdpMIME) {
		h.log.Warn("unsupported Content-Type", "type", mediaType)
		return
	}

	// 异步处理业务逻辑 (P-M2: 绑 session ctx + 30s 超时)
	go func() {
		ctx, cancel := context.WithTimeout(h.session.Context(), 30*time.Second)
		defer cancel()
		h.handleManscdpQuery(ctx, req)
	}()
}

func (h *MessageHandler) handleManscdpQuery(ctx context.Context, req *sip.Request) {
	// 1. 解析 Query (P-B1: CharsetReader 支持 GB18030)
	query, err := manscdp.ParseQuery(req.Body())
	if err != nil {
		h.log.Warn("parse manscdp query failed", "err", err)
		return
	}

	h.log.Info("manscdp query received",
		"cmd_type", query.CmdType,
		"sn", query.SN,
		"device_id", query.DeviceID,
	)

	// 2. S-M3: DeviceID 不匹配 → 静默丢弃
	if query.DeviceID != h.session.DeviceID() {
		h.log.Warn("query target mismatch",
			"expected", h.session.DeviceID(),
			"got", query.DeviceID,
		)
		return
	}

	// 3. 路由到 QueryHandler
	var bodies [][]byte
	switch query.CmdType {
	case "Catalog":
		bs, herr := h.queryHandler.HandleCatalogQuery(ctx, query)
		if herr != nil {
			h.log.Error("catalog handler failed", "err", herr)
			return
		}
		bodies = bs
	case "DeviceInfo":
		b, herr := h.queryHandler.HandleDeviceInfoQuery(ctx, query)
		if herr != nil {
			h.log.Error("deviceinfo handler failed", "err", herr)
			return
		}
		bodies = [][]byte{b}
	case "DeviceStatus":
		b, herr := h.queryHandler.HandleDeviceStatusQuery(ctx, query)
		if herr != nil {
			h.log.Error("devicestatus handler failed", "err", herr)
			return
		}
		bodies = [][]byte{b}
	default:
		h.log.Warn("unsupported CmdType", "cmd_type", query.CmdType)
		return
	}

	// 4. 逐片发送 Response MESSAGE (P-H4 + R3-H3)
	for i, body := range bodies {
		status, err := h.sendResponseMessage(ctx, body)
		if err != nil {
			h.log.Error("send response failed",
				"chunk", i+1, "total", len(bodies),
				"status", status.String(), "err", err)
			continue // best-effort: 单片失败继续
		}
		h.log.Debug("response chunk sent",
			"chunk", i+1, "total", len(bodies),
			"status", status.String())
	}
}

// sendResponseMessage 构造并发送单条 Response MESSAGE, 等平台 200 OK.
// 返回 SendStatus 区分成功/超时/被拒.
func (h *MessageHandler) sendResponseMessage(ctx context.Context, body []byte) (SendStatus, error) {
	if h.client == nil {
		return SendStatusTxError, errors.New("sipgo client is nil (test mode?)")
	}
	req := h.buildResponseRequest(body)

	tx, err := h.client.TransactionRequest(ctx, req)
	if err != nil {
		return SendStatusTxError, err
	}
	defer tx.Terminate()

	// N-B5: 严格 200 判定, 1xx 继续等, 非 200 记 warn
	for {
		select {
		case resp := <-tx.Responses():
			if resp.StatusCode < 200 {
				continue // 1xx 临时响应
			}
			if resp.StatusCode == 200 {
				return SendStatusOK, nil
			}
			h.log.Warn("response chunk rejected by platform",
				"status", resp.StatusCode, "reason", resp.Reason)
			return SendStatusRejected, nil
		case <-tx.Done():
			if err := tx.Err(); err != nil {
				return SendStatusTxError, err
			}
			return SendStatusTxError, fmt.Errorf("transaction terminated without response")
		case <-ctx.Done():
			return SendStatusCtxDone, ctx.Err()
		case <-time.After(ResponseTimeout):
			return SendStatusTimeout, nil
		}
	}
}

// buildResponseRequest 构造 Response MESSAGE 请求.
// N-H3: Via 由本层生成 (sipgo v1.4.0 Client.TransactionRequest 不自动补 Via).
func (h *MessageHandler) buildResponseRequest(body []byte) *sip.Request {
	platformURI := h.session.PlatformURI()
	deviceURI := h.session.DeviceURI()

	req := sip.NewRequest(sip.MESSAGE, platformURI)

	// From: 设备 (复用 fromTag)
	fromHeader := &sip.FromHeader{
		Address: deviceURI,
		Params:  sip.HeaderParams{},
	}
	fromHeader.Params.Add("tag", h.session.FromTag())
	req.AppendHeader(fromHeader)

	// To: 平台 (对话外, 无 tag)
	toHeader := &sip.ToHeader{
		Address: platformURI,
		Params:  sip.HeaderParams{},
	}
	req.AppendHeader(toHeader)

	// Call-ID: 新生成 (sipgo v1.4.0: CallIDHeader 是 type xxx string)
	callID := sip.CallIDHeader(sip.GenerateTagN(24))
	req.AppendHeader(&callID)

	// CSeq: session 全局递增
	cseqHeader := &sip.CSeqHeader{
		SeqNo:      h.session.NextCSeq(),
		MethodName: sip.MESSAGE,
	}
	req.AppendHeader(cseqHeader)

	// Via: 本层生成 (N-H3)
	viaHeader := &sip.ViaHeader{
		ProtocolName:    "SIP",
		ProtocolVersion: "2.0",
		Transport:       strings.ToUpper(h.session.Transport()),
		Host:            h.session.LocalHost(),
		Port:            h.session.LocalPort(),
		Params:          sip.HeaderParams{},
	}
	viaHeader.Params.Add("branch", sip.GenerateBranch())
	viaHeader.Params.Add("rport", "")
	req.AppendHeader(viaHeader)

	// Max-Forwards
	maxFwd := sip.MaxForwardsHeader(70)
	req.AppendHeader(&maxFwd)

	// Content-Type
	ct := sip.ContentTypeHeader(ManscdpMIME)
	req.AppendHeader(&ct)

	// Body (sipgo 自动填 Content-Length)
	req.SetBody(body)

	return req
}
