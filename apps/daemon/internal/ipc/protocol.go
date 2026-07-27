// Package ipc 提供 daemon 与 Tauri 前端之间的 JSON-RPC 2.0 stdio IPC。
//
// 帧格式(plan §2.2):
//   请求  {"jsonrpc":"2.0","method":"start_device","params":{...},"id":1}
//   响应  {"jsonrpc":"2.0","id":1,"result":{...}}
//   错误  {"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"..."}}
//   事件  {"jsonrpc":"2.0","method":"device_state","params":{...}}   (无 id)
//
// stdin 逐行读入(每行一个完整帧),stdout 逐行写出。
// slog 走 stderr,不污染 stdout。
package ipc

import (
	"encoding/json"
	"fmt"
)

// Request 是一条 JSON-RPC 请求帧(反序列化后)。
//
// ID 保留成 json.RawMessage 原样透传:JSON-RPC 2.0 允许 id 是 string / number / null,
// 用 RawMessage 避免反序列化把 int 变 float64 再序列化回去时误差(id="1" vs id=1)。
type Request struct {
	JSONRPC string          `json:"jsonrpc"`
	Method  string          `json:"method"`
	Params  json.RawMessage `json:"params,omitempty"`
	ID      json.RawMessage `json:"id,omitempty"`
}

// IsNotification 判断此帧是否为 Notification(§4.1: no id field)。
//
// JSON-RPC 2.0 §4.1: 缺 id 字段视为 notification;id=null 也按 notification 处理
// (§5 说 notification 不能回响应,故不应发含 id=null 的请求;为兼容非严格实现,
// 我们同时把 id=null 视为 notification,不发响应,避免让不理解 null id 的对端困惑)。
func (r *Request) IsNotification() bool {
	return len(r.ID) == 0 || string(r.ID) == "null"
}

// DecodeRequest 解一行 JSON-RPC 请求。返回的 Request 里 ID 保留原始 RawMessage。
//
// 不校验 jsonrpc == "2.0" — 让上层决定是否严格拒绝,保留向前兼容余地。
func DecodeRequest(line []byte) (*Request, error) {
	var req Request
	if err := json.Unmarshal(line, &req); err != nil {
		return nil, fmt.Errorf("ipc: parse request frame: %w", err)
	}
	return &req, nil
}

// EncodeResponse 编码成功响应。id 原样透传,result 序列化时按 encoding/json 规则。
//
// 返回的字节不含换行,由调用方负责按 JSON-lines 加 "\n"。
func EncodeResponse(id json.RawMessage, result any) []byte {
	if id == nil {
		id = json.RawMessage("null")
	}
	resp := struct {
		JSONRPC string          `json:"jsonrpc"`
		ID      json.RawMessage `json:"id"`
		Result  any             `json:"result"`
	}{"2.0", id, result}
	b, err := json.Marshal(resp)
	if err != nil {
		// result 含 unmarshalable 类型(如 chan/func) — 极罕见,降级为 error 响应。
		return EncodeError(id, -32603, "internal error: marshal result: "+err.Error())
	}
	return b
}

// EncodeError 编码错误响应。id 为 nil 时序列化成 null(JSON-RPC 2.0 §5.1)。
func EncodeError(id json.RawMessage, code int, msg string) []byte {
	if id == nil {
		id = json.RawMessage("null")
	}
	resp := struct {
		JSONRPC string          `json:"jsonrpc"`
		ID      json.RawMessage `json:"id"`
		Error   errorPayload    `json:"error"`
	}{"2.0", id, errorPayload{Code: code, Message: msg}}
	b, _ := json.Marshal(resp) // struct 全为原生类型,不会失败
	return b
}

type errorPayload struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

// EncodeNotification 编码事件(server → client push)。
//
// 无 id 字段(JSON-RPC 2.0 §4.1)。params 可为 nil(不带 params 字段)。
func EncodeNotification(method string, params any) []byte {
	// 手工构造 map 是为了当 params 为 nil 时不带 params 字段(而不是带一个 "params":null)。
	obj := map[string]any{
		"jsonrpc": "2.0",
		"method":  method,
	}
	if params != nil {
		obj["params"] = params
	}
	b, err := json.Marshal(obj)
	if err != nil {
		// 兜底:params 无法序列化时降级发一条 diagnostic notification。
		fallback, _ := json.Marshal(map[string]any{
			"jsonrpc": "2.0",
			"method":  "internal_error",
			"params": map[string]any{
				"reason":   "marshal_failed",
				"detail":   err.Error(),
				"of_event": method,
			},
		})
		return fallback
	}
	return b
}
