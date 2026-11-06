/**
 * 通知中心的纯逻辑（与 Vue/事件无关，便于单测）。
 *
 * 通知不是"日志"：日志页存的是完整 IPC/信令流水，通知只保留**用户需要知道的那几件事**
 * ——平台下发命令、订阅建立/退订、设备状态翻转、语音广播、采集错误。
 * 位置上报这类周期性事件不进通知，否则列表几秒就被刷满。
 */

/** 保留条数上限。超过就丢最旧的，避免长时间运行后内存无限增长。 */
export const MAX_NOTIFICATIONS = 50;

/** 通知级别。`error` 在界面上带红色标记，`warning` 橙色，其余中性。 */
export const NOTIFICATION_LEVELS = ["info", "success", "warning", "error"];

const KIND_LABELS = {
  MobilePosition: "位置订阅",
  Catalog: "目录订阅",
  Alarm: "报警订阅",
  PTZPosition: "PTZ 精准位置订阅",
};

const DEVICE_STATE_MESSAGES = {
  Registered: { level: "success", title: "设备已注册" },
  Failed: { level: "error", title: "设备注册失败" },
  Disconnected: { level: "warning", title: "设备已断开" },
  InCall: { level: "info", title: "平台开始点播" },
};

function normalizeLevel(level) {
  return NOTIFICATION_LEVELS.includes(level) ? level : "info";
}

/**
 * 追加一条通知，返回**新数组**（不原地改，方便 Vue 触发更新与单测断言）。
 *
 * 标题为空视为无效输入直接丢弃：通知列表里出现空行比不出现更糟。
 * `id` 单调递增而非复用下标——列表头部插入后下标会整体偏移，
 * 用下标当 key 会让抽屉里的行错位。
 */
export function appendNotification(list, input, max = MAX_NOTIFICATIONS) {
  const title = String(input?.title ?? "").trim();
  if (!title) return list;
  const previous = list ?? [];
  const nextId = previous.reduce((acc, item) => Math.max(acc, item.id ?? 0), 0) + 1;
  const item = {
    id: nextId,
    level: normalizeLevel(input.level),
    title,
    detail: input.detail ? String(input.detail) : null,
    ts: Number.isFinite(input.ts) ? input.ts : Date.now(),
    read: false,
  };
  return [item, ...previous].slice(0, Math.max(1, max));
}

export function markAllRead(list) {
  return (list ?? []).map((item) => (item.read ? item : { ...item, read: true }));
}

export function markRead(list, id) {
  return (list ?? []).map((item) => (item.id === id && !item.read ? { ...item, read: true } : item));
}

export function unreadCount(list) {
  return (list ?? []).filter((item) => !item.read).length;
}

/** 设备状态翻转 → 通知。`Registering` 这类中间态不提示，否则每次重连都刷一条。 */
export function deviceStateNotification(state) {
  const mapped = DEVICE_STATE_MESSAGES[state];
  return mapped ? { ...mapped, detail: `状态：${state}` } : null;
}

/**
 * 订阅**状态翻转**才提示，不代表每次上报都提示。
 * 参数是前后两个 `active` 值：相同则返回 null。
 */
export function subscriptionNotification(previousActive, nextActive, kind) {
  if (previousActive === nextActive) return null;
  const label = KIND_LABELS[kind] ?? kind ?? "订阅";
  return nextActive
    ? { level: "success", title: `${label}已建立` }
    : { level: "warning", title: `${label}已退订` };
}

export function broadcastNotification(active) {
  return active
    ? { level: "info", title: "平台语音广播开始" }
    : { level: "info", title: "平台语音广播结束" };
}

/** 平台命令事件已经是人话（summary），只需按 kind 定级。 */
export function platformCommandNotification(payload) {
  const summary = String(payload?.summary ?? "").trim();
  if (!summary) return null;
  const kind = String(payload?.kind ?? "");
  const level = /error|fail|reject/i.test(kind) ? "error" : "info";
  return { level, title: summary, detail: kind || null, ts: payload?.ts_ms };
}

/** 相对时间：通知列表靠它表达"刚才/3 分钟前"，避免一屏全是时分秒。 */
export function formatRelative(tsMs, nowMs) {
  if (!Number.isFinite(tsMs)) return "";
  const delta = Math.max(0, (nowMs ?? Date.now()) - tsMs);
  if (delta < 60_000) return "刚刚";
  const minutes = Math.floor(delta / 60_000);
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  return `${Math.floor(hours / 24)} 天前`;
}
