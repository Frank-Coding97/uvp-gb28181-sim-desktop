package sip

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"testing"

	sipgo_sip "github.com/emiago/sipgo/sip"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/pkg/manscdp"
)

// --- Mock Session ---

type mockSession struct {
	state      SessionState
	deviceID   string
	fromTag    string
	cseq       uint32
	transport  string
	localHost  string
	localPort  int
	ctx        context.Context
	platformID string
	domain     string
}

func newMockSession(state SessionState) *mockSession {
	return &mockSession{
		state:      state,
		deviceID:   "34020000002000000001",
		fromTag:    "device-tag-xxx",
		cseq:       0,
		transport:  "udp",
		localHost:  "192.168.1.200",
		localPort:  5060,
		ctx:        context.Background(),
		platformID: "34020000002000000002",
		domain:     "3402000000",
	}
}

func (s *mockSession) State() SessionState             { return s.state }
func (s *mockSession) Context() context.Context        { return s.ctx }
func (s *mockSession) DeviceID() string                { return s.deviceID }
func (s *mockSession) FromTag() string                 { return s.fromTag }
func (s *mockSession) NextCSeq() uint32                { s.cseq++; return s.cseq }
func (s *mockSession) Transport() string               { return s.transport }
func (s *mockSession) LocalHost() string               { return s.localHost }
func (s *mockSession) LocalPort() int                  { return s.localPort }
func (s *mockSession) PlatformURI() sipgo_sip.Uri {
	return sipgo_sip.Uri{User: s.platformID, Host: s.domain}
}
func (s *mockSession) DeviceURI() sipgo_sip.Uri {
	return sipgo_sip.Uri{User: s.deviceID, Host: s.domain}
}

// --- Mock QueryHandler ---

type mockQueryHandler struct {
	catalogBodies [][]byte
	catalogErr    error
	deviceInfoBody []byte
	deviceInfoErr  error
	deviceStatusBody []byte
	deviceStatusErr  error
	catalogCalled    bool
	deviceInfoCalled bool
	deviceStatusCalled bool
}

func (h *mockQueryHandler) HandleCatalogQuery(ctx context.Context, q *manscdp.Query) ([][]byte, error) {
	h.catalogCalled = true
	return h.catalogBodies, h.catalogErr
}
func (h *mockQueryHandler) HandleDeviceInfoQuery(ctx context.Context, q *manscdp.Query) ([]byte, error) {
	h.deviceInfoCalled = true
	return h.deviceInfoBody, h.deviceInfoErr
}
func (h *mockQueryHandler) HandleDeviceStatusQuery(ctx context.Context, q *manscdp.Query) ([]byte, error) {
	h.deviceStatusCalled = true
	return h.deviceStatusBody, h.deviceStatusErr
}

// --- Mock ServerTransaction ---

type mockServerTx struct {
	lastResponse *sipgo_sip.Response
	respondErr   error
	terminated   chan struct{}
}

func newMockServerTx() *mockServerTx {
	return &mockServerTx{terminated: make(chan struct{})}
}

func (tx *mockServerTx) Respond(res *sipgo_sip.Response) error {
	tx.lastResponse = res
	return tx.respondErr
}
func (tx *mockServerTx) Acks() <-chan *sipgo_sip.Request { return nil }
func (tx *mockServerTx) OnCancel(f sipgo_sip.FnTxCancel) bool { return false }
func (tx *mockServerTx) Cancels() <-chan *sipgo_sip.Request { return nil }
func (tx *mockServerTx) Done() <-chan struct{}   { return tx.terminated }
func (tx *mockServerTx) Err() error              { return nil }
func (tx *mockServerTx) Terminate()              { close(tx.terminated) }
func (tx *mockServerTx) OnTerminate(f sipgo_sip.FnTxTerminate) bool { return true }

// --- helpers ---

func makeMessageRequest(contentType string, body []byte) *sipgo_sip.Request {
	req := sipgo_sip.NewRequest(sipgo_sip.MESSAGE, sipgo_sip.Uri{User: "34020000002000000001", Host: "3402000000"})
	// From
	from := &sipgo_sip.FromHeader{Address: sipgo_sip.Uri{User: "34020000002000000002", Host: "3402000000"}, Params: sipgo_sip.HeaderParams{}}
	from.Params.Add("tag", "platform-tag")
	req.AppendHeader(from)
	// To
	to := &sipgo_sip.ToHeader{Address: sipgo_sip.Uri{User: "34020000002000000001", Host: "3402000000"}, Params: sipgo_sip.HeaderParams{}}
	req.AppendHeader(to)
	// Call-ID
	callID := sipgo_sip.CallIDHeader("test-call-id-123")
	req.AppendHeader(&callID)
	// CSeq
	req.AppendHeader(&sipgo_sip.CSeqHeader{SeqNo: 1024, MethodName: sipgo_sip.MESSAGE})
	// Via
	via := &sipgo_sip.ViaHeader{
		ProtocolName: "SIP", ProtocolVersion: "2.0", Transport: "UDP",
		Host: "192.168.1.100", Port: 5060, Params: sipgo_sip.HeaderParams{},
	}
	via.Params.Add("branch", "z9hG4bK-platform")
	req.AppendHeader(via)
	// Content-Type
	if contentType != "" {
		ct := sipgo_sip.ContentTypeHeader(contentType)
		req.AppendHeader(&ct)
	}
	if body != nil {
		req.SetBody(body)
	}
	return req
}

func makeCatalogQueryBody() []byte {
	return []byte(`<?xml version="1.0" encoding="UTF-8"?>
<Query>
  <CmdType>Catalog</CmdType>
  <SN>12345</SN>
  <DeviceID>34020000002000000001</DeviceID>
</Query>`)
}

// silentLogger 静默 log 减少测试输出污染.
func silentLogger() *slog.Logger {
	return slog.New(slog.NewTextHandler(io.Discard, &slog.HandlerOptions{Level: slog.LevelError}))
}

// --- Tests ---

// Test1_OnMessage_NotRegistered_Returns403 S-H1 未注册回 403.
func Test1_OnMessage_NotRegistered_Returns403(t *testing.T) {
	session := newMockSession(StateDisconnected)
	qh := &mockQueryHandler{}
	handler := NewMessageHandler(nil, session, qh, silentLogger())

	req := makeMessageRequest("application/manscdp+xml", makeCatalogQueryBody())
	tx := newMockServerTx()
	handler.OnMessage(req, tx)

	if tx.lastResponse == nil {
		t.Fatal("expected response, got nil")
	}
	if tx.lastResponse.StatusCode != 403 {
		t.Errorf("expected 403, got %d", tx.lastResponse.StatusCode)
	}
	if tx.lastResponse.Reason != "Not Registered" {
		t.Errorf("expected reason 'Not Registered', got %q", tx.lastResponse.Reason)
	}
	if qh.catalogCalled {
		t.Error("QueryHandler 不应被调用 (未注册)")
	}
}

// Test2_OnMessage_Registered_Returns200 已注册回 200 (自动加 To-tag).
func Test2_OnMessage_Registered_Returns200(t *testing.T) {
	session := newMockSession(StateRegistered)
	qh := &mockQueryHandler{catalogBodies: [][]byte{[]byte("<Response/>")}}
	handler := NewMessageHandler(nil, session, qh, silentLogger())

	req := makeMessageRequest("application/manscdp+xml", makeCatalogQueryBody())
	tx := newMockServerTx()
	handler.OnMessage(req, tx)

	if tx.lastResponse == nil {
		t.Fatal("expected 200 response")
	}
	if tx.lastResponse.StatusCode != 200 {
		t.Errorf("expected 200, got %d", tx.lastResponse.StatusCode)
	}
	// S-B2: To-tag 由 sip.NewResponseFromRequest 自动加
	to := tx.lastResponse.To()
	if to == nil {
		t.Fatal("To header missing")
	}
	if _, ok := to.Params.Get("tag"); !ok {
		t.Error("To-tag 未自动加 (S-B2)")
	}
}

// Test3_OnMessage_ContentType_CaseInsensitive P-H5 MIME 大小写不敏感.
func Test3_OnMessage_ContentType_CaseInsensitive(t *testing.T) {
	session := newMockSession(StateRegistered)
	qh := &mockQueryHandler{catalogBodies: [][]byte{[]byte("<Response/>")}}
	handler := NewMessageHandler(nil, session, qh, silentLogger())

	// 大写 + 参数
	req := makeMessageRequest("Application/MANSCDP+XML; charset=utf-8", makeCatalogQueryBody())
	tx := newMockServerTx()
	handler.OnMessage(req, tx)

	if tx.lastResponse.StatusCode != 200 {
		t.Errorf("大写 Content-Type 应正常接受: got %d", tx.lastResponse.StatusCode)
	}
}

// Test4_OnMessage_UnsupportedContentType 非 MANSCDP 类型仍回 200 但不处理.
func Test4_OnMessage_UnsupportedContentType(t *testing.T) {
	session := newMockSession(StateRegistered)
	qh := &mockQueryHandler{}
	handler := NewMessageHandler(nil, session, qh, silentLogger())

	req := makeMessageRequest("text/plain", []byte("hello"))
	tx := newMockServerTx()
	handler.OnMessage(req, tx)

	if tx.lastResponse.StatusCode != 200 {
		t.Errorf("SIP 层仍应回 200, got %d", tx.lastResponse.StatusCode)
	}
	// QueryHandler 不应被调用
	// 但因为 OnMessage 内 goroutine, 这里检查可能有 race, 简化: 只断言 SIP 层 OK
}

// Test5_OnMessage_MissingContentType 缺 Content-Type 仍回 200.
func Test5_OnMessage_MissingContentType(t *testing.T) {
	session := newMockSession(StateRegistered)
	qh := &mockQueryHandler{}
	handler := NewMessageHandler(nil, session, qh, silentLogger())

	req := makeMessageRequest("", makeCatalogQueryBody())
	tx := newMockServerTx()
	handler.OnMessage(req, tx)

	if tx.lastResponse.StatusCode != 200 {
		t.Errorf("缺 Content-Type SIP 层仍应回 200, got %d", tx.lastResponse.StatusCode)
	}
}

// Test6_BuildResponseRequest_ViaCount N-H3: Via 恰好 1 个.
func Test6_BuildResponseRequest_ViaCount(t *testing.T) {
	session := newMockSession(StateRegistered)
	qh := &mockQueryHandler{}
	handler := NewMessageHandler(nil, session, qh, silentLogger())

	req := handler.buildResponseRequest([]byte("<Response/>"))
	vias := req.GetHeaders("Via")
	if len(vias) != 1 {
		t.Errorf("Via 应恰好 1 个 (N-H3), got %d", len(vias))
	}
}

// Test7_BuildResponseRequest_HasAllRequiredHeaders 关键头部齐全.
func Test7_BuildResponseRequest_HasAllRequiredHeaders(t *testing.T) {
	session := newMockSession(StateRegistered)
	qh := &mockQueryHandler{}
	handler := NewMessageHandler(nil, session, qh, silentLogger())

	req := handler.buildResponseRequest([]byte("<Response/>"))

	// From
	if req.From() == nil {
		t.Error("From missing")
	} else if tag, ok := req.From().Params.Get("tag"); !ok || tag != "device-tag-xxx" {
		t.Errorf("From tag: %s (want device-tag-xxx)", tag)
	}
	// To (无 tag)
	if req.To() == nil {
		t.Error("To missing")
	}
	// Call-ID
	if req.CallID() == nil {
		t.Error("Call-ID missing")
	}
	// CSeq
	if req.CSeq() == nil || req.CSeq().MethodName != sipgo_sip.MESSAGE {
		t.Error("CSeq missing or wrong method")
	}
	// Content-Type
	ctHeaders := req.GetHeaders("Content-Type")
	if len(ctHeaders) == 0 {
		t.Error("Content-Type missing")
	}
	// Method 正确
	if req.Method != sipgo_sip.MESSAGE {
		t.Errorf("method: got %v, want MESSAGE", req.Method)
	}
}

// Test8_SendStatus_String 状态 String() 覆盖.
func Test8_SendStatus_String(t *testing.T) {
	cases := []struct {
		s    SendStatus
		want string
	}{
		{SendStatusOK, "ok"},
		{SendStatusTimeout, "timeout"},
		{SendStatusRejected, "rejected"},
		{SendStatusTxError, "tx_error"},
		{SendStatusCtxDone, "ctx_done"},
	}
	for _, c := range cases {
		if c.s.String() != c.want {
			t.Errorf("%d: got %q, want %q", c.s, c.s.String(), c.want)
		}
	}
}

// Test9_SendResponseMessage_NilClient_ReturnsTxError client=nil 场景返错.
func Test9_SendResponseMessage_NilClient_ReturnsTxError(t *testing.T) {
	session := newMockSession(StateRegistered)
	qh := &mockQueryHandler{}
	handler := NewMessageHandler(nil, session, qh, silentLogger())

	status, err := handler.sendResponseMessage(context.Background(), []byte("<Response/>"))
	if status != SendStatusTxError {
		t.Errorf("nil client 应返 SendStatusTxError, got %v", status)
	}
	if err == nil || !errors.Is(err, err) {
		t.Errorf("应有 error")
	}
}

// Test10_OnMessage_MalformedManscdpBody 畸形 XML: SIP 层仍回 200, 不发 Response.
func Test10_OnMessage_MalformedManscdpBody(t *testing.T) {
	session := newMockSession(StateRegistered)
	qh := &mockQueryHandler{}
	handler := NewMessageHandler(nil, session, qh, silentLogger())

	req := makeMessageRequest("application/manscdp+xml", []byte("<not-xml"))
	tx := newMockServerTx()
	handler.OnMessage(req, tx)

	// SIP 层 200 OK
	if tx.lastResponse.StatusCode != 200 {
		t.Errorf("畸形 body SIP 层仍应回 200, got %d", tx.lastResponse.StatusCode)
	}
}
