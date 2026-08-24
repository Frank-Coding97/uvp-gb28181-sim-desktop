// 设备状态 + 配置 + 注册/注销逻辑:单例共享,顶栏(App.vue)与单设备页(Device.vue)共用。
// 提到 store 是为了让顶栏的"注册/注销"按钮在任意页面都能操作同一台设备,
// 且与单设备页的配置表单是同一份数据(改配置→顶栏注册,行为一致)。
import { ref, computed } from "vue";
import { invoke } from "@tauri-apps/api/core";
import type { PlatformProfile } from "./platform";

export type DState = "Disconnected" | "Registering" | "Registered" | "InCall" | "Failed";

export interface DeviceForm {
  device_id: string;
  gb_version: string;
  channel_name: string;
  video_source: string;
  catalog_template: string;
}

const FORM_KEY = "uvp_device_form";

function defaultForm(): DeviceForm {
  return {
    device_id: "35020000001310000001",
    gb_version: "2022",
    channel_name: "Camera-1",
    video_source: "",
    catalog_template: "",
  };
}

function loadForm(): DeviceForm {
  try {
    const raw = localStorage.getItem(FORM_KEY);
    if (raw) return { ...defaultForm(), ...JSON.parse(raw) };
  } catch {
    // 忽略损坏数据
  }
  return defaultForm();
}

// 单例响应式状态(整个应用共享一份)。
const form = ref<DeviceForm>(loadForm());
// 设备当前状态灯(由后端 device_state 事件驱动;App.vue 订阅后写入)。
const deviceState = ref<DState>("Disconnected");
// 注册起始时刻(在线时长基准)。
const startedAt = ref<number | null>(null);
// 引擎里是否存在设备实例(与状态灯解耦):设备后台在跑(哪怕在重试注册)即为 true。
const deviceLive = ref(false);

export function persistForm() {
  localStorage.setItem(FORM_KEY, JSON.stringify(form.value));
}

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
  // "注册"禁用:设备实例存在时禁用(避免重复启动)。
  const startDisabled = computed(() => deviceLive.value);
  // "注销"禁用:设备实例不存在时禁用。注册失败/重试中设备仍在跑,注销可点。
  const stopDisabled = computed(() => !deviceLive.value);
  const canReport = computed(
    () => deviceState.value === "Registered" || deviceState.value === "InCall"
  );

  // 注册上线。返回 {ok, msg}:调用方决定如何提示(顶栏走错误条,页面走 message)。
  async function startDevice(platform: PlatformProfile | undefined): Promise<{ ok: boolean; msg: string }> {
    if (!platform) return { ok: false, msg: "请先在顶栏配置目标平台" };
    persistForm();
    try {
      const config = {
        server_host: platform.server_host, server_port: platform.server_port,
        // Rust 注册链当前字段名仍为 server_domain，实际承载 20 位服务器 ID。
        server_domain: platform.server_id, password: platform.password,
        transport: platform.transport,
        signaling_encoding: platform.signaling_encoding ?? "GB18030",
        ...form.value,
        video_source: form.value.video_source.trim() || null,
      };
      const msg = await invoke<string>("start_device", { config });
      deviceLive.value = true;
      return { ok: true, msg };
    } catch (e) {
      // 启动本身失败(如配置非法):后端未留下实例,保持可重新启动。
      deviceLive.value = false;
      deviceState.value = "Failed";
      return { ok: false, msg: String(e) };
    }
  }

  async function stopDevice(): Promise<{ ok: boolean; msg: string }> {
    try {
      await invoke<string>("stop_device");
      deviceLive.value = false;
      deviceState.value = "Disconnected";
      startedAt.value = null;
      return { ok: true, msg: "设备已停止" };
    } catch (e) {
      return { ok: false, msg: String(e) };
    }
  }

  // 与引擎对账:以引擎真实状态为准同步 deviceLive(切页/重启后仍准确)。
  async function reconcile() {
    try {
      const st = await invoke<{ running: boolean }>("get_device_status");
      deviceLive.value = st.running;
      if (!st.running && deviceState.value !== "Disconnected") {
        deviceState.value = "Disconnected";
        startedAt.value = null;
      }
    } catch {
      // 忽略
    }
  }

  return {
    form, deviceState, startedAt, deviceLive,
    statusMeta, startDisabled, stopDisabled, canReport,
    startDevice, stopDevice, reconcile,
  };
}
