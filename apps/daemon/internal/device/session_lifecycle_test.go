package device

import (
	"context"
	"runtime"
	"testing"
	"time"
)

// TestSession_HeartbeatRunsAfterRegister: Start 成功后应自动 spawn heartbeat + renewal。
func TestSession_HeartbeatRunsAfterRegister(t *testing.T) {
	client := &heartbeatFakeClient{
		registerResp: mockResponse{statusCode: 200},
		messageResp:  mockResponse{statusCode: 200},
	}
	cfg := newTestConfig()
	cfg.HeartbeatIntervalSecs = 0 // 走默认;实际用 SetHeartbeatInterval override
	session := NewRegistrationSession(client, cfg)
	bus := &captureBus{}
	session.SetPublisher(bus)

	// 用短心跳间隔便于测试
	session.SetHeartbeatInterval(30 * time.Millisecond)

	if err := session.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}
	defer session.Stop()

	// 等心跳跑几次
	time.Sleep(150 * time.Millisecond)

	if n := client.messageCount(); n < 2 {
		t.Errorf("expected >= 2 heartbeats within 150ms, got %d", n)
	}
	if bus.count("heartbeat_result") < 2 {
		t.Errorf("expected >= 2 heartbeat_result events, got %d", bus.count("heartbeat_result"))
	}
}

// TestSession_StopCancelsBoth: Stop 后 heartbeat + renewal 都退,goroutine 归零。
func TestSession_StopCancelsBoth(t *testing.T) {
	// 预热
	{
		c := &heartbeatFakeClient{
			registerResp: mockResponse{statusCode: 200},
			messageResp:  mockResponse{statusCode: 200},
		}
		s := NewRegistrationSession(c, newTestConfig())
		s.SetHeartbeatInterval(50 * time.Millisecond)
		_ = s.Start(context.Background())
		_ = s.Stop()
	}
	runtime.GC()
	time.Sleep(50 * time.Millisecond)

	before := runtime.NumGoroutine()

	// 20 次循环 (每次 spawn 2 goroutine),观察是否泄漏
	for i := 0; i < 20; i++ {
		client := &heartbeatFakeClient{
			registerResp: mockResponse{statusCode: 200},
			messageResp:  mockResponse{statusCode: 200},
		}
		session := NewRegistrationSession(client, newTestConfig())
		session.SetHeartbeatInterval(30 * time.Millisecond)
		if err := session.Start(context.Background()); err != nil {
			t.Fatalf("Start #%d: %v", i, err)
		}
		time.Sleep(30 * time.Millisecond)
		if err := session.Stop(); err != nil {
			t.Fatalf("Stop #%d: %v", i, err)
		}
	}

	runtime.GC()
	time.Sleep(150 * time.Millisecond)

	after := runtime.NumGoroutine()
	if diff := after - before; diff > 2 {
		t.Errorf("goroutine leak with heartbeat+renewal spawn: before=%d after=%d diff=%d",
			before, after, diff)
	}
}

// TestSession_HeartbeatIntervalFromConfig: 未调 SetHeartbeatInterval 时用 cfg.HeartbeatIntervalSecs。
func TestSession_HeartbeatIntervalFromConfig(t *testing.T) {
	client := &heartbeatFakeClient{
		registerResp: mockResponse{statusCode: 200},
		messageResp:  mockResponse{statusCode: 200},
	}
	cfg := newTestConfig()
	// 手工设 1s (最小合法值),测试用 SetHeartbeatInterval 覆盖为更短
	cfg.HeartbeatIntervalSecs = 1
	session := NewRegistrationSession(client, cfg)

	// 生产路径:不 Override,heartbeat 应按 1s 跑
	if err := session.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}
	defer session.Stop()

	// 1.5s 内应至少 1 次心跳
	time.Sleep(1500 * time.Millisecond)
	if n := client.messageCount(); n < 1 {
		t.Errorf("expected >= 1 heartbeat within 1.5s at 1s interval, got %d", n)
	}
}
