package device

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"strings"
	"sync/atomic"
	"time"

	"github.com/emiago/sipgo/sip"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/internal/gb28181"
)

// EventPublisher 是 device 层向外推事件的最小接口 (对齐 ipc.Publisher 签名)。
//
// 独立在 device 包内定义:避免 device → ipc 反向依赖。
// 生产实现由 main.go 用 ipc.Server 满足 (方法签名一致)。
type EventPublisher interface {
	Publish(method string, payload map[string]any, priority bool)
}

// heartbeatSendTimeout 是单次心跳 MESSAGE 请求超时 (spec Q4 R3)。
//
// 5s 让检测更快:短暂网络抖动仍能捕获,3 次连败在 ~15s+ 触发降级。
// 60s 心跳周期下,3 次连败 ~ 180s 判定,与 spec Q4"约 3 分钟"一致。
const heartbeatSendTimeout = 5 * time.Second

// Heartbeat 是心跳保活循环。
//
// 生命周期由外层 ctx 控制 (来自 session.InternalContext()):
//   - ctx.Done → 退出,ticker.Stop 释放
//   - session.State != Registered → 不发送 (被 Stop 或注册失败)
//
// 计数规则 (spec Q4):
//   - 每次 MESSAGE 请求超时 / 失败 → failCount+1
//   - 每次 MESSAGE 成功 (200 OK 或任何非 error 响应) → failCount=0
//   - failCount 达到 gb28181.MaxHeartbeatFailBeforeDown → session.markFailed
type Heartbeat struct {
	session   *RegistrationSession
	interval  time.Duration
	failCount atomic.Int32
	snCounter atomic.Uint64
}

// NewHeartbeat 创建 (不启动) 心跳循环。interval <= 0 时用默认 60s。
func NewHeartbeat(session *RegistrationSession, interval time.Duration) *Heartbeat {
	if interval <= 0 {
		interval = time.Duration(gb28181.DefaultHeartbeatSecs) * time.Second
	}
	return &Heartbeat{session: session, interval: interval}
}

// Run 阻塞直至 ctx.Done。启动一个 time.Ticker,每 tick 发一条 Keepalive MESSAGE。
//
// 每次 tick 用 heartbeatSendTimeout 派生请求 ctx (不是外层 ctx),
// 避免单次 tick 的网络阻塞卡住整个 goroutine 的 cancel 响应。
func (h *Heartbeat) Run(ctx context.Context) {
	ticker := time.NewTicker(h.interval)
	defer ticker.Stop()

	slog.Info("heartbeat loop start",
		"device_id", h.session.cfg.DeviceID,
		"interval", h.interval)

	for {
		select {
		case <-ctx.Done():
			slog.Info("heartbeat loop exit", "reason", ctx.Err())
			return
		case <-ticker.C:
			h.tick(ctx)
		}
	}
}

// tick 发一次 Keepalive MESSAGE。
//
// 成功 → publish heartbeat_result{ok:true, consecutive_fails:0}
// 失败 → failCount+1, publish heartbeat_result{ok:false, consecutive_fails:N, error:...}
// 连续失败 >= MaxHeartbeatFailBeforeDown → session.markFailed
func (h *Heartbeat) tick(parentCtx context.Context) {
	// 短超时,防止一次心跳卡住循环
	reqCtx, cancel := context.WithTimeout(parentCtx, heartbeatSendTimeout)
	defer cancel()

	sn := h.snCounter.Add(1)
	req, err := h.buildKeepaliveRequest(int(sn))
	if err != nil {
		h.onFail(fmt.Errorf("build keepalive: %w", err))
		return
	}

	resp, err := h.session.client.Do(reqCtx, req)
	if err != nil {
		h.onFail(err)
		return
	}
	if resp != nil && resp.StatusCode >= 400 {
		h.onFail(fmt.Errorf("keepalive response %d %s", resp.StatusCode, resp.Reason))
		return
	}
	h.onSuccess()
}

// onSuccess 心跳成功:重置计数 + 推 ok 事件。
func (h *Heartbeat) onSuccess() {
	h.failCount.Store(0)
	h.session.publishEvent("heartbeat_result", map[string]any{
		"ok":                true,
		"consecutive_fails": 0,
		"ts_ms":             time.Now().UnixMilli(),
	}, false)
}

// onFail 心跳失败:计数+1,推 error 事件,达到阈值降级。
func (h *Heartbeat) onFail(err error) {
	// P0-2 fix: 先判 ctx.Canceled 再累加,避免 shutdown 期间误计数
	if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
		// 正常 shutdown 路径,不算失败
		return
	}

	n := h.failCount.Add(1)
	slog.Warn("heartbeat fail",
		"consecutive_fails", n,
		"error", err)

	payload := map[string]any{
		"ok":                false,
		"consecutive_fails": int(n),
		"error":             err.Error(),
		"ts_ms":             time.Now().UnixMilli(),
	}
	h.session.publishEvent("heartbeat_result", payload, false)

	if n >= int32(gb28181.MaxHeartbeatFailBeforeDown) {
		// 只降级一次:markFailed 内部 stopOnce 保证幂等
		h.session.markFailed("心跳连续失败")
		// 推 device_state:Failed (与 handlers.publishState 结构对齐)
		h.session.publishEvent("device_state", map[string]any{
			"state":                   "Failed",
			"registered_expires_secs": h.session.RegisteredExpires(),
			"reason":                  "心跳连续失败",
		}, true)
	}
}

// buildKeepaliveRequest 构造 Keepalive MESSAGE 请求。
//
// 复用 session.buildRegisterRequest 的 URI / From / To / Contact / Call-ID 逻辑
// 但 method=MESSAGE,加 Content-Type: Application/MANSCDP+xml + GB18030 编码的 body。
// CSeq 从 session.cseqCounter 单调递增 (spec Q10)。
func (h *Heartbeat) buildKeepaliveRequest(sn int) (*sip.Request, error) {
	cfg := h.session.cfg
	isTCP := strings.EqualFold(cfg.Transport, "tcp")

	// Request-URI 与 REGISTER 相同 (发给平台 AOR)
	requestURIStr := gb28181.BuildRegisterRequestURI(cfg)
	var requestURI sip.Uri
	if err := sip.ParseUri(requestURIStr, &requestURI); err != nil {
		return nil, fmt.Errorf("parse request-URI %q: %w", requestURIStr, err)
	}
	// M4 TCP: 加 transport=tcp 参数 (与 REGISTER 一致,让 LiveGBS 等平台按 TCP 路由)
	if isTCP {
		if requestURI.UriParams == nil {
			requestURI.UriParams = sip.NewParams()
		}
		requestURI.UriParams.Add("transport", "tcp")
	}

	req := sip.NewRequest(sip.MESSAGE, requestURI)

	// From (含 tag,与 REGISTER 同 tag)
	var fromURI sip.Uri
	if err := sip.ParseUri(gb28181.BuildFromURI(cfg), &fromURI); err != nil {
		return nil, fmt.Errorf("parse from-URI: %w", err)
	}
	fromParams := sip.NewParams()
	fromParams.Add("tag", h.session.fromTag)
	req.AppendHeader(&sip.FromHeader{
		Address: fromURI,
		Params:  fromParams,
	})

	// To (无 tag,发给平台 AOR - 心跳收发同 AOR 场景,不同于 REGISTER 的 To=自己)
	// 国标 A.2.5: Keepalive MESSAGE To 头用平台 SIP ID @ 平台域
	toURIStr := gb28181.BuildRegisterRequestURI(cfg)
	var toURI sip.Uri
	if err := sip.ParseUri(toURIStr, &toURI); err != nil {
		return nil, fmt.Errorf("parse to-URI: %w", err)
	}
	req.AppendHeader(&sip.ToHeader{
		Address: toURI,
		Params:  sip.NewParams(),
	})

	// Call-ID (全期复用)
	callID := sip.CallIDHeader(h.session.callID)
	req.AppendHeader(&callID)

	// CSeq (全局单调递增)
	seq := h.session.cseqCounter.Add(1)
	req.AppendHeader(&sip.CSeqHeader{
		SeqNo:      uint32(seq),
		MethodName: sip.MESSAGE,
	})

	// Max-Forwards
	maxFwd := sip.MaxForwardsHeader(70)
	req.AppendHeader(&maxFwd)

	// User-Agent
	req.AppendHeader(sip.NewHeader("User-Agent", gb28181.UserAgent))

	// Contact 头
	contactURI := sip.Uri{
		User: cfg.DeviceID,
		Host: cfg.ServerHost,
		Port: 0,
	}
	req.AppendHeader(&sip.ContactHeader{
		Address: contactURI,
		Params:  sip.NewParams(),
	})

	// Content-Type
	req.AppendHeader(sip.NewHeader("Content-Type", "Application/MANSCDP+xml"))

	// Body: Keepalive XML → GB18030
	xml := gb28181.BuildKeepaliveXML(gb28181.KeepaliveParams{
		SN:       sn,
		DeviceID: cfg.DeviceID,
	})
	body, err := gb28181.EncodeMANSCDPBody(xml)
	if err != nil {
		return nil, fmt.Errorf("encode MANSCDP body: %w", err)
	}
	req.SetBody(body) // 自动填 Content-Length

	// 显式目的地址 (与 REGISTER 同款,绕过 URI host DNS)
	req.SetDestination(fmt.Sprintf("%s:%d", cfg.ServerHost, cfg.ServerPort))

	return req, nil
}
