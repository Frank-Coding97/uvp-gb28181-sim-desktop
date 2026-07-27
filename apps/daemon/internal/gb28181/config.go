// Package gb28181 提供国标 GB/T 28181-2022 方言层:
// SIP URI 构造 / GB18030 编解码 / MANSCDP XML 构造 / 常量表。
//
// 本包为纯函数库,不含状态,不引入 sipgo 依赖(避免耦合)。
package gb28181

import (
	"errors"
	"fmt"
	"strings"
)

// SipConfig 是设备端 SIP 注册的完整配置。
type SipConfig struct {
	// DeviceID 是本设备的 20 位国标 ID (SIP 用户名)。
	DeviceID string `json:"device_id"`

	// ServerHost 是上级平台的 IP 或域名。
	ServerHost string `json:"server_host"`

	// ServerPort 是上级平台的 SIP 端口 (默认 5060)。
	ServerPort int `json:"server_port"`

	// ServerID 是上级平台的 20 位 SIP ID。
	// REGISTER Request-URI 的 user 部分优先用这个 (spec Q5)。
	// 留空则回退用 ServerDomain。
	ServerID string `json:"server_id"`

	// ServerDomain 是上级平台的 10 位域 (SIP domain 部分)。
	ServerDomain string `json:"server_domain"`

	// Password 是 SIP Digest 鉴权密码。
	Password string `json:"password"`

	// Transport 是 SIP 传输协议: "udp" (M1 仅支持) 或 "tcp" (M4)。
	Transport string `json:"transport"`

	// HeartbeatIntervalSecs 是 Keepalive 心跳周期,秒。
	// M1 不使用 (无心跳),M3 生效。默认 60。
	HeartbeatIntervalSecs int `json:"heartbeat_interval_secs,omitempty"`

	// ExpiresSecs 是 REGISTER Expires 头请求值,秒。默认 3600。
	// 实际有效期以平台 200 OK 响应为准 (spec Q9)。
	ExpiresSecs int `json:"expires_secs,omitempty"`
}

// Validate 校验必填字段与格式约束。
func (c *SipConfig) Validate() error {
	if len(c.DeviceID) != 20 {
		return fmt.Errorf("device_id 必须 20 位,当前 %d 位", len(c.DeviceID))
	}
	if !isAllDigits(c.DeviceID) {
		return errors.New("device_id 必须全数字")
	}
	if c.ServerHost == "" {
		return errors.New("server_host 必填")
	}
	if c.ServerPort <= 0 || c.ServerPort > 65535 {
		return fmt.Errorf("server_port 非法: %d", c.ServerPort)
	}
	if c.ServerDomain != "" && len(c.ServerDomain) != 10 {
		return fmt.Errorf("server_domain 必须 10 位或留空,当前 %d 位", len(c.ServerDomain))
	}
	if c.ServerID != "" && len(c.ServerID) != 20 {
		return fmt.Errorf("server_id 必须 20 位或留空,当前 %d 位", len(c.ServerID))
	}
	if c.ServerID == "" && c.ServerDomain == "" {
		return errors.New("server_id 与 server_domain 至少填一项")
	}
	if c.Password == "" {
		return errors.New("password 必填")
	}
	t := strings.ToLower(c.Transport)
	if t != "" && t != "udp" && t != "tcp" {
		return fmt.Errorf("transport 只支持 udp/tcp,当前: %s", c.Transport)
	}
	return nil
}

// ApplyDefaults 填充可选字段的默认值。
func (c *SipConfig) ApplyDefaults() {
	if c.Transport == "" {
		c.Transport = "udp"
	}
	c.Transport = strings.ToLower(c.Transport)
	if c.HeartbeatIntervalSecs == 0 {
		c.HeartbeatIntervalSecs = DefaultHeartbeatSecs
	}
	if c.ExpiresSecs == 0 {
		c.ExpiresSecs = DefaultExpiresSecs
	}
	// server_id 留空回退 server_domain (spec Q5) 的动作放到 uri.go,
	// 保持 config 原始数据不动,方便测试与诊断。
}

// EffectiveServerID 返回用于 Request-URI user 部分的 ID。
// spec Q5: 优先 ServerID (20 位平台 ID),留空回退 ServerDomain (10 位域)。
func (c *SipConfig) EffectiveServerID() string {
	if c.ServerID != "" {
		return c.ServerID
	}
	return c.ServerDomain
}

func isAllDigits(s string) bool {
	for _, r := range s {
		if r < '0' || r > '9' {
			return false
		}
	}
	return true
}
