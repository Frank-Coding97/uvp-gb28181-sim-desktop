<script setup lang="ts">
// 设备校时：展示逻辑协议时钟的偏移与来源，并提供 NTP 配置与手动校时。
//
// 这里**不改系统时钟**——与移动端一致，只维护"平台时间 − 本地时间"的偏移，
// 用于心跳/报警/位置等 MANSCDP 时间字段。
import { computed, onMounted, onUnmounted, ref } from "vue";
import { NButton, NInput, NInputNumber, NSwitch, NTag, useMessage } from "naive-ui";
import {
  getClockState,
  normalizeTimeSyncSettings,
  saveTimeSync,
  syncNtpNow,
  validateTimeSyncSettings,
  type ClockState,
  type TimeSyncSettings,
} from "../platform";

const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const message = useMessage();

const state = ref<ClockState | null>(null);
const draft = ref<TimeSyncSettings>(normalizeTimeSyncSettings(null));
/** 后端当前值。单独存一份，才能区分"用户改了草稿"与"后端值变了"。 */
const baseline = ref<TimeSyncSettings>(normalizeTimeSyncSettings(null));
const nowMs = ref(Date.now());
const loading = ref(false);
const saving = ref(false);
const probing = ref(false);
let clockTimer: number | null = null;
let refreshTimer: number | null = null;

const sourceLabel = computed(() => {
  switch (state.value?.clock.source) {
    case "ntp": return "NTP 服务器";
    case "sip_date": return "平台 SIP Date";
    default: return "未校时";
  }
});

const sourceType = computed(() => {
  switch (state.value?.clock.source) {
    case "ntp": return "success" as const;
    case "sip_date": return "info" as const;
    default: return "warning" as const;
  }
});

const offsetText = computed(() => fmtOffset(state.value?.clock.offset_secs ?? 0));
const platformNowMs = computed(() => nowMs.value + (state.value?.clock.offset_secs ?? 0) * 1000);
const dirty = computed(() => {
  return JSON.stringify(normalizeTimeSyncSettings(draft.value))
    !== JSON.stringify(normalizeTimeSyncSettings(baseline.value));
});

function pad(value: number) {
  return String(value).padStart(2, "0");
}

function fmtTime(ms: number) {
  if (!ms) return "—";
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function fmtOffset(secs: number) {
  if (secs === 0) return "0 秒（与平台一致）";
  return `${secs > 0 ? "+" : "−"}${Math.abs(secs)} 秒`;
}

async function load(silent = false) {
  if (!isTauri) return;
  if (!silent) loading.value = true;
  try {
    const next = await getClockState();
    const incoming = normalizeTimeSyncSettings(next.time_sync);
    state.value = next;
    const hadDraft = dirty.value;
    baseline.value = incoming;
    // 用户没有未保存改动时草稿跟随后端；有改动时保留草稿，避免轮询把输入冲掉。
    if (!hadDraft) draft.value = { ...incoming };
  } catch (error) {
    if (!silent) message.error(String(error));
  } finally {
    loading.value = false;
  }
}

async function save() {
  const invalid = validateTimeSyncSettings(draft.value);
  if (invalid) {
    message.error(invalid);
    return;
  }
  if (!state.value) {
    message.error("校时状态尚未加载");
    return;
  }
  saving.value = true;
  try {
    await saveTimeSync(normalizeTimeSyncSettings(draft.value), state.value.config_revision);
    message.success(
      draft.value.ntp_enabled
        ? "NTP 配置已保存，设备运行中会立即按新配置刷新"
        : "NTP 已关闭，设备时间回退到 SIP Date",
    );
    await load(true);
  } catch (error) {
    message.error(String(error));
  } finally {
    saving.value = false;
  }
}

async function probe() {
  probing.value = true;
  try {
    const result = await syncNtpNow();
    message.success(
      `已校时：${result.server}，偏移 ${fmtOffset(result.offset_secs)}，RTT ${result.round_trip_ms}ms`,
    );
    await load(true);
  } catch (error) {
    message.error(`NTP 校时失败：${String(error)}`);
    await load(true);
  } finally {
    probing.value = false;
  }
}

/** 放弃未保存改动，回到后端当前配置。 */
function resetDraft() {
  draft.value = { ...baseline.value };
}

onMounted(() => {
  void load();
  clockTimer = window.setInterval(() => { nowMs.value = Date.now(); }, 1000);
  // 每 10 秒与后端对账一次：NTP 周期任务可能刚更新了偏移，页面不该停在旧值上。
  refreshTimer = window.setInterval(() => { void load(true); }, 10_000);
});

onUnmounted(() => {
  if (clockTimer) clearInterval(clockTimer);
  if (refreshTimer) clearInterval(refreshTimer);
});
</script>

<template>
  <div class="page clock-page">
    <main class="glass-card clock-card">
      <div class="clock-head">
        <div class="clock-head-copy">
          <div class="clock-head-title">当前校时状态</div>
          <div class="clock-head-sub">优先级：NTP 生效期间不覆盖偏移；NTP 失败时回退到最近一次 SIP Date。</div>
        </div>
        <div class="clock-head-actions">
          <n-tag :type="sourceType" round>{{ sourceLabel }}</n-tag>
          <n-button secondary :loading="loading" @click="load()">刷新</n-button>
          <n-button type="primary" :loading="probing" :disabled="!isTauri" @click="probe">立即校时</n-button>
        </div>
      </div>

      <section class="clock-section">
        <div class="clock-note">逻辑协议时钟的平台基准、本机时钟与偏移。只影响设备上报的时间戳，不修改操作系统时间。</div>

        <div class="stat-grid">
        <div class="stat">
          <small>设备对外时间（校时后）</small>
          <strong class="mono">{{ fmtTime(platformNowMs) }}</strong>
        </div>
        <div class="stat">
          <small>本机系统时间</small>
          <strong class="mono muted">{{ fmtTime(nowMs) }}</strong>
        </div>
        <div class="stat">
          <small>偏移（平台 − 本地）</small>
          <strong :class="['mono', (state?.clock.offset_secs ?? 0) === 0 ? '' : 'accent']">{{ offsetText }}</strong>
        </div>
        <div class="stat">
          <small>最近一次校时</small>
          <strong class="mono">{{ state?.clock.synced_at_ms ? fmtTime(state.clock.synced_at_ms) : "从未校时" }}</strong>
        </div>
        <div class="stat">
          <small>SIP Date 回退值</small>
          <strong class="mono">
            {{ state?.clock.sip_synced ? fmtOffset(state.clock.sip_offset_secs) : "尚未收到平台 Date" }}
          </strong>
        </div>
        <div class="stat">
          <small>周期刷新任务</small>
          <strong>{{ state?.ntp_running ? "运行中" : "未运行" }}</strong>
          <span class="hint">{{ state?.ntp_running ? "设备运行期间按间隔查询 NTP" : "启用 NTP 并启动设备后开始" }}</span>
        </div>
      </div>

      <div class="detail-row">
        <span class="detail-label">来源凭证</span>
        <code>{{ state?.clock.detail || "尚无校时凭证" }}</code>
      </div>

      <div v-if="state?.clock.last_error" class="error-row">
        <b>最近一次失败</b>
        <span>{{ state.clock.last_error }}</span>
      </div>
      </section>

      <section class="clock-section">
        <div class="clock-section-head">
          <div>
            <h2>NTP 校时</h2>
            <p>默认关闭；关闭或查询失败时按注册 200 OK 的 SIP Date 校时。设备运行中修改会立即重启刷新任务。</p>
          </div>
          <n-switch v-model:value="draft.ntp_enabled" :disabled="!isTauri" />
        </div>

      <div class="form-grid">
        <label class="field">
          <span>NTP 服务器</span>
          <n-input v-model:value="draft.ntp_server" placeholder="ntp.aliyun.com" :disabled="!draft.ntp_enabled" />
        </label>
        <label class="field">
          <span>端口</span>
          <n-input-number v-model:value="draft.ntp_port" :min="1" :max="65535" :disabled="!draft.ntp_enabled" style="width: 100%" />
        </label>
        <label class="field">
          <span>刷新间隔（秒）</span>
          <n-input-number
            v-model:value="draft.refresh_interval_secs"
            :min="1"
            :max="86400"
            :disabled="!draft.ntp_enabled"
            style="width: 100%"
          />
        </label>
      </div>

      <footer class="panel-foot">
        <span v-if="dirty" class="dirty">有未保存的修改</span>
        <span v-else class="hint">配置已与后端一致</span>
        <div class="foot-actions">
          <n-button secondary :disabled="!dirty || saving" @click="resetDraft">撤销</n-button>
          <n-button type="primary" :loading="saving" :disabled="!dirty || !isTauri" @click="save">保存配置</n-button>
        </div>
      </footer>
      </section>
    </main>
  </div>
</template>

<style scoped>
/* 页面宽度与全局 .page（theme.css）保持一致；与设置页同口径：滚动下沉到主卡片内部，
   卡片靠 flex 撑满可用高度、始终留在视口内，四角圆角在任意滚动位置都完整保留。 */
.page { width: min(1480px, 100%); margin: 0 auto; }
.clock-page { flex: 1; min-height: 0; display: flex; flex-direction: column; overflow: hidden; padding-bottom: 16px; box-sizing: border-box; }
.clock-card { flex: 1 1 auto; min-height: 0; display: flex; flex-direction: column; overflow-y: auto; overflow-x: hidden; padding: 0; }
/* 冻结头部：铺满卡片宽度、顶部两角沿用卡片圆角、去掉悬浮投影，是卡片头部而非独立横条。 */
.clock-head { position: sticky; top: 0; z-index: 5; flex: 0 0 auto; display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 14px 16px 15px; border-bottom: 1px solid var(--border-default); border-radius: var(--radius-lg) var(--radius-lg) 0 0; background: color-mix(in srgb, #fff 50%, var(--bg-base)); box-shadow: none; }
.clock-head-copy { min-width: 0; }
.clock-head-title { color: var(--text-secondary); font-size: 13px; font-weight: 600; }
.clock-head-sub { margin-top: 4px; color: var(--text-tertiary); font-size: 11px; line-height: 1.5; }
.clock-head-actions { display: flex; align-items: center; gap: 8px; flex: 0 0 auto; }
.clock-section { display: grid; gap: 16px; padding: 18px 16px 16px; }
.clock-section + .clock-section { border-top: 1px solid var(--border-default); }
.clock-note { color: var(--text-tertiary); font-size: 12px; line-height: 1.6; }
.clock-section-head { display: flex; align-items: start; justify-content: space-between; gap: 16px; }
.clock-section-head h2 { margin: 0; color: var(--text-primary); font-size: 16px; }
.clock-section-head p { margin: 6px 0 0; color: var(--text-tertiary); font-size: 12px; line-height: 1.6; }
.stat-grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 10px; }
.stat { min-width: 0; padding: 12px 14px; border: 1px solid var(--border-subtle); border-radius: 10px; background: rgba(255, 255, 255, .55); }
.stat small { display: block; color: var(--text-tertiary); font-size: 11px; }
.stat strong { display: block; margin-top: 6px; overflow: hidden; color: var(--text-primary); font-size: 15px; text-overflow: ellipsis; white-space: nowrap; }
.stat strong.muted { color: var(--text-secondary); font-weight: 500; }
.stat strong.accent { color: var(--accent); }
.stat .hint { display: block; margin-top: 5px; color: var(--text-tertiary); font-size: 10px; }
.mono { font-family: "SF Mono", Menlo, monospace; font-variant-numeric: tabular-nums; }
.detail-row { display: grid; grid-template-columns: 88px minmax(0, 1fr); align-items: center; gap: 10px; }
.detail-label { color: var(--text-tertiary); font-size: 12px; }
.detail-row code { padding: 8px 10px; border: 1px solid var(--border-subtle); border-radius: 8px; color: var(--text-secondary); background: rgba(15, 23, 42, .03); font-size: 12px; overflow-wrap: anywhere; }
.error-row { display: grid; grid-template-columns: 88px minmax(0, 1fr); gap: 10px; padding: 10px 12px; border: 1px solid rgba(208, 48, 80, .28); border-radius: 8px; color: var(--error); background: rgba(208, 48, 80, .05); font-size: 12px; }
.form-grid { display: grid; grid-template-columns: minmax(0, 2fr) minmax(120px, 1fr) minmax(140px, 1fr); gap: 12px; }
.field { display: grid; gap: 6px; }
.field > span { color: var(--text-secondary); font-size: 12px; }
.panel-foot { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding-top: 4px; border-top: 1px solid var(--border-subtle); }
.foot-actions { display: flex; gap: 8px; }
.hint { color: var(--text-tertiary); font-size: 12px; }
.dirty { color: var(--warning); font-size: 12px; }
@media (max-width: 900px) {
  .stat-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .form-grid { grid-template-columns: 1fr; }
}
@media (max-width: 680px) { .clock-head { align-items: flex-start; flex-direction: column; } }
</style>
