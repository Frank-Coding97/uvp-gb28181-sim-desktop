package device

import (
	"context"
	"runtime"
	"testing"
	"time"
)

// T0 RED: Session 持根 ctx,Stop() cancel 根 ctx → 所有派生 ctx.Done() 触发。
//
// 这类测试并发/生命周期敏感 —— 必须 `go test -race`。
func TestSession_ContextTreeCancel(t *testing.T) {
	client := &mockSipClient{
		responses: []mockResponse{{statusCode: 200}},
	}
	session := NewRegistrationSession(client, newTestConfig())

	if err := session.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	// InternalContext 应可用且未 cancel
	internal := session.InternalContext()
	if internal == nil {
		t.Fatal("InternalContext() returned nil")
	}
	select {
	case <-internal.Done():
		t.Fatal("InternalContext should be alive before Stop")
	default:
	}

	// 从根 ctx 派生一个子 ctx (模拟 heartbeat/renewal goroutine)
	childCtx, childCancel := context.WithCancel(internal)
	defer childCancel()

	// Stop → 应触发 childCtx.Done
	if err := session.Stop(); err != nil {
		t.Fatalf("Stop: %v", err)
	}

	select {
	case <-childCtx.Done():
		// ok
	case <-time.After(500 * time.Millisecond):
		t.Fatal("child ctx did not observe cancel within 500ms after Stop")
	}
	select {
	case <-internal.Done():
		// ok
	case <-time.After(50 * time.Millisecond):
		t.Fatal("internal ctx should be cancelled after Stop")
	}
}

// Stop() 多次调用应幂等,不 panic。
func TestSession_StopIdempotent_ContextTree(t *testing.T) {
	client := &mockSipClient{
		responses: []mockResponse{{statusCode: 200}},
	}
	session := NewRegistrationSession(client, newTestConfig())
	if err := session.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	for i := 0; i < 5; i++ {
		if err := session.Stop(); err != nil {
			t.Fatalf("Stop #%d: %v", i, err)
		}
	}

	// 状态应为 Disconnected
	if session.State() != StateDisconnected {
		t.Errorf("state after Stop = %v, want Disconnected", session.State())
	}
}

// T0 RED: 100 次 start/stop 循环 goroutine 稳定 (spec R5)。
func TestSession_NoGoroutineLeak_LongRun(t *testing.T) {
	// 预热一次让 runtime 内部 goroutine 稳定
	{
		c := &mockSipClient{responses: []mockResponse{{statusCode: 200}}}
		s := NewRegistrationSession(c, newTestConfig())
		_ = s.Start(context.Background())
		_ = s.Stop()
	}
	runtime.GC()
	time.Sleep(50 * time.Millisecond)

	before := runtime.NumGoroutine()

	for i := 0; i < 100; i++ {
		client := &mockSipClient{
			responses: []mockResponse{{statusCode: 200}},
		}
		session := NewRegistrationSession(client, newTestConfig())
		if err := session.Start(context.Background()); err != nil {
			t.Fatalf("Start #%d: %v", i, err)
		}
		if err := session.Stop(); err != nil {
			t.Fatalf("Stop #%d: %v", i, err)
		}
	}

	runtime.GC()
	time.Sleep(100 * time.Millisecond)

	after := runtime.NumGoroutine()
	// 允许 2 个偏差 (GC / test framework)
	if diff := after - before; diff > 2 {
		t.Errorf("goroutine leak: before=%d after=%d diff=%d", before, after, diff)
	}
}

// Start 之前 InternalContext() 应返回 nil (未启动)。
func TestSession_InternalContext_BeforeStart(t *testing.T) {
	client := &mockSipClient{}
	session := NewRegistrationSession(client, newTestConfig())
	if session.InternalContext() != nil {
		t.Error("InternalContext should be nil before Start")
	}
}

// Start 失败后 InternalContext 也应 cancel (状态 Failed)。
func TestSession_InternalContext_CancelledOnFailure(t *testing.T) {
	client := &mockSipClient{
		responses: []mockResponse{{statusCode: 403, reason: "Forbidden"}},
	}
	session := NewRegistrationSession(client, newTestConfig())
	_ = session.Start(context.Background())
	if session.State() != StateFailed {
		t.Fatalf("state = %v, want Failed", session.State())
	}
	if ctx := session.InternalContext(); ctx != nil {
		// 失败后 ctx 应 cancel (heartbeat/renewal 若已 spawn 也要退)
		select {
		case <-ctx.Done():
			// ok
		case <-time.After(100 * time.Millisecond):
			t.Error("internal ctx should be cancelled after Start failure")
		}
	}
}
