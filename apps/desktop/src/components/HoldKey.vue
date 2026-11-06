<script setup lang="ts">
// 云台镜头/变焦的小方键：单击一步，长按连续，松手即停。
import { onUnmounted, ref } from "vue";

const props = withDefaults(defineProps<{
  label: string;
  ariaLabel?: string;
  disabled?: boolean;
  tone?: "default" | "lens";
}>(), {
  ariaLabel: "",
  disabled: false,
  tone: "default",
});

const emit = defineEmits<{ trigger: [repeat: boolean] }>();

const pressed = ref(false);
let delayTimer = 0;
let repeatTimer = 0;

function stop() {
  window.clearTimeout(delayTimer);
  window.clearInterval(repeatTimer);
  delayTimer = 0;
  repeatTimer = 0;
  pressed.value = false;
  window.removeEventListener("pointerup", stop);
  window.removeEventListener("pointercancel", stop);
}

function start(event: PointerEvent) {
  if (props.disabled) return;
  event.preventDefault();
  stop();
  pressed.value = true;
  emit("trigger", false);
  window.addEventListener("pointerup", stop);
  window.addEventListener("pointercancel", stop);
  delayTimer = window.setTimeout(() => {
    repeatTimer = window.setInterval(() => emit("trigger", true), 60);
  }, 320);
}

onUnmounted(stop);
</script>

<template>
  <button
    type="button"
    class="hold-key"
    :class="[tone, { pressed }]"
    :aria-label="ariaLabel || label"
    :disabled="disabled"
    @pointerdown="start"
    @contextmenu.prevent
  >
    {{ label }}
  </button>
</template>

<style scoped>
.hold-key {
  display: grid;
  min-width: 34px;
  height: 30px;
  place-items: center;
  padding: 0 8px;
  border: 1px solid var(--border-default);
  border-radius: 7px;
  color: var(--text-primary);
  background: var(--bg-elevated);
  font-size: 12px;
  font-weight: 650;
  cursor: pointer;
  touch-action: none;
  user-select: none;
  transition: background .12s ease, border-color .12s ease, color .12s ease;
}
.hold-key.lens { min-width: 38px; height: 26px; font-size: 11px; }
.hold-key:hover:not(:disabled) { border-color: var(--border-accent); }
.hold-key.pressed {
  border-color: var(--accent);
  color: #fff;
  background: var(--accent);
}
.hold-key:disabled { cursor: not-allowed; opacity: .5; }
</style>
