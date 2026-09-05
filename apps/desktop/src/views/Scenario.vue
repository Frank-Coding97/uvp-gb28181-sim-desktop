<script setup lang="ts">
// 压力测试工作台：场景编排、生命周期控制、实时指标和报告导出。
import {
  computed,
  nextTick,
  onActivated,
  onMounted,
  onUnmounted,
  ref,
} from "vue";
import {
  NAlert,
  NButton,
  useMessage,
} from "naive-ui";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { usePlatform } from "../platform";
import { buildScenarioToml } from "../lib/scenarioToml";
import { formatError, type IpcErrorDto } from "../lib/ipcError";
import ScenarioForm, { type ScenarioFormModel } from "../components/ScenarioForm.vue";
import ScenarioCharts, { type MetricsSnapshot } from "../components/ScenarioCharts.vue";

const message = useMessage();
const { active: activePlatform, activeHasSecret, refreshSecretStatus } = usePlatform();

const form = ref<ScenarioFormModel>({
  base_device_id: "34020000001320000001",
  count: 10,
  heartbeat_interval: 60,
  channels_per_device: 1,
  gb_version: "V2022",
  media_profile: "A",
  bitrate_kbps: 512,
  ramp_per_second: 50,
  active_ratio: 100,
  video_source: "",
  position_enabled: false,
  position_interval: 10,
  alarm_enabled: false,
  alarm_interval: 30,
});

const mediaLabels: Record<string, string> = {
  A: "A · 仅信令",
  B: "B · 轻量伪流",
  C: "C · 真实视频",
};

const running = ref(false);
const starting = ref(false);
const stopping = ref(false);
const stopPending = ref(false);
const currentRunId = ref<number | null>(null);
let lastSeenRunId = 0;
let reconcileSequence = 0;
const hasReport = ref(false);
const statusText = ref("就绪");
const statusTone = computed(() => {
  if (starting.value || stopping.value || stopPending.value) return "busy";
  return running.value ? "running" : "idle";
});
const platformLabel = computed(() => {
  const p = activePlatform.value;
  return p ? `${p.name} · ${p.server_host}:${p.server_port} · ${p.transport}` : "未配置目标平台";
});

interface StressState {
  run_id: number;
  running: boolean;
  stopping: boolean;
  reason?: "completed" | "failed" | "stopped" | "stopping";
  error?: IpcErrorDto | string;
  device_count?: number;
  metrics?: MetricsSnapshot;
}

interface MetricsTick {
  run_id: number;
  metrics: MetricsSnapshot;
}

interface StressStatus {
  running: boolean;
  stopping: boolean;
  run_id: number | null;
  device_count: number;
  has_report: boolean;
}

const latest = ref<MetricsSnapshot | null>(null);
const chartRef = ref<InstanceType<typeof ScenarioCharts> | null>(null);

const successRate = computed(() =>
  latest.value ? `${(latest.value.device_success_rate * 100).toFixed(1)}%` : "—",
);
const successHealth = computed(() => {
  if (!latest.value || latest.value.register_first_attempt === 0) return "等待采样";
  const rate = latest.value.device_success_rate;
  if (rate >= 0.98) return "健康";
  if (rate >= 0.9) return "需观察";
  return "异常";
});
const onlineText = computed(() => {
  const m = latest.value;
  if (!m) return "—";
  return `${m.devices_online}`;
});
const onlineNote = computed(() => {
  const m = latest.value;
  if (!m) return "等待上线";
  return `峰值 ${m.devices_online_peak} · 目标 ${form.value.count}`;
});
const latencyText = computed(() => {
  const l = latest.value?.register_latency;
  if (!l || l.count === 0) return "—";
  return `${l.p95_ms} ms`;
});
const latencyNote = computed(() => {
  const l = latest.value?.register_latency;
  if (!l || l.count === 0) return "等待采样";
  return `p50 ${l.p50_ms} · p99 ${l.p99_ms} · max ${l.max_ms} ms`;
});
const heartbeatLatencyNote = computed(() => {
  const l = latest.value?.heartbeat_latency;
  if (!l || l.count === 0) return "";
  return `心跳 p95 ${l.p95_ms} ms`;
});
const trustWarning = computed(() => {
  const m = latest.value;
  if (!m || m.results_trustworthy) return "";
  const t = m.transport as Record<string, number> | undefined;
  const parts: string[] = [];
  if (m.fail_local) parts.push(`本地发送失败 ${m.fail_local}`);
  if (t?.dropped_queue_full) parts.push(`入站队列丢弃 ${t.dropped_queue_full}`);
  if (t?.send_errors) parts.push(`socket 发送错误 ${t.send_errors}`);
  if (t?.parse_errors) parts.push(`报文解析失败 ${t.parse_errors}`);
  if (t?.rate_limited)
    parts.push(`UDP 来源限流 ${t.rate_limited} 包/${t.rate_limited_bytes} 字节`);
  if (t?.anti_amplification_blocked)
    parts.push(`反放大阻断 ${t.anti_amplification_blocked}`);
  if (t?.oversized_sent) parts.push(`超 MTU 报文 ${t.oversized_sent}`);
  return `检测到本机瓶颈(${parts.join("、") || "本地压力 " + m.local_pressure}),失败数不能直接归因给平台;请降低设备数或放宽爬坡后重测。`;
});

const streamNote = computed(() => {
  if (!latest.value) return "等待点播";
  if (latest.value.active_streams > 0) return "平台正在收流";
  return form.value.media_profile === "A" ? "仅信令场景" : "等待 INVITE 点播";
});
const heartbeatNote = computed(() => {
  const failed = latest.value?.heartbeat_failed ?? 0;
  return failed > 0 ? "检查网络抖动" : "心跳稳定";
});
const reportNote = computed(() => {
  const total = (latest.value?.position_reported ?? 0) + (latest.value?.alarm_reported ?? 0);
  return total > 0 ? "主动上报已生效" : "未触发主动上报";
});

const failureAdvice = computed(() => {
  const m = latest.value;
  if (!m || m.register_failed === 0) return "暂无失败。若平台无设备上线，优先检查平台地址、域 ID 与设备 ID 是否匹配。";
  if (m.fail_local > 0 && m.fail_local >= m.fail_timeout) return "本机瓶颈为主:本地 socket 发送失败或队列溢出。先减少设备数、放宽爬坡速率,再判断平台承载。";
  if (m.fail_rejected_auth > 0 && m.fail_rejected_auth >= m.fail_rejected_overload) {
    return "平台以 401/403 拒绝:这是凭据或权限问题,增减并发不会改变结果。核对 SIP 密码、平台域 ID、设备编号是否已在平台注册。";
  }
  if (m.fail_rejected_overload > 0 && m.fail_rejected_overload >= m.fail_timeout) {
    return "平台以 486/503 拒绝:这是平台侧过载信号,已经触到承载上限。降低设备数或爬坡速率再看拐点在哪。";
  }
  if (m.fail_rejected_other > 0 && m.fail_rejected_other >= m.fail_timeout) {
    return "平台以其它 4xx/5xx 拒绝(常见 404):设备编号可能不被平台接受。核对编号规则与平台侧的设备清单。";
  }
  if (m.fail_timeout >= m.fail_other) return "事务超时较多：检查平台 IP/端口、防火墙、SIP 监听地址和本机网络出口。";
  return "网络或其它错误较多：查看系统日志页，确认本机 socket 权限、媒体源和运行时依赖。";
});

const failureWatermark = computed(() => {
  const n = latest.value?.first_failure_online;
  if (n === null || n === undefined) return "";
  return `首次失败发生在在线 ${n} 台时`;
});
const rampThrottled = computed(() => (latest.value?.ramp_throttled ?? 0) > 0);
const failureSamples = computed(() => (latest.value?.failure_samples ?? []).slice(0, 5));

function calcFailPercent(val?: number): string {
  const total = latest.value?.register_failed ?? 0;
  if (!total || !val) return "0%";
  return `${Math.min(100, Math.max(0, Math.round((val / total) * 100)))}%`;
}
const scenarioSummary = computed(() => {
  const media = mediaLabels[form.value.media_profile] ?? form.value.media_profile;
  return [
    `${form.value.count} 台设备`,
    form.value.ramp_per_second ? `爬坡 ${form.value.ramp_per_second} 台/秒` : "一次性拉起",
    `${form.value.channels_per_device} 通道/设备`,
    media,
  ].join(" · ");
});

function validateForm(): string | null {
  const p = activePlatform.value;
  if (!p) return "请先在顶栏配置目标平台";
  if (!activeHasSecret.value) return "当前平台档案尚未配置 SIP 密码，请在顶栏「编辑」中填写并保存";
  if (!/^\d{20}$/.test(form.value.base_device_id)) return "起始设备 ID 必须是 20 位数字";
  if (form.value.count < 1 || form.value.count > 10_000) return "设备数量必须在 1 到 10000 之间";
  if (form.value.media_profile === "C" && !form.value.video_source.trim()) {
    return "C 档真实媒体压测必须选择视频源";
  }
  return null;
}

async function pickVideoSource() {
  try {
    const picked = await invoke<string | null>("pick_video_source");
    if (typeof picked === "string") form.value.video_source = picked;
  } catch (error) {
    message.error(formatError(error, "选择文件失败"), { duration: 0, closable: true });
  }
}

async function startStress() {
  await refreshSecretStatus();
  const validation = validateForm();
  if (validation) {
    message.warning(validation);
    return;
  }

  starting.value = true;
  statusText.value = "正在启动";
  try {
    const toml = buildScenarioToml(form.value, activePlatform.value!);
    await invoke("validate_scenario", { toml });
    chartRef.value?.resetSeries();
    latest.value = null;
    hasReport.value = false;
    const result = await invoke<string>("start_stress", {
      toml,
      count: form.value.count,
      profileId: activePlatform.value!.id,
      positionInterval: form.value.position_enabled ? form.value.position_interval : 0,
      alarmInterval: form.value.alarm_enabled ? form.value.alarm_interval : 0,
    });
    const state = await reconcile();
    if (state?.running) {
      message.success(result);
    } else {
      message.warning("启动请求已提交，但任务已结束，请查看运行日志");
    }
  } catch (error) {
    await reconcile();
    if (!running.value) statusText.value = "启动失败";
    message.error(formatError(error, "压测启动失败"), { duration: 0, closable: true });
  } finally {
    starting.value = false;
  }
}

async function stopStress() {
  stopPending.value = true;
  statusText.value = "正在停止";
  try {
    const result = await invoke<string>("stop_stress");
    await reconcile();
    message.info(result);
  } catch (error) {
    await reconcile();
    message.error(formatError(error, "压测停止失败"), { duration: 0, closable: true });
  } finally {
    stopPending.value = false;
  }
}

async function exportReport() {
  try {
    const path = await invoke<string>("export_report");
    message.success(`报告已导出：${path}`, { duration: 6000 });
  } catch (error) {
    message.error(formatError(error, "报告导出失败"), { duration: 0, closable: true });
  }
}

async function reconcile(): Promise<StressStatus | null> {
  const requestId = ++reconcileSequence;
  try {
    const state = await invoke<StressStatus>("get_stress_status");
    if (requestId !== reconcileSequence) return null;
    if (state.run_id !== null) {
      if (state.run_id < lastSeenRunId) return null;
      lastSeenRunId = Math.max(lastSeenRunId, state.run_id);
    }

    running.value = state.running;
    stopping.value = state.stopping;
    currentRunId.value = state.run_id;
    hasReport.value = state.has_report;
    if (state.running) {
      statusText.value = state.stopping
        ? `正在停止 · ${state.device_count} 台`
        : `运行中 · ${state.device_count} 台`;
      try {
        const snapshot = JSON.parse(await invoke<string>("get_metrics")) as MetricsSnapshot;
        if (requestId !== reconcileSequence || currentRunId.value !== state.run_id) return null;
        latest.value = snapshot;
      } catch {
        // 等待事件推送
      }
    } else if (statusText.value !== "异常结束") {
      statusText.value = state.has_report ? "已停止" : "就绪";
    }
    return requestId === reconcileSequence ? state : null;
  } catch (error) {
    if (requestId !== reconcileSequence) return null;
    statusText.value = "引擎不可用";
    console.error("压测状态对账失败", error);
    return null;
  }
}

let unlistenMetrics: UnlistenFn | null = null;
let unlistenState: UnlistenFn | null = null;

onMounted(async () => {
  unlistenMetrics = await listen<MetricsTick>("metrics_tick", (event) => {
    const tick = event.payload;
    if (currentRunId.value !== tick.run_id) return;
    latest.value = tick.metrics;
    hasReport.value = true;
    chartRef.value?.pushPoint(tick.metrics);
  });
  unlistenState = await listen<StressState>("stress_state", (event) => {
    const state = event.payload;
    if (state.run_id < lastSeenRunId) return;
    if (state.running && currentRunId.value === null && state.run_id === lastSeenRunId) return;
    if (currentRunId.value !== null && currentRunId.value !== state.run_id) {
      if (state.run_id < currentRunId.value) return;
    }

    reconcileSequence += 1;
    if (state.run_id > lastSeenRunId) lastSeenRunId = state.run_id;
    if (state.running) currentRunId.value = state.run_id;

    running.value = state.running;
    stopping.value = state.stopping;
    if (state.metrics) {
      latest.value = state.metrics;
      hasReport.value = true;
    }

    if (state.running) {
      statusText.value = state.stopping
        ? `正在停止 · ${state.device_count ?? form.value.count} 台`
        : `运行中 · ${state.device_count ?? form.value.count} 台`;
      return;
    }

    currentRunId.value = null;
    if (state.reason === "failed") {
      statusText.value = "异常结束";
      message.error(formatError(state.error ?? "未知错误", "压测异常结束"));
    } else {
      statusText.value = state.reason === "completed" ? "已完成" : "已停止";
    }
  });
  await reconcile();
});

onActivated(async () => {
  await reconcile();
  await nextTick();
  chartRef.value?.resize();
});

onUnmounted(() => {
  unlistenMetrics?.();
  unlistenState?.();
});
</script>

<template>
  <div class="page" :class="{ 'is-running': running || starting || stopping || stopPending }">
    <header class="page-header">
      <div>
        <div class="eyebrow">LOAD TESTING WORKBENCH</div>
        <h1>压力测试工作台</h1>
        <p>高并发模拟注册、心跳、媒体与主动上报，实时观察平台承载与瓶颈诊断。</p>
      </div>
      <div class="header-actions">
        <div class="status-chip" :class="statusTone">
          <span class="status-dot" />
          {{ statusText }}
        </div>
        <n-button size="small" secondary :disabled="!hasReport" @click="exportReport">导出脱敏报告</n-button>
      </div>
    </header>

    <n-alert v-if="activePlatform?.transport === 'TCP'" type="info" :show-icon="false" class="notice">
      当前使用 SIP over TCP：目录树很大（单条报文超 MTU）或网络丢包较多时更稳，代价是每台设备一条 TCP 连接，万级压测请优先用 UDP。
    </n-alert>

    <div v-if="trustWarning" class="trust-warning" role="status" aria-live="polite">
      <span aria-hidden="true">⚠</span>
      <span>{{ trustWarning }}</span>
    </div>

    <!-- 顶部 6 列高密 KPI 指标矩阵 -->
    <section class="metric-grid">
      <div class="glass-card metric primary">
        <span class="metric-label">上线成功率</span>
        <strong class="metric-value">{{ successRate }}</strong>
        <small class="metric-sub">
          {{ latest?.register_succeeded ?? 0 }} 成功 / {{ latest?.register_first_attempt ?? 0 }} 台 · 重试 {{ latest?.register_retry ?? 0 }} · {{ successHealth }}
        </small>
      </div>
      <div class="glass-card metric">
        <span class="metric-label">当前在线</span>
        <strong class="metric-value">{{ onlineText }}</strong>
        <small class="metric-sub">{{ onlineNote }}</small>
      </div>
      <div class="glass-card metric">
        <span class="metric-label">注册延迟 P95</span>
        <strong class="metric-value">{{ latencyText }}</strong>
        <small class="metric-sub">{{ latencyNote }}</small>
      </div>
      <div class="glass-card metric">
        <span class="metric-label">活跃推流</span>
        <strong class="metric-value">{{ latest?.active_streams ?? 0 }}</strong>
        <small class="metric-sub">{{ streamNote }}</small>
      </div>
      <div class="glass-card metric">
        <span class="metric-label">心跳成功</span>
        <strong class="metric-value">{{ latest?.heartbeat_succeeded ?? 0 }}</strong>
        <small class="metric-sub">
          失败 {{ latest?.heartbeat_failed ?? 0 }} · {{ heartbeatNote }}
          <template v-if="heartbeatLatencyNote"> · {{ heartbeatLatencyNote }}</template>
        </small>
      </div>
      <div class="glass-card metric">
        <span class="metric-label">主动上报</span>
        <strong class="metric-value">{{ (latest?.position_reported ?? 0) + (latest?.alarm_reported ?? 0) }}</strong>
        <small class="metric-sub">位置 {{ latest?.position_reported ?? 0 }} · 报警 {{ latest?.alarm_reported ?? 0 }} · {{ reportNote }}</small>
      </div>
    </section>

    <!-- 核心压测双列协同布局：左侧走势与归因，右侧 340px 紧凑控制面板 -->
    <div class="pressure-layout">
      <!-- 左列：监控走势与深度失败归因 -->
      <div class="monitor-column">
        <ScenarioCharts ref="chartRef" />

        <div class="glass-card panel failure-panel">
          <div class="panel-head">
            <div>
              <div class="panel-title">失败归因与根因诊断</div>
              <div class="panel-sub">根据错误类型、状态码与本机指标智能分析承载瓶颈</div>
            </div>
            <div class="failure-badge" :class="{ 'has-fail': (latest?.register_failed ?? 0) > 0 }">
              注册失败 {{ latest?.register_failed ?? 0 }}
            </div>
          </div>

          <!-- 失败分类水平条形图对比 -->
          <div class="failure-bars">
            <div class="fb-item">
              <div class="fb-header">
                <span class="fb-label"><i class="timeout" />事务超时</span>
                <span class="fb-val">{{ latest?.fail_timeout ?? 0 }} <em class="fb-pct">({{ calcFailPercent(latest?.fail_timeout) }})</em></span>
              </div>
              <div class="fb-track">
                <div class="fb-fill timeout" :style="{ width: calcFailPercent(latest?.fail_timeout) }" />
              </div>
            </div>

            <div class="fb-item">
              <div class="fb-header">
                <span class="fb-label"><i class="rejected" />平台拒绝 (4xx/5xx)</span>
                <span class="fb-val">{{ latest?.fail_rejected ?? 0 }} <em class="fb-pct">({{ calcFailPercent(latest?.fail_rejected) }})</em></span>
              </div>
              <div class="fb-track">
                <div class="fb-fill rejected" :style="{ width: calcFailPercent(latest?.fail_rejected) }" />
              </div>
              <div v-if="(latest?.fail_rejected ?? 0) > 0" class="fb-sub-items">
                <div class="fb-sub">
                  <span>· 凭据/鉴权 401/403</span>
                  <b>{{ latest?.fail_rejected_auth ?? 0 }}</b>
                </div>
                <div class="fb-sub">
                  <span>· 平台过载 486/503</span>
                  <b>{{ latest?.fail_rejected_overload ?? 0 }}</b>
                </div>
                <div class="fb-sub">
                  <span>· 其它拒绝</span>
                  <b>{{ latest?.fail_rejected_other ?? 0 }}</b>
                </div>
              </div>
            </div>

            <div class="fb-item">
              <div class="fb-header">
                <span class="fb-label"><i class="local" />本机瓶颈</span>
                <span class="fb-val">{{ latest?.fail_local ?? 0 }} <em class="fb-pct">({{ calcFailPercent(latest?.fail_local) }})</em></span>
              </div>
              <div class="fb-track">
                <div class="fb-fill local" :style="{ width: calcFailPercent(latest?.fail_local) }" />
              </div>
            </div>

            <div class="fb-item">
              <div class="fb-header">
                <span class="fb-label"><i class="other" />网络或其它</span>
                <span class="fb-val">{{ latest?.fail_other ?? 0 }} <em class="fb-pct">({{ calcFailPercent(latest?.fail_other) }})</em></span>
              </div>
              <div class="fb-track">
                <div class="fb-fill other" :style="{ width: calcFailPercent(latest?.fail_other) }" />
              </div>
            </div>
          </div>

          <!-- 诊断处置建议 -->
          <div class="diag-card">
            <div class="diag-title">诊断建议</div>
            <div class="diag-desc">{{ failureAdvice }}</div>
          </div>

          <div v-if="failureWatermark" class="failure-meta">{{ failureWatermark }}</div>
          <div v-if="rampThrottled" class="failure-meta warn">
            爬坡曾因本机背压减速 {{ latest?.ramp_throttled }} 次 —— 本机跟不上设定速率，
            这段时间的失败不能算作平台承载上限。
          </div>

          <!-- 失败样本复现 -->
          <details v-if="failureSamples.length" class="failure-samples">
            <summary>失败样本（{{ failureSamples.length }} 条，可挑一台复现）</summary>
            <ul>
              <li v-for="sample in failureSamples" :key="sample.device_id + sample.online_at_failure">
                <code>{{ sample.device_id }}</code>
                <span>{{ sample.kind }}<template v-if="sample.status"> · {{ sample.status }}</template></span>
                <span class="dim">在线 {{ sample.online_at_failure }} 台时</span>
              </li>
            </ul>
          </details>
        </div>
      </div>

      <!-- 右列：紧凑表单控制面板 (340px 固定宽度) -->
      <div class="control-column">
        <ScenarioForm
          v-model:form="form"
          :running="running"
          :starting="starting"
          :stopping="stopping"
          :stop-pending="stopPending"
          :platform-label="platformLabel"
          :scenario-summary="scenarioSummary"
          @start="startStress"
          @stop="stopStress"
          @pick-video="pickVideoSource"
        />
      </div>
    </div>
  </div>
</template>

<style scoped>
.page {
  width: min(1360px, 100%);
  margin: 0 auto;
}
.page-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
  margin: 4px 0 10px;
}
.eyebrow {
  color: var(--accent);
  font-size: 10px;
  font-weight: 800;
  letter-spacing: 1.2px;
}
h1 {
  margin: 2px 0 0;
  color: var(--text-primary);
  font-size: 20px;
  letter-spacing: -0.3px;
}
.page-header p {
  margin: 2px 0 0;
  color: var(--text-tertiary);
  font-size: 12px;
  line-height: 1.4;
}
.header-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex: 0 0 auto;
}
.status-chip {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  min-height: 24px;
  padding: 0 10px;
  border: 1px solid var(--border-default);
  border-radius: 999px;
  background: var(--bg-surface);
  color: var(--text-secondary);
  font-size: 12px;
  font-weight: 650;
}
.status-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--text-tertiary);
}
.status-chip.running {
  color: var(--success);
  border-color: rgba(8, 179, 136, 0.25);
  background: rgba(8, 179, 136, 0.08);
}
.status-chip.running .status-dot {
  background: var(--success);
  box-shadow: 0 0 0 3px rgba(8, 179, 136, 0.12);
  animation: pulse 1.8s infinite;
}
.status-chip.busy {
  color: var(--warning);
}
.status-chip.busy .status-dot {
  background: var(--warning);
}
.notice {
  margin-bottom: 10px;
  border-radius: var(--radius-sm);
  font-size: 12px;
}
.trust-warning {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  margin-bottom: 10px;
  padding: 8px 10px;
  border-radius: var(--radius-sm);
  font-size: 12px;
  line-height: 1.45;
  background: color-mix(in srgb, var(--warning) 12%, transparent);
  border: 1px solid color-mix(in srgb, var(--warning) 32%, transparent);
  color: var(--text-primary);
}

/* 顶部 6 列紧凑 KPI 矩阵 */
.metric-grid {
  display: grid;
  grid-template-columns: repeat(6, 1fr);
  gap: 10px;
  margin-bottom: 12px;
}
.metric {
  position: relative;
  overflow: hidden;
  padding: 8px 12px;
  border-radius: var(--radius-md);
}
.metric::after {
  content: "";
  position: absolute;
  width: 50px;
  height: 50px;
  right: -15px;
  top: -20px;
  border-radius: 50%;
  background: var(--accent-dim);
  pointer-events: none;
}
.metric.primary {
  border-color: var(--border-accent);
  background: linear-gradient(145deg, var(--bg-elevated), color-mix(in srgb, var(--accent) 10%, transparent));
}
.metric-label {
  display: block;
  color: var(--text-tertiary);
  font-size: 11px;
  font-weight: 600;
}
.metric-value {
  display: block;
  margin-top: 2px;
  color: var(--text-primary);
  font-size: 20px;
  font-weight: 750;
  line-height: 1.15;
  font-variant-numeric: tabular-nums;
}
.metric.primary .metric-value {
  color: var(--accent-deep, #1e63dc);
}
.metric-sub {
  display: block;
  margin-top: 4px;
  color: var(--text-tertiary);
  font-size: 10.5px;
  line-height: 1.35;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

/* 压测核心双列协同布局 */
.pressure-layout {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 340px;
  gap: 12px;
  align-items: start;
}
.monitor-column {
  display: flex;
  flex-direction: column;
  gap: 12px;
  min-width: 0;
}
.control-column {
  position: sticky;
  top: 12px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/* 失败归因面板 */
.panel {
  padding: 12px 14px;
  border-radius: var(--radius-md);
}
.panel-head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  margin-bottom: 8px;
}
.panel-title {
  color: var(--text-primary);
  font-size: 13.5px;
  font-weight: 700;
}
.panel-sub {
  font-size: 11px;
  color: var(--text-tertiary);
  margin-top: 2px;
}
.failure-badge {
  padding: 2px 8px;
  border-radius: 999px;
  font-size: 11.5px;
  font-weight: 700;
  background: var(--bg-surface);
  color: var(--text-tertiary);
  border: 1px solid var(--border-default);
  font-variant-numeric: tabular-nums;
}
.failure-badge.has-fail {
  background: rgba(239, 68, 68, 0.1);
  color: var(--error);
  border-color: rgba(239, 68, 68, 0.3);
}

.failure-bars {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin: 6px 0;
}
.fb-item {
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.fb-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  font-size: 12px;
}
.fb-label {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  color: var(--text-secondary);
}
.fb-label i {
  width: 6px;
  height: 6px;
  border-radius: 50%;
}
.fb-label i.timeout { background: var(--warning); }
.fb-label i.rejected { background: var(--error); }
.fb-label i.local { background: var(--accent-deep, #1e63dc); }
.fb-label i.other { background: var(--violet); }
.fb-val {
  color: var(--text-primary);
  font-weight: 700;
  font-variant-numeric: tabular-nums;
}
.fb-pct {
  font-style: normal;
  font-weight: 500;
  font-size: 11px;
  color: var(--text-tertiary);
  margin-left: 2px;
}
.fb-track {
  width: 100%;
  height: 5px;
  background: rgba(120, 140, 170, 0.15);
  border-radius: 999px;
  overflow: hidden;
}
.fb-fill {
  height: 100%;
  border-radius: 999px;
  transition: width 0.3s cubic-bezier(0.4, 0, 0.2, 1);
}
.fb-fill.timeout { background: var(--warning); }
.fb-fill.rejected { background: var(--error); }
.fb-fill.local { background: var(--accent-deep, #1e63dc); }
.fb-fill.other { background: var(--violet); }

.fb-sub-items {
  display: flex;
  gap: 12px;
  padding-left: 12px;
  margin-top: 1px;
}
.fb-sub {
  font-size: 11px;
  color: var(--text-tertiary);
  display: flex;
  gap: 4px;
}
.fb-sub b {
  color: var(--text-secondary);
  font-variant-numeric: tabular-nums;
}

.diag-card {
  margin-top: 4px;
  padding: 8px 10px;
  border-radius: var(--radius-sm);
  background: rgba(56, 132, 255, 0.06);
  border: 1px solid rgba(56, 132, 255, 0.18);
}
.diag-title {
  font-size: 11px;
  font-weight: 750;
  color: var(--accent-deep, #1e63dc);
  margin-bottom: 2px;
}
.diag-desc {
  color: var(--text-secondary);
  font-size: 11.5px;
  line-height: 1.5;
}

.failure-meta {
  margin-top: 4px;
  font-size: 11.5px;
  line-height: 1.45;
  color: var(--text-tertiary);
}
.failure-meta.warn {
  color: var(--warning);
}
.failure-samples {
  margin-top: 4px;
  font-size: 11.5px;
  color: var(--text-tertiary);
}
.failure-samples summary {
  cursor: pointer;
  font-weight: 600;
}
.failure-samples ul {
  margin: 4px 0 0;
  padding-left: 14px;
}
.failure-samples li {
  display: flex;
  gap: 8px;
  align-items: baseline;
  padding: 1px 0;
}
.failure-samples code {
  font-family: ui-monospace, "SF Mono", Menlo, monospace;
  color: var(--text-secondary);
}
.failure-samples .dim {
  color: var(--text-tertiary);
  margin-left: auto;
}

@keyframes pulse {
  50% { box-shadow: 0 0 0 7px rgba(8, 179, 136, 0); }
}

@media (max-width: 1200px) {
  .metric-grid {
    grid-template-columns: repeat(3, 1fr);
  }
}

@media (max-width: 980px) {
  .pressure-layout {
    grid-template-columns: 1fr;
  }
  .control-column {
    position: static;
  }
}

@media (max-width: 640px) {
  .metric-grid {
    grid-template-columns: repeat(2, 1fr);
  }
  .page-header {
    flex-direction: column;
  }
  .header-actions {
    width: 100%;
    justify-content: space-between;
  }
  .fb-sub-items {
    flex-direction: column;
    gap: 2px;
  }
}
</style>
