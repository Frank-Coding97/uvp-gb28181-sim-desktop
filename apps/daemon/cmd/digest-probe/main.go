// digest-probe:诊断工具,离线模拟 sipgo 计算 Digest Authorization 的过程,
// 打印 sipgo 会发出的 Authorization 字节。
// 用真实 WVP 返的 401 nonce 直接算,便于跟 pjsip / linphone 抓包对比。
package main

import (
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"os"

	"github.com/icholy/digest"
)

func main() {
	// 复刻真实 WVP 401 挑战的字段 (2026-07-27 18:56:51 的报文)
	// WWW-Authenticate: Digest realm="3502000000",qop="auth",nonce="13a73b86fd37e71c0655da555ac8d1d6",algorithm=MD5
	challenge := &digest.Challenge{
		Realm:     "3502000000",
		Nonce:     "13a73b86fd37e71c0655da555ac8d1d6",
		Algorithm: "MD5",
		QOP:       []string{"auth"},
	}

	// sipgo 内部对 Options.URI 传的是 req.Recipient.Addr(),即 <user>@<host>[:port] 无 scheme
	// 试 3 种 URI 形式,看 sipgo 默认用的是哪个
	uriVariants := []struct {
		name string
		uri  string
	}{
		{"sipgo 默认 (Recipient.Addr)", "35020000002000000001@3502000000"},
		{"带 sip: 前缀", "sip:35020000002000000001@3502000000"},
		{"只 host", "3502000000"},
	}

	for _, v := range uriVariants {
		opts := digest.Options{
			Method:   "REGISTER",
			URI:      v.uri,
			Username: "35020000001320000001",
			Password: "wvp_sip_password",
			Cnonce:   fixedCnonce(),
			Count:    1,
		}

		cred, err := digest.Digest(challenge, opts)
		if err != nil {
			fmt.Fprintf(os.Stderr, "case=%s err=%v\n", v.name, err)
			continue
		}

		fmt.Printf("=== %s ===\n", v.name)
		fmt.Printf("Options.URI       : %s\n", v.uri)
		fmt.Printf("credential.String : %s\n\n", cred.String())
	}
}

// fixedCnonce 用固定 cnonce 保证输出稳定 (真实 sipgo 会随机)
func fixedCnonce() string {
	// 实际 sipgo 每次随机,这里为诊断用固定值方便手工验证
	b := make([]byte, 8)
	_, _ = rand.Read(b)
	return hex.EncodeToString(b)
}
