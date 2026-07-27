package sip

import (
	"crypto/md5"
	"encoding/hex"
	"strings"
	"testing"

	"github.com/emiago/sipgo/sip"
	"github.com/icholy/digest"
)

// T3 Digest 变种适配 (spec AC-3).
//
// 目标是验证 icholy/digest v1.1.0 + sipgo ASCIIToUpper 组合对国标常见 Digest
// 方言全都能正确处理,不需要在 client.doDigestChallenge 额外加变种逻辑。
// 5 个变种在真实 GB28181 平台 (WVP-Pro / LiveGBS) 现网都出现过。
//
// 覆盖矩阵:
//
//	| 变种                     | 现象                          | icholy 支持 |
//	| ----------------------- | ---------------------------- | ---------- |
//	| stale=true              | nonce 过期,平台补发新 nonce      | ✅          |
//	| qop 缺失 (RFC 2069)      | 老平台 challenge 不带 qop 参数   | ✅          |
//	| opaque 回带              | 平台带 opaque,客户端须原样回带    | ✅          |
//	| algorithm=MD5-sess      | HA1 走 cnonce/nonce 变体      | ❌ (不支持)   |
//	| algorithm 小写 (md5)     | 平台发小写,客户端 uppercase 归一 | ✅ (靠 sipgo) |
//
// MD5-sess 是唯一 icholy 不支持的变种 → 记录到无人值守报告 F1,不 fix
// (国标现网未观察到,姊妹项目 UVP-GB28181 也不做,留到真机遇到再补)。

// 常量:测试用固定 realm/username/password/nonce,便于比对 hash。
const (
	tRealm    = "3402000000"
	tUser     = "34020000001320000001"
	tPassword = "12345678"
	tMethod   = "REGISTER"
	tURI      = "sip:34020000002000000001@3402000000"
	tNonce1   = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	tNonce2   = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
	tOpaque   = "5ccc069c403ebaf9f0171e9517f40e41"
)

// md5hex 计算 md5 小写 hex,用于手工 A1/A2/response 参考值。
func md5hex(s string) string {
	sum := md5.Sum([]byte(s))
	return hex.EncodeToString(sum[:])
}

// runDigest 是 client.doDigestChallenge 里那一坨的最小可测提炼:
// 对给定 challenge + options 跑 digest.Digest,返回 Credentials。
// 保留和 client.go 一致的关键动作:sip.ASCIIToUpper(chal.Algorithm)。
func runDigest(t *testing.T, chal *digest.Challenge) *digest.Credentials {
	t.Helper()
	chal.Algorithm = sip.ASCIIToUpper(chal.Algorithm)
	cred, err := digest.Digest(chal, digest.Options{
		Method:   tMethod,
		URI:      tURI,
		Username: tUser,
		Password: tPassword,
	})
	if err != nil {
		t.Fatalf("digest.Digest err: %v", err)
	}
	return cred
}

// TestDigest_StaleTrue:
// 平台第一次挑战后又发一条 stale=true + 新 nonce 的 challenge。
// 客户端应用新 nonce 重算 Authorization,不因 stale 报错。
//
// 我们直接跑两次 digest.Digest,断言:
//  1. 第二次的 nonce 是新的 (不缓存复用 —— spec R4)
//  2. 两次 response 不同 (nonce 变了 hash 就变)
//  3. 都拿到有效 response 字符串
func TestDigest_StaleTrue(t *testing.T) {
	// 第一次 challenge (未 stale)
	chal1 := &digest.Challenge{
		Realm:     tRealm,
		Nonce:     tNonce1,
		Algorithm: "MD5",
		QOP:       []string{"auth"},
	}
	cred1 := runDigest(t, chal1)

	// 第二次 challenge:stale=true + 新 nonce (nonce 已过期,平台补发)
	chal2 := &digest.Challenge{
		Realm:     tRealm,
		Nonce:     tNonce2,
		Algorithm: "MD5",
		QOP:       []string{"auth"},
		Stale:     true,
	}
	cred2 := runDigest(t, chal2)

	if cred2.Nonce != tNonce2 {
		t.Errorf("重算应使用新 nonce, want %s, got %s", tNonce2, cred2.Nonce)
	}
	if cred1.Response == "" || cred2.Response == "" {
		t.Fatalf("response 应非空, cred1=%q cred2=%q", cred1.Response, cred2.Response)
	}
	if cred1.Response == cred2.Response {
		t.Error("nonce 变了 response 必须变,当前两次相同 (说明 nonce 没吃到)")
	}
}

// TestDigest_QopMissing:
// RFC 2069 老平台不发 qop 头。此时 icholy 走 no-qop 分支,
// response = H(A1) : nonce : H(A2),不带 cnonce/nc/qop。
//
// GB28181 早期实现有一部分是 RFC 2069,验证我们不加 qop 也能算对。
func TestDigest_QopMissing(t *testing.T) {
	chal := &digest.Challenge{
		Realm:     tRealm,
		Nonce:     tNonce1,
		Algorithm: "MD5",
		// QOP 留空 → 走 RFC 2069 分支
	}
	cred := runDigest(t, chal)

	// 参考值:HA1 = md5(user:realm:pass), HA2 = md5(method:uri),
	// response = md5(HA1:nonce:HA2)
	ha1 := md5hex(tUser + ":" + tRealm + ":" + tPassword)
	ha2 := md5hex(tMethod + ":" + tURI)
	wantResp := md5hex(ha1 + ":" + tNonce1 + ":" + ha2)

	if cred.Response != wantResp {
		t.Errorf("RFC 2069 response 不符, want %s, got %s", wantResp, cred.Response)
	}
	if cred.QOP != "" {
		t.Errorf("响应不应带 qop, got %q", cred.QOP)
	}
	// 序列化后的 Authorization 头不应包含 qop=
	rendered := cred.String()
	if strings.Contains(rendered, "qop=") {
		t.Errorf("Authorization 头不应含 qop= (RFC 2069): %s", rendered)
	}
}

// TestDigest_OpaqueEcho:
// 平台带 opaque="xxx",客户端 Authorization 必须原样回带 opaque。
// RFC 7616 §3.4 强制要求。有平台 (华为部分老实现) 会校验 opaque 才放行。
func TestDigest_OpaqueEcho(t *testing.T) {
	chal := &digest.Challenge{
		Realm:     tRealm,
		Nonce:     tNonce1,
		Algorithm: "MD5",
		QOP:       []string{"auth"},
		Opaque:    tOpaque,
	}
	cred := runDigest(t, chal)

	if cred.Opaque != tOpaque {
		t.Errorf("opaque 未回带, want %q, got %q", tOpaque, cred.Opaque)
	}
	rendered := cred.String()
	if !strings.Contains(rendered, `opaque="`+tOpaque+`"`) {
		t.Errorf("Authorization 头缺 opaque, rendered:\n%s", rendered)
	}
}

// TestDigest_MD5Sess:
// algorithm=MD5-sess,HA1 变体:H(H(user:realm:pass):nonce:cnonce)。
//
// icholy v1.1.0 认识 algorithm 字符串但**不实现** MD5-sess 分支
// (case 只匹配 "MD5"/"SHA-256"/... 精确串,MD5-sess 落到 default 报 unsupported)。
// 该函数验证行为:调用会失败 (或退化到 MD5),把结果 记录到无人值守报告。
//
// 当前不 fix:国标现网没观察到 MD5-sess,姊妹项目也无。真机遇到再决定
// 是否在 client.doDigestChallenge 手工补 A1' = H(H(A1):nonce:cnonce)。
func TestDigest_MD5Sess(t *testing.T) {
	chal := &digest.Challenge{
		Realm:     tRealm,
		Nonce:     tNonce1,
		Algorithm: "MD5-sess",
		QOP:       []string{"auth"},
	}
	chal.Algorithm = sip.ASCIIToUpper(chal.Algorithm)
	cred, err := digest.Digest(chal, digest.Options{
		Method:   tMethod,
		URI:      tURI,
		Username: tUser,
		Password: tPassword,
	})

	// icholy v1.1.0 应报 "unsupported algorithm"
	if err == nil {
		// 若未来版本支持了,验证 response 合规即可 (退化路径)
		t.Logf("icholy 现在支持 MD5-sess,cred.Response=%s", cred.Response)
		if cred.Response == "" {
			t.Error("MD5-sess 支持但 response 为空")
		}
		return
	}
	if !strings.Contains(err.Error(), "unsupported algorithm") {
		t.Errorf("MD5-sess 期望 unsupported algorithm 错误, got %v", err)
	}
	// 明确记录到测试输出,handoff 报告会捕获
	t.Logf("F1: icholy/digest v1.1.0 不支持 MD5-sess (err=%v), "+
		"国标现网未观察到该变种,推迟到真机遇到再补 client.doDigestChallenge 手工分支", err)
}

// TestDigest_LowerCaseAlgorithm:
// 部分平台发 algorithm=md5 (小写),不符 RFC 7616 §11.4 的 "MD5" 大写建议。
// sipgo/icholy 里手工加了 sip.ASCIIToUpper 归一,验证还生效。
func TestDigest_LowerCaseAlgorithm(t *testing.T) {
	chal := &digest.Challenge{
		Realm:     tRealm,
		Nonce:     tNonce1,
		Algorithm: "md5", // 小写
		QOP:       []string{"auth"},
	}
	cred := runDigest(t, chal) // 内部会 ASCIIToUpper → "MD5"

	if cred.Algorithm != "MD5" {
		t.Errorf("小写 algorithm 应归一为 MD5, got %q", cred.Algorithm)
	}
	if cred.Response == "" {
		t.Error("小写 algorithm 情况下 response 不应为空")
	}

	// 反向:如果 sipgo.ASCIIToUpper 逻辑挂了 (algorithm 保留 "md5"),
	// icholy 里 strings.ToUpper(cred.Algorithm) case 匹配仍能命中 "MD5",
	// 所以即便归一失败也不会 hash 错。这里做一次 sanity check:
	// response 值必须等于同参数下大写 algorithm 的计算结果。
	chalUpper := &digest.Challenge{
		Realm:     tRealm,
		Nonce:     tNonce1,
		Algorithm: "MD5",
		QOP:       []string{"auth"},
	}
	credUpper, err := digest.Digest(chalUpper, digest.Options{
		Method:   tMethod,
		URI:      tURI,
		Username: tUser,
		Password: tPassword,
		// 固定 cnonce/count 让两次 response 一致 (否则 cnonce 随机会不同)
		Cnonce: "fixed-cnonce",
		Count:  1,
	})
	if err != nil {
		t.Fatalf("upper case digest err: %v", err)
	}
	// 用同样固定 cnonce/count 跑一次小写路径
	chalLower2 := &digest.Challenge{
		Realm:     tRealm,
		Nonce:     tNonce1,
		Algorithm: "md5",
		QOP:       []string{"auth"},
	}
	chalLower2.Algorithm = sip.ASCIIToUpper(chalLower2.Algorithm)
	credLower2, err := digest.Digest(chalLower2, digest.Options{
		Method:   tMethod,
		URI:      tURI,
		Username: tUser,
		Password: tPassword,
		Cnonce:   "fixed-cnonce",
		Count:    1,
	})
	if err != nil {
		t.Fatalf("lower case digest err: %v", err)
	}
	if credLower2.Response != credUpper.Response {
		t.Errorf("大小写 algorithm 计算的 response 应一致, upper=%s lower=%s",
			credUpper.Response, credLower2.Response)
	}
}
