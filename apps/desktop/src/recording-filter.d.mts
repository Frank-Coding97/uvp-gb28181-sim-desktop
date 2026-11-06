export interface QuickRangeOption {
  label: string;
  value: "all" | "today" | "7d" | "30d" | "custom";
}

export interface TimeRange {
  from: number;
  to: number;
}

export interface RecordingLike {
  id: string;
  channel_id: string;
  path: string;
  kind: string;
  start_time_ms: number;
  end_time_ms: number;
}

export interface RecordingFilterInput {
  keyword?: string;
  kind?: string;
  from?: number | null;
  to?: number | null;
}

export const QUICK_RANGES: QuickRangeOption[];

export function startOfDayMs(ms: number): number;
export function quickRange(key: string, nowMs: number): TimeRange | null;
export function formatLocalInput(ms: number): string;
export function parseLocalInput(text: string): number | null;
export function durationSeconds(entry: RecordingLike | null): number;
export function formatDuration(secs: number): string;
export function overlapsRange(entry: RecordingLike | null, from: number | null, to: number | null): boolean;
/** 保留入参的具体类型，页面拿到的仍是完整录像条目而不是被擦成公共字段。 */
export function filterRecordings<T extends RecordingLike>(entries: T[], input?: RecordingFilterInput): T[];
export function customRange(fromText: string, toText: string): { from: number | null; to: number | null };
