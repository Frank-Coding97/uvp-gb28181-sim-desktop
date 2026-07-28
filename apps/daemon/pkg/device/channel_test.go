package device

import (
	"errors"
	"testing"
)

const testDeviceID = "34020000002000000001"

// Test1_ValidateDeviceID_Valid 合法设备 ID.
func Test1_ValidateDeviceID_Valid(t *testing.T) {
	if err := ValidateDeviceID(testDeviceID); err != nil {
		t.Errorf("valid device ID rejected: %v", err)
	}
}

// Test2_ValidateDeviceID_WrongTypeCode 类型码非 200 拒绝.
func Test2_ValidateDeviceID_WrongTypeCode(t *testing.T) {
	err := ValidateDeviceID("34020000001320000001") // 132 是视频通道
	if err == nil {
		t.Fatal("非 200 类型码应拒绝")
	}
	if !errors.Is(err, ErrInvalidDeviceID) {
		t.Errorf("期望 ErrInvalidDeviceID, 实际 %v", err)
	}
}

// Test3_ValidateDeviceID_NotNumeric 非纯数字拒绝.
func Test3_ValidateDeviceID_NotNumeric(t *testing.T) {
	err := ValidateDeviceID("3402000000200000000A")
	if err == nil {
		t.Fatal("非纯数字应拒绝")
	}
}

// Test4_ValidateDeviceID_WrongLength 长度非 20 拒绝.
func Test4_ValidateDeviceID_WrongLength(t *testing.T) {
	if err := ValidateDeviceID("34020000002000000"); err == nil {
		t.Fatal("短长度应拒绝")
	}
	if err := ValidateDeviceID("340200000020000000012"); err == nil {
		t.Fatal("超长应拒绝")
	}
}

// Test5_ValidateChannelID_Video 合法视频通道.
func Test5_ValidateChannelID_Video(t *testing.T) {
	if err := ValidateChannelID(testDeviceID, "34020000001320000001", "video"); err != nil {
		t.Errorf("valid video channel rejected: %v", err)
	}
}

// Test6_ValidateChannelID_Alarm 合法报警通道.
func Test6_ValidateChannelID_Alarm(t *testing.T) {
	if err := ValidateChannelID(testDeviceID, "34020000001340000001", "alarm"); err != nil {
		t.Errorf("valid alarm channel rejected: %v", err)
	}
}

// Test7_ValidateChannelID_WrongTypeCode 类型码不匹配.
func Test7_ValidateChannelID_WrongTypeCode(t *testing.T) {
	// 通道类型 "video" 但 ID 里是 134 (报警码)
	err := ValidateChannelID(testDeviceID, "34020000001340000001", "video")
	if err == nil {
		t.Fatal("类型码不匹配应拒绝")
	}
	if !errors.Is(err, ErrInvalidChannelID) {
		t.Errorf("期望 ErrInvalidChannelID, 实际 %v", err)
	}
}

// Test8_ValidateChannelID_PrefixMismatch 中心+行业前缀不匹配.
func Test8_ValidateChannelID_PrefixMismatch(t *testing.T) {
	err := ValidateChannelID(testDeviceID, "99999999991320000001", "video") // 前缀 9999999999 != 34020000
	if err == nil {
		t.Fatal("前缀不匹配应拒绝")
	}
	if !errors.Is(err, ErrIDPrefixMismatch) {
		t.Errorf("期望 ErrIDPrefixMismatch, 实际 %v", err)
	}
}

// Test9_ValidateChannelID_SeqZero 序号 0 拒绝.
func Test9_ValidateChannelID_SeqZero(t *testing.T) {
	err := ValidateChannelID(testDeviceID, "34020000001320000000", "video")
	if err == nil {
		t.Fatal("seq=0 应拒绝")
	}
	if !errors.Is(err, ErrInvalidChannelID) {
		t.Errorf("期望 ErrInvalidChannelID, 实际 %v", err)
	}
}

// Test10_ValidateChannelID_InvalidType 未知通道类型.
func Test10_ValidateChannelID_InvalidType(t *testing.T) {
	err := ValidateChannelID(testDeviceID, "34020000001320000001", "unknown")
	if err == nil {
		t.Fatal("未知类型应拒绝")
	}
	if !errors.Is(err, ErrInvalidChannelType) {
		t.Errorf("期望 ErrInvalidChannelType, 实际 %v", err)
	}
}

// Test11_TypeCodeOf 类型码映射.
func Test11_TypeCodeOf(t *testing.T) {
	if code, _ := TypeCodeOf("video"); code != "132" {
		t.Errorf("video: got %q, want 132", code)
	}
	if code, _ := TypeCodeOf("alarm"); code != "134" {
		t.Errorf("alarm: got %q, want 134", code)
	}
	if _, err := TypeCodeOf("bad"); err == nil {
		t.Error("未知类型应返错")
	}
}

// Test12_NextChannelIDs_EmptyList 空列表从序号 1 起.
func Test12_NextChannelIDs_EmptyList(t *testing.T) {
	ids, err := NextChannelIDs(nil, testDeviceID, "video", 1)
	if err != nil {
		t.Fatalf("empty list should not error: %v", err)
	}
	if len(ids) != 1 || ids[0] != "34020000001320000001" {
		t.Errorf("expected [34020000001320000001], got %v", ids)
	}
}

// Test13_NextChannelIDs_ScanMax 扫描最大序号 +1.
func Test13_NextChannelIDs_ScanMax(t *testing.T) {
	existing := []string{
		"34020000001320000005",
		"34020000001320000003",
		"34020000001320000001",
	}
	ids, err := NextChannelIDs(existing, testDeviceID, "video", 2)
	if err != nil {
		t.Fatalf("failed: %v", err)
	}
	expected := []string{"34020000001320000006", "34020000001320000007"}
	if len(ids) != 2 || ids[0] != expected[0] || ids[1] != expected[1] {
		t.Errorf("got %v, want %v", ids, expected)
	}
}

// Test14_NextChannelIDs_DifferentTypesIsolated 视频/报警类型码隔离.
func Test14_NextChannelIDs_DifferentTypesIsolated(t *testing.T) {
	// existing 里有 video (132) 序号 100, 请求 alarm (134) 从 1 起
	existing := []string{"34020000001320000100"}
	ids, err := NextChannelIDs(existing, testDeviceID, "alarm", 1)
	if err != nil {
		t.Fatalf("failed: %v", err)
	}
	if ids[0] != "34020000001340000001" {
		t.Errorf("alarm should start from 1 (isolated from video), got %s", ids[0])
	}
}

// Test15_NextChannelIDs_CountLimit count 范围校验.
func Test15_NextChannelIDs_CountLimit(t *testing.T) {
	if _, err := NextChannelIDs(nil, testDeviceID, "video", 0); err == nil {
		t.Error("count=0 应返错")
	}
	if _, err := NextChannelIDs(nil, testDeviceID, "video", 51); err == nil {
		t.Error("count=51 应返错")
	}
	if _, err := NextChannelIDs(nil, testDeviceID, "video", -1); err == nil {
		t.Error("count=-1 应返错")
	}
}

// Test16_NextChannelIDs_SeqOverflow 序号溢出 (>9999999).
func Test16_NextChannelIDs_SeqOverflow(t *testing.T) {
	existing := []string{"34020000001329999999"} // 最大序号
	_, err := NextChannelIDs(existing, testDeviceID, "video", 1)
	if err == nil {
		t.Fatal("溢出应返错")
	}
	if !errors.Is(err, ErrSeqOverflow) {
		t.Errorf("期望 ErrSeqOverflow, 实际 %v", err)
	}
}

// Test17_ValidateChannelBatch_Success 批量校验通过.
func Test17_ValidateChannelBatch_Success(t *testing.T) {
	channels := []Channel{
		{ID: "34020000001320000001", Type: "video"},
		{ID: "34020000001340000001", Type: "alarm"},
	}
	if err := ValidateChannelBatch(testDeviceID, channels); err != nil {
		t.Errorf("valid batch rejected: %v", err)
	}
}

// Test18_ValidateChannelBatch_Duplicate 批量含重复 ID 拒绝.
func Test18_ValidateChannelBatch_Duplicate(t *testing.T) {
	channels := []Channel{
		{ID: "34020000001320000001", Type: "video"},
		{ID: "34020000001320000001", Type: "video"}, // 重复
	}
	err := ValidateChannelBatch(testDeviceID, channels)
	if err == nil {
		t.Fatal("重复 ID 应拒绝")
	}
	if !errors.Is(err, ErrChannelAlreadyExist) {
		t.Errorf("期望 ErrChannelAlreadyExist, 实际 %v", err)
	}
}

// Test19_ValidateChannelBatch_InvalidChannel 批量含非法 ID 整体拒绝.
func Test19_ValidateChannelBatch_InvalidChannel(t *testing.T) {
	channels := []Channel{
		{ID: "34020000001320000001", Type: "video"},
		{ID: "34020000001320000000", Type: "video"}, // seq=0 非法
	}
	err := ValidateChannelBatch(testDeviceID, channels)
	if err == nil {
		t.Fatal("非法通道应拒绝整个 batch")
	}
}

// Test20_Package_Nostate 无状态验证: 多次调用不共享状态.
func Test20_Package_Nostate(t *testing.T) {
	// 两次调用 NextChannelIDs 用同样 existing 参数, 结果必须一样 (确认无缓存)
	existing := []string{"34020000001320000010"}
	a, _ := NextChannelIDs(existing, testDeviceID, "video", 1)
	b, _ := NextChannelIDs(existing, testDeviceID, "video", 1)
	if a[0] != b[0] {
		t.Errorf("非无状态: 两次调用结果不同 %s vs %s", a[0], b[0])
	}
}
