/**
 * 录像列表的时间范围筛选（与 Rust 侧无关的纯函数，供页面与单测共用）。
 *
 * 关键语义：**按时间窗重叠筛选，而不是按开始时间落在窗内**。
 * 一段 20:00-08:00+1 的夜间录像在「今天」筛选里必须出现 —— 用户找的是
 * "今天这段画面"，而不是"今天才开始录的文件"。按开始时间判会把跨夜录像藏起来。
 */

/** 快速时间范围选项。`custom` 表示由两个日期输入框自己填。 */
export const QUICK_RANGES = [
  { label: "全部时间", value: "all" },
  { label: "今天", value: "today" },
  { label: "近 7 天", value: "7d" },
  { label: "近 30 天", value: "30d" },
  { label: "自定义", value: "custom" },
];

const DAY_MS = 86_400_000;

/** 本地时区当天零点。用本地时间是因为列表展示的就是本地时间。 */
export function startOfDayMs(ms) {
  const date = new Date(ms);
  date.setHours(0, 0, 0, 0);
  return date.getTime();
}

/**
 * 把快速范围换算成毫秒区间（左闭右开）。
 * `all` / `custom` 返回 null，表示不按时间过滤（自定义由输入框决定）。
 */
export function quickRange(key, nowMs) {
  if (key === "today") {
    const from = startOfDayMs(nowMs);
    return { from, to: from + DAY_MS };
  }
  if (key === "7d" || key === "30d") {
    const days = key === "7d" ? 7 : 30;
    // 「近 N 天」含今天，所以从 N-1 天前的零点算起。
    const from = startOfDayMs(nowMs) - (days - 1) * DAY_MS;
    return { from, to: startOfDayMs(nowMs) + DAY_MS };
  }
  return null;
}

function pad(value) {
  return String(value).padStart(2, "0");
}

/** `Date` → `<input type="datetime-local">` 需要的 `YYYY-MM-DDTHH:mm`。 */
export function formatLocalInput(ms) {
  if (!Number.isFinite(ms)) return "";
  const date = new Date(ms);
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** 反向解析。空串/非法值返回 null（表示该端点不限）。 */
export function parseLocalInput(text) {
  if (typeof text !== "string" || !text.trim()) return null;
  const parsed = new Date(text).getTime();
  return Number.isFinite(parsed) ? parsed : null;
}

/** 录像时长（秒），向下取整；索引里时间戳倒挂时返回 0 而不是负数。 */
export function durationSeconds(entry) {
  const span = (entry?.end_time_ms ?? 0) - (entry?.start_time_ms ?? 0);
  return span > 0 ? Math.floor(span / 1000) : 0;
}

/** 时长展示：`1:05:09` / `5:09`。 */
export function formatDuration(secs) {
  const total = Math.max(0, Math.floor(secs ?? 0));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const rest = total % 60;
  if (hours > 0) return `${hours}:${pad(minutes)}:${pad(rest)}`;
  return `${minutes}:${pad(rest)}`;
}

/**
 * 时间窗判定：录像与 `[from, to)` 有任何交集就算命中。
 * 任一端点为 null 表示该侧不限。
 */
export function overlapsRange(entry, from, to) {
  const start = entry?.start_time_ms ?? 0;
  const end = entry?.end_time_ms ?? start;
  if (from !== null && end <= from) return false;
  if (to !== null && start >= to) return false;
  return true;
}

/**
 * 组合筛选：类型 + 关键字 + 时间窗。
 * 关键字同时匹配 ID / 通道 / 文件名，与旧行为一致（用户可能记得其中任一）。
 */
export function filterRecordings(entries, { keyword = "", kind = "all", from = null, to = null } = {}) {
  const query = String(keyword ?? "").trim().toLowerCase();
  return (entries ?? []).filter((entry) => {
    if (kind !== "all" && entry.kind !== kind) return false;
    if (!overlapsRange(entry, from, to)) return false;
    if (!query) return true;
    return `${entry.id} ${entry.channel_id} ${entry.path}`.toLowerCase().includes(query);
  });
}

/** 自定义范围的两个端点整理成 `{from, to}`：倒序时自动交换，空值表示不限。 */
export function customRange(fromText, toText) {
  let from = parseLocalInput(fromText);
  let to = parseLocalInput(toText);
  if (from !== null && to !== null && from > to) [from, to] = [to, from];
  // 结束时间精确到分钟，用户选 12:00 通常期望包含 12:00 这一分钟。
  if (to !== null) to += 60_000;
  return { from, to };
}
