package gb28181

import (
	"errors"
	"strings"
	"testing"
)

func TestValidate_OK(t *testing.T) {
	cfg := &SipConfig{
		DeviceID:     "34020000001320000001",
		ServerHost:   "192.168.1.10",
		ServerPort:   5060,
		ServerID:     "34020000002000000001",
		ServerDomain: "3402000000",
		Password:     "12345678",
	}
	if err := cfg.Validate(); err != nil {
		t.Errorf("unexpected error: %v", err)
	}
}

func TestValidate_DeviceIDLength(t *testing.T) {
	cases := []struct {
		name string
		id   string
	}{
		{"太短", "3402"},
		{"太长", "34020000001320000001XX"},
		{"含字母", "34020000001320000ABC"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			cfg := &SipConfig{
				DeviceID:     tc.id,
				ServerHost:   "1.1.1.1",
				ServerPort:   5060,
				ServerDomain: "3402000000",
				Password:     "pw",
			}
			if err := cfg.Validate(); err == nil {
				t.Error("expected error for invalid device_id")
			}
		})
	}
}

func TestValidate_ServerPortRange(t *testing.T) {
	for _, port := range []int{0, -1, 65536} {
		cfg := &SipConfig{
			DeviceID:     "34020000001320000001",
			ServerHost:   "1.1.1.1",
			ServerPort:   port,
			ServerDomain: "3402000000",
			Password:     "pw",
		}
		if err := cfg.Validate(); err == nil {
			t.Errorf("port %d should be invalid", port)
		}
	}
}

func TestValidate_ServerIDOrDomainRequired(t *testing.T) {
	cfg := &SipConfig{
		DeviceID:   "34020000001320000001",
		ServerHost: "1.1.1.1",
		ServerPort: 5060,
		Password:   "pw",
	}
	err := cfg.Validate()
	if err == nil {
		t.Fatal("expected error when both server_id and server_domain empty")
	}
	if !strings.Contains(err.Error(), "至少填一项") {
		t.Errorf("error message should mention 至少填一项, got: %v", err)
	}
}

func TestValidate_TransportRestriction(t *testing.T) {
	cfg := &SipConfig{
		DeviceID:     "34020000001320000001",
		ServerHost:   "1.1.1.1",
		ServerPort:   5060,
		ServerDomain: "3402000000",
		Password:     "pw",
		Transport:    "tls",
	}
	if err := cfg.Validate(); err == nil {
		t.Error("expected error for unsupported tls transport")
	}
}

func TestApplyDefaults(t *testing.T) {
	cfg := &SipConfig{}
	cfg.ApplyDefaults()
	if cfg.Transport != "udp" {
		t.Errorf("default transport should be udp, got %q", cfg.Transport)
	}
	if cfg.HeartbeatIntervalSecs != DefaultHeartbeatSecs {
		t.Errorf("default heartbeat should be %d, got %d", DefaultHeartbeatSecs, cfg.HeartbeatIntervalSecs)
	}
	if cfg.ExpiresSecs != DefaultExpiresSecs {
		t.Errorf("default expires should be %d, got %d", DefaultExpiresSecs, cfg.ExpiresSecs)
	}
}

func TestEffectiveServerID_PriorityIDoverDomain(t *testing.T) {
	// spec Q5: 优先 ServerID
	cfg := &SipConfig{
		ServerID:     "34020000002000000001",
		ServerDomain: "3402000000",
	}
	if got := cfg.EffectiveServerID(); got != "34020000002000000001" {
		t.Errorf("should prefer ServerID, got %q", got)
	}
}

func TestEffectiveServerID_FallbackToDomain(t *testing.T) {
	cfg := &SipConfig{
		ServerID:     "",
		ServerDomain: "3402000000",
	}
	if got := cfg.EffectiveServerID(); got != "3402000000" {
		t.Errorf("should fallback to ServerDomain, got %q", got)
	}
}

var _ = errors.New // 保留 errors 用于将来 sentinel 测试
