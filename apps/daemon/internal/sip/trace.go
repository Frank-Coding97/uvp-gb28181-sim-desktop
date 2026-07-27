package sip

import (
	"bytes"
	"strconv"
	"sync"
	"sync/atomic"
	"time"
)

// TraceEvent 是一条 SIP 报文观察记录。
//
// M2 阶段由 Client.Do / Client.doDigestChallenge 前后手工调 Emit 生成,
// 尚未 hook sipgo transport 层(那是 M3+ 优化)。
type TraceEvent struct {
	// Seq 是该事件在本进程内的全局递增序号(从 1 开始),用于前端排序。
	// 由 TracePublisher 内部自动填,业务侧不用管。
	Seq int64 `json:"seq"`

	// Timestamp 事件发生时刻(UTC)。
	Timestamp time.Time `json:"ts"`

	// Direction 报文方向: "in" 或 "out"(见 DirectionInbound / DirectionOutbound)。
	Direction string `json:"direction"`

	// Method 是 SIP method 名(REGISTER / MESSAGE / INVITE ...)。
	// 对 inbound 响应等于原请求的 method(CSeq method 部分),
	// 便于前端把请求/响应配对显示。
	Method string `json:"method,omitempty"`

	// StatusCode 仅对 inbound response 有效(0 表示 outbound / 无状态码)。
	StatusCode int `json:"status_code,omitempty"`

	// CSeq 是 CSeq 头的序号部分,方便前端按事务分组。0 表示未解析出。
	CSeq uint32 `json:"cseq,omitempty"`

	// CallID SIP 会话 ID(便于跨事务串联)。
	CallID string `json:"call_id,omitempty"`

	// Peer 对端地址 host:port(outbound 是 remote / inbound 是 remote)。
	Peer string `json:"peer,omitempty"`

	// Summary 一句话摘要:outbound "REGISTER sip:..." / inbound "401 Unauthorized"。
	Summary string `json:"summary"`

	// RawBytes 完整原始报文字节(含 CRLF)。
	// 前端展开条目时显示;JSON 序列化会转为 base64 或数字数组 —
	// 我们改序列成字符串(SIP 报文本来就是 ASCII,GB18030 body 也可读)。
	RawBytes []byte `json:"-"`

	// RawText 是 RawBytes 的字符串视图(便于 JSON 序列化)。EncodeForIPC 时用。
	RawText string `json:"raw,omitempty"`
}

// Direction 常量。
const (
	DirectionOutbound = "out"
	DirectionInbound  = "in"
)

// TracePublisher 是 Emit 回调接口。
//
// M2 生产实现:内部把 event 转 map 后调 ipc.Server.Publish("sip_trace", ..., false)。
// M2 测试实现:BufferedTraceCollector,验证用。
type TracePublisher interface {
	Emit(event TraceEvent)
}

// TracePublisherFunc 让普通 func 满足接口(测试常用)。
type TracePublisherFunc func(TraceEvent)

// Emit 实现 TracePublisher。
func (f TracePublisherFunc) Emit(e TraceEvent) { f(e) }

// BufferedTraceCollector 是环形缓冲收集器,主要用于测试断言与调试。
//
// 满 buffer 时覆盖最老事件(与前端 UI "最近 50 条" 语义一致)。
// 内部用 mutex + 单调 seq,支持并发 Emit + Snapshot。
type BufferedTraceCollector struct {
	mu       sync.Mutex
	events   []TraceEvent
	capacity int
	head     int // 下一个写入位置
	full     bool
	seq      atomic.Int64
}

// NewBufferedTraceCollector 建收集器,capacity <= 0 时用默认 128。
func NewBufferedTraceCollector(capacity int) *BufferedTraceCollector {
	if capacity <= 0 {
		capacity = 128
	}
	return &BufferedTraceCollector{
		events:   make([]TraceEvent, capacity),
		capacity: capacity,
	}
}

// Emit 记录一条事件,自动补 Seq 与 Timestamp(如果调用方没填)。
func (b *BufferedTraceCollector) Emit(e TraceEvent) {
	if e.Seq == 0 {
		e.Seq = b.seq.Add(1)
	}
	if e.Timestamp.IsZero() {
		e.Timestamp = time.Now()
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	b.events[b.head] = e
	b.head = (b.head + 1) % b.capacity
	if b.head == 0 {
		b.full = true
	}
}

// Snapshot 返回当前所有事件的副本,按插入顺序(旧 → 新)。
func (b *BufferedTraceCollector) Snapshot() []TraceEvent {
	b.mu.Lock()
	defer b.mu.Unlock()
	if !b.full {
		out := make([]TraceEvent, b.head)
		copy(out, b.events[:b.head])
		return out
	}
	out := make([]TraceEvent, b.capacity)
	copy(out, b.events[b.head:])
	copy(out[b.capacity-b.head:], b.events[:b.head])
	return out
}

// SummarizeSIPBytes 提取一句话摘要用于列表展示。
//
// 逻辑:
//   - 空/短输入直接返回空串
//   - 首行 "SIP/2.0 <code> <reason>"     → "<code> <reason>"
//   - 首行 "<METHOD> sip:...    SIP/2.0" → "<METHOD> sip:..."
//   - 其他 → 首行原样(裁剪到 120 字符)
func SummarizeSIPBytes(raw []byte) string {
	if len(raw) == 0 {
		return ""
	}
	// 找第一行(CRLF 或 LF)
	end := bytes.IndexAny(raw, "\r\n")
	if end == -1 {
		end = len(raw)
	}
	first := raw[:end]
	if len(first) > 200 {
		first = first[:200]
	}
	firstStr := string(first)

	// 响应格式
	if bytes.HasPrefix(first, []byte("SIP/2.0")) {
		// SIP/2.0 401 Unauthorized
		parts := bytes.SplitN(first, []byte(" "), 3)
		if len(parts) >= 2 {
			code := string(parts[1])
			reason := ""
			if len(parts) == 3 {
				reason = string(parts[2])
			}
			if reason != "" {
				return code + " " + reason
			}
			return code
		}
	}

	// 请求格式 "METHOD Request-URI SIP/2.0"
	spaceIdx := bytes.IndexByte(first, ' ')
	if spaceIdx > 0 && spaceIdx < len(first)-1 {
		endURI := bytes.LastIndex(first, []byte(" SIP/2.0"))
		if endURI > spaceIdx {
			return string(first[:endURI])
		}
	}
	return firstStr
}

// ExtractCSeq 从 SIP 报文里抽 CSeq 序号(找 "CSeq: <num> <method>" 行)。
// 未找到返回 0。
func ExtractCSeq(raw []byte) uint32 {
	// 简单扫行,不用完整 parser。
	for len(raw) > 0 {
		lineEnd := bytes.IndexAny(raw, "\r\n")
		var line []byte
		if lineEnd < 0 {
			line = raw
			raw = nil
		} else {
			line = raw[:lineEnd]
			raw = raw[lineEnd+1:]
			// 跳过 \r\n 的 \n
			if len(raw) > 0 && raw[0] == '\n' {
				raw = raw[1:]
			}
		}
		lineLower := bytes.ToLower(bytes.TrimSpace(line))
		if !bytes.HasPrefix(lineLower, []byte("cseq:")) {
			continue
		}
		v := bytes.TrimSpace(line[len("CSeq:"):])
		space := bytes.IndexByte(v, ' ')
		if space > 0 {
			v = v[:space]
		}
		n, err := strconv.ParseUint(string(v), 10, 32)
		if err == nil {
			return uint32(n)
		}
	}
	return 0
}

// ExtractCallID 从 SIP 报文里抽 Call-ID 头(未找到返回空串)。
func ExtractCallID(raw []byte) string {
	for len(raw) > 0 {
		lineEnd := bytes.IndexAny(raw, "\r\n")
		var line []byte
		if lineEnd < 0 {
			line = raw
			raw = nil
		} else {
			line = raw[:lineEnd]
			raw = raw[lineEnd+1:]
			if len(raw) > 0 && raw[0] == '\n' {
				raw = raw[1:]
			}
		}
		lineLower := bytes.ToLower(bytes.TrimSpace(line))
		if !bytes.HasPrefix(lineLower, []byte("call-id:")) && !bytes.HasPrefix(lineLower, []byte("i:")) {
			continue
		}
		// 找 : 后的值
		colon := bytes.IndexByte(line, ':')
		if colon < 0 {
			continue
		}
		return string(bytes.TrimSpace(line[colon+1:]))
	}
	return ""
}
