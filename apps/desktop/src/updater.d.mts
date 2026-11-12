/** 在线升级纯逻辑的类型声明（实现见 updater.mjs）。 */
export interface UpdateNoticeInput {
  level: "info" | "success" | "warning" | "error";
  title: string;
  detail?: string;
}

export declare function normalizeVersion(version: unknown): string;
export declare function formatBytes(bytes: unknown): string;
export declare function progressPercent(downloaded: unknown, total: unknown): number | null;
export declare function clampedPercent(downloaded: unknown, total: unknown): number;
export declare function summarizeNotes(notes: unknown, max?: number): string;
export declare function updateNotice(version: unknown, notes?: unknown): UpdateNoticeInput | null;
export declare function shouldNotifyVersion(lastNotified: unknown, version: unknown): boolean;
