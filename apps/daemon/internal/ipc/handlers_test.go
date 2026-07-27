package ipc

import (
	"context"
	"encoding/json"
	"errors"
	"sync"
	"testing"
	"time"

	"github.com/emiago/sipgo/sip"
)

// --- test fakes ---

// fakeSession 模拟 device.RegistrationSession 的最小接口。
// state / stopped 用 mutex 保护, State() 常在主 goroutine 读, Start()/Stop() 在 handler goroutine 写。
type fakeSession struct {
	startErr error
	expires  int

	startedCh chan struct{}

	mu      sync.Mutex
	state   string
	stopped bool
}

func (s *fakeSession) Start(ctx context.Context) error {
	if s.startedCh != nil {
		close(s.startedCh)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.startErr != nil {
		s.state = "Failed"
		return s.startErr
	}
	s.state = "Registered"
	return nil
}

func (s *fakeSession) Stop() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.stopped = true
	s.state = "Disconnected"
	return nil
}

func (s *fakeSession) State() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.state
}
func (s *fakeSession) RegisteredExpires() int { return s.expires }
func (s *fakeSession) wasStopped() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.stopped
}

// fakeSipClient 满足 device.SipClient(实际不会被调,只是让 buildRegisterRequest 能过)。
type fakeSipClient struct{}

func (fakeSipClient) Do(ctx context.Context, req *sip.Request) (*sip.Response, error) {
	return nil, errors.New("fakeSipClient not used")
}

// --- handler 测试 ---

func TestStartDevice_HappyPath(t *testing.T) {
	dm := NewDeviceManager()
	// 注入 fake:handler 不真调 sip 层。
	fs := &fakeSession{expires: 3600, startedCh: make(chan struct{})}
	dm.SetFactory(func(cfg StartDeviceParams) (Session, error) {
		return fs, nil
	})

	r := NewRouter()
	dm.RegisterHandlers(r, nil) // 无 publisher, 状态事件走不了但不影响返回

	params, _ := json.Marshal(StartDeviceParams{
		DeviceID:     "35020000001320000001",
		ServerHost:   "127.0.0.1",
		ServerPort:   5060,
		ServerID:     "34020000002000000001",
		ServerDomain: "3402000000",
		Password:     "secret",
		Transport:    "udp",
	})
	result, err := r.Dispatch(context.Background(), "start_device", params)
	if err != nil {
		t.Fatalf("start_device: %v", err)
	}
	m := result.(map[string]any)
	if m["request_id"] == "" {
		t.Errorf("expected request_id, got %v", m)
	}
	// 等异步 goroutine 启动
	select {
	case <-fs.startedCh:
	case <-time.After(1 * time.Second):
		t.Fatalf("session.Start not invoked within 1s")
	}
}

func TestStartDevice_AlreadyRunning(t *testing.T) {
	dm := NewDeviceManager()
	dm.SetFactory(func(cfg StartDeviceParams) (Session, error) {
		return &fakeSession{startedCh: make(chan struct{}, 1)}, nil
	})
	r := NewRouter()
	dm.RegisterHandlers(r, nil)

	params, _ := json.Marshal(StartDeviceParams{
		DeviceID: "35020000001320000001", ServerHost: "1.2.3.4", ServerPort: 5060,
		ServerDomain: "3402000000", Password: "x", Transport: "udp",
	})
	// 第一次成功
	if _, err := r.Dispatch(context.Background(), "start_device", params); err != nil {
		t.Fatalf("first: %v", err)
	}
	// 第二次应报"已在跑"
	_, err := r.Dispatch(context.Background(), "start_device", params)
	if err == nil {
		t.Fatalf("expected error on second start_device")
	}
	if !errors.Is(err, ErrDeviceAlreadyStarted) {
		t.Errorf("err = %v, want ErrDeviceAlreadyStarted", err)
	}
}

func TestStartDevice_InvalidParams(t *testing.T) {
	dm := NewDeviceManager()
	// 不 SetFactory:走生产路径, 校验 params.Validate 拒非法输入。
	r := NewRouter()
	dm.RegisterHandlers(r, nil)

	// device_id 只有 19 位
	params, _ := json.Marshal(map[string]any{
		"device_id":     "3502000000132000000", // 19 位
		"server_host":   "1.2.3.4",
		"server_port":   5060,
		"server_domain": "3402000000",
		"password":      "x",
	})
	_, err := r.Dispatch(context.Background(), "start_device", params)
	if err == nil {
		t.Fatalf("expected validation error")
	}
}

func TestStopDevice_Idempotent(t *testing.T) {
	dm := NewDeviceManager()
	fs := &fakeSession{}
	dm.SetFactory(func(cfg StartDeviceParams) (Session, error) { return fs, nil })
	r := NewRouter()
	dm.RegisterHandlers(r, nil)

	// 先 start
	params, _ := json.Marshal(StartDeviceParams{
		DeviceID: "35020000001320000001", ServerHost: "1.2.3.4", ServerPort: 5060,
		ServerDomain: "3402000000", Password: "x", Transport: "udp",
	})
	_, _ = r.Dispatch(context.Background(), "start_device", params)

	// stop 应该成功
	if _, err := r.Dispatch(context.Background(), "stop_device", nil); err != nil {
		t.Fatalf("stop_device 1: %v", err)
	}
	// 再 stop 无害(幂等)
	if _, err := r.Dispatch(context.Background(), "stop_device", nil); err != nil {
		t.Fatalf("stop_device 2: %v", err)
	}
	if !fs.wasStopped() {
		t.Errorf("session.Stop 未被调")
	}
}

func TestGetDeviceStatus_Uninitialized(t *testing.T) {
	dm := NewDeviceManager()
	r := NewRouter()
	dm.RegisterHandlers(r, nil)
	result, err := r.Dispatch(context.Background(), "get_device_status", nil)
	if err != nil {
		t.Fatalf("get_device_status: %v", err)
	}
	m := result.(map[string]any)
	if m["state"] != "Disconnected" {
		t.Errorf("state = %v, want Disconnected", m["state"])
	}
	if m["registered_expires_secs"] != 0 {
		t.Errorf("registered_expires_secs = %v, want 0", m["registered_expires_secs"])
	}
}

func TestGetDeviceStatus_AfterStart(t *testing.T) {
	dm := NewDeviceManager()
	fs := &fakeSession{expires: 1800, startedCh: make(chan struct{})}
	dm.SetFactory(func(cfg StartDeviceParams) (Session, error) { return fs, nil })
	r := NewRouter()
	dm.RegisterHandlers(r, nil)

	params, _ := json.Marshal(StartDeviceParams{
		DeviceID: "35020000001320000001", ServerHost: "1.2.3.4", ServerPort: 5060,
		ServerDomain: "3402000000", Password: "x", Transport: "udp",
	})
	_, _ = r.Dispatch(context.Background(), "start_device", params)
	// 等 Start 跑完
	<-fs.startedCh
	// 给一点点时间让 state 落地
	deadline := time.Now().Add(500 * time.Millisecond)
	for time.Now().Before(deadline) {
		if fs.State() == "Registered" {
			break
		}
		time.Sleep(5 * time.Millisecond)
	}

	result, _ := r.Dispatch(context.Background(), "get_device_status", nil)
	m := result.(map[string]any)
	if m["state"] != "Registered" {
		t.Errorf("state = %v, want Registered", m["state"])
	}
	if m["registered_expires_secs"] != 1800 {
		t.Errorf("expires = %v, want 1800", m["registered_expires_secs"])
	}
}

func TestStartDevice_PublishesStateChange(t *testing.T) {
	dm := NewDeviceManager()
	fs := &fakeSession{expires: 3600, startedCh: make(chan struct{})}
	dm.SetFactory(func(cfg StartDeviceParams) (Session, error) { return fs, nil })

	r := NewRouter()
	// mock publisher: 收集 device_state 事件(mutex 保护跨 goroutine append/read)
	var mu sync.Mutex
	var events []map[string]any
	publisher := PublishFunc(func(method string, payload map[string]any, priority bool) {
		if method == "device_state" {
			mu.Lock()
			events = append(events, payload)
			mu.Unlock()
		}
	})
	dm.RegisterHandlers(r, publisher)

	params, _ := json.Marshal(StartDeviceParams{
		DeviceID: "35020000001320000001", ServerHost: "1.2.3.4", ServerPort: 5060,
		ServerDomain: "3402000000", Password: "x", Transport: "udp",
	})
	_, _ = r.Dispatch(context.Background(), "start_device", params)

	<-fs.startedCh
	// 等 async 完成 publish
	deadline := time.Now().Add(500 * time.Millisecond)
	for time.Now().Before(deadline) {
		mu.Lock()
		n := len(events)
		mu.Unlock()
		if n >= 2 { // 至少 Registering 和 Registered
			break
		}
		time.Sleep(5 * time.Millisecond)
	}

	mu.Lock()
	defer mu.Unlock()
	if len(events) < 2 {
		t.Fatalf("expected >=2 device_state events, got %d: %+v", len(events), events)
	}
	if events[0]["state"] != "Registering" {
		t.Errorf("first event state = %v, want Registering", events[0]["state"])
	}
	if events[len(events)-1]["state"] != "Registered" {
		t.Errorf("last event state = %v, want Registered", events[len(events)-1]["state"])
	}
}
