package ipc

import (
	"context"
	"encoding/json"
	"errors"
	"testing"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/pkg/device"
	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/pkg/manscdp"
)

const testDeviceID = "34020000002000000001"

func setupDeps(t *testing.T) *Deps {
	t.Helper()
	cs, err := device.NewConfigStore(testDeviceID, manscdp.EncodingGB18030)
	if err != nil {
		t.Fatalf("new config store: %v", err)
	}
	return &Deps{ConfigStore: cs, Version: "v0.3.0-test"}
}

// Test1_HandleListChannels_Empty 空列表.
func Test1_HandleListChannels_Empty(t *testing.T) {
	deps := setupDeps(t)
	handler := HandleListChannels(deps)
	result, err := handler(context.Background(), json.RawMessage(`{}`))
	if err != nil {
		t.Fatalf("list failed: %v", err)
	}
	arr, ok := result.(ListChannelsResp)
	if !ok {
		t.Fatalf("expected ListChannelsResp, got %T", result)
	}
	if len(arr) != 0 {
		t.Errorf("expected empty, got %d", len(arr))
	}
}

// Test2_HandleApplyConfig_Success apply_config 原子应用.
func Test2_HandleApplyConfig_Success(t *testing.T) {
	deps := setupDeps(t)
	handler := HandleApplyConfig(deps)

	req := `{"req":{"encoding":"UTF-8","channels":[{"id":"3402000000132` + `0000001","name":"摄像头一号","type":"video","status":"ON"}]}}`
	result, err := handler(context.Background(), json.RawMessage(req))
	if err != nil {
		t.Fatalf("apply_config failed: %v", err)
	}
	resp, ok := result.(ApplyConfigResp)
	if !ok {
		t.Fatalf("expected ApplyConfigResp, got %T", result)
	}
	if resp.Encoding != "UTF-8" {
		t.Errorf("encoding: got %s, want UTF-8", resp.Encoding)
	}
	if len(resp.Channels) != 1 {
		t.Errorf("channels: got %d, want 1", len(resp.Channels))
	}
	// ParentID 应被 ConfigStore 强制填
	if resp.Channels[0].ParentID != testDeviceID {
		t.Errorf("ParentID 未强制填: %s", resp.Channels[0].ParentID)
	}
}

// Test3_HandleApplyConfig_InvalidEncoding 未知编码拒绝.
func Test3_HandleApplyConfig_InvalidEncoding(t *testing.T) {
	deps := setupDeps(t)
	handler := HandleApplyConfig(deps)

	req := `{"req":{"encoding":"Latin-1","channels":[]}}`
	_, err := handler(context.Background(), json.RawMessage(req))
	if err == nil {
		t.Fatal("未知编码应拒绝")
	}
}

// Test4_HandleApplyConfig_EmptyChannels 空数组允许 (N-H5).
func Test4_HandleApplyConfig_EmptyChannels(t *testing.T) {
	deps := setupDeps(t)
	handler := HandleApplyConfig(deps)

	req := `{"req":{"encoding":"GB18030","channels":[]}}`
	result, err := handler(context.Background(), json.RawMessage(req))
	if err != nil {
		t.Fatalf("empty channels should succeed: %v", err)
	}
	resp := result.(ApplyConfigResp)
	if len(resp.Channels) != 0 {
		t.Errorf("expected empty channels, got %d", len(resp.Channels))
	}
}

// Test5_HandleApplyConfig_InvalidChannel 非法通道整体拒绝.
func Test5_HandleApplyConfig_InvalidChannel(t *testing.T) {
	deps := setupDeps(t)
	// 先填一批合法
	handler := HandleApplyConfig(deps)
	initReq := `{"req":{"encoding":"GB18030","channels":[{"id":"34020000001320000001","name":"a","type":"video","status":"ON"}]}}`
	handler(context.Background(), json.RawMessage(initReq))

	// 再传非法 (seq=0)
	badReq := `{"req":{"encoding":"UTF-8","channels":[{"id":"34020000001320000000","name":"bad","type":"video","status":"ON"}]}}`
	_, err := handler(context.Background(), json.RawMessage(badReq))
	if err == nil {
		t.Fatal("非法通道应拒绝")
	}

	// 回滚: 编码应保持 GB18030
	getEnc := HandleGetManscdpEncoding(deps)
	encResult, _ := getEnc(context.Background(), json.RawMessage(`{}`))
	if encResult.(EncodingResp).Encoding != "GB18030" {
		t.Errorf("回滚失败: encoding = %v", encResult)
	}
}

// Test6_HandleUpdateChannel_Success 更新单通道.
func Test6_HandleUpdateChannel_Success(t *testing.T) {
	deps := setupDeps(t)
	applyH := HandleApplyConfig(deps)
	applyH(context.Background(), json.RawMessage(
		`{"req":{"encoding":"GB18030","channels":[{"id":"34020000001320000001","name":"原名","type":"video","status":"ON"}]}}`))

	updH := HandleUpdateChannel(deps)
	req := `{"req":{"channel":{"id":"34020000001320000001","name":"新名","type":"video","status":"ON"}}}`
	result, err := updH(context.Background(), json.RawMessage(req))
	if err != nil {
		t.Fatalf("update failed: %v", err)
	}
	dto := result.(ChannelDTO)
	if dto.Name != "新名" {
		t.Errorf("update result name: %s", dto.Name)
	}
}

// Test7_HandleDeleteChannel_Success 删除单通道.
func Test7_HandleDeleteChannel_Success(t *testing.T) {
	deps := setupDeps(t)
	applyH := HandleApplyConfig(deps)
	applyH(context.Background(), json.RawMessage(
		`{"req":{"encoding":"GB18030","channels":[{"id":"34020000001320000001","name":"a","type":"video","status":"ON"}]}}`))

	delH := HandleDeleteChannel(deps)
	req := `{"req":{"id":"34020000001320000001"}}`
	result, err := delH(context.Background(), json.RawMessage(req))
	if err != nil {
		t.Fatalf("delete failed: %v", err)
	}
	if !result.(DeleteChannelResp).OK {
		t.Error("expected {ok: true}")
	}
}

// Test8_HandleDeleteChannel_NotFound 删除不存在返错.
func Test8_HandleDeleteChannel_NotFound(t *testing.T) {
	deps := setupDeps(t)
	delH := HandleDeleteChannel(deps)
	_, err := delH(context.Background(), json.RawMessage(`{"req":{"id":"34020000001320999999"}}`))
	if err == nil {
		t.Fatal("不存在应返错")
	}
	if !errors.Is(err, ErrChannelNotFound) {
		t.Errorf("期望 ErrChannelNotFound, 实际 %v", err)
	}
}

// Test9_HandleGetManscdpEncoding 读当前编码.
func Test9_HandleGetManscdpEncoding(t *testing.T) {
	deps := setupDeps(t)
	handler := HandleGetManscdpEncoding(deps)
	result, err := handler(context.Background(), json.RawMessage(`{}`))
	if err != nil {
		t.Fatalf("get encoding failed: %v", err)
	}
	if result.(EncodingResp).Encoding != "GB18030" {
		t.Errorf("expected GB18030, got %v", result)
	}
}

// Test10_HandleHealthz daemon 就绪.
func Test10_HandleHealthz(t *testing.T) {
	deps := setupDeps(t)
	handler := HandleHealthz(deps)
	result, err := handler(context.Background(), json.RawMessage(`{}`))
	if err != nil {
		t.Fatalf("healthz failed: %v", err)
	}
	resp := result.(HealthzResp)
	if !resp.Ready {
		t.Error("expected ready=true")
	}
	if resp.Version != "v0.3.0-test" {
		t.Errorf("version: %s", resp.Version)
	}
}

// Test11_RegisterAll 注册 6 个 method.
func Test11_RegisterAll(t *testing.T) {
	deps := setupDeps(t)
	registered := make(map[string]bool)
	fakeRouter := &mockRouter{registered: registered}
	RegisterAll(fakeRouter, deps)
	expected := []string{
		"list_channels", "update_channel", "delete_channel",
		"apply_config", "get_manscdp_encoding", "healthz",
	}
	for _, m := range expected {
		if !registered[m] {
			t.Errorf("method %s 未注册", m)
		}
	}
	if len(registered) != len(expected) {
		t.Errorf("注册数量: got %d, want %d", len(registered), len(expected))
	}
}

type mockRouter struct {
	registered map[string]bool
}

func (r *mockRouter) Register(method string, h func(ctx context.Context, params json.RawMessage) (any, error)) {
	r.registered[method] = true
}

// Test12_ChannelDTO_FromToDomain 值往返.
func Test12_ChannelDTO_FromToDomain(t *testing.T) {
	orig := device.Channel{
		ID:           "34020000001320000001",
		Name:         "test",
		Type:         "video",
		Manufacturer: "UVP",
		Model:        "M1",
		Owner:        "o",
		CivilCode:    "340200",
		Address:      "addr",
		Status:       "ON",
		ParentID:     "34020000002000000001",
	}
	dto := FromDomain(orig)
	back := dto.ToDomain()
	// 除 ParentID (ToDomain 不填, 由 ConfigStore 强制) 其他字段一致
	if back.ID != orig.ID || back.Name != orig.Name || back.Type != orig.Type {
		t.Errorf("往返丢失字段: back=%+v", back)
	}
	if dto.CivilCode != "340200" {
		t.Error("camelCase civilCode 未映射")
	}
}
