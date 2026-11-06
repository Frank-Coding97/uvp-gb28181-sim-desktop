import assert from "node:assert/strict";
import test from "node:test";
import {
  MAX_NOTIFICATIONS,
  appendNotification,
  broadcastNotification,
  deviceStateNotification,
  formatRelative,
  markAllRead,
  markRead,
  platformCommandNotification,
  subscriptionNotification,
  unreadCount,
} from "./notifications.mjs";

test("新通知插到最前且默认未读", () => {
  const list = appendNotification([], { title: "设备已注册", level: "success", ts: 1 });
  assert.equal(list.length, 1);
  assert.equal(list[0].title, "设备已注册");
  assert.equal(list[0].level, "success");
  assert.equal(list[0].read, false);
  assert.equal(list[0].ts, 1);
});

test("标题为空的通知被丢弃", () => {
  const list = appendNotification([], { title: "   " });
  assert.deepEqual(list, []);
  assert.deepEqual(appendNotification([], { title: "" }), []);
  // 传 null 也不该炸，返回原列表。
  const existing = appendNotification([], { title: "a" });
  assert.equal(appendNotification(existing, null), existing);
});

test("未知级别退化为 info", () => {
  const list = appendNotification([], { title: "x", level: "fatal" });
  assert.equal(list[0].level, "info");
});

test("超出上限时丢弃最旧的", () => {
  let list = [];
  for (let i = 0; i < MAX_NOTIFICATIONS + 5; i += 1) {
    list = appendNotification(list, { title: `第 ${i} 条` });
  }
  assert.equal(list.length, MAX_NOTIFICATIONS);
  // 最新在最前，最旧的已被挤掉。
  assert.equal(list[0].title, `第 ${MAX_NOTIFICATIONS + 4} 条`);
  assert.ok(!list.some((item) => item.title === "第 0 条"));
});

test("id 单调递增且不复用下标", () => {
  let list = appendNotification([], { title: "a" });
  list = appendNotification(list, { title: "b" });
  list = appendNotification(list, { title: "c" });
  assert.deepEqual(list.map((item) => item.id), [3, 2, 1]);
});

test("追加不修改原数组", () => {
  const base = appendNotification([], { title: "a" });
  const snapshot = JSON.stringify(base);
  appendNotification(base, { title: "b" });
  assert.equal(JSON.stringify(base), snapshot);
});

test("已读与未读计数", () => {
  let list = appendNotification([], { title: "a" });
  list = appendNotification(list, { title: "b" });
  assert.equal(unreadCount(list), 2);
  list = markRead(list, list[0].id);
  assert.equal(unreadCount(list), 1);
  assert.equal(list[0].read, true);
  list = markAllRead(list);
  assert.equal(unreadCount(list), 0);
  // 全部已读后再标记不应改变引用相等性以外的内容。
  assert.deepEqual(markAllRead(list), list);
});

test("设备状态只在有意义的状态上提示", () => {
  assert.deepEqual(deviceStateNotification("Registered"), {
    level: "success",
    title: "设备已注册",
    detail: "状态：Registered",
  });
  assert.equal(deviceStateNotification("Failed").level, "error");
  assert.equal(deviceStateNotification("Disconnected").level, "warning");
  // 中间态不提示，否则每次重连都刷一条。
  assert.equal(deviceStateNotification("Registering"), null);
  assert.equal(deviceStateNotification("Unknown"), null);
});

test("订阅只在状态翻转时提示", () => {
  assert.deepEqual(subscriptionNotification(false, true, "Catalog"), {
    level: "success",
    title: "目录订阅已建立",
  });
  assert.deepEqual(subscriptionNotification(true, false, "Alarm"), {
    level: "warning",
    title: "报警订阅已退订",
  });
  // 上报计数变化会让事件重复到达，状态没变就不该再提示。
  assert.equal(subscriptionNotification(true, true, "MobilePosition"), null);
  assert.equal(subscriptionNotification(false, false, "MobilePosition"), null);
  // 未知类别退化为原样展示，不至于显示 undefined。
  assert.equal(subscriptionNotification(false, true, "Presence").title, "Presence已建立");
});

test("语音广播开始与结束都有提示", () => {
  assert.equal(broadcastNotification(true).title, "平台语音广播开始");
  assert.equal(broadcastNotification(false).title, "平台语音广播结束");
});

test("平台命令通知沿用后端摘要并按 kind 定级", () => {
  assert.deepEqual(platformCommandNotification({ kind: "control", summary: "平台控制：PTZ", ts_ms: 9 }), {
    level: "info",
    title: "平台控制：PTZ",
    detail: "control",
    ts: 9,
  });
  assert.equal(
    platformCommandNotification({ kind: "control_error", summary: "控制失败" }).level,
    "error",
  );
  // 空摘要不产生通知，避免列表出现无名行。
  assert.equal(platformCommandNotification({ kind: "x", summary: "  " }), null);
  assert.equal(platformCommandNotification(null), null);
});

test("相对时间按量级换单位", () => {
  const now = 1_700_000_000_000;
  assert.equal(formatRelative(now - 5_000, now), "刚刚");
  assert.equal(formatRelative(now - 3 * 60_000, now), "3 分钟前");
  assert.equal(formatRelative(now - 5 * 3_600_000, now), "5 小时前");
  assert.equal(formatRelative(now - 3 * 86_400_000, now), "3 天前");
  // 时钟回拨导致时间戳在未来时按"刚刚"处理，不显示负数。
  assert.equal(formatRelative(now + 10_000, now), "刚刚");
  assert.equal(formatRelative(Number.NaN, now), "");
});
