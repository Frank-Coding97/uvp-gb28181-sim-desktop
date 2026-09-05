<script setup lang="ts">
import { computed, ref, useId } from "vue";
import { NButton, NInput, NModal, NSelect, useMessage } from "naive-ui";
import { invoke } from "@tauri-apps/api/core";
import { useDevice } from "../device";
import { useLiveSources } from "../composables/useLiveSources";
import MediaSourcePanel from "./MediaSourcePanel.vue";
import { formatError } from "../lib/ipcError";

const message = useMessage();
const { form, canReport } = useDevice();
const { mediaMode } = useLiveSources();

const idGbVersionLabel = useId();
const videoCollapsed = ref(true);

const mediaModeSummary = computed(() => {
  switch (mediaMode.value) {
    case "file":
      return form.value.video_source ? "视频文件" : "文件源(未选)";
    case "camera":
      return "摄像头采集";
    case "screen":
      return "屏幕画面";
    case "none":
      return "无媒体(仅信令)";
    default:
      return "媒体源";
  }
});

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
  <div class="glass-card console-panel identity-panel">
    <div class="panel-head-tight">
      <div class="head-left">
        <div class="panel-title">设备身份与国标参数</div>
        <span class="param-badge">GB/T 28181</span>
      </div>
      <div class="head-actions">
        <n-button size="tiny" secondary :disabled="!canReport" @click="openAlarmModal">上报报警...</n-button>
        <n-button size="tiny" secondary :disabled="!canReport" @click="firePosition">上报 GPS</n-button>
      </div>
    </div>

    <!-- 国标基础与扩展参数:自适应多列紧凑网格 -->
    <div class="identity-grid">
      <div class="fg">
        <span :id="idGbVersionLabel" class="fg-label">国标版本</span>
        <div class="seg" role="group" :aria-labelledby="idGbVersionLabel">
          <button
            type="button"
            :class="{ on: form.gb_version === '2022' }"
            :aria-pressed="form.gb_version === '2022'"
            @click="form.gb_version = '2022'"
          >
            GB/T 2022
          </button>
          <button
            type="button"
            :class="{ on: form.gb_version === '2016' }"
            :aria-pressed="form.gb_version === '2016'"
            @click="form.gb_version = '2016'"
          >
            GB/T 2016
          </button>
        </div>
      </div>
      <div class="fg">
        <label for="device-id-input">设备编号</label>
        <input id="device-id-input" v-model="form.device_id" class="inp" placeholder="20 位国标 ID" />
      </div>
      <div class="fg">
        <label for="channel-name-input">通道名称</label>
        <input id="channel-name-input" v-model="form.channel_name" class="inp" placeholder="通道名称" />
      </div>
      <div class="fg">
        <label for="catalog-template-select">目录模板</label>
        <select id="catalog-template-select" v-model="form.catalog_template" class="inp">
          <option value="">单通道(默认)</option>
          <option value="nvr-8ch">8 通道 NVR</option>
          <option value="civil-3x2">跨区划(3 区 × 2 通道)</option>
          <option value="large-16ch">16 通道大型监控</option>
        </select>
      </div>
      <div class="fg">
        <label for="manufacturer-input">设备厂商</label>
        <input id="manufacturer-input" v-model="form.manufacturer" class="inp" placeholder="厂商名称" />
      </div>
      <div class="fg">
        <label for="model-input">设备型号</label>
        <input id="model-input" v-model="form.model" class="inp" placeholder="产品型号" />
      </div>
      <div class="fg">
        <label for="firmware-input">固件版本</label>
        <input id="firmware-input" v-model="form.firmware" class="inp" placeholder="固件版本号" />
      </div>
      <div class="fg">
        <label for="expires-input">注册有效期 (s)</label>
        <input
          id="expires-input"
          v-model.number="form.expires"
          type="number"
          min="60"
          max="86400"
          step="60"
          class="inp"
          placeholder="3600"
        />
      </div>
      <div class="fg">
        <label for="heartbeat-input">心跳周期 (s)</label>
        <input
          id="heartbeat-input"
          v-model.number="form.heartbeat_interval"
          type="number"
          min="5"
          max="300"
          step="5"
          class="inp"
          placeholder="60"
        />
      </div>
      <div class="fg">
        <label for="heartbeat-timeout-input">超时次数</label>
        <input
          id="heartbeat-timeout-input"
          v-model.number="form.max_heartbeat_timeout"
          type="number"
          min="1"
          max="10"
          step="1"
          class="inp"
          placeholder="3"
        />
      </div>
    </div>

    <!-- 视频参数设置:折叠面板 -->
    <div class="video-config-collapsible">
      <button
        type="button"
        class="vcc-header"
        :aria-expanded="!videoCollapsed"
        @click="videoCollapsed = !videoCollapsed"
      >
        <div class="vcc-left">
          <span class="vcc-arrow" :class="{ rotated: !videoCollapsed }">▶</span>
          <span class="vcc-title">视频参数设置</span>
          <span class="vcc-tag">{{ mediaModeSummary }}</span>
        </div>
        <span class="vcc-toggle">{{ videoCollapsed ? '展开' : '收起' }}</span>
      </button>
      <div v-show="!videoCollapsed" class="vcc-body">
        <MediaSourcePanel />
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
  </div>
</template>

<style scoped>
.identity-panel {
  box-sizing: border-box;
  display: flex;
  flex-direction: column;
}
.panel-head-tight {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 8px;
}
.head-left {
  display: flex;
  align-items: center;
  gap: 8px;
}
.head-actions {
  display: flex;
  align-items: center;
  gap: 6px;
}
.identity-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
  gap: 6px 10px;
}
.identity-grid .fg {
  margin-bottom: 0;
}

/* 视频参数折叠条 */
.video-config-collapsible {
  margin-top: 8px;
  border: 1px solid var(--border-default);
  border-radius: var(--radius-xs);
  background: var(--bg-surface);
  overflow: hidden;
}
.vcc-header {
  width: 100%;
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 6px 8px;
  background: transparent;
  border: none;
  cursor: pointer;
  text-align: left;
  transition: background 0.15s;
}
.vcc-header:hover {
  background: rgba(120, 140, 170, 0.08);
}
.vcc-left {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
}
.vcc-arrow {
  font-size: 9px;
  color: var(--text-tertiary);
  transition: transform 0.2s;
  display: inline-block;
}
.vcc-arrow.rotated {
  transform: rotate(90deg);
}
.vcc-title {
  font-size: 11.5px;
  font-weight: 700;
  color: var(--text-primary);
}
.vcc-tag {
  font-size: 10px;
  padding: 1px 6px;
  border-radius: 999px;
  background: rgba(56, 132, 255, 0.1);
  color: var(--accent-deep, #1e63dc);
  font-weight: 600;
  white-space: nowrap;
}
.vcc-toggle {
  font-size: 11px;
  color: var(--accent);
  font-weight: 600;
  flex-shrink: 0;
}
.vcc-body {
  padding: 6px 8px 8px;
  border-top: 1px dashed var(--border-default);
  background: var(--bg-elevated);
}

.alarm-form {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.af-item {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.af-item .af-label {
  font-size: var(--fs-xs);
  color: var(--text-secondary);
  font-weight: 600;
}
.af-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 4px;
}

@media (max-width: 760px) {
  .identity-grid {
    grid-template-columns: 1fr;
  }
}
</style>
