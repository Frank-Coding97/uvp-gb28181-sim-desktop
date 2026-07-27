package gb28181

// 国标 GB/T 28181 与 SIP 相关的默认常量。
// 独立文件方便测试引用,也避免散落在业务代码里的魔法数字。
const (
	// DefaultExpiresSecs 是 REGISTER Expires 头请求值默认。
	// 国标 A.1.1 推荐 3600 秒。
	DefaultExpiresSecs = 3600

	// DefaultHeartbeatSecs 是 Keepalive 心跳默认周期。
	// 国标 A.2.5 推荐 60 秒,过短加重平台负载,过长故障感知太慢。
	DefaultHeartbeatSecs = 60

	// UserAgent 是 SIP User-Agent 头值,标识设备端软件版本。
	UserAgent = "UVP-Sim-Desktop/0.2.0"

	// DefaultSIPPort 是国标推荐的 SIP 监听端口。
	DefaultSIPPort = 5060

	// MaxHeartbeatFailBeforeDown 是心跳连续失败几次判定注册失败。
	// spec Q4: 3 次(约 3 分钟) 给出确定性下线判断。
	MaxHeartbeatFailBeforeDown = 3

	// ExpiresRenewalRatio 是续约触发时机:实际 Expires 的多少比例。
	// spec Q9: 0.8,但至少留 60 秒余量。
	ExpiresRenewalRatio = 0.8
)
