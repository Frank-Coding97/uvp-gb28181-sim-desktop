//go:build integration

// Package integration 是 daemon 集成测试。
//
// 用独立 raw UDP mock (不依赖 sipgo) 起假平台,起 daemon 子进程发 REGISTER,
// 验证完整闭环。用 build tag "integration" 隔离,go test ./... 默认不跑。
// 显式 go test -tags=integration ./test/integration/... 才执行。
package integration

import (
	"context"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/internal/gb28181"
	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/test/mocks"
)

func TestE2E_RegisterAgainstRawMock(t *testing.T) {
	mock, err := mocks.StartRawMockPlatform(mocks.MockOpts{
		Realm:               "3402000000",
		Password:            "12345678",
		AuthAlgo:            "MD5",
		ExpiresReply:        3600,
	})
	if err != nil {
		t.Fatalf("start mock: %v", err)
	}
	defer mock.Stop()

	daemonBin := buildDaemon(t)
	cfgPath := writeConfig(t, &gb28181.SipConfig{
		DeviceID:     "34020000001320000001",
		ServerHost:   mock.Host(),
		ServerPort:   mock.Port(),
		ServerID:     "34020000002000000001",
		ServerDomain: "3402000000",
		Password:     "12345678",
		Transport:    "udp",
		ExpiresSecs:  3600,
	})

	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()

	stderr, err := runDaemon(ctx, daemonBin, "--mode", "once", "--config", cfgPath, "--log-level", "debug")
	if err != nil {
		t.Logf("daemon stderr:\n%s", stderr)
		t.Fatalf("daemon exit: %v", err)
	}

	// 验证 daemon log 含成功标记
	if !strings.Contains(stderr, "REGISTER 200 OK") {
		t.Errorf("daemon should log 'REGISTER 200 OK'\nstderr:\n%s", stderr)
	}

	// mock 平台侧断言
	reqs := mock.Received()
	if len(reqs) != 2 {
		t.Fatalf("expected 2 REGISTER (initial + auth), got %d\nrequests: %+v", len(reqs), reqs)
	}

	// 第 1 条无 Authorization
	if reqs[0].HasAuthorization {
		t.Error("first REGISTER should not have Authorization header")
	}
	// 第 2 条有 Authorization
	if !reqs[1].HasAuthorization {
		t.Error("second REGISTER should have Authorization header")
	}

	// spec Q10 验证:两次 REGISTER 的 Call-ID 一致
	if reqs[0].CallID != reqs[1].CallID {
		t.Errorf("Call-ID should be consistent, got %q vs %q", reqs[0].CallID, reqs[1].CallID)
	}
	// CSeq 递增
	if reqs[1].CSeqNum <= reqs[0].CSeqNum {
		t.Errorf("CSeq should be monotonic, got %d then %d", reqs[0].CSeqNum, reqs[1].CSeqNum)
	}
	// From tag 一致 (spec Q10:整个生命周期同一 tag)
	if reqs[0].FromTag != reqs[1].FromTag {
		t.Errorf("From tag should be same across requests, got %q vs %q", reqs[0].FromTag, reqs[1].FromTag)
	}
}

func TestE2E_WrongPassword(t *testing.T) {
	mock, err := mocks.StartRawMockPlatform(mocks.MockOpts{
		Realm:       "3402000000",
		Password:    "correct_password",
		AuthAlgo:    "MD5",
		ForceReject: 403,
	})
	if err != nil {
		t.Fatal(err)
	}
	defer mock.Stop()

	daemonBin := buildDaemon(t)
	cfgPath := writeConfig(t, &gb28181.SipConfig{
		DeviceID:     "34020000001320000001",
		ServerHost:   mock.Host(),
		ServerPort:   mock.Port(),
		ServerID:     "34020000002000000001",
		ServerDomain: "3402000000",
		Password:     "wrong_password",
		Transport:    "udp",
		ExpiresSecs:  3600,
	})

	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()

	stderr, err := runDaemon(ctx, daemonBin, "--mode", "once", "--config", cfgPath, "--log-level", "debug")
	if err == nil {
		t.Fatalf("daemon should fail with wrong password\nstderr:\n%s", stderr)
	}
	if !strings.Contains(stderr, "REGISTER") {
		t.Errorf("stderr should mention REGISTER failure\nstderr:\n%s", stderr)
	}
}

// --- helpers ---

func buildDaemon(t *testing.T) string {
	t.Helper()
	tmpDir := t.TempDir()
	binPath := filepath.Join(tmpDir, "uvp-daemon-test")
	// 项目根: 当前 test/integration → ../../ = apps/daemon
	daemonRoot := filepath.Join("..", "..")
	cmd := exec.Command("go", "build", "-o", binPath, "./cmd/uvp-daemon")
	cmd.Dir = daemonRoot
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("build daemon: %v\n%s", err, out)
	}
	return binPath
}

func writeConfig(t *testing.T, cfg *gb28181.SipConfig) string {
	t.Helper()
	tmpFile := filepath.Join(t.TempDir(), "cfg.json")
	data, err := json.Marshal(cfg)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(tmpFile, data, 0o644); err != nil {
		t.Fatal(err)
	}
	return tmpFile
}

func runDaemon(ctx context.Context, binPath string, args ...string) (string, error) {
	cmd := exec.CommandContext(ctx, binPath, args...)
	// stderr 是 slog 输出;stdout M1 未使用
	out, err := cmd.CombinedOutput()
	return string(out), err
}
