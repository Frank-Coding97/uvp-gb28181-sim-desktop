package device

import (
	"context"
	"fmt"
	"log/slog"
	"time"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/internal/gb28181"
)

// renewalMinFloor 是续约触发时机下限保护。
//
// 若 expires 极短 (< 5s),按公式算会得到 <= 0,退化到此下限。
// 500ms 兼顾 (a) 生产场景不会用这么短 (b) 测试可跑短周期不阻塞。
const renewalMinFloor = 500 * time.Millisecond

// Renewal 是到期续约循环。
//
// 触发时机 (spec Q9):
//   renewAt = max(expires * 0.8, expires - 60)  // 至少留 60s 余量
//   若 expires <= 60s → renewAt = expires - 5   // 极短周期兜底
//
// 每次续约成功后重新计算触发点,递归排下一个 timer。
// 生命周期由外层 ctx 控制,cancel 后 timer.Stop 立即释放。
type Renewal struct {
	session *RegistrationSession
}

// NewRenewal 创建续约循环 (不启动)。
func NewRenewal(session *RegistrationSession) *Renewal {
	return &Renewal{session: session}
}

// Run 阻塞直至 ctx.Done 或 session 迁移到 Failed。
//
// 每一轮:
//   1. 计算 renewAt (基于 session.RegisteredExpires())
//   2. 等 timer 到期 or ctx.Done
//   3. 到期 → session.reregister()
//      - 成功 → 更新 RegisteredExpires,状态保持 Registered,进入下一轮
//      - 失败 → session.markFailed,循环退出
func (r *Renewal) Run(ctx context.Context) {
	slog.Info("renewal loop start", "device_id", r.session.cfg.DeviceID)

	for {
		wait := computeRenewalWait(r.session.RegisteredExpires())
		timer := time.NewTimer(wait)

		select {
		case <-ctx.Done():
			timer.Stop()
			slog.Info("renewal loop exit", "reason", ctx.Err())
			return
		case <-timer.C:
			if err := r.session.reregister(ctx); err != nil {
				slog.Warn("renewal failed", "error", err)
				r.session.publishEvent("device_state", map[string]any{
					"state":                   "Failed",
					"registered_expires_secs": r.session.RegisteredExpires(),
					"reason":                  fmt.Sprintf("续约失败: %v", err),
				}, true)
				r.session.markFailed("续约失败")
				return
			}
			// 成功:状态保持 Registered,进入下一轮 (RegisteredExpires 已在 reregister 内更新)
			slog.Info("renewal success",
				"new_expires_secs", r.session.RegisteredExpires())
			r.session.publishEvent("renewal_result", map[string]any{
				"ok":                      true,
				"registered_expires_secs": r.session.RegisteredExpires(),
				"ts_ms":                   time.Now().UnixMilli(),
			}, false)
		}
	}
}

// computeRenewalWait 按 spec Q9 计算下次续约触发的等待时长。
//
//   expires 秒 → wait = max(expires*0.8, expires-60) 秒
//   expires <= 60s → wait = max(expires-5, 5s) 秒 (极短周期兜底)
//   expires <= 0 → wait = 5s (完全无效值时快速尝试)
func computeRenewalWait(expiresSecs int) time.Duration {
	if expiresSecs <= 0 {
		return renewalMinFloor
	}
	if expiresSecs <= 60 {
		w := time.Duration(expiresSecs-5) * time.Second
		if w < renewalMinFloor {
			w = renewalMinFloor
		}
		return w
	}
	ratio := time.Duration(float64(expiresSecs)*gb28181.ExpiresRenewalRatio) * time.Second
	margin := time.Duration(expiresSecs-60) * time.Second
	if ratio > margin {
		return ratio
	}
	return margin
}
