//go:build integration

package integration

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os/exec"
	"strings"
	"testing"
	"time"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/test/mocks"
)

// TestE2E_StdioMode_StartDevice 验证 M2 --stdio 模式完整链路:
//   1. 起 mock 平台 (raw UDP)
//   2. 起 daemon --stdio 子进程
//   3. stdin 发 start_device JSON-RPC 请求
//   4. 等 stdout 响应 {result: {request_id, started}}
//   5. 收 device_state=Registering / Registered 事件 (priority 通道)
//   6. 收 4 条 sip_trace 事件 (bulk 通道): >>> REGISTER / <<< 401 / >>> AUTH / <<< 200
//   7. 断言 mock 侧收到 2 条 REGISTER(无 Auth + 有 Auth)
func TestE2E_StdioMode_StartDevice(t *testing.T) {
	// 1. 起 mock 平台
	mock, err := mocks.StartRawMockPlatform(mocks.MockOpts{
		Realm:        "3402000000",
		Password:     "12345678",
		AuthAlgo:     "MD5",
		ExpiresReply: 3600,
	})
	if err != nil {
		t.Fatalf("start mock: %v", err)
	}
	defer mock.Stop()

	// 2. 编译 daemon + 起 --stdio
	daemonBin := buildDaemon(t)
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()

	cmd := exec.CommandContext(ctx, daemonBin, "--stdio")
	stdin, err := cmd.StdinPipe()
	if err != nil {
		t.Fatal(err)
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := cmd.Start(); err != nil {
		t.Fatalf("daemon start: %v", err)
	}
	defer func() {
		stdin.Close()
		_ = cmd.Wait()
	}()

	// 起 stderr reader (slog 走这里,保存供诊断)
	stderrBuf := make(chan string, 1)
	go func() {
		data, _ := io.ReadAll(stderr)
		stderrBuf <- string(data)
	}()

	// 3. 发 start_device JSON-RPC 请求
	reqPayload := map[string]any{
		"jsonrpc": "2.0",
		"method":  "start_device",
		"params": map[string]any{
			"device_id":     "34020000001320000001",
			"server_host":   mock.Host(),
			"server_port":   mock.Port(),
			"server_id":     "34020000002000000001",
			"server_domain": "3402000000",
			"password":      "12345678",
			"transport":     "udp",
		},
		"id": 1,
	}
	reqLine, _ := json.Marshal(reqPayload)
	reqLine = append(reqLine, '\n')
	if _, err := stdin.Write(reqLine); err != nil {
		t.Fatalf("write stdin: %v", err)
	}

	// 4. 读 stdout JSON-lines,收集响应 + 事件
	scanner := bufio.NewScanner(stdout)
	scanner.Buffer(make([]byte, 0, 64*1024), 1*1024*1024)

	var startResp map[string]any
	var deviceStates []string
	var traces []map[string]any
	deadline := time.Now().Add(15 * time.Second)

	for scanner.Scan() {
		if time.Now().After(deadline) {
			t.Fatalf("timeout waiting for events")
		}
		line := scanner.Text()
		if strings.TrimSpace(line) == "" {
			continue
		}
		var frame map[string]any
		if err := json.Unmarshal([]byte(line), &frame); err != nil {
			t.Logf("non-JSON line: %s", line)
			continue
		}

		// 响应 (id 字段存在)
		if idVal, ok := frame["id"]; ok && idVal != nil {
			startResp = frame
			t.Logf("got start_device response: %v", frame)
			continue
		}

		// 事件 (method 字段存在)
		if method, ok := frame["method"].(string); ok {
			switch method {
			case "device_state":
				params, _ := frame["params"].(map[string]any)
				state, _ := params["state"].(string)
				deviceStates = append(deviceStates, state)
				t.Logf("device_state: %s", state)
			case "sip_trace":
				params, _ := frame["params"].(map[string]any)
				traces = append(traces, params)
				t.Logf("sip_trace: dir=%v method=%v status=%v", params["direction"], params["method"], params["status_code"])
			}
		}

		// 退出条件:收到 Registered state + 至少 4 条 trace(>>> REG / <<< 401 / >>> AUTH / <<< 200)
		if len(deviceStates) >= 2 && deviceStates[len(deviceStates)-1] == "Registered" && len(traces) >= 4 {
			break
		}
	}

	// 关 stdin 触发 daemon EOF 退出
	stdin.Close()
	_ = cmd.Wait()
	stderrLog := <-stderrBuf

	// 5. 断言 start_device 响应
	if startResp == nil {
		t.Fatalf("no start_device response received")
	}
	result, _ := startResp["result"].(map[string]any)
	if reqID, _ := result["request_id"].(string); reqID == "" {
		t.Errorf("start_device result.request_id missing: %v", startResp)
	}
	if started, _ := result["started"].(bool); !started {
		t.Errorf("start_device result.started = %v, want true", started)
	}

	// 6. 断言 device_state 事件序列:Registering → Registered
	if len(deviceStates) < 2 {
		t.Fatalf("expected >=2 device_state events, got %d: %v", len(deviceStates), deviceStates)
	}
	if deviceStates[0] != "Registering" {
		t.Errorf("first device_state = %q, want Registering", deviceStates[0])
	}
	if deviceStates[len(deviceStates)-1] != "Registered" {
		t.Errorf("last device_state = %q, want Registered", deviceStates[len(deviceStates)-1])
	}

	// 7. 断言 sip_trace 事件:至少 4 条(outbound REGISTER / inbound 401 / outbound REGISTER auth / inbound 200)
	if len(traces) < 4 {
		t.Fatalf("expected >=4 sip_trace events, got %d", len(traces))
	}
	// 第 1 条:out REGISTER
	if traces[0]["direction"] != "out" || traces[0]["method"] != "REGISTER" {
		t.Errorf("trace[0] = %v, want out REGISTER", traces[0])
	}
	// 第 2 条:in 401
	if traces[1]["direction"] != "in" {
		t.Errorf("trace[1].direction = %v, want in", traces[1]["direction"])
	}
	if statusCode, ok := traces[1]["status_code"].(float64); !ok || int(statusCode) != 401 {
		t.Errorf("trace[1].status_code = %v, want 401", traces[1]["status_code"])
	}
	// 第 3 条:out REGISTER (with auth)
	if traces[2]["direction"] != "out" || traces[2]["method"] != "REGISTER" {
		t.Errorf("trace[2] = %v, want out REGISTER", traces[2])
	}
	// 第 4 条:in 200
	if traces[3]["direction"] != "in" {
		t.Errorf("trace[3].direction = %v, want in", traces[3]["direction"])
	}
	if statusCode, ok := traces[3]["status_code"].(float64); !ok || int(statusCode) != 200 {
		t.Errorf("trace[3].status_code = %v, want 200", traces[3]["status_code"])
	}

	// 8. 断言 mock 平台侧收到 2 条 REGISTER
	mockReqs := mock.Received()
	if len(mockReqs) != 2 {
		t.Errorf("mock received %d REGISTER, want 2: %+v", len(mockReqs), mockReqs)
	}
	if len(mockReqs) >= 2 {
		if mockReqs[0].HasAuthorization {
			t.Errorf("first REGISTER should not have Authorization")
		}
		if !mockReqs[1].HasAuthorization {
			t.Errorf("second REGISTER should have Authorization")
		}
	}

	// stderr 日志留作诊断
	if t.Failed() {
		t.Logf("daemon stderr:\n%s", stderrLog)
	}
}

// TestE2E_StdioMode_StopDevice 验证 stop_device 幂等:无活跃 session 也返 {stopped: true}。
func TestE2E_StdioMode_StopDevice(t *testing.T) {
	daemonBin := buildDaemon(t)
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	cmd := exec.CommandContext(ctx, daemonBin, "--stdio")
	stdin, _ := cmd.StdinPipe()
	stdout, _ := cmd.StdoutPipe()
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	defer func() {
		stdin.Close()
		_ = cmd.Wait()
	}()

	// 直接 stop(未 start)
	stopReq := map[string]any{"jsonrpc": "2.0", "method": "stop_device", "id": 1}
	line, _ := json.Marshal(stopReq)
	line = append(line, '\n')
	stdin.Write(line)

	scanner := bufio.NewScanner(stdout)
	var resp map[string]any
	deadline := time.Now().Add(3 * time.Second)
	for scanner.Scan() && time.Now().Before(deadline) {
		if err := json.Unmarshal(scanner.Bytes(), &resp); err == nil {
			if resp["id"] != nil {
				break
			}
		}
	}
	stdin.Close()
	_ = cmd.Wait()

	if resp == nil {
		t.Fatal("no stop_device response")
	}
	result, _ := resp["result"].(map[string]any)
	if stopped, _ := result["stopped"].(bool); !stopped {
		t.Errorf("stop_device result.stopped = %v, want true", stopped)
	}
}

// TestE2E_StdioMode_GetDeviceStatus 验证未启动时返 {state: Disconnected, registered_expires_secs: 0}。
func TestE2E_StdioMode_GetDeviceStatus(t *testing.T) {
	daemonBin := buildDaemon(t)
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	cmd := exec.CommandContext(ctx, daemonBin, "--stdio")
	stdin, _ := cmd.StdinPipe()
	stdout, _ := cmd.StdoutPipe()
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	defer func() {
		stdin.Close()
		_ = cmd.Wait()
	}()

	req := map[string]any{"jsonrpc": "2.0", "method": "get_device_status", "id": 1}
	line, _ := json.Marshal(req)
	line = append(line, '\n')
	stdin.Write(line)

	scanner := bufio.NewScanner(stdout)
	var resp map[string]any
	deadline := time.Now().Add(3 * time.Second)
	for scanner.Scan() && time.Now().Before(deadline) {
		if err := json.Unmarshal(scanner.Bytes(), &resp); err == nil {
			if resp["id"] != nil {
				break
			}
		}
	}
	stdin.Close()
	_ = cmd.Wait()

	if resp == nil {
		t.Fatal("no get_device_status response")
	}
	result, _ := resp["result"].(map[string]any)
	if state, _ := result["state"].(string); state != "Disconnected" {
		t.Errorf("state = %q, want Disconnected", state)
	}
	if expires, _ := result["registered_expires_secs"].(float64); expires != 0 {
		t.Errorf("registered_expires_secs = %v, want 0", expires)
	}
}

// TestE2E_StdioMode_Ping 验证内建 ping handler 返回 {pong: true}。
func TestE2E_StdioMode_Ping(t *testing.T) {
	daemonBin := buildDaemon(t)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	cmd := exec.CommandContext(ctx, daemonBin, "--stdio")
	stdin, _ := cmd.StdinPipe()
	stdout, _ := cmd.StdoutPipe()
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	defer func() {
		stdin.Close()
		_ = cmd.Wait()
	}()

	req := map[string]any{"jsonrpc": "2.0", "method": "ping", "id": 42}
	line, _ := json.Marshal(req)
	line = append(line, '\n')
	stdin.Write(line)

	scanner := bufio.NewScanner(stdout)
	var resp map[string]any
	deadline := time.Now().Add(2 * time.Second)
	for scanner.Scan() && time.Now().Before(deadline) {
		if err := json.Unmarshal(scanner.Bytes(), &resp); err == nil {
			if resp["id"] != nil {
				break
			}
		}
	}
	stdin.Close()
	_ = cmd.Wait()

	if resp == nil {
		t.Fatal("no ping response")
	}
	result, _ := resp["result"].(map[string]any)
	if pong, _ := result["pong"].(bool); !pong {
		t.Errorf("ping result.pong = %v, want true", pong)
	}
	// 验证 id 透传
	if idVal, _ := resp["id"].(float64); int(idVal) != 42 {
		t.Errorf("response id = %v, want 42", resp["id"])
	}
}

// TestE2E_StdioMode_MethodNotFound 验证未知 method 返回 JSON-RPC -32601 错误。
func TestE2E_StdioMode_MethodNotFound(t *testing.T) {
	daemonBin := buildDaemon(t)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	cmd := exec.CommandContext(ctx, daemonBin, "--stdio")
	stdin, _ := cmd.StdinPipe()
	stdout, _ := cmd.StdoutPipe()
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	defer func() {
		stdin.Close()
		_ = cmd.Wait()
	}()

	req := map[string]any{"jsonrpc": "2.0", "method": "no_such_method", "id": 1}
	line, _ := json.Marshal(req)
	line = append(line, '\n')
	stdin.Write(line)

	scanner := bufio.NewScanner(stdout)
	var resp map[string]any
	deadline := time.Now().Add(2 * time.Second)
	for scanner.Scan() && time.Now().Before(deadline) {
		if err := json.Unmarshal(scanner.Bytes(), &resp); err == nil {
			if resp["id"] != nil {
				break
			}
		}
	}
	stdin.Close()
	_ = cmd.Wait()

	if resp == nil {
		t.Fatal("no response")
	}
	errObj, ok := resp["error"].(map[string]any)
	if !ok {
		t.Fatalf("expected error field, got %v", resp)
	}
	code, _ := errObj["code"].(float64)
	if int(code) != -32601 {
		t.Errorf("error.code = %v, want -32601 (method not found)", code)
	}
	msg, _ := errObj["message"].(string)
	if !strings.Contains(msg, "no_such_method") {
		t.Errorf("error.message should mention method name, got %q", msg)
	}
}

// 打印所有 trace 事件摘要(诊断用)。
func dumpTraces(t *testing.T, traces []map[string]any) {
	t.Helper()
	if !t.Failed() {
		return
	}
	t.Log("=== sip_trace events ===")
	for i, tr := range traces {
		dir := tr["direction"]
		method := tr["method"]
		status := tr["status_code"]
		summary := tr["summary"]
		t.Logf("[%d] %v %v %v | %v", i, dir, method, status, summary)
	}
}

func init() {
	// 让编译器知道 dumpTraces 被测试用,虽然实际只在 t.Failed() 时才调
	_ = fmt.Sprint
}
