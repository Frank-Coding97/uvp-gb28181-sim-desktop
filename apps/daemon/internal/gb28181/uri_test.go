package gb28181

import "testing"

func TestBuildRegisterRequestURI(t *testing.T) {
	cfg := &SipConfig{
		ServerID:     "34020000002000000001",
		ServerDomain: "3402000000",
	}
	got := BuildRegisterRequestURI(cfg)
	want := "sip:34020000002000000001@3402000000"
	if got != want {
		t.Errorf("got %q, want %q", got, want)
	}
}

func TestServerIDFallback(t *testing.T) {
	// spec Q5: ServerID 留空回退 ServerDomain
	cfg := &SipConfig{
		ServerID:     "",
		ServerDomain: "3402000000",
	}
	got := BuildRegisterRequestURI(cfg)
	want := "sip:3402000000@3402000000"
	if got != want {
		t.Errorf("got %q, want %q", got, want)
	}
}

func TestBuildDeviceContactURI(t *testing.T) {
	cfg := &SipConfig{DeviceID: "34020000001320000001"}
	got := BuildDeviceContactURI(cfg, "192.168.1.100", 5060)
	want := "sip:34020000001320000001@192.168.1.100:5060"
	if got != want {
		t.Errorf("got %q, want %q", got, want)
	}
}

func TestBuildFromURI(t *testing.T) {
	cfg := &SipConfig{
		DeviceID:     "34020000001320000001",
		ServerDomain: "3402000000",
	}
	got := BuildFromURI(cfg)
	want := "sip:34020000001320000001@3402000000"
	if got != want {
		t.Errorf("got %q, want %q", got, want)
	}
}

func TestBuildToURI_SameAsFrom(t *testing.T) {
	// RFC 3261 §10.2: REGISTER 的 To 与 From 都是设备 AOR
	cfg := &SipConfig{
		DeviceID:     "34020000001320000001",
		ServerDomain: "3402000000",
	}
	if BuildToURI(cfg) != BuildFromURI(cfg) {
		t.Error("REGISTER 的 To 应与 From 相同")
	}
}
