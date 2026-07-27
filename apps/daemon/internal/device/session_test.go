package device

import (
	"context"
	"errors"
	"fmt"
	"runtime"
	"sync"
	"testing"
	"time"

	"github.com/emiago/sipgo/sip"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/internal/gb28181"
	sipx "github.com/menglulu/uvp-gb28181-sim-desktop/daemon/internal/sip"
)

// mockSipClient 记录调用并按预设脚本返回响应,用于测试 session 状态机。
// 不用 sipgo,不依赖网络,严格 mock 接口边界。
type mockSipClient struct {
	mu          sync.Mutex
	calls       []*sip.Request
	responses   []mockResponse
	callIndex   int
	overrideErr error // 若非 nil,所有 Do 返回该错误
}

type mockResponse struct {
	statusCode           int
	reason               string
	expiresHeader        string // 独立 Expires 头值(为空表示不设)
	contactExpiresParam  string // Contact 头 expires 参数
}

func (m *mockSipClient) Do(ctx context.Context, req *sip.Request) (*sip.Response, error) {
	m.mu.Lock()
	defer m.mu.Unlock()

	m.calls = append(m.calls, req)

	if m.overrideErr != nil {
		return nil, m.overrideErr
	}

	if m.callIndex >= len(m.responses) {
		return nil, errors.New("mock: no more responses configured")
	}
	spec := m.responses[m.callIndex]
	m.callIndex++

	resp := sip.NewResponseFromRequest(req, spec.statusCode, spec.reason, nil)
	if spec.expiresHeader != "" {
		resp.AppendHeader(sip.NewHeader("Expires", spec.expiresHeader))
	}
	if spec.contactExpiresParam != "" {
		// 构造 Contact 头带 expires 参数
		contactURI := sip.Uri{User: "test", Host: "1.1.1.1", Port: 5060}
		contactParams := sip.NewParams()
		contactParams.Add("expires", spec.contactExpiresParam)
		resp.AppendHeader(&sip.ContactHeader{
			Address: contactURI,
			Params:  contactParams,
		})
	}
	return resp, nil
}

func (m *mockSipClient) callCount() int {
	m.mu.Lock()
	defer m.mu.Unlock()
	return len(m.calls)
}

func (m *mockSipClient) lastCall() *sip.Request {
	m.mu.Lock()
	defer m.mu.Unlock()
	if len(m.calls) == 0 {
		return nil
	}
	return m.calls[len(m.calls)-1]
}

// newRawTestSession 创建一个禁用 auto-spawn heartbeat/renewal 的 session,
// 便于 T1/T2/T3 各自独立单元测试内部 loop,不受 T4 auto-spawn 干扰。
func newRawTestSession(client SipClient, cfg *gb28181.SipConfig) *RegistrationSession {
	s := NewRegistrationSession(client, cfg)
	s.DisableBackgroundLoopsForTest()
	return s
}

func newTestConfig() *gb28181.SipConfig {
	return &gb28181.SipConfig{
		DeviceID:     "34020000001320000001",
		ServerHost:   "127.0.0.1",
		ServerPort:   5060,
		ServerID:     "34020000002000000001",
		ServerDomain: "3402000000",
		Password:     "12345678",
		Transport:    "udp",
		ExpiresSecs:  3600,
	}
}

func TestSession_RegisterSuccess(t *testing.T) {
	client := &mockSipClient{
		responses: []mockResponse{
			{statusCode: 200, reason: "OK"},
		},
	}
	session := NewRegistrationSession(client, newTestConfig())

	if err := session.Start(context.Background()); err != nil {
		t.Fatalf("Start failed: %v", err)
	}
	if session.State() != StateRegistered {
		t.Errorf("expected StateRegistered, got %s", session.State())
	}

	// spec Q10: Call-ID 与 From tag 应该已设置
	req := client.lastCall()
	if req == nil {
		t.Fatal("no request received")
	}
	if req.CallID() == nil || string(*req.CallID()) == "" {
		t.Error("Call-ID should be set")
	}
}

func TestSession_ParseServerExpires_HeaderPriority(t *testing.T) {
	// spec Q9: 独立 Expires 头 (无 Contact expires 参数场景)
	client := &mockSipClient{
		responses: []mockResponse{
			{statusCode: 200, expiresHeader: "300"},
		},
	}
	session := NewRegistrationSession(client, newTestConfig())

	if err := session.Start(context.Background()); err != nil {
		t.Fatal(err)
	}
	if session.RegisteredExpires() != 300 {
		t.Errorf("expected registered expires 300, got %d", session.RegisteredExpires())
	}
}

func TestSession_ParseServerExpires_ContactPriorityWins(t *testing.T) {
	// spec Q9: Contact 头 expires 参数优先于独立 Expires 头
	client := &mockSipClient{
		responses: []mockResponse{
			{
				statusCode:          200,
				expiresHeader:       "300",
				contactExpiresParam: "600",
			},
		},
	}
	session := NewRegistrationSession(client, newTestConfig())

	if err := session.Start(context.Background()); err != nil {
		t.Fatal(err)
	}
	if session.RegisteredExpires() != 600 {
		t.Errorf("expected Contact expires priority (600), got %d", session.RegisteredExpires())
	}
}

func TestSession_ExpiresBothMissing_FallbackToRequest(t *testing.T) {
	// spec Q11: 响应缺 Expires 且 Contact 无 expires 参数 → 用请求发出去的值
	client := &mockSipClient{
		responses: []mockResponse{
			{statusCode: 200}, // 无 Expires 无 Contact
		},
	}
	cfg := newTestConfig()
	cfg.ExpiresSecs = 1800
	session := NewRegistrationSession(client, cfg)

	if err := session.Start(context.Background()); err != nil {
		t.Fatal(err)
	}
	if session.RegisteredExpires() != 1800 {
		t.Errorf("expected fallback to request value 1800, got %d", session.RegisteredExpires())
	}
}

func TestSession_RegisterTimeout(t *testing.T) {
	client := &mockSipClient{
		overrideErr: fmt.Errorf("%w: mock timeout", sipx.ErrTimeout),
	}
	session := NewRegistrationSession(client, newTestConfig())

	err := session.Start(context.Background())
	if err == nil {
		t.Fatal("expected error, got nil")
	}
	if !errors.Is(err, sipx.ErrTimeout) {
		t.Errorf("expected ErrTimeout in chain, got: %v", err)
	}
	if session.State() != StateFailed {
		t.Errorf("expected StateFailed, got %s", session.State())
	}
}

func TestSession_RegisterRejected(t *testing.T) {
	// 平台返回 403 (密码错等)
	client := &mockSipClient{
		responses: []mockResponse{
			{statusCode: 403, reason: "Forbidden"},
		},
	}
	session := NewRegistrationSession(client, newTestConfig())

	err := session.Start(context.Background())
	if err == nil {
		t.Fatal("expected error for 403")
	}
	if session.State() != StateFailed {
		t.Errorf("expected StateFailed, got %s", session.State())
	}
}

func TestSession_StopIdempotent(t *testing.T) {
	client := &mockSipClient{
		responses: []mockResponse{{statusCode: 200}},
	}
	session := NewRegistrationSession(client, newTestConfig())
	_ = session.Start(context.Background())

	// 多次 Stop 应该都不 panic
	if err := session.Stop(); err != nil {
		t.Error(err)
	}
	if err := session.Stop(); err != nil {
		t.Error(err)
	}
	if session.State() != StateDisconnected {
		t.Errorf("expected StateDisconnected after Stop, got %s", session.State())
	}
}

func TestSession_StartOnlyOnce(t *testing.T) {
	client := &mockSipClient{
		responses: []mockResponse{
			{statusCode: 200},
			{statusCode: 200}, // 第二次 Start 若真发,mock 会命中这条
		},
	}
	session := NewRegistrationSession(client, newTestConfig())
	if err := session.Start(context.Background()); err != nil {
		t.Fatal(err)
	}

	// 第二次 Start 应该拒绝 (状态已在 Registered)
	err := session.Start(context.Background())
	if err == nil {
		t.Error("second Start should fail")
	}

	// 只调用了 1 次 client.Do
	if client.callCount() != 1 {
		t.Errorf("expected 1 client call, got %d", client.callCount())
	}
}

func TestSession_NoGoroutineLeak(t *testing.T) {
	// spec R5: 反复 start/stop 100 次 goroutine 应稳定
	// M1 session 本身不 spawn goroutine (heartbeat/renewal 归 M3),
	// 但仍做这个断言避免未来引入泄漏。
	before := runtime.NumGoroutine()

	for i := 0; i < 100; i++ {
		client := &mockSipClient{
			responses: []mockResponse{{statusCode: 200}},
		}
		session := NewRegistrationSession(client, newTestConfig())
		_ = session.Start(context.Background())
		_ = session.Stop()
	}

	// 等 gc/调度稳定
	time.Sleep(100 * time.Millisecond)
	runtime.GC()

	after := runtime.NumGoroutine()
	// 允许 2 个的偏差 (GC / test framework)
	if after-before > 2 {
		t.Errorf("goroutine leak: before=%d, after=%d", before, after)
	}
}

// 用一次 sipx 类型引用防止 import 被"未使用"清掉
var _ = sipx.ErrTimeout
