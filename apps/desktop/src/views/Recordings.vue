<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { NButton, NEmpty, NIcon, NInput, NPopconfirm, NSelect, NTag, useMessage } from "naive-ui";
import { PlayCircleOutline, RefreshOutline, StopCircleOutline, TrashOutline, VideocamOutline } from "@vicons/ionicons5";
import { useDevice } from "../device";

type RecordingPhase = "idle" | "starting" | "recording" | "finalizing" | "failed";
type RecordingKind = "time" | "alarm" | "manual";

interface RecordingEntry {
  id: string;
  channel_id: string;
  start_time_ms: number;
  end_time_ms: number;
  start_time: string;
  end_time: string;
  source: "local" | "platform";
  kind: RecordingKind;
  path: string;
  size_bytes: number;
}

interface RecordingState {
  phase: RecordingPhase;
  active_id: string | null;
  error: string | null;
}

const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const message = useMessage();
const { deviceLive } = useDevice();
const recordings = ref<RecordingEntry[]>([]);
const state = ref<RecordingState>({ phase: "idle", active_id: null, error: null });
const loading = ref(false);
const actionBusy = ref(false);
const keyword = ref("");
const kind = ref<RecordingKind | "all">("all");
const selected = ref<RecordingEntry | null>(null);
let timer: number | null = null;

const phaseText: Record<RecordingPhase, string> = {
  idle: "空闲",
  starting: "等待关键帧",
  recording: "录像中",
  finalizing: "正在收尾",
  failed: "录像失败",
};
const kindText: Record<RecordingKind, string> = { time: "定时", alarm: "报警", manual: "手动" };
const kindOptions = [
  { label: "全部类型", value: "all" },
  { label: "手动录像", value: "manual" },
  { label: "定时录像", value: "time" },
  { label: "报警录像", value: "alarm" },
];
const isActive = computed(() => ["starting", "recording", "finalizing"].includes(state.value.phase));
const canStart = computed(() => deviceLive.value && !isActive.value);
const filtered = computed(() => {
  const query = keyword.value.trim().toLowerCase();
  return recordings.value.filter((entry) =>
    (kind.value === "all" || entry.kind === kind.value)
    && (!query || `${entry.id} ${entry.channel_id} ${entry.path}`.toLowerCase().includes(query)),
  );
});
const selectedUrl = computed(() => selected.value && isTauri ? convertFileSrc(selected.value.path) : "");

function formatBytes(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

async function refresh(silent = false) {
  if (!isTauri) return;
  if (!silent) loading.value = true;
  try {
    const [items, current] = await Promise.all([
      invoke<RecordingEntry[]>("get_recordings"),
      invoke<RecordingState>("get_recording_state"),
    ]);
    recordings.value = items.slice().sort((a, b) => b.start_time_ms - a.start_time_ms);
    state.value = current;
    if (selected.value) selected.value = items.find((item) => item.id === selected.value?.id) ?? null;
  } catch (error) {
    if (!silent) message.error(String(error));
  } finally {
    loading.value = false;
  }
}

async function toggleRecording() {
  if (actionBusy.value) return;
  actionBusy.value = true;
  try {
    if (isActive.value) {
      await invoke("stop_recording");
      message.success("录像已收尾并写入索引");
    } else {
      await invoke("start_recording");
      message.success("录像已启动");
    }
    await refresh(true);
  } catch (error) {
    message.error(String(error));
    await refresh(true);
  } finally {
    actionBusy.value = false;
  }
}

async function remove(entry: RecordingEntry) {
  try {
    await invoke("delete_recording", { id: entry.id });
    if (selected.value?.id === entry.id) selected.value = null;
    message.success("录像已删除");
    await refresh(true);
  } catch (error) {
    message.error(String(error));
  }
}

onMounted(() => {
  void refresh();
  timer = window.setInterval(() => void refresh(true), 1000);
});
onUnmounted(() => { if (timer) clearInterval(timer); });
</script>

<template>
  <div class="recording-page">
    <header class="page-head">
      <div>
        <span class="eyebrow">LOCAL MEDIA ARCHIVE</span>
        <h1>录像中心</h1>
        <p>录制当前模拟设备的共享画面，并为国标检索、回放与下载提供真实文件。</p>
      </div>
      <div class="head-actions">
        <n-button secondary :loading="loading" @click="refresh()"><template #icon><n-icon><RefreshOutline /></n-icon></template>刷新</n-button>
        <n-button :type="isActive ? 'error' : 'primary'" :loading="actionBusy" :disabled="!isActive && !canStart" @click="toggleRecording">
          <template #icon><n-icon><StopCircleOutline v-if="isActive" /><VideocamOutline v-else /></n-icon></template>
          {{ isActive ? "停止录像" : "开始录像" }}
        </n-button>
      </div>
    </header>

    <section class="status-strip">
      <div><small>录像状态</small><strong :class="`phase-${state.phase}`"><i />{{ phaseText[state.phase] }}</strong></div>
      <div><small>设备采集</small><strong>{{ deviceLive ? "设备运行中" : "设备未启动" }}</strong></div>
      <div><small>已完成录像</small><strong>{{ recordings.length }} 段</strong></div>
      <div class="status-error"><small>最近错误</small><strong>{{ state.error || "无" }}</strong></div>
    </section>

    <div class="workspace">
      <section class="library-panel">
        <div class="toolbar">
          <n-input v-model:value="keyword" clearable placeholder="搜索通道、文件或 ID" />
          <n-select v-model:value="kind" :options="kindOptions" />
        </div>
        <div v-if="filtered.length" class="recording-list">
          <article v-for="entry in filtered" :key="entry.id" class="recording-row" :class="{ selected: selected?.id === entry.id }" @click="selected = entry">
            <button class="play-button" type="button" title="本地播放" @click.stop="selected = entry"><n-icon><PlayCircleOutline /></n-icon></button>
            <div class="recording-main">
              <div><strong>{{ entry.start_time }}</strong><n-tag size="small" :bordered="false">{{ kindText[entry.kind] }}</n-tag><n-tag size="small" :bordered="false" type="info">{{ entry.source === "platform" ? "平台触发" : "本地触发" }}</n-tag></div>
              <small>{{ entry.channel_id }} · {{ entry.path }}</small>
            </div>
            <div class="recording-meta"><strong>{{ formatBytes(entry.size_bytes) }}</strong><small>{{ entry.end_time }}</small></div>
            <n-popconfirm positive-text="删除" negative-text="取消" @positive-click="remove(entry)">
              <template #trigger><n-button quaternary circle type="error" @click.stop><template #icon><n-icon><TrashOutline /></n-icon></template></n-button></template>
              确定删除该录像文件与索引吗？
            </n-popconfirm>
          </article>
        </div>
        <n-empty v-else class="empty" description="暂无符合条件的完成录像" />
      </section>

      <aside class="player-panel">
        <template v-if="selected">
          <div class="player-title"><div><small>LOCAL PLAYBACK</small><h2>{{ selected.start_time }}</h2></div><n-tag>{{ kindText[selected.kind] }}</n-tag></div>
          <video :key="selected.id" controls preload="metadata" :src="selectedUrl" />
          <dl><dt>通道</dt><dd>{{ selected.channel_id }}</dd><dt>时间范围</dt><dd>{{ selected.start_time }} — {{ selected.end_time }}</dd><dt>文件</dt><dd>{{ selected.path }}</dd></dl>
        </template>
        <n-empty v-else class="player-empty" description="选择一段录像进行本地播放" />
      </aside>
    </div>
  </div>
</template>

<style scoped>
.recording-page { height: 100%; min-height: 0; display: grid; grid-template-rows: auto auto minmax(0, 1fr); gap: 12px; }
.page-head { display: flex; align-items: end; justify-content: space-between; gap: 20px; }
.eyebrow, .player-title small { color: var(--accent); font: 650 10px/1.2 "SF Mono", monospace; letter-spacing: .14em; }
h1 { margin: 4px 0 2px; color: var(--text-primary); font-size: 25px; } h2 { margin: 4px 0 0; font-size: 15px; }
.page-head p { margin: 0; color: var(--text-secondary); } .head-actions { display: flex; gap: 8px; }
.status-strip { display: grid; grid-template-columns: .8fr 1fr .8fr 2fr; border: 1px solid var(--border-default); border-radius: 12px; background: rgba(255,255,255,.58); }
.status-strip > div { min-width: 0; padding: 11px 16px; border-right: 1px solid var(--border-subtle); } .status-strip > div:last-child { border: 0; }
.status-strip small { display: block; color: var(--text-tertiary); font-size: 10px; } .status-strip strong { display: flex; align-items: center; gap: 7px; margin-top: 3px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.status-strip i { width: 7px; height: 7px; border-radius: 50%; background: var(--text-tertiary); } .phase-recording i { background: var(--error); box-shadow: 0 0 0 4px rgba(208,48,80,.1); } .phase-failed { color: var(--error); }
.workspace { min-height: 0; display: grid; grid-template-columns: minmax(0, 1.45fr) minmax(340px, .75fr); gap: 12px; }
.library-panel, .player-panel { min-height: 0; overflow: hidden; border: 1px solid var(--border-default); border-radius: 14px; background: rgba(255,255,255,.58); }
.library-panel { display: grid; grid-template-rows: auto minmax(0, 1fr); } .toolbar { display: grid; grid-template-columns: 1fr 160px; gap: 8px; padding: 12px; border-bottom: 1px solid var(--border-subtle); }
.recording-list { overflow: auto; padding: 6px; } .recording-row { display: grid; grid-template-columns: 38px minmax(0,1fr) auto 34px; align-items: center; gap: 10px; margin-bottom: 4px; padding: 10px; border: 1px solid transparent; border-radius: 10px; cursor: pointer; }
.recording-row:hover, .recording-row.selected { border-color: rgba(56,132,255,.3); background: var(--accent-soft); } .play-button { display: grid; place-items: center; width: 34px; height: 34px; border: 0; border-radius: 50%; color: var(--accent); background: white; font-size: 23px; cursor: pointer; }
.recording-main { min-width: 0; } .recording-main > div { display: flex; align-items: center; gap: 6px; } .recording-main small { display: block; margin-top: 5px; overflow: hidden; color: var(--text-tertiary); text-overflow: ellipsis; white-space: nowrap; }
.recording-meta { text-align: right; } .recording-meta small { display: block; margin-top: 4px; color: var(--text-tertiary); font-size: 10px; }
.player-panel { padding: 14px; overflow: auto; } .player-title { display: flex; justify-content: space-between; align-items: start; margin-bottom: 12px; } video { width: 100%; min-height: 220px; border-radius: 10px; background: #07111f; }
dl { display: grid; grid-template-columns: 72px minmax(0,1fr); gap: 8px 12px; margin: 14px 0 0; font-size: 11px; } dt { color: var(--text-tertiary); } dd { margin: 0; overflow-wrap: anywhere; color: var(--text-secondary); }
.empty, .player-empty { align-self: center; margin: auto; } @media (max-width: 1000px) { .workspace { grid-template-columns: 1fr; overflow: auto; } .player-panel { min-height: 360px; } .status-strip { grid-template-columns: repeat(2,1fr); } }
</style>
