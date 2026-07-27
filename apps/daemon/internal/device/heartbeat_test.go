package device

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/emiago/sipgo/sip"
)

// heartbeatFakeClient 独立于 mockSipClient:
//   - 按 method 分开路由响应
//   - 支持"前 N 次失败"脚本
//   - 记录调用 method / body / cseq / call-id
type heartbeatFakeClient struct {
	mu               sync.Mutex
	calls            []*sip.Request
	registerResp     mockResponse
	messageResp      mockResponse
	messageErr       error
	messageFailFirst int32 // 前 N 次 MESSAGE 返回错误
	messageOverrideN atomic.Int32
}

func (c *heartbeatFakeClient) Do(ctx context.Context, req *sip.Request) (*sip.Response, error) {
	c.mu.Lock()
	c.calls = append(c.calls, req)
	c.mu.Unlock()

	switch req.Method {
	case sip.REGISTER:
		return newMockResp(req, c.registerResp), nil
	case sip.MESSAGE:
		// 前 messageFailFirst 次失败
		n := c.messageOverrideN.Add(1)
		if int32(c.messageFailFirst) > 0 && n <= int32(c.messageFailFirst) {
			return nil, fmt.Errorf("mock heartbeat send fail #%d", n)
		}
		if c.messageErr != nil {
			return nil, c.messageErr
		}
		return newMockResp(req, c.messageResp), nil
	default:
		return nil, fmt.Errorf("unexpected method: %s", req.Method)
	}
}

func (c *heartbeatFakeClient) messageCount() int {
	c.mu.Lock()
	defer c.mu.Unlock()
	n := 0
	for _, r := range c.calls {
		if r.Method == sip.MESSAGE {
			n++
		}
	}
	return n
}

func (c *heartbeatFakeClient) messages() []*sip.Request {
	c.mu.Lock()
	defer c.mu.Unlock()
	var out []*sip.Request
	for _, r := range c.calls {
		if r.Method == sip.MESSAGE {
			out = append(out, r)
		}
	}
	return out
}

func newMockResp(req *sip.Request, spec mockResponse) *sip.Response {
	return sip.NewResponseFromRequest(req, spec.statusCode, spec.reason, nil)
}

// captureBus 记录 device 层发布的事件,断言用。
type captureBus struct {
	mu     sync.Mutex
	events []capturedEvent
}

type capturedEvent struct {
	Method  string
	Payload map[string]any
}

func (b *captureBus) Publish(method string, payload map[string]any, priority bool) {
	b.mu.Lock()
	defer b.mu.Unlock()
	// clone payload 避免调用方后续修改
	cp := make(map[string]any, len(payload))
	for k, v := range payload {
		cp[k] = v
	}
	b.events = append(b.events, capturedEvent{Method: method, Payload: cp})
}

func (b *captureBus) find(method string) []capturedEvent {
	b.mu.Lock()
	defer b.mu.Unlock()
	var out []capturedEvent
	for _, e := range b.events {
		if e.Method == method {
			out = append(out, e)
		}
	}
	return out
}

func (b *captureBus) count(method string) int {
	return len(b.find(method))
}

// TestHeartbeat_TickerSendsKeepalive: 短 interval 下 tick 应产生 MESSAGE 报文。
func TestHeartbeat_TickerSendsKeepalive(t *testing.T) {
	client := &heartbeatFakeClient{
		registerResp: mockResponse{statusCode: 200},
		messageResp:  mockResponse{statusCode: 200},
	}
	session := NewRegistrationSession(client, newTestConfig())
	bus := &captureBus{}
	session.SetPublisher(bus)

	if err := session.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	hb := NewHeartbeat(session, 30*time.Millisecond)
	ctx, cancel := context.WithCancel(session.InternalContext())
	defer cancel()

	done := make(chan struct{})
	go func() { hb.Run(ctx); close(done) }()

	time.Sleep(100 * time.Millisecond)
	cancel()
	<-done

	if n := client.messageCount(); n < 2 {
		t.Errorf("expected at least 2 MESSAGE within 100ms/30ms tick, got %d", n)
	}
	// heartbeat_result ok=true 应至少一条
	if bus.count("heartbeat_result") < 2 {
		t.Errorf("expected at least 2 heartbeat_result events, got %d", bus.count("heartbeat_result"))
	}
}

// TestHeartbeat_KeepaliveXMLBody: 生成的 MESSAGE 应带 MANSCDP Keepalive body。
func TestHeartbeat_KeepaliveXMLBody(t *testing.T) {
	client := &heartbeatFakeClient{
		registerResp: mockResponse{statusCode: 200},
		messageResp:  mockResponse{statusCode: 200},
	}
	session := NewRegistrationSession(client, newTestConfig())
	bus := &captureBus{}
	session.SetPublisher(bus)
	_ = session.Start(context.Background())

	hb := NewHeartbeat(session, 20*time.Millisecond)
	ctx, cancel := context.WithCancel(session.InternalContext())
	defer cancel()

	done := make(chan struct{})
	go func() { hb.Run(ctx); close(done) }()
	time.Sleep(60 * time.Millisecond)
	cancel()
	<-done

	msgs := client.messages()
	if len(msgs) == 0 {
		t.Fatal("no MESSAGE sent")
	}
	body := string(msgs[0].Body())
	// GB18030 编码,ASCII 部分保持,可直接 substring
	if !strings.Contains(body, "Keepalive") {
		t.Errorf("body missing Keepalive: %q", body)
	}
	if !strings.Contains(body, session.cfg.DeviceID) {
		t.Errorf("body missing DeviceID: %q", body)
	}
	// Content-Type 头
	ct := msgs[0].GetHeader("Content-Type")
	if ct == nil || !strings.Contains(strings.ToLower(ct.Value()), "manscdp") {
		t.Errorf("Content-Type missing MANSCDP: %v", ct)
	}
	// SN 单调递增
	if len(msgs) >= 2 {
		body1 := string(msgs[0].Body())
		body2 := string(msgs[1].Body())
		if body1 == body2 {
			t.Error("consecutive Keepalive bodies identical, SN not incremented")
		}
	}
}

// TestHeartbeat_FailCountReset: 1 次失败后 1 次成功,failCount 归零 (不进入降级)。
func TestHeartbeat_FailCountReset(t *testing.T) {
	client := &heartbeatFakeClient{
		registerResp:     mockResponse{statusCode: 200},
		messageResp:      mockResponse{statusCode: 200},
		messageFailFirst: 1, // 只失败 1 次
	}
	session := NewRegistrationSession(client, newTestConfig())
	bus := &captureBus{}
	session.SetPublisher(bus)
	_ = session.Start(context.Background())

	hb := NewHeartbeat(session, 20*time.Millisecond)
	ctx, cancel := context.WithCancel(session.InternalContext())
	defer cancel()

	done := make(chan struct{})
	go func() { hb.Run(ctx); close(done) }()
	time.Sleep(200 * time.Millisecond) // 应至少 8-10 次 tick
	cancel()
	<-done

	if session.State() != StateRegistered {
		t.Errorf("session state = %v after 1 fail + N success, want Registered", session.State())
	}
	// consecutive_fails 应最终归零
	events := bus.find("heartbeat_result")
	if len(events) < 3 {
		t.Fatalf("too few heartbeat events: %d", len(events))
	}
	last := events[len(events)-1]
	if ok, _ := last.Payload["ok"].(bool); !ok {
		t.Errorf("last event should be ok=true, got %v", last.Payload)
	}
	if cf, _ := last.Payload["consecutive_fails"].(int); cf != 0 {
		t.Errorf("last consecutive_fails = %d, want 0", cf)
	}
}

// TestHeartbeat_ThreeConsecutiveFailsDown: 连续 3 次失败降级 Failed + emit failure reason。
func TestHeartbeat_ThreeConsecutiveFailsDown(t *testing.T) {
	client := &heartbeatFakeClient{
		registerResp: mockResponse{statusCode: 200},
		messageErr:   errors.New("mock permanent fail"),
	}
	session := NewRegistrationSession(client, newTestConfig())
	bus := &captureBus{}
	session.SetPublisher(bus)
	_ = session.Start(context.Background())

	hb := NewHeartbeat(session, 20*time.Millisecond)
	ctx, cancel := context.WithCancel(session.InternalContext())
	defer cancel()

	done := make(chan struct{})
	go func() { hb.Run(ctx); close(done) }()

	// 等到 session 状态转 Failed 或超时
	deadline := time.Now().Add(500 * time.Millisecond)
	for time.Now().Before(deadline) {
		if session.State() == StateFailed {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}
	cancel()
	<-done

	if session.State() != StateFailed {
		t.Errorf("session state = %v after 3+ fails, want Failed", session.State())
	}
	// heartbeat_result ok=false 应至少 3 条
	fails := 0
	for _, e := range bus.find("heartbeat_result") {
		if ok, _ := e.Payload["ok"].(bool); !ok {
			fails++
		}
	}
	if fails < 3 {
		t.Errorf("expected at least 3 failed heartbeat_result events, got %d", fails)
	}
}

// TestHeartbeat_CtxCancelExits: ctx cancel 立即退出 goroutine。
func TestHeartbeat_CtxCancelExits(t *testing.T) {
	client := &heartbeatFakeClient{
		registerResp: mockResponse{statusCode: 200},
		messageResp:  mockResponse{statusCode: 200},
	}
	session := NewRegistrationSession(client, newTestConfig())
	_ = session.Start(context.Background())

	// interval 长到不会触发,靠 cancel 退出
	hb := NewHeartbeat(session, time.Hour)
	ctx, cancel := context.WithCancel(session.InternalContext())

	done := make(chan struct{})
	go func() { hb.Run(ctx); close(done) }()

	// 快速 cancel
	time.Sleep(10 * time.Millisecond)
	cancel()

	select {
	case <-done:
		// ok
	case <-time.After(200 * time.Millisecond):
		t.Fatal("Heartbeat.Run did not exit within 200ms of ctx.cancel()")
	}
}

// TestHeartbeat_SessionStopCancels: session.Stop() 应能停下 heartbeat。
func TestHeartbeat_SessionStopCancels(t *testing.T) {
	client := &heartbeatFakeClient{
		registerResp: mockResponse{statusCode: 200},
		messageResp:  mockResponse{statusCode: 200},
	}
	session := NewRegistrationSession(client, newTestConfig())
	_ = session.Start(context.Background())

	hb := NewHeartbeat(session, 30*time.Millisecond)
	done := make(chan struct{})
	go func() { hb.Run(session.InternalContext()); close(done) }()

	time.Sleep(50 * time.Millisecond)
	_ = session.Stop()

	select {
	case <-done:
		// ok
	case <-time.After(300 * time.Millisecond):
		t.Fatal("Heartbeat did not stop when session.Stop() called")
	}
}
