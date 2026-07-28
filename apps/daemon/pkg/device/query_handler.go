// Query Handler: 编排 Catalog / DeviceInfo / DeviceStatus 三种查询的响应.
//
// v4 关键:
//   - 从 ConfigStore.Snapshot() 拿一致 encoding+channels 快照 (R3-B2)
//   - CatalogItem 缺失字段填默认值 (S-H3)
//   - 分片按 body 字节上限 (P-M1 + R3-B1)
package device

import (
	"context"
	"fmt"
	"sync/atomic"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/pkg/manscdp"
)

// DeviceConfig 设备基础信息 (供 DeviceInfo 响应使用).
type DeviceConfig struct {
	Name         string // 设备名称
	Manufacturer string // 厂商
	Model        string // 型号
	Firmware     string // 固件版本
}

// QueryHandler 处理平台 MANSCDP Query 请求.
type QueryHandler struct {
	configStore  *ConfigStore
	deviceConfig *DeviceConfig
	transport    atomic.Value // string: "udp" / "tcp", 用于选 body 上限
}

// NewQueryHandler 创建 QueryHandler.
func NewQueryHandler(
	configStore *ConfigStore,
	deviceConfig *DeviceConfig,
	initTransport string,
) *QueryHandler {
	h := &QueryHandler{
		configStore:  configStore,
		deviceConfig: deviceConfig,
	}
	h.transport.Store(initTransport)
	return h
}

// SetTransport 传输层切换时更新 body 上限.
func (h *QueryHandler) SetTransport(tp string) {
	h.transport.Store(tp)
}

// maxBodyBytes 根据 transport 返回分片字节上限.
func (h *QueryHandler) maxBodyBytes() int {
	if h.transport.Load().(string) == "tcp" {
		return manscdp.MaxBodyBytesTCP
	}
	return manscdp.MaxBodyBytesUDP
}

// HandleCatalogQuery 处理目录查询.
// 返回: N 片编码后的 body ([]byte), 上层 MessageHandler 逐片发送.
func (h *QueryHandler) HandleCatalogQuery(
	ctx context.Context,
	query *manscdp.Query,
) ([][]byte, error) {
	// R3-B2: 一次拿 encoding + channels 的一致快照
	snapshot := h.configStore.Snapshot()

	// 转 CatalogItem, S-H3: 缺失字段补默认值
	items := make([]manscdp.CatalogItem, len(snapshot.Channels))
	for i, ch := range snapshot.Channels {
		items[i] = manscdp.CatalogItem{
			DeviceID:     ch.ID,
			Name:         defaultString(ch.Name, fallbackName(ch.ID)),
			Manufacturer: defaultString(ch.Manufacturer, h.deviceConfig.Manufacturer),
			Model:        defaultString(ch.Model, h.deviceConfig.Model),
			Owner:        defaultString(ch.Owner, "UVP-Simulator"),
			CivilCode:    defaultString(ch.CivilCode, "340200"),
			Address:      ch.Address, // 允许空
			Parental:     0,          // 通道恒 0
			ParentID:     ch.ParentID,
			SafetyWay:    0,
			RegisterWay:  1,
			Secrecy:      0,
			Status:       defaultString(ch.Status, "ON"),
		}
	}

	maxBytes := h.maxBodyBytes()
	responses, err := manscdp.BuildCatalogResponses(
		query.SN, query.DeviceID, items, maxBytes, snapshot.Encoding,
	)
	if err != nil {
		return nil, fmt.Errorf("build catalog responses: %w", err)
	}
	bodies := make([][]byte, len(responses))
	for i, resp := range responses {
		body, err := manscdp.EncodeResponse(&resp, snapshot.Encoding)
		if err != nil {
			return nil, fmt.Errorf("encode chunk #%d: %w", i+1, err)
		}
		bodies[i] = body
	}
	return bodies, nil
}

// HandleDeviceInfoQuery 处理设备信息查询.
func (h *QueryHandler) HandleDeviceInfoQuery(
	ctx context.Context,
	query *manscdp.Query,
) ([]byte, error) {
	snapshot := h.configStore.Snapshot()
	resp := manscdp.BuildDeviceInfoResponse(
		query.SN, query.DeviceID,
		h.deviceConfig.Name,
		h.deviceConfig.Manufacturer,
		h.deviceConfig.Model,
		h.deviceConfig.Firmware,
		len(snapshot.Channels),
	)
	return manscdp.EncodeResponse(&resp, snapshot.Encoding)
}

// HandleDeviceStatusQuery 处理设备状态查询.
func (h *QueryHandler) HandleDeviceStatusQuery(
	ctx context.Context,
	query *manscdp.Query,
) ([]byte, error) {
	snapshot := h.configStore.Snapshot()
	resp := manscdp.BuildDeviceStatusResponse(
		query.SN, query.DeviceID,
		true,  // online (session 已 Registered 才走到这里)
		true,  // encoding (v2 前期固定 ON)
		false, // recording (v2 前期固定 OFF)
	)
	return manscdp.EncodeResponse(&resp, snapshot.Encoding)
}

func defaultString(v, fallback string) string {
	if v == "" {
		return fallback
	}
	return v
}

// fallbackName 从通道 ID 推导默认名称: "通道-<序号>".
func fallbackName(id string) string {
	if len(id) == 20 {
		return "通道-" + id[13:]
	}
	return "通道-" + id
}
