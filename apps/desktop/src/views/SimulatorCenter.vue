<script setup lang="ts">
// 桌面端「模拟器中心」。
//
// 结构：左侧 3D 舞台（只表达设备姿态，不接视频流）+ 右侧三段式控制台
//（常驻状态总览 / 常驻云台手操 / 分组只读回显）。
// 功能口径对齐移动端模拟中心：云台/定位/图像/设备；除云台手操外全部只读回显。
// 布局按视口宽高双向自适应：不够放时整页滚动，绝不把内容压进一屏再裁掉。
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import PtzCamera3D from "../components/PtzCamera3D.vue";
import PtzPad from "../components/PtzPad.vue";
import HoldKey from "../components/HoldKey.vue";
import { useDevice } from "../device";
import { tabForSummary, useSimulatorCenter, type SimTab } from "../simulator/useSimulatorCenter";

const sim = useSimulatorCenter();
const { deviceLive, reconcile } = useDevice();

// 云台手操常驻右侧，不参与分组切换；只有只读回显需要按主题分组。
type DetailTab = Exclude<SimTab, "ptz">;
const detailTab = ref<DetailTab>("position");
const detailBadges = ref<DetailTab[]>([]);
const showFov = ref(true);
const autoFollow = ref(true);

const detailTabs: Array<{ key: DetailTab; label: string; hint: string }> = [
  { key: "position", label: "定位", hint: "预置位 / 巡航" },
  { key: "image", label: "图像", hint: "遮挡 / 事件" },
  { key: "device", label: "设备", hint: "存储 / 维护" },
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

function selectDetail(key: DetailTab) {
  detailTab.value = key;
  detailBadges.value = detailBadges.value.filter((item) => item !== key);
}

// 预置位在移动端只是只读 chip；桌面端已有 3D 舞台与本机姿态积分，
// 直接调用 store 的 seekToPreset 即可本机调用，不向平台发任何指令。
function callPreset(preset: { id: number; name: string; pan: number; tilt: number; zoom: number }) {
  sim.seekToPreset(preset.id, preset.name, {
    pan: preset.pan,
    tilt: preset.tilt,
    zoom: preset.zoom,
  });
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
    // 云台命令由常驻手操区与顶部状态条承接，不再把手操区切走。
    if (!target || target === "ptz" || target === detailTab.value) return;
    if (autoFollow.value) selectDetail(target);
    else if (!detailBadges.value.includes(target)) {
      detailBadges.value = [...detailBadges.value, target];
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

      <div class="header-right">
        <label class="follow-toggle">
          <input v-model="autoFollow" type="checkbox" />
          平台命令自动切换分组
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

      <!-- 右：常驻手操 + 平台回显 -->
      <aside class="glass-card console">
        <!-- 1) 全局状态总览：切换回显分组时始终可见 -->
        <section class="overview" aria-label="设备状态总览">
          <div class="overview-main">
            <span class="overview-dot" :style="{ '--dot': sim.headline.value.color }" />
            <div class="overview-text">
              <strong>{{ sim.headline.value.text }}</strong>
              <small>
                最近命令 {{ sim.lastCommand.value ? timeText(sim.lastCommand.value.at) : "—" }}
                · {{ sim.lastCommand.value?.summary ?? "尚未收到平台命令" }}
              </small>
            </div>
          </div>
          <div class="lamp-row">
            <span
              v-for="lamp in lampItems"
              :key="lamp.key"
              class="lamp"
              :class="{ on: lamp.active }"
              :style="{ '--lamp': lamp.color }"
            >
              <i />
              <b>{{ lamp.label }}</b>
              <small>{{ lamp.active ? "开" : "关" }}</small>
            </span>
          </div>
        </section>

        <!-- 2) 云台手操：常驻，不随回显分组切换 -->
        <section class="control-section" aria-labelledby="control-title">
          <div class="section-head">
            <h3 id="control-title">云台手操</h3>
            <span>按下走一步 · 长按连续</span>
          </div>

          <div class="pose-strip">
            <div class="pose-item"><span>水平</span><b>{{ pose.pan.toFixed(0) }}°</b></div>
            <div class="pose-item"><span>俯仰</span><b>{{ pose.tilt.toFixed(0) }}°</b></div>
            <div class="pose-item"><span>变倍</span><b>{{ pose.zoom.toFixed(1) }}×</b></div>
          </div>

          <div class="ptz-workbench">
            <PtzPad @adjust="onPad" />
            <div class="lens-stack">
              <div class="lens-card">
                <div class="lens-card-info">
                  <span>变倍</span>
                  <b>{{ pose.zoom.toFixed(1) }}×</b>
                </div>
                <div class="lens-keys">
                  <HoldKey label="−" aria-label="缩小" @trigger="onZoom(-1, $event)" />
                  <HoldKey label="+" aria-label="放大" @trigger="onZoom(1, $event)" />
                </div>
              </div>
              <div class="lens-card">
                <div class="lens-card-info">
                  <span>光圈</span>
                  <b>{{ pct(pose.iris) }}%</b>
                  <div class="meter"><i :style="{ width: `${pct(pose.iris)}%` }" /></div>
                </div>
                <div class="lens-keys">
                  <HoldKey tone="lens" label="−" aria-label="光圈缩小" @trigger="onIris(-1, $event)" />
                  <HoldKey tone="lens" label="+" aria-label="光圈放大" @trigger="onIris(1, $event)" />
                </div>
              </div>
              <div class="lens-card">
                <div class="lens-card-info">
                  <span>聚焦</span>
                  <b>{{ pct(pose.focus) }}%</b>
                  <div class="meter"><i :style="{ width: `${pct(pose.focus)}%` }" /></div>
                </div>
                <div class="lens-keys">
                  <HoldKey tone="lens" label="近" aria-label="聚焦近" @trigger="onFocus(-1, $event)" />
                  <HoldKey tone="lens" label="远" aria-label="聚焦远" @trigger="onFocus(1, $event)" />
                </div>
              </div>
            </div>
          </div>

          <div class="ptz-status-bar">
            <span><i class="ptz-status-dot" :class="{ hot: sim.ptzActive.value || sim.seeking.value }" />{{ sim.headline.value.text }}</span>
            <span>{{ sim.directionText.value !== "—" ? sim.directionText.value : "待命" }}</span>
            <span>{{ sim.localPushing.value ? "本机调整中" : "平台控制" }}</span>
          </div>
        </section>

        <!-- 3) 平台回显：分组切换只作用于本区，不打断手操 -->
        <section class="feedback" aria-labelledby="feedback-title">
          <div class="section-head">
            <h3 id="feedback-title">平台回显</h3>
            <span>只读 · 平台下发</span>
          </div>

          <div class="segment" role="tablist" aria-label="平台回显分组">
            <button
              v-for="tab in detailTabs"
              :key="tab.key"
              :id="`tab-${tab.key}`"
              type="button"
              role="tab"
              :aria-selected="detailTab === tab.key"
              :aria-controls="`panel-${tab.key}`"
              :class="{ active: detailTab === tab.key }"
              @click="selectDetail(tab.key)"
            >
              <b>{{ tab.label }}</b>
              <small>{{ tab.hint }}</small>
              <i v-if="detailBadges.includes(tab.key)" class="segment-badge" aria-label="该分组有新命令" />
            </button>
          </div>

          <div
            :id="`panel-${detailTab}`"
            class="feedback-body"
            role="tabpanel"
            :aria-labelledby="`tab-${detailTab}`"
          >
            <!-- 定位 -->
            <template v-if="detailTab === 'position'">
              <article class="info-card">
                <header>
                  <h4>预置位</h4>
                  <span>{{ sim.presets.value.length ? `${sim.presets.value.length} 个 · 当前 ${activePresetLabel}` : "平台未下发" }}</span>
                </header>
                <div v-if="sim.presets.value.length" class="preset-list">
                  <button
                    v-for="preset in sim.presets.value"
                    :key="preset.id"
                    type="button"
                    class="preset-button"
                    :class="{ current: sim.activePreset.value === preset.id }"
                    :title="`本机调用 ${preset.name}`"
                    @click="callPreset(preset)"
                  >
                    <b>P{{ preset.id }}</b><span>{{ preset.name }}</span>
                  </button>
                </div>
                <p v-else class="empty">未配置 · 等待平台下发</p>
              </article>

              <article class="info-card">
                <header><h4>看守位</h4><span>平台下发</span></header>
                <template v-if="sim.home.configured">
                  <div class="kv-row"><span>状态</span><b>{{ sim.home.enabled ? "已启用" : "已关闭" }}</b></div>
                  <div class="kv-row"><span>指向预置位</span><b>{{ sim.home.presetIndex !== null ? `P${sim.home.presetIndex}` : "未下发" }}</b></div>
                  <div class="kv-row"><span>自动归位</span><b>{{ sim.home.resetTime !== null ? `${sim.home.resetTime}s` : "字段待后端补齐" }}</b></div>
                </template>
                <p v-else class="empty">未配置 · 等待平台下发</p>
              </article>

              <article class="info-card">
                <header><h4>巡航轨迹</h4><span>{{ sim.cruiseTracks.value.length ? `${sim.cruiseTracks.value.length} 条` : "平台未下发" }}</span></header>
                <template v-if="sim.cruiseTracks.value.length">
                  <div v-for="track in sim.cruiseTracks.value" :key="track.id" class="kv-row" :class="{ hot: track.active }">
                    <span>#{{ track.id }}{{ track.active ? " 运行中" : "" }}</span>
                    <b>{{ track.points.length ? track.points.map((p) => `P${p}`).join(" → ") : "暂无点位" }}</b>
                  </div>
                  <div v-for="track in sim.cruiseTracks.value" :key="`meta-${track.id}`" class="kv-row">
                    <span>#{{ track.id }} 参数</span>
                    <b>{{ track.dwell !== null ? `停留 ${track.dwell}s` : "停留未下发" }} · {{ track.speed !== null ? `速度 ${track.speed}` : "速度未下发" }}</b>
                  </div>
                </template>
                <p v-else class="empty">未配置 · 等待平台下发</p>
              </article>

              <article class="info-card">
                <header><h4>自动扫描</h4><span>{{ sim.scanGroups.value.length ? `${sim.scanGroups.value.length} 组` : "平台未下发" }}</span></header>
                <template v-if="sim.scanGroups.value.length">
                  <div v-for="group in sim.scanGroups.value" :key="group.id" class="kv-row" :class="{ hot: group.active }">
                    <span>#{{ group.id }}{{ group.active ? " 运行中" : "" }}</span>
                    <b>{{ group.left ?? "未设" }} ↔ {{ group.right ?? "未设" }} · {{ group.speed ?? "速度未下发" }}</b>
                  </div>
                </template>
                <p v-else class="empty">平台未下发扫描组</p>
              </article>
            </template>

            <!-- 图像 -->
            <template v-else-if="detailTab === 'image'">
              <article class="info-card">
                <header><h4>画面遮挡</h4><span>GB-2022 A.2.1.17</span></header>
                <div class="kv-row"><span>状态</span><b>{{ sim.mask.configured ? (sim.mask.on ? "已启用" : "已停用") : "平台未配置" }}</b></div>
                <div class="kv-row"><span>遮挡区域</span><b>{{ sim.mask.on ? `${sim.mask.regions.length} 个` : "—" }}</b></div>
                <div v-for="region in sim.mask.regions" :key="region.seq" class="kv-row">
                  <span>区域 {{ region.seq }}</span>
                  <b>{{ region.left }},{{ region.top }} → {{ region.right }},{{ region.bottom }}</b>
                </div>
              </article>

              <article class="info-card">
                <header><h4>画面镜像</h4><span>A.2.1.23</span></header>
                <div class="mirror-row">
                  <span
                    v-for="(label, index) in mirrorOptions"
                    :key="label"
                    class="mirror-chip"
                    :class="{ selected: sim.mirror.configured && sim.mirror.mode === index }"
                  >{{ label }}</span>
                </div>
                <p class="hint-line">{{ sim.mirror.configured ? "平台已下发" : "平台未配置，四张卡均不点亮" }}</p>
              </article>

              <article class="info-card">
                <header><h4>画面事件</h4><span>平台下发回显</span></header>
                <div class="kv-row" :class="{ hot: sim.keyframeActive.value }"><span>请求关键帧</span><b>{{ sim.keyframeActive.value ? "已收到平台请求" : "等待平台请求" }}</b></div>
                <div class="kv-row" :class="{ hot: sim.nowMs.value - sim.snapshotAt.value < 3000 }"><span>抓拍</span><b>{{ sim.snapshotAt.value ? timeText(sim.snapshotAt.value) : "尚未收到抓拍" }}</b></div>
                <div class="kv-row" :class="{ hot: sim.targetActive.value }"><span>目标跟踪</span><b>{{ targetLabel }}</b></div>
                <div class="kv-row" :class="{ hot: dragZoomRecent }"><span>拉框联动</span><b>{{ dragZoomRecent ? "最近 3 秒收到" : "暂无" }}</b></div>
              </article>

              <article class="info-card">
                <header><h4>前端 OSD</h4><span>平台配置回显</span></header>
                <div class="kv-row"><span>OSD 信息</span><b>{{ sim.osd.configured ? (sim.osd.show ? "开" : "关") : "未配置" }}</b></div>
                <div class="kv-row"><span>时间水印</span><b>{{ sim.osd.configured ? (sim.osd.timeShow ? "开" : "关") : "未配置" }}</b></div>
              </article>
            </template>

            <!-- 设备 -->
            <template v-else>
              <article class="info-card">
                <header><h4>存储卡</h4><span>{{ sim.storage.queried ? `${sim.storage.cards.length} 张` : "平台查询触发" }}</span></header>
                <p v-if="!sim.storage.queried" class="empty">等待平台下发存储卡查询</p>
                <p v-else-if="sim.storage.cards.length === 0" class="empty">未安装存储卡 (SumNum=0)</p>
                <template v-else>
                  <div v-for="card in sim.storage.cards" :key="card.id" class="kv-row">
                    <span>{{ card.name }} · {{ storageCapacity(card.capacityMb) }}</span>
                    <b>{{ card.status }} · {{ storageStatus(card) }}</b>
                  </div>
                </template>
              </article>

              <article class="info-card">
                <header><h4>设备维护</h4><span>平台命令回显</span></header>
                <div class="kv-row" :class="{ hot: sim.rebooting.value }"><span>远程重启</span><b>{{ sim.rebooting.value ? "正在重启" : "暂无重启任务" }}</b></div>
                <div class="kv-row" :class="{ hot: formatRecent }"><span>格式化 SD 卡</span><b>{{ formatRecent ? `收到请求 · ${timeText(sim.formatAt.value)}` : "暂无请求" }}</b></div>
                <div class="kv-row" :class="{ hot: sim.lastCommand.value?.summary === '在线升级' }"><span>升级请求</span><b>{{ sim.lastCommand.value?.summary === "在线升级" ? "平台已下发" : "暂无请求" }}</b></div>
                <div v-if="sim.upgrade.value" class="upgrade-card">
                  <div class="upgrade-head">
                    <span>在线升级{{ sim.upgrade.value.firmware ? ` v${sim.upgrade.value.firmware}` : "" }}</span>
                    <b>{{ sim.upgrade.value.result }}</b>
                    <strong>{{ sim.upgrade.value.percent }}%</strong>
                  </div>
                  <div class="meter"><i :style="{ width: `${sim.upgrade.value.percent}%` }" /></div>
                </div>
              </article>

              <article class="info-card">
                <header><h4>最近平台命令</h4><span>{{ sim.lastCommand.value ? timeText(sim.lastCommand.value.at) : "—" }}</span></header>
                <div class="last-command">{{ sim.lastCommand.value?.summary ?? "尚未收到平台命令" }}</div>
              </article>
            </template>
          </div>
        </section>
      </aside>
    </div>
  </div>
</template>

<style scoped>
/* 与 theme.css 的 .page-wrap 同口径：内容超出视口时整页滚动。
   父级 .content 是 overflow:hidden，若此处不滚动，超出部分会被直接裁掉。 */
.sim-page { width: min(1540px, 100%); margin: 0 auto; flex: 1; min-height: 0; display: flex; flex-direction: column; overflow-y: auto; overflow-x: hidden; }
.page-header {
  display: grid;
  grid-template-columns: minmax(280px, 1fr) auto;
  align-items: center;
  gap: 20px;
  margin-bottom: 16px;
}
.header-right { display: flex; align-items: center; gap: 12px; }
.follow-toggle { display: flex; align-items: center; gap: 5px; color: var(--text-tertiary); font-size: 11px; white-space: nowrap; cursor: pointer; }
.follow-toggle input { accent-color: var(--accent); }
.device-pill { padding: 6px 11px; border: 1px solid var(--border-default); border-radius: 999px; color: var(--text-tertiary); background: rgba(255, 255, 255, 0.48); font-size: 12px; white-space: nowrap; }
.device-pill.online { border-color: rgba(24, 160, 88, 0.25); color: var(--success); background: rgba(24, 160, 88, 0.08); }

/* 右栏 clamp(440px, 34%, 520px)：窄窗口优先保住手操宽度，宽窗口不无限膨胀。
   grid-template-rows:auto + flex:1 0 auto：行高先按内容撑开，视口更高时才拉伸铺满，
   不再是 minmax(0,1fr) 把三段硬压进一屏导致回显区被裁。 */
.sim-layout { display: grid; grid-template-columns: minmax(0, 1fr) clamp(440px, 34%, 520px); grid-template-rows: auto; gap: 16px; align-items: stretch; flex: 1 0 auto; min-height: 0; }
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

.console { display: flex; min-width: 0; height: auto; flex-direction: column; gap: 12px; padding: 14px; }

/* 右侧统一分区头：13px 标题 + 11px 说明，取代原先 8~10px 的小字堆叠。 */
.section-head { display: flex; align-items: baseline; justify-content: space-between; gap: 8px; }
.section-head h3 { margin: 0; color: var(--text-primary); font-size: 13px; font-weight: 700; }
.section-head > span { color: var(--text-tertiary); font-size: 11px; white-space: nowrap; }

/* 1) 状态总览：任何回显分组下都常驻 */
.overview { display: grid; gap: 10px; padding: 11px 12px; border: 1px solid var(--border-default); border-radius: 12px; background: var(--bg-surface); }
.overview-main { display: flex; align-items: center; gap: 10px; min-width: 0; }
.overview-dot { width: 9px; height: 9px; flex: none; border-radius: 50%; background: var(--dot); box-shadow: 0 0 10px var(--dot); }
.overview-text { display: grid; gap: 3px; min-width: 0; }
.overview-text strong { overflow: hidden; color: var(--text-primary); font-size: 13px; font-weight: 700; text-overflow: ellipsis; white-space: nowrap; }
.overview-text small { overflow: hidden; color: var(--text-tertiary); font-size: 11px; text-overflow: ellipsis; white-space: nowrap; }
.lamp-row { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 6px; }
.lamp { display: flex; align-items: center; gap: 5px; min-width: 0; padding: 5px 7px; border: 1px solid var(--border-default); border-radius: 8px; background: var(--bg-base); }
.lamp i { width: 7px; height: 7px; flex: none; border-radius: 50%; background: var(--border-default); }
.lamp b { color: var(--text-secondary); font-size: 11px; font-weight: 600; }
.lamp small { margin-left: auto; color: var(--text-tertiary); font-size: 10px; }
.lamp.on { border-color: color-mix(in srgb, var(--lamp) 38%, transparent); background: color-mix(in srgb, var(--lamp) 8%, transparent); }
.lamp.on i { background: var(--lamp); box-shadow: 0 0 8px color-mix(in srgb, var(--lamp) 60%, transparent); }
.lamp.on b { color: var(--lamp); }

/* 2) 云台手操：两段式，方盘在左、镜头参数在右，按键命中区域放大 */
.control-section { display: grid; gap: 10px; }
.pose-strip { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); padding: 8px 4px; border-radius: 10px; background: var(--bg-base); }
.pose-item { display: grid; justify-items: center; gap: 2px; }
.pose-item + .pose-item { border-left: 1px solid var(--border-default); }
.pose-item span { color: var(--text-tertiary); font-size: 11px; }
.pose-item b { color: var(--text-primary); font: 700 20px "SF Mono", monospace; }
.ptz-workbench { display: grid; grid-template-columns: auto minmax(0, 1fr); align-items: center; gap: 12px; }
.lens-stack { display: grid; gap: 8px; min-width: 0; }
.lens-card { display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: center; gap: 8px; padding: 8px 10px; border: 1px solid var(--border-default); border-radius: 10px; background: var(--bg-surface); }
.lens-card-info { display: grid; gap: 4px; min-width: 0; }
.lens-card-info > span { color: var(--text-tertiary); font-size: 11px; }
.lens-card-info > b { color: var(--text-primary); font: 700 15px "SF Mono", monospace; }
.lens-keys { display: flex; gap: 5px; }
.meter { height: 3px; overflow: hidden; border-radius: 99px; background: var(--border-default); }
.meter i { display: block; height: 100%; border-radius: 99px; background: var(--accent); transition: width .15s ease; }
/* 子组件保持移动端同款不动，只在控制台内放大命中区域。 */
.console :deep(.hold-key) { min-width: 38px; height: 34px; padding-inline: 8px; font-size: 13px; border-radius: 8px; }
.console :deep(.hold-key.lens) { min-width: 36px; }
.console :deep(.hold-key:focus-visible) { outline: 2px solid var(--accent); outline-offset: 2px; }
.ptz-status-bar { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); align-items: center; padding: 7px 10px; border-radius: 9px; background: var(--bg-base); }
.ptz-status-bar > span { display: flex; align-items: center; justify-content: center; gap: 5px; overflow: hidden; color: var(--text-secondary); font-size: 11px; text-overflow: ellipsis; white-space: nowrap; }
.ptz-status-bar > span + span { border-left: 1px solid var(--border-default); }
.ptz-status-dot { width: 6px; height: 6px; flex: none; border-radius: 50%; background: var(--text-tertiary); opacity: .5; }
.ptz-status-dot.hot { background: var(--accent); opacity: 1; }

/* 3) 平台回显：统一卡片 + 键值行，只有本区滚动 */
.feedback { display: flex; flex: 1; min-height: 0; flex-direction: column; gap: 10px; }
.segment { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 4px; padding: 3px; border-radius: 10px; background: var(--bg-base); }
.segment > button { position: relative; display: grid; gap: 2px; padding: 7px 4px; border: 0; border-radius: 8px; color: var(--text-secondary); background: transparent; font: inherit; text-align: center; cursor: pointer; }
.segment > button b { font-size: 12px; font-weight: 650; }
.segment > button small { color: var(--text-tertiary); font-size: 10px; }
.segment > button.active { color: var(--accent); background: var(--bg-elevated); box-shadow: 0 1px 3px rgba(16,46,98,.08); }
.segment > button.active b { color: var(--accent); }
.segment > button:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
.segment-badge { position: absolute; top: 6px; right: 8px; width: 6px; height: 6px; border-radius: 50%; background: var(--error); box-shadow: 0 0 6px rgba(229, 72, 92, .55); }
.feedback-body { display: grid; align-content: start; gap: 8px; flex: 1; min-height: 0; max-height: clamp(220px, 32vh, 400px); overflow-y: auto; padding-right: 3px; }
.feedback-body::-webkit-scrollbar { width: 5px; }
.feedback-body::-webkit-scrollbar-thumb { border-radius: 99px; background: rgba(143,161,189,.28); }
.info-card { padding: 10px 12px; border: 1px solid var(--border-default); border-radius: 10px; background: var(--bg-surface); }
.info-card > header { display: flex; align-items: baseline; justify-content: space-between; gap: 8px; margin-bottom: 6px; }
.info-card h4 { margin: 0; color: var(--text-secondary); font-size: 12px; font-weight: 700; }
.info-card > header > span { color: var(--text-tertiary); font-size: 11px; text-align: right; }
.kv-row { display: flex; align-items: baseline; justify-content: space-between; gap: 10px; padding: 5px 0; font-size: 11px; }
.kv-row + .kv-row { border-top: 1px solid var(--border-default); }
.kv-row > span { flex: none; color: var(--text-tertiary); }
.kv-row > b { overflow: hidden; color: var(--text-primary); font-weight: 600; text-align: right; text-overflow: ellipsis; white-space: nowrap; }
.kv-row.hot > b { color: var(--accent); }
.empty { margin: 0; padding: 10px; border: 1px dashed var(--border-default); border-radius: 8px; color: var(--text-tertiary); font-size: 11px; text-align: center; }
.preset-list { display: flex; flex-wrap: wrap; gap: 6px; }
.preset-button { display: inline-flex; align-items: center; gap: 6px; min-height: 34px; padding: 6px 9px; border: 1px solid var(--border-accent); border-radius: 8px; color: var(--accent); background: var(--accent-dim); font: inherit; font-size: 11px; cursor: pointer; }
.preset-button b { font: 700 11px "SF Mono", monospace; }
.preset-button span { color: var(--text-secondary); }
.preset-button:hover { background: var(--accent-pale); }
.preset-button.current { border-color: var(--accent); color: #fff; background: var(--accent); }
.preset-button.current span { color: rgba(255,255,255,.85); }
.preset-button:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
.mirror-row { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 5px; }
.mirror-chip { display: grid; place-items: center; padding: 7px 2px; border: 1px solid var(--border-default); border-radius: 8px; color: var(--text-tertiary); background: var(--bg-base); font-size: 11px; }
.mirror-chip.selected { border-color: var(--accent); color: var(--accent); background: var(--accent-dim); font-weight: 700; }
.hint-line { margin: 7px 0 0; color: var(--text-tertiary); font-size: 11px; }
.upgrade-card { margin-top: 6px; padding: 8px 10px; border: 1px solid rgba(56,132,255,.18); border-radius: 8px; background: var(--accent-dim); }
.upgrade-head { display: flex; align-items: baseline; gap: 8px; margin-bottom: 6px; }
.upgrade-head span { flex: 1; color: var(--accent); font-size: 11px; font-weight: 650; }
.upgrade-head b { color: var(--text-secondary); font-size: 10px; }
.upgrade-head strong { color: var(--text-primary); font: 700 12px "SF Mono", monospace; }
.last-command { padding: 8px 10px; border-radius: 8px; color: var(--text-primary); background: var(--bg-base); font-size: 11px; line-height: 1.6; word-break: break-all; }

/* 窄窗口改上下两行：必须显式写 grid-template-rows: auto auto，
   只给一行时控制台会落到隐式行，被父级 overflow:hidden 直接裁掉。 */
@media (max-width: 1180px) {
  .page-header { grid-template-columns: 1fr auto; }
  .sim-layout { grid-template-columns: 1fr; grid-template-rows: auto auto; min-height: 0; }
  .stage { min-height: 460px; }
  .console {
    display: grid;
    grid-template-columns: minmax(280px, 0.85fr) minmax(0, 1.15fr);
    align-items: start;
    gap: 12px;
  }
  .overview { grid-row: 1; grid-column: 1 / -1; }
  .control-section { grid-row: 2; grid-column: 1; }
  .feedback { grid-row: 2; grid-column: 2; }
  .ptz-workbench { grid-template-columns: 1fr; justify-items: center; }
  .lens-stack { width: 100%; }
  .feedback-body { grid-template-columns: repeat(2, minmax(0, 1fr)); max-height: 360px; }
}
@media (max-width: 720px) {
  .console { grid-template-columns: 1fr; }
  .overview, .control-section, .feedback { grid-row: auto; grid-column: 1; }
  .feedback-body { grid-template-columns: 1fr; }
  .ptz-workbench { grid-template-columns: 1fr; justify-items: center; }
  .lens-stack { width: 100%; }
  .page-header { grid-template-columns: 1fr; }
  .header-right { justify-content: space-between; }
}

/* 矮窗口（非全屏常见尺寸）：只收紧留白，不缩字号、不缩按键命中区域。 */
@media (max-height: 820px) and (min-width: 1181px) {
  .console { gap: 8px; padding: 10px; }
  .overview { gap: 7px; padding: 8px 10px; }
  .control-section { gap: 8px; }
  .pose-strip { padding: 6px 4px; }
  .lens-stack { gap: 6px; }
  .lens-card { padding: 6px 9px; }
  .ptz-status-bar { padding: 6px 10px; }
  .feedback { gap: 8px; }
  .feedback-body { max-height: 190px; }
  .stage { min-height: 520px; }
}
</style>
