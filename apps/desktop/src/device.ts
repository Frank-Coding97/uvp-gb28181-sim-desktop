// 单设备运行状态。持久化配置由 Rust ConfigStore 管理，这里只保留本次启动输入。
import { computed, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { usePlatform, type EffectiveDeviceConfig, type PlatformProfile } from "./platform";

export type DState = "Disconnected" | "Registering" | "Registered" | "InCall" | "Failed";

export interface StartDeviceInput {
  profile_id: string;
  password: string;
  video_source: string | null;
  catalog_template: string;
}

export interface DeviceForm {
  device_id: string;
  gb_version: "2016" | "2022";
  channel_name: string;
  video_source: string;
  catalog_template: string;
}

export interface DeviceStatus {
  running: boolean;
  capture_state: string;
  effective_config: EffectiveDeviceConfig | null;
}

const form = ref<DeviceForm>({
  device_id: "",
  gb_version: "2022",
  channel_name: "",
  video_source: "",
  catalog_template: "",
});
const deviceState = ref<DState>("Disconnected");
const startedAt = ref<number | null>(null);
const deviceLive = ref(false);
const effectiveConfig = ref<EffectiveDeviceConfig | null>(null);
const { config } = usePlatform();
watch(config, (next) => {
  if (!next) return;
  form.value.device_id = next.device.device_id;
  form.value.channel_name = next.device.channel_name;
  const active = next.profiles.find((profile) => profile.id === next.active_profile_id);
  form.value.gb_version = active?.gb_version === "V2016" ? "2016" : "2022";
}, { immediate: true });

// 媒体源与目录模板只属于本次启动会话；保留旧调用点但不再持久化。
export function persistForm() {}

export function useDevice() {
  const statusMeta = computed(() => {
    switch (deviceState.value) {
      case "Registering": return { text: "注册中", color: "var(--warning)" };
      case "Registered":  return { text: "已注册", color: "var(--success)" };
      case "InCall":      return { text: "推流中", color: "var(--accent)" };
      case "Failed":      return { text: "注册失败", color: "var(--error)" };
      default:            return { text: "未连接", color: "var(--text-tertiary)" };
    }
  });
  const startDisabled = computed(() => deviceLive.value);
  const stopDisabled = computed(() => !deviceLive.value);
  const canReport = computed(
    () => deviceState.value === "Registered" || deviceState.value === "InCall"
  );

  async function startDevice(
    platform: PlatformProfile | undefined,
    password = "",
  ): Promise<{ ok: boolean; msg: string }> {
    if (!platform) return { ok: false, msg: "请先配置目标平台" };
    if (platform.transport === "Tcp") return { ok: false, msg: "信令 TCP 尚未实现，请改用 UDP" };
    const input: StartDeviceInput = {
      profile_id: platform.id,
      password,
      video_source: form.value.video_source.trim() || null,
      catalog_template: form.value.catalog_template,
    };
    try {
      const msg = await invoke<string>("start_device", { input });
      deviceLive.value = true;
      return { ok: true, msg };
    } catch (cause) {
      deviceLive.value = false;
      effectiveConfig.value = null;
      deviceState.value = "Failed";
      return { ok: false, msg: String(cause) };
    }
  }

  async function stopDevice(): Promise<{ ok: boolean; msg: string }> {
    try {
      await invoke<string>("stop_device");
      deviceLive.value = false;
      effectiveConfig.value = null;
      deviceState.value = "Disconnected";
      startedAt.value = null;
      return { ok: true, msg: "设备已停止" };
    } catch (cause) {
      return { ok: false, msg: String(cause) };
    }
  }

  async function reconcile(): Promise<DeviceStatus | null> {
    try {
      const status = await invoke<DeviceStatus>("get_device_status");
      deviceLive.value = status.running;
      effectiveConfig.value = status.effective_config;
      if (!status.running && deviceState.value !== "Disconnected") {
        deviceState.value = "Disconnected";
        startedAt.value = null;
      }
      return status;
    } catch {
      return null;
    }
  }

  return {
    form, deviceState, startedAt, deviceLive, effectiveConfig,
    statusMeta, startDisabled, stopDisabled, canReport,
    startDevice, stopDevice, reconcile,
  };
}
