<script setup lang="ts">
// 云台控制卡片:3D 球机舞台(罗盘/俯仰标尺/方向 HUD)+ 右侧姿态与预置位读数。
// 姿态由 usePtzPose 从后端 ptz_action / ptz_preset 事件积分得出,本组件只渲染。
import { onMounted, onUnmounted } from "vue";
import ThreePtzCamera from "./ThreePtzCamera.vue";
import { PRESET_IDS, usePtzPose } from "../composables/usePtzPose";
import { useDevice } from "../device";

// 设备未启动时整卡进入 idle 态。
//
// 之前无论设备有没有起来,3D 舞台、罗盘、姿态读数、预置位芯片都照常渲染,
// 显示的是默认值 —— 看起来像"在运行",而平台其实根本没连上。
// 这是最容易误导用户的一种缺失状态。
const { deviceLive } = useDevice();

const {
  ptz, pan, tilt, zoom, seeking, activePreset, presetNames,
  lastPtzAt, ptzActive, moving, ptzButtons,
  zoomText, dirText, speedText, statusText,
  normalizedPan, compassDialStyle, tiltMarkerStyle, poseText,
  startPtzTracking, stopPtzTracking,
} = usePtzPose();

onMounted(startPtzTracking);
onUnmounted(stopPtzTracking);
</script>

<template>
  <div class="glass-card console-panel ptz-panel" :class="{ idle: !deviceLive }">
    <div class="ptz-head">
      <div class="panel-title">云台控制</div>
      <div class="ptz-sub">
        {{ deviceLive ? '平台下发 PTZ 时机芯与光学变倍实时联动' : '设备未启动,以下为静态示意' }}
      </div>
    </div>
    <!-- 未启动蒙层:说清"这不是实时数据"以及下一步做什么。 -->
    <div v-if="!deviceLive" class="ptz-idle-note">
      设备未启动,云台读数不是实时值。请在上方「注册」按钮启动设备,平台下发 PTZ 后此处会实时联动。
    </div>
    <div class="ptz-body">
      <div class="ptz-stage" :class="{ active: ptzActive, seeking }">
        <div class="stage-grid"></div>
        <!-- 左上角 PTZ LIVE 标识 -->
        <div class="stage-caption">
          <span class="live-dot"></span>
          <b>PTZ LIVE</b>
        </div>
        <!-- 右上角 对称呼应的运行状态胶囊 -->
        <div class="stage-caption stage-caption-right" :class="{ active: ptzActive, seeking }">
          <span class="motion-dot"></span>
          <b>{{ statusText }}</b>
        </div>

        <div class="compass-ring" aria-hidden="true">
          <div class="compass-dial" :style="compassDialStyle">
            <span class="north">N</span><span class="east">E</span><span class="south">S</span><span class="west">W</span>
          </div>
          <b class="heading-readout">{{ normalizedPan.toFixed(0).padStart(3, "0") }}°</b>
        </div>
        <div class="direction-hud" aria-hidden="true">
          <span class="hud-up" :class="{ on: ptz.up }">↑</span>
          <span class="hud-down" :class="{ on: ptz.down }">↓</span>
          <span class="hud-left" :class="{ on: ptz.left }">←</span>
          <span class="hud-right" :class="{ on: ptz.right }">→</span>
        </div>
        <div class="tilt-gauge" aria-hidden="true">
          <span class="tilt-plus">+90°</span><span class="tilt-zero">0°</span><span class="tilt-minus">−90°</span>
          <i class="tilt-track"></i>
          <b class="tilt-marker" :style="tiltMarkerStyle">{{ tilt.toFixed(0) }}°</b>
        </div>
        <ThreePtzCamera :pan="pan" :tilt="tilt" :zoom="zoom" :active="ptzActive" :seeking="seeking" />
        <div class="axis-label pan-axis">PAN 360°</div>
        <div class="axis-label tilt-axis">TILT ±90°</div>
      </div>
      <div class="ptz-info">
        <div class="ptz-stat-grid">
          <div class="ptz-stat"><span>朝向</span><b class="pose">{{ poseText }}</b></div>
          <div class="ptz-stat"><span>方向</span><b :class="{ hot: seeking || moving }">{{ dirText }}</b></div>
          <div class="ptz-stat"><span>变倍</span><b :class="{ hot: ptz.zoom_in || ptz.zoom_out }">{{ zoomText }}</b></div>
          <div class="ptz-stat"><span>速度</span><b :class="{ hot: moving || ptz.zoom_in || ptz.zoom_out }">{{ speedText }}</b></div>
        </div>
        <div class="ptz-stat action-state">
          <span>平台命令 · {{ lastPtzAt }}</span>
          <span class="action-chips">
            <b
              v-for="button in ptzButtons"
              :key="button.key"
              class="action-chip"
              :class="{ on: button.active, stop: button.key === 'stop' }"
            >{{ button.label }}</b>
          </span>
        </div>
        <div class="ptz-stat presets">
          <span>预置位</span>
          <span class="preset-chips">
            <b
              v-for="id in PRESET_IDS"
              :key="id"
              class="pchip"
              :class="{ on: activePreset === id && seeking }"
            ><small>{{ id }}</small>{{ presetNames[id] }}</b>
          </span>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* 3D 舞台的高光强度随主题走:暗色下 35% 白会在深底上拉出一道亮边。 */
.ptz-panel { --stage-gloss: rgba(255, 255, 255, 0.35); box-sizing: border-box; display: flex; flex-direction: column; }
:root[data-theme="dark"] .ptz-panel { --stage-gloss: rgba(255, 255, 255, 0.06); }
.ptz-head {
  display: flex; align-items: baseline; justify-content: space-between;
  gap: 8px; margin-bottom: 6px; flex: 0 0 auto;
}
.ptz-head .panel-title { margin-bottom: 0; }
.ptz-sub { font-size: var(--fs-sm); color: var(--text-tertiary); }
.ptz-body { flex: 1 1 0; min-height: 0; display: flex; gap: 10px; align-items: stretch; }

/* 桌面球机 3D 舞台:弹性自动放大,占卡片主要宽度并随窗口舒展 */
.ptz-stage {
  position: relative; flex: 1.35 1 280px; height: 100%; min-height: 140px; overflow: hidden;
  border-radius: var(--radius-sm); perspective: 900px; isolation: isolate;
  background:
    radial-gradient(circle at 50% 38%, var(--accent-glow), transparent 31%),
    linear-gradient(155deg, var(--bg-elevated), var(--bg-card));
  border: 1px solid var(--border-default);
  box-shadow: inset 0 1px 0 var(--stage-gloss), inset 0 -16px 32px rgba(57,87,126,.07);
}
.stage-grid {
  position: absolute; inset: 54% -12% -36%; transform: rotateX(65deg); transform-origin: center top;
  background-image: linear-gradient(rgba(68,111,166,.1) 1px, transparent 1px), linear-gradient(90deg, rgba(68,111,166,.1) 1px, transparent 1px);
  background-size: 18px 18px; mask-image: linear-gradient(to bottom, #000, transparent 82%);
}
.stage-caption {
  position: absolute; z-index: 9; left: 6px; top: 6px; display: flex; align-items: center; gap: 4px;
  padding: 2px 6px; border-radius: 5px; color: var(--text-secondary); background: var(--bg-elevated);
  border: 1px solid var(--border-default); box-shadow: 0 2px 6px rgba(39,67,103,.05);
}
.stage-caption.stage-caption-right { left: auto; right: 6px; }
.stage-caption.stage-caption-right.active {
  color: var(--accent); border-color: var(--border-accent); background: var(--bg-hover);
}
.stage-caption .live-dot { width: 5px; height: 5px; border-radius: 50%; background: var(--success); box-shadow: 0 0 5px var(--success); }
.stage-caption .motion-dot { width: 5px; height: 5px; border-radius: 50%; background: var(--text-tertiary); }
.stage-caption.active .motion-dot { background: var(--accent); box-shadow: 0 0 5px var(--accent); }
.stage-caption b { font: 800 8px/1 "SF Mono", Menlo, monospace; letter-spacing: .5px; }
.ptz-stage.active .stage-caption .live-dot { background: var(--accent); box-shadow: 0 0 7px var(--accent); animation: liveBlink 1s ease-in-out infinite; }
@keyframes liveBlink { 50% { opacity: .35; } }

.compass-ring {
  position: absolute; z-index: 2; left: 50%; top: 64%; width: 170px; height: 58px;
  margin-left: -85px; border-radius: 50%; transform: rotateX(68deg);
  border: 1.5px solid var(--border-accent); box-shadow: 0 0 0 5px var(--stage-gloss), inset 0 0 14px var(--accent-dim);
}
.compass-dial {
  position: absolute; inset: 3px; border-radius: 50%; transition: transform .12s linear;
  background: repeating-conic-gradient(from -1deg, rgba(61,105,158,.58) 0 2deg, transparent 2deg 10deg);
}
.compass-dial::after { content: ""; position: absolute; inset: 27%; border-radius: 50%; border: 1px dashed var(--border-accent); }
.compass-dial span { position: absolute; color: var(--text-secondary); font-size: var(--fs-xs); font-weight: 800; }
.compass-dial .north { left: 50%; top: -2px; transform: translateX(-50%); color: var(--accent); }
.compass-dial .east { right: 2px; top: 50%; transform: translateY(-50%); }
.compass-dial .south { left: 50%; bottom: -2px; transform: translateX(-50%); }
.compass-dial .west { left: 2px; top: 50%; transform: translateY(-50%); }
.heading-readout {
  position: absolute; z-index: 2; left: 50%; top: 50%; transform: translate(-50%, -50%) rotateX(-68deg);
  color: var(--accent); font: 800 9.5px/1 "SF Mono", Menlo, monospace; background: var(--bg-elevated);
  padding: 2px 5px; border-radius: 5px; box-shadow: 0 2px 5px rgba(43,68,100,.12);
}
.direction-hud span {
  position: absolute; z-index: 10; display: grid; place-items: center; width: 18px; height: 18px;
  border-radius: 4px; color: var(--text-tertiary); background: var(--bg-surface); border: 1px solid var(--border-default);
  font-size: var(--fs-sm); transition: .15s ease;
}
.direction-hud span.on { color: #fff; background: var(--accent); box-shadow: 0 0 10px var(--accent-glow); transform: scale(1.1); }
.hud-up { top: 6px; left: calc(50% - 9px); }
.hud-down { bottom: 6px; left: calc(50% - 9px); }
.hud-left { left: 6px; top: calc(50% - 9px); }
.hud-right { right: 6px; top: calc(50% - 9px); }
.tilt-gauge {
  position: absolute; z-index: 9; right: 10px; top: 38px; width: 32px; height: 68px;
  color: var(--text-tertiary); font: 700 7px/1 "SF Mono", Menlo, monospace;
}
.tilt-gauge > span { position: absolute; right: 0; }
.tilt-plus { top: 0; }
.tilt-zero { top: 32px; }
.tilt-minus { bottom: 0; }
.tilt-track { position: absolute; left: 3px; top: 2px; width: 2px; height: 64px; border-radius: 2px; background: linear-gradient(var(--accent), var(--text-tertiary)); }
.tilt-track::before, .tilt-track::after { content: ""; position: absolute; left: -2px; width: 5px; height: 1px; background: var(--text-tertiary); }
.tilt-track::before { top: 0; }
.tilt-track::after { bottom: 0; }
.tilt-marker {
  position: absolute; left: -1px; top: 29px; min-width: 22px; height: 12px; padding-left: 6px; border-radius: 4px;
  color: #fff; background: var(--accent); font-size: var(--fs-xs); line-height: 12px; box-shadow: 0 2px 5px var(--accent-glow);
  transition: transform .12s linear;
}
.tilt-marker::before {
  content: ""; position: absolute; left: -2px; top: 3.5px; border-width: 2.5px 3px 2.5px 0;
  border-style: solid; border-color: transparent var(--accent) transparent transparent;
}
.axis-label { position: absolute; z-index: 8; color: var(--text-tertiary); font: 700 7px/1 "SF Mono", Menlo, monospace; letter-spacing: .3px; }
.pan-axis { left: 8px; bottom: 8px; }
.tilt-axis { right: 8px; bottom: 8px; }
.ptz-stage.active { box-shadow: inset 0 1px 0 var(--stage-gloss), inset 0 -16px 32px rgba(57,87,126,.07), 0 0 0 2px var(--accent-dim); }
.ptz-stage.seeking { animation: stageSeekPulse 1.15s ease-in-out infinite; }
@keyframes stageSeekPulse {
  50% { box-shadow: inset 0 1px 0 var(--stage-gloss), inset 0 -16px 32px rgba(57,87,126,.07), 0 0 16px var(--accent-glow); }
}

/* 云台右侧参数信息:顶部对齐、2x2 指标卡、下部预置位卡片占满剩余空间 */
.ptz-info {
  flex: 1 1 210px; display: flex; flex-direction: column; justify-content: flex-start;
  gap: 6px; min-width: 0; overflow: hidden; height: 100%;
}
.ptz-stat-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 5px; flex: 0 0 auto; }
.ptz-stat {
  display: flex; flex-direction: column; gap: 2px; min-width: 0; padding: 5px 8px; border-radius: var(--radius-xs);
  background: var(--bg-card); border: 1px solid var(--border-default);
}
.ptz-stat span { font-size: var(--fs-xs); color: var(--text-tertiary); width: auto; flex-shrink: 0; text-transform: uppercase; letter-spacing: .2px; }
.ptz-stat b { font-size: var(--fs-sm); color: var(--text-primary); min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.ptz-stat b.hot { color: var(--accent); }
.ptz-stat b.pose { font-variant-numeric: tabular-nums; font-size: var(--fs-sm); }
.ptz-stat.action-state { flex: 0 0 auto; }

.action-chips { display: flex; flex-wrap: wrap; gap: 3px; margin-top: 2px; }
.action-chip {
  display: inline-flex; align-items: center; justify-content: center; min-width: 24px; height: 17px; padding: 0 4px;
  border-radius: 4px; color: var(--text-tertiary); background: var(--bg-surface);
  border: 1px solid var(--border-default); font-size: var(--fs-xs); transition: all .15s ease;
}
.action-chip.on { color: #fff; background: var(--accent); border-color: var(--accent); box-shadow: 0 0 6px var(--accent-glow); transform: translateY(-1px); }
.action-chip.stop.on { background: var(--text-secondary); border-color: var(--text-secondary); box-shadow: 0 0 6px rgba(100,116,139,.2); }

/* 预置位卡片:自适应占满剩余垂直空间 */
.ptz-stat.presets { flex: 1 1 0; min-height: 0; display: flex; flex-direction: column; }
.preset-chips { display: flex; gap: 4px; flex-wrap: wrap; align-content: flex-start; margin-top: 3px; overflow-y: auto; }
.pchip {
  display: inline-flex; align-items: center; justify-content: center; gap: 3px;
  min-width: 58px; height: 20px; padding: 0 5px; border-radius: 4px;
  font-size: var(--fs-xs); font-weight: 600; color: var(--text-secondary);
  background: var(--bg-surface); border: 1px solid var(--border-default); transition: all 0.15s;
}
.pchip small {
  display: inline-grid; place-items: center; width: 13px; height: 13px; border-radius: 3px;
  color: var(--accent); background: var(--accent-dim); font: 700 8px/1 "SF Mono", Menlo, monospace;
}
.pchip.on { color: #fff; background: var(--accent); border-color: var(--accent); box-shadow: 0 0 6px var(--accent-glow); transform: translateY(-1px); }
.pchip.on small { color: var(--accent); background: #fff; }

@media (max-width: 1080px) {
  .ptz-panel { min-height: 280px; }
}
@media (max-width: 760px) {
  .ptz-body { flex-direction: column; }
  .ptz-stage { flex-basis: 200px; width: 100%; }
}
/* 未启动态:整卡降饱和 + 蒙层提示,避免把默认值误读成实时数据。 */
.ptz-panel.idle .ptz-body {
  filter: saturate(0.25) opacity(0.55);
  pointer-events: none;
}
.ptz-idle-note {
  margin: var(--sp-2) 0 var(--sp-1);
  padding: var(--sp-2) var(--sp-3);
  border: 1px dashed var(--border-default);
  border-radius: var(--radius-xs);
  background: var(--bg-surface);
  font-size: var(--fs-sm);
  line-height: var(--lh-normal);
  color: var(--text-secondary);
}
@media (prefers-reduced-motion: reduce) {
  .ptz-panel.idle .ptz-body { filter: opacity(0.6); }
}
</style>
