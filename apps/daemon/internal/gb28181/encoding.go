package gb28181

import (
	"golang.org/x/text/encoding/simplifiedchinese"
	"golang.org/x/text/transform"
)

// EncodeGB18030 把 UTF-8 字符串编码为 GB18030 字节序列。
//
// GB/T 28181 A.1.2 规定信令字符集为 GB18030,大部分主流平台按此约定收发。
// 注册功能本身无 body,但 MANSCDP (心跳/目录/PTZ) 都要 GB18030 编码。
func EncodeGB18030(s string) ([]byte, error) {
	encoder := simplifiedchinese.GB18030.NewEncoder()
	out, _, err := transform.Bytes(encoder, []byte(s))
	if err != nil {
		return nil, err
	}
	return out, nil
}

// DecodeGB18030 把 GB18030 字节序列解码为 UTF-8 字符串。
//
// 用于收到平台 MANSCDP MESSAGE / NOTIFY 时解析 XML body。
func DecodeGB18030(b []byte) (string, error) {
	decoder := simplifiedchinese.GB18030.NewDecoder()
	out, _, err := transform.Bytes(decoder, b)
	if err != nil {
		return "", err
	}
	return string(out), nil
}
