// Package device 提供设备端业务状态机。
//
// M1 仅实现 RegistrationSession 最小闭环:Start → Registered → 等待退。
// 心跳/续约/注销归 M3,状态机会扩展 Renewing / ShuttingDown 子状态。
package device

// State 是注册会话的对外可见状态。
type State int32

const (
	// StateDisconnected 未连接:初始态或注销后的稳态。
	StateDisconnected State = iota

	// StateRegistering 注册中:REGISTER 已发送等待响应,或 401 后重发中。
	StateRegistering

	// StateRegistered 已注册:200 OK 已收,心跳/续约都在此稳态内进行。
	// 续约期间也保持此状态(用户不感知,spec Q9)。
	StateRegistered

	// StateFailed 注册失败:超时/被拒/心跳降级。
	StateFailed
)

func (s State) String() string {
	switch s {
	case StateDisconnected:
		return "Disconnected"
	case StateRegistering:
		return "Registering"
	case StateRegistered:
		return "Registered"
	case StateFailed:
		return "Failed"
	default:
		return "Unknown"
	}
}
