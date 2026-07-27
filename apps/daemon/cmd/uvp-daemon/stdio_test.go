package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"strings"
	"testing"
	"time"
)

// TestStdioPingPong 验证 --stdio 骨架能响应 JSON-RPC ping。
//
// M2 T0 验收:echo 一行 JSON-RPC 请求进 stdin,能从 stdout 收到对应 response,
// 且 stdout 严格按 JSON-lines(每行一个完整 JSON 对象)。
func TestStdioPingPong(t *testing.T) {
	in := strings.NewReader(`{"jsonrpc":"2.0","method":"ping","id":1}` + "\n")
	var out bytes.Buffer

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	err := runStdioLoop(ctx, in, &out)
	// EOF 是 stdin 关闭的正常退出信号(Tauri kill daemon 时 close stdin 走这里)。
	if err != nil && !errors.Is(err, io.EOF) {
		t.Fatalf("runStdioLoop returned unexpected error: %v", err)
	}

	line := strings.TrimSpace(out.String())
	if line == "" {
		t.Fatalf("stdout empty, expected response line")
	}
	var resp map[string]any
	if err := json.Unmarshal([]byte(line), &resp); err != nil {
		t.Fatalf("stdout not JSON: %v (raw=%q)", err, line)
	}
	if resp["jsonrpc"] != "2.0" {
		t.Errorf("jsonrpc field = %v, want 2.0", resp["jsonrpc"])
	}
	// JSON 反序列化 int → float64,兼容判断
	if id, ok := resp["id"].(float64); !ok || id != 1 {
		t.Errorf("id = %v, want 1", resp["id"])
	}
	result, ok := resp["result"].(map[string]any)
	if !ok {
		t.Fatalf("result missing or wrong type: %v", resp["result"])
	}
	if result["pong"] != true {
		t.Errorf("result.pong = %v, want true", result["pong"])
	}
}

// TestStdioUnknownMethod 未知 method 返回 -32601。
func TestStdioUnknownMethod(t *testing.T) {
	in := strings.NewReader(`{"jsonrpc":"2.0","method":"do_something_unknown","id":42}` + "\n")
	var out bytes.Buffer
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	err := runStdioLoop(ctx, in, &out)
	if err != nil && !errors.Is(err, io.EOF) {
		t.Fatalf("unexpected error: %v", err)
	}

	var resp map[string]any
	if err := json.Unmarshal(bytes.TrimSpace(out.Bytes()), &resp); err != nil {
		t.Fatalf("bad JSON: %v", err)
	}
	errObj, ok := resp["error"].(map[string]any)
	if !ok {
		t.Fatalf("expected error object, got %v", resp)
	}
	if code, _ := errObj["code"].(float64); code != -32601 {
		t.Errorf("error.code = %v, want -32601", errObj["code"])
	}
}

// TestStdioParseError 非法 JSON 返回 -32700 且 id=null。
func TestStdioParseError(t *testing.T) {
	in := strings.NewReader("this is not json\n")
	var out bytes.Buffer
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	err := runStdioLoop(ctx, in, &out)
	if err != nil && !errors.Is(err, io.EOF) {
		t.Fatalf("unexpected error: %v", err)
	}
	var resp map[string]any
	if err := json.Unmarshal(bytes.TrimSpace(out.Bytes()), &resp); err != nil {
		t.Fatalf("bad JSON: %v", err)
	}
	if resp["id"] != nil {
		t.Errorf("id on parse error = %v, want null", resp["id"])
	}
	errObj := resp["error"].(map[string]any)
	if errObj["code"].(float64) != -32700 {
		t.Errorf("code = %v, want -32700", errObj["code"])
	}
}

// TestStdioNotificationNoResponse 无 id (Notification) 不产生响应。
func TestStdioNotificationNoResponse(t *testing.T) {
	// 两条:一条 notification (无响应),一条正常 ping (有响应)
	in := strings.NewReader(
		`{"jsonrpc":"2.0","method":"ping"}` + "\n" +
			`{"jsonrpc":"2.0","method":"ping","id":9}` + "\n",
	)
	var out bytes.Buffer
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	_ = runStdioLoop(ctx, in, &out)

	// 应该只有 1 行响应。
	lines := strings.Split(strings.TrimSpace(out.String()), "\n")
	if len(lines) != 1 {
		t.Fatalf("expected 1 response line, got %d: %q", len(lines), out.String())
	}
	var resp map[string]any
	if err := json.Unmarshal([]byte(lines[0]), &resp); err != nil {
		t.Fatalf("bad JSON: %v", err)
	}
	if id, _ := resp["id"].(float64); id != 9 {
		t.Errorf("response for wrong id: %v", resp["id"])
	}
}
