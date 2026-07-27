package ipc

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"strings"
	"sync"
	"time"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/internal/gb28181"
)

// ErrDeviceAlreadyStarted 已有活跃 session,再调 start_device 报此错。
// JSON-RPC 层映射到 -32001 (业务错)。
var ErrDeviceAlreadyStarted = errors.New("device already started")

// Session 是 handler 使用的 device.RegistrationSession 最小接口。
//
// 抽出接口是为了让 handler_test.go 用 fakeSession 走测试路径,不拉起真 UDP。
// 生产实现由 main.go 用 device.RegistrationSession 满足(RegistrationSession
// 的方法签名与此接口一致:Start(ctx) error / Stop() error / State() State /
// RegisteredExpires() int)—— main.go 会用小 adapter 把 State() State 转 string。
type Session interface {
	Start(ctx context.Context) error
	Stop() error
	State() string
	RegisteredExpires() int
}

// SessionFactory 生产 Session 实例。start_device handler 拿到 params 后调它建 session。
//
// 生产实现:main.go 在这里 new sip.Transport / sip.Client / device.RegistrationSession。
// 测试实现:直接返 fakeSession。
type SessionFactory func(params StartDeviceParams) (Session, error)

// Publisher 是 handler 推事件的接口(对齐 ipc.Server.Publish 签名)。
type Publisher interface {
	Publish(method string, payload map[string]any, priority bool)
}

// PublishFunc 让普通 func 满足 Publisher。
type PublishFunc func(method string, payload map[string]any, priority bool)

// Publish 实现 Publisher。
func (f PublishFunc) Publish(m string, p map[string]any, pr bool) { f(m, p, pr) }

// StartDeviceParams 是 start_device 请求参数(与 spec Q4 对齐)。
type StartDeviceParams struct {
	DeviceID     string `json:"device_id"`
	ServerHost   string `json:"server_host"`
	ServerPort   int    `json:"server_port"`
	ServerID     string `json:"server_id,omitempty"`
	ServerDomain string `json:"server_domain,omitempty"`
	Password     string `json:"password"`
	Transport    string `json:"transport,omitempty"`

	// 可选 tuning(默认由 gb28181.ApplyDefaults 填)
	HeartbeatIntervalSecs int `json:"heartbeat_interval_secs,omitempty"`
	ExpiresSecs           int `json:"expires_secs,omitempty"`
}

// ToSipConfig 转 gb28181.SipConfig 并校验。
//
// 集中校验点:handler 一开始就拒非法 payload,不进 sip 层。
func (p StartDeviceParams) ToSipConfig() (*gb28181.SipConfig, error) {
	cfg := &gb28181.SipConfig{
		DeviceID:              strings.TrimSpace(p.DeviceID),
		ServerHost:            strings.TrimSpace(p.ServerHost),
		ServerPort:            p.ServerPort,
		ServerID:              strings.TrimSpace(p.ServerID),
		ServerDomain:          strings.TrimSpace(p.ServerDomain),
		Password:              p.Password,
		Transport:             strings.ToLower(strings.TrimSpace(p.Transport)),
		HeartbeatIntervalSecs: p.HeartbeatIntervalSecs,
		ExpiresSecs:           p.ExpiresSecs,
	}
	if err := cfg.Validate(); err != nil {
		return nil, err
	}
	cfg.ApplyDefaults()
	return cfg, nil
}

// DeviceManager 持有单个活跃 session,并暴露 3 个 handler。
//
// 单实例约束:M2 阶段前端只有一个"设备模拟器"视图,同一时刻最多一个 session。
// M3+ 若做多设备管理(阵列压测)再改成 map[deviceID]Session。
type DeviceManager struct {
	mu      sync.Mutex
	session Session
	factory SessionFactory

	// 用来 async 启动 Start(ctx) 时的 ctx / cancel
	activeCancel context.CancelFunc
}

// NewDeviceManager 建一个未装 factory 的 manager。
// 生产路径必须调 SetFactory 挂 factory,否则 start_device 会返错。
func NewDeviceManager() *DeviceManager {
	return &DeviceManager{}
}

// SetFactory 设置 session 工厂。测试 / 生产各自注入。
func (d *DeviceManager) SetFactory(f SessionFactory) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.factory = f
}

// RegisterHandlers 把 start_device / stop_device / get_device_status 挂到 router。
// pub 可为 nil(测试路径不推事件时);生产由 main.go 传 ipc.Server。
func (d *DeviceManager) RegisterHandlers(r *Router, pub Publisher) {
	r.Register("start_device", d.startDevice(pub))
	r.Register("stop_device", d.stopDevice(pub))
	r.Register("get_device_status", d.getDeviceStatus)
}

// startDevice 返回 handler 闭包(闭包捕获 publisher, 每次调用共享)。
//
// 行为:
//   1. 校验 params → 建 session (通过 factory)
//   2. 立即 publish device_state=Registering(优先队列)
//   3. 起 goroutine 阻塞调 session.Start,成功 → device_state=Registered,失败 → =Failed
//   4. 立即返回 {request_id, started: true}
//
// 幂等:已有活跃 session → 返回 ErrDeviceAlreadyStarted (前端应先 stop_device)。
func (d *DeviceManager) startDevice(pub Publisher) Handler {
	return func(ctx context.Context, raw json.RawMessage) (any, error) {
		var params StartDeviceParams
		if len(raw) > 0 {
			if err := json.Unmarshal(raw, &params); err != nil {
				return nil, fmt.Errorf("start_device: parse params: %w", err)
			}
		}
		cfg, err := params.ToSipConfig()
		if err != nil {
			return nil, fmt.Errorf("start_device: validate: %w", err)
		}
		// 用规范化后的字段回填(测试路径 factory 可能读 params 里的字段)
		params.Transport = cfg.Transport
		params.HeartbeatIntervalSecs = cfg.HeartbeatIntervalSecs
		params.ExpiresSecs = cfg.ExpiresSecs

		d.mu.Lock()
		if d.session != nil {
			d.mu.Unlock()
			return nil, ErrDeviceAlreadyStarted
		}
		if d.factory == nil {
			d.mu.Unlock()
			return nil, errors.New("start_device: no session factory configured")
		}
		session, err := d.factory(params)
		if err != nil {
			d.mu.Unlock()
			return nil, fmt.Errorf("start_device: create session: %w", err)
		}
		d.session = session
		// 独立 ctx 让业务方 stopDevice / 进程 shutdown 都能打断长任务。
		sessCtx, cancel := context.WithCancel(context.Background())
		d.activeCancel = cancel
		d.mu.Unlock()

		reqID := newRequestID()

		// 立即广播 Registering(优先事件, 前端 UI 转"注册中")
		publishState(pub, "Registering", 0, "")

		// async 执行 Start,不阻塞 handler 返回
		go func() {
			// Start 内部会做 REGISTER → 401 → AUTH → 200,期间会通过 tracer emit
			// sip_trace 事件(如果 sipClient 挂了 tracer)。
			err := session.Start(sessCtx)
			// 拿当前 state (Start 内部已更新到 StateRegistered / Failed)
			state := session.State()
			var reason string
			if err != nil {
				reason = err.Error()
				slog.Warn("session.Start failed",
					"error", err, "device_id", cfg.DeviceID)
			}
			publishState(pub, state, session.RegisteredExpires(), reason)
		}()

		return map[string]any{
			"request_id": reqID,
			"started":    true,
		}, nil
	}
}

// stopDevice 停当前 session。
//
// 幂等:无 session 也不报错。
// M3: session.Stop() 现在会发 Expires=0 REGISTER (6s 超时),放到 goroutine 里跑,
// handler 立即返回避免阻塞 IPC。session 内部完成后会自己 publish device_state:Disconnected。
func (d *DeviceManager) stopDevice(pub Publisher) Handler {
	return func(ctx context.Context, raw json.RawMessage) (any, error) {
		d.mu.Lock()
		s := d.session
		cancel := d.activeCancel
		d.session = nil
		d.activeCancel = nil
		d.mu.Unlock()

		if s == nil {
			// 幂等
			publishState(pub, "Disconnected", 0, "")
			return map[string]any{"stopped": true}, nil
		}
		// async 走 session.Stop:内部含 6s 超时 + 发 Expires=0 REGISTER,不阻塞 handler。
		go func() {
			if err := s.Stop(); err != nil {
				slog.Warn("session.Stop error", "error", err)
			}
			if cancel != nil {
				cancel()
			}
			// session 会自己 publish device_state:Disconnected;这里补一发,保证 UI
			// 即便 session.Stop 里 publish 丢失也能收到最终态。
			publishState(pub, "Disconnected", 0, "")
		}()
		return map[string]any{"stopped": true}, nil
	}
}

// getDeviceStatus 返回当前 session 状态快照。无 session → Disconnected + 0。
func (d *DeviceManager) getDeviceStatus(ctx context.Context, raw json.RawMessage) (any, error) {
	d.mu.Lock()
	s := d.session
	d.mu.Unlock()
	if s == nil {
		return map[string]any{
			"state":                   "Disconnected",
			"registered_expires_secs": 0,
		}, nil
	}
	return map[string]any{
		"state":                   s.State(),
		"registered_expires_secs": s.RegisteredExpires(),
	}, nil
}

// publishState 是内部辅助:统一 device_state 事件 payload 结构。
// pub 为 nil 时静默丢弃(测试路径常见)。
func publishState(pub Publisher, state string, expiresSecs int, reason string) {
	if pub == nil {
		return
	}
	payload := map[string]any{
		"state":                   state,
		"registered_expires_secs": expiresSecs,
	}
	if reason != "" {
		payload["reason"] = reason
	}
	// device_state 是关键事件,永不丢:priority=true。
	pub.Publish("device_state", payload, true)
}

// newRequestID 生成一个短随机字符串(12 字节 → 24 hex),用于 start_device 返回值。
func newRequestID() string {
	b := make([]byte, 12)
	if _, err := rand.Read(b); err != nil {
		// crypto/rand 极罕见失败(OS 熵源) — 降级到时间戳。
		return fmt.Sprintf("req-%d", time.Now().UnixNano())
	}
	return hex.EncodeToString(b)
}
