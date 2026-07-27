package ipc

import (
	"bytes"
	"encoding/json"
	"testing"
)

// TestDecodeRequest_Basic 解 JSON-RPC 请求(method + params + id)。
func TestDecodeRequest_Basic(t *testing.T) {
	line := []byte(`{"jsonrpc":"2.0","method":"start_device","params":{"device_id":"x"},"id":7}`)
	req, err := DecodeRequest(line)
	if err != nil {
		t.Fatalf("decode: %v", err)
	}
	if req.JSONRPC != "2.0" {
		t.Errorf("jsonrpc = %q, want 2.0", req.JSONRPC)
	}
	if req.Method != "start_device" {
		t.Errorf("method = %q, want start_device", req.Method)
	}
	if string(req.ID) != "7" {
		t.Errorf("id = %s, want 7", req.ID)
	}
	if req.IsNotification() {
		t.Errorf("should not be notification")
	}
}

// TestDecodeRequest_Notification id 缺失即视为 notification。
func TestDecodeRequest_Notification(t *testing.T) {
	line := []byte(`{"jsonrpc":"2.0","method":"heartbeat_tick"}`)
	req, err := DecodeRequest(line)
	if err != nil {
		t.Fatalf("decode: %v", err)
	}
	if !req.IsNotification() {
		t.Errorf("expected notification, got id=%q", req.ID)
	}
}

// TestDecodeRequest_MalformedJSON 非法 JSON 返错。
func TestDecodeRequest_MalformedJSON(t *testing.T) {
	_, err := DecodeRequest([]byte(`not json`))
	if err == nil {
		t.Fatal("expected error, got nil")
	}
}

// TestEncodeResponse 响应格式对齐 JSON-RPC 2.0 §5。
func TestEncodeResponse(t *testing.T) {
	id := json.RawMessage("42")
	got := EncodeResponse(id, map[string]any{"ok": true})
	var parsed map[string]any
	if err := json.Unmarshal(got, &parsed); err != nil {
		t.Fatalf("bad JSON: %v", err)
	}
	if parsed["jsonrpc"] != "2.0" {
		t.Errorf("jsonrpc = %v", parsed["jsonrpc"])
	}
	if parsed["id"].(float64) != 42 {
		t.Errorf("id = %v", parsed["id"])
	}
	if !bytes.Contains(got, []byte(`"result"`)) {
		t.Errorf("expected result field, got %s", got)
	}
	if bytes.Contains(got, []byte(`"error"`)) {
		t.Errorf("response should not carry error field on success")
	}
}

// TestEncodeError 错误响应含 code + message,不含 result。
func TestEncodeError(t *testing.T) {
	id := json.RawMessage(`"abc"`)
	got := EncodeError(id, -32601, "method not found: xxx")
	var parsed map[string]any
	if err := json.Unmarshal(got, &parsed); err != nil {
		t.Fatalf("bad JSON: %v", err)
	}
	if parsed["id"] != "abc" {
		t.Errorf("id = %v", parsed["id"])
	}
	if bytes.Contains(got, []byte(`"result"`)) {
		t.Errorf("error response should not carry result field")
	}
	e := parsed["error"].(map[string]any)
	if e["code"].(float64) != -32601 {
		t.Errorf("code = %v", e["code"])
	}
	if e["message"] != "method not found: xxx" {
		t.Errorf("message = %v", e["message"])
	}
}

// TestEncodeError_NilIDBecomesNull id 为 nil 时序列化为 null (JSON-RPC 2.0 §5.1)。
func TestEncodeError_NilIDBecomesNull(t *testing.T) {
	got := EncodeError(nil, -32700, "parse error")
	var parsed map[string]any
	if err := json.Unmarshal(got, &parsed); err != nil {
		t.Fatalf("bad JSON: %v", err)
	}
	if parsed["id"] != nil {
		t.Errorf("nil id should be null, got %v", parsed["id"])
	}
}

// TestEncodeNotification method + params, 无 id 字段。
func TestEncodeNotification(t *testing.T) {
	got := EncodeNotification("device_state", map[string]any{"state": "Registered"})
	var parsed map[string]any
	if err := json.Unmarshal(got, &parsed); err != nil {
		t.Fatalf("bad JSON: %v", err)
	}
	if parsed["method"] != "device_state" {
		t.Errorf("method = %v", parsed["method"])
	}
	if _, hasID := parsed["id"]; hasID {
		t.Errorf("notification must not contain id, got %v", parsed)
	}
	if parsed["jsonrpc"] != "2.0" {
		t.Errorf("jsonrpc = %v", parsed["jsonrpc"])
	}
}
