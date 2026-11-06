// 通知中心运行时：把引擎事件折算成「用户需要知道的事」，供顶栏铃铛与抽屉消费。
//
// 纯逻辑在 notifications.mjs（可单测），这里只做三件事：
// 1) 持有响应式列表；2) 订阅引擎事件；3) 把事件过滤成通知。
// 位置上报这类周期性事件**不进通知**，否则列表每隔几秒就被刷满。
import { computed, reactive } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  appendNotification,
  broadcastNotification,
  deviceStateNotification,
  markAllRead,
  markRead,
  platformCommandNotification,
  subscriptionNotification,
  unreadCount,
} from "./notifications.mjs";
import type { NotificationInput, NotificationItem } from "./notifications.mjs";

const state = reactive<{ items: NotificationItem[] }>({ items: [] });

/** 当前通知列表（最新在前）。 */
export const notificationItems = computed(() => state.items);
export const notificationUnread = computed(() => unreadCount(state.items));
export const notificationCount = computed(() => state.items.length);

export function pushNotification(input: NotificationInput) {
  state.items = appendNotification(state.items, input);
}

export function markNotificationsRead() {
  state.items = markAllRead(state.items);
}

export function markNotificationRead(id: number) {
  state.items = markRead(state.items, id);
}

export function clearNotifications() {
  state.items = [];
}

/** 订阅状态按类别记上一次的 active：事件在每次上报后都会到达，只有翻转才提示。 */
const subscriptionActive = new Map<string, boolean>();
let listeners: UnlistenFn[] = [];

function isTauri() {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/**
 * 启动事件订阅。返回停止函数（重复调用是安全的）。
 *
 * 只订阅"事件驱动"的通知源：设备状态、平台命令、订阅翻转、语音广播、采集错误。
 * 录像/抓拍等由各自页面在操作成功时就地提示，不再二次进通知中心。
 */
export async function startNotificationCenter(): Promise<() => void> {
  if (!isTauri() || listeners.length > 0) return stopNotificationCenter;
  const collected: UnlistenFn[] = [];

  collected.push(await listen<string>("device_state", (event) => {
    const mapped = deviceStateNotification(event.payload);
    if (mapped) pushNotification(mapped);
  }));

  collected.push(await listen<{ code?: string; message?: string }>("device_error", (event) => {
    const message = event.payload?.message?.trim();
    if (!message) return;
    pushNotification({
      level: "error",
      title: "设备错误",
      detail: event.payload?.code ? `${event.payload.code} · ${message}` : message,
    });
  }));

  collected.push(await listen<{ kind?: string; summary?: string; ts_ms?: number }>("platform_command", (event) => {
    const mapped = platformCommandNotification(event.payload);
    if (mapped) pushNotification(mapped);
  }));

  collected.push(await listen<{ kind: string; active: boolean }>("subscription_state", (event) => {
    const { kind, active } = event.payload ?? {};
    if (!kind) return;
    const previous = subscriptionActive.get(kind) ?? false;
    subscriptionActive.set(kind, active);
    const mapped = subscriptionNotification(previous, active, kind);
    if (mapped) pushNotification(mapped);
  }));

  collected.push(await listen<{ active: boolean }>("broadcast_state", (event) => {
    pushNotification(broadcastNotification(Boolean(event.payload?.active)));
  }));

  listeners = collected;
  return stopNotificationCenter;
}

export function stopNotificationCenter() {
  for (const unlisten of listeners) unlisten();
  listeners = [];
}

/** 供测试/调试：重置内部状态（不触碰已注册的监听）。 */
export function resetNotificationCenterState() {
  state.items = [];
  subscriptionActive.clear();
}
