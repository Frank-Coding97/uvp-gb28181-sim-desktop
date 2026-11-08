<script setup lang="ts">
// 桌面端「模拟器中心」。
//
// 结构：左侧 3D 舞台（只表达设备姿态，不接视频流）+ 右侧四页控制台。
// 功能口径对齐移动端模拟中心：云台/定位/图像/设备；除云台页本机手操外全部只读回显。
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import PtzCamera3D from "../components/PtzCamera3D.vue";
import PtzPad from "../components/PtzPad.vue";
import HoldKey from "../components/HoldKey.vue";
import { useDevice } from "../device";
import { tabForSummary, useSimulatorCenter, type SimTab } from "../simulator/useSimulatorCenter";

const sim = useSimulatorCenter();
const { deviceLive, reconcile } = useDevice();

const activeTab = ref<SimTab>("ptz");
const tabBadges = ref<SimTab[]>([]);
const showFov = ref(true);
const autoFollow = ref(true);

const tabs: Array<{ key: SimTab; label: string; hint: string }> = [
  { key: "ptz", label: "云台", hint: "本机手操" },
  { key: "position", label: "定位", hint: "预置位/巡航" },
  { key: "image", label: "图像", hint: "遮挡/镜像" },
  { key: "device", label: "设备", hint: "状态/存储" },
];

const normalizedPan = sim.normalizedPan;
const pose = sim.pose;

const flashing = computed(() => sim.nowMs.value - sim.snapshotAt.value < 170);
const formatRecent = computed(() => sim.nowMs.value - sim.formatAt.value < 30_000);
const dragZoomRecent = computed(() => sim.nowMs.value - sim.dragZoomAt.value < 3_000);
const activePresetLabel = computed(() => {
  if (sim.activePreset.value === null) return "—";
  const preset = sim.presets.value.find((item) => item.id === sim.activePreset.value);
  return preset?.name ?? `预置位 ${sim.activePreset.value}`;
});
const targetLabel = computed(() => {
  if (!sim.targetActive.value) return "未在跟踪";
  const mode = sim.targetMode.value ? ` · ${sim.targetMode.value}` : "";
  return sim.targetBox.value
    ? `平台已下发${mode} · 框选区域已同步`
    : `平台已下发${mode}（框选区域待后端）`;
});
const targetBoxStyle = computed(() => {
  const box = sim.targetBox.value;
  if (!box) return {};
  return {
    left: `${box.left * 100}%`,
    top: `${box.top * 100}%`,
    width: `${box.width * 100}%`,
    height: `${box.height * 100}%`,
  };
});
const dragZoomStyle = computed(() => {
  const box = sim.dragZoomBox.value;
  if (!box) return {};
  return {
    left: `${box.left * 100}%`,
    top: `${box.top * 100}%`,
    width: `${box.width * 100}%`,
    height: `${box.height * 100}%`,
  };
});
const osdTimeText = computed(() => {
  const date = new Date(sim.nowMs.value);
  const pad = (value: number) => String(value).padStart(2, "0");
  const ymd = `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
  const hms = `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
  if (sim.osd.timeType === 1) {
    return `${date.getFullYear()}年${pad(date.getMonth() + 1)}月${pad(date.getDate())}日 ${hms}`;
  }
  return `${ymd} ${hms}`;
});
function osdStyle(x: number, y: number) {
  const width = sim.osd.length || 1920;
  const height = sim.osd.width || 1080;
  return {
    left: `${Math.min(0.98, Math.max(0, x / width)) * 100}%`,
    top: `${Math.min(0.98, Math.max(0, y / height)) * 100}%`,
  };
}

const lampItems = computed(() => [
  { key: "recording", label: "录像", active: sim.recording.value, color: "var(--error)" },
  { key: "guarded", label: "布防", active: sim.guarded.value, color: "var(--success)" },
  { key: "alarming", label: "报警", active: sim.alarming.value, color: "var(--warning)" },
  { key: "wiper", label: "雨刷", active: sim.wiper.value, color: "var(--cyan)" },
]);

const mirrorOptions = ["不启用", "左右", "上下", "中心"];

function pct(value: number): number {
  return Math.round(Math.min(1, Math.max(0, value)) * 100);
}

function timeText(ts: number): string {
  if (!ts) return "—";
  return new Date(ts).toLocaleTimeString("zh-CN", { hour12: false });
}

function onPad(dPan: number, dTilt: number) {
  sim.adjustPtz(dPan, dTilt, 0);
}

function selectTab(key: SimTab) {
  activeTab.value = key;
  tabBadges.value = tabBadges.value.filter((item) => item !== key);
}

function onZoom(direction: 1 | -1, repeat: boolean) {
  sim.adjustPtz(0, 0, direction * (repeat ? 0.15 : 0.5));
}

function onIris(direction: 1 | -1, repeat: boolean) {
  sim.adjustLens(0, direction * (repeat ? 0.02 : 0.05));
}

function onFocus(direction: 1 | -1, repeat: boolean) {
  sim.adjustLens(direction * (repeat ? 0.02 : 0.05), 0);
}

function storageCapacity(mb: number): string {
  if (mb < 1024) return `${mb}MB`;
  return `${(mb / 1024).toFixed(1)}GB`;
}

function storageStatus(card: { status: string; freeMb: number; progress: number | null }): string {
  if (card.status === "格式化中") return card.progress !== null ? `进度 ${card.progress}%` : "格式化中";
  if (card.status === "正常" || card.status === "空闲") return `剩余 ${storageCapacity(card.freeMb)}`;
  return "—";
}

watch(
  () => sim.lastCommand.value?.at,
  () => {
    const command = sim.lastCommand.value;
    if (!command) return;
    const target = tabForSummary(command.summary);
    if (target && target !== activeTab.value) {
      if (autoFollow.value) selectTab(target);
      else if (!tabBadges.value.includes(target)) tabBadges.value = [...tabBadges.value, target];
    }
  },
);

onMounted(async () => {
  sim.start();
  if (import.meta.env.DEV) {
    // 开发期调试句柄：便于在浏览器/自动化里注入平台事件验证状态机。
    (window as unknown as { __simulatorCenter?: typeof sim }).__simulatorCenter = sim;
  }
  await reconcile();
});

onUnmounted(() => {
  sim.stop();
});
</script>

<template>
  <div class="sim-page">
    <header class="page-header">
      <div class="title-block">
        <div class="page-title">模拟器中心</div>
        <div class="page-sub">3D 设备姿态 + 平台控制回显；本页不主动控制平台</div>
      </div>

      <div class="headline" :style="{ '--dot': sim.headline.value.color }">
        <i />
        <span>{{ sim.headline.value.text }}</span>
      </div>

      <div class="header-right">
        <label class="follow-toggle">
          <input v-model="autoFollow" type="checkbox" />
          平台命令自动切页
        </label>
        <span class="device-pill" :class="{ online: deviceLive }">
          {{ deviceLive ? "设备运行中" : "设备未运行" }}
        </span>
      </div>
    </header>

    <div class="sim-layout">
      <!-- 左：3D 舞台 -->
      <section class="glass-card stage-card">
        <div
          class="stage"
          :class="{ active: sim.ptzActive.value, seeking: sim.seeking.value, guarded: sim.guarded.value }"
        >
          <div class="stage-grid" />
          <div class="stage-glow" />

          <div
            v-if="sim.osd.configured && (sim.osd.timeShow || (sim.osd.show && sim.osd.items.length))"
            class="stage-osd"
          >
            <div v-if="sim.osd.timeShow" class="osd-line" :style="osdStyle(sim.osd.timeX, sim.osd.timeY)">
              {{ osdTimeText }}
            </div>
            <template v-if="sim.osd.show">
              <div
                v-for="(item, index) in sim.osd.items"
                :key="index"
                class="osd-line"
                :style="osdStyle(item.x, item.y)"
              >
                {{ item.text }}
              </div>
            </template>
          </div>

          <div class="stage-caption">
            <i />
            <b>PTZ LIVE</b>
            <span>{{ sim.headline.value.text }}</span>
          </div>

          <div class="compass">
            <div class="dial" :style="{ transform: `rotate(${(-normalizedPan).toFixed(1)}deg)` }">
              <span>N</span><span>E</span><span>S</span><span>W</span>
            </div>
            <b>{{ normalizedPan.toFixed(0).padStart(3, "0") }}°</b>
          </div>

          <PtzCamera3D
            :pan="pose.pan"
            :tilt="pose.tilt"
            :zoom="pose.zoom"
            :focus="pose.focus"
            :iris="pose.iris"
            :zoom-max="20"
            :active="sim.ptzActive.value"
            :seeking="sim.seeking.value"
            :online="deviceLive"
            :show-fov="showFov"
          />

          <div v-if="sim.guarded.value" class="guard-halo" />
          <div v-if="sim.wiper.value" class="wiper-line" />

          <div v-if="flashing" class="snapshot-flash" />

          <div class="stage-tags">
            <span v-if="sim.recording.value" class="tag recording">REC</span>
            <span v-if="sim.guarded.value" class="tag guarded">布防</span>
            <span v-if="sim.targetActive.value" class="tag target">目标跟踪</span>
            <span v-if="dragZoomRecent" class="tag dragzoom">拉框联动</span>
          </div>

          <div v-if="sim.targetBox.value" class="target-box" :style="targetBoxStyle">
            <span>目标跟踪{{ sim.targetMode.value ? ` · ${sim.targetMode.value}` : "" }}</span>
          </div>

          <div v-if="dragZoomRecent && sim.dragZoomBox.value" class="drag-zoom-box" :style="dragZoomStyle">
            <span>拉框联动</span>
          </div>

          <button class="fov-toggle" :class="{ on: showFov }" type="button" @click="showFov = !showFov">
            视场锥
          </button>

          <div class="stage-foot">
            <span>最近命令 · {{ sim.lastCommand.value ? timeText(sim.lastCommand.value.at) : "—" }}</span>
            <b>{{ sim.lastCommand.value?.summary ?? "尚未收到平台命令" }}</b>
          </div>

          <div v-if="!deviceLive" class="offline-hint">
            设备未运行 · 本机手操可用；平台命令需先在首页注册
          </div>
        </div>
      </section>

      <!-- 右：四页控制台 -->
      <aside class="glass-card console">
        <div class="console-head">
          <div>
            <div class="panel-title">平台控制 HUD</div>
            <div class="panel-sub">平台命令会自动切到对应页</div>
          </div>
          <div class="active-preset">当前预置位 <b>{{ activePresetLabel }}</b></div>
        </div>

        <div class="tab-row">
          <button
            v-for="tab in tabs"
            :key="tab.key"
            type="button"
            class="tab"
            :class="{ active: activeTab === tab.key }"
            @click="selectTab(tab.key)"
          >
            <b>{{ tab.label }}</b>
            <small>{{ tab.hint }}</small>
            <i v-if="tabBadges.includes(tab.key)" class="tab-badge" />
          </button>
        </div>

        <div class="console-body">
          <!-- 云台 -->
          <template v-if="activeTab === 'ptz'">
            <div class="pose-strip">
              <div class="pose-item">
                <span>水平</span>
                <b>{{ pose.pan.toFixed(0) }}°</b>
              </div>
              <div class="pose-divider" />
              <div class="pose-item">
                <span>俯仰</span>
                <b>{{ pose.tilt.toFixed(0) }}°</b>
              </div>
              <div class="pose-divider" />
              <div class="pose-item">
                <span>变焦</span>
                <b>{{ pose.zoom.toFixed(1) }}×</b>
              </div>
            </div>

            <div class="ptz-workbench">
              <div class="ptz-left">
                <div class="control-card zoom-card">
                  <div class="card-label">变焦</div>
                  <div class="big-readout">{{ pose.zoom.toFixed(1) }}<small>×</small></div>
                  <div class="key-row">
                    <HoldKey label="−" aria-label="缩小" @trigger="onZoom(-1, $event)" />
                    <HoldKey label="+" aria-label="放大" @trigger="onZoom(1, $event)" />
                  </div>
                </div>
              </div>

              <PtzPad @adjust="onPad" />

              <div class="ptz-right lens-column">
                <div class="control-card lens-card">
                  <div class="lens-head"><span>光圈</span><b>{{ pct(pose.iris) }}%</b></div>
                  <div class="meter"><i :style="{ width: `${pct(pose.iris)}%` }" /></div>
                  <div class="key-row">
                    <HoldKey class="lens-key" tone="lens" label="−" aria-label="光圈缩小" @trigger="onIris(-1, $event)" />
                    <HoldKey class="lens-key" tone="lens" label="+" aria-label="光圈放大" @trigger="onIris(1, $event)" />
                  </div>
                </div>
                <div class="control-card lens-card">
                  <div class="lens-head"><span>聚焦</span><b>{{ pct(pose.focus) }}%</b></div>
                  <div class="meter"><i :style="{ width: `${pct(pose.focus)}%` }" /></div>
                  <div class="key-row">
                    <HoldKey class="lens-key" tone="lens" label="近" aria-label="聚焦近" @trigger="onFocus(-1, $event)" />
                    <HoldKey class="lens-key" tone="lens" label="远" aria-label="聚焦远" @trigger="onFocus(1, $event)" />
                  </div>
                </div>
              </div>
            </div>

            <div class="ptz-status-bar">
              <span class="ptz-status-item"><i class="ptz-status-dot" :class="{ hot: sim.ptzActive.value || sim.seeking.value }" />{{ sim.headline.value.text }}</span>
              <span class="ptz-status-sep" />
              <span class="ptz-status-item">{{ sim.directionText.value !== '—' ? sim.directionText.value : '待命' }}</span>
              <span class="ptz-status-sep" />
              <span class="ptz-status-item">{{ sim.localPushing.value ? "本机调整中" : "平台控制" }}</span>
            </div>
          </template>

          <!-- 定位 -->
          <template v-else-if="activeTab === 'position'">
            <section class="block">
              <div class="block-head"><span>预置位</span><small>{{ sim.presets.value.length ? `${sim.presets.value.length} 个` : "平台未下发" }}</small></div>
              <div v-if="sim.presets.value.length" class="preset-chips">
                <span
                  v-for="preset in sim.presets.value"
                  :key="preset.id"
                  class="preset-chip"
                  :class="{ current: sim.activePreset.value === preset.id }"
                >
                  P{{ preset.id }} <small>{{ preset.name }}</small>
                </span>
              </div>
              <div v-else class="empty">未配置 · 等待平台下发</div>
            </section>

            <section class="block">
              <div class="block-head"><span>看守位</span><small>平台下发</small></div>
              <div v-if="sim.home.configured" class="home-card">
                <span class="dot" :class="{ on: sim.home.enabled }" />
                <b>{{ sim.home.enabled ? "已启用" : "已关闭" }}</b>
                <span v-if="sim.home.presetIndex !== null">指向 P{{ sim.home.presetIndex }}</span>
                <span v-if="sim.home.resetTime !== null">自动归位 {{ sim.home.resetTime }}s</span>
                <span v-else class="muted">字段待后端补齐</span>
              </div>
              <div v-else class="empty">未配置 · 等待平台下发</div>
            </section>

            <section class="block">
              <div class="block-head"><span>巡航轨迹</span><small>{{ sim.cruiseTracks.value.length ? `${sim.cruiseTracks.value.length} 条` : "平台未下发" }}</small></div>
              <div v-if="sim.cruiseTracks.value.length" class="track-list">
                <div v-for="track in sim.cruiseTracks.value" :key="track.id" class="track-row" :class="{ active: track.active }">
                  <b>#{{ track.id }}{{ track.active ? " ▶" : "" }}</b>
                  <div>
                    <div>{{ track.points.length ? track.points.map((p) => `P${p}`).join(" → ") : "暂无点位" }}</div>
                    <small>{{ track.dwell !== null ? `每点停留 ${track.dwell}s` : "停留未下发" }} · {{ track.speed !== null ? `速度 ${track.speed}` : "速度未下发" }}</small>
                  </div>
                </div>
              </div>
              <div v-else class="empty">未配置 · 等待平台下发</div>
            </section>

            <section class="block">
              <div class="block-head"><span>自动扫描</span><small>{{ sim.scanGroups.value.length ? `${sim.scanGroups.value.length} 组` : "平台未下发" }}</small></div>
              <div v-if="sim.scanGroups.value.length" class="scan-list">
                <span v-for="group in sim.scanGroups.value" :key="group.id" class="scan-chip" :class="{ active: group.active }">
                  扫描#{{ group.id }}{{ group.active ? " ▶" : "" }} {{ group.left ?? "未设" }}↔{{ group.right ?? "未设" }} · {{ group.speed ?? "速度未下发" }}
                </span>
              </div>
              <div v-else class="empty">平台未下发扫描组</div>
            </section>
          </template>

          <!-- 图像 -->
          <template v-else-if="activeTab === 'image'">
            <section class="block">
              <div class="block-head"><span>画面遮挡</span><small>GB-2022 A.2.1.17</small></div>
              <div class="state-row">
                <span class="dot" :class="{ on: sim.mask.on }" />
                <b>{{ sim.mask.configured ? (sim.mask.on ? "已启用" : "已停用") : "平台未配置" }}</b>
                <span v-if="sim.mask.on && sim.mask.regions.length" class="muted">{{ sim.mask.regions.length }} 个区域</span>
                <span v-else-if="sim.mask.on" class="muted">已启用 · 无区域</span>
              </div>
              <div v-if="sim.mask.on && sim.mask.regions.length" class="mask-list">
                <div v-for="region in sim.mask.regions" :key="region.seq" class="mask-row">
                  <b>区域 {{ region.seq }}</b>
                  <span>{{ region.left }},{{ region.top }} → {{ region.right }},{{ region.bottom }}</span>
                </div>
              </div>
            </section>

            <section class="block">
              <div class="block-head"><span>画面镜像</span><small>A.2.1.23</small></div>
              <div class="mirror-row">
                <span
                  v-for="(label, index) in mirrorOptions"
                  :key="label"
                  class="mirror-chip"
                  :class="{ selected: sim.mirror.configured && sim.mirror.mode === index }"
                >
                  {{ label }}
                </span>
              </div>
              <div class="hint-line">{{ sim.mirror.configured ? "平台已下发" : "平台未配置，四张卡均不点亮" }}</div>
            </section>

            <section class="block">
              <div class="block-head"><span>画面事件</span><small>平台下发回显</small></div>
              <div class="event-list">
                <div class="event-row" :class="{ hot: sim.keyframeActive.value }">
                  <span>请求关键帧</span>
                  <b>{{ sim.keyframeActive.value ? "已收到平台请求" : "等待平台请求" }}</b>
                </div>
                <div class="event-row" :class="{ hot: sim.nowMs.value - sim.snapshotAt.value < 3000 }">
                  <span>抓拍</span>
                  <b>{{ sim.snapshotAt.value ? timeText(sim.snapshotAt.value) : "尚未收到抓拍" }}</b>
                </div>
                <div class="event-row" :class="{ hot: sim.targetActive.value }">
                  <span>目标跟踪</span>
                  <b>{{ targetLabel }}</b>
                </div>
                <div class="event-row" :class="{ hot: dragZoomRecent }">
                  <span>拉框联动</span>
                  <b>{{ dragZoomRecent ? "最近 3 秒收到" : "暂无" }}</b>
                </div>
              </div>
            </section>

            <section class="block">
              <div class="block-head"><span>前端 OSD</span><small>平台配置回显</small></div>
              <div class="event-list">
                <div class="event-row"><span>OSD 信息</span><b>{{ sim.osd.configured ? (sim.osd.show ? "开" : "关") : "未配置" }}</b></div>
                <div class="event-row"><span>时间水印</span><b>{{ sim.osd.configured ? (sim.osd.timeShow ? "开" : "关") : "未配置" }}</b></div>
              </div>
            </section>
          </template>

          <!-- 设备 -->
          <template v-else>
            <div class="lamp-grid">
              <div
                v-for="lamp in lampItems"
                :key="lamp.key"
                class="lamp-card"
                :class="{ on: lamp.active }"
                :style="{ '--lamp': lamp.color }"
              >
                <i />
                <div><b>{{ lamp.label }}</b><small>{{ lamp.active ? "已开启" : "未开启" }}</small></div>
              </div>
            </div>

            <section class="block">
              <div class="block-head"><span>存储卡</span><small>{{ sim.storage.queried ? `${sim.storage.cards.length} 张` : "平台查询触发" }}</small></div>
              <div v-if="!sim.storage.queried" class="empty">等待平台下发存储卡查询</div>
              <div v-else-if="sim.storage.cards.length === 0" class="empty">未安装存储卡 (SumNum=0)</div>
              <div v-else class="storage-list">
                <div v-for="card in sim.storage.cards" :key="card.id" class="storage-row">
                  <i />
                  <b>{{ card.name }}</b>
                  <span>{{ storageCapacity(card.capacityMb) }}</span>
                  <span>{{ storageStatus(card) }}</span>
                  <span>{{ card.status }}</span>
                </div>
              </div>
            </section>

            <section class="block">
              <div class="block-head"><span>设备维护</span><small>平台命令回显</small></div>
              <div class="event-list">
                <div class="event-row" :class="{ hot: sim.rebooting.value }">
                  <span>远程重启</span>
                  <b>{{ sim.rebooting.value ? "正在重启" : "暂无重启任务" }}</b>
                </div>
                <div v-if="sim.upgrade.value" class="upgrade-card">
                  <div class="upgrade-head">
                    <span>在线升级{{ sim.upgrade.value.firmware ? ` v${sim.upgrade.value.firmware}` : "" }}</span>
                    <b>{{ sim.upgrade.value.result }}</b>
                    <strong>{{ sim.upgrade.value.percent }}%</strong>
                  </div>
                  <div class="meter"><i :style="{ width: `${sim.upgrade.value.percent}%` }" /></div>
                </div>
                <div class="event-row" :class="{ hot: formatRecent }">
                  <span>格式化 SD 卡</span>
                  <b>{{ formatRecent ? `收到请求 · ${timeText(sim.formatAt.value)}` : "暂无请求" }}</b>
                </div>
                <div class="event-row" :class="{ hot: sim.lastCommand.value?.summary === '在线升级' }">
                  <span>升级请求</span>
                  <b>{{ sim.lastCommand.value?.summary === "在线升级" ? "平台已下发" : "暂无请求" }}</b>
                </div>
              </div>
            </section>

            <section class="block">
              <div class="block-head"><span>最近平台命令</span><small>{{ sim.lastCommand.value ? timeText(sim.lastCommand.value.at) : "—" }}</small></div>
              <div class="last-command">{{ sim.lastCommand.value?.summary ?? "尚未收到平台命令" }}</div>
            </section>
          </template>
        </div>
      </aside>
    </div>
  </div>
</template>

<style scoped>
.sim-page { width: min(1540px, 100%); margin: 0 auto; flex: 1; min-height: 0; display: flex; flex-direction: column; }
.page-header {
  display: grid;
  grid-template-columns: minmax(280px, 1fr) auto auto;
  align-items: center;
  gap: 20px;
  margin-bottom: 16px;
}
.headline {
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 7px 12px;
  border: 1px solid var(--border-default);
  border-radius: 999px;
  color: var(--text-secondary);
  background: rgba(255, 255, 255, 0.5);
  font-size: 12px;
  font-weight: 600;
  white-space: nowrap;
}
.headline i { width: 7px; height: 7px; border-radius: 50%; background: var(--dot); box-shadow: 0 0 10px var(--dot); }
.header-right { display: flex; align-items: center; gap: 12px; }
.follow-toggle { display: flex; align-items: center; gap: 5px; color: var(--text-tertiary); font-size: 11px; white-space: nowrap; cursor: pointer; }
.follow-toggle input { accent-color: var(--accent); }
.device-pill { padding: 6px 11px; border: 1px solid var(--border-default); border-radius: 999px; color: var(--text-tertiary); background: rgba(255, 255, 255, 0.48); font-size: 12px; white-space: nowrap; }
.device-pill.online { border-color: rgba(24, 160, 88, 0.25); color: var(--success); background: rgba(24, 160, 88, 0.08); }

.sim-layout { display: grid; grid-template-columns: minmax(0, 1.55fr) minmax(390px, 0.85fr); grid-template-rows: minmax(0, 1fr); gap: 16px; align-items: stretch; flex: 1; min-height: 0; }
.stage-card { min-width: 0; padding: 14px; }
.stage {
  position: relative;
  height: 100%;
  min-height: 648px;
  overflow: hidden;
  border-radius: 12px;
  background: radial-gradient(circle at 50% 42%, #173451 0%, #0a1a2b 48%, #06101a 80%);
  border: 1px solid rgba(163, 196, 235, 0.12);
}
.stage-grid { position: absolute; inset: 0; opacity: .15; background-image: linear-gradient(rgba(108,162,221,.16) 1px, transparent 1px), linear-gradient(90deg, rgba(108,162,221,.16) 1px, transparent 1px); background-size: 44px 44px; mask-image: radial-gradient(circle at 50% 48%, #000 0%, transparent 74%); }
.stage-glow { position: absolute; inset: 0; background: radial-gradient(460px 300px at 58% 38%, rgba(56,132,255,.16), transparent 72%), radial-gradient(380px 220px at 28% 76%, rgba(0,185,214,.08), transparent 72%); }
.stage-osd { position: absolute; z-index: 5; inset: 0; overflow: hidden; pointer-events: none; }
.stage-osd .osd-line { position: absolute; color: rgba(245, 250, 255, 0.88); font: 500 10px/1.4 "SF Mono", "Consolas", monospace; text-shadow: 0 1px 2px rgba(0, 0, 0, 0.9), 0 0 5px rgba(0, 0, 0, 0.55); white-space: nowrap; }
.stage-caption { position: absolute; z-index: 7; top: 16px; left: 50%; transform: translateX(-50%); display: flex; align-items: center; gap: 8px; color: rgba(218,232,251,.62); font-size: 10px; letter-spacing: .08em; }
.stage-caption i { width: 7px; height: 7px; border-radius: 50%; background: #7b8ca3; }
.stage.active .stage-caption i, .stage.seeking .stage-caption i { background: #45d483; box-shadow: 0 0 10px #45d483; }
.stage-caption b { color: #fff; font-size: 10px; }
.stage-caption span { color: rgba(218,232,251,.82); letter-spacing: 0; font-size: 11px; }
.compass { position: absolute; z-index: 5; top: 16px; right: 18px; width: 92px; height: 92px; border: 1px solid rgba(163,196,235,.24); border-radius: 50%; background: rgba(5,16,29,.18); }
.dial { position: absolute; inset: 8px; transition: transform .1s linear; }
.dial span { position: absolute; color: rgba(212,230,251,.55); font-size: 10px; }
.dial span:nth-child(1) { top: 0; left: 50%; transform: translateX(-50%); }
.dial span:nth-child(2) { top: 50%; right: 0; transform: translateY(-50%); }
.dial span:nth-child(3) { bottom: 0; left: 50%; transform: translateX(-50%); }
.dial span:nth-child(4) { top: 50%; left: 0; transform: translateY(-50%); }
.compass > b { position: absolute; inset: 0; display: grid; place-items: center; color: #fff; font: 700 17px "SF Mono", monospace; }
.stage-tags { position: absolute; z-index: 6; top: 118px; left: 18px; display: flex; flex-wrap: wrap; gap: 6px; max-width: 220px; }
.tag { padding: 3px 7px; border-radius: 5px; font-size: 9px; font-weight: 750; letter-spacing: .06em; }
.tag.recording { color: #fff; background: rgba(229,72,92,.9); }
.tag.guarded { color: #e9fff5; background: rgba(8,179,136,.8); }
.tag.target { color: #2a1a00; background: rgba(245,170,40,.9); }
.tag.dragzoom { color: #061426; background: rgba(90,190,255,.85); }
.target-box { position: absolute; z-index: 6; border: 1.5px solid rgba(245, 170, 40, 0.9); border-radius: 2px; background: rgba(245, 170, 40, 0.06); box-shadow: 0 0 14px rgba(245, 170, 40, 0.22); pointer-events: none; }
.target-box::before, .target-box::after { content: ""; position: absolute; width: 12px; height: 12px; border-color: #f5aa28; }
.target-box::before { top: -2px; left: -2px; border-top: 2px solid; border-left: 2px solid; }
.target-box::after { right: -2px; bottom: -2px; border-right: 2px solid; border-bottom: 2px solid; }
.target-box span { position: absolute; left: -1px; top: -20px; padding: 2px 6px; border-radius: 4px; color: #2a1a00; background: rgba(245, 170, 40, 0.92); font-size: 9px; font-weight: 700; white-space: nowrap; }
.drag-zoom-box { position: absolute; z-index: 6; border: 1.5px dashed rgba(90, 190, 255, 0.9); border-radius: 2px; background: rgba(90, 190, 255, 0.06); box-shadow: 0 0 14px rgba(90, 190, 255, 0.2); pointer-events: none; }
.drag-zoom-box span { position: absolute; left: -1px; top: -18px; padding: 2px 6px; border-radius: 4px; color: #061426; background: rgba(90, 190, 255, 0.9); font-size: 9px; font-weight: 700; white-space: nowrap; }
.fov-toggle { position: absolute; z-index: 6; right: 18px; bottom: 58px; padding: 5px 10px; border: 1px solid rgba(163,196,235,.24); border-radius: 7px; color: rgba(212,230,251,.62); background: rgba(5,16,29,.4); font-size: 10px; cursor: pointer; }
.fov-toggle.on { border-color: rgba(90,160,255,.55); color: #dcecff; background: rgba(56,132,255,.18); }
.stage-foot { position: absolute; z-index: 6; right: 18px; bottom: 16px; left: 18px; display: flex; align-items: center; gap: 10px; padding: 8px 10px; border: 1px solid rgba(163,196,235,.14); border-radius: 8px; background: rgba(5,16,29,.42); backdrop-filter: blur(8px); }
.stage-foot span { color: rgba(212,230,251,.5); font-size: 10px; white-space: nowrap; }
.stage-foot b { overflow: hidden; color: rgba(236,246,255,.9); font-size: 11px; font-weight: 600; text-overflow: ellipsis; white-space: nowrap; }
.offline-hint {
  position: absolute;
  z-index: 6;
  left: 18px;
  bottom: 58px;
  padding: 5px 9px;
  border: 1px solid rgba(245, 170, 40, 0.28);
  border-radius: 6px;
  color: rgba(255, 222, 170, 0.9);
  background: rgba(120, 74, 12, 0.38);
  font-size: 9px;
  backdrop-filter: blur(6px);
}
.offline-cover { position: absolute; z-index: 7; inset: 0; display: grid; place-content: center; gap: 8px; text-align: center; color: rgba(223,234,249,.72); background: rgba(5,16,29,.46); backdrop-filter: blur(2px); }
.offline-cover b { color: #fff; font-size: 18px; }
.offline-cover span { font-size: 12px; }
.guard-halo { position: absolute; z-index: 3; inset: 0; pointer-events: none; border: 2px solid rgba(8,179,136,.38); background: radial-gradient(circle at 50% 55%, transparent 34%, rgba(8,179,136,.08) 72%, transparent 100%); box-shadow: inset 0 0 70px rgba(8,179,136,.12); }
.wiper-line { position: absolute; z-index: 4; left: 50%; bottom: -4%; width: 1px; height: 46%; transform-origin: 50% 100%; background: linear-gradient(to top, rgba(190, 225, 255, 0.02), rgba(190, 225, 255, 0.3)); box-shadow: 0 0 9px rgba(120, 180, 255, 0.24); animation: wiper-sweep 2s ease-in-out infinite alternate; pointer-events: none; }
.wiper-line::after { content: ""; position: absolute; left: -3px; top: 0; width: 7px; height: 76%; border-radius: 4px; background: rgba(205, 235, 255, 0.16); }
@keyframes wiper-sweep { from { transform: rotate(-28deg); } to { transform: rotate(28deg); } }
.snapshot-flash { position: absolute; z-index: 8; inset: 0; background: #fff; animation: flash-out .18s ease-out forwards; pointer-events: none; }
@keyframes flash-out { from { opacity: .82; } to { opacity: 0; } }

.console { display: flex; min-width: 0; height: 100%; flex-direction: column; padding: 14px; }
.console-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 10px; }
.panel-title { color: var(--text-primary); font-size: 13px; font-weight: 750; }
.panel-sub { margin-top: 4px; color: var(--text-tertiary); font-size: 10px; }
.active-preset { padding: 5px 8px; border-radius: 6px; color: var(--text-tertiary); background: rgba(56,132,255,.07); font-size: 9px; white-space: nowrap; }
.active-preset b { color: var(--accent); font-size: 10px; }
.tab-row { display: grid; grid-template-columns: repeat(4, 1fr); gap: 3px; margin-top: 12px; padding: 3px; border-radius: 9px; background: var(--bg-base); }
.tab { position: relative; display: grid; gap: 1px; padding: 7px 4px; border: 0; border-radius: 7px; color: var(--text-secondary); background: transparent; cursor: pointer; text-align: center; }
.tab b { font-size: 12px; font-weight: 650; }
.tab small { color: var(--text-tertiary); font-size: 8px; }
.tab.active { color: var(--accent); background: var(--bg-elevated); box-shadow: 0 1px 3px rgba(16,46,98,.08); }
.tab.active small { color: var(--accent); opacity: .7; }
.tab-badge { position: absolute; top: 5px; right: 6px; width: 5px; height: 5px; border-radius: 50%; background: var(--error); box-shadow: 0 0 6px rgba(229, 72, 92, 0.55); }
.console-body { flex: 1; min-height: 0; margin-top: 12px; overflow-y: auto; padding-right: 3px; }
.console-body::-webkit-scrollbar { width: 5px; }
.console-body::-webkit-scrollbar-thumb { border-radius: 99px; background: rgba(143,161,189,.28); }

/* 姿态读数条 */
.pose-strip { display: flex; align-items: center; gap: 0; margin-top: 2px; padding: 10px 4px; border-radius: 10px; background: var(--bg-base); }
.pose-item { flex: 1; display: flex; flex-direction: column; align-items: center; gap: 3px; }
.pose-item span { color: var(--text-tertiary); font-size: 9px; font-weight: 600; letter-spacing: .04em; text-transform: uppercase; }
.pose-item b { color: var(--text-primary); font: 700 20px "SF Mono", monospace; letter-spacing: -.02em; }
.pose-divider { width: 1px; height: 32px; background: var(--border-default); flex-shrink: 0; }

/* 云台工作台 */
.ptz-workbench { display: grid; grid-template-columns: 1fr auto 1fr; align-items: center; gap: 10px; margin-top: 10px; }
.ptz-left { display: flex; align-items: center; justify-content: flex-end; }
.ptz-right { }
.control-card { padding: 9px; border: 1px solid var(--border-default); border-radius: 10px; background: rgba(255,255,255,.34); }
.zoom-card { display: grid; justify-items: center; gap: 7px; min-width: 72px; }
.card-label { color: var(--text-tertiary); font-size: 10px; font-weight: 650; }
.big-readout { color: var(--text-primary); font: 750 22px "SF Mono", monospace; }
.big-readout small { color: var(--text-tertiary); font-size: 11px; }
.key-row { display: flex; gap: 5px; }
.lens-column { display: grid; gap: 7px; }
.lens-card { display: grid; gap: 5px; }
.lens-head { display: flex; align-items: center; justify-content: space-between; color: var(--text-tertiary); font-size: 10px; }
.lens-head b { color: var(--text-primary); font: 650 11px "SF Mono", monospace; }
.meter { height: 3px; overflow: hidden; border-radius: 99px; background: var(--border-default); }
.meter i { display: block; height: 100%; border-radius: 99px; background: var(--accent); transition: width .15s ease; }
/* 云台状态条 */
.ptz-status-bar { display: flex; align-items: center; gap: 0; margin-top: 10px; padding: 7px 10px; border-radius: 8px; background: var(--bg-base); font-size: 10.5px; }
.ptz-status-item { flex: 1; display: flex; align-items: center; justify-content: center; gap: 5px; color: var(--text-secondary); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.ptz-status-dot { width: 6px; height: 6px; flex-shrink: 0; border-radius: 50%; background: var(--text-tertiary); opacity: .5; transition: background .2s, opacity .2s; }
.ptz-status-dot.hot { background: var(--accent); opacity: 1; }
.ptz-status-sep { width: 1px; height: 14px; background: var(--border-default); flex-shrink: 0; }
.hint-line { margin: 9px 0 0; color: var(--text-tertiary); font-size: 9px; line-height: 1.6; }

.block { margin-top: 12px; padding-top: 11px; border-top: 1px solid var(--border-default); }
.block:first-child { margin-top: 0; padding-top: 0; border-top: 0; }
.block-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; margin-bottom: 8px; }
.block-head span { color: var(--text-secondary); font-size: 11px; font-weight: 700; }
.block-head small { color: var(--text-tertiary); font-size: 9px; }
.empty { padding: 12px; border: 1px dashed var(--border-default); border-radius: 8px; color: var(--text-tertiary); font-size: 10px; text-align: center; }
.preset-chips { display: flex; flex-wrap: wrap; gap: 5px; }
.preset-chip { padding: 5px 7px; border: 1px solid var(--border-default); border-radius: 6px; color: var(--accent); background: var(--accent-dim); font: 650 10px "SF Mono", monospace; }
.preset-chip small { margin-left: 3px; color: var(--text-tertiary); font: 500 9px inherit; }
.preset-chip.current { border-color: var(--accent); color: #fff; background: var(--accent); }
.preset-chip.current small { color: rgba(255,255,255,.78); }
.home-card { display: flex; flex-wrap: wrap; align-items: center; gap: 9px; padding: 9px 10px; border-radius: 8px; background: var(--bg-base); }
.home-card b { color: var(--text-primary); font-size: 11px; }
.home-card span { color: var(--text-secondary); font-size: 10px; }
.home-card .muted { color: var(--text-tertiary); font-size: 9px; }
.dot { width: 7px; height: 7px; border-radius: 50%; background: var(--border-default); }
.dot.on { background: var(--success); box-shadow: 0 0 7px rgba(8,179,136,.5); }
.track-list { display: grid; gap: 5px; }
.track-row { display: flex; gap: 9px; padding: 7px 8px; border-radius: 7px; background: var(--bg-base); }
.track-row.active { outline: 1px solid rgba(56,132,255,.25); background: var(--accent-dim); }
.track-row > b { color: var(--accent); font: 650 10px "SF Mono", monospace; white-space: nowrap; }
.track-row > div { min-width: 0; }
.track-row div div { overflow: hidden; color: var(--text-secondary); font-size: 10px; text-overflow: ellipsis; white-space: nowrap; }
.track-row small { color: var(--text-tertiary); font-size: 9px; }
.scan-list { display: flex; flex-wrap: wrap; gap: 5px; }
.scan-chip { padding: 5px 7px; border-radius: 99px; color: var(--text-secondary); background: var(--border-default); font: 600 9px "SF Mono", monospace; }
.scan-chip.active { color: #fff; background: var(--accent); }
.state-row { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; padding: 9px 10px; border-radius: 8px; background: var(--bg-base); }
.state-row b { color: var(--text-primary); font-size: 11px; }
.state-row .muted { color: var(--text-tertiary); font-size: 9px; }
.mask-list { display: grid; gap: 3px; margin-top: 5px; }
.mask-row { display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 5px 8px; border-radius: 6px; background: var(--bg-base); }
.mask-row b { color: var(--text-secondary); font-size: 9px; }
.mask-row span { color: var(--text-tertiary); font: 9px "SF Mono", monospace; }
.mirror-row { display: grid; grid-template-columns: repeat(4, 1fr); gap: 4px; }
.mirror-chip { display: grid; place-items: center; padding: 8px 2px; border: 1px solid var(--border-default); border-radius: 6px; color: var(--text-tertiary); background: var(--bg-base); font-size: 10px; }
.mirror-chip.selected { border-color: var(--accent); color: var(--accent); background: var(--accent-dim); font-weight: 700; }
.event-list { display: grid; gap: 4px; }
.event-row { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 7px 9px; border-radius: 7px; background: var(--bg-base); }
.event-row span { color: var(--text-tertiary); font-size: 10px; white-space: nowrap; }
.event-row b { overflow: hidden; color: var(--text-secondary); font-size: 10px; font-weight: 600; text-overflow: ellipsis; white-space: nowrap; }
.event-row.hot { background: var(--accent-dim); }
.event-row.hot b { color: var(--accent); }
.lamp-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 7px; }
.lamp-card { display: flex; align-items: center; gap: 8px; padding: 9px 10px; border: 1px solid var(--border-default); border-radius: 8px; background: var(--bg-base); }
.lamp-card i { width: 10px; height: 10px; border-radius: 50%; background: var(--border-default); }
.lamp-card.on { border-color: color-mix(in srgb, var(--lamp) 38%, transparent); background: color-mix(in srgb, var(--lamp) 8%, transparent); }
.lamp-card.on i { background: var(--lamp); box-shadow: 0 0 9px color-mix(in srgb, var(--lamp) 60%, transparent); }
.lamp-card div { display: grid; gap: 1px; }
.lamp-card b { color: var(--text-primary); font-size: 11px; }
.lamp-card small { color: var(--text-tertiary); font-size: 8px; }
.lamp-card.on b { color: var(--lamp); }
.lamp-card.on small { color: color-mix(in srgb, var(--lamp) 80%, var(--text-secondary)); }
.storage-list { display: grid; gap: 4px; }
.storage-row { display: grid; grid-template-columns: auto 1.2fr .8fr 1fr .7fr; align-items: center; gap: 7px; padding: 6px 8px; border-radius: 7px; background: var(--bg-base); }
.storage-row i { width: 6px; height: 6px; border-radius: 50%; background: var(--success); }
.storage-row b { color: var(--text-primary); font-size: 10px; }
.storage-row span { color: var(--text-secondary); font: 9px "SF Mono", monospace; text-align: right; }
.upgrade-card { padding: 8px 9px; border: 1px solid rgba(56,132,255,.18); border-radius: 8px; background: var(--accent-dim); }
.upgrade-head { display: flex; align-items: center; gap: 8px; margin-bottom: 6px; }
.upgrade-head span { flex: 1; color: var(--accent); font-size: 10px; font-weight: 650; }
.upgrade-head b { color: var(--text-secondary); font-size: 9px; }
.upgrade-head strong { color: var(--text-primary); font: 700 11px "SF Mono", monospace; }
.last-command { padding: 9px 10px; border-radius: 8px; color: var(--text-primary); background: var(--bg-base); font-size: 10px; line-height: 1.6; word-break: break-all; }

@media (max-width: 1180px) {
  .page-header { grid-template-columns: 1fr auto; }
  .headline { grid-column: 1 / -1; grid-row: 2; justify-self: start; }
  .sim-layout { grid-template-columns: 1fr; }
  .stage { min-height: 500px; }
  .console { height: auto; }
  .console-body { max-height: 60vh; }
}
@media (max-width: 720px) {
  .ptz-workbench { grid-template-columns: 1fr; justify-items: center; }
  .lens-column { width: 100%; grid-template-columns: 1fr 1fr; }
  .page-header { grid-template-columns: 1fr; }
  .header-right { justify-content: space-between; }
}
</style>
