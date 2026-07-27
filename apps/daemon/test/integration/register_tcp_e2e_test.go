//go:build integration

package integration

import (
	"context"
	"strings"
	"testing"
	"time"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/internal/gb28181"
	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/test/mocks"
)

// TestE2E_TCPRegisterAgainstRawMock:
// 起 raw TCP mock 平台,daemon --mode once + transport=tcp 应完整跑完
// REGISTER → 401 → AUTH → 200 OK,且 transport 参数正确出现在报文里。
//
// 反同源污染:mock 用原生 net.Listen + 手工分帧,不依赖 sipgo,
// 验证 daemon 生成的字节确实是标准 SIP over TCP。
func TestE2E_TCPRegisterAgainstRawMock(t *testing.T) {
	mock, err := mocks.StartRawTCPMockPlatform(mocks.MockOpts{
		Realm:        "3402000000",
		Password:     "12345678",
		AuthAlgo:     "MD5",
		Qop:          "auth",
		ExpiresReply: 3600,
	})
	if err != nil {
		t.Fatalf("start tcp mock: %v", err)
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
		Transport:    "tcp",
		ExpiresSecs:  3600,
	})

	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()

	stderr, err := runDaemon(ctx, daemonBin, "--mode", "once", "--config", cfgPath, "--log-level", "debug")
	if err != nil {
		t.Logf("daemon stderr:\n%s", stderr)
		t.Fatalf("daemon exit: %v", err)
	}

	if !strings.Contains(stderr, "REGISTER 200 OK") {
		t.Errorf("daemon should log 'REGISTER 200 OK'\nstderr:\n%s", stderr)
	}

	reqs := mock.Received()
	if len(reqs) != 2 {
		t.Fatalf("expected 2 REGISTER (initial + auth), got %d\nrequests: %+v",
			len(reqs), reqs)
	}

	// 第 1 条无 Authorization
	if reqs[0].HasAuthorization {
		t.Error("first REGISTER should not have Authorization header")
	}
	// 第 2 条必须带 Authorization
	if !reqs[1].HasAuthorization {
		t.Error("second REGISTER should have Authorization header")
	}

	// TCP 特有:两条 REGISTER 都应在 Request-URI 或报文里包含 transport=tcp
	for i, r := range reqs {
		raw := strings.ToLower(string(r.RawBytes))
		if !strings.Contains(raw, "transport=tcp") {
			t.Errorf("REGISTER #%d should contain transport=tcp\nraw:\n%s",
				i, string(r.RawBytes))
		}
	}

	// spec Q10:Call-ID / From tag 一致,CSeq 单调递增
	if reqs[0].CallID != reqs[1].CallID {
		t.Errorf("Call-ID should be consistent, got %q vs %q",
			reqs[0].CallID, reqs[1].CallID)
	}
	if reqs[1].CSeqNum <= reqs[0].CSeqNum {
		t.Errorf("CSeq should be monotonic, got %d then %d",
			reqs[0].CSeqNum, reqs[1].CSeqNum)
	}
	if reqs[0].FromTag != reqs[1].FromTag {
		t.Errorf("From tag should be same across requests, got %q vs %q",
			reqs[0].FromTag, reqs[1].FromTag)
	}
}

// TestE2E_TCPWrongPassword:TCP 通道下密码错误也应 403 快速失败。
func TestE2E_TCPWrongPassword(t *testing.T) {
	mock, err := mocks.StartRawTCPMockPlatform(mocks.MockOpts{
		Realm:       "3402000000",
		Password:    "correct",
		AuthAlgo:    "MD5",
		Qop:         "auth",
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
		Password:     "wrong",
		Transport:    "tcp",
		ExpiresSecs:  3600,
	})

	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()

	stderr, err := runDaemon(ctx, daemonBin, "--mode", "once", "--config", cfgPath, "--log-level", "debug")
	if err == nil {
		t.Fatalf("daemon should fail with wrong password (tcp)\nstderr:\n%s", stderr)
	}
	if !strings.Contains(stderr, "REGISTER") {
		t.Errorf("stderr should mention REGISTER failure\nstderr:\n%s", stderr)
	}
}
