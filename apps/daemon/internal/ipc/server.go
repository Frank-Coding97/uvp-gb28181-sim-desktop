package ipc

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"sync/atomic"
	"time"
)

// Server 是 stdio JSON-RPC 2.0 服务器,负责:
//   - stdin 按行读入 → 派发 Router → 写响应到 stdout
//   - Publish() 供业务层推事件,内部按 priority/bulk 双队列走 stdout
//   - 每 5 分钟汇报一次 bulk 丢弃统计(plan R3,P2-5 调整频率)
//
// 双队列设计(plan §2.2 + R3):
//   priorityCh — 命令响应 + device_state 等业务关键事件,永不丢弃(cap 大 + 阻塞发送)。
//   bulkCh     — sip_trace / 心跳成功等高频事件,满了直接丢弃并累计 droppedCount。
//
// 单 pump goroutine 从两队列 select 出下一帧写 stdout,priority 用 default 分支
// 保证在两队列都有数据时优先出 priority(select 内偏置)。
type Server struct {
	router *Router

	priorityCh chan []byte
	bulkCh     chan []byte

	seqCounter    atomic.Uint64
	droppedCount  atomic.Uint64
	statsInterval time.Duration // 默认 60s,测试用短

	// 关闭协调:pump 从 stopped 读到关闭信号后退出。Run 返回前 close(stopped)。
	stopped chan struct{}
}

// ServerOptions 微调 Server 参数,测试用得多。
type ServerOptions struct {
	PriorityBufferSize int           // 默认 1024
	BulkBufferSize     int           // 默认 256
	StatsInterval      time.Duration // 默认 5 分钟;<=0 禁用统计广播
}

// NewServer 建 Server(默认队列容量)。
func NewServer(router *Router) *Server {
	return NewServerWithOptions(router, ServerOptions{})
}

// NewServerWithOptions 建 Server 并允许覆盖默认参数。
func NewServerWithOptions(router *Router, opts ServerOptions) *Server {
	pri := opts.PriorityBufferSize
	if pri <= 0 {
		pri = 1024
	}
	bulk := opts.BulkBufferSize
	if bulk <= 0 {
		bulk = 256
	}
	interval := opts.StatsInterval
	if interval == 0 {
		interval = 5 * time.Minute // P2-5: 60s → 5min,bulk 丢包是异常,不需频繁推送
	}
	return &Server{
		router:        router,
		priorityCh:    make(chan []byte, pri),
		bulkCh:        make(chan []byte, bulk),
		statsInterval: interval,
		stopped:       make(chan struct{}),
	}
}

// Router 暴露内部 router 便于外部注册 handler。
func (s *Server) Router() *Router { return s.router }

// DroppedCount 返回累计被丢弃的 bulk 事件数(诊断用)。
func (s *Server) DroppedCount() uint64 { return s.droppedCount.Load() }

// Run 阻塞主循环,直到 ctx cancel 或 stdin EOF。
//
// 内部启动 2 个 goroutine:
//   - reader: 按行读 stdin → 派发 → 结果送 priorityCh
//   - stats:  周期发送 bulk_stats notification(可选)
// 主 goroutine 兼当 pump,从 priorityCh/bulkCh select 出下一帧写 stdout。
//
// stdin EOF 是正常关闭信号,Run 返回 nil。
func (s *Server) Run(ctx context.Context, stdin io.Reader, stdout io.Writer) error {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	defer close(s.stopped)

	writer := bufio.NewWriter(stdout)

	// reader goroutine
	readerDone := make(chan error, 1)
	go func() {
		readerDone <- s.readLoop(ctx, stdin)
	}()

	// stats goroutine (可选)
	if s.statsInterval > 0 {
		go s.statsLoop(ctx)
	}

	// pump: select priorityCh / bulkCh / ctx.Done / readerDone
	//
	// 双队列 select 顺序语义(P2-6 注释):
	//   Go select 在多 case 同时 ready 时随机选一个,但因为 priorityCh cap=1024
	//   大于 bulkCh cap=256,实际上 priority 更不容易满,大部分时候两个 ch 都有
	//   数据时会随机选。真正的"优先级"靠 producer 侧:device_state 等关键事件
	//   走 priority(阻塞直到入队),sip_trace 走 bulk(满即丢)。这里的 select
	//   只是"公平消费",不是"优先调度"。
	//
	// 优先输出 priorityCh:每轮先非阻塞尝试 priorityCh,拿到就写,否则再走完整 select。
	for {
		// 优先偏置(plan R3):同轮先 drain priority,再看 bulk。
		select {
		case frame := <-s.priorityCh:
			if err := writeFrame(writer, frame); err != nil {
				return err
			}
			continue
		default:
		}

		select {
		case frame := <-s.priorityCh:
			if err := writeFrame(writer, frame); err != nil {
				return err
			}
		case frame := <-s.bulkCh:
			if err := writeFrame(writer, frame); err != nil {
				return err
			}
		case err := <-readerDone:
			// stdin EOF 或读错误:排空剩余队列后退出。
			s.drainAndFlush(writer)
			if err != nil && !errors.Is(err, io.EOF) {
				return err
			}
			return nil
		case <-ctx.Done():
			s.drainAndFlush(writer)
			return nil
		}
	}
}

// writeFrame 写一行 JSON + 换行,立即 Flush。
//
// Tauri 子进程 stdout pipe buffer 通常 64 KiB,不 flush 会卡帧;每帧 flush 开销 ~us 级,可接受。
func writeFrame(w *bufio.Writer, frame []byte) error {
	if _, err := w.Write(frame); err != nil {
		return fmt.Errorf("ipc: write stdout: %w", err)
	}
	if err := w.WriteByte('\n'); err != nil {
		return fmt.Errorf("ipc: write newline: %w", err)
	}
	if err := w.Flush(); err != nil {
		return fmt.Errorf("ipc: flush stdout: %w", err)
	}
	return nil
}

// drainAndFlush 尽力把队列里已有的帧写完(优先 priority),用于 ctx cancel 后的 graceful 收尾。
// 非阻塞:队列空即返回。
func (s *Server) drainAndFlush(w *bufio.Writer) {
	for {
		select {
		case frame := <-s.priorityCh:
			_ = writeFrame(w, frame)
		default:
			// priority 排空后再排 bulk
			select {
			case frame := <-s.bulkCh:
				_ = writeFrame(w, frame)
			default:
				return
			}
		}
	}
}

// readLoop 按行扫 stdin,decode → dispatch → 结果送 priorityCh(阻塞发送保证不丢)。
//
// scanner.Scan 阻塞在 stdin 上,ctx cancel 时无法直接打断。
// 解决:主循环 select 靠 readerDone / ctx.Done 竞争退出,readLoop 自然在 stdin 关闭时结束。
func (s *Server) readLoop(ctx context.Context, stdin io.Reader) error {
	scanner := bufio.NewScanner(stdin)
	// 单帧上限 1 MiB (Tauri 侧命令 payload 一般 <10 KiB, SIP Trace 长报文极端也 <100 KiB)。
	scanner.Buffer(make([]byte, 0, 64*1024), 1024*1024)

	for scanner.Scan() {
		if err := ctx.Err(); err != nil {
			return err
		}
		line := scanner.Bytes()
		if len(line) == 0 {
			continue
		}
		// 复制一份 line 再传出去:scanner.Bytes() 底层缓冲区会被下一次 Scan 覆盖。
		lineCopy := make([]byte, len(line))
		copy(lineCopy, line)

		resp := s.processFrame(ctx, lineCopy)
		if resp == nil {
			continue // Notification: 不回响应
		}
		// 命令响应走 priorityCh,阻塞发送(pump 迟早会读走)。
		select {
		case s.priorityCh <- resp:
		case <-ctx.Done():
			return ctx.Err()
		}
	}
	if err := scanner.Err(); err != nil {
		return err
	}
	return io.EOF
}

// processFrame 解一帧,派发,组装响应字节。Notification 返回 nil。
func (s *Server) processFrame(ctx context.Context, line []byte) []byte {
	req, err := DecodeRequest(line)
	if err != nil {
		// parse 失败:按 JSON-RPC 2.0 §5.1 用 null id 报 -32700。
		return EncodeError(nil, -32700, err.Error())
	}
	if req.IsNotification() {
		// 派发但忽略结果(允许 fire-and-forget)。
		_, _ = s.router.Dispatch(ctx, req.Method, req.Params)
		return nil
	}
	result, err := s.router.Dispatch(ctx, req.Method, req.Params)
	if err != nil {
		if errors.Is(err, ErrMethodNotFound) {
			return EncodeError(req.ID, -32601, "method not found: "+req.Method)
		}
		return EncodeError(req.ID, -32000, err.Error())
	}
	return EncodeResponse(req.ID, result)
}

// Publish 推一条事件到 stdout(异步)。
//
// priority=true → 走 priorityCh,阻塞直到 pump 消费(不丢)。
// priority=false → 走 bulkCh,满了立即丢弃并 droppedCount++(不阻塞业务)。
//
// 事件自动带 seq 递增号(方便前端按顺序处理与断点检测)。
func (s *Server) Publish(method string, payload map[string]any, priority bool) {
	seq := s.seqCounter.Add(1)
	if payload == nil {
		payload = map[string]any{}
	}
	// 复制一份避免调用方后续 mutate payload map(常见坑)。
	// 这里深拷贝一层足够(测试与业务里 payload 都是新构造的 map,内嵌值不共享指针)。
	pcopy := make(map[string]any, len(payload)+1)
	for k, v := range payload {
		pcopy[k] = v
	}
	pcopy["seq"] = seq
	pcopy["ts_ms"] = time.Now().UnixMilli()

	frame := EncodeNotification(method, pcopy)

	// P0-3 fix: sip_trace 改走 priority 队列,避免丢关键 401/AUTH 报文
	if priority || method == "sip_trace" {
		s.priorityCh <- frame
		return
	}
	select {
	case s.bulkCh <- frame:
	default:
		s.droppedCount.Add(1)
		slog.Debug("ipc: bulk queue full, event dropped", "method", method, "seq", seq)
	}
}

// statsLoop 周期发布 bulk_stats notification,让前端知道丢包量。
// M2 初版是 60s,实际场景 bulk 丢包是异常事件,5 分钟足够(减少前端无用刷新)。
func (s *Server) statsLoop(ctx context.Context) {
	tick := time.NewTicker(s.statsInterval)
	defer tick.Stop()
	for {
		select {
		case <-tick.C:
			dropped := s.droppedCount.Load()
			// dropped 只增,不清零;前端算增量。
			frame := EncodeNotification("bulk_stats", map[string]any{
				"dropped_count": dropped,
				"seq":           s.seqCounter.Add(1),
				"ts_ms":         time.Now().UnixMilli(),
			})
			select {
			case s.priorityCh <- frame:
			case <-ctx.Done():
				return
			}
		case <-ctx.Done():
			return
		}
	}
}

// ensure json.RawMessage still used implicit via encoders.
var _ = json.RawMessage(nil)
