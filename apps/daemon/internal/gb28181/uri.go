package gb28181

import "fmt"

// BuildRegisterRequestURI 构造 REGISTER 消息的 Request-URI。
//
// spec Q5: user 部分用 20 位 ServerID (平台 SIP ID),留空回退 ServerDomain。
// host 部分固定用 ServerDomain (10 位域)。
//
// 例: sip:34020000002000000001@3402000000
func BuildRegisterRequestURI(cfg *SipConfig) string {
	user := cfg.EffectiveServerID()
	host := cfg.ServerDomain
	if host == "" {
		// 若 ServerDomain 留空 (罕见),用 ServerID 兜底
		host = cfg.ServerID
	}
	return fmt.Sprintf("sip:%s@%s", user, host)
}

// BuildDeviceContactURI 构造设备端 Contact 头 URI。
//
// host 部分是本机对外可达 IP,port 是本机 SIP 监听端口 (平台回复用)。
// user 部分是设备 ID。
//
// 例: sip:34020000001320000001@192.168.1.100:5060
func BuildDeviceContactURI(cfg *SipConfig, localHost string, localPort int) string {
	return fmt.Sprintf("sip:%s@%s:%d", cfg.DeviceID, localHost, localPort)
}

// BuildFromURI 构造 From 头的 URI (不含 tag)。
//
// 设备端 AOR: sip:<DeviceID>@<ServerDomain>
// tag 由调用方另外拼接。
func BuildFromURI(cfg *SipConfig) string {
	host := cfg.ServerDomain
	if host == "" {
		host = cfg.ServerID
	}
	return fmt.Sprintf("sip:%s@%s", cfg.DeviceID, host)
}

// BuildToURI 构造 To 头的 URI (REGISTER 中与 From 相同,均为设备 AOR)。
//
// 注意 To 头 URI 用设备 AOR 而非平台 AOR (RFC 3261 §10.2:REGISTER 的 To
// 表示"要注册的地址",即设备自己)。这跟 Request-URI 不同。
func BuildToURI(cfg *SipConfig) string {
	return BuildFromURI(cfg)
}
