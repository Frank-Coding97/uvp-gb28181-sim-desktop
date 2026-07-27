package ipc

import (
	"context"
	"encoding/json"
	"errors"
	"testing"
)

// TestRouter_DispatchRegistered 注册 handler 后能被派发。
func TestRouter_DispatchRegistered(t *testing.T) {
	r := NewRouter()
	r.Register("echo", func(ctx context.Context, params json.RawMessage) (any, error) {
		return map[string]any{"got": string(params)}, nil
	})
	got, err := r.Dispatch(context.Background(), "echo", json.RawMessage(`"hi"`))
	if err != nil {
		t.Fatalf("dispatch: %v", err)
	}
	m := got.(map[string]any)
	if m["got"] != `"hi"` {
		t.Errorf("params passthrough failed: %v", m)
	}
}

// TestRouter_DispatchUnknown 未注册 method 返回 ErrMethodNotFound。
func TestRouter_DispatchUnknown(t *testing.T) {
	r := NewRouter()
	_, err := r.Dispatch(context.Background(), "no_such", nil)
	if !errors.Is(err, ErrMethodNotFound) {
		t.Errorf("err = %v, want ErrMethodNotFound", err)
	}
}

// TestRouter_HandlerErrorBubblesUp handler 返错原样传出。
func TestRouter_HandlerErrorBubblesUp(t *testing.T) {
	sentinel := errors.New("boom")
	r := NewRouter()
	r.Register("bad", func(ctx context.Context, params json.RawMessage) (any, error) {
		return nil, sentinel
	})
	_, err := r.Dispatch(context.Background(), "bad", nil)
	if !errors.Is(err, sentinel) {
		t.Errorf("err = %v, want boom", err)
	}
}

// TestRouter_BuiltinPing 内建 ping handler 返回 {"pong": true}。
func TestRouter_BuiltinPing(t *testing.T) {
	r := NewRouter()
	RegisterBuiltins(r)
	got, err := r.Dispatch(context.Background(), "ping", nil)
	if err != nil {
		t.Fatalf("ping: %v", err)
	}
	m := got.(map[string]any)
	if m["pong"] != true {
		t.Errorf("pong = %v", m["pong"])
	}
}
