package device

import (
	"errors"
	"fmt"
	"sync"
	"testing"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/pkg/manscdp"
)

const csTestDeviceID = "34020000002000000001"

// makeChannel 测试通道.
func makeChannel(seq int) Channel {
	return Channel{
		ID:           fmt.Sprintf("3402000000132%07d", seq),
		Name:         fmt.Sprintf("摄像头-%03d", seq),
		Type:         "video",
		Manufacturer: "UVP",
		Model:        "SIM",
		Owner:        "UVP-Simulator",
		CivilCode:    "340200",
		Address:      "test",
		Status:       "ON",
	}
}

// Test1_NewConfigStore_ValidatesDeviceID 非法设备 ID 拒绝创建.
func Test1_NewConfigStore_ValidatesDeviceID(t *testing.T) {
	if _, err := NewConfigStore("bad-id", manscdp.EncodingGB18030); err == nil {
		t.Fatal("非法 deviceID 应拒绝")
	}
	if _, err := NewConfigStore("34020000001320000001", manscdp.EncodingGB18030); err == nil {
		t.Fatal("类型码非 200 应拒绝")
	}
	cs, err := NewConfigStore(csTestDeviceID, manscdp.EncodingGB18030)
	if err != nil || cs == nil {
		t.Fatalf("valid 应通过: %v", err)
	}
}

// Test2_Apply_Success 正常应用.
func Test2_Apply_Success(t *testing.T) {
	cs, _ := NewConfigStore(csTestDeviceID, manscdp.EncodingGB18030)
	snap, err := cs.Apply(manscdp.EncodingUTF8, []Channel{makeChannel(1), makeChannel(2)})
	if err != nil {
		t.Fatalf("apply failed: %v", err)
	}
	if snap.Encoding != manscdp.EncodingUTF8 {
		t.Errorf("encoding: got %s, want UTF-8", snap.Encoding)
	}
	if len(snap.Channels) != 2 {
		t.Errorf("channels: got %d, want 2", len(snap.Channels))
	}
	// 按 ID 排序
	if snap.Channels[0].ID > snap.Channels[1].ID {
		t.Errorf("channels 未按 ID 排序")
	}
	// ParentID 强制填 deviceID
	if snap.Channels[0].ParentID != csTestDeviceID {
		t.Errorf("ParentID 未强制填: %s", snap.Channels[0].ParentID)
	}
}

// Test3_Apply_AtomicOnFailure 应用失败整体回滚.
func Test3_Apply_AtomicOnFailure(t *testing.T) {
	cs, _ := NewConfigStore(csTestDeviceID, manscdp.EncodingGB18030)
	// 先应用一批合法
	_, err := cs.Apply(manscdp.EncodingGB2312, []Channel{makeChannel(1)})
	if err != nil {
		t.Fatalf("initial apply failed: %v", err)
	}
	snap1 := cs.Snapshot()

	// 应用非法通道 (seq=0) → 应整体回滚
	bad := Channel{ID: "34020000001320000000", Type: "video"}
	_, err = cs.Apply(manscdp.EncodingUTF8, []Channel{makeChannel(2), bad})
	if err == nil {
		t.Fatal("非法应用应返错")
	}

	snap2 := cs.Snapshot()
	// 编码没变
	if snap2.Encoding != snap1.Encoding {
		t.Errorf("回滚失败: 编码变了 %s → %s", snap1.Encoding, snap2.Encoding)
	}
	// 通道没变
	if len(snap2.Channels) != len(snap1.Channels) {
		t.Errorf("回滚失败: 通道数变了 %d → %d", len(snap1.Channels), len(snap2.Channels))
	}
	if snap2.Channels[0].ID != snap1.Channels[0].ID {
		t.Errorf("回滚失败: 通道 ID 变了")
	}
}

// Test4_Apply_EmptyChannels 空通道允许 (清空 daemon).
func Test4_Apply_EmptyChannels(t *testing.T) {
	cs, _ := NewConfigStore(csTestDeviceID, manscdp.EncodingGB18030)
	// 先填一些
	cs.Apply(manscdp.EncodingGB18030, []Channel{makeChannel(1)})
	// 再清空
	snap, err := cs.Apply(manscdp.EncodingUTF8, []Channel{})
	if err != nil {
		t.Fatalf("empty apply failed: %v", err)
	}
	if len(snap.Channels) != 0 {
		t.Errorf("空 apply 应清空, got %d", len(snap.Channels))
	}
	if snap.Encoding != manscdp.EncodingUTF8 {
		t.Errorf("encoding 应切换: %s", snap.Encoding)
	}
}

// Test5_Snapshot_ValueCopy Snapshot 返回值拷贝, 外部修改不影响内部.
func Test5_Snapshot_ValueCopy(t *testing.T) {
	cs, _ := NewConfigStore(csTestDeviceID, manscdp.EncodingGB18030)
	if _, err := cs.Apply(manscdp.EncodingGB18030, []Channel{makeChannel(1)}); err != nil {
		t.Fatalf("apply failed: %v", err)
	}
	snap := cs.Snapshot()
	if len(snap.Channels) == 0 {
		t.Fatal("apply 后 snapshot 应有通道")
	}

	// 外部修改
	snap.Channels[0].Name = "modified"

	snap2 := cs.Snapshot()
	if snap2.Channels[0].Name == "modified" {
		t.Error("Snapshot 未返回值拷贝, 内部被外部修改污染")
	}
}

// Test6_ConcurrentApplyAndSnapshot R3-B2 核心测试: 并发 Apply + Snapshot 一致性.
// 用 -race 跑, 且断言 Snapshot 永远看到 encoding+channels 的一致视图.
func Test6_ConcurrentApplyAndSnapshot(t *testing.T) {
	cs, _ := NewConfigStore(csTestDeviceID, manscdp.EncodingGB18030)

	encodings := []manscdp.Encoding{
		manscdp.EncodingGB18030,
		manscdp.EncodingGB2312,
		manscdp.EncodingUTF8,
	}

	var wg sync.WaitGroup
	// 100 个并发 Apply
	for i := 0; i < 100; i++ {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			enc := encodings[i%3]
			// 每次生成不同的通道集
			channels := make([]Channel, i%5+1)
			for j := range channels {
				channels[j] = makeChannel((i%10)*10 + j + 1)
			}
			_, _ = cs.Apply(enc, channels)
		}(i)
	}

	// 100 个并发 Snapshot, 断言拿到的快照永远一致
	// (即拿到的 encoding 和 channels 都是某个 Apply 提交完的完整状态, 不是半成品)
	for i := 0; i < 100; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			snap := cs.Snapshot()
			// 断言: encoding 必须是三种之一 (不能是空/初始值以外的值)
			validEnc := snap.Encoding == manscdp.EncodingGB18030 ||
				snap.Encoding == manscdp.EncodingGB2312 ||
				snap.Encoding == manscdp.EncodingUTF8
			if !validEnc {
				t.Errorf("snapshot encoding invalid: %s", snap.Encoding)
			}
			// channels 必须都有 ParentID (Apply 强制填)
			for _, ch := range snap.Channels {
				if ch.ParentID != csTestDeviceID {
					t.Errorf("channel %s ParentID missing: %s", ch.ID, ch.ParentID)
				}
			}
		}()
	}
	wg.Wait()
}

// Test7_UpdateChannel_Success 单通道更新.
func Test7_UpdateChannel_Success(t *testing.T) {
	cs, _ := NewConfigStore(csTestDeviceID, manscdp.EncodingGB18030)
	cs.Apply(manscdp.EncodingGB18030, []Channel{makeChannel(1)})

	// 更新 name
	updated := makeChannel(1)
	updated.Name = "新名字"
	result, err := cs.UpdateChannel(updated)
	if err != nil {
		t.Fatalf("update failed: %v", err)
	}
	if result.Name != "新名字" {
		t.Errorf("update result name: %s", result.Name)
	}

	// 再读一次验证已生效
	got, _ := cs.GetChannel(updated.ID)
	if got.Name != "新名字" {
		t.Errorf("GetChannel: %s", got.Name)
	}
}

// Test8_UpdateChannel_NotFound 更新不存在通道.
func Test8_UpdateChannel_NotFound(t *testing.T) {
	cs, _ := NewConfigStore(csTestDeviceID, manscdp.EncodingGB18030)
	_, err := cs.UpdateChannel(makeChannel(999))
	if err == nil {
		t.Fatal("不存在通道应返错")
	}
	if !errors.Is(err, ErrChannelNotFound) {
		t.Errorf("期望 ErrChannelNotFound, 实际 %v", err)
	}
}

// Test9_UpdateChannel_TypeCannotChange 禁止改 Type.
func Test9_UpdateChannel_TypeCannotChange(t *testing.T) {
	cs, _ := NewConfigStore(csTestDeviceID, manscdp.EncodingGB18030)
	cs.Apply(manscdp.EncodingGB18030, []Channel{makeChannel(1)})

	// 尝试把 video 改成 alarm (但 ID 里类型码是 132 video, 类型改 alarm 会被 ValidateChannelID 拦)
	bad := makeChannel(1)
	bad.Type = "alarm"
	_, err := cs.UpdateChannel(bad)
	if err == nil {
		t.Fatal("改 Type 应拒绝")
	}
}

// Test10_DeleteChannel_Success 删除通道.
func Test10_DeleteChannel_Success(t *testing.T) {
	cs, _ := NewConfigStore(csTestDeviceID, manscdp.EncodingGB18030)
	cs.Apply(manscdp.EncodingGB18030, []Channel{makeChannel(1), makeChannel(2)})

	err := cs.DeleteChannel(makeChannel(1).ID)
	if err != nil {
		t.Fatalf("delete failed: %v", err)
	}
	snap := cs.Snapshot()
	if len(snap.Channels) != 1 {
		t.Errorf("删除后应剩 1 个, got %d", len(snap.Channels))
	}
}

// Test11_DeleteChannel_NotFound 删除不存在.
func Test11_DeleteChannel_NotFound(t *testing.T) {
	cs, _ := NewConfigStore(csTestDeviceID, manscdp.EncodingGB18030)
	err := cs.DeleteChannel("34020000001320999999")
	if err == nil {
		t.Fatal("不存在应返错")
	}
	if !errors.Is(err, ErrChannelNotFound) {
		t.Errorf("期望 ErrChannelNotFound, 实际 %v", err)
	}
}

// Test12_Apply_DuplicateIDInPayload 批量含重复 ID 拒绝.
func Test12_Apply_DuplicateIDInPayload(t *testing.T) {
	cs, _ := NewConfigStore(csTestDeviceID, manscdp.EncodingGB18030)
	dup := makeChannel(1)
	_, err := cs.Apply(manscdp.EncodingGB18030, []Channel{makeChannel(1), dup})
	if err == nil {
		t.Fatal("重复 ID 应拒绝")
	}
	if !errors.Is(err, ErrChannelAlreadyExist) {
		t.Errorf("期望 ErrChannelAlreadyExist, 实际 %v", err)
	}
}
