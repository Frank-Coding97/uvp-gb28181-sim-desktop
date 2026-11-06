// 录像计划的纯函数实现（与 crates/record-plan 同语义）。
//
// 放在 .mjs 而不是 .vue 里，是为了让 node:test 能**真正执行**这些函数：
// 涂格子、擦格子、合并相邻段这些操作一旦和后端不一致，用户看到的计划就和
// 实际录像对不上，靠肉眼看代码是发现不了的。
//
// 语义：左闭右开 [start_sec, end_sec)，相邻段自动合并（连点两格 = 一段）。

export const SECS_PER_DAY = 86400;
export const BLOCK_SECS = 1800;
export const BLOCKS_PER_DAY = 48;
export const MAX_SEGMENTS_PER_DAY = 8;

/** 空计划（7 天全空，默认关闭）。 */
export function emptyPlan() {
  return {
    enabled: false,
    week: { days: Array.from({ length: 7 }, () => ({ segments: [] })) },
  };
}

/** 排序 + 裁剪到当天 + 合并重叠与相邻段。 */
export function normalizeDay(segments) {
  const cleaned = segments
    .map((segment) => ({
      start_sec: Math.max(0, Math.min(segment.start_sec, SECS_PER_DAY)),
      end_sec: Math.max(0, Math.min(segment.end_sec, SECS_PER_DAY)),
    }))
    .filter((segment) => segment.end_sec > segment.start_sec)
    .sort((a, b) => a.start_sec - b.start_sec || a.end_sec - b.end_sec);
  const merged = [];
  for (const segment of cleaned) {
    const last = merged[merged.length - 1];
    // <= 而不是 <：相邻段（8:00-8:30 + 8:30-9:00）也要合并。
    if (last && segment.start_sec <= last.end_sec) {
      last.end_sec = Math.max(last.end_sec, segment.end_sec);
    } else {
      merged.push({ ...segment });
    }
  }
  return merged;
}

/** 加入一段并归一化。 */
export function addSegment(segments, segment) {
  return normalizeDay([...segments, segment]);
}

/** 减去一段（可能把一段切成两段）。 */
export function subtractRange(segments, startSec, endSec) {
  if (endSec <= startSec) return segments;
  const next = [];
  for (const segment of segments) {
    if (segment.end_sec <= startSec || segment.start_sec >= endSec) {
      next.push({ ...segment });
      continue;
    }
    if (segment.start_sec < startSec) {
      next.push({ start_sec: segment.start_sec, end_sec: startSec });
    }
    if (segment.end_sec > endSec) {
      next.push({ start_sec: endSec, end_sec: segment.end_sec });
    }
  }
  return normalizeDay(next);
}

/** 整段是否都被覆盖（决定格子该涂上还是擦掉）。 */
export function coversWhole(segments, startSec, endSec) {
  let cursor = startSec;
  while (cursor < endSec) {
    const covering = segments.filter(
      (segment) => segment.start_sec <= cursor && cursor < segment.end_sec,
    );
    if (!covering.length) return false;
    const reach = Math.max(...covering.map((segment) => segment.end_sec));
    cursor = Math.min(reach, endSec);
  }
  return true;
}

/** 切换某天第 block 格（0..47）。返回新的周计划结构。 */
export function toggleBlock(plan, weekday, block) {
  if (block < 0 || block >= BLOCKS_PER_DAY) return plan;
  const days = plan.week.days.map((day, index) => {
    if (index !== weekday) return day;
    const start = block * BLOCK_SECS;
    const end = start + BLOCK_SECS;
    const segments = coversWhole(day.segments, start, end)
      ? subtractRange(day.segments, start, end)
      : addSegment(day.segments, { start_sec: start, end_sec: end });
    return { segments };
  });
  return { ...plan, week: { days } };
}

/** 某天 48 格的覆盖状态。 */
export function dayBlocks(segments) {
  return Array.from({ length: BLOCKS_PER_DAY }, (_, block) => {
    const start = block * BLOCK_SECS;
    return coversWhole(segments, start, start + BLOCK_SECS);
  });
}

/** 此刻（周几 + 当天秒）是否应录像。 */
export function shouldRecord(plan, weekday, sec) {
  if (!plan.enabled) return false;
  const day = plan.week.days[weekday];
  if (!day) return false;
  return day.segments.some(
    (segment) => segment.start_sec <= sec && sec < segment.end_sec,
  );
}

/** 补齐 7 天并归一化每日段落（后端会再归一化一次，这里保证界面自洽）。 */
export function normalizePlan(plan) {
  const days = Array.from({ length: 7 }, (_, index) => ({
    segments: normalizeDay(plan?.week?.days?.[index]?.segments ?? []),
  }));
  return { enabled: Boolean(plan?.enabled), week: { days } };
}

/** `HH:MM:SS`；86400 显示为 24:00:00。 */
export function formatHms(sec) {
  const value = Math.max(0, Math.min(Math.round(sec), SECS_PER_DAY));
  const pad = (input) => String(input).padStart(2, "0");
  return `${pad(Math.floor(value / 3600))}:${pad(Math.floor((value % 3600) / 60))}:${pad(value % 60)}`;
}

/** 解析 `H:MM`、`HH:MM:SS`、`24:00`；非法返回 null。 */
export function parseHms(text) {
  const parts = String(text).trim().split(":");
  if (parts.length < 2 || parts.length > 3) return null;
  const [hour, minute, second = "0"] = parts;
  if (!/^\d+$/.test(hour) || !/^\d+$/.test(minute) || !/^\d+$/.test(second)) return null;
  const h = Number(hour);
  const m = Number(minute);
  const s = Number(second);
  if (m > 59 || s > 59) return null;
  const total = h * 3600 + m * 60 + s;
  return total <= SECS_PER_DAY ? total : null;
}

export const WEEKDAY_LABELS = ["一", "二", "三", "四", "五", "六", "日"];

/** 每 2 小时一个刻度标签，用于网格上方的时间轴。 */
export function hourTicks() {
  return Array.from({ length: 12 }, (_, index) => `${String(index * 2).padStart(2, "0")}:00`);
}
