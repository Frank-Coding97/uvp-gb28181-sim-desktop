package gb28181

import "testing"

func TestGB18030RoundTrip(t *testing.T) {
	cases := []string{
		"",
		"pure ASCII",
		"测试设备-01",
		"你好, world!",
		"混排 mixed ASCII 中文 & 符号 · 【】",
	}
	for _, s := range cases {
		t.Run(s, func(t *testing.T) {
			enc, err := EncodeGB18030(s)
			if err != nil {
				t.Fatalf("encode: %v", err)
			}
			dec, err := DecodeGB18030(enc)
			if err != nil {
				t.Fatalf("decode: %v", err)
			}
			if dec != s {
				t.Errorf("round trip mismatch: got %q, want %q", dec, s)
			}
		})
	}
}

func TestGB18030_ASCIIUnchanged(t *testing.T) {
	// 纯 ASCII 编码后字节应与 UTF-8 一致 (GB18030 兼容 ASCII)
	s := "REGISTER sip:test@example.com SIP/2.0"
	enc, err := EncodeGB18030(s)
	if err != nil {
		t.Fatal(err)
	}
	if string(enc) != s {
		t.Errorf("ASCII should encode unchanged: got %q", string(enc))
	}
}
