import assert from "node:assert/strict";
import test from "node:test";
import {
  addSegment,
  BLOCKS_PER_DAY,
  coversWhole,
  dayBlocks,
  emptyPlan,
  formatHms,
  normalizeDay,
  normalizePlan,
  parseHms,
  SECS_PER_DAY,
  shouldRecord,
  subtractRange,
  toggleBlock,
} from "./record-plan.mjs";

test("相邻段自动合并（连点两格 = 一段）", () => {
  let segments = addSegment([], { start_sec: 8 * 3600, end_sec: 9 * 3600 });
  segments = addSegment(segments, { start_sec: 9 * 3600, end_sec: 10 * 3600 });
  assert.deepEqual(segments, [{ start_sec: 8 * 3600, end_sec: 10 * 3600 }]);
});

test("重叠段合并并取外层边界", () => {
  let segments = addSegment([], { start_sec: 8 * 3600, end_sec: 10 * 3600 });
  segments = addSegment(segments, { start_sec: 9 * 3600, end_sec: 11 * 3600 });
  assert.deepEqual(segments, [{ start_sec: 8 * 3600, end_sec: 11 * 3600 }]);
});

test("减掉中间一段会切成两段", () => {
  const segments = subtractRange(
    [{ start_sec: 8 * 3600, end_sec: 12 * 3600 }],
    9 * 3600,
    10 * 3600,
  );
  assert.deepEqual(segments, [
    { start_sec: 8 * 3600, end_sec: 9 * 3600 },
    { start_sec: 10 * 3600, end_sec: 12 * 3600 },
  ]);
});

test("涂格与擦格，且左闭右开", () => {
  let plan = toggleBlock(emptyPlan(), 0, 16); // 周一 08:00-08:30
  assert.equal(plan.week.days[0].segments.length, 1);
  assert.equal(shouldRecord({ ...plan, enabled: true }, 0, 8 * 3600), true);
  assert.equal(
    shouldRecord({ ...plan, enabled: true }, 0, 8 * 3600 + 1800),
    false,
    "8:30 属于下一格，不应命中",
  );

  plan = toggleBlock(plan, 0, 17); // 合并成 08:00-09:00
  assert.deepEqual(plan.week.days[0].segments, [{ start_sec: 8 * 3600, end_sec: 9 * 3600 }]);

  plan = toggleBlock(plan, 0, 16); // 擦掉 08:00-08:30
  assert.deepEqual(plan.week.days[0].segments, [
    { start_sec: 8 * 3600 + 1800, end_sec: 9 * 3600 },
  ]);
});

test("部分覆盖的格子不显示为满格", () => {
  const segments = [{ start_sec: 8 * 3600 + 600, end_sec: 8 * 3600 + 2400 }];
  assert.equal(coversWhole(segments, 8 * 3600, 8 * 3600 + 1800), false);
  const blocks = dayBlocks(segments);
  assert.equal(blocks.length, BLOCKS_PER_DAY);
  assert.equal(blocks[16], false);
  assert.equal(blocks.filter(Boolean).length, 0);
});

test("总开关关闭时 never 录像", () => {
  const plan = normalizePlan({ enabled: false, week: { days: [] } });
  plan.week.days[0].segments = [{ start_sec: 0, end_sec: SECS_PER_DAY }];
  assert.equal(shouldRecord({ ...plan, enabled: false }, 0, 3600), false);
  assert.equal(shouldRecord({ ...plan, enabled: true }, 0, 3600), true);
});

test("normalizePlan 补齐 7 天并裁掉越界段", () => {
  const plan = normalizePlan({
    enabled: true,
    week: { days: [{ segments: [{ start_sec: 3600, end_sec: SECS_PER_DAY + 600 }] }] },
  });
  assert.equal(plan.week.days.length, 7);
  assert.deepEqual(plan.week.days[0].segments, [{ start_sec: 3600, end_sec: SECS_PER_DAY }]);
  assert.deepEqual(plan.week.days[6].segments, []);
});

test("空计划默认关闭，避免升级后突然自动录像", () => {
  assert.equal(emptyPlan().enabled, false);
  assert.equal(emptyPlan().week.days.length, 7);
});

test("时间格式往返与边界", () => {
  assert.equal(parseHms("08:30"), 8 * 3600 + 1800);
  assert.equal(parseHms("8:30:15"), 8 * 3600 + 1800 + 15);
  assert.equal(parseHms("24:00:00"), SECS_PER_DAY);
  assert.equal(parseHms("24:00:01"), null);
  assert.equal(parseHms("08:60"), null);
  assert.equal(parseHms("abc"), null);
  assert.equal(formatHms(8 * 3600 + 1800), "08:30:00");
  assert.equal(formatHms(SECS_PER_DAY), "24:00:00");
});

test("归一化不会把非相邻段合并", () => {
  const segments = normalizeDay([
    { start_sec: 8 * 3600, end_sec: 9 * 3600 },
    { start_sec: 10 * 3600, end_sec: 11 * 3600 },
  ]);
  assert.equal(segments.length, 2);
});
