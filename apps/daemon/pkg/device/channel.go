// Package device 目录查询三件套的领域层.
//
// v4 关键决策 (D3):
//   - channel.go: 无状态校验/ID 生成工具 (本文件)
//   - config_store.go: 独占 encoding + channels, 一把 configMu 覆盖读写
//
// 无 ChannelManager (v3 已下线) - 所有状态由 ConfigStore 独占, 避免双所有权数据竞争.
package device

import (
	"errors"
	"fmt"
	"regexp"
	"strconv"
)

// 类型码常量 (S-B4 修订: 设备/视频/报警分开, 避免序号冲突).
const (
	TypeCodeDevice = "200" // 设备本体
	TypeCodeVideo  = "132" // 视频通道
	TypeCodeAlarm  = "134" // 报警输入通道

	maxSeqNo = 9_999_999 // 7 位序号上限
)

// reNumeric20 20 位纯数字 (通道 ID / 设备 ID 通用格式).
var reNumeric20 = regexp.MustCompile(`^[0-9]{20}$`)

// 错误集.
var (
	ErrChannelNotFound     = errors.New("device: channel not found")
	ErrChannelAlreadyExist = errors.New("device: channel already exists")
	ErrInvalidChannelID    = errors.New("device: invalid channel ID")
	ErrInvalidChannelType  = errors.New("device: invalid channel type")
	ErrIDPrefixMismatch    = errors.New("device: channel ID center/industry does not match device")
	ErrSeqOverflow         = errors.New("device: channel sequence overflow (> 9999999)")
	ErrInvalidDeviceID     = errors.New("device: invalid device ID")
)

// Channel 通道配置 (完整字段, 对齐 spec S-H3 修订).
type Channel struct {
	ID           string // 20 位国标编码
	Name         string
	Type         string // "video" / "alarm"
	Manufacturer string
	Model        string
	Owner        string
	CivilCode    string
	Address      string
	Status       string // ON / OFF
	ParentID     string // 父设备 ID (ConfigStore 强制设为 deviceID)
}

// ValidateDeviceID 校验设备 ID (S-B4: 类型码必须是 200).
func ValidateDeviceID(id string) error {
	if !reNumeric20.MatchString(id) {
		return fmt.Errorf("%w: device ID must be 20 numeric digits, got %q", ErrInvalidDeviceID, id)
	}
	if id[10:13] != TypeCodeDevice {
		return fmt.Errorf("%w: device ID type code must be %s, got %s", ErrInvalidDeviceID, TypeCodeDevice, id[10:13])
	}
	return nil
}

// ValidateChannelID 校验通道 ID (P-H8: 长度/纯数字/前缀/类型码/序号范围).
// deviceID 提供中心+行业前缀, channelType 决定类型码.
func ValidateChannelID(deviceID, id, channelType string) error {
	if !reNumeric20.MatchString(id) {
		return fmt.Errorf("%w: channel ID must be 20 numeric digits, got %q", ErrInvalidChannelID, id)
	}
	if len(deviceID) < 10 {
		return fmt.Errorf("%w: deviceID too short: %q", ErrInvalidDeviceID, deviceID)
	}
	// 中心+行业 8+2 位必须继承设备
	if id[:10] != deviceID[:10] {
		return fmt.Errorf("%w: got prefix %s, device %s", ErrIDPrefixMismatch, id[:10], deviceID[:10])
	}
	// 类型码校验
	expectedType, err := TypeCodeOf(channelType)
	if err != nil {
		return err
	}
	if id[10:13] != expectedType {
		return fmt.Errorf("%w: type code must be %s for %s, got %s",
			ErrInvalidChannelID, expectedType, channelType, id[10:13])
	}
	// 序号范围 1-9999999
	seq, _ := strconv.Atoi(id[13:])
	if seq <= 0 || seq > maxSeqNo {
		return fmt.Errorf("%w: sequence out of range 1-%d, got %d", ErrInvalidChannelID, maxSeqNo, seq)
	}
	return nil
}

// TypeCodeOf 通道类型 -> 类型码.
func TypeCodeOf(t string) (string, error) {
	switch t {
	case "video":
		return TypeCodeVideo, nil
	case "alarm":
		return TypeCodeAlarm, nil
	default:
		return "", fmt.Errorf("%w: %s", ErrInvalidChannelType, t)
	}
}

// NextChannelIDs 无状态生成 N 个连续 ID (D3: 不加锁, 调用方需持有 ConfigStore.mu).
//
// existing: 现有通道 ID 列表 (用于扫描最大序号)
// deviceID: 设备 ID (提供中心+行业+类型码前缀)
// channelType: "video" / "alarm"
// count: 生成数量 (1-50)
//
// 返回: 递增的 N 个 ID.
func NextChannelIDs(existing []string, deviceID, channelType string, count int) ([]string, error) {
	if count <= 0 || count > 50 {
		return nil, fmt.Errorf("invalid count %d, must be 1-50", count)
	}
	if err := ValidateDeviceID(deviceID); err != nil {
		return nil, err
	}
	typeCode, err := TypeCodeOf(channelType)
	if err != nil {
		return nil, err
	}
	prefix := deviceID[:10] + typeCode // 13 位前缀
	maxSeq := 0
	for _, id := range existing {
		if len(id) == 20 && id[:13] == prefix {
			seq, _ := strconv.Atoi(id[13:])
			if seq > maxSeq {
				maxSeq = seq
			}
		}
	}
	if maxSeq+count > maxSeqNo {
		return nil, fmt.Errorf("%w: existing max=%d + count=%d exceeds %d",
			ErrSeqOverflow, maxSeq, count, maxSeqNo)
	}
	ids := make([]string, count)
	for i := 0; i < count; i++ {
		ids[i] = fmt.Sprintf("%s%07d", prefix, maxSeq+i+1)
	}
	return ids, nil
}

// ValidateChannelBatch 批量校验一组 channels.
// 返回错误时不修改任何状态, 调用方 (ConfigStore.Apply) 拿到全绿再原子提交.
func ValidateChannelBatch(deviceID string, channels []Channel) error {
	if err := ValidateDeviceID(deviceID); err != nil {
		return err
	}
	seen := make(map[string]bool, len(channels))
	for _, ch := range channels {
		if err := ValidateChannelID(deviceID, ch.ID, ch.Type); err != nil {
			return fmt.Errorf("channel %s: %w", ch.ID, err)
		}
		if seen[ch.ID] {
			return fmt.Errorf("%w: %s duplicate in payload", ErrChannelAlreadyExist, ch.ID)
		}
		seen[ch.ID] = true
	}
	return nil
}
