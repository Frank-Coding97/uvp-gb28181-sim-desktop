// MANSCDP Query 解析器.
//
// P-B1 修订 (v4): 用 xml.NewDecoder + CharsetReader 支持 GB18030/GB2312/UTF-8 声明的入站报文.
// xml.Unmarshal 无法可靠解析非 UTF-8 XML, 必须走 Decoder + charset.NewReaderLabel.
package manscdp

import (
	"bytes"
	"encoding/xml"
	"errors"
	"fmt"

	"golang.org/x/net/html/charset"
)

// ErrParseQuery Query 解析失败 (畸形 XML / 缺字段).
var ErrParseQuery = errors.New("manscdp: parse query failed")

// ParseQuery 从 MESSAGE body 解析 Query.
// 支持 GB18030 / GB2312 / UTF-8 声明的入站 XML.
// 校验必填: CmdType, SN (非 0), DeviceID.
func ParseQuery(body []byte) (*Query, error) {
	if len(body) == 0 {
		return nil, fmt.Errorf("%w: empty body", ErrParseQuery)
	}
	dec := xml.NewDecoder(bytes.NewReader(body))
	// 关键: 让 decoder 按 XML 声明的 encoding 属性自动选码.
	// charset.NewReaderLabel 处理 "GB18030" / "GB2312" / "GBK" / "UTF-8" 等常见标签.
	dec.CharsetReader = charset.NewReaderLabel

	var q Query
	if err := dec.Decode(&q); err != nil {
		return nil, fmt.Errorf("%w: %v", ErrParseQuery, err)
	}

	if q.CmdType == "" {
		return nil, fmt.Errorf("%w: missing CmdType", ErrParseQuery)
	}
	// SN == 0 通常是缺字段 (国标 SN 恒 >0, 平台从 1 起递增)
	// 若未来出现合法 SN=0 场景, 此处放宽
	if q.SN == 0 {
		return nil, fmt.Errorf("%w: missing SN", ErrParseQuery)
	}
	if q.DeviceID == "" {
		return nil, fmt.Errorf("%w: missing DeviceID", ErrParseQuery)
	}
	return &q, nil
}
