package device

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"log/slog"
	"strconv"
	"sync"
	"sync/atomic"
	"time"

	"github.com/emiago/sipgo/sip"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/internal/gb28181"
)

// SipClient 抽象 SIP 客户端能力,便于测试注入 mock。
//
// 与 internal/sip.Client 的方法签名一致但独立定义:
// 让 device 层不直接依赖 sipgo,测试时可用 mockClient 实现。
type SipClient interface {
	Do(ctx context.Context, req *sip.Request) (*sip.Response, error)
}

// RegistrationSession 是设备端注册会话。
//
// M1 生命周期:
//   Start() → 构造 REGISTER → 发送 → 拿 200 OK → 状态转 Registered → 结束
//
// M3 生命周期:
//   Start(parent ctx) → REGISTER → 200 OK → 派生根 ctx (s.ctx) →
//   spawn heartbeat + renewal goroutine 各绑 s.ctx →
//   Stop() → cancel s.ctx (所有子 goroutine 收 Done 退出) → 状态 Disconnected。
//
// 并发不变式 (spec R5):
//   - s.ctx / s.cancel 只在 Start 内部初始化 (原子 CAS 保护)
//   - Stop 幂等 (多次调用不 panic,sync.Once 包裹 cancel)
//   - 子 goroutine 必须绑 s.ctx,不允许长驻在 context.Background() 里
type RegistrationSession struct {
	client SipClient // 通过接口注入,便于 mock

	cfg *gb28181.SipConfig

	// 状态字段用 atomic 让 State() 无锁读。
	state atomic.Int32

	// SIP 会话标识 (spec Q10):
	// 整个 device 生命周期复用同一 Call-ID / From tag,CSeq 全局单调递增。
	callID      string
	fromTag     string
	cseqCounter atomic.Uint64

	// 平台确认的 Expires 值 (spec Q9):
	// 响应 Contact 头 expires 参数 > 响应 Expires 头 > 请求发出去的值。
	registeredExpires atomic.Int64

	// 生命周期根 ctx。Start 内初始化,Stop 内 cancel。
	// 子 goroutine (heartbeat / renewal) 从这里派生。
	ctxMu    sync.Mutex
	ctx      context.Context
	cancel   context.CancelFunc
	stopOnce sync.Once
}

// NewRegistrationSession 创建会话。
//
// 此时只是初始化,不真实发起注册。调 Start 才发 REGISTER。
func NewRegistrationSession(client SipClient, cfg *gb28181.SipConfig) *RegistrationSession {
	callID := generateCallID()
	fromTag := generateTag("from")
	return &RegistrationSession{
		client:  client,
		cfg:     cfg,
		callID:  callID,
		fromTag: fromTag,
	}
}

// Start 发起注册。同步阻塞至注册结果确定 (成功 / 失败 / 超时)。
//
// M3 变化:
//   - Start 内部派生 s.ctx (来自 context.Background,不受 parent ctx 束缚)
//   - parent ctx 只用来给这一次 REGISTER 请求做超时/取消 (原语义保留)
//   - 注册成功后 s.ctx 保持存活给 heartbeat / renewal 用,直到 Stop() cancel
//   - 注册失败后 s.ctx 立即 cancel (避免上层残留 goroutine)
//
// 上层调用者 (main.go runOnce / stdio handler) 拿到 nil error 表示 200 OK。
// heartbeat / renewal 的 spawn 见 T4 (session.go Start 收尾处)。
func (s *RegistrationSession) Start(parentCtx context.Context) error {
	if !s.state.CompareAndSwap(int32(StateDisconnected), int32(StateRegistering)) {
		return errors.New("session already started")
	}

	// 派生根 ctx。用 context.Background 作父级 —— parentCtx 只用于这次 REGISTER 请求,
	// 不能让 parentCtx cancel 就把 heartbeat/renewal 也带走 (上层可能只想给单个 RPC 定 15s
	// 超时,不想影响 daemon 长驻的心跳循环)。
	s.ctxMu.Lock()
	if s.ctx == nil {
		s.ctx, s.cancel = context.WithCancel(context.Background())
	}
	s.ctxMu.Unlock()

	req, err := s.buildRegisterRequest()
	if err != nil {
		s.markFailedLocked()
		return fmt.Errorf("build REGISTER: %w", err)
	}

	slog.Info("sending REGISTER",
		"cseq", req.CSeq().SeqNo,
		"call_id", s.callID,
		"request_uri", req.Recipient.String())

	resp, err := s.client.Do(parentCtx, req)
	if err != nil {
		s.markFailedLocked()
		return fmt.Errorf("REGISTER: %w", err)
	}

	if resp.StatusCode != 200 {
		s.markFailedLocked()
		return fmt.Errorf("REGISTER unexpected status: %d %s", resp.StatusCode, resp.Reason)
	}

	// 解析平台确认的 Expires (spec Q9)
	expires := s.parseResponseExpires(resp)
	s.registeredExpires.Store(int64(expires))

	s.state.Store(int32(StateRegistered))
	slog.Info("REGISTER 200 OK",
		"registered_expires_secs", expires,
		"platform_server", resp.GetHeader("Server"))

	return nil
}

// Stop 主动停止会话。cancel 根 ctx → heartbeat / renewal / 任意从 s.ctx 派生的子任务收 Done。
//
// 幂等 (spec AC-11): 多次调用不 panic,不重复推送事件。
// M3 T3 会扩展:Stop 内先发 Expires=0 REGISTER,再 cancel。
func (s *RegistrationSession) Stop() error {
	s.stopOnce.Do(func() {
		s.ctxMu.Lock()
		cancel := s.cancel
		s.ctxMu.Unlock()

		if cancel != nil {
			cancel()
		}
		s.state.Store(int32(StateDisconnected))
	})
	return nil
}

// InternalContext 返回 session 生命周期根 ctx。
//
// 用于 heartbeat / renewal / shutdown 等内部 goroutine 从这里派生 ctx。
// Start() 之前返回 nil,Start() 后返回一个未 cancel 的 ctx (或 Failed 后已 cancel 的 ctx)。
// 外部调用者应视为**只读**:不要在这里再 cancel,统一走 session.Stop()。
func (s *RegistrationSession) InternalContext() context.Context {
	s.ctxMu.Lock()
	defer s.ctxMu.Unlock()
	return s.ctx
}

// markFailedLocked 把 session 迁移到 Failed 状态,并 cancel 根 ctx。
// 用于 Start 失败路径与 heartbeat 3 次连续失败 (T1) 场景。
func (s *RegistrationSession) markFailedLocked() {
	s.state.Store(int32(StateFailed))
	s.stopOnce.Do(func() {
		s.ctxMu.Lock()
		cancel := s.cancel
		s.ctxMu.Unlock()
		if cancel != nil {
			cancel()
		}
	})
}

// markFailed 是 markFailedLocked 的公开别名,给 heartbeat / renewal 用。
func (s *RegistrationSession) markFailed(reason string) {
	slog.Warn("session marked failed", "reason", reason)
	s.markFailedLocked()
}

// State 返回当前状态 (原子读,无锁)。
func (s *RegistrationSession) State() State {
	return State(s.state.Load())
}

// RegisteredExpires 返回平台确认的 Expires 值 (秒)。
// 未注册返回 0。用于 M3 计算续约触发时机。
func (s *RegistrationSession) RegisteredExpires() int {
	return int(s.registeredExpires.Load())
}

// buildRegisterRequest 构造 REGISTER 请求。
//
// URI 与头字段遵循国标 GB/T 28181 A.1.1 + spec Q5/Q10。
// TODO(M2 T5): 加 Trace observer 后,把这里生成的报文推给前端。
func (s *RegistrationSession) buildRegisterRequest() (*sip.Request, error) {
	requestURIStr := gb28181.BuildRegisterRequestURI(s.cfg)
	var requestURI sip.Uri
	if err := sip.ParseUri(requestURIStr, &requestURI); err != nil {
		return nil, fmt.Errorf("parse request-URI %q: %w", requestURIStr, err)
	}

	req := sip.NewRequest(sip.REGISTER, requestURI)

	// From (含 tag)
	var fromURI sip.Uri
	if err := sip.ParseUri(gb28181.BuildFromURI(s.cfg), &fromURI); err != nil {
		return nil, fmt.Errorf("parse from-URI: %w", err)
	}
	fromParams := sip.NewParams()
	fromParams.Add("tag", s.fromTag)
	req.AppendHeader(&sip.FromHeader{
		Address: fromURI,
		Params:  fromParams,
	})

	// To (无 tag,REGISTER 初始注册)
	var toURI sip.Uri
	if err := sip.ParseUri(gb28181.BuildToURI(s.cfg), &toURI); err != nil {
		return nil, fmt.Errorf("parse to-URI: %w", err)
	}
	req.AppendHeader(&sip.ToHeader{
		Address: toURI,
		Params:  sip.NewParams(),
	})

	// Call-ID (全期复用)
	callID := sip.CallIDHeader(s.callID)
	req.AppendHeader(&callID)

	// CSeq (全局单调递增,不同 method 也不重置 - spec Q10)
	seq := s.cseqCounter.Add(1)
	req.AppendHeader(&sip.CSeqHeader{
		SeqNo:      uint32(seq),
		MethodName: sip.REGISTER,
	})

	// Expires
	expires := sip.ExpiresHeader(s.cfg.ExpiresSecs)
	req.AppendHeader(&expires)

	// Max-Forwards
	maxFwd := sip.MaxForwardsHeader(70)
	req.AppendHeader(&maxFwd)

	// User-Agent (sipgo v1.4.0 无内建 UserAgentHeader 类型,用 NewHeader 通用构造)
	req.AppendHeader(sip.NewHeader("User-Agent", gb28181.UserAgent))

	// Content-Length: 0 (REGISTER 无 body)
	cl := sip.ContentLengthHeader(0)
	req.AppendHeader(&cl)

	// Contact 头 (RFC 3261 §10.2 强制,GB28181 平台没这个会静默拒绝)。
	// user 用设备 ID,host:port 用本机对外 IP + 期望的 UDP 端口。
	// M1 客户端没绑固定端口,用 sipgo 挑的临时端口 (0=让平台从 Via 头拿)。
	// 这里先写占位 IP:0,由 sipgo 传输层在 WriteMsg 前根据实际 socket 改写。
	contactURI := sip.Uri{
		User: s.cfg.DeviceID,
		Host: s.cfg.ServerHost, // 占位,sipgo 会用实际 laddr 覆盖
		Port: 0,
	}
	req.AppendHeader(&sip.ContactHeader{
		Address: contactURI,
		Params:  sip.NewParams(),
	})

	// 显式设置目的地址:国标 Request-URI 的 host 是 SIP 域 (10 位数字或域名 ID),
	// sipgo 默认按 host 做 DNS 解析会打到错的 IP。用 SetDestination 显式指定
	// 平台真实地址 (ServerHost:ServerPort),绕过 URI host 的 DNS 步骤。
	req.SetDestination(fmt.Sprintf("%s:%d", s.cfg.ServerHost, s.cfg.ServerPort))

	return req, nil
}

// parseResponseExpires 解析平台 200 OK 中的实际 Expires (spec Q9)。
//
// 优先级:Contact 头 expires 参数 > 独立 Expires 头 > 请求发出去的默认值。
func (s *RegistrationSession) parseResponseExpires(resp *sip.Response) int {
	// 1. Contact 头 expires 参数
	if contact := resp.Contact(); contact != nil {
		if expiresStr, ok := contact.Params.Get("expires"); ok {
			if n, err := strconv.Atoi(expiresStr); err == nil {
				return n
			}
		}
	}

	// 2. 独立 Expires 头
	if expiresHdr := resp.GetHeader("Expires"); expiresHdr != nil {
		if n, err := strconv.Atoi(expiresHdr.Value()); err == nil {
			return n
		}
	}

	// 3. 兜底:请求的默认值 + warn log (spec Q11)
	slog.Warn("platform 200 OK missing Expires header and Contact expires param, fallback to request value",
		"fallback_expires_secs", s.cfg.ExpiresSecs)
	return s.cfg.ExpiresSecs
}

func generateCallID() string {
	b := make([]byte, 12)
	if _, err := rand.Read(b); err != nil {
		// crypto/rand 失败极罕见 (OS 熵源问题),退化到时间戳。
		return fmt.Sprintf("uvp-fallback-%d", time.Now().UnixNano())
	}
	return "uvp-" + hex.EncodeToString(b)
}

func generateTag(prefix string) string {
	b := make([]byte, 6)
	if _, err := rand.Read(b); err != nil {
		return fmt.Sprintf("%s-%d", prefix, time.Now().UnixNano())
	}
	return prefix + "-" + hex.EncodeToString(b)
}
