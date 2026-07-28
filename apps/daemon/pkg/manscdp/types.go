// Package manscdp implements GB/T 28181-2022 MANSCDP XML messages.
//
// v2 M5 (目录查询三件套) 覆盖 3 种 CmdType:
//   - Catalog: 目录查询, 分片响应
//   - DeviceInfo: 设备信息查询
//   - DeviceStatus: 设备状态查询
//
// 契约来源: spec/plan v4 (2026-07-28 T0 定死后).
package manscdp

import "encoding/xml"

// Query 通用查询结构 (平台 -> 设备).
// 3 种 CmdType 复用同一结构.
type Query struct {
	XMLName  xml.Name `xml:"Query"`
	CmdType  string   `xml:"CmdType"`  // Catalog / DeviceInfo / DeviceStatus
	SN       int      `xml:"SN"`       // 序列号 (平台生成, 设备回显)
	DeviceID string   `xml:"DeviceID"` // 目标设备 ID
}

// CatalogResponse 目录查询响应 (S-H3: 13 字段全输出对齐 GB-2022 附录 A.2.6.1).
type CatalogResponse struct {
	XMLName    xml.Name    `xml:"Response"`
	CmdType    string      `xml:"CmdType"`    // Catalog
	SN         int         `xml:"SN"`         // 回显 Query 的 SN
	DeviceID   string      `xml:"DeviceID"`   // 本设备 ID
	SumNum     int         `xml:"SumNum"`     // 总通道数 (所有分片相同)
	DeviceList *DeviceList `xml:"DeviceList"` // 当前分片的通道列表 (P-B5: 空目录也必须输出)
}

// DeviceList Catalog 通道列表.
type DeviceList struct {
	Num   int           `xml:"Num,attr"` // 本分片通道数 (0 也要输出)
	Items []CatalogItem `xml:"Item"`
}

// CatalogItem 通道条目.
// S-H3 修订: 13 字段全输出, 缺失字段由上层填默认值.
type CatalogItem struct {
	DeviceID     string `xml:"DeviceID"`     // 20 位通道编码 (S-B4: 视频 132 / 报警 134)
	Name         string `xml:"Name"`         // 中文通道名
	Manufacturer string `xml:"Manufacturer"` // 缺失时后端补 "UVP"
	Model        string `xml:"Model"`        // 缺失时后端补 "SIM-Desktop-v2"
	Owner        string `xml:"Owner"`        // 缺失时后端补 "UVP-Simulator"
	CivilCode    string `xml:"CivilCode"`    // 缺失时补 "340200"
	Address      string `xml:"Address"`      // 允许空字符串
	Parental     int    `xml:"Parental"`     // 0=通道, 1=父设备 (本 spec 恒 0)
	ParentID     string `xml:"ParentID"`     // 父设备编码 (ConfigStore 强制设为 device ID)
	SafetyWay    int    `xml:"SafetyWay"`    // 0=不加密
	RegisterWay  int    `xml:"RegisterWay"`  // 1=Digest MD5
	Secrecy      int    `xml:"Secrecy"`      // 0=不保密
	Status       string `xml:"Status"`       // ON / OFF
}

// DeviceInfoResponse 设备信息查询响应 (国标 A.2.4.6).
type DeviceInfoResponse struct {
	XMLName      xml.Name `xml:"Response"`
	CmdType      string   `xml:"CmdType"` // DeviceInfo
	SN           int      `xml:"SN"`
	DeviceID     string   `xml:"DeviceID"`
	Result       string   `xml:"Result"` // OK
	DeviceName   string   `xml:"DeviceName"`
	Manufacturer string   `xml:"Manufacturer"`
	Model        string   `xml:"Model"`
	Firmware     string   `xml:"Firmware"`
	Channel      int      `xml:"Channel"` // 通道总数
}

// DeviceStatusResponse 设备状态查询响应 (国标 A.2.4.7).
// S-H4 修订: Alarmstatus 强制输出 (GB-2022 要求, 空列表 Num="0" 也输出).
type DeviceStatusResponse struct {
	XMLName     xml.Name         `xml:"Response"`
	CmdType     string           `xml:"CmdType"` // DeviceStatus
	SN          int              `xml:"SN"`
	DeviceID    string           `xml:"DeviceID"`
	Result      string           `xml:"Result"`     // OK
	Online      string           `xml:"Online"`     // ONLINE / OFFLINE
	Status      string           `xml:"Status"`     // OK / ERROR
	Encode      string           `xml:"Encode"`     // ON / OFF (v2 前期固定 ON)
	Record      string           `xml:"Record"`     // ON / OFF (v2 前期固定 OFF)
	DeviceTime  string           `xml:"DeviceTime"` // ISO8601
	Alarmstatus AlarmstatusBlock `xml:"Alarmstatus"`
}

// AlarmstatusBlock 报警状态列表.
// v2 前期无报警, 固定 Num="0" 输出.
type AlarmstatusBlock struct {
	Num   int               `xml:"Num,attr"`
	Items []AlarmstatusItem `xml:"Item,omitempty"`
}

// AlarmstatusItem GB-2022 报警项.
type AlarmstatusItem struct {
	DeviceID   string `xml:"DeviceID"`
	DutyStatus string `xml:"DutyStatus"` // ALARM / OFFDUTY
}
