package gb28181

import (
	"fmt"
	"strings"
)

// KeepaliveParams 是构造 Keepalive MANSCDP XML 的输入参数。
type KeepaliveParams struct {
	SN       int    // 单调递增序列号
	DeviceID string // 20 位设备 ID
}

// BuildKeepaliveXML 构造心跳 Keepalive 的 MANSCDP XML 字符串 (UTF-8)。
//
// spec AC-6: 平台按此判断设备在线。Content-Type: Application/MANSCDP+xml。
// XML 声明用 GB2312 (向下兼容,大部分平台按此判定),实际字节由 EncodeGB18030 编码。
//
//   <?xml version="1.0" encoding="GB2312"?>
//   <Notify>
//     <CmdType>Keepalive</CmdType>
//     <SN>{{SN}}</SN>
//     <DeviceID>{{DeviceID}}</DeviceID>
//     <Status>OK</Status>
//   </Notify>
func BuildKeepaliveXML(p KeepaliveParams) string {
	var b strings.Builder
	b.Grow(200)
	b.WriteString(`<?xml version="1.0" encoding="GB2312"?>` + "\n")
	b.WriteString("<Notify>\n")
	b.WriteString("  <CmdType>Keepalive</CmdType>\n")
	fmt.Fprintf(&b, "  <SN>%d</SN>\n", p.SN)
	fmt.Fprintf(&b, "  <DeviceID>%s</DeviceID>\n", p.DeviceID)
	b.WriteString("  <Status>OK</Status>\n")
	b.WriteString("</Notify>")
	return b.String()
}

// EncodeMANSCDPBody 把 XML 字符串编码为 GB18030 字节。
//
// 便捷封装:注册流程本身不用,但心跳/目录/PTZ 的 body 都走这个函数。
// 出于对齐目的 M1 就把它落好,避免后续 M3 反复扩散调用点。
func EncodeMANSCDPBody(xml string) ([]byte, error) {
	return EncodeGB18030(xml)
}
