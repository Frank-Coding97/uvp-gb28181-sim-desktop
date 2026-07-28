// MANSCDP XML 编码器.
//
// S-B1 修订 (v4): XML 声明与实际字节编码必须一致, 无 "声明 GB2312 实际 GB18030" 混搭.
//
// 三模式:
//   - GB18030 (默认): 声明 GB18030 + 编码器 GB18030, 全字符集覆盖
//   - GB2312 兼容: 声明 GB2312 + 编码器 GBK (GB2312 严格超集), 老平台方言
//   - UTF-8: 声明 UTF-8 + 原生, 部分工具/测试
package manscdp

import (
	"encoding/xml"
	"errors"
	"fmt"

	"golang.org/x/text/encoding/simplifiedchinese"
)

// Encoding MANSCDP body 编码选项.
type Encoding string

const (
	EncodingGB18030 Encoding = "GB18030" // 默认, GB-2022 规范
	EncodingGB2312  Encoding = "GB2312"  // 老平台兼容 (实际用 GBK 编码器)
	EncodingUTF8    Encoding = "UTF-8"   // 部分工具方言
)

// 分片字节上限 (spec Q5 + R3-B1 修订).
const (
	// MaxBodyBytesUDP UDP 单包 body 上限 = 1000 字节.
	// 为 SIP headers (Via/From/To/Call-ID/CSeq/Content-Type/Max-Forwards/Content-Length + Request-URI,
	// 实测 350-500 字节) 预留 500 字节, 保证完整 UDP datagram (body + SIP headers + IP/UDP 28B header)
	// <= 常见 MTU 1500 字节, 避免 IP 分片.
	MaxBodyBytesUDP = 1000
	// MaxBodyBytesTCP TCP 单包 body 上限 = 32KB (TCP 有可靠字节流分帧).
	MaxBodyBytesTCP = 32768
)

// ErrItemTooLarge 单个 CatalogItem 编码后超过 maxBodyBytes 时返回.
// N-B6 + R3-M3: 命名传输无关 (TCP 也可能触发), 绝不发送超限包.
var ErrItemTooLarge = errors.New("catalog item too large for body size limit")

// ErrUnsupportedEncoding 未知编码.
var ErrUnsupportedEncoding = errors.New("unsupported encoding")

// EncodeResponse 序列化 MANSCDP Response 结构体 + 按 enc 编码字节.
//
// 流程:
//  1. xml.MarshalIndent 生成 UTF-8 XML (不含声明)
//  2. 拼接对应编码的 <?xml ?> 声明
//  3. 用目标编码器 (GB18030 / GBK / UTF-8 原生) 转字节
//
// S-B1 硬约束: 声明与实际编码器**严格成对**, 禁止跨用.
func EncodeResponse(v interface{}, enc Encoding) ([]byte, error) {
	// 1. 生成 UTF-8 XML bytes
	xmlBytes, err := xml.MarshalIndent(v, "", "  ")
	if err != nil {
		return nil, fmt.Errorf("manscdp: marshal xml: %w", err)
	}

	// 2. 拼接编码声明
	decl, err := xmlDeclFor(enc)
	if err != nil {
		return nil, err
	}
	utf8Payload := append([]byte(decl), xmlBytes...)

	// 3. 转目标编码
	return encodeBytes(utf8Payload, enc)
}

// xmlDeclFor 返回与 enc 严格一致的 XML 声明.
func xmlDeclFor(enc Encoding) (string, error) {
	switch enc {
	case EncodingGB18030:
		return `<?xml version="1.0" encoding="GB18030"?>` + "\n", nil
	case EncodingGB2312:
		return `<?xml version="1.0" encoding="GB2312"?>` + "\n", nil
	case EncodingUTF8:
		return `<?xml version="1.0" encoding="UTF-8"?>` + "\n", nil
	default:
		return "", fmt.Errorf("%w: %s", ErrUnsupportedEncoding, enc)
	}
}

// encodeBytes 用 enc 对应编码器把 UTF-8 payload 转字节.
func encodeBytes(utf8Payload []byte, enc Encoding) ([]byte, error) {
	switch enc {
	case EncodingGB18030:
		return simplifiedchinese.GB18030.NewEncoder().Bytes(utf8Payload)
	case EncodingGB2312:
		// GBK 是 GB2312 严格超集, 覆盖常用字更全.
		// R3-M4: 若遇 GBK 集外字符 (emoji), encoder 返错, 上层可降级 GB18030.
		return simplifiedchinese.GBK.NewEncoder().Bytes(utf8Payload)
	case EncodingUTF8:
		// Go 原生 UTF-8, 直接返回拷贝.
		out := make([]byte, len(utf8Payload))
		copy(out, utf8Payload)
		return out, nil
	default:
		return nil, fmt.Errorf("%w: %s", ErrUnsupportedEncoding, enc)
	}
}

