// 推流质量卡的状态判定测试。
//
// 重点锁三条:
// 1. "平台没回 RR"和"平台回报 0% 丢包"必须是两种不同的界面状态 ——
//    把前者画成"一切正常"会让人以为链路已经验证过了;
// 2. 反馈曾经来过又断掉(age 超过阈值)要能判出来:这比"从来没有"更值得警惕;
// 3. 后端返回 null(无会话)时必须落回空闲,不能留着上一次的读数。
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { MediaTelemetry, RtcpFeedback } from "./useMediaTelemetry";

let nextTelemetry: MediaTelemetry | null = null;
let shouldThrow = false;

const invokeMock = vi.fn(async (cmd: string, _args?: unknown) => {
  if (cmd === "get_media_telemetry") {
    if (shouldThrow) throw new Error("boom");
    return nextTelemetry;
  }
  return undefined;
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));

const { useMediaTelemetry, formatBitrate, formatBytes, formatMs } = await import(
  "./useMediaTelemetry"
);

function feedback(overrides: Partial<RtcpFeedback> = {}): RtcpFeedback {
  return {
    reports: 0,
    loss_percent: 0,
    cumulative_lost: 0,
    jitter_ms: 0,
    jitter_peak_ms: 0,
    rtt_ms: null,
    reporter_ssrc: 0,
    age_ms: null,
    peer_bye: false,
    unparsed: 0,
    ...overrides,
  };
}

function telemetry(overrides: Partial<MediaTelemetry> = {}): MediaTelemetry {
  return {
    is_history: false,
    media_source: null,
    local_rtcp_port: 30001,
    sent_packets: 1200,
    sent_bytes: 1_440_000,
    sent_rtcp: 4,
    send_errors: 0,
    feedback: feedback(),
    ...overrides,
  };
}

describe("推流质量遥测", () => {
  const tm = useMediaTelemetry();

  beforeEach(() => {
    nextTelemetry = null;
    shouldThrow = false;
    tm.telemetry.value = null;
    tm.lastError.value = "";
    invokeMock.mockClear();
  });

  it("无会话时落到空闲态", async () => {
    await tm.refreshMediaTelemetry();
    expect(tm.streaming.value).toBe(false);
    expect(tm.hasFeedback.value).toBe(false);
    // 没有会话时不该提示"反馈不可用" —— 那会把"没在推流"说成"反馈有问题"。
    expect(tm.feedbackUnavailable.value).toBe(false);
    expect(tm.qualityWarning.value).toBe(false);
  });

  it("推流中但平台一条 RR 都没回时标为反馈不可用", async () => {
    nextTelemetry = telemetry();
    await tm.refreshMediaTelemetry();
    expect(tm.streaming.value).toBe(true);
    expect(tm.hasFeedback.value).toBe(false);
    expect(tm.feedbackUnavailable.value).toBe(true);
    // 关键:没有反馈时不能因为 loss_percent 是 0 就报"质量正常"。
    expect(tm.qualityWarning.value).toBe(false);
  });

  it("平台回报 0% 丢包与未回报是两种状态", async () => {
    nextTelemetry = telemetry({
      feedback: feedback({ reports: 3, loss_percent: 0, age_ms: 1200 }),
    });
    await tm.refreshMediaTelemetry();
    expect(tm.hasFeedback.value).toBe(true);
    expect(tm.feedbackUnavailable.value).toBe(false);
    expect(tm.feedbackStale.value).toBe(false);
    expect(tm.qualityWarning.value).toBe(false);
  });

  it("丢包率或抖动越线时给出质量告警", async () => {
    nextTelemetry = telemetry({
      feedback: feedback({ reports: 2, loss_percent: 2.5, age_ms: 800 }),
    });
    await tm.refreshMediaTelemetry();
    expect(tm.qualityWarning.value).toBe(true);

    // 丢包为 0 但抖动大也要告警:抖动同样会让画面卡。
    nextTelemetry = telemetry({
      feedback: feedback({ reports: 2, loss_percent: 0, jitter_ms: 80, age_ms: 800 }),
    });
    await tm.refreshMediaTelemetry();
    expect(tm.qualityWarning.value).toBe(true);

    // 刚好在阈值以下不告警。
    nextTelemetry = telemetry({
      feedback: feedback({ reports: 2, loss_percent: 0.5, jitter_ms: 20, age_ms: 800 }),
    });
    await tm.refreshMediaTelemetry();
    expect(tm.qualityWarning.value).toBe(false);
  });

  it("反馈中断超过阈值时标为已中断", async () => {
    nextTelemetry = telemetry({
      feedback: feedback({ reports: 5, age_ms: 30_000 }),
    });
    await tm.refreshMediaTelemetry();
    expect(tm.hasFeedback.value).toBe(true);
    expect(tm.feedbackStale.value).toBe(true);
  });

  it("轮询启停幂等且能把异常写进 lastError", async () => {
    nextTelemetry = telemetry();
    tm.startMediaTelemetry();
    tm.startMediaTelemetry(); // 重复启动不应再起一个定时器
    await vi.waitFor(() => expect(invokeMock).toHaveBeenCalled());
    tm.stopMediaTelemetry();
    tm.stopMediaTelemetry();
    expect(tm.streaming.value).toBe(true);

    shouldThrow = true;
    await tm.refreshMediaTelemetry();
    expect(tm.lastError.value).toContain("boom");
    expect(tm.telemetry.value).toBeNull();
  });

  it("格式化:字节/毫秒/平均包长", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2.0 KB");
    expect(formatBytes(5 * 1024 * 1024)).toBe("5.00 MB");
    expect(formatBytes(3 * 1024 * 1024 * 1024)).toBe("3.00 GB");

    expect(formatMs(null)).toBe("—");
    expect(formatMs(4.25)).toBe("4.3 ms");
    expect(formatMs(120)).toBe("120 ms");
    expect(formatMs(2500)).toBe("2.5 s");

    // 没发过包时不编一个平均值出来。
    expect(formatBitrate(0, 0)).toBe("—");
    expect(formatBitrate(1400, 1)).toBe("1400 B/包");
  });
});
