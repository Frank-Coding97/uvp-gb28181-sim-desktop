package manscdp

import (
	"bytes"
	"errors"
	"strings"
	"testing"

	"golang.org/x/text/encoding/simplifiedchinese"
)

// sampleCatalogResponse 构造一个通用测试响应.
func sampleCatalogResponse() *CatalogResponse {
	return &CatalogResponse{
		CmdType:  "Catalog",
		SN:       12345,
		DeviceID: "34020000002000000001",
		SumNum:   1,
		DeviceList: &DeviceList{
			Num: 1,
			Items: []CatalogItem{{
				DeviceID:     "34020000001320000001",
				Name:         "摄像头一号",
				Manufacturer: "UVP",
				Model:        "SIM-Desktop-v2",
				Owner:        "UVP-Simulator",
				CivilCode:    "340200",
				Address:      "办公楼一层",
				Parental:     0,
				ParentID:     "34020000002000000001",
				SafetyWay:    0,
				RegisterWay:  1,
				Secrecy:      0,
				Status:       "ON",
			}},
		},
	}
}

// Test1_EncodeCatalogResponse_GB18030 GB18030 编码 + 中文往返.
func Test1_EncodeCatalogResponse_GB18030(t *testing.T) {
	body, err := EncodeResponse(sampleCatalogResponse(), EncodingGB18030)
	if err != nil {
		t.Fatalf("encode failed: %v", err)
	}
	// 声明必须是 GB18030
	if !bytes.HasPrefix(body, []byte(`<?xml version="1.0" encoding="GB18030"?>`)) {
		t.Errorf("GB18030 声明缺失, body 前 60 字节: %q", body[:min(60, len(body))])
	}
	// 反解码验证中文可解回来
	utf8, err := simplifiedchinese.GB18030.NewDecoder().Bytes(body)
	if err != nil {
		t.Fatalf("GB18030 decode failed: %v", err)
	}
	if !strings.Contains(string(utf8), "摄像头一号") {
		t.Errorf("GB18030 编码后无法解回中文: %s", string(utf8))
	}
}

// Test2_EncodeCatalogResponse_GB2312 GB2312 声明 + GBK 编码器 (常用字).
func Test2_EncodeCatalogResponse_GB2312(t *testing.T) {
	body, err := EncodeResponse(sampleCatalogResponse(), EncodingGB2312)
	if err != nil {
		t.Fatalf("encode failed: %v", err)
	}
	if !bytes.HasPrefix(body, []byte(`<?xml version="1.0" encoding="GB2312"?>`)) {
		t.Errorf("GB2312 声明缺失, body 前 60 字节: %q", body[:min(60, len(body))])
	}
	// GBK 反解验证
	utf8, err := simplifiedchinese.GBK.NewDecoder().Bytes(body)
	if err != nil {
		t.Fatalf("GBK decode failed: %v", err)
	}
	if !strings.Contains(string(utf8), "摄像头一号") {
		t.Errorf("GBK 编码后无法解回中文")
	}
}

// Test3_EncodeCatalogResponse_UTF8 UTF-8 编码 + 中文.
func Test3_EncodeCatalogResponse_UTF8(t *testing.T) {
	body, err := EncodeResponse(sampleCatalogResponse(), EncodingUTF8)
	if err != nil {
		t.Fatalf("encode failed: %v", err)
	}
	if !bytes.HasPrefix(body, []byte(`<?xml version="1.0" encoding="UTF-8"?>`)) {
		t.Errorf("UTF-8 声明缺失")
	}
	// UTF-8 原生, 直接检查中文
	if !strings.Contains(string(body), "摄像头一号") {
		t.Errorf("UTF-8 中文未找到")
	}
}

// Test4_DeclarationMatchesActualEncoding S-B1 硬约束: 声明与实际字节严格对应.
func Test4_DeclarationMatchesActualEncoding(t *testing.T) {
	resp := sampleCatalogResponse()

	// GB18030 声明 -> 实际字节必须能被 GB18030 decoder 解回 UTF-8 中文
	gb18030Body, _ := EncodeResponse(resp, EncodingGB18030)
	if _, err := simplifiedchinese.GB18030.NewDecoder().Bytes(gb18030Body); err != nil {
		t.Errorf("GB18030 声明配 GB18030 编码器, decoder 应能解回: %v", err)
	}

	// GB2312 声明 -> 实际是 GBK 字节, GBK decoder 能解回
	gb2312Body, _ := EncodeResponse(resp, EncodingGB2312)
	if _, err := simplifiedchinese.GBK.NewDecoder().Bytes(gb2312Body); err != nil {
		t.Errorf("GB2312 声明配 GBK 编码器, decoder 应能解回: %v", err)
	}

	// 反面: GB18030 body 不应被认为是 UTF-8 (中文字节序列不同)
	if bytes.Contains(gb18030Body[50:], []byte("摄像头一号")) {
		t.Errorf("GB18030 body 不应含原生 UTF-8 中文字节")
	}
}

// Test5_ContentLength_IsEncodedByteCount Content-Length 语义 = 编码后字节数.
func Test5_ContentLength_IsEncodedByteCount(t *testing.T) {
	resp := sampleCatalogResponse()

	// UTF-8 body 应比 GB18030 body 长 (中文 3 字节 vs 2 字节)
	utf8Body, _ := EncodeResponse(resp, EncodingUTF8)
	gb18030Body, _ := EncodeResponse(resp, EncodingGB18030)

	if len(utf8Body) <= len(gb18030Body) {
		t.Errorf("UTF-8 应比 GB18030 长 (中文 3B vs 2B): utf8=%d gb18030=%d", len(utf8Body), len(gb18030Body))
	}

	// 都必须 > 200 (最小合理长度)
	if len(gb18030Body) < 200 {
		t.Errorf("GB18030 body 过短: %d", len(gb18030Body))
	}
}

// Test6_CatalogResponse_HasAll13Fields S-H3: 13 字段全输出.
func Test6_CatalogResponse_HasAll13Fields(t *testing.T) {
	body, _ := EncodeResponse(sampleCatalogResponse(), EncodingUTF8)
	xmlStr := string(body)

	required := []string{
		"<DeviceID>", "<Name>", "<Manufacturer>", "<Model>",
		"<Owner>", "<CivilCode>", "<Address>",
		"<Parental>", "<ParentID>",
		"<SafetyWay>", "<RegisterWay>", "<Secrecy>",
		"<Status>",
	}
	for _, tag := range required {
		if !strings.Contains(xmlStr, tag) {
			t.Errorf("CatalogItem 缺字段 %s", tag)
		}
	}
}

// Test7_DeviceStatusResponse_AlarmstatusForced S-H4: Alarmstatus 强制输出.
func Test7_DeviceStatusResponse_AlarmstatusForced(t *testing.T) {
	resp := &DeviceStatusResponse{
		CmdType:    "DeviceStatus",
		SN:         12347,
		DeviceID:   "34020000002000000001",
		Result:     "OK",
		Online:     "ONLINE",
		Status:     "OK",
		Encode:     "ON",
		Record:     "OFF",
		DeviceTime: "2026-07-28T09:30:00",
		Alarmstatus: AlarmstatusBlock{
			Num:   0,
			Items: nil,
		},
	}
	body, err := EncodeResponse(resp, EncodingUTF8)
	if err != nil {
		t.Fatalf("encode failed: %v", err)
	}
	xmlStr := string(body)
	if !strings.Contains(xmlStr, `<Alarmstatus Num="0">`) {
		t.Errorf("Alarmstatus Num=\"0\" 未强制输出, body: %s", xmlStr)
	}
}

// Test8_UnsupportedEncoding_ReturnsError 未知编码返错.
func Test8_UnsupportedEncoding_ReturnsError(t *testing.T) {
	_, err := EncodeResponse(sampleCatalogResponse(), Encoding("Latin1"))
	if err == nil {
		t.Fatal("期望返 ErrUnsupportedEncoding, 实际 nil")
	}
	if !errors.Is(err, ErrUnsupportedEncoding) {
		t.Errorf("期望 ErrUnsupportedEncoding, 实际 %v", err)
	}
}

// Test9_DeviceInfoResponse_HasAllFields DeviceInfo 6 字段完整.
func Test9_DeviceInfoResponse_HasAllFields(t *testing.T) {
	resp := &DeviceInfoResponse{
		CmdType:      "DeviceInfo",
		SN:           12346,
		DeviceID:     "34020000002000000001",
		Result:       "OK",
		DeviceName:   "GB28181 模拟设备",
		Manufacturer: "UVP",
		Model:        "SIM-Desktop-v2",
		Firmware:     "v0.2.0",
		Channel:      5,
	}
	body, err := EncodeResponse(resp, EncodingUTF8)
	if err != nil {
		t.Fatalf("encode failed: %v", err)
	}
	xmlStr := string(body)
	required := []string{
		"<CmdType>DeviceInfo</CmdType>",
		"<Result>OK</Result>",
		"<DeviceName>GB28181 模拟设备</DeviceName>",
		"<Manufacturer>UVP</Manufacturer>",
		"<Firmware>v0.2.0</Firmware>",
		"<Channel>5</Channel>",
	}
	for _, tag := range required {
		if !strings.Contains(xmlStr, tag) {
			t.Errorf("DeviceInfo 缺字段: %s", tag)
		}
	}
}

func min(a, b int) int {
	if a < b {
		return a
	}
	return b
}
