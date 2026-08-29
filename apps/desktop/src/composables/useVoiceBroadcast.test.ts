// 语音广播面板的取数与判定逻辑。
//
// 重点锁两条:
// 1. 后端返回 null(设备未启动 / 无会话)时界面必须落到"空闲",而不是留着上一次的会话;
// 2. "多久没收到包"要能把"链路可疑"判出来 —— 这是附录 K a) 媒体流丢失监测在界面上的落点。
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { BroadcastSession } from "./useVoiceBroadcast";

let nextStatus: BroadcastSession | null = null;
let shouldThrow = false;
let hangupCalls = 0;

const invokeMock = vi.fn(async (cmd: string, _args?: unknown) => {
  if (cmd === "get_broadcast_status") {
    if (shouldThrow) throw new Error("boom");
    return nextStatus;
  }
  if (cmd === "hangup_broadcast") {
    hangupCalls += 1;
    nextStatus = null;
    return "已挂断语音广播并通知平台";
  }
  return undefined;
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));

const { useVoiceBroadcast, payloadTypeLabel } = await import("./useVoiceBroadcast");

function session(overrides: Partial<BroadcastSession["recv"]> = {}): BroadcastSession {
  return {
    source_id: "34020000002000000001",
    local_port: 41234,
    call_id: "bcast-1",
    recv: {
      packets: 100,
      payload_bytes: 16000,
      lost: 0,
      reordered: 0,
      malformed: 0,
      payload_type: 8,
      level_pct: 40,
      idle_ms: 20,
      jitter_ms: 1.5,
      rr_sent: 2,
      peer_sr_seen: true,
      ...overrides,
    },
  };
}

describe("语音广播状态", () => {
  const bc = useVoiceBroadcast();

  beforeEach(() => {
    nextStatus = null;
    shouldThrow = false;
    hangupCalls = 0;
    bc.session.value = null;
    bc.lastError.value = "";
    invokeMock.mockClear();
  });

  it("无会话时落到空闲态", async () => {
    bc.startBroadcastPolling();
    await vi.waitFor(() => expect(invokeMock).toHaveBeenCalled());
    bc.stopBroadcastPolling();
    expect(bc.session.value).toBeNull();
    expect(bc.hasMedia.value).toBe(false);
    expect(bc.stalled.value).toBe(false);
  });

  it("收到会话后暴露统计与音量", async () => {
    nextStatus = session();
    bc.startBroadcastPolling();
    await vi.waitFor(() => expect(bc.session.value).not.toBeNull());
    bc.stopBroadcastPolling();
    expect(bc.hasMedia.value).toBe(true);
    expect(bc.stalled.value).toBe(false);
    expect(bc.levelPercent.value).toBe("40%");
  });

  it("电平超过 100 时钳到 100(不能把音量条撑破)", async () => {
    nextStatus = session({ level_pct: 140 });
    bc.startBroadcastPolling();
    await vi.waitFor(() => expect(bc.session.value).not.toBeNull());
    bc.stopBroadcastPolling();
    expect(bc.levelPercent.value).toBe("100%");
  });

  it("建流但一直没有音频包时判为链路可疑", async () => {
    // 附录 K a):包断了超过 3 秒就该释放链路。
    nextStatus = session({ packets: 0, payload_bytes: 0, payload_type: null, idle_ms: 5000 });
    bc.startBroadcastPolling();
    await vi.waitFor(() => expect(bc.session.value).not.toBeNull());
    bc.stopBroadcastPolling();
    expect(bc.hasMedia.value).toBe(false);
    expect(bc.stalled.value).toBe(true);
  });

  it("idle 为 null(尚未开始计时)不算可疑", async () => {
    nextStatus = session({ idle_ms: null });
    bc.startBroadcastPolling();
    await vi.waitFor(() => expect(bc.session.value).not.toBeNull());
    bc.stopBroadcastPolling();
    expect(bc.stalled.value).toBe(false);
  });

  it("取数异常时清掉会话并记下错误", async () => {
    nextStatus = session();
    bc.startBroadcastPolling();
    await vi.waitFor(() => expect(bc.session.value).not.toBeNull());
    shouldThrow = true;
    await vi.waitFor(() => expect(bc.lastError.value).not.toBe(""));
    bc.stopBroadcastPolling();
    expect(bc.session.value).toBeNull();
  });

  it("挂断后立刻刷新为空闲", async () => {
    nextStatus = session();
    bc.startBroadcastPolling();
    await vi.waitFor(() => expect(bc.session.value).not.toBeNull());
    bc.stopBroadcastPolling();
    const result = await bc.hangup();
    expect(result.ok).toBe(true);
    expect(hangupCalls).toBe(1);
    expect(bc.session.value).toBeNull();
  });

  it("重复启动轮询不会叠加定时器", async () => {
    nextStatus = session();
    bc.startBroadcastPolling();
    bc.startBroadcastPolling();
    bc.startBroadcastPolling();
    await vi.waitFor(() => expect(bc.session.value).not.toBeNull());
    bc.stopBroadcastPolling();
    // 三次 start 只应触发一次立即取数(定时器只有一个)。
    expect(invokeMock.mock.calls.filter((c) => c[0] === "get_broadcast_status")).toHaveLength(1);
  });
});

describe("负载类型标签", () => {
  it("按 RFC 3551 静态绑定给出人读名称", () => {
    // 附录 C.2.4:语音广播用 G.711A(PT=8)。
    expect(payloadTypeLabel(8)).toContain("PCMA");
    expect(payloadTypeLabel(0)).toContain("PCMU");
    expect(payloadTypeLabel(96)).toContain("PS");
    expect(payloadTypeLabel(102)).toBe("PT 102");
    expect(payloadTypeLabel(null)).toBe("—");
  });
});
