package ipc

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sync"
)

// ErrMethodNotFound 表示 method 未注册。上层用 errors.Is 判定后回 -32601。
var ErrMethodNotFound = errors.New("ipc: method not found")

// Handler 是 JSON-RPC method 的处理函数签名。
//
// params 是原始 JSON 片段(可能为 nil / null / 空对象),handler 内部按需 Unmarshal。
// 保留 RawMessage 而非 map[string]any 是为了让 handler 拿静态类型 struct(强校验字段)。
type Handler func(ctx context.Context, params json.RawMessage) (result any, err error)

// Router 是 method 名 → Handler 的注册表。并发安全。
//
// Register 通常在启动时集中调用一次,Dispatch 在 stdin goroutine 每帧一次。
// 用 RWMutex 保护:Dispatch 期间允许并发注册(虽然实际不会),读多写极少。
type Router struct {
	mu       sync.RWMutex
	handlers map[string]Handler
}

// NewRouter 建空路由表。
func NewRouter() *Router {
	return &Router{handlers: make(map[string]Handler)}
}

// Register 登记 handler。重复注册直接覆盖(方便测试 mock)。
func (r *Router) Register(method string, h Handler) {
	if method == "" {
		panic("ipc: Register with empty method")
	}
	if h == nil {
		panic("ipc: Register nil handler for " + method)
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	r.handlers[method] = h
}

// Dispatch 派发一次 method 调用。未注册返回 ErrMethodNotFound。
//
// handler panic 会被 recover 转成 error,避免整个 daemon 因单条命令挂掉。
func (r *Router) Dispatch(ctx context.Context, method string, params json.RawMessage) (result any, err error) {
	r.mu.RLock()
	h, ok := r.handlers[method]
	r.mu.RUnlock()
	if !ok {
		return nil, fmt.Errorf("%w: %s", ErrMethodNotFound, method)
	}
	defer func() {
		if p := recover(); p != nil {
			err = fmt.Errorf("ipc: handler %q panicked: %v", method, p)
		}
	}()
	return h(ctx, params)
}

// Has 判断 method 是否已注册(测试与诊断用)。
func (r *Router) Has(method string) bool {
	r.mu.RLock()
	defer r.mu.RUnlock()
	_, ok := r.handlers[method]
	return ok
}

// RegisterBuiltins 注册所有 daemon 内建 method(目前只有 ping)。
//
// 业务 method (start_device / stop_device / get_device_status) 由 T3 单独注册,
// 便于 main.go 在启动时组装依赖(session manager)后再挂钩。
func RegisterBuiltins(r *Router) {
	r.Register("ping", func(ctx context.Context, params json.RawMessage) (any, error) {
		return map[string]any{"pong": true}, nil
	})
}
