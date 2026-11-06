export interface NotificationItem {
  id: number;
  level: "info" | "success" | "warning" | "error";
  title: string;
  detail: string | null;
  ts: number;
  read: boolean;
}

export interface NotificationInput {
  level?: NotificationItem["level"];
  title: string;
  detail?: string | null;
  ts?: number;
}

export const MAX_NOTIFICATIONS: number;
export const NOTIFICATION_LEVELS: NotificationItem["level"][];

export function appendNotification(
  list: NotificationItem[],
  input: NotificationInput,
  max?: number,
): NotificationItem[];
export function markAllRead(list: NotificationItem[]): NotificationItem[];
export function markRead(list: NotificationItem[], id: number): NotificationItem[];
export function unreadCount(list: NotificationItem[]): number;
export function deviceStateNotification(state: string): { level: NotificationItem["level"]; title: string; detail: string } | null;
export function subscriptionNotification(
  previousActive: boolean,
  nextActive: boolean,
  kind: string,
): { level: NotificationItem["level"]; title: string } | null;
export function broadcastNotification(active: boolean): { level: NotificationItem["level"]; title: string };
export function platformCommandNotification(payload: {
  kind?: string;
  summary?: string;
  ts_ms?: number;
}): { level: NotificationItem["level"]; title: string; detail: string | null; ts?: number } | null;
export function formatRelative(tsMs: number, nowMs?: number): string;
