// Package sip 封装 sipgo UAC 能力,对上层业务提供最小接口:
// Transport (UDP/TCP 传输初始化)、Client (自动处理 401 Digest)。
//
// M1 仅实现 UDP + Digest MD5 基础流程。TCP 归 M4,Trace observer 归 M2。
package sip

import (
	"fmt"
	"net"
	"strings"

	"github.com/emiago/sipgo"
)

// TransportConfig 是 Transport 初始化参数。
type TransportConfig struct {
	// Protocol 是传输协议: "udp" 或 "tcp"。M1 仅支持 "udp"。
	Protocol string

	// LocalAddr 是本地绑定地址,格式 "host:port"。
	// "0.0.0.0:0" 表示所有网卡+随机端口。
	LocalAddr string
}

// Transport 封装 sipgo UserAgent + 本地 UDP/TCP socket。
type Transport struct {
	ua        *sipgo.UserAgent
	protocol  string
	localAddr string // 实际绑定的 host:port
}

// NewTransport 创建 sipgo UserAgent 并绑定本地 socket。
func NewTransport(cfg TransportConfig) (*Transport, error) {
	protocol := strings.ToLower(cfg.Protocol)
	if protocol == "" {
		protocol = "udp"
	}
	if protocol != "udp" && protocol != "tcp" {
		return nil, fmt.Errorf("unsupported protocol: %s (M1 仅支持 udp,TCP 归 M4)", cfg.Protocol)
	}
	if protocol == "tcp" {
		return nil, fmt.Errorf("TCP 传输归 M4,当前 M1 仅支持 udp")
	}

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
	}, nil
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
