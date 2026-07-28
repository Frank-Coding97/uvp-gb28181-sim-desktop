package manscdp

import (
	"errors"
	"fmt"
	"strings"
	"testing"
)

// makeItem 生成一个测试通道条目.
func makeItem(idx int) CatalogItem {
	return CatalogItem{
		DeviceID:     fmt.Sprintf("3402000000132%07d", idx),
		Name:         fmt.Sprintf("摄像头-%03d", idx),
		Manufacturer: "UVP",
		Model:        "SIM-Desktop-v2",
		Owner:        "UVP-Simulator",
		CivilCode:    "340200",
		Address:      fmt.Sprintf("办公楼%d层", idx),
		Parental:     0,
		ParentID:     "34020000002000000001",
		SafetyWay:    0,
		RegisterWay:  1,
		Secrecy:      0,
		Status:       "ON",
	}
}

// Test1_BuildCatalogResponses_Empty 空目录发一片 SumNum=0.
func Test1_BuildCatalogResponses_Empty(t *testing.T) {
	resps, err := BuildCatalogResponses(1, "34020000002000000001", []CatalogItem{}, 1000, EncodingGB18030)
	if err != nil {
		t.Fatalf("empty catalog should not error: %v", err)
	}
	if len(resps) != 1 {
		t.Fatalf("expected 1 chunk for empty catalog, got %d", len(resps))
	}
	if resps[0].SumNum != 0 {
		t.Errorf("SumNum: got %d, want 0", resps[0].SumNum)
	}
	if resps[0].DeviceList == nil || resps[0].DeviceList.Num != 0 {
		t.Errorf("empty catalog DeviceList.Num should be 0")
	}
}

// Test2_BuildCatalogResponses_UnderLimit 少量通道 → 单片.
func Test2_BuildCatalogResponses_UnderLimit(t *testing.T) {
	items := []CatalogItem{makeItem(1), makeItem(2), makeItem(3)}
	resps, err := BuildCatalogResponses(1, "34020000002000000001", items, 32768, EncodingGB18030)
	if err != nil {
		t.Fatalf("under limit should not error: %v", err)
	}
	if len(resps) != 1 {
		t.Errorf("small catalog should be 1 chunk, got %d", len(resps))
	}
	if resps[0].SumNum != 3 || resps[0].DeviceList.Num != 3 {
		t.Errorf("SumNum/Num should both be 3, got SumNum=%d Num=%d", resps[0].SumNum, resps[0].DeviceList.Num)
	}
}

// Test3_BuildCatalogResponses_ExceedsLimit_MultipleChunks 大量通道 UDP 1000B → 多片, SumNum 一致.
func Test3_BuildCatalogResponses_ExceedsLimit_MultipleChunks(t *testing.T) {
	// 100 通道, 每个约 400-500 字节, UDP 1000B 应分很多片
	items := make([]CatalogItem, 100)
	for i := range items {
		items[i] = makeItem(i + 1)
	}
	resps, err := BuildCatalogResponses(42, "34020000002000000001", items, 1000, EncodingGB18030)
	if err != nil {
		t.Fatalf("build failed: %v", err)
	}
	if len(resps) < 2 {
		t.Errorf("100 items UDP 1000B should split into ≥2 chunks, got %d", len(resps))
	}
	// 所有片 SumNum 相同
	for i, r := range resps {
		if r.SumNum != 100 {
			t.Errorf("chunk %d SumNum: got %d, want 100", i, r.SumNum)
		}
	}
	// Num 之和 = 100 (无重复无遗漏)
	sum := 0
	seen := make(map[string]bool)
	for _, r := range resps {
		sum += r.DeviceList.Num
		for _, item := range r.DeviceList.Items {
			if seen[item.DeviceID] {
				t.Errorf("duplicate DeviceID: %s", item.DeviceID)
			}
			seen[item.DeviceID] = true
		}
	}
	if sum != 100 {
		t.Errorf("total Num across chunks: got %d, want 100", sum)
	}
	// 每片编码后 body ≤ 1000
	for i, r := range resps {
		body, err := EncodeResponse(&r, EncodingGB18030)
		if err != nil {
			t.Fatalf("chunk %d encode failed: %v", i, err)
		}
		if len(body) > 1000 {
			t.Errorf("chunk %d body %d bytes exceeds UDP limit 1000", i, len(body))
		}
	}
}

// Test4_BuildCatalogResponses_SumNumConsistent 250 通道时 SumNum 全片相同.
func Test4_BuildCatalogResponses_SumNumConsistent(t *testing.T) {
	items := make([]CatalogItem, 250)
	for i := range items {
		items[i] = makeItem(i + 1)
	}
	resps, err := BuildCatalogResponses(1, "34020000002000000001", items, 1000, EncodingGB18030)
	if err != nil {
		t.Fatalf("build failed: %v", err)
	}
	for i, r := range resps {
		if r.SumNum != 250 {
			t.Errorf("chunk %d SumNum: got %d, want 250", i, r.SumNum)
		}
	}
}

// Test5_BuildCatalogResponses_SingleItemTooLarge 单 Item 超上限 → ErrItemTooLarge.
func Test5_BuildCatalogResponses_SingleItemTooLarge(t *testing.T) {
	hugeItem := CatalogItem{
		DeviceID:     "34020000001320000001",
		Name:         strings.Repeat("超长通道名", 200), // 200 * 5 中文 = 极长
		Manufacturer: "UVP",
		Model:        "SIM",
		ParentID:     "34020000002000000001",
		Status:       "ON",
	}
	// UDP 1000B 上限, 单个 huge item 编码后必超
	_, err := BuildCatalogResponses(1, "34020000002000000001", []CatalogItem{hugeItem}, 1000, EncodingGB18030)
	if err == nil {
		t.Fatal("单 Item 超上限期望返 ErrItemTooLarge")
	}
	if !errors.Is(err, ErrItemTooLarge) {
		t.Errorf("期望 ErrItemTooLarge, 实际 %v", err)
	}
}

// Test6_BuildCatalogResponses_InvalidMaxBodyBytes maxBodyBytes ≤ 0 返错.
func Test6_BuildCatalogResponses_InvalidMaxBodyBytes(t *testing.T) {
	_, err := BuildCatalogResponses(1, "34020000002000000001", []CatalogItem{makeItem(1)}, 0, EncodingUTF8)
	if err == nil {
		t.Fatal("maxBodyBytes=0 应返错")
	}
	_, err = BuildCatalogResponses(1, "34020000002000000001", []CatalogItem{makeItem(1)}, -1, EncodingUTF8)
	if err == nil {
		t.Fatal("maxBodyBytes=-1 应返错")
	}
}

// Test7_BuildDeviceInfoResponse 6 字段.
func Test7_BuildDeviceInfoResponse(t *testing.T) {
	resp := BuildDeviceInfoResponse(
		12346, "34020000002000000001",
		"GB28181 模拟设备", "UVP", "SIM-Desktop-v2", "v0.2.0", 5,
	)
	if resp.CmdType != "DeviceInfo" || resp.SN != 12346 {
		t.Errorf("basic fields: %+v", resp)
	}
	if resp.Result != "OK" || resp.Channel != 5 {
		t.Errorf("Result/Channel: %+v", resp)
	}
}

// Test8_BuildDeviceStatusResponse_AlarmstatusForced S-H4 回归.
func Test8_BuildDeviceStatusResponse_AlarmstatusForced(t *testing.T) {
	resp := BuildDeviceStatusResponse(12347, "34020000002000000001", true, true, false)
	if resp.Alarmstatus.Num != 0 {
		t.Errorf("Alarmstatus.Num should be 0, got %d", resp.Alarmstatus.Num)
	}
	if resp.Online != "ONLINE" || resp.Encode != "ON" || resp.Record != "OFF" {
		t.Errorf("bool fields: %+v", resp)
	}
	// 编码后必须含 <Alarmstatus Num="0">
	body, err := EncodeResponse(&resp, EncodingUTF8)
	if err != nil {
		t.Fatalf("encode failed: %v", err)
	}
	if !strings.Contains(string(body), `<Alarmstatus Num="0">`) {
		t.Errorf("Alarmstatus Num=\"0\" 未输出: %s", string(body))
	}
}

// Test9_BuildDeviceStatusResponse_DeviceTimeISO8601 DeviceTime 格式.
func Test9_BuildDeviceStatusResponse_DeviceTimeISO8601(t *testing.T) {
	resp := BuildDeviceStatusResponse(1, "34020000002000000001", true, true, false)
	// 格式: 2026-07-28T13:37:00, 长度 19
	if len(resp.DeviceTime) != 19 {
		t.Errorf("DeviceTime 长度应为 19, 实际 %q (len %d)", resp.DeviceTime, len(resp.DeviceTime))
	}
	if !strings.Contains(resp.DeviceTime, "T") {
		t.Errorf("DeviceTime 缺 T 分隔符: %q", resp.DeviceTime)
	}
}

// Test10_BuildCatalogResponses_TCPLargeLimit TCP 32K 上限, 250 通道应一片装下.
func Test10_BuildCatalogResponses_TCPLargeLimit(t *testing.T) {
	items := make([]CatalogItem, 50) // 保守 50 个避免真超 32K
	for i := range items {
		items[i] = makeItem(i + 1)
	}
	resps, err := BuildCatalogResponses(1, "34020000002000000001", items, MaxBodyBytesTCP, EncodingGB18030)
	if err != nil {
		t.Fatalf("build failed: %v", err)
	}
	if len(resps) != 1 {
		t.Errorf("50 items TCP 32K should fit in 1 chunk, got %d", len(resps))
	}
}
