import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const [app, bell, center, pure] = await Promise.all([
  readFile(new URL("../App.vue", import.meta.url), "utf8"),
  readFile(new URL("../components/NotificationBell.vue", import.meta.url), "utf8"),
  readFile(new URL("../notification-center.ts", import.meta.url), "utf8"),
  readFile(new URL("../notifications.mjs", import.meta.url), "utf8"),
]);

test("顶栏挂载通知铃铛，并在壳层启动/停止订阅", () => {
  assert.match(app, /import NotificationBell from "\.\/components\/NotificationBell\.vue";/);
  assert.match(app, /<NotificationBell \/>/);
  assert.match(app, /await startNotificationCenter\(\);/);
  assert.match(app, /stopNotificationCenter\(\);/);
});

test("铃铛有未读角标，打开即置读，可清空", () => {
  assert.match(bell, /<n-badge/);
  assert.match(bell, /notificationUnread/);
  assert.match(bell, /if \(open\.value && hasUnread\.value\) markNotificationsRead\(\);/);
  assert.match(bell, /clearNotifications/);
  assert.match(bell, /markNotificationRead\(item\.id\)/);
});

test("通知只订阅事件驱动源，不订阅高频流水", () => {
  for (const event of ["device_state", "device_error", "platform_command", "subscription_state", "broadcast_state"]) {
    assert.ok(center.includes(`"${event}"`), `缺少订阅：${event}`);
  }
  // sip_trace / metrics_tick 是高频流水，进通知会被刷爆。
  assert.doesNotMatch(center, /listen<[^>]*>\("sip_trace"/);
  assert.doesNotMatch(center, /listen<[^>]*>\("metrics_tick"/);
});

test("订阅通知只在状态翻转时产生，不随每次上报刷屏", () => {
  assert.match(center, /const previous = subscriptionActive\.get\(kind\) \?\? false;/);
  assert.match(center, /const mapped = subscriptionNotification\(previous, active, kind\);/);
  assert.match(pure, /if \(previousActive === nextActive\) return null;/);
});

test("最多保留 50 条且最新在最前", () => {
  assert.match(pure, /export const MAX_NOTIFICATIONS = 50;/);
  assert.match(pure, /return \[item, \.\.\.previous\]\.slice\(0, Math\.max\(1, max\)\);/);
});

test("通知列表不可原地改坏（返回新数组）", () => {
  assert.match(pure, /export function appendNotification\(list, input, max = MAX_NOTIFICATIONS\)/);
  // 空标题直接丢弃：列表里出现空行比不出现更糟。
  assert.match(pure, /if \(!title\) return list;/);
  // id 单调递增，不用下标（头部插入会让下标整体偏移）。
  assert.match(pure, /const nextId = previous\.reduce/);
});

test("设备中间态不产生通知", () => {
  assert.match(pure, /const DEVICE_STATE_MESSAGES = \{/);
  assert.doesNotMatch(pure, /Registering: \{/, "Registering 属于中间态，不该提示");
});

test("相对时间不出现负数", () => {
  assert.match(pure, /const delta = Math\.max\(0, \(nowMs \?\? Date\.now\(\)\) - tsMs\);/);
});

test("通知中心对外暴露的接口齐全", () => {
  for (const api of [
    "export const notificationItems",
    "export const notificationUnread",
    "export function pushNotification",
    "export function markNotificationsRead",
    "export function clearNotifications",
    "export async function startNotificationCenter",
  ]) {
    assert.ok(center.includes(api), `缺少接口：${api}`);
  }
});
