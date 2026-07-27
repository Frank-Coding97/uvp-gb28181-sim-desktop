package gb28181

import (
	"strings"
	"testing"
)

func TestBuildKeepaliveXML(t *testing.T) {
	got := BuildKeepaliveXML(KeepaliveParams{
		SN:       42,
		DeviceID: "34020000001320000001",
	})

	// 检查关键字段
	if !strings.Contains(got, `<?xml version="1.0" encoding="GB2312"?>`) {
		t.Error("missing XML declaration with GB2312 encoding")
	}
	if !strings.Contains(got, "<CmdType>Keepalive</CmdType>") {
		t.Error("missing CmdType Keepalive")
	}
	if !strings.Contains(got, "<SN>42</SN>") {
		t.Error("missing SN")
	}
	if !strings.Contains(got, "<DeviceID>34020000001320000001</DeviceID>") {
		t.Error("missing DeviceID")
	}
	if !strings.Contains(got, "<Status>OK</Status>") {
		t.Error("missing Status OK")
	}
	if !strings.HasSuffix(got, "</Notify>") {
		t.Error("should end with </Notify>")
	}
}

func TestEncodeMANSCDPBody(t *testing.T) {
	xml := BuildKeepaliveXML(KeepaliveParams{SN: 1, DeviceID: "34020000001320000001"})
	body, err := EncodeMANSCDPBody(xml)
	if err != nil {
		t.Fatal(err)
	}
	if len(body) == 0 {
		t.Error("body should not be empty")
	}
	// XML 全 ASCII, GB18030 编码后长度不变
	if len(body) != len(xml) {
		t.Errorf("ASCII-only XML byte length should equal string length, got %d vs %d", len(body), len(xml))
	}
}
