package manscdp

import (
	"errors"
	"testing"

	"golang.org/x/text/encoding/simplifiedchinese"
)

// Test1_ParseQuery_UTF8 UTF-8 入站.
func Test1_ParseQuery_UTF8(t *testing.T) {
	body := []byte(`<?xml version="1.0" encoding="UTF-8"?>
<Query>
  <CmdType>Catalog</CmdType>
  <SN>12345</SN>
  <DeviceID>34020000002000000001</DeviceID>
</Query>`)
	q, err := ParseQuery(body)
	if err != nil {
		t.Fatalf("parse failed: %v", err)
	}
	if q.CmdType != "Catalog" {
		t.Errorf("CmdType: got %q, want Catalog", q.CmdType)
	}
	if q.SN != 12345 {
		t.Errorf("SN: got %d, want 12345", q.SN)
	}
	if q.DeviceID != "34020000002000000001" {
		t.Errorf("DeviceID: got %q", q.DeviceID)
	}
}

// Test2_ParseQuery_GB18030_Chinese GB18030 入站 + 中文 (虽然 Query 通常没中文, 但 CmdType/DeviceID 全 ASCII 也要走 CharsetReader).
func Test2_ParseQuery_GB18030(t *testing.T) {
	xmlStr := `<?xml version="1.0" encoding="GB18030"?>
<Query>
  <CmdType>Catalog</CmdType>
  <SN>12345</SN>
  <DeviceID>34020000002000000001</DeviceID>
</Query>`
	body, err := simplifiedchinese.GB18030.NewEncoder().Bytes([]byte(xmlStr))
	if err != nil {
		t.Fatalf("encode setup failed: %v", err)
	}
	q, err := ParseQuery(body)
	if err != nil {
		t.Fatalf("parse GB18030 failed: %v", err)
	}
	if q.CmdType != "Catalog" || q.SN != 12345 {
		t.Errorf("GB18030 解析结果错误: %+v", q)
	}
}

// Test3_ParseQuery_GB2312 GB2312 入站 (老平台方言).
func Test3_ParseQuery_GB2312(t *testing.T) {
	xmlStr := `<?xml version="1.0" encoding="GB2312"?>
<Query>
  <CmdType>DeviceInfo</CmdType>
  <SN>888</SN>
  <DeviceID>34020000002000000001</DeviceID>
</Query>`
	body, err := simplifiedchinese.GBK.NewEncoder().Bytes([]byte(xmlStr))
	if err != nil {
		t.Fatalf("encode setup failed: %v", err)
	}
	q, err := ParseQuery(body)
	if err != nil {
		t.Fatalf("parse GB2312 failed: %v", err)
	}
	if q.CmdType != "DeviceInfo" || q.SN != 888 {
		t.Errorf("GB2312 解析结果错误: %+v", q)
	}
}

// Test4_ParseQuery_MissingCmdType 缺 CmdType 返错.
func Test4_ParseQuery_MissingCmdType(t *testing.T) {
	body := []byte(`<?xml version="1.0" encoding="UTF-8"?>
<Query>
  <SN>12345</SN>
  <DeviceID>34020000002000000001</DeviceID>
</Query>`)
	_, err := ParseQuery(body)
	if err == nil {
		t.Fatal("期望缺 CmdType 返错")
	}
	if !errors.Is(err, ErrParseQuery) {
		t.Errorf("期望 ErrParseQuery, 实际 %v", err)
	}
}

// Test5_ParseQuery_MissingSN 缺 SN 返错.
func Test5_ParseQuery_MissingSN(t *testing.T) {
	body := []byte(`<?xml version="1.0" encoding="UTF-8"?>
<Query>
  <CmdType>Catalog</CmdType>
  <DeviceID>34020000002000000001</DeviceID>
</Query>`)
	_, err := ParseQuery(body)
	if err == nil {
		t.Fatal("期望缺 SN 返错")
	}
}

// Test6_ParseQuery_MissingDeviceID 缺 DeviceID 返错.
func Test6_ParseQuery_MissingDeviceID(t *testing.T) {
	body := []byte(`<?xml version="1.0" encoding="UTF-8"?>
<Query>
  <CmdType>Catalog</CmdType>
  <SN>1</SN>
</Query>`)
	_, err := ParseQuery(body)
	if err == nil {
		t.Fatal("期望缺 DeviceID 返错")
	}
}

// Test7_ParseQuery_MalformedXML 畸形 XML 返错.
func Test7_ParseQuery_MalformedXML(t *testing.T) {
	body := []byte(`<not xml at all`)
	_, err := ParseQuery(body)
	if err == nil {
		t.Fatal("期望畸形 XML 返错")
	}
}

// Test8_ParseQuery_EmptyBody 空 body 返错.
func Test8_ParseQuery_EmptyBody(t *testing.T) {
	_, err := ParseQuery([]byte{})
	if err == nil {
		t.Fatal("期望空 body 返错")
	}
}

// Test9_ParseQuery_XMLDeclarationWithoutEncoding 无 encoding 声明 (默认 UTF-8).
func Test9_ParseQuery_XMLDeclarationWithoutEncoding(t *testing.T) {
	body := []byte(`<?xml version="1.0"?>
<Query>
  <CmdType>DeviceStatus</CmdType>
  <SN>1</SN>
  <DeviceID>34020000002000000001</DeviceID>
</Query>`)
	q, err := ParseQuery(body)
	if err != nil {
		t.Fatalf("无 encoding 声明应默认 UTF-8: %v", err)
	}
	if q.CmdType != "DeviceStatus" {
		t.Errorf("解析错误: %+v", q)
	}
}

// Test10_ParseQuery_UnknownCmdType 未知 CmdType 也能解析出来 (业务层处理, 解析器不判断).
func Test10_ParseQuery_UnknownCmdType(t *testing.T) {
	body := []byte(`<?xml version="1.0" encoding="UTF-8"?>
<Query>
  <CmdType>RecordInfo</CmdType>
  <SN>1</SN>
  <DeviceID>34020000002000000001</DeviceID>
</Query>`)
	q, err := ParseQuery(body)
	if err != nil {
		t.Fatalf("未知 CmdType 应解析成功 (业务层拒绝): %v", err)
	}
	if q.CmdType != "RecordInfo" {
		t.Errorf("CmdType: got %q, want RecordInfo", q.CmdType)
	}
}
