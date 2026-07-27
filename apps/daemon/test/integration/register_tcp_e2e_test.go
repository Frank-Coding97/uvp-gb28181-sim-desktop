//go:build integration

package integration

import (
	"context"
	"io"
	"os/exec"
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

// TestE2E_TCPLongConnection_MultipleRequests 验证 TCP 长连接复用多个请求。
//
// TCP 传输核心:1 REGISTER + 多次心跳 MESSAGE 应在**同一个 TCP 连接**上分帧发送。
// 反同源污染:mock 用原生 net.Listen 手工分帧,验证 daemon 生成的字节流符合 RFC 4571。
func TestE2E_TCPLongConnection_MultipleRequests(t *testing.T) {
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
		DeviceID:              "34020000001320000001",
		ServerHost:            mock.Host(),
		ServerPort:            mock.Port(),
		ServerID:              "34020000002000000001",
		ServerDomain:          "3402000000",
		Password:              "12345678",
		Transport:             "tcp",
		ExpiresSecs:           3600,
		HeartbeatIntervalSecs: 2, // 2s 短周期,快速触发心跳
	})

	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()

	// daemon 后台跑 (不用 --mode once,让心跳循环起来)
	cmd := exec.CommandContext(ctx, daemonBin, "--config", cfgPath, "--log-level", "debug")
	stderr, err := cmd.StderrPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	defer func() {
		cmd.Process.Kill()
		cmd.Wait()
	}()

	// 等 8 秒,应该走了: 1 REGISTER (2 个报文:初始 + auth) + 至少 3 次心跳 MESSAGE
	time.Sleep(8 * time.Second)

	stderrBytes, _ := io.ReadAll(stderr)
	stderrStr := string(stderrBytes)

	if !strings.Contains(stderrStr, "REGISTER 200 OK") {
		t.Errorf("daemon should log 'REGISTER 200 OK'\nstderr:\n%s", stderrStr)
	}

	reqs := mock.Received()
	// 至少 5 个请求: 2 REGISTER (初始 + auth) + 3 MESSAGE (心跳)
	if len(reqs) < 5 {
		t.Fatalf("expected >=5 requests (2 REGISTER + 3 MESSAGE), got %d\nrequests: %+v",
			len(reqs), reqs)
	}

	// 验证前 2 个是 REGISTER
	if !strings.Contains(string(reqs[0].RawBytes), "REGISTER") {
		t.Errorf("request #0 should be REGISTER, got:\n%s", string(reqs[0].RawBytes))
	}
	if !strings.Contains(string(reqs[1].RawBytes), "REGISTER") {
		t.Errorf("request #1 should be REGISTER, got:\n%s", string(reqs[1].RawBytes))
	}

	// 后续应该是 MESSAGE (心跳)
	messageCount := 0
	for i := 2; i < len(reqs); i++ {
		if strings.Contains(string(reqs[i].RawBytes), "MESSAGE") {
			messageCount++
		}
	}
	if messageCount < 3 {
		t.Errorf("expected >=3 MESSAGE (heartbeat), got %d", messageCount)
	}

	// 验证 TCP 长连接复用:mock 的 ConnectionID 字段应该全部一致
	// (mocks.RawTCPMockPlatform 每个连接分配唯一 ID,同连接的请求 ID 相同)
	firstConnID := reqs[0].ConnectionID
	for i, r := range reqs {
		if r.ConnectionID != firstConnID {
			t.Errorf("request #%d on different connection: expected %s, got %s",
				i, firstConnID, r.ConnectionID)
		}
	}

	t.Logf("TCP long-connection test passed: %d requests on same connection (ID=%s)",
		len(reqs), firstConnID)
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
