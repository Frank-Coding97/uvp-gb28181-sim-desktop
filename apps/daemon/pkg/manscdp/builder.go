// MANSCDP Response 构造器.
//
// v4 契约要点:
//   - P-B5: 空目录发一片 SumNum=0, Num=0 (不能零 Response)
//   - P-M1 + R3-B1: BuildCatalogResponses 按编码后 body 字节数动态装箱 (UDP 1000 / TCP 32K)
//   - N-B6 + R3-M3: 单 Item 编码后超 maxBodyBytes -> ErrItemTooLarge, 绝不发送超限包
//   - S-H4: BuildDeviceStatusResponse 强制输出 <Alarmstatus Num="0">
package manscdp

import (
	"fmt"
	"time"
)

// BuildCatalogResponses 按 body 字节上限动态分片构造 Catalog 响应.
//
// items: 全部通道列表 (可为空)
// maxBodyBytes: 单片 body 字节上限 (UDP 用 MaxBodyBytesUDP, TCP 用 MaxBodyBytesTCP)
// enc: 编码选项 (用于试编码估算实际字节数)
//
// 返回: 分片后的多个 Response 结构体 (编码在上层用 EncodeResponse).
// 空 items -> 一片 SumNum=0, Num=0 (P-B5).
// 单 Item 超上限 -> ErrItemTooLarge (N-B6, 不强放).
func BuildCatalogResponses(
	sn int,
	deviceID string,
	items []CatalogItem,
	maxBodyBytes int,
	enc Encoding,
) ([]CatalogResponse, error) {
	if maxBodyBytes <= 0 {
		return nil, fmt.Errorf("manscdp: invalid maxBodyBytes: %d", maxBodyBytes)
	}

	totalCount := len(items)

	// P-B5: 空目录也发一片
	if totalCount == 0 {
		return []CatalogResponse{{
			CmdType:  "Catalog",
			SN:       sn,
			DeviceID: deviceID,
			SumNum:   0,
			DeviceList: &DeviceList{
				Num:   0,
				Items: nil,
			},
		}}, nil
	}

	// P-M1: 按字节上限动态装箱
	var responses []CatalogResponse
	var current []CatalogItem

	flush := func() {
		if len(current) == 0 {
			return
		}
		responses = append(responses, CatalogResponse{
			CmdType:  "Catalog",
			SN:       sn,
			DeviceID: deviceID,
			SumNum:   totalCount,
			DeviceList: &DeviceList{
				Num:   len(current),
				Items: current,
			},
		})
		current = nil
	}

	for _, item := range items {
		// 试探性构造当前片 + 新 item, 编码后测字节
		candidate := append([]CatalogItem{}, current...)
		candidate = append(candidate, item)
		respCandidate := CatalogResponse{
			CmdType:  "Catalog",
			SN:       sn,
			DeviceID: deviceID,
			SumNum:   totalCount,
			DeviceList: &DeviceList{
				Num:   len(candidate),
				Items: candidate,
			},
		}
		encoded, err := EncodeResponse(&respCandidate, enc)
		if err != nil {
			return nil, fmt.Errorf("manscdp: encode probe for chunking: %w", err)
		}

		if len(encoded) <= maxBodyBytes {
			// 塞得下, 加入当前片
			current = candidate
			continue
		}

		// 塞不下: 封当前片 (若非空), 开始新片
		if len(current) > 0 {
			flush()
			current = []CatalogItem{item}
			// 再验证新片 (单个 item) 是否超上限
			singleResp := CatalogResponse{
				CmdType:  "Catalog",
				SN:       sn,
				DeviceID: deviceID,
				SumNum:   totalCount,
				DeviceList: &DeviceList{
					Num:   1,
					Items: current,
				},
			}
			singleEncoded, err := EncodeResponse(&singleResp, enc)
			if err != nil {
				return nil, fmt.Errorf("manscdp: encode probe: %w", err)
			}
			if len(singleEncoded) > maxBodyBytes {
				return nil, fmt.Errorf("%w: item %s encoded size %d > maxBodyBytes %d",
					ErrItemTooLarge, item.DeviceID, len(singleEncoded), maxBodyBytes)
			}
		} else {
			// N-B6: 单个 item 就超上限 -> 立即返错, 绝不发送
			return nil, fmt.Errorf("%w: item %s encoded size %d > maxBodyBytes %d",
				ErrItemTooLarge, item.DeviceID, len(encoded), maxBodyBytes)
		}
	}
	flush()

	return responses, nil
}

// BuildDeviceInfoResponse 构造 DeviceInfo 响应.
func BuildDeviceInfoResponse(
	sn int,
	deviceID string,
	deviceName string,
	manufacturer string,
	model string,
	firmware string,
	channelCount int,
) DeviceInfoResponse {
	return DeviceInfoResponse{
		CmdType:      "DeviceInfo",
		SN:           sn,
		DeviceID:     deviceID,
		Result:       "OK",
		DeviceName:   deviceName,
		Manufacturer: manufacturer,
		Model:        model,
		Firmware:     firmware,
		Channel:      channelCount,
	}
}

// BuildDeviceStatusResponse 构造 DeviceStatus 响应.
// S-H4: Alarmstatus 强制输出 (空列表 Num="0" 也输出).
func BuildDeviceStatusResponse(
	sn int,
	deviceID string,
	online bool,
	encoding bool,
	recording bool,
) DeviceStatusResponse {
	onlineStr := "OFFLINE"
	if online {
		onlineStr = "ONLINE"
	}
	encodeStr := "OFF"
	if encoding {
		encodeStr = "ON"
	}
	recordStr := "OFF"
	if recording {
		recordStr = "ON"
	}
	return DeviceStatusResponse{
		CmdType:    "DeviceStatus",
		SN:         sn,
		DeviceID:   deviceID,
		Result:     "OK",
		Online:     onlineStr,
		Status:     "OK", // v2 前期固定 OK
		Encode:     encodeStr,
		Record:     recordStr,
		DeviceTime: time.Now().Format("2006-01-02T15:04:05"),
		Alarmstatus: AlarmstatusBlock{
			Num:   0,
			Items: nil,
		},
	}
}
