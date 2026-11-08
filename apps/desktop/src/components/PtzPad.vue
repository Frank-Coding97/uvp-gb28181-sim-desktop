<script setup lang="ts">
// 移动端同款云台方盘：按下走一步，长按连续，松手即停。
import { onUnmounted, ref } from "vue";

const props = withDefaults(defineProps<{ disabled?: boolean }>(), { disabled: false });
const emit = defineEmits<{ adjust: [dPan: number, dTilt: number] }>();

const STEP = 5;
const CONT_STEP = 1;
const REPEAT_DELAY = 320;
const REPEAT_INTERVAL = 60;

type Key = { dPan: number; dTilt: number; glyph: string; label: string };
const keys: Array<Key | null> = [
  { dPan: -STEP, dTilt: STEP, glyph: "↖", label: "左上" },
  { dPan: 0, dTilt: STEP, glyph: "↑", label: "向上" },
  { dPan: STEP, dTilt: STEP, glyph: "↗", label: "右上" },
  { dPan: -STEP, dTilt: 0, glyph: "←", label: "向左" },
  null,
  { dPan: STEP, dTilt: 0, glyph: "→", label: "向右" },
  { dPan: -STEP, dTilt: -STEP, glyph: "↙", label: "左下" },
  { dPan: 0, dTilt: -STEP, glyph: "↓", label: "向下" },
  { dPan: STEP, dTilt: -STEP, glyph: "↘", label: "右下" },
];

const active = ref<Key | null>(null);
let delayTimer = 0;
let repeatTimer = 0;

function stop() {
  window.clearTimeout(delayTimer);
  window.clearInterval(repeatTimer);
  delayTimer = 0;
  repeatTimer = 0;
  active.value = null;
  window.removeEventListener("pointerup", stop);
  window.removeEventListener("pointercancel", stop);
}

function start(key: Key, event: PointerEvent) {
  if (props.disabled) return;
  stop();
  event.preventDefault();
  active.value = key;
  emit("adjust", key.dPan, key.dTilt);
  window.addEventListener("pointerup", stop);
  window.addEventListener("pointercancel", stop);
  delayTimer = window.setTimeout(() => {
    repeatTimer = window.setInterval(() => {
      emit("adjust", Math.sign(key.dPan) * CONT_STEP, Math.sign(key.dTilt) * CONT_STEP);
    }, REPEAT_INTERVAL);
  }, REPEAT_DELAY);
}

onUnmounted(stop);
</script>

<template>
  <div class="ptz-pad" :class="{ disabled }">
    <template v-for="(key, index) in keys" :key="index">
      <div v-if="!key" class="pad-center" aria-hidden="true"><i /><b /></div>
      <button
        v-else
        type="button"
        class="pad-key"
        :class="{ active: active === key }"
        :aria-label="key.label"
        :disabled="disabled"
        @pointerdown="start(key, $event)"
        @contextmenu.prevent
      >
        {{ key.glyph }}
      </button>
    </template>
  </div>
</template>

<style scoped>
.ptz-pad {
  display: grid;
  grid-template-columns: repeat(3, 44px);
  gap: 5px;
  padding: 6px;
  width: max-content;
  border: 1px solid var(--border-default);
  border-radius: 14px;
  background: rgba(255, 255, 255, 0.42);
  box-shadow: 0 2px 10px rgba(16, 46, 98, .06);
}
.pad-key {
  display: grid;
  width: 44px;
  height: 44px;
  place-items: center;
  border: 1px solid var(--border-default);
  border-radius: 9px;
  color: var(--text-primary);
  background: var(--bg-elevated);
  font-size: 17px;
  font-weight: 650;
  cursor: pointer;
  touch-action: none;
  user-select: none;
  transition: background .12s ease, border-color .12s ease, color .12s ease, box-shadow .12s ease;
  box-shadow: 0 1px 3px rgba(16, 46, 98, .07);
}
.pad-key:hover:not(:disabled) {
  border-color: var(--border-accent);
  background: var(--bg-hover);
}
.pad-key.active {
  border-color: var(--accent);
  color: #fff;
  background: var(--accent);
}
.pad-key:disabled { cursor: not-allowed; opacity: .5; }
.pad-center {
  display: grid;
  width: 44px;
  height: 44px;
  place-items: center;
}
.pad-center i {
  position: absolute;
  width: 20px;
  height: 20px;
  border-radius: 50%;
  background: var(--accent-pale);
}
.pad-center b {
  position: absolute;
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--accent);
}
.ptz-pad.disabled { opacity: .6; }
</style>
