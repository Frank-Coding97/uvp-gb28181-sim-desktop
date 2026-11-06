import assert from "node:assert/strict";
import test from "node:test";
import {
  QUICK_RANGES,
  customRange,
  durationSeconds,
  filterRecordings,
  formatDuration,
  formatLocalInput,
  overlapsRange,
  parseLocalInput,
  quickRange,
  startOfDayMs,
} from "./recording-filter.mjs";

const HOUR = 3_600_000;
const DAY = 86_400_000;

/** 用本地日历拼时间戳，避免测试对时区写死。 */
function local(y, m, d, hh = 0, mm = 0) {
  return new Date(y, m - 1, d, hh, mm, 0, 0).getTime();
}

function entry(id, startMs, endMs, kind = "manual", channel = "ch-1", path = `/rec/${id}.ps`) {
  return { id, channel_id: channel, path, kind, start_time_ms: startMs, end_time_ms: endMs };
}

test("当天零点按本地时区计算", () => {
  const now = local(2026, 10, 6, 15, 30);
  assert.equal(startOfDayMs(now), local(2026, 10, 6));
});

test("今天与近 N 天区间左闭右开且含当天", () => {
  const now = local(2026, 10, 6, 15, 30);
  assert.deepEqual(quickRange("today", now), {
    from: local(2026, 10, 6),
    to: local(2026, 10, 7),
  });
  // 近 7 天含今天，所以从 6 天前的零点起。
  assert.deepEqual(quickRange("7d", now), {
    from: local(2026, 9, 30),
    to: local(2026, 10, 7),
  });
  assert.deepEqual(quickRange("30d", now), {
    from: local(2026, 9, 7),
    to: local(2026, 10, 7),
  });
});

test("全部时间与自定义不产生固定区间", () => {
  assert.equal(quickRange("all", local(2026, 10, 6)), null);
  assert.equal(quickRange("custom", local(2026, 10, 6)), null);
  assert.equal(quickRange("unknown", local(2026, 10, 6)), null);
  assert.equal(QUICK_RANGES.length, 5);
});

test("跨夜录像按重叠命中而不是按开始时间", () => {
  // 10-05 20:00 录到 10-06 02:00：用户找「今天这段画面」时必须能筛出来。
  const overnight = entry("rec-night", local(2026, 10, 5, 20), local(2026, 10, 6, 2));
  const today = quickRange("today", local(2026, 10, 6, 9));
  assert.ok(overlapsRange(overnight, today.from, today.to));
  assert.equal(filterRecordings([overnight], { from: today.from, to: today.to }).length, 1);
});

test("恰好落在窗口边界外的录像被排除", () => {
  const today = quickRange("today", local(2026, 10, 6, 9));
  // 昨天 23:00-24:00 结束于今天零点，交集为空（左闭右开）。
  const before = entry("rec-before", local(2026, 10, 5, 23), local(2026, 10, 6));
  assert.equal(overlapsRange(before, today.from, today.to), false);
  // 明天零点开始，同样不命中。
  const after = entry("rec-after", local(2026, 10, 7), local(2026, 10, 7, 1));
  assert.equal(overlapsRange(after, today.from, today.to), false);
});

test("端点为 null 表示该侧不限", () => {
  const item = entry("rec-1", local(2026, 10, 6, 10), local(2026, 10, 6, 11));
  assert.ok(overlapsRange(item, null, null));
  assert.ok(overlapsRange(item, local(2026, 10, 1), null));
  assert.ok(overlapsRange(item, null, local(2026, 10, 9)));
  assert.equal(overlapsRange(item, local(2026, 10, 7), null), false);
});

test("关键字匹配 id、通道与文件名，且大小写不敏感", () => {
  const items = [
    entry("rec-A", local(2026, 10, 6, 10), local(2026, 10, 6, 11), "manual", "ch-9", "/rec/a.PS"),
    entry("rec-B", local(2026, 10, 6, 12), local(2026, 10, 6, 13), "alarm", "ch-1", "/rec/b.ps"),
  ];
  assert.deepEqual(filterRecordings(items, { keyword: "rec-a" }).map((i) => i.id), ["rec-A"]);
  assert.deepEqual(filterRecordings(items, { keyword: "CH-9" }).map((i) => i.id), ["rec-A"]);
  assert.deepEqual(filterRecordings(items, { keyword: "b.ps" }).map((i) => i.id), ["rec-B"]);
  assert.equal(filterRecordings(items, { keyword: "   " }).length, 2);
});

test("类型与时间窗叠加生效", () => {
  const items = [
    entry("rec-1", local(2026, 10, 6, 10), local(2026, 10, 6, 11), "manual"),
    entry("rec-2", local(2026, 10, 6, 12), local(2026, 10, 6, 13), "alarm"),
    entry("rec-3", local(2026, 10, 4, 12), local(2026, 10, 4, 13), "alarm"),
  ];
  const from = local(2026, 10, 6);
  const to = local(2026, 10, 7);
  assert.deepEqual(
    filterRecordings(items, { kind: "alarm", from, to }).map((i) => i.id),
    ["rec-2"],
  );
  assert.deepEqual(filterRecordings(items, { kind: "all", from, to }).map((i) => i.id), ["rec-1", "rec-2"]);
});

test("时长计算对时间戳倒挂兜底为 0", () => {
  assert.equal(durationSeconds(entry("rec-1", 0, HOUR)), 3600);
  assert.equal(durationSeconds(entry("rec-1", HOUR, HOUR)), 0);
  assert.equal(durationSeconds(entry("rec-1", HOUR, 0)), 0);
  assert.equal(durationSeconds(null), 0);
});

test("时长展示超过一小时补时分秒", () => {
  assert.equal(formatDuration(0), "0:00");
  assert.equal(formatDuration(9), "0:09");
  assert.equal(formatDuration(309), "5:09");
  assert.equal(formatDuration(3909), "1:05:09");
  assert.equal(formatDuration(-5), "0:00");
});

test("自定义范围空值不限、倒序自动交换、结束时间含当分钟", () => {
  assert.deepEqual(customRange("", ""), { from: null, to: null });
  assert.deepEqual(customRange("2026-10-06T08:00", "2026-10-06T09:00"), {
    from: local(2026, 10, 6, 8),
    to: local(2026, 10, 6, 9) + 60_000,
  });
  // 用户填反了也要能用，不能筛出空列表。
  assert.deepEqual(customRange("2026-10-06T09:00", "2026-10-06T08:00"), {
    from: local(2026, 10, 6, 8),
    to: local(2026, 10, 6, 9) + 60_000,
  });
  // 非法文本当作不限，而不是变成 NaN 把列表全滤掉。
  assert.deepEqual(customRange("不是时间", ""), { from: null, to: null });
});

test("datetime-local 输入值格式化与解析互逆", () => {
  const ms = local(2026, 10, 6, 8, 5);
  assert.equal(formatLocalInput(ms), "2026-10-06T08:05");
  assert.equal(parseLocalInput("2026-10-06T08:05"), ms);
  assert.equal(parseLocalInput(""), null);
  assert.equal(parseLocalInput("  "), null);
  assert.equal(parseLocalInput(undefined), null);
  assert.equal(formatLocalInput(Number.NaN), "");
});

test("近7天窗口不会把 8 天前的录像算进来", () => {
  const now = local(2026, 10, 6, 15);
  const range = quickRange("7d", now);
  const old = entry("rec-old", local(2026, 9, 29, 10), local(2026, 9, 29, 11));
  const recent = entry("rec-new", local(2026, 10, 2, 10), local(2026, 10, 2, 11));
  assert.equal(overlapsRange(old, range.from, range.to), false);
  assert.ok(overlapsRange(recent, range.from, range.to));
  assert.ok(range.to - range.from >= 6 * DAY);
});
