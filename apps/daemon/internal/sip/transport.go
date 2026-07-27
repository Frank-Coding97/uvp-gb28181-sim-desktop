// Package sip 封装 sipgo UAC 能力,对上层业务提供最小接口:
// Transport (UDP/TCP 传输初始化)、Client (自动处理 401 Digest)。
//
// M1 UDP + Digest MD5 · M2 Trace observer · M4 起 TCP + Digest 方言。
package sip

import (
	"fmt"
	"net"
	"strings"

	"github.com/emiago/sipgo"
)

// TransportConfig 是 Transport 初始化参数。
type TransportConfig struct {
	// Protocol 是传输协议: "udp" (默认) 或 "tcp"。
	// TCP 自 M4 起支持。TLS/WS 归后续 spec。
	Protocol string

	// LocalAddr 是本地绑定地址,格式 "host:port"。
	// "0.0.0.0:0" 表示所有网卡+随机端口。
	LocalAddr string

	// LocalHost 是本机对外可达 IP (Contact 头 / Via 头 host 部分)。
	// 空则由 DiscoverLocalIP 探测。
	LocalHost string

	// LocalPort 是希望绑定的本地 UDP 端口 (0=随机)。
	// GB28181 场景一般是 5060,便于平台把响应发回同一端口。
	LocalPort int
}

// Transport 封装 sipgo UserAgent + 本地 UDP/TCP socket。
type Transport struct {
	ua        *sipgo.UserAgent
	protocol  string
	localAddr string // 实际绑定的 host:port
	localHost string
	localPort int
}

// NewTransport 创建 sipgo UserAgent 并绑定本地 socket。
func NewTransport(cfg TransportConfig) (*Transport, error) {
	protocol := strings.ToLower(cfg.Protocol)
	if protocol == "" {
		protocol = "udp"
	}
	if protocol != "udp" && protocol != "tcp" {
		return nil, fmt.Errorf("unsupported protocol: %s (仅支持 udp/tcp)", cfg.Protocol)
	}

	// sipgo v1.4.0 内建 UDP + TCP,UA 层不需要区分。
	// Client 层通过 Request-URI 的 transport 参数 + SetDestination 让 sipgo
	// 选正确的 socket 类型。TCP 分帧按 Content-Length 严格处理 (RFC 3261 §18.3)。
	ua, err := sipgo.NewUA(
		sipgo.WithUserAgent("UVP-Sim-Desktop/0.2.0"),
	)
	if err != nil {
		return nil, fmt.Errorf("sipgo NewUA: %w", err)
	}

	return &Transport{
		ua:        ua,
		protocol:  protocol,
		localAddr: cfg.LocalAddr,
		localHost: cfg.LocalHost,
		localPort: cfg.LocalPort,
	}, nil
}

// LocalHost 返回本机对外可达 IP (Client 层构造 Contact / Via 用)。
func (t *Transport) LocalHost() string {
	return t.localHost
}

// LocalPort 返回期望绑定的本地 UDP 端口 (0=随机)。
func (t *Transport) LocalPort() int {
	return t.localPort
}

// UserAgent 暴露内部 sipgo.UserAgent 给 Client 层。
func (t *Transport) UserAgent() *sipgo.UserAgent {
	return t.ua
}

// Protocol 返回当前传输协议 ("udp" / "tcp")。
func (t *Transport) Protocol() string {
	return t.protocol
}

// DiscoverLocalIP 通过 UDP connect 探测本机对外可达 IP (不真发包)。
//
// 用于构造 Contact 头 host 部分:平台从 Via/Contact 拿这个 IP 回包。
// 参考 v1 discover_local_ip:UDP socket 只做 connect 拿本地 addr。
func (t *Transport) DiscoverLocalIP(serverAddr string) string {
	// 用 UDP 因为 connect 不真握手,只做路由查询。
	conn, err := net.Dial("udp", serverAddr)
	if err != nil {
		return "127.0.0.1"
	}
	defer conn.Close()
	localAddr, ok := conn.LocalAddr().(*net.UDPAddr)
	if !ok {
		return "127.0.0.1"
	}
	return localAddr.IP.String()
}

// Close 释放 UserAgent 资源。
func (t *Transport) Close() error {
	if t.ua != nil {
		return t.ua.Close()
	}
	return nil
}
