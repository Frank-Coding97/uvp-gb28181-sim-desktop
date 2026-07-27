package device

import (
	"context"
	"strconv"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/emiago/sipgo/sip"
)

// shutdownFakeClient: 记录 REGISTER 请求,可为不同轮次返回不同响应/延迟。
type shutdownFakeClient struct {
	mu             sync.Mutex
	calls          []*sip.Request
	responses      []mockResponse
	delayPerCall   []time.Duration // 每次 Do 之前 sleep,模拟慢平台
	messageIgnored atomic.Bool     // MESSAGE 请求不参与响应脚本
}

func (c *shutdownFakeClient) Do(ctx context.Context, req *sip.Request) (*sip.Response, error) {
	c.mu.Lock()
	idx := len(c.calls)
	c.calls = append(c.calls, req)
	// MESSAGE 单独响应 200,不占 responses 脚本
	if req.Method == sip.MESSAGE {
		c.mu.Unlock()
		return sip.NewResponseFromRequest(req, 200, "OK", nil), nil
	}
	var delay time.Duration
	if idx < len(c.delayPerCall) {
		delay = c.delayPerCall[idx]
	}
	c.mu.Unlock()

	if delay > 0 {
		select {
		case <-time.After(delay):
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}

	c.mu.Lock()
	defer c.mu.Unlock()
	// registerResponses 按 REGISTER 顺序取
	registerIdx := 0
	for _, r := range c.calls[:idx+1] {
		if r.Method == sip.REGISTER {
			registerIdx++
		}
	}
	respIdx := registerIdx - 1
	if respIdx >= len(c.responses) {
		return sip.NewResponseFromRequest(req, 200, "OK", nil), nil
	}
	spec := c.responses[respIdx]
	return sip.NewResponseFromRequest(req, spec.statusCode, spec.reason, nil), nil
}

func (c *shutdownFakeClient) allCalls() []*sip.Request {
	c.mu.Lock()
	defer c.mu.Unlock()
	out := make([]*sip.Request, len(c.calls))
	copy(out, c.calls)
	return out
}

func (c *shutdownFakeClient) registerCalls() []*sip.Request {
	c.mu.Lock()
	defer c.mu.Unlock()
	var out []*sip.Request
	for _, r := range c.calls {
		if r.Method == sip.REGISTER {
			out = append(out, r)
		}
	}
	return out
}

// TestShutdown_SendsExpiresZero: Stop 后应发一条 Expires=0 REGISTER。
func TestShutdown_SendsExpiresZero(t *testing.T) {
	client := &shutdownFakeClient{
		responses: []mockResponse{
			{statusCode: 200}, // 初始 REGISTER
			{statusCode: 200}, // 注销 REGISTER
		},
	}
	session := newRawTestSession(client, newTestConfig())
	if err := session.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	if err := session.Stop(); err != nil {
		t.Fatalf("Stop: %v", err)
	}

	// 应该有 2 条 REGISTER
	regs := client.registerCalls()
	if len(regs) < 2 {
		t.Fatalf("expected 2 REGISTER calls, got %d", len(regs))
	}
	// 第 2 条 Expires 头 = 0
	last := regs[len(regs)-1]
	expHdr := last.GetHeader("Expires")
	if expHdr == nil {
		t.Fatal("shutdown REGISTER missing Expires header")
	}
	v, err := strconv.Atoi(expHdr.Value())
	if err != nil || v != 0 {
		t.Errorf("shutdown Expires = %q, want 0", expHdr.Value())
	}
	// CSeq 应比初始大
	cseq1 := regs[0].CSeq().SeqNo
	cseq2 := regs[1].CSeq().SeqNo
	if cseq2 <= cseq1 {
		t.Errorf("shutdown CSeq %d not > initial %d", cseq2, cseq1)
	}
	// Call-ID / From tag 保持
	if string(*regs[0].CallID()) != string(*regs[1].CallID()) {
		t.Errorf("Call-ID changed: %v → %v", regs[0].CallID(), regs[1].CallID())
	}
	if session.State() != StateDisconnected {
		t.Errorf("state after Stop = %v, want Disconnected", session.State())
	}
}

// TestShutdown_TimeoutForceCleanup: 平台无响应,6s 超时后仍进 Disconnected。
func TestShutdown_TimeoutForceCleanup(t *testing.T) {
	client := &shutdownFakeClient{
		responses: []mockResponse{
			{statusCode: 200}, // 初始 REGISTER 快速
			{statusCode: 200}, // 注销 REGISTER 会延迟
		},
		// 让注销 REGISTER 慢 8s (超过 6s 超时)
		delayPerCall: []time.Duration{0, 8 * time.Second},
	}
	session := newRawTestSession(client, newTestConfig())
	if err := session.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	start := time.Now()
	err := session.Stop()
	dur := time.Since(start)

	// Stop 不应报错 (即便平台不响应也进 Disconnected)
	if err != nil {
		t.Errorf("Stop should not error on timeout, got: %v", err)
	}
	// 应在 ~6s 内返回,不能等 8s
	if dur > 7*time.Second {
		t.Errorf("Stop took %v, expected < 7s (6s timeout + jitter)", dur)
	}
	if session.State() != StateDisconnected {
		t.Errorf("state = %v after Stop timeout, want Disconnected", session.State())
	}
}

// TestShutdown_CancelsChildTasks: Stop 后 heartbeat/renewal 若已 spawn,都被 cancel。
func TestShutdown_CancelsChildTasks(t *testing.T) {
	client := &shutdownFakeClient{
		responses: []mockResponse{
			{statusCode: 200},
			{statusCode: 200},
		},
	}
	session := newRawTestSession(client, newTestConfig())
	if err := session.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	// 手工 spawn heartbeat / renewal 绑到 session.InternalContext()
	hb := NewHeartbeat(session, 30*time.Millisecond)
	rn := NewRenewal(session)
	hbDone := make(chan struct{})
	rnDone := make(chan struct{})
	go func() { hb.Run(session.InternalContext()); close(hbDone) }()
	go func() { rn.Run(session.InternalContext()); close(rnDone) }()

	time.Sleep(100 * time.Millisecond)
	_ = session.Stop()

	select {
	case <-hbDone:
	case <-time.After(500 * time.Millisecond):
		t.Fatal("heartbeat did not exit after Stop")
	}
	select {
	case <-rnDone:
	case <-time.After(500 * time.Millisecond):
		t.Fatal("renewal did not exit after Stop")
	}
}

// TestShutdown_IdempotentStop: 多次 Stop 只应发一次 Expires=0 REGISTER。
func TestShutdown_IdempotentStop(t *testing.T) {
	client := &shutdownFakeClient{
		responses: []mockResponse{
			{statusCode: 200},
			{statusCode: 200},
		},
	}
	session := newRawTestSession(client, newTestConfig())
	_ = session.Start(context.Background())

	for i := 0; i < 3; i++ {
		if err := session.Stop(); err != nil {
			t.Errorf("Stop #%d: %v", i, err)
		}
	}

	regs := client.registerCalls()
	if len(regs) != 2 {
		t.Errorf("expected exactly 2 REGISTER (init + 1 shutdown), got %d", len(regs))
	}
}

// TestShutdown_SkipsIfNotRegistered: 未注册就 Stop,不发 Expires=0 REGISTER。
func TestShutdown_SkipsIfNotRegistered(t *testing.T) {
	client := &shutdownFakeClient{
		responses: []mockResponse{
			{statusCode: 403, reason: "Forbidden"}, // 初始注册失败
		},
	}
	session := newRawTestSession(client, newTestConfig())
	_ = session.Start(context.Background()) // 会失败 → Failed

	if err := session.Stop(); err != nil {
		t.Errorf("Stop on Failed session: %v", err)
	}

	regs := client.registerCalls()
	// 只有初始 REGISTER,没有 shutdown REGISTER
	if len(regs) != 1 {
		t.Errorf("expected only initial REGISTER, got %d", len(regs))
	}
}
