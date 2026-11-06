<script setup lang="ts">
import { computed, onActivated, ref } from "vue";
import { NButton, NSwitch, NTag, useMessage } from "naive-ui";
import { getRecordPlan, saveRecordPlan, type RecordPlanState } from "../platform";
import {
  BLOCKS_PER_DAY,
  WEEKDAY_LABELS,
  addSegment,
  dayBlocks,
  emptyPlan,
  formatHms,
  hourTicks,
  normalizeDay,
  normalizePlan,
  parseHms,
  toggleBlock,
  type RecordPlanSettings,
} from "../record-plan.mjs";

const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const message = useMessage();
const state = ref<RecordPlanState | null>(null);
const draft = ref<RecordPlanSettings>(emptyPlan());
const activeDay = ref(0);
const saving = ref(false);
const loading = ref(false);
const ticks = hourTicks();

const phaseText = computed(() => {
  const phase = state.value?.recording?.phase ?? "idle";
  return ({
    idle: "未录像",
    starting: "启动中",
    recording: "录像中",
    finalizing: "收尾中",
    failed: "失败",
  } as Record<string, string>)[phase] ?? phase;
});

const todayBlocks = computed(() => {
  const weekday = state.value?.weekday ?? 0;
  return dayBlocks(draft.value.week.days[weekday]?.segments ?? []);
});

const activeBlocks = computed(() => dayBlocks(draft.value.week.days[activeDay.value]?.segments ?? []));
const activeSegments = computed(() => draft.value.week.days[activeDay.value]?.segments ?? []);
const dirty = computed(() => {
  if (!state.value) return false;
  return JSON.stringify(normalizePlan(state.value.plan)) !== JSON.stringify(normalizePlan(draft.value));
});
const totalSegments = computed(() =>
  draft.value.week.days.reduce((sum, day) => sum + day.segments.length, 0),
);

function blockLabel(block: number) {
  return `${formatHms(block * 1800).slice(0, 5)}`;
}

function toggle(block: number) {
  draft.value = toggleBlock(draft.value, activeDay.value, block);
}

function addBlock() {
  // 默认加一段 08:00-09:00，用户随后可改到秒级。
  const segments = draft.value.week.days[activeDay.value]?.segments ?? [];
  const days = draft.value.week.days.map((day, index) =>
    index === activeDay.value
      ? { segments: addSegment(segments, { start_sec: 8 * 3600, end_sec: 9 * 3600 }) }
      : day,
  );
  draft.value = { ...draft.value, week: { days } };
}

function removeSegment(index: number) {
  const days = draft.value.week.days.map((day, dayIndex) => {
    if (dayIndex !== activeDay.value) return day;
    return { segments: day.segments.filter((_, segIndex) => segIndex !== index) };
  });
  draft.value = { ...draft.value, week: { days } };
}

/** 手工改时段起止（支持 HH:MM:SS）。非法或倒流的时间直接回退，不让脏值进草稿。 */
function editSegment(index: number, field: "start_sec" | "end_sec", text: string) {
  const parsed = parseHms(text);
  if (parsed === null) {
    message.warning("时间格式应为 HH:MM 或 HH:MM:SS");
    return;
  }
  const day = draft.value.week.days[activeDay.value];
  const edited = { ...day.segments[index], [field]: parsed };
  if (edited.end_sec <= edited.start_sec) {
    message.warning("结束时间必须晚于开始时间");
    return;
  }
  const segments = normalizeDay(
    day.segments.map((segment, segIndex) => (segIndex === index ? edited : segment)),
  );
  const days = draft.value.week.days.map((item, dayIndex) =>
    dayIndex === activeDay.value ? { segments } : item,
  );
  draft.value = { ...draft.value, week: { days } };
}

function clearDay() {
  const days = draft.value.week.days.map((day, index) =>
    index === activeDay.value ? { segments: [] } : day,
  );
  draft.value = { ...draft.value, week: { days } };
}

function copyToAllDays() {
  const source = draft.value.week.days[activeDay.value]?.segments ?? [];
  const days = draft.value.week.days.map(() => ({
    segments: source.map((segment) => ({ ...segment })),
  }));
  draft.value = { ...draft.value, week: { days } };
  message.success(`已把周${WEEKDAY_LABELS[activeDay.value]}的计划复制到全周`);
}

async function load() {
  if (!isTauri) return;
  loading.value = true;
  try {
    const next = await getRecordPlan();
    state.value = next;
    draft.value = normalizePlan(next.plan);
    activeDay.value = next.weekday;
  } catch (error) {
    message.error(`读取录像计划失败：${String(error)}`);
  } finally {
    loading.value = false;
  }
}

async function save() {
  if (!state.value) return;
  saving.value = true;
  try {
    const saved = await saveRecordPlan(normalizePlan(draft.value), state.value.config_revision);
    state.value = { ...state.value, plan: saved.record_plan ?? draft.value, config_revision: saved.config_revision };
    draft.value = normalizePlan(saved.record_plan ?? draft.value);
    message.success("录像计划已保存，运行中的设备立即生效");
  } catch (error) {
    message.error(`保存失败：${String(error)}`);
  } finally {
    saving.value = false;
  }
}

function reset() {
  draft.value = normalizePlan(state.value?.plan);
}

onActivated(() => { void load(); });
</script>

<template>
  <div class="page">
    <div class="page-header">
      <div>
        <div class="page-title">录像计划</div>
        <div class="page-sub">按周时间表自动起停本地录像；30 分钟格子快速涂选，时段可精确到秒</div>
      </div>
      <div class="header-actions">
        <span class="stat">{{ totalSegments }} 段 · 周{{ state?.weekday_label ?? "-" }} {{ state?.secs_of_day ? formatHms(state.secs_of_day) : "" }}</span>
        <n-button size="small" :loading="loading" @click="load">刷新</n-button>
      </div>
    </div>

    <div class="layout">
      <section class="glass-card panel">
        <div class="toolbar">
          <label class="switch">
            <n-switch v-model:value="draft.enabled" size="small" />
            <span>启用定时录像</span>
          </label>
          <div class="days">
            <button
              v-for="(label, index) in WEEKDAY_LABELS"
              :key="label"
              class="day"
              :class="{ on: activeDay === index, today: state?.weekday === index }"
              @click="activeDay = index"
            >周{{ label }}</button>
          </div>
        </div>

        <div class="axis">
          <span v-for="tick in ticks" :key="tick">{{ tick }}</span>
        </div>
        <div class="grid" :aria-label="`周${WEEKDAY_LABELS[activeDay]} 24 小时计划`">
          <button
            v-for="block in BLOCKS_PER_DAY"
            :key="block"
            class="cell"
            :class="{
              on: activeBlocks[block - 1],
              now: state?.weekday === activeDay && Math.floor((state?.secs_of_day ?? -1) / 1800) === block - 1,
              half: block % 2 === 0,
            }"
            :title="`${blockLabel(block - 1)} - ${blockLabel(block)}`"
            @click="toggle(block - 1)"
          />
        </div>

        <div class="segment-head">
          <span class="label">周{{ WEEKDAY_LABELS[activeDay] }} 的时段（{{ activeSegments.length }} 段，最多 8 段）</span>
          <div class="segment-actions">
            <n-button size="tiny" secondary @click="addBlock">添加时段</n-button>
            <n-button size="tiny" quaternary :disabled="!activeSegments.length" @click="clearDay">清空当天</n-button>
            <n-button size="tiny" quaternary @click="copyToAllDays">复制到全周</n-button>
          </div>
        </div>

        <div v-if="!activeSegments.length" class="empty-day">当天没有时段；点上方格子即可开始绘制</div>
        <div v-else class="segments">
          <div v-for="(segment, index) in activeSegments" :key="`${segment.start_sec}-${index}`" class="segment-row">
            <input
              class="time-input"
              :value="formatHms(segment.start_sec)"
              aria-label="开始时间"
              @change="editSegment(index, 'start_sec', ($event.target as HTMLInputElement).value)"
            />
            <span class="dash">→</span>
            <input
              class="time-input"
              :value="formatHms(segment.end_sec)"
              aria-label="结束时间"
              @change="editSegment(index, 'end_sec', ($event.target as HTMLInputElement).value)"
            />
            <span class="duration">{{ Math.round((segment.end_sec - segment.start_sec) / 60) }} 分钟</span>
            <n-button size="tiny" quaternary type="error" @click="removeSegment(index)">删除</n-button>
          </div>
        </div>

        <div class="actions">
          <n-button type="primary" size="small" :loading="saving" :disabled="!dirty" @click="save">保存计划</n-button>
          <n-button size="small" :disabled="!dirty || saving" @click="reset">撤销</n-button>
          <span v-if="dirty" class="dirty">有未保存的修改</span>
        </div>
      </section>

      <aside class="glass-card panel status-panel">
        <div class="panel-title">运行状态</div>
        <div class="info-row"><span>设备</span><b>{{ state?.scheduler_running ? "运行中" : "未启动" }}</b></div>
        <div class="info-row"><span>调度任务</span><b>{{ state?.scheduler_running ? "已启动（10s 判定一次）" : "未启动" }}</b></div>
        <div class="info-row"><span>当前录像</span><b>{{ phaseText }}</b></div>
        <div class="info-row"><span>此刻判定</span>
          <n-tag size="small" :type="state?.in_window ? 'success' : 'default'" :bordered="false">
            {{ state?.in_window ? "在计划时段内" : "不在计划时段" }}
          </n-tag>
        </div>
        <div class="info-row"><span>本机时间</span><code>{{ state ? `周${state.weekday_label} ${formatHms(state.secs_of_day)}` : "—" }}</code></div>

        <div class="today">
          <span class="label">今日计划</span>
          <div class="mini-grid">
            <i v-for="(on, index) in todayBlocks" :key="index" :class="{ on }" />
          </div>
        </div>

        <div class="hint">
          计划起的录像会标记为「定时」来源；手动启动的录像不会被计划停掉。设备停止或计划关闭时，调度任务随之取消。
        </div>
      </aside>
    </div>
  </div>
</template>

<style scoped>
.page { width: min(1480px, 100%); margin: 0 auto; }
.page-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; margin: 18px 2px 20px; }
.page-title { color: var(--text-primary); font-size: 24px; font-weight: 750; }
.page-sub { margin-top: 6px; color: var(--text-tertiary); font-size: 13px; }
.header-actions { display: flex; align-items: center; gap: 12px; }
.stat { color: var(--text-tertiary); font-size: 12px; font-variant-numeric: tabular-nums; }
.layout { display: grid; grid-template-columns: minmax(0, 1fr) 300px; gap: 18px; align-items: start; }
.panel { padding: 18px; }
.panel-title { margin-bottom: 14px; color: var(--text-primary); font-size: 15px; font-weight: 700; }
.toolbar { display: flex; align-items: center; justify-content: space-between; gap: 16px; flex-wrap: wrap; margin-bottom: 14px; }
.switch { display: inline-flex; align-items: center; gap: 8px; color: var(--text-secondary); font-size: 12px; }
.days { display: flex; gap: 6px; }
.day { padding: 4px 10px; border: 1px solid var(--border-default); border-radius: 8px; background: rgba(255,255,255,.6); color: var(--text-secondary); font-size: 12px; cursor: pointer; }
.day.on { border-color: var(--accent); background: var(--accent-dim); color: var(--accent); font-weight: 650; }
.day.today::after { content: "今"; margin-left: 4px; color: var(--warning); font-size: 10px; }
.axis { display: grid; grid-template-columns: repeat(12, 1fr); color: var(--text-tertiary); font-size: 10px; margin-bottom: 4px; }
.axis span { text-align: left; }
.grid { display: grid; grid-template-columns: repeat(48, 1fr); gap: 2px; margin-bottom: 18px; }
.cell { height: 34px; padding: 0; border: 1px solid color-mix(in srgb, var(--border-default) 70%, transparent); border-radius: 3px; background: rgba(255,255,255,.5); cursor: pointer; }
.cell.half { border-right-width: 2px; }
.cell.on { border-color: var(--accent); background: color-mix(in srgb, var(--accent) 42%, white); }
.cell.now { box-shadow: inset 0 0 0 2px var(--warning); }
.segment-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; flex-wrap: wrap; margin-bottom: 10px; }
.segment-actions { display: flex; gap: 6px; }
.label { color: var(--text-tertiary); font-size: 11px; }
.empty-day { padding: 22px; border: 1px dashed var(--border-default); border-radius: 10px; color: var(--text-tertiary); text-align: center; font-size: 12px; }
.segments { display: grid; gap: 8px; }
.segment-row { display: flex; align-items: center; gap: 8px; padding: 8px 10px; border: 1px solid var(--border-default); border-radius: 9px; background: rgba(255,255,255,.5); }
.time-input { width: 96px; padding: 4px 8px; border: 1px solid var(--border-default); border-radius: 6px; background: #fff; color: var(--text-primary); font-family: ui-monospace, monospace; font-size: 12px; }
.time-input:focus { border-color: var(--accent); outline: none; }
.dash { color: var(--text-tertiary); font-size: 12px; }
.duration { margin-left: auto; color: var(--text-tertiary); font-size: 11px; }
.actions { display: flex; align-items: center; gap: 10px; margin-top: 18px; }
.dirty { color: var(--warning); font-size: 11px; }
.status-panel { align-self: start; }
.info-row { display: grid; gap: 4px; margin-bottom: 12px; color: var(--text-tertiary); font-size: 11px; }
.info-row b, .info-row code { color: var(--text-secondary); font-size: 12px; }
.today { margin: 16px 0; }
.mini-grid { display: grid; grid-template-columns: repeat(48, 1fr); gap: 1px; margin-top: 6px; }
.mini-grid i { display: block; height: 14px; border-radius: 1px; background: color-mix(in srgb, var(--border-default) 60%, white); }
.mini-grid i.on { background: var(--accent); }
.hint { padding: 10px 12px; border-radius: 9px; background: var(--accent-dim); color: var(--text-secondary); font-size: 11px; line-height: 1.6; }
@media (max-width: 1080px) { .layout { grid-template-columns: 1fr; } .grid { gap: 1px; } .cell { height: 26px; } }
</style>
