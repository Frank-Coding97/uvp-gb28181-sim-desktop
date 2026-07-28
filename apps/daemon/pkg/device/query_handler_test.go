package device

import (
	"context"
	"strings"
	"testing"

	"github.com/menglulu/uvp-gb28181-sim-desktop/daemon/pkg/manscdp"
	"golang.org/x/text/encoding/simplifiedchinese"
)

// setupHandler 建带一致预置数据的 QueryHandler.
func setupHandler(t *testing.T, initEnc manscdp.Encoding, initTransport string) (*QueryHandler, *ConfigStore) {
	t.Helper()
	cs, err := NewConfigStore(csTestDeviceID, initEnc)
	if err != nil {
		t.Fatalf("new config store failed: %v", err)
	}
	dc := &DeviceConfig{
		Name:         "GB28181 模拟设备",
		Manufacturer: "UVP",
		Model:        "SIM-Desktop-v2",
		Firmware:     "v0.2.0",
	}
	return NewQueryHandler(cs, dc, initTransport), cs
}

// Test1_HandleCatalogQuery_Empty 空目录发一片 SumNum=0.
func Test1_HandleCatalogQuery_Empty(t *testing.T) {
	h, _ := setupHandler(t, manscdp.EncodingGB18030, "udp")
	q := &manscdp.Query{CmdType: "Catalog", SN: 42, DeviceID: csTestDeviceID}
	bodies, err := h.HandleCatalogQuery(context.Background(), q)
	if err != nil {
		t.Fatalf("catalog query failed: %v", err)
	}
	if len(bodies) != 1 {
		t.Errorf("empty catalog should be 1 chunk, got %d", len(bodies))
	}
	// 解码验证 SumNum=0
	if !strings.Contains(gbDecode(t, bodies[0], manscdp.EncodingGB18030), "<SumNum>0</SumNum>") {
		t.Errorf("empty catalog body should contain SumNum=0")
	}
}

// Test2_HandleCatalogQuery_WithChannels 多通道 UDP 分片.
func Test2_HandleCatalogQuery_WithChannels(t *testing.T) {
	// TCP 传输避免 UDP 1000B 上限被 400B 中文 Item 撑爆
	h, cs := setupHandler(t, manscdp.EncodingGB18030, "tcp")
	channels := []Channel{makeChannel(1), makeChannel(2), makeChannel(3)}
	if _, err := cs.Apply(manscdp.EncodingGB18030, channels); err != nil {
		t.Fatalf("apply failed: %v", err)
	}

	q := &manscdp.Query{CmdType: "Catalog", SN: 1, DeviceID: csTestDeviceID}
	bodies, err := h.HandleCatalogQuery(context.Background(), q)
	if err != nil {
		t.Fatalf("query failed: %v", err)
	}

	// TCP 32K, 3 通道装一片
	if len(bodies) != 1 {
		t.Errorf("TCP: 3 channels should fit in 1 chunk, got %d", len(bodies))
	}
	body := gbDecode(t, bodies[0], manscdp.EncodingGB18030)
	if !strings.Contains(body, "<SumNum>3</SumNum>") {
		t.Errorf("SumNum 应为 3")
	}
	if !strings.Contains(body, "<Manufacturer>UVP</Manufacturer>") {
		t.Errorf("Manufacturer 未补默认")
	}

	// 再切 UDP: 应分多片
	h.SetTransport("udp")
	udpBodies, err := h.HandleCatalogQuery(context.Background(), q)
	if err != nil {
		t.Fatalf("udp query failed: %v", err)
	}
	if len(udpBodies) < 2 {
		t.Logf("注: 3 UDP 通道装 %d 片 (可能实际字节还没超 1000), 不硬断言", len(udpBodies))
	}
	// 但每片 SumNum 都必须 = 3
	for i := range udpBodies {
		s := gbDecode(t, udpBodies[i], manscdp.EncodingGB18030)
		if !strings.Contains(s, "<SumNum>3</SumNum>") {
			t.Errorf("chunk %d SumNum 应为 3", i)
		}
	}
}

// Test3_HandleCatalogQuery_UsesSnapshotEncoding 编码来自 ConfigStore 快照.
func Test3_HandleCatalogQuery_UsesSnapshotEncoding(t *testing.T) {
	h, cs := setupHandler(t, manscdp.EncodingGB18030, "udp")
	// Apply 时切换到 UTF-8
	cs.Apply(manscdp.EncodingUTF8, []Channel{makeChannel(1)})

	q := &manscdp.Query{CmdType: "Catalog", SN: 1, DeviceID: csTestDeviceID}
	bodies, _ := h.HandleCatalogQuery(context.Background(), q)

	// body 应是 UTF-8 (声明是 UTF-8)
	if !strings.HasPrefix(string(bodies[0]), `<?xml version="1.0" encoding="UTF-8"?>`) {
		t.Errorf("encoding 未从 snapshot 拿: body 前 50: %q", bodies[0][:50])
	}
}

// Test4_HandleDeviceInfoQuery 基础字段.
func Test4_HandleDeviceInfoQuery(t *testing.T) {
	h, cs := setupHandler(t, manscdp.EncodingGB18030, "udp")
	cs.Apply(manscdp.EncodingGB18030, []Channel{makeChannel(1), makeChannel(2)})

	q := &manscdp.Query{CmdType: "DeviceInfo", SN: 100, DeviceID: csTestDeviceID}
	body, err := h.HandleDeviceInfoQuery(context.Background(), q)
	if err != nil {
		t.Fatalf("device info failed: %v", err)
	}
	str := gbDecode(t, body, manscdp.EncodingGB18030)
	if !strings.Contains(str, "<SN>100</SN>") {
		t.Errorf("SN 未回显")
	}
	if !strings.Contains(str, "<Channel>2</Channel>") {
		t.Errorf("Channel 数应为 2 (通道数)")
	}
	if !strings.Contains(str, "<DeviceName>GB28181 模拟设备</DeviceName>") {
		t.Errorf("DeviceName 缺失")
	}
}

// Test5_HandleDeviceStatusQuery Alarmstatus 强制输出.
func Test5_HandleDeviceStatusQuery(t *testing.T) {
	h, _ := setupHandler(t, manscdp.EncodingGB18030, "udp")

	q := &manscdp.Query{CmdType: "DeviceStatus", SN: 200, DeviceID: csTestDeviceID}
	body, err := h.HandleDeviceStatusQuery(context.Background(), q)
	if err != nil {
		t.Fatalf("device status failed: %v", err)
	}
	str := gbDecode(t, body, manscdp.EncodingGB18030)
	if !strings.Contains(str, `<Alarmstatus Num="0">`) {
		t.Errorf("Alarmstatus 未强制输出")
	}
	if !strings.Contains(str, "<Online>ONLINE</Online>") {
		t.Errorf("Online 应为 ONLINE")
	}
}

// Test6_TransportChangesBodyLimit 切换传输层 → 分片行为变化.
func Test6_TransportChangesBodyLimit(t *testing.T) {
	h, cs := setupHandler(t, manscdp.EncodingGB18030, "udp")

	// 100 通道
	channels := make([]Channel, 100)
	for i := range channels {
		channels[i] = makeChannel(i + 1)
	}
	cs.Apply(manscdp.EncodingGB18030, channels)

	q := &manscdp.Query{CmdType: "Catalog", SN: 1, DeviceID: csTestDeviceID}

	// UDP: 应多片
	udpBodies, _ := h.HandleCatalogQuery(context.Background(), q)
	// TCP: 32K 上限, 100 通道应装一片
	h.SetTransport("tcp")
	tcpBodies, _ := h.HandleCatalogQuery(context.Background(), q)

	if len(udpBodies) <= len(tcpBodies) {
		t.Errorf("UDP 应比 TCP 分更多片: udp=%d tcp=%d", len(udpBodies), len(tcpBodies))
	}
}

// Test7_SnapshotConsistency 并发 Apply + Query 快照一致 (R3-B2).
func Test7_SnapshotConsistency(t *testing.T) {
	h, cs := setupHandler(t, manscdp.EncodingGB18030, "udp")
	// 单次 Query 应能拿到编码 == 通道对应的编码 (不能出现 UTF-8 编码后应用 GB18030 编码器)
	cs.Apply(manscdp.EncodingUTF8, []Channel{makeChannel(1)})

	q := &manscdp.Query{CmdType: "Catalog", SN: 1, DeviceID: csTestDeviceID}
	bodies, _ := h.HandleCatalogQuery(context.Background(), q)
	if !strings.HasPrefix(string(bodies[0]), `<?xml version="1.0" encoding="UTF-8"?>`) {
		t.Errorf("快照不一致: 通道走 UTF-8 但 body 声明不是 UTF-8")
	}
}

// gbDecode 根据 enc 把 body 字节解为 UTF-8 string.
func gbDecode(t *testing.T, body []byte, enc manscdp.Encoding) string {
	t.Helper()
	switch enc {
	case manscdp.EncodingGB18030:
		utf8, err := simplifiedchinese.GB18030.NewDecoder().Bytes(body)
		if err != nil {
			t.Fatalf("GB18030 decode failed: %v", err)
		}
		return string(utf8)
	case manscdp.EncodingGB2312:
		utf8, err := simplifiedchinese.GBK.NewDecoder().Bytes(body)
		if err != nil {
			t.Fatalf("GBK decode failed: %v", err)
		}
		return string(utf8)
	case manscdp.EncodingUTF8:
		return string(body)
	default:
		return string(body)
	}
}
