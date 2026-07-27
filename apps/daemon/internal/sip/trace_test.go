package sip

import (
	"strings"
	"sync"
	"testing"
	"time"
)

// TestTraceEvent_BasicFields TraceEvent 结构含必须字段。
func TestTraceEvent_BasicFields(t *testing.T) {
	e := TraceEvent{
		Timestamp: time.Now(),
		Direction: DirectionOutbound,
		Method:    "REGISTER",
		RawBytes:  []byte("REGISTER sip:...\r\n"),
	}
	if e.Direction != "out" {
		t.Errorf("DirectionOutbound = %q, want out", e.Direction)
	}
	if len(e.RawBytes) == 0 {
		t.Fatalf("RawBytes empty")
	}
}

// TestBufferedTraceCollector 记录多条事件并按顺序返回。
func TestBufferedTraceCollector(t *testing.T) {
	c := NewBufferedTraceCollector(4)
	c.Emit(TraceEvent{Direction: DirectionOutbound, Method: "REGISTER"})
	c.Emit(TraceEvent{Direction: DirectionInbound, StatusCode: 401})
	c.Emit(TraceEvent{Direction: DirectionOutbound, Method: "REGISTER"})
	c.Emit(TraceEvent{Direction: DirectionInbound, StatusCode: 200})

	got := c.Snapshot()
	if len(got) != 4 {
		t.Fatalf("len = %d, want 4", len(got))
	}
	if got[0].Direction != "out" || got[0].Method != "REGISTER" {
		t.Errorf("first event wrong: %+v", got[0])
	}
	if got[3].Direction != "in" || got[3].StatusCode != 200 {
		t.Errorf("last event wrong: %+v", got[3])
	}
}

// TestBufferedTraceCollector_RingOverflow 满 buffer 覆盖最老事件(容量 3, 塞 5)。
func TestBufferedTraceCollector_RingOverflow(t *testing.T) {
	c := NewBufferedTraceCollector(3)
	for i := 0; i < 5; i++ {
		c.Emit(TraceEvent{Method: "REGISTER", CSeq: uint32(i)})
	}
	got := c.Snapshot()
	if len(got) != 3 {
		t.Fatalf("len = %d, want 3", len(got))
	}
	// 应保留最后 3 条: CSeq=2,3,4
	if got[0].CSeq != 2 || got[2].CSeq != 4 {
		t.Errorf("ring content wrong: %+v", got)
	}
}

// TestBufferedTraceCollector_Concurrent 并发 Emit 无 data race。
func TestBufferedTraceCollector_Concurrent(t *testing.T) {
	c := NewBufferedTraceCollector(1000)
	var wg sync.WaitGroup
	for i := 0; i < 10; i++ {
		wg.Add(1)
		go func(id int) {
			defer wg.Done()
			for j := 0; j < 100; j++ {
				c.Emit(TraceEvent{Method: "MSG", CSeq: uint32(id*100 + j)})
			}
		}(i)
	}
	wg.Wait()

	got := c.Snapshot()
	if len(got) != 1000 {
		t.Errorf("expected 1000 events, got %d", len(got))
	}
}

// TestSummarizeRawBytes 单行摘要提取 method + Request-URI 或状态码。
func TestSummarizeRawBytes(t *testing.T) {
	outboundRegister := []byte(
		"REGISTER sip:3502000000@192.168.10.220:8160 SIP/2.0\r\n" +
			"From: <sip:35020000001320000001@3502000000>;tag=abc\r\n" +
			"CSeq: 1 REGISTER\r\n" +
			"\r\n",
	)
	sum := SummarizeSIPBytes(outboundRegister)
	if !strings.Contains(sum, "REGISTER") {
		t.Errorf("summary should contain REGISTER, got %q", sum)
	}

	inbound401 := []byte(
		"SIP/2.0 401 Unauthorized\r\n" +
			"From: <sip:35020000001320000001@3502000000>;tag=abc\r\n" +
			"CSeq: 1 REGISTER\r\n" +
			"\r\n",
	)
	sum2 := SummarizeSIPBytes(inbound401)
	if !strings.Contains(sum2, "401") {
		t.Errorf("summary should contain 401, got %q", sum2)
	}
}

// TestSummarizeRawBytes_Empty 空/短输入不 panic。
func TestSummarizeRawBytes_Empty(t *testing.T) {
	// 不 panic 即通过
	_ = SummarizeSIPBytes(nil)
	_ = SummarizeSIPBytes([]byte(""))
	_ = SummarizeSIPBytes([]byte("garbage"))
}
