//go:build longrun

package longrun

import (
	"context"
	"runtime"
	"testing"
	"time"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/internal/device"
	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/internal/gb28181"
	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/test/mocks"
)

// TestRegister_30MinNoLeak 验证 session 长时间运行无 goroutine 泄漏。
//
// M3 并发场景 (session ctx 树 + heartbeat/renewal goroutine) 容易泄漏,
// 此测试挂 30 分钟,每 5 分钟检查一次 goroutine 数是否稳定。
//
// 运行: go test -tags=longrun -timeout=35m ./test/longrun/...
// (或手工指定测试: go test -tags=longrun -run=TestRegister_30MinNoLeak)
func TestRegister_30MinNoLeak(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping 30-min test in short mode")
	}

	// 起 mock 平台 (UDP,简单场景)
	mock, err := mocks.StartRawUDPMockPlatform(mocks.MockOpts{
		Realm:        "3402000000",
		Password:     "12345678",
		AuthAlgo:     "MD5",
		Qop:          "auth",
		ExpiresReply: 3600,
	})
	if err != nil {
		t.Fatalf("start mock platform: %v", err)
	}
	defer mock.Stop()

	// 起 session (短心跳周期 2s,让 goroutine 更活跃)
	cfg := &gb28181.SipConfig{
		DeviceID:              "34020000001320000001",
		ServerHost:            mock.Host(),
		ServerPort:            mock.Port(),
		ServerID:              "34020000002000000001",
		ServerDomain:          "3402000000",
		Password:              "12345678",
		Transport:             "udp",
		ExpiresSecs:           3600,
		HeartbeatIntervalSecs: 2,
	}

	client := mocks.NewMockSipClient(mock)
	session := device.NewRegistrationSession(client, cfg)

	ctx := context.Background()
	if err := session.Start(ctx); err != nil {
		t.Fatalf("session.Start: %v", err)
	}
	defer session.Stop()

	// 记录初始 goroutine 数 (GC 后稳定)
	runtime.GC()
	time.Sleep(100 * time.Millisecond)
	baseline := runtime.NumGoroutine()
	t.Logf("baseline goroutines: %d", baseline)

	// 挂 30 分钟,每 5 分钟检查一次
	for i := 0; i < 6; i++ {
		time.Sleep(5 * time.Minute)
		runtime.GC()
		time.Sleep(100 * time.Millisecond)
		current := runtime.NumGoroutine()
		t.Logf("minute %d: goroutines=%d (baseline=%d, delta=%+d)",
			(i+1)*5, current, baseline, current-baseline)

		// 容忍 +5 (网络栈偶尔临时 goroutine),超过即泄漏
		if current > baseline+5 {
			t.Errorf("minute %d: goroutine leak detected, baseline=%d current=%d",
				(i+1)*5, baseline, current)
		}
	}

	t.Logf("30-min test passed, no leak detected")
}
