<script setup lang="ts">
// 媒体源配置区:媒体模式选择 + 上报动作 + 三种源(文件/摄像头/屏幕)的参数面板 + 采集自检结果。
// 状态集中在 useLiveSources,这里只负责渲染与提示。
import { ref } from "vue";
import { useMessage, NButton, NModal, NSelect, NInput } from "naive-ui";
import { invoke } from "@tauri-apps/api/core";
import { useDevice } from "../device";
import { useLiveSources } from "../composables/useLiveSources";
import { useMediaPreview } from "../composables/useMediaPreview";
import MediaAudioFields from "./MediaAudioFields.vue";
import MediaVideoFields from "./MediaVideoFields.vue";
import { formatError } from "../lib/ipcError";

const message = useMessage();
const { form, startDisabled, canReport } = useDevice();
const {
  mediaMode,
  liveCatalog,
  sourceLoading,
  probeState,
  selectedCameraIndex,
  selectedMicrophone,
  selectedCameraAudioCodec,
  selectedDisplayId,
  selectedScreenAudio,
  selectedScreenAudioCodec,
  screenProfileValidationError,
  canProbeSource,
  syncLiveSourceUri,
  setMediaMode,
  pickVideoSource,
  refreshLiveSources,
  probeLiveSource,
} = useLiveSources();
const { refreshVideoPreview } = useMediaPreview();

async function onPickVideoSource() {
  const picked = await pickVideoSource();
  if (!picked.ok) {
    message.error(picked.error);
    return;
  }
  if (picked.path) refreshVideoPreview(true);
}

async function onRefreshLiveSources() {
  const result = await refreshLiveSources();
  if (result.ok) message.success("采集设备列表已刷新");
  else message.error(result.error ?? "设备枚举失败");
}

async function onProbeLiveSource() {
  const loadingMsg = message.loading("正在探测硬件采集通道与编码器…", { duration: 0 });
  try {
    const outcome = await probeLiveSource();
    if (!outcome) return;
    if (outcome.ready) {
      message.success(outcome.message);
      refreshVideoPreview(true); // 自检通过后联动刷新画面预览
    } else {
      message.warning(outcome.message);
    }
  } finally {
    loadingMsg.destroy();
  }
}

const ALARM_TYPES = [
  { label: "1 - 人工视频报警", value: 1, defaultDesc: "人工视频触发报警" },
  { label: "2 - 运动目标检测 (常用)", value: 2, defaultDesc: "画面检测到运动目标" },
  { label: "3 - 遗留物检测", value: 3, defaultDesc: "检测到可疑遗留物" },
  { label: "4 - 物体移除检测", value: 4, defaultDesc: "重要资产/物体被移除" },
  { label: "5 - 绊线检测", value: 5, defaultDesc: "目标穿越防范警戒绊线" },
  { label: "6 - 区域入侵检测", value: 6, defaultDesc: "目标非法入侵禁区" },
  { label: "7 - 逆行检测", value: 7, defaultDesc: "检测到车辆/人员逆向通行" },
  { label: "8 - 目标徘徊检测", value: 8, defaultDesc: "敏感区域检测到人员异常徘徊" },
  { label: "9 - 流量统计", value: 9, defaultDesc: "通道瞬时流量达到预警阈值" },
  { label: "10 - 密度检测", value: 10, defaultDesc: "监控区域人群聚集过密" },
  { label: "11 - 视频异常检测", value: 11, defaultDesc: "视频画面异常或信号丢失" },
  { label: "12 - 快速移动", value: 12, defaultDesc: "检测到人员异常快速奔跑" },
  { label: "13 - 图像遮挡/篡改", value: 13, defaultDesc: "镜头被遮挡或视角被篡改" },
];

const ALARM_PRIORITIES = [
  { label: "一级警情 (最紧急)", value: 1 },
  { label: "二级警情 (严重)", value: 2 },
  { label: "三级警情 (一般)", value: 3 },
  { label: "四级警情 (提示)", value: 4 },
];

const alarmModalOpen = ref(false);
const selectedAlarmType = ref(2);
const selectedPriority = ref(1);
const alarmDesc = ref("画面检测到运动目标");

function openAlarmModal() {
  alarmModalOpen.value = true;
}

function onAlarmTypeChange(val: number) {
  selectedAlarmType.value = val;
  const found = ALARM_TYPES.find((t) => t.value === val);
  if (found) {
    alarmDesc.value = found.defaultDesc;
  }
}

async function doSendAlarm() {
  try {
    const res = await invoke<string>("fire_alarm", {
      description: alarmDesc.value,
      alarm_type: selectedAlarmType.value,
      priority: selectedPriority.value,
    });
    message.success(res);
    alarmModalOpen.value = false;
  } catch (e) {
    message.error(formatError(e, "上报报警失败"), { duration: 0, closable: true });
  }
}

async function firePosition() {
  try {
    message.success(await invoke<string>("fire_position", { longitude: 116.397, latitude: 39.908 }));
  } catch (e) {
    message.error(formatError(e, "上报位置失败"), { duration: 0, closable: true });
  }
}
</script>

<template>
  <!-- 媒体源类型与操作行:下拉框 + 右侧上报操作(同行紧凑排布) -->
  <div class="media-control-bar">
    <div class="media-select-wrap">
      <label for="media-mode-select">媒体源类型</label>
      <select
        id="media-mode-select"
        v-model="mediaMode"
        :disabled="startDisabled"
        class="inp media-mode-select"
        @change="setMediaMode(mediaMode)"
      >
        <option value="file">视频文件（循环推送本地视频）</option>
        <option value="screen">电脑屏幕实时画面（ScreenCaptureKit 原生采集）</option>
        <option value="camera">电脑摄像头（含 iPhone 连续互通）</option>
        <option value="none">无媒体（仅信令联调）</option>
      </select>
    </div>
    <div class="media-actions-wrap">
      <button
        v-if="mediaMode === 'camera' || mediaMode === 'screen'"
        type="button"
        class="file-btn compact"
        :disabled="startDisabled || sourceLoading"
        @click="onRefreshLiveSources"
      >
        {{ sourceLoading ? '刷新中…' : '刷新设备' }}
      </button>
      <button
        v-if="mediaMode === 'camera' || mediaMode === 'screen'"
        type="button"
        class="file-btn compact primary"
        :disabled="!canProbeSource || probeState?.kind === 'loading'"
        @click="onProbeLiveSource"
      >
        {{ probeState?.kind === 'loading' ? '测试中…' : '测试采集' }}
      </button>
      <n-button size="small" tertiary :disabled="!canReport" @click="openAlarmModal">上报报警...</n-button>
      <n-button size="small" tertiary :disabled="!canReport" @click="firePosition">上报 GPS</n-button>
    </div>
  </div>

  <!-- 国标报警 (MANSCDP) 弹窗 -->
  <n-modal
    v-model:show="alarmModalOpen"
    preset="card"
    title="上报国标报警 (MANSCDP Alarm)"
    :mask-closable="false"
    style="width: 380px"
  >
    <div class="alarm-form">
      <div class="af-item">
        <span class="af-label">报警类型 (GB28181 A.2.5.3 规范)</span>
        <n-select
          :value="selectedAlarmType"
          size="small"
          aria-label="报警类型"
          :options="ALARM_TYPES"
          @update:value="onAlarmTypeChange"
        />
      </div>
      <div class="af-item">
        <span class="af-label">警情级别 (Priority)</span>
        <n-select
          v-model:value="selectedPriority"
          size="small"
          aria-label="警情级别"
          :options="ALARM_PRIORITIES"
        />
      </div>
      <div class="af-item">
        <span class="af-label">报警描述 (Description)</span>
        <n-input
          v-model:value="alarmDesc"
          size="small"
          aria-label="报警描述"
          placeholder="输入报警描述..."
        />
      </div>
      <div class="af-actions">
        <n-button size="small" type="primary" @click="doSendAlarm">立即发送</n-button>
        <n-button size="small" @click="alarmModalOpen = false">取消</n-button>
      </div>
    </div>
  </n-modal>

  <!-- 1. 视频文件模式:仅显示文件选择行,音视频参数隐藏 -->
  <div v-if="mediaMode === 'file'" class="source-config file-config-box">
    <div class="file-row">
      <input
        v-model="form.video_source"
        :disabled="startDisabled"
        class="inp"
        aria-label="视频文件路径"
        placeholder="选择 H.264/H.265 裸流或 MP4、MOV 等视频文件"
      />
      <button type="button" class="file-btn" :disabled="startDisabled" @click="onPickVideoSource">
        选择文件
      </button>
    </div>
    <div class="fg-hint">容器文件会在设备启动前转封装；有音频轨时自动转为 G.711A 后与视频复用。</div>
  </div>

  <!-- 2. 摄像头模式 -->
  <div v-else-if="mediaMode === 'camera'" class="source-config">
    <div class="source-capability">
      <span class="cap-badge" :class="liveCatalog.ffmpeg_available ? 'ok' : 'warn'">
        FFmpeg {{ liveCatalog.ffmpeg_available ? '可用' : '未找到' }}
      </span>
      <span>AVFoundation 摄像头与音频采集</span>
    </div>
    <div class="media-param-columns">
      <MediaVideoFields title="摄像头视频参数" :disabled="startDisabled || sourceLoading">
        <template #device>
          <div class="fg">
            <label for="camera-device-select">视频采集设备</label>
            <select
              id="camera-device-select"
              v-model.number="selectedCameraIndex"
              :disabled="startDisabled || sourceLoading"
              class="inp"
              @change="syncLiveSourceUri"
            >
              <option v-if="!liveCatalog.cameras.length && selectedCameraIndex === null" :value="null" disabled>
                未发现摄像头
              </option>
              <option
                v-if="selectedCameraIndex !== null && !liveCatalog.cameras.some(item => item.index === selectedCameraIndex)"
                :value="selectedCameraIndex"
                disabled
              >
                [{{ selectedCameraIndex }}] 当前摄像头不可用
              </option>
              <option v-for="camera in liveCatalog.cameras" :key="camera.index" :value="camera.index">
                [{{ camera.index }}] {{ camera.name }}
              </option>
            </select>
          </div>
        </template>
      </MediaVideoFields>

      <MediaAudioFields v-model:codec="selectedCameraAudioCodec" :disabled="startDisabled || sourceLoading">
        <template #device>
          <div class="fg">
            <label for="camera-mic-select">音频采集设备</label>
            <select
              id="camera-mic-select"
              v-model="selectedMicrophone"
              :disabled="startDisabled || sourceLoading"
              class="inp"
              @change="syncLiveSourceUri"
            >
              <option value="none">不采集音频（画面将没有声音）</option>
              <option
                v-if="selectedMicrophone !== 'none' && !liveCatalog.microphones.some(item => String(item.index) === selectedMicrophone)"
                :value="selectedMicrophone"
                disabled
              >
                [{{ selectedMicrophone }}] 当前音频设备不可用
              </option>
              <option v-for="mic in liveCatalog.microphones" :key="mic.index" :value="String(mic.index)">
                [{{ mic.index }}] {{ mic.name }}
              </option>
            </select>
          </div>
        </template>
      </MediaAudioFields>
    </div>

    <div v-if="screenProfileValidationError" class="source-notice error">
      {{ screenProfileValidationError }}
    </div>
    <div v-if="liveCatalog.avfoundation_error" class="source-notice error">
      {{ liveCatalog.avfoundation_error }}
    </div>
  </div>

  <!-- 3. 屏幕录制模式 -->
  <div v-else-if="mediaMode === 'screen'" class="source-config">
    <div class="source-capability">
      <span class="cap-badge ok">ScreenCaptureKit</span>
      <span>原生屏幕与系统音频采集</span>
    </div>
    <div class="media-param-columns">
      <MediaVideoFields title="屏幕画面参数" :disabled="startDisabled || sourceLoading">
        <template #device>
          <div class="fg">
            <label for="screen-device-select">显示器选择</label>
            <select
              id="screen-device-select"
              v-model.number="selectedDisplayId"
              :disabled="startDisabled || sourceLoading"
              class="inp"
              @change="syncLiveSourceUri"
            >
              <option v-if="!liveCatalog.screens.length && selectedDisplayId === null" :value="null" disabled>
                未发现可用显示器
              </option>
              <option
                v-if="selectedDisplayId !== null && !liveCatalog.screens.some(item => item.display_id === selectedDisplayId)"
                :value="selectedDisplayId"
                disabled
              >
                Display {{ selectedDisplayId }}（当前不可用）
              </option>
              <option v-for="screen in liveCatalog.screens" :key="screen.display_id" :value="screen.display_id">
                {{ screen.name }}
              </option>
            </select>
          </div>
        </template>
      </MediaVideoFields>

      <MediaAudioFields v-model:codec="selectedScreenAudioCodec" :disabled="startDisabled || sourceLoading">
        <template #device>
          <div class="fg">
            <label for="screen-audio-select">声音采集源</label>
            <select
              id="screen-audio-select"
              v-model="selectedScreenAudio"
              :disabled="startDisabled || sourceLoading"
              class="inp"
              @change="syncLiveSourceUri"
            >
              <option value="system">系统播放声音</option>
              <option
                v-if="selectedScreenAudio.startsWith('microphone:') && !liveCatalog.microphones.some(item => `microphone:${item.index}` === selectedScreenAudio)"
                :value="selectedScreenAudio"
                disabled
              >
                麦克风 {{ selectedScreenAudio.slice('microphone:'.length) }}（当前不可用）
              </option>
              <option
                v-for="mic in liveCatalog.microphones"
                :key="`screen-mic-${mic.index}`"
                :value="`microphone:${mic.index}`"
              >
                麦克风 · [{{ mic.index }}] {{ mic.name }}
              </option>
              <option value="none">不采集声音</option>
            </select>
          </div>
        </template>
      </MediaAudioFields>
    </div>

    <div v-if="screenProfileValidationError" class="source-notice error">
      {{ screenProfileValidationError }}
    </div>
    <div v-if="liveCatalog.screen_error" class="source-notice error">
      {{ liveCatalog.screen_error }}
      <span>请在“系统设置 → 隐私与安全性 → 屏幕与系统音频录制”中允许本应用。</span>
    </div>
  </div>

  <div v-if="probeState" class="probe-result" :class="probeState.kind">{{ probeState.message }}</div>
</template>

<style scoped>
.media-control-bar {
  display: flex; align-items: flex-end; justify-content: space-between;
  gap: 10px; margin-top: 8px; padding-top: 8px;
  border-top: 1px solid var(--border-default);
}
.media-select-wrap { display: flex; flex-direction: column; gap: 2px; flex: 1 1 260px; max-width: 360px; }
.media-select-wrap label { font-size: var(--fs-sm); color: var(--text-secondary); font-weight: 600; }
.media-mode-select { font-weight: 600; color: var(--text-primary); }
.media-actions-wrap { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; flex: 0 0 auto; }
.file-config-box { margin-top: 6px; }

.source-config {
  margin-top: 6px; padding: 7px 8px; border-radius: var(--radius-sm);
  border: 1px solid var(--border-default); background: var(--bg-card);
}
.media-param-columns { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 8px; margin-top: 4px; }
.source-capability {
  display: flex; align-items: center; gap: 6px; margin-bottom: 5px;
  color: var(--text-secondary); font-size: var(--fs-xs);
}
.cap-badge { padding: 1px 5px; border-radius: 6px; font-size: var(--fs-xs); font-weight: 650; }
.cap-badge.ok { color: var(--success); background: color-mix(in srgb, var(--success) 12%, transparent); }
.cap-badge.warn { color: var(--warning); background: color-mix(in srgb, var(--warning) 12%, transparent); }
.file-row { display: flex; gap: 6px; align-items: stretch; }
.file-row .inp { flex: 1 1 auto; }
.probe-result { margin-top: 5px; padding: 4px 6px; border-radius: 4px; font-size: var(--fs-xs); line-height: 1.3; }
.probe-result.loading { color: var(--accent); background: var(--accent-dim); }
.probe-result.success { color: var(--success); background: color-mix(in srgb, var(--success) 10%, transparent); }
.probe-result.warning { color: var(--warning); background: color-mix(in srgb, var(--warning) 10%, transparent); }
.probe-result.error { color: var(--error); background: color-mix(in srgb, var(--error) 9%, transparent); }

@media (max-width: 760px) {
  .media-param-columns { grid-template-columns: 1fr; }
}

.alarm-form { display: flex; flex-direction: column; gap: 10px; }
.af-item { display: flex; flex-direction: column; gap: 4px; }
.af-item .af-label { font-size: var(--fs-xs); color: var(--text-secondary); font-weight: 600; }
.af-actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 4px; }
</style>
