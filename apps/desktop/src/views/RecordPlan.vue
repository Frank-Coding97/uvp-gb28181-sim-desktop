<script setup lang="ts">
// 录像计划：一张周时间表，按住拖动即可涂出录像时段。
// 交互约定（对齐主流 NVR / DVR 的周计划表）：
//   - 唯一编辑区 = 7 行 × 48 格；点一下切一格，按住拖出矩形可一次涂/擦多天多时段；
//   - 拖动过程只做视觉预览，松手时一次性提交，避免中间态反复归一化；
//   - 秒级时段编辑收进默认折叠的「精确时段」，不干扰"拖完就保存"的主流程；
//   - 状态用文字 + 颜色双通道表达，不依赖纯颜色传达信息。
import { computed, onActivated, onDeactivated, onMounted, onUnmounted, ref } from "vue";
import { NButton, NSwitch, useMessage } from "naive-ui";
import { getRecordPlan, saveRecordPlan, type RecordPlanState } from "../platform";
import {
  BLOCKS_PER_DAY,
  MAX_SEGMENTS_PER_DAY,
  SECS_PER_DAY,
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
/** 已落盘基线。dirty 只比它与草稿，不依赖运行状态是否已从后端加载。 */
const baseline = ref<RecordPlanSettings>(emptyPlan());
/** 精确时段面板与"复制到全周"的源天；点击/拖动任意格子会自动跟到那一天。 */
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

const weekBlocks = computed(() => draft.value.week.days.map((day) => dayBlocks(day.segments)));
const weekStats = computed(() =>
  draft.value.week.days.map((day) => ({
    count: day.segments.length,
    minutes: Math.round(day.segments.reduce((sum, seg) => sum + seg.end_sec - seg.start_sec, 0) / 60),
  })),
);
const weekTotalMinutes = computed(() => weekStats.value.reduce((sum, day) => sum + day.minutes, 0));
const activeSegments = computed(() => draft.value.week.days[activeDay.value]?.segments ?? []);
const activeDayMinutes = computed(() => weekStats.value[activeDay.value]?.minutes ?? 0);
const daysConfigured = computed(() => weekStats.value.filter((day) => day.count > 0).length);
const effective = computed(() => draft.value.enabled && daysConfigured.value > 0);
const totalSegments = computed(() =>
  draft.value.week.days.reduce((sum, day) => sum + day.segments.length, 0),
);
const todayBlocks = computed(() => {
  const weekday = state.value?.weekday ?? 0;
  return dayBlocks(draft.value.week.days[weekday]?.segments ?? []);
});
const nowPercent = computed(() =>
  state.value ? `${(state.value.secs_of_day / SECS_PER_DAY) * 100}%` : "0%",
);
const dirty = computed(() =>
  JSON.stringify(normalizePlan(baseline.value)) !== JSON.stringify(normalizePlan(draft.value)),
);

function fmtDuration(minutes: number) {
  if (minutes <= 0) return "0 分钟";
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  if (!hours) return `${rest} 分钟`;
  return rest ? `${hours} 小时 ${rest} 分` : `${hours} 小时`;
}

/** 第 block 格的起始时间；block 取 0..48，48 表示 24:00。 */
function blockLabel(block: number) {
  return formatHms(block * 1800).slice(0, 5);
}

function toggle(block: number) {
  draft.value = toggleBlock(draft.value, activeDay.value, block);
}

// ── 拖拽涂选：一张表上直接拖出矩形 ────────────────────────────────────────
// 起点格的状态决定整块矩形的目标值：从空格起拖 = 涂蓝，从蓝格起拖 = 擦除。
// 拖动期间不碰草稿，只在松手时用 toggleBlock 把差异格一次性翻过去。
type DragState = {
  day0: number;
  block0: number;
  day1: number;
  block1: number;
  value: boolean;
  moved: boolean;
};
const drag = ref<DragState | null>(null);
let suppressClick = false;

/** 用命中测试找格子，这样即使鼠标快速划过或拖出网格再回来也不会丢格。 */
function locate(event: PointerEvent) {
  const el = document.elementFromPoint(event.clientX, event.clientY) as HTMLElement | null;
  const cell = el?.closest?.("[data-block]") as HTMLElement | null;
  if (!cell) return null;
  const day = Number(cell.dataset.day);
  const block = Number(cell.dataset.block);
  return Number.isInteger(day) && Number.isInteger(block) ? { day, block } : null;
}

const dragHint = computed(() => {
  const d = drag.value;
  if (!d?.moved) return "";
  const dayA = Math.min(d.day0, d.day1);
  const dayB = Math.max(d.day0, d.day1);
  const blockA = Math.min(d.block0, d.block1);
  const blockB = Math.max(d.block0, d.block1);
  const days = dayA === dayB
    ? `周${WEEKDAY_LABELS[dayA]}`
    : `周${WEEKDAY_LABELS[dayA]} → 周${WEEKDAY_LABELS[dayB]}`;
  const cells = (dayB - dayA + 1) * (blockB - blockA + 1);
  return `${days} ${blockLabel(blockA - 1)} → ${blockLabel(blockB)} · ${cells} 格将${d.value ? "设为录像" : "被擦除"}，松开应用`;
});

function previewState(day: number, block: number): "" | "on" | "off" {
  const d = drag.value;
  if (!d?.moved) return "";
  const dayA = Math.min(d.day0, d.day1);
  const dayB = Math.max(d.day0, d.day1);
  const blockA = Math.min(d.block0, d.block1);
  const blockB = Math.max(d.block0, d.block1);
  if (day < dayA || day > dayB || block < blockA || block > blockB) return "";
  return d.value ? "on" : "off";
}

function onPointerDown(event: PointerEvent) {
  if (event.button !== 0) return;
  const hit = locate(event);
  if (!hit) return;
  activeDay.value = hit.day;
  drag.value = {
    day0: hit.day,
    block0: hit.block,
    day1: hit.day,
    block1: hit.block,
    value: !weekBlocks.value[hit.day][hit.block - 1],
    moved: false,
  };
}

function onPointerMove(event: PointerEvent) {
  const d = drag.value;
  if (!d) return;
  const hit = locate(event);
  if (!hit) return;
  if (hit.day === d.day1 && hit.block === d.block1) return;
  d.day1 = hit.day;
  d.block1 = hit.block;
  if (hit.day !== d.day0 || hit.block !== d.block0) d.moved = true;
}

function commitRect(d: DragState) {
  const dayA = Math.min(d.day0, d.day1);
  const dayB = Math.max(d.day0, d.day1);
  const blockA = Math.min(d.block0, d.block1);
  const blockB = Math.max(d.block0, d.block1);
  const before = weekBlocks.value;
  let next = draft.value;
  for (let day = dayA; day <= dayB; day++) {
    for (let block = blockA; block <= blockB; block++) {
      if (before[day][block - 1] === d.value) continue;
      next = toggleBlock(next, day, block - 1);
    }
  }
  // 擦除矩形可能把一段切两段，超过后端上限时宁可不提交。
  if (next.week.days.some((day) => day.segments.length > MAX_SEGMENTS_PER_DAY)) {
    message.warning(`每天最多 ${MAX_SEGMENTS_PER_DAY} 段，请缩小拖动范围`);
    return;
  }
  draft.value = next;
  activeDay.value = dayA;
}

function onPointerUp() {
  const d = drag.value;
  if (!d) return;
  drag.value = null;
  if (!d.moved) return; // 单击交给 click，键盘也能走同一条路径
  commitRect(d);
  // click 会派发到按下/松开的共同祖先，用一次性延时标志吞掉它。
  suppressClick = true;
  window.setTimeout(() => { suppressClick = false; }, 0);
}

/** 鼠标单击与键盘 Enter / Space 都走这里；拖动产生的 click 被抑制。 */
function onClick(event: MouseEvent) {
  if (suppressClick) { suppressClick = false; return; }
  const cell = (event.target as HTMLElement)?.closest?.("[data-block]") as HTMLElement | null;
  if (!cell) return;
  const day = Number(cell.dataset.day);
  const block = Number(cell.dataset.block);
  activeDay.value = day;
  toggle(block - 1);
}

onMounted(() => {
  window.addEventListener("pointermove", onPointerMove);
  window.addEventListener("pointerup", onPointerUp);
  window.addEventListener("pointercancel", onPointerUp);
});
onUnmounted(() => {
  window.removeEventListener("pointermove", onPointerMove);
  window.removeEventListener("pointerup", onPointerUp);
  window.removeEventListener("pointercancel", onPointerUp);
});

// ── 批量操作 ──────────────────────────────────────────────────────────────
function fillWeek() {
  draft.value = {
    ...draft.value,
    week: { days: Array.from({ length: 7 }, () => ({ segments: [{ start_sec: 0, end_sec: SECS_PER_DAY }] })) },
  };
}

function fillDay(day: number) {
  activeDay.value = day;
  const days = draft.value.week.days.map((item, index) =>
    index === day ? { segments: [{ start_sec: 0, end_sec: SECS_PER_DAY }] } : item,
  );
  draft.value = { ...draft.value, week: { days } };
}

function clearWeek() {
  draft.value = {
    ...draft.value,
    week: { days: Array.from({ length: 7 }, () => ({ segments: [] })) },
  };
}

/** 清空某一天的时段；不带参数时作用于当前选中天（精确时段面板里的「清空当天」）。 */
function clearDay(day: number = activeDay.value) {
  activeDay.value = day;
  const days = draft.value.week.days.map((item, index) => (index === day ? { segments: [] } : item));
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

// ── 精确时段（可选，默认折叠） ────────────────────────────────────────────
function addBlock() {
  const segments = draft.value.week.days[activeDay.value]?.segments ?? [];
  if (segments.length >= MAX_SEGMENTS_PER_DAY) {
    message.warning(`每天最多 ${MAX_SEGMENTS_PER_DAY} 段`);
    return;
  }
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

/** 输入后无条件回写草稿真实值：非法输入不会停留在输入框里造成"界面与计划不一致"。 */
function onTimeChange(index: number, field: "start_sec" | "end_sec", event: Event) {
  const input = event.target as HTMLInputElement;
  editSegment(index, field, input.value);
  const current = draft.value.week.days[activeDay.value]?.segments[index];
  if (current) input.value = formatHms(current[field]);
}

// ── 读写 ──────────────────────────────────────────────────────────────────
async function load(silent = false) {
  if (!isTauri) return;
  if (!silent) loading.value = true;
  try {
    const next = await getRecordPlan();
    // 有未保存改动时只刷新运行状态，绝不覆盖用户正在编辑的草稿。
    const hadDraft = dirty.value;
    state.value = next;
    if (!hadDraft) {
      baseline.value = normalizePlan(next.plan);
      draft.value = normalizePlan(next.plan);
      activeDay.value = next.weekday;
    }
  } catch (error) {
    if (!silent) message.error(`读取录像计划失败：${String(error)}`);
  } finally {
    loading.value = false;
  }
}

async function save() {
  if (!state.value) {
    message.warning("尚未连接桌面后端，无法保存计划");
    return;
  }
  saving.value = true;
  try {
    const saved = await saveRecordPlan(normalizePlan(draft.value), state.value.config_revision);
    const savedPlan = normalizePlan(saved.record_plan ?? draft.value);
    state.value = { ...state.value, plan: savedPlan, config_revision: saved.config_revision };
    baseline.value = savedPlan;
    draft.value = savedPlan;
    message.success("录像计划已保存，运行中的设备立即生效");
  } catch (error) {
    message.error(`保存失败：${String(error)}`);
  } finally {
    saving.value = false;
  }
}

function reset() {
  draft.value = normalizePlan(baseline.value);
}

// 调度器每 10s 判定一次，页面状态跟随刷新；离开页面立即停表，避免后台空转。
let pollTimer: number | undefined;
onActivated(() => {
  void load();
  if (isTauri) pollTimer = window.setInterval(() => { void load(true); }, 10000);
});
onDeactivated(() => {
  if (pollTimer) window.clearInterval(pollTimer);
  pollTimer = undefined;
});
</script>

<template>
  <div class="page page-wrap">
    <!-- 不再单独占一条页头横幅：标题与操作随主卡片头部一起下移，滚动时少一层横条 -->
    <section class="glass-card plan-card">
      <div class="plan-head">
        <div class="plan-head-copy">
          <div class="panel-title">录像计划</div>
          <div class="panel-sub">按住拖动可跨天连续涂选，从蓝色格起拖则是擦除</div>
        </div>
        <div class="plan-head-right">
          <div class="legend"><i class="legend-on" />录像<i class="legend-off" />不录像</div>
          <div class="master-switch" :class="{ off: !draft.enabled }">
            <span>启用定时录像</span>
            <n-switch v-model:value="draft.enabled" size="medium" aria-label="启用定时录像" />
            <span class="switch-state" :class="{ live: effective }">
              <i />{{ effective ? "生效中" : draft.enabled ? "无时段" : "已暂停" }}
            </span>
          </div>
          <span v-if="dirty" class="dirty-chip"><i />有未保存的修改</span>
          <span class="stat">{{ totalSegments }} 段 · 每周 {{ fmtDuration(weekTotalMinutes) }}</span>
          <n-button size="small" :loading="loading" @click="load()">刷新</n-button>
        </div>
      </div>

      <div v-if="!draft.enabled" class="notice" role="status">
        <span class="notice-icon" aria-hidden="true">!</span>
        <span><b>计划当前不会触发录像。</b>可以继续编排时段，准备好后打开开关并保存。</span>
      </div>

      <div class="toolbar">
        <div class="toolbar-buttons">
          <n-button size="small" secondary @click="fillWeek">全周 24 小时</n-button>
          <n-button size="small" secondary @click="clearWeek">清空全周</n-button>
          <n-button size="small" secondary :disabled="!activeSegments.length" @click="copyToAllDays">复制到全周</n-button>
        </div>
        <div class="toolbar-hint" :class="{ active: drag?.moved }" aria-live="polite">
          {{ drag?.moved ? dragHint : "单击切一格 · 按住拖动涂一片 · 松开即应用" }}
        </div>
      </div>

      <div class="schedule" @pointerdown="onPointerDown" @click="onClick">
        <div class="axis-row" aria-hidden="true">
          <span class="axis-pad" />
          <div class="axis-ticks">
            <template v-for="(tick, index) in ticks" :key="tick">
              <span v-if="index % 2 === 0" :style="{ gridColumn: `${index * 4 + 1} / span 8` }">{{ tick }}</span>
            </template>
            <span class="axis-end">24:00</span>
          </div>
          <span class="axis-pad axis-sum">时长</span>
        </div>

        <div
          v-for="(label, day) in WEEKDAY_LABELS"
          :key="label"
          class="day-row"
          :class="{ today: state?.weekday === day }"
        >
          <div class="day-head">
            <span class="day-name">周{{ label }}</span>
            <em v-if="state?.weekday === day">今</em>
            <button class="day-fill" type="button" :aria-label="`周${label} 全天录像`" @click.stop="fillDay(day)">全天</button>
          </div>
          <div class="day-cells" role="group" :aria-label="`周${label} 24 小时录像计划`">
            <button
              v-for="block in BLOCKS_PER_DAY"
              :key="block"
              type="button"
              class="cell"
              :class="{
                on: weekBlocks[day][block - 1],
                hour: block % 2 === 0,
                now: state?.weekday === day && Math.floor((state?.secs_of_day ?? -1) / 1800) === block - 1,
                'preview-on': previewState(day, block) === 'on',
                'preview-off': previewState(day, block) === 'off',
              }"
              :data-day="day"
              :data-block="block"
              :data-time="`${blockLabel(block - 1)} – ${blockLabel(block)}`"
              :title="`周${label} ${blockLabel(block - 1)} – ${blockLabel(block)}`"
              :aria-pressed="!!weekBlocks[day][block - 1]"
              :aria-label="`周${label} ${blockLabel(block - 1)} 到 ${blockLabel(block)}，${weekBlocks[day][block - 1] ? '录像' : '不录像'}`"
            />
            <div v-if="state?.weekday === day" class="now-line" :style="{ left: nowPercent }" aria-hidden="true" />
          </div>
          <div class="day-end">
            <span class="day-sum" :class="{ muted: !weekStats[day].count }">
              {{ weekStats[day].count ? `${weekStats[day].count} 段 · ${fmtDuration(weekStats[day].minutes)}` : "未配置" }}
            </span>
            <button
              class="day-clear"
              type="button"
              :disabled="!weekStats[day].count"
              :title="`清空周${label}的录像时段`"
              :aria-label="`清空周${label}的录像时段`"
              @click.stop="clearDay(day)"
            >清空</button>
          </div>
        </div>
      </div>

      <div class="plan-foot">
        <div class="foot-hint">
          <span>每格 30 分钟 · 相邻时段自动合并</span>
          <span v-if="state">本机时间 周{{ state.weekday_label }} {{ formatHms(state.secs_of_day) }}</span>
        </div>
        <div class="foot-status">
          <i class="save-dot" :class="{ dirty }" />
          {{ dirty ? "有未保存的修改 · 保存后立即生效" : "计划已与设备一致" }}
        </div>
        <div class="foot-actions">
          <n-button size="small" :disabled="!dirty || saving" @click="reset">撤销修改</n-button>
          <n-button type="primary" size="small" :loading="saving" :disabled="!dirty" @click="save">保存计划</n-button>
        </div>
      </div>
    </section>

    <!-- 秒级编辑是进阶能力，默认收起，不占主流程 -->
    <details class="glass-card advanced">
      <summary>
        <span class="adv-title">精确时段（可选）</span>
        <span class="adv-sub">周{{ WEEKDAY_LABELS[activeDay] }} · {{ activeSegments.length }} / {{ MAX_SEGMENTS_PER_DAY }} 段 · 共 {{ fmtDuration(activeDayMinutes) }} — 需要精确到秒时再展开</span>
      </summary>
      <div class="adv-body">
        <div class="segment-head">
          <span class="label">点表格任意一天，这里会切换到那天的时段</span>
          <div class="segment-actions">
            <n-button size="tiny" secondary :disabled="activeSegments.length >= MAX_SEGMENTS_PER_DAY" @click="addBlock">添加时段</n-button>
            <n-button size="tiny" quaternary :disabled="!activeSegments.length" @click="clearDay()">清空当天</n-button>
            <n-button size="tiny" quaternary :disabled="!activeSegments.length" @click="copyToAllDays">复制到全周</n-button>
          </div>
        </div>
        <div v-if="!activeSegments.length" class="empty-day">
          <div class="empty-title">周{{ WEEKDAY_LABELS[activeDay] }}还没有录制时段</div>
          <div class="empty-sub">在上方时间表拖一段，或直接添加 08:00 – 09:00</div>
          <n-button size="tiny" secondary @click="addBlock">添加 08:00 – 09:00</n-button>
        </div>
        <div v-else class="segments">
          <div v-for="(segment, index) in activeSegments" :key="`${segment.start_sec}-${index}`" class="segment-row">
            <span class="seg-index">{{ String(index + 1).padStart(2, "0") }}</span>
            <input
              class="time-input"
              :value="formatHms(segment.start_sec)"
              aria-label="开始时间"
              @change="onTimeChange(index, 'start_sec', $event)"
            />
            <span class="dash">→</span>
            <input
              class="time-input"
              :value="formatHms(segment.end_sec)"
              aria-label="结束时间"
              @change="onTimeChange(index, 'end_sec', $event)"
            />
            <span class="duration">{{ Math.round((segment.end_sec - segment.start_sec) / 60) }} 分钟</span>
            <n-button size="tiny" quaternary type="error" @click="removeSegment(index)">删除</n-button>
          </div>
        </div>
      </div>
    </details>

    <section class="glass-card status-strip">
      <div class="strip-head">
        <span class="panel-title">运行状态</span>
        <span v-if="!isTauri" class="preview-note">浏览器预览模式：未连接桌面后端，状态数据不刷新。</span>
      </div>
      <div class="strip-items">
        <div class="strip-item">
          <span>此刻判定</span>
          <b :class="{ ok: state?.in_window }"><i class="verdict-dot" />{{ state?.in_window ? "在计划时段内" : "不在计划时段" }}</b>
        </div>
        <div class="strip-item"><span>设备</span><b>{{ state?.scheduler_running ? "运行中" : "未启动" }}</b></div>
        <div class="strip-item"><span>调度任务</span><b>{{ state?.scheduler_running ? "已启动（10s 判定一次）" : "未启动" }}</b></div>
        <div class="strip-item"><span>当前录像</span><b>{{ phaseText }}</b></div>
        <div class="strip-item"><span>本机时间</span><b><code>{{ state ? `周${state.weekday_label} ${formatHms(state.secs_of_day)}` : "—" }}</code></b></div>
      </div>
      <div class="strip-today">
        <span class="label">今日计划</span>
        <div class="mini-grid" role="img" :aria-label="`今日 48 格计划，${todayBlocks.filter(Boolean).length} 格已配置`">
          <i v-for="(on, index) in todayBlocks" :key="index" :class="{ on }" />
        </div>
        <div class="mini-ticks"><span>00:00</span><span>12:00</span><span>24:00</span></div>
      </div>
      <div class="hint">
        计划起的录像会标记为「定时」来源；手动启动的录像不会被计划停掉。设备停止或计划关闭时，调度任务随之取消。
      </div>
    </section>
  </div>
</template>

<style scoped>
.page { width: min(1480px, 100%); margin: 0 auto; padding-inline: 12px; padding-bottom: 8px; }
.stat { color: var(--text-secondary); font-size: 12px; font-variant-numeric: tabular-nums; }
.dirty-chip {
  display: inline-flex; align-items: center; gap: 6px;
  padding: 4px 10px; border-radius: 999px;
  background: color-mix(in srgb, var(--warning) 14%, #fff);
  border: 1px solid color-mix(in srgb, var(--warning) 36%, transparent);
  color: #a4630a; font-size: 11.5px; font-weight: 600; white-space: nowrap;
}
.dirty-chip i { width: 6px; height: 6px; border-radius: 50%; background: var(--warning); }

/* ── 主画布 ────────────────────────────────────────────────────────────── */
.plan-card { padding: 18px 20px 16px; }
.plan-head { display: flex; align-items: center; justify-content: space-between; gap: 16px; flex-wrap: wrap; margin-bottom: 14px; }
.panel-title { color: var(--text-primary); font-size: 15px; font-weight: 700; }
.panel-sub { margin-top: 4px; color: var(--text-secondary); font-size: 11.5px; line-height: 1.5; }
.plan-head-right { display: flex; align-items: center; gap: 14px; flex-wrap: wrap; }
.legend { display: inline-flex; align-items: center; gap: 6px; color: var(--text-secondary); font-size: 11px; }
.legend i { width: 12px; height: 10px; border-radius: 2px; display: inline-block; }
.legend-on { background: linear-gradient(180deg, #4a91ff, #2f76ee); }
.legend-off { background: #fff; border: 1px solid var(--border-default); margin-left: 8px; }
.master-switch {
  display: inline-flex; align-items: center; gap: 10px;
  padding: 7px 12px; border-radius: 10px;
  background: var(--bg-surface); border: 1px solid var(--border-default);
  color: var(--text-primary); font-size: 12.5px; font-weight: 650; white-space: nowrap;
}
.master-switch.off { border-color: var(--border-subtle); }
.switch-state {
  display: inline-flex; align-items: center; gap: 5px;
  padding: 2px 8px; border-radius: 999px; font-size: 10.5px; font-weight: 600;
  background: rgba(143, 161, 189, 0.16); color: var(--text-secondary);
}
.switch-state.live { background: color-mix(in srgb, var(--success) 14%, #fff); color: #067a5e; }
.switch-state i { width: 6px; height: 6px; border-radius: 50%; background: currentColor; }

.notice {
  display: flex; align-items: center; gap: 10px; padding: 10px 14px; margin-bottom: 12px;
  border-radius: var(--radius-md); background: color-mix(in srgb, var(--warning) 12%, #fff);
  border: 1px solid color-mix(in srgb, var(--warning) 32%, transparent);
  color: var(--text-primary); font-size: 12px; line-height: 1.5;
}
.notice-icon {
  flex: 0 0 auto; width: 18px; height: 18px; border-radius: 50%;
  display: grid; place-items: center; background: var(--warning); color: #fff; font-size: 12px; font-weight: 700;
}

.toolbar {
  display: flex; align-items: center; justify-content: space-between; gap: 12px; flex-wrap: wrap;
  margin-bottom: 12px; padding: 9px 12px; border-radius: 10px;
  background: var(--accent-pale); border: 1px solid var(--border-subtle);
}
.toolbar-buttons { display: flex; gap: 6px; flex-wrap: wrap; }
.toolbar-hint { color: var(--text-secondary); font-size: 11.5px; font-variant-numeric: tabular-nums; }
.toolbar-hint.active { color: var(--accent-deep); font-weight: 650; }

/* 刻度与所有行共用同一套列定义与 gap，时间轴才不会相对格子漂移。 */
.schedule { user-select: none; -webkit-user-select: none; }
.axis-row, .day-row {
  display: grid; grid-template-columns: 84px minmax(0, 1fr) 142px; gap: 10px; align-items: center;
}
.axis-row { margin-bottom: 6px; }
.axis-ticks {
  position: relative; display: grid; grid-template-columns: repeat(48, 1fr); gap: 1px;
  height: 15px; color: var(--text-secondary); font-size: 10px; font-variant-numeric: tabular-nums;
}
.axis-ticks > span { white-space: nowrap; }
.axis-end { position: absolute; right: 0; top: 0; }
/* 「时长」标题让开行尾清空按钮的宽度，压在数字上方而不是压在按钮上方。 */
.axis-sum { padding-right: 40px; text-align: right; color: var(--text-secondary); font-size: 10px; }

.day-row { margin-bottom: 5px; }
.day-row.today .day-name { color: var(--accent-deep); }
.day-head { display: flex; align-items: center; gap: 5px; min-width: 0; }
.day-name { color: var(--text-primary); font-size: 12.5px; font-weight: 650; white-space: nowrap; }
.day-head em {
  display: inline-block; padding: 0 4px; border-radius: 4px;
  background: var(--warning); color: #fff; font-size: 9px; font-style: normal; font-weight: 700;
}
.day-fill {
  margin-left: auto; padding: 2px 6px; border: 1px solid var(--border-default); border-radius: 6px;
  background: rgba(255, 255, 255, 0.75); color: var(--text-secondary);
  font-size: 10px; cursor: pointer; opacity: 0.55; transition: opacity var(--transition), border-color var(--transition);
}
.day-row:hover .day-fill, .day-fill:focus-visible { opacity: 1; }
.day-fill:hover { border-color: var(--accent); color: var(--accent-deep); }

/* 1px 缝 + 同色边框：涂满后是一整条时段带，而不是一串独立按钮。 */
.day-cells { position: relative; display: grid; grid-template-columns: repeat(48, 1fr); gap: 1px; min-width: 0; }
.cell {
  position: relative; height: 34px; padding: 0;
  border: 1px solid rgba(120, 160, 210, 0.34);
  border-radius: 2px;
  background: linear-gradient(180deg, #fbfdff, #eef5ff);
  cursor: crosshair; touch-action: none;
  transition: background var(--transition), transform var(--transition), border-color var(--transition);
}
.cell.hour { border-right-width: 2px; border-right-color: rgba(76, 99, 133, 0.32); }
.cell:hover { transform: translateY(-2px); border-color: var(--accent); background: var(--accent-pale); }
.cell:focus-visible { outline: 2px solid var(--accent-deep); outline-offset: 1px; z-index: 5; }
.cell.on { border-color: #3f81f2; background: linear-gradient(180deg, #4a91ff, #2f76ee); }
.cell.on.hour { border-right-color: rgba(255, 255, 255, 0.55); }
.cell.on:hover { background: linear-gradient(180deg, #5a9bff, #3884ff); }
.cell.now { box-shadow: inset 0 0 0 2px var(--warning); }
/* 拖拽预览：目标态用半透明填充 + 描边，松手前就能看到范围。 */
.cell.preview-on { background: linear-gradient(180deg, #86b6ff, #5a93f5); border-color: #2f76ee; box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.75); }
.cell.preview-off {
  background: repeating-linear-gradient(135deg, #fff 0 6px, #ffe6d6 6px 12px);
  border-color: #e58b4c; box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.7);
}
.cell::after {
  content: attr(data-time); position: absolute; bottom: calc(100% + 6px); left: 50%; transform: translateX(-50%);
  z-index: 6; padding: 3px 7px; border-radius: 6px; background: #123055; color: #fff;
  font-size: 10px; white-space: nowrap; pointer-events: none; opacity: 0; transition: opacity .12s;
}
.cell:hover::after, .cell:focus-visible::after { opacity: 1; }
.now-line { position: absolute; top: -3px; bottom: -3px; width: 2px; background: var(--warning); pointer-events: none; z-index: 4; }
.now-line::before {
  content: ""; position: absolute; top: -4px; left: 50%; transform: translateX(-50%);
  border-left: 5px solid transparent; border-right: 5px solid transparent; border-top: 6px solid var(--warning);
}
/* 行尾：时长与「清空」并排，清空默认压低存在感，悬停该行才提亮。 */
.day-end { display: flex; align-items: center; justify-content: flex-end; gap: 6px; min-width: 0; }
.day-sum { color: var(--text-secondary); font-size: 11px; text-align: right; white-space: nowrap; font-variant-numeric: tabular-nums; }
.day-sum.muted { color: var(--text-tertiary); }
.day-clear {
  flex: 0 0 auto; padding: 2px 6px; border: 1px solid var(--border-default); border-radius: 6px;
  background: rgba(255, 255, 255, 0.75); color: var(--text-secondary);
  font-size: 10px; line-height: 1.4; cursor: pointer; opacity: 0.55;
  transition: opacity var(--transition), border-color var(--transition), color var(--transition);
}
.day-row:hover .day-clear, .day-clear:focus-visible { opacity: 1; }
.day-clear:hover { border-color: var(--error); color: var(--error); }
.day-clear:disabled, .day-row:hover .day-clear:disabled { opacity: 0.25; cursor: default; border-color: var(--border-subtle); color: var(--text-tertiary); }

.plan-foot {
  display: flex; align-items: center; justify-content: space-between; gap: 14px; flex-wrap: wrap;
  margin-top: 14px; padding-top: 14px; border-top: 1px solid var(--border-default);
}
.foot-hint { display: flex; gap: 14px; flex-wrap: wrap; color: var(--text-secondary); font-size: 11px; }
.foot-status { display: inline-flex; align-items: center; gap: 8px; color: var(--text-secondary); font-size: 11.5px; }
.save-dot { width: 8px; height: 8px; border-radius: 50%; background: var(--success); flex: 0 0 auto; }
.save-dot.dirty { background: var(--warning); }
.foot-actions { display: flex; gap: 8px; flex: 0 0 auto; }

/* ── 可选：精确时段 ────────────────────────────────────────────────────── */
.advanced { margin-top: 14px; padding: 0; }
.advanced summary {
  display: flex; align-items: baseline; gap: 10px; flex-wrap: wrap;
  padding: 13px 18px; cursor: pointer; list-style: none;
}
.advanced summary::-webkit-details-marker { display: none; }
.advanced summary::after {
  content: "展开"; margin-left: auto; color: var(--accent-deep); font-size: 11.5px; font-weight: 600;
}
.advanced[open] summary::after { content: "收起"; }
.advanced summary:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
.adv-title { color: var(--text-primary); font-size: 13px; font-weight: 650; }
.adv-sub { color: var(--text-secondary); font-size: 11.5px; font-variant-numeric: tabular-nums; }
.adv-body { padding: 0 18px 16px; border-top: 1px solid var(--border-default); }
.label { color: var(--text-secondary); font-size: 11.5px; }
.segment-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; flex-wrap: wrap; margin: 14px 0 10px; }
.segment-actions { display: flex; gap: 6px; flex-wrap: wrap; }
.empty-day {
  display: flex; flex-direction: column; align-items: center; gap: 7px;
  padding: 20px; border: 1px dashed var(--border-accent); border-radius: 12px;
  background: rgba(255, 255, 255, 0.45); text-align: center;
}
.empty-title { color: var(--text-primary); font-size: 13px; font-weight: 600; }
.empty-sub { color: var(--text-secondary); font-size: 11.5px; }
.segments { display: grid; gap: 7px; }
.segment-row {
  display: flex; align-items: center; gap: 9px; padding: 8px 11px;
  border: 1px solid var(--border-default); border-radius: 10px; background: rgba(255, 255, 255, 0.68);
}
.seg-index { color: var(--text-tertiary); font-size: 10px; font-family: ui-monospace, monospace; }
.time-input {
  width: 96px; padding: 5px 9px; border: 1px solid var(--border-default); border-radius: 7px;
  background: #fff; color: var(--text-primary); font-family: ui-monospace, monospace;
  font-size: 12px; font-variant-numeric: tabular-nums;
}
.time-input:focus { border-color: var(--accent); outline: none; box-shadow: 0 0 0 3px var(--accent-dim); }
.dash { color: var(--text-secondary); font-size: 12px; }
.duration { margin-left: auto; color: var(--text-secondary); font-size: 11px; font-variant-numeric: tabular-nums; }

/* ── 运行状态 ─────────────────────────────────────────────────────────── */
.status-strip { margin-top: 14px; padding: 16px 18px; }
.strip-head { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; margin-bottom: 12px; }
.preview-note { padding: 4px 9px; border-radius: 7px; background: var(--accent-dim); color: var(--text-secondary); font-size: 11px; }
.strip-items { display: grid; grid-template-columns: repeat(5, minmax(0, 1fr)); gap: 10px; }
.strip-item {
  display: flex; flex-direction: column; gap: 5px; padding: 9px 12px;
  border-radius: 9px; background: var(--bg-surface); border: 1px solid var(--border-subtle);
}
.strip-item > span { color: var(--text-secondary); font-size: 10.5px; }
.strip-item b { display: inline-flex; align-items: center; gap: 6px; color: var(--text-primary); font-size: 12px; font-weight: 650; }
.strip-item b.ok { color: #067a5e; }
.strip-item code { font-family: ui-monospace, monospace; font-size: 11px; }
.verdict-dot { width: 7px; height: 7px; border-radius: 50%; background: var(--text-tertiary); flex: 0 0 auto; }
.strip-item b.ok .verdict-dot { background: var(--success); }
.strip-today { margin: 14px 0 12px; }
.mini-grid { display: grid; grid-template-columns: repeat(48, 1fr); gap: 1px; margin-top: 7px; }
.mini-grid i { display: block; height: 14px; border-radius: 1px; background: rgba(120, 160, 210, 0.2); }
.mini-grid i.on { background: var(--accent); }
.mini-ticks { display: flex; justify-content: space-between; margin-top: 5px; color: var(--text-secondary); font-size: 10px; }
.hint { padding: 10px 13px; border-radius: 10px; background: var(--accent-dim); color: var(--text-secondary); font-size: 11px; line-height: 1.6; }

@media (max-width: 1240px) {
  .strip-items { grid-template-columns: repeat(3, minmax(0, 1fr)); }
}
@media (max-width: 1080px) {
  .axis-row, .day-row { grid-template-columns: 70px minmax(0, 1fr) 126px; gap: 7px; }
  .axis-sum { padding-right: 38px; }
  .cell { height: 30px; }
}
@media (max-width: 720px) {
  .page { padding-inline: 8px; }
  .plan-card { padding: 14px 12px; }
  .axis-row, .day-row { grid-template-columns: 54px minmax(0, 1fr) 0; gap: 5px; }
  .day-end, .day-sum, .axis-sum { display: none; }
  .cell { height: 26px; }
  .strip-items { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .plan-foot { flex-direction: column; align-items: stretch; }
  .foot-actions { justify-content: flex-end; }
}
@media (prefers-reduced-motion: reduce) {
  .cell, .day-fill, .cell::after { transition: none; }
  .cell:hover { transform: none; }
}
</style>
