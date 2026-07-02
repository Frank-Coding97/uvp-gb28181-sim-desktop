//! SIP Digest 认证(MD5)。
//!
//! M0 占位。M1 实现:解析 401/407 的 `WWW-Authenticate` 挑战,
//! 计算 `response = MD5(HA1:nonce:HA2)`,构造 `Authorization` 头。GB28181 用 MD5。
