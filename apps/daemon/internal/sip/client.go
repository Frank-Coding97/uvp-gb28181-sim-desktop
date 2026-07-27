package sip

import (
	"context"
	"errors"
	"fmt"

	"github.com/emiago/sipgo"
	"github.com/emiago/sipgo/sip"
)

// ClientConfig 是 Client 初始化参数。
type ClientConfig struct {
	// Username 是 Digest 挑战应答用户名 (通常等于设备 ID)。
	Username string

	// Password 是 Digest 密码。
	Password string
}

// Client 封装 sipgo.Client,提供带 Digest 挑战应答的高层 Do 方法。
type Client struct {
	transport *Transport
	client    *sipgo.Client
	username  string
	password  string
}

// NewClient 基于 Transport 创建 sipgo Client。
func NewClient(transport *Transport, cfg ClientConfig) *Client {
	client, err := sipgo.NewClient(transport.UserAgent())
	if err != nil {
		// sipgo NewClient 只在 UA 为 nil 时失败,transport 已经保证 UA 非 nil,
		// 这里用 panic 而非返错误 (库前置条件违反,不是运行时错误)。
		panic(fmt.Sprintf("sipgo NewClient: %v", err))
	}
	return &Client{
		transport: transport,
		client:    client,
		username:  cfg.Username,
		password:  cfg.Password,
	}
}

// Sentinel errors。上层用 errors.Is 判定,不字符串匹配。
var (
	ErrTimeout           = errors.New("sip: request timeout")
	ErrPlatformRejected  = errors.New("sip: platform rejected")
	ErrProtocol          = errors.New("sip: protocol error")
	ErrOther             = errors.New("sip: other")
)

// Do 发送请求并自动处理 401 挑战应答。
//
// 流程:
//   1. 发原始 req,拿到响应
//   2. 若 2xx: 返回
//   3. 若 401 (WWW-Authenticate): 用 Digest MD5 重新构造 Authorization 头,重发
//   4. 若 4xx-6xx (非 401): 返回 ErrPlatformRejected 包装
//   5. 若超时: 返回 ErrTimeout 包装
func (c *Client) Do(ctx context.Context, req *sip.Request) (*sip.Response, error) {
	resp, err := c.client.Do(ctx, req)
	if err != nil {
		if errors.Is(err, context.DeadlineExceeded) || errors.Is(err, context.Canceled) {
			return nil, fmt.Errorf("%w: %v", ErrTimeout, err)
		}
		return nil, fmt.Errorf("%w: %v", ErrOther, err)
	}

	// 401 挑战应答
	if resp.StatusCode == 401 {
		return c.doDigestChallenge(ctx, req, resp)
	}

	if resp.StatusCode >= 400 {
		return resp, fmt.Errorf("%w: %d %s", ErrPlatformRejected, resp.StatusCode, resp.Reason)
	}

	return resp, nil
}

// doDigestChallenge 处理 401 挑战:解析 WWW-Authenticate → 构造 Authorization → 重发。
//
// 使用 sipgo 内建 DoDigestAuth,支持 MD5 + qop=auth (RFC 7616 主流子集)。
// 若平台 stale=true 或 nonce 过期,sipgo 内部会重新解析新 nonce,不缓存复用 (spec R4)。
func (c *Client) doDigestChallenge(
	ctx context.Context,
	req *sip.Request,
	challenge *sip.Response,
) (*sip.Response, error) {
	authOpts := sipgo.DigestAuth{
		Username: c.username,
		Password: c.password,
	}
	resp, err := c.client.DoDigestAuth(ctx, req, challenge, authOpts)
	if err != nil {
		if errors.Is(err, context.DeadlineExceeded) || errors.Is(err, context.Canceled) {
			return nil, fmt.Errorf("%w: %v", ErrTimeout, err)
		}
		return nil, fmt.Errorf("%w: %v", ErrOther, err)
	}

	if resp.StatusCode >= 400 {
		return resp, fmt.Errorf("%w: %d %s (鉴权后)", ErrPlatformRejected, resp.StatusCode, resp.Reason)
	}
	return resp, nil
}

// SipgoClient 暴露原始 sipgo.Client 给需要底层能力的调用者 (谨慎使用)。
func (c *Client) SipgoClient() *sipgo.Client {
	return c.client
}
