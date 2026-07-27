package ipc

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"strings"
	"sync"
	"testing"
	"time"
)

// TestServer_PingRoundtrip 集成:stdin 发 ping,stdout 收 pong。
func TestServer_PingRoundtrip(t *testing.T) {
	s := NewServer(NewRouter())
	RegisterBuiltins(s.Router())

	in := strings.NewReader(`{"jsonrpc":"2.0","method":"ping","id":1}` + "\n")
	var out bytes.Buffer

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	if err := s.Run(ctx, in, &out); err != nil && err != context.DeadlineExceeded {
		// Run 遇 stdin EOF 返 nil (正常退出),不该报别的错。
		t.Fatalf("Run: %v", err)
	}

	line := strings.TrimSpace(out.String())
	var resp map[string]any
	if err := json.Unmarshal([]byte(line), &resp); err != nil {
		t.Fatalf("bad JSON: %v", err)
	}
	if resp["result"].(map[string]any)["pong"] != true {
		t.Errorf("no pong: %v", resp)
	}
}

// TestServer_PublishPriorityAlwaysWritten priority 事件永不丢弃,并写到 stdout。
func TestServer_PublishPriorityAlwaysWritten(t *testing.T) {
	s := NewServer(NewRouter())

	in := &blockingReader{}
	var out threadSafeBuffer

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	done := make(chan struct{})
	go func() {
		_ = s.Run(ctx, in, &out)
		close(done)
	}()

	// 发 10 条 priority 事件 (device_state) — 全部必须写出去。
	for i := 0; i < 10; i++ {
		s.Publish("device_state", map[string]any{"seq_local": i}, true)
	}

	// 给 pump goroutine 一点时间 flush。
	deadline := time.Now().Add(1 * time.Second)
	for time.Now().Before(deadline) {
		if strings.Count(out.String(), "device_state") >= 10 {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}

	cancel()
	<-done

	got := strings.Count(out.String(), "device_state")
	if got != 10 {
		t.Errorf("expected 10 device_state events on stdout, got %d\nout=%s", got, out.String())
	}
}

// TestServer_PublishBulkMayDrop bulk 通道满时新事件被丢并累计 dropped_count。
func TestServer_PublishBulkMayDrop(t *testing.T) {
	// 用小容量 bulk 通道确保能触发丢弃(注入 opt)。
	s := NewServerWithOptions(NewRouter(), ServerOptions{BulkBufferSize: 2, PriorityBufferSize: 2})

	// 未启动 Run,pump 不消费,bulkCh 一定会满 → 后续 Publish 丢包。
	for i := 0; i < 20; i++ {
		s.Publish("sip_trace", map[string]any{"n": i}, false)
	}
	dropped := s.DroppedCount()
	if dropped == 0 {
		t.Fatalf("expected some drops (queue is 2, published 20), got %d", dropped)
	}
	// 保守断言:至少丢了 15 条(2 挤进队列,+ 少量在 select 那边落队的窗口,剩下都丢)。
	if dropped < 15 {
		t.Errorf("dropped=%d, expected >= 15", dropped)
	}
}

// TestServer_ResponseVsEventInterleave 请求响应与 event 交错都能出。
func TestServer_ResponseVsEventInterleave(t *testing.T) {
	s := NewServer(NewRouter())
	RegisterBuiltins(s.Router())

	// ping + 保持 stdin 打开(避免 EOF 触发 pump 退出;Tauri 场景下 stdin 常驻)。
	in := io.MultiReader(
		strings.NewReader(`{"jsonrpc":"2.0","method":"ping","id":1}`+"\n"),
		&blockingReader{},
	)
	var out threadSafeBuffer

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	done := make(chan struct{})
	go func() {
		_ = s.Run(ctx, in, &out)
		close(done)
	}()

	// ping 走完 → 发一条 event
	// 用轮询等 pong 出现,再 Publish 触发交错场景。
	waitForContains(t, &out, "pong", 1*time.Second)
	s.Publish("device_state", map[string]any{"state": "Registered"}, true)
	waitForContains(t, &out, "device_state", 1*time.Second)

	cancel()
	<-done

	if !strings.Contains(out.String(), "pong") || !strings.Contains(out.String(), "device_state") {
		t.Errorf("missing pong or event: %s", out.String())
	}
}

// blockingReader 永不返回数据(用来让 Run 主循环一直等 stdin)。
type blockingReader struct{}

func (b *blockingReader) Read(p []byte) (int, error) {
	// 无限阻塞:测试完靠 ctx cancel 让 pump 退出;Run 的 stdin goroutine 会靠 ctx 检查退出。
	// 简化:sleep 到调用方超时。
	time.Sleep(5 * time.Second)
	return 0, nil
}

// threadSafeBuffer 允许并发 Write / Read(测试里 pump goroutine 写,主 goroutine 读)。
type threadSafeBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (t *threadSafeBuffer) Write(p []byte) (int, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.buf.Write(p)
}

func (t *threadSafeBuffer) String() string {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.buf.String()
}

func waitForContains(t *testing.T, b *threadSafeBuffer, needle string, timeout time.Duration) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if strings.Contains(b.String(), needle) {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Errorf("timeout waiting for %q in stdout, got: %s", needle, b.String())
}
