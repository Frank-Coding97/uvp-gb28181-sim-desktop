<script setup lang="ts">
// 仅开发预览：对比旧版程序化球机和重画版室外变速球机。
// 不在左侧导航里，路由 /ptz-lab 手动访问。
import { onUnmounted, ref } from "vue";
import ThreePtzCamera from "../components/ThreePtzCamera.vue";
import PtzCamera3D from "../components/PtzCamera3D.vue";

const pan = ref(-36);
const tilt = ref(14);
const zoom = ref(3.2);
const focus = ref(0.5);
const iris = ref(0.5);
const active = ref(false);
const guarded = ref(false);
const showFovCone = ref(true);
const selfTest = ref(false);
const cruising = ref(false);

let selfTestTimer: number | undefined;
let cruiseTimer: number | undefined;

function runSelfTest(): void {
  selfTest.value = false;
  window.clearTimeout(selfTestTimer);
  // 下一帧再置 true，保证连续点击也能重新触发 watcher。
  requestAnimationFrame(() => {
    selfTest.value = true;
    selfTestTimer = window.setTimeout(() => {
      selfTest.value = false;
    }, 7200);
  });
}

function toggleCruise(): void {
  cruising.value = !cruising.value;
  window.clearInterval(cruiseTimer);
  if (!cruising.value) return;
  cruiseTimer = window.setInterval(() => {
    pan.value = ((pan.value + 0.8 + 180) % 360) - 180;
  }, 50);
}

function recallPreset(index: number): void {
  const presets: Record<number, [number, number, number]> = {
    1: [-120, 20, 1],
    2: [90, -15, 2],
    3: [0, 30, 1.5],
  };
  const [nextPan, nextTilt, nextZoom] = presets[index] ?? [0, 0, 1];
  pan.value = nextPan;
  tilt.value = nextTilt;
  zoom.value = nextZoom;
}

function resetPose(): void {
  pan.value = 0;
  tilt.value = 0;
  zoom.value = 1;
  active.value = false;
  guarded.value = false;
  showFovCone.value = false;
  cruising.value = false;
  window.clearInterval(cruiseTimer);
  window.clearTimeout(selfTestTimer);
  selfTest.value = false;
}

onUnmounted(() => {
  window.clearTimeout(selfTestTimer);
  window.clearInterval(cruiseTimer);
});
</script>

<template>
  <div class="lab">
    <header class="lab-header">
      <div>
        <p class="eyebrow">DEVELOPMENT PREVIEW</p>
        <h1>PTZ 相机 · 重画预览</h1>
        <p class="sub">左侧旧版保留对照，右侧是新版「室外变速球机」。仅开发预览，不在导航中。</p>
      </div>
      <div class="header-actions">
        <button :class="{ on: active }" @click="active = !active">运行灯</button>
        <button :class="{ on: guarded }" @click="guarded = !guarded">布防</button>
        <button :class="{ on: showFovCone }" @click="showFovCone = !showFovCone">FOV 光锥</button>
        <button @click="runSelfTest">开机自检</button>
        <button @click="resetPose">复位</button>
      </div>
    </header>

    <div class="stages">
      <section class="stage-card old">
        <div class="stage-meta">
          <div><span class="tag">旧版</span><b>程序化球机</b></div>
          <small>球头 + U 型臂 · 蓝色光锥</small>
        </div>
        <div class="stage">
          <div class="stage-grid" />
          <ThreePtzCamera :pan="pan" :tilt="tilt" :zoom="zoom" :active="active" :seeking="cruising" />
        </div>
      </section>

      <section class="stage-card new">
        <div class="stage-meta">
          <div><span class="tag accent">新版 A</span><b>红外变速球机（立杆）</b></div>
          <small>叉臂云台 + 视场锥 + 地面投影</small>
        </div>
        <div class="stage">
          <div class="stage-glow" />
          <PtzCamera3D
            :pan="pan"
            :tilt="tilt"
            :zoom="zoom"
            :focus="focus"
            :iris="iris"
            :active="active"
            :seeking="cruising"
            :show-fov="showFovCone"
          />
        </div>
      </section>
    </div>

    <section class="controls">
      <label>
        <span>水平 <b>{{ pan.toFixed(1) }}°</b></span>
        <input v-model.number="pan" type="range" min="-180" max="180" step="0.5" />
      </label>
      <label>
        <span>俯仰 <b>{{ tilt.toFixed(1) }}°</b></span>
        <input v-model.number="tilt" type="range" min="-90" max="90" step="0.5" />
      </label>
      <label>
        <span>变倍 <b>{{ zoom.toFixed(1) }}×</b></span>
        <input v-model.number="zoom" type="range" min="1" max="20" step="0.1" />
      </label>
      <label>
        <span>聚焦 <b>{{ (focus * 100).toFixed(0) }}%</b></span>
        <input v-model.number="focus" type="range" min="0" max="1" step="0.01" />
      </label>
      <label>
        <span>光圈 <b>{{ (iris * 100).toFixed(0) }}%</b></span>
        <input v-model.number="iris" type="range" min="0" max="1" step="0.01" />
      </label>
      <div class="preset-row">
        <button @click="recallPreset(1)">预置位 1</button>
        <button @click="recallPreset(2)">预置位 2</button>
        <button @click="recallPreset(3)">预置位 3</button>
        <button :class="{ on: cruising }" @click="toggleCruise">
          {{ cruising ? "停止巡航" : "自动巡航" }}
        </button>
      </div>
    </section>
  </div>
</template>

<style scoped>
.lab {
  min-height: 100%;
  padding: 22px 24px 28px;
  color: #dbe6ef;
  background:
    radial-gradient(900px 500px at 78% 4%, rgba(56, 132, 255, 0.18), transparent 60%),
    radial-gradient(700px 420px at 8% 18%, rgba(0, 185, 214, 0.1), transparent 58%),
    #07111c;
}
.lab-header {
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  gap: 24px;
  margin-bottom: 18px;
}
.eyebrow {
  margin: 0 0 8px;
  color: #6f8aa3;
  font: 700 10px/1 "SF Mono", "Consolas", monospace;
  letter-spacing: .16em;
}
h1 {
  margin: 0;
  color: #f1f7fc;
  font-size: 25px;
  font-weight: 750;
  letter-spacing: -.02em;
}
.sub {
  margin: 7px 0 0;
  color: #8fa6b8;
  font-size: 12px;
}
.header-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 7px;
}
button {
  padding: 7px 11px;
  border: 1px solid rgba(143, 166, 184, 0.22);
  border-radius: 7px;
  color: #b9c8d6;
  background: rgba(255, 255, 255, 0.04);
  font-size: 11px;
  cursor: pointer;
  transition: border-color .18s ease, color .18s ease, background .18s ease;
}
button:hover {
  border-color: rgba(90, 160, 255, 0.55);
  color: #eaf4ff;
}
button.on {
  border-color: rgba(56, 132, 255, 0.62);
  color: #dcecff;
  background: rgba(56, 132, 255, 0.18);
}
.stages {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 16px;
}
.stage-card {
  overflow: hidden;
  border: 1px solid rgba(143, 166, 184, 0.16);
  border-radius: 14px;
  background: rgba(255, 255, 255, 0.025);
  box-shadow: 0 20px 50px rgba(0, 0, 0, 0.22);
}
.stage-meta {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 11px 14px;
  border-bottom: 1px solid rgba(143, 166, 184, 0.12);
}
.stage-meta > div {
  display: flex;
  align-items: center;
  gap: 9px;
}
.stage-meta b {
  color: #eaf3fb;
  font-size: 12px;
  font-weight: 650;
}
.stage-meta small {
  color: #6f8497;
  font-size: 10px;
}
.tag {
  padding: 3px 7px;
  border: 1px solid rgba(143, 166, 184, 0.26);
  border-radius: 5px;
  color: #8fa6b8;
  font: 700 9px/1 "SF Mono", "Consolas", monospace;
  letter-spacing: .08em;
}
.tag.accent {
  border-color: rgba(56, 132, 255, 0.42);
  color: #8dc0ff;
  background: rgba(56, 132, 255, 0.1);
}
.stage {
  position: relative;
  height: 560px;
  overflow: hidden;
  background:
    radial-gradient(circle at 50% 38%, #173451 0%, #0a1a2b 46%, #07111c 78%);
}
.stage-grid {
  position: absolute;
  inset: 0;
  opacity: .16;
  background-image:
    linear-gradient(rgba(108, 162, 221, .18) 1px, transparent 1px),
    linear-gradient(90deg, rgba(108, 162, 221, .18) 1px, transparent 1px);
  background-size: 42px 42px;
  mask-image: radial-gradient(circle at 50% 45%, #000 0%, transparent 72%);
}
.stage-glow {
  position: absolute;
  inset: 0;
  background:
    radial-gradient(420px 260px at 58% 38%, rgba(56, 132, 255, 0.14), transparent 70%),
    radial-gradient(320px 180px at 30% 72%, rgba(0, 185, 214, 0.08), transparent 70%);
}
.controls {
  display: grid;
  grid-template-columns: repeat(5, minmax(0, 1fr)) auto;
  gap: 18px;
  align-items: end;
  margin-top: 16px;
  padding: 14px 16px;
  border: 1px solid rgba(143, 166, 184, 0.14);
  border-radius: 12px;
  background: rgba(255, 255, 255, 0.025);
}
.controls label {
  display: grid;
  gap: 8px;
}
.controls label > span {
  display: flex;
  justify-content: space-between;
  color: #8fa6b8;
  font-size: 10px;
}
.controls label b {
  color: #dcecff;
  font: 600 10px "SF Mono", "Consolas", monospace;
}
input[type="range"] {
  width: 100%;
  accent-color: #3884ff;
}
.preset-row {
  display: flex;
  flex-wrap: wrap;
  gap: 7px;
  justify-content: flex-end;
}
@media (max-width: 1180px) {
  .stages { grid-template-columns: 1fr; }
  .stage { height: 480px; }
  .controls { grid-template-columns: 1fr; }
  .preset-row { justify-content: flex-start; }
}
</style>
