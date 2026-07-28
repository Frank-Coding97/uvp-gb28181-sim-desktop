// Package ipc: v2 M5 目录查询三件套的 JSON-RPC 处理器.
//
// v4 契约 (D4): 6 个 method
//   - list_channels / update_channel / delete_channel
//   - apply_config (D1 唯一保存路径, R3-B2 真原子锁)
//   - get_manscdp_encoding / healthz (D6 hydrate 门禁探针)
//
// 复用现有 internal/ipc/Router 的 JSON-RPC 2.0 stdio pattern.
package ipc

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/pkg/device"
	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/pkg/manscdp"
)

// ChannelDTO 前后端通用通道 DTO (camelCase JSON tag, N-B4).
type ChannelDTO struct {
	ID           string `json:"id"`
	Name         string `json:"name"`
	Type         string `json:"type"`
	Manufacturer string `json:"manufacturer"`
	Model        string `json:"model"`
	Owner        string `json:"owner"`
	CivilCode    string `json:"civilCode"`
	Address      string `json:"address"`
	Status       string `json:"status"`
	ParentID     string `json:"parentId"`
}

// FromDomain device.Channel → ChannelDTO.
func FromDomain(ch device.Channel) ChannelDTO {
	return ChannelDTO{
		ID: ch.ID, Name: ch.Name, Type: ch.Type,
		Manufacturer: ch.Manufacturer, Model: ch.Model,
		Owner: ch.Owner, CivilCode: ch.CivilCode, Address: ch.Address,
		Status: ch.Status, ParentID: ch.ParentID,
	}
}

// ToDomain ChannelDTO → device.Channel.
// 注意: ParentID 由 ConfigStore 强制填, 此处传的值会被覆盖.
func (d ChannelDTO) ToDomain() device.Channel {
	return device.Channel{
		ID: d.ID, Name: d.Name, Type: d.Type,
		Manufacturer: d.Manufacturer, Model: d.Model,
		Owner: d.Owner, CivilCode: d.CivilCode, Address: d.Address,
		Status: d.Status,
	}
}

// Deps 汇聚 handler 依赖 (供 main.go 组装后注入).
type Deps struct {
	ConfigStore *device.ConfigStore
	Version     string // daemon 版本号 (供 healthz 返回)
}

// --- Request/Response DTOs ---

// ListChannelsResp 直接返回数组 (不套信封).
// 为方便 Go 里返回, 用别名类型.
type ListChannelsResp []ChannelDTO

// UpdateChannelReq (N-B4: req 包一层).
type UpdateChannelReq struct {
	Channel ChannelDTO `json:"channel"`
}

// DeleteChannelReq (N-B4: req 包一层).
type DeleteChannelReq struct {
	ID string `json:"id"`
}

// DeleteChannelResp.
type DeleteChannelResp struct {
	OK bool `json:"ok"`
}

// ApplyConfigReq (D1 唯一保存路径).
type ApplyConfigReq struct {
	Encoding string       `json:"encoding"`
	Channels []ChannelDTO `json:"channels"`
}

// ApplyConfigResp 应用后的完整快照.
type ApplyConfigResp struct {
	Encoding string       `json:"encoding"`
	Channels []ChannelDTO `json:"channels"`
}

// EncodingResp get_manscdp_encoding 响应.
type EncodingResp struct {
	Encoding string `json:"encoding"`
}

// HealthzResp daemon 就绪探针 (D6 hydrate 门禁).
type HealthzResp struct {
	Ready   bool   `json:"ready"`
	Version string `json:"version"`
}

// --- Handlers (符合 internal/ipc.Handler 签名) ---

// HandleListChannels list_channels.
// params: {} (无参数)
// result: ChannelDTO[] (直接数组)
func HandleListChannels(deps *Deps) func(ctx context.Context, params json.RawMessage) (any, error) {
	return func(ctx context.Context, params json.RawMessage) (any, error) {
		snap := deps.ConfigStore.Snapshot()
		dtos := make(ListChannelsResp, len(snap.Channels))
		for i, ch := range snap.Channels {
			dtos[i] = FromDomain(ch)
		}
		return dtos, nil
	}
}

// HandleUpdateChannel update_channel.
// params: {req: {channel: {...}}}
// result: ChannelDTO (更新后)
func HandleUpdateChannel(deps *Deps) func(ctx context.Context, params json.RawMessage) (any, error) {
	return func(ctx context.Context, params json.RawMessage) (any, error) {
		var wrapped struct {
			Req UpdateChannelReq `json:"req"`
		}
		if err := json.Unmarshal(params, &wrapped); err != nil {
			return nil, fmt.Errorf("parse params: %w", err)
		}
		updated, err := deps.ConfigStore.UpdateChannel(wrapped.Req.Channel.ToDomain())
		if err != nil {
			return nil, err
		}
		return FromDomain(updated), nil
	}
}

// HandleDeleteChannel delete_channel.
// params: {req: {id: "..."}}
// result: {ok: true}
func HandleDeleteChannel(deps *Deps) func(ctx context.Context, params json.RawMessage) (any, error) {
	return func(ctx context.Context, params json.RawMessage) (any, error) {
		var wrapped struct {
			Req DeleteChannelReq `json:"req"`
		}
		if err := json.Unmarshal(params, &wrapped); err != nil {
			return nil, fmt.Errorf("parse params: %w", err)
		}
		if err := deps.ConfigStore.DeleteChannel(wrapped.Req.ID); err != nil {
			return nil, err
		}
		return DeleteChannelResp{OK: true}, nil
	}
}

// HandleApplyConfig apply_config (D1 唯一保存路径, R3-B2 真原子锁).
// params: {req: {encoding, channels: [...]}}
// result: {encoding, channels: [...]}
func HandleApplyConfig(deps *Deps) func(ctx context.Context, params json.RawMessage) (any, error) {
	return func(ctx context.Context, params json.RawMessage) (any, error) {
		var wrapped struct {
			Req ApplyConfigReq `json:"req"`
		}
		if err := json.Unmarshal(params, &wrapped); err != nil {
			return nil, fmt.Errorf("parse params: %w", err)
		}
		// 校验编码 (D7 三模式)
		enc, err := parseEncoding(wrapped.Req.Encoding)
		if err != nil {
			return nil, err
		}
		// 转 DTO → domain
		channels := make([]device.Channel, len(wrapped.Req.Channels))
		for i, dto := range wrapped.Req.Channels {
			channels[i] = dto.ToDomain()
		}
		// R3-B2 关键: ConfigStore.Apply 一把 configMu 覆盖 encoding + channels
		snapshot, err := deps.ConfigStore.Apply(enc, channels)
		if err != nil {
			return nil, err
		}
		// 返回一致快照
		respChannels := make([]ChannelDTO, len(snapshot.Channels))
		for i, ch := range snapshot.Channels {
			respChannels[i] = FromDomain(ch)
		}
		return ApplyConfigResp{
			Encoding: string(snapshot.Encoding),
			Channels: respChannels,
		}, nil
	}
}

// HandleGetManscdpEncoding get_manscdp_encoding.
// params: {} (无参数)
// result: {encoding: "GB18030"}
func HandleGetManscdpEncoding(deps *Deps) func(ctx context.Context, params json.RawMessage) (any, error) {
	return func(ctx context.Context, params json.RawMessage) (any, error) {
		snap := deps.ConfigStore.Snapshot()
		return EncodingResp{Encoding: string(snap.Encoding)}, nil
	}
}

// HandleHealthz healthz (D6 hydrate 门禁探针).
// params: {} (无参数)
// result: {ready: true, version: "..."}
func HandleHealthz(deps *Deps) func(ctx context.Context, params json.RawMessage) (any, error) {
	return func(ctx context.Context, params json.RawMessage) (any, error) {
		return HealthzResp{Ready: true, Version: deps.Version}, nil
	}
}

// --- Registrar ---

// Registrar 支持 Register(method, handler) 的对象 (符合 internal/ipc.Router 签名).
type Registrar interface {
	Register(method string, h func(ctx context.Context, params json.RawMessage) (any, error))
}

// RegisterAll 一次性注册所有 6 个 M5 method.
func RegisterAll(r Registrar, deps *Deps) {
	r.Register("list_channels", HandleListChannels(deps))
	r.Register("update_channel", HandleUpdateChannel(deps))
	r.Register("delete_channel", HandleDeleteChannel(deps))
	r.Register("apply_config", HandleApplyConfig(deps))
	r.Register("get_manscdp_encoding", HandleGetManscdpEncoding(deps))
	r.Register("healthz", HandleHealthz(deps))
}

// parseEncoding 字符串 → manscdp.Encoding, D7 三模式.
func parseEncoding(s string) (manscdp.Encoding, error) {
	switch s {
	case "GB18030":
		return manscdp.EncodingGB18030, nil
	case "GB2312":
		return manscdp.EncodingGB2312, nil
	case "UTF-8":
		return manscdp.EncodingUTF8, nil
	default:
		return "", fmt.Errorf("unsupported encoding: %s", s)
	}
}

// ErrChannelNotFound 供上层 JSON-RPC 层判定错误类型.
var ErrChannelNotFound = device.ErrChannelNotFound

func init() {
	// 避免 unused import 编译警告
	_ = errors.New
}
