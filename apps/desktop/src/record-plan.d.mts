export interface PlanSegment {
  start_sec: number;
  end_sec: number;
}

export interface DayPlan {
  segments: PlanSegment[];
}

export interface WeekPlan {
  days: DayPlan[];
}

export interface RecordPlanSettings {
  enabled: boolean;
  week: WeekPlan;
}

export const SECS_PER_DAY: number;
export const BLOCK_SECS: number;
export const BLOCKS_PER_DAY: number;
export const MAX_SEGMENTS_PER_DAY: number;
export const WEEKDAY_LABELS: string[];

export function emptyPlan(): RecordPlanSettings;
export function normalizeDay(segments: PlanSegment[]): PlanSegment[];
export function addSegment(segments: PlanSegment[], segment: PlanSegment): PlanSegment[];
export function subtractRange(segments: PlanSegment[], startSec: number, endSec: number): PlanSegment[];
export function coversWhole(segments: PlanSegment[], startSec: number, endSec: number): boolean;
export function toggleBlock(plan: RecordPlanSettings, weekday: number, block: number): RecordPlanSettings;
export function dayBlocks(segments: PlanSegment[]): boolean[];
export function shouldRecord(plan: RecordPlanSettings, weekday: number, sec: number): boolean;
export function normalizePlan(plan: RecordPlanSettings | null | undefined): RecordPlanSettings;
export function formatHms(sec: number): string;
export function parseHms(text: string): number | null;
export function hourTicks(): string[];
