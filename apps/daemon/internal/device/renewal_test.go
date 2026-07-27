package device

import (
	"context"
	"errors"
	"sync"
	"testing"
	"time"

	"github.com/emiago/sipgo/sip"
)

// TestRenewal_ComputeWait_MinFloor60s: 公式边界
func TestRenewal_ComputeWait_Formula(t *testing.T) {
	cases := []struct {
		expires int
		want    time.Duration
		note    string
	}{
		// 3600s * 0.8 = 2880s;  3600 - 60 = 3540s → 取 max = 3540s
		{3600, 3540 * time.Second, "长 Expires 用 -60 margin"},
		// 100s * 0.8 = 80s;   100 - 60 = 40s → 取 max = 80s
		{100, 80 * time.Second, "短 Expires 用 0.8 ratio"},
		// 200 * 0.8 = 160s;   200 - 60 = 140s → 取 max = 160s
		{200, 160 * time.Second, "中等 Expires 用 0.8 ratio"},
		// 60s → 走极短分支 60-5 = 55s
		{60, 55 * time.Second, "60s 极短兜底"},
		// 30s → 30-5 = 25s
		{30, 25 * time.Second, "30s 极短兜底"},
		// 5s → 5-5 = 0 → floor 到 5s
		{5, renewalMinFloor, "极端小 Expires floor 到 5s"},
		// 0s → floor
		{0, renewalMinFloor, "0s 走 floor"},
	}
	for _, c := range cases {
		got := computeRenewalWait(c.expires)
		if got != c.want {
			t.Errorf("computeRenewalWait(%d) = %v, want %v (%s)", c.expires, got, c.want, c.note)
		}
	}
}

// renewalFakeClient: 路由 REGISTER 到脚本化的多轮响应,记录 CSeq 递增。
type renewalFakeClient struct {
	mu                sync.Mutex
	registerResponses []mockResponse
	registerErr       error
	idx               int
	calls             []*sip.Request
}

func (c *renewalFakeClient) Do(ctx context.Context, req *sip.Request) (*sip.Response, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.calls = append(c.calls, req)
	if req.Method != sip.REGISTER {
		return nil, errors.New("renewalFakeClient only serves REGISTER")
	}
	if c.registerErr != nil {
		return nil, c.registerErr
	}
	if c.idx >= len(c.registerResponses) {
		return nil, errors.New("renewalFakeClient: no more responses")
	}
	spec := c.registerResponses[c.idx]
	c.idx++
	resp := sip.NewResponseFromRequest(req, spec.statusCode, spec.reason, nil)
	if spec.expiresHeader != "" {
		resp.AppendHeader(sip.NewHeader("Expires", spec.expiresHeader))
	}
	return resp, nil
}

// TestRenewal_TriggerAtWaitBoundary: 短 Expires 场景下确认 renewal 触发时机
// (用 config expires=6s → wait = max(4.8, -54) → 极短分支 = 6-5 = 1s)
func TestRenewal_TriggerFires(t *testing.T) {
	client := &renewalFakeClient{
		registerResponses: []mockResponse{
			{statusCode: 200, expiresHeader: "6"}, // 首次注册,Expires=6s → wait=1s
			{statusCode: 200, expiresHeader: "6"}, // 首次续约
		},
	}
	session := newRawTestSession(client, newTestConfig())
	bus := &captureBus{}
	session.SetPublisher(bus)
	if err := session.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	// 触发 renewal 应在 ~1s 后 (6-5 = 1s)
	rn := NewRenewal(session)
	ctx, cancel := context.WithCancel(session.InternalContext())
	defer cancel()

	done := make(chan struct{})
	go func() { rn.Run(ctx); close(done) }()

	// 等 ~1.5s (给点余量)
	time.Sleep(1500 * time.Millisecond)
	cancel()
	<-done

	// 应该至少发过 2 次 REGISTER (初始 + 续约)
	if len(client.calls) < 2 {
		t.Fatalf("expected >= 2 REGISTER calls, got %d", len(client.calls))
	}
	if session.State() != StateRegistered {
		t.Errorf("state after renewal = %v, want Registered", session.State())
	}
	// renewal_result 事件应有 ok=true
	if bus.count("renewal_result") == 0 {
		t.Error("expected renewal_result event")
	}
	// CSeq 单调递增
	if len(client.calls) >= 2 {
		cseq1 := client.calls[0].CSeq().SeqNo
		cseq2 := client.calls[1].CSeq().SeqNo
		if cseq2 <= cseq1 {
			t.Errorf("CSeq not monotonic: %d → %d", cseq1, cseq2)
		}
	}
}

// TestRenewal_SuccessKeepsRegistered: 续约 200 OK 状态保持 Registered。
func TestRenewal_SuccessKeepsRegistered(t *testing.T) {
	client := &renewalFakeClient{
		registerResponses: []mockResponse{
			{statusCode: 200, expiresHeader: "6"},
			{statusCode: 200, expiresHeader: "6"},
			{statusCode: 200, expiresHeader: "6"},
		},
	}
	session := newRawTestSession(client, newTestConfig())
	if err := session.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	rn := NewRenewal(session)
	ctx, cancel := context.WithCancel(session.InternalContext())
	defer cancel()

	done := make(chan struct{})
	go func() { rn.Run(ctx); close(done) }()

	time.Sleep(1500 * time.Millisecond)
	if session.State() != StateRegistered {
		t.Errorf("mid-renewal state = %v, want Registered", session.State())
	}
	cancel()
	<-done
}

// TestRenewal_FailureMarksFailed: 续约收 403 → session Failed。
func TestRenewal_FailureMarksFailed(t *testing.T) {
	client := &renewalFakeClient{
		registerResponses: []mockResponse{
			{statusCode: 200, expiresHeader: "6"}, // 首注册成功
			{statusCode: 403, reason: "Forbidden"}, // 续约被拒
		},
	}
	session := newRawTestSession(client, newTestConfig())
	bus := &captureBus{}
	session.SetPublisher(bus)
	if err := session.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	rn := NewRenewal(session)
	ctx, cancel := context.WithCancel(session.InternalContext())
	defer cancel()

	done := make(chan struct{})
	go func() { rn.Run(ctx); close(done) }()

	// 等续约触发 + 判定
	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		if session.State() == StateFailed {
			break
		}
		time.Sleep(50 * time.Millisecond)
	}
	cancel()
	<-done

	if session.State() != StateFailed {
		t.Errorf("state after 403 renewal = %v, want Failed", session.State())
	}
	// device_state:Failed 事件应有
	found := false
	for _, e := range bus.find("device_state") {
		if s, _ := e.Payload["state"].(string); s == "Failed" {
			found = true
			break
		}
	}
	if !found {
		t.Error("expected device_state:Failed event")
	}
}

// TestRenewal_CtxCancelExits: ctx cancel 立即退出。
func TestRenewal_CtxCancelExits(t *testing.T) {
	client := &renewalFakeClient{
		registerResponses: []mockResponse{
			{statusCode: 200, expiresHeader: "3600"}, // 大 Expires → wait ~ 3540s
		},
	}
	session := newRawTestSession(client, newTestConfig())
	if err := session.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	rn := NewRenewal(session)
	ctx, cancel := context.WithCancel(session.InternalContext())
	done := make(chan struct{})
	go func() { rn.Run(ctx); close(done) }()

	// 快速 cancel
	time.Sleep(10 * time.Millisecond)
	cancel()

	select {
	case <-done:
		// ok
	case <-time.After(200 * time.Millisecond):
		t.Fatal("Renewal.Run did not exit within 200ms of cancel")
	}
}
