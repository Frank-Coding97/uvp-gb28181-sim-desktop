package device

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"time"

	"github.com/emiago/sipgo/sip"
)

// shutdownTimeout 是主动注销 REGISTER 的最长等待。
// spec AC-11: 6 秒内无响应也强制清理为 Disconnected。
const shutdownTimeout = 6 * time.Second

// sendExpiresZeroRegister 构造并发送一条 Expires:0 REGISTER 请求 (主动注销)。
//
// - 与常规 REGISTER 复用同一 Call-ID / From tag,CSeq 递增
// - Expires 头显式设 0 (覆盖 buildRegisterRequest 里的默认 cfg.ExpiresSecs)
// - 走完整 Digest 挑战 (client.Do 内 401 → AUTH → 200 自动处理)
// - ctx 超时或平台无响应时返错,由调用方决定是否强清
func (s *RegistrationSession) sendExpiresZeroRegister(ctx context.Context) error {
	req, err := s.buildRegisterRequest()
	if err != nil {
		return fmt.Errorf("build shutdown REGISTER: %w", err)
	}
	// 覆盖 Expires 头
	req.RemoveHeader("Expires")
	zero := sip.ExpiresHeader(0)
	req.AppendHeader(&zero)

	slog.Info("sending shutdown REGISTER (Expires=0)",
		"cseq", req.CSeq().SeqNo,
		"call_id", s.callID)

	resp, err := s.client.Do(ctx, req)
	if err != nil {
		return fmt.Errorf("shutdown REGISTER: %w", err)
	}
	if resp != nil && resp.StatusCode >= 400 {
		return fmt.Errorf("shutdown REGISTER %d %s", resp.StatusCode, resp.Reason)
	}
	return nil
}

// shutdownGracefully 是 Stop() 内部路径:
//   1. 若当前状态 == Registered → 派生 shutdownTimeout ctx,发 Expires=0 REGISTER
//   2. 不管成功/失败/超时,都置 Disconnected + cancel 根 ctx
//   3. publish device_state:Disconnected
//
// 只有 Registered 状态才发注销 (Failed / Disconnected / Registering 都跳过)。
func (s *RegistrationSession) shutdownGracefully() {
	state := s.State()
	if state == StateRegistered {
		ctx, cancel := context.WithTimeout(context.Background(), shutdownTimeout)
		if err := s.sendExpiresZeroRegister(ctx); err != nil {
			// 只 warn 不 error,继续走强清
			if errors.Is(err, context.DeadlineExceeded) {
				slog.Warn("shutdown REGISTER timeout, force cleanup", "timeout", shutdownTimeout)
			} else {
				slog.Warn("shutdown REGISTER error, force cleanup", "error", err)
			}
		}
		cancel()
	}

	// 关根 ctx → heartbeat / renewal 收 Done
	s.ctxMu.Lock()
	c := s.cancel
	s.ctxMu.Unlock()
	if c != nil {
		c()
	}
	s.state.Store(int32(StateDisconnected))
	s.publishEvent("device_state", map[string]any{
		"state":                   "Disconnected",
		"registered_expires_secs": 0,
	}, true)
}
