// ConfigStore: 唯一事实源, 一把 configMu 覆盖 encoding + channels.
//
// R3-B2 关键: 消除"encoding 与 channels 分属两把锁"造成的数据竞争,
// 保证并发 Query 永远看到 encoding+channels 一致快照.
package device

import (
	"fmt"
	"sort"
	"sync"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/pkg/manscdp"
)

// ConfigSnapshot 一致的配置快照 (encoding + channels 同时读).
type ConfigSnapshot struct {
	Encoding manscdp.Encoding
	Channels []Channel // 值拷贝, 外部可自由改动不影响内部
}

// ConfigStore 独占 encoding + channels 的真原子存储 (D3 + R3-B2).
type ConfigStore struct {
	mu       sync.RWMutex
	encoding manscdp.Encoding
	channels map[string]Channel
	deviceID string
}

// NewConfigStore 创建 ConfigStore.
// deviceID 必须通过 ValidateDeviceID (类型码 200, 20 位纯数字).
func NewConfigStore(deviceID string, initEnc manscdp.Encoding) (*ConfigStore, error) {
	if err := ValidateDeviceID(deviceID); err != nil {
		return nil, err
	}
	return &ConfigStore{
		encoding: initEnc,
		channels: make(map[string]Channel),
		deviceID: deviceID,
	}, nil
}

// Apply 原子提交 encoding + channels (D1 唯一保存路径).
// 任一步失败整体回滚, 外部看不到中间状态.
func (cs *ConfigStore) Apply(enc manscdp.Encoding, channels []Channel) (ConfigSnapshot, error) {
	// 1. 离锁做全量校验 (避免锁内做重活)
	if err := ValidateChannelBatch(cs.deviceID, channels); err != nil {
		return ConfigSnapshot{}, err
	}
	// 2. 构造 validated map (强制 ParentID = deviceID)
	validated := make(map[string]Channel, len(channels))
	for _, ch := range channels {
		ch.ParentID = cs.deviceID
		validated[ch.ID] = ch
	}
	// 3. ★ 一把锁覆盖编码 + 通道 (R3-B2 核心)
	cs.mu.Lock()
	defer cs.mu.Unlock()
	cs.encoding = enc
	cs.channels = validated
	return cs.snapshotLocked(), nil
}

// UpdateChannel 单个通道更新 (供 update_channel IPC).
// 允许改 Name/Manufacturer/... 等字段, 禁改 ID/Type/ParentID.
func (cs *ConfigStore) UpdateChannel(ch Channel) (Channel, error) {
	if err := ValidateChannelID(cs.deviceID, ch.ID, ch.Type); err != nil {
		return Channel{}, err
	}
	cs.mu.Lock()
	defer cs.mu.Unlock()
	existing, ok := cs.channels[ch.ID]
	if !ok {
		return Channel{}, fmt.Errorf("%w: %s", ErrChannelNotFound, ch.ID)
	}
	if existing.Type != ch.Type {
		return Channel{}, fmt.Errorf("%w: type cannot be changed after creation", ErrInvalidChannelID)
	}
	ch.ParentID = cs.deviceID
	cs.channels[ch.ID] = ch
	return ch, nil
}

// DeleteChannel 删除单个通道 (供 delete_channel IPC).
func (cs *ConfigStore) DeleteChannel(id string) error {
	cs.mu.Lock()
	defer cs.mu.Unlock()
	if _, ok := cs.channels[id]; !ok {
		return fmt.Errorf("%w: %s", ErrChannelNotFound, id)
	}
	delete(cs.channels, id)
	return nil
}

// Snapshot 读一致快照 (RLock 允许并发, Query Handler 用它).
func (cs *ConfigStore) Snapshot() ConfigSnapshot {
	cs.mu.RLock()
	defer cs.mu.RUnlock()
	return cs.snapshotLocked()
}

// GetChannel 单查一个通道值拷贝.
func (cs *ConfigStore) GetChannel(id string) (Channel, error) {
	cs.mu.RLock()
	defer cs.mu.RUnlock()
	ch, ok := cs.channels[id]
	if !ok {
		return Channel{}, fmt.Errorf("%w: %s", ErrChannelNotFound, id)
	}
	return ch, nil
}

// DeviceID 只读访问设备 ID.
func (cs *ConfigStore) DeviceID() string {
	return cs.deviceID
}

// snapshotLocked 在已持锁时构造快照, 内部方法.
func (cs *ConfigStore) snapshotLocked() ConfigSnapshot {
	channels := make([]Channel, 0, len(cs.channels))
	for _, ch := range cs.channels {
		channels = append(channels, ch) // 值拷贝
	}
	// 按 ID 排序保证 list_channels 顺序稳定
	sort.Slice(channels, func(i, j int) bool { return channels[i].ID < channels[j].ID })
	return ConfigSnapshot{
		Encoding: cs.encoding,
		Channels: channels,
	}
}
