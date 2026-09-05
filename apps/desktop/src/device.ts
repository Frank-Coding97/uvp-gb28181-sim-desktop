// 设备状态 + 配置 + 注册/注销逻辑。表单使用 V1 envelope 持久化，写失败不会发 IPC。
import { computed, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import type { PlatformProfile } from "./platform";
import { formatError } from "./lib/ipcError";
import {
  createVersionedStorage,
  type DecodeResult,
  type StorageSaveResult,
} from "./lib/versionedStorage";

export type DState = "Disconnected" | "Registering" | "Registered" | "InCall" | "Failed";
export type GbVersion = "2016" | "2022";

export interface DeviceForm {
  device_id: string;
  gb_version: GbVersion;
  channel_name: string;
  video_source: string;
  catalog_template: string;
  manufacturer?: string;
  model?: string;
  firmware?: string;
  heartbeat_interval?: number;
  max_heartbeat_timeout?: number;
  expires?: number;
  video_fps?: number;
}

const FORM_KEY = "uvp_device_form";

function defaultForm(): DeviceForm {
  return {
    device_id: "35020000001310000001",
    gb_version: "2022",
    channel_name: "Camera-1",
    video_source: "",
    catalog_template: "",
    manufacturer: "UVP-Sim",
    model: "IPC-GB28181",
    firmware: "V2.4.0-R2026",
    heartbeat_interval: 60,
    max_heartbeat_timeout: 3,
    expires: 3600,
    video_fps: 25,
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function boundedString(value: unknown, fallback: string, maxLength: number, required = false): string {
  if (typeof value !== "string" || value.length > maxLength || (required && !value.trim())) return fallback;
  return value;
}

function boundedInteger(value: unknown, fallback: number, min: number, max: number): number {
  return Number.isFinite(value) && Number.isInteger(value) && (value as number) >= min && (value as number) <= max
    ? value as number
    : fallback;
}

function decodeForm(value: unknown): DecodeResult<DeviceForm> {
  if (!isRecord(value)) throw new Error("invalid device form");
  const defaults = defaultForm();
  const deviceId = typeof value.device_id === "string" && /^\d{20}$/.test(value.device_id)
    ? value.device_id
    : defaults.device_id;
  const gbVersion: GbVersion = value.gb_version === "2016" || value.gb_version === "2022"
    ? value.gb_version
    : defaults.gb_version;
  const data: DeviceForm = {
    device_id: deviceId,
    gb_version: gbVersion,
    channel_name: boundedString(value.channel_name, defaults.channel_name, 128, true),
    video_source: boundedString(value.video_source, defaults.video_source, 2048),
    catalog_template: boundedString(value.catalog_template, defaults.catalog_template, 256),
    manufacturer: boundedString(value.manufacturer, defaults.manufacturer!, 128),
    model: boundedString(value.model, defaults.model!, 128),
    firmware: boundedString(value.firmware, defaults.firmware!, 128),
    heartbeat_interval: boundedInteger(value.heartbeat_interval, defaults.heartbeat_interval!, 5, 300),
    max_heartbeat_timeout: boundedInteger(value.max_heartbeat_timeout, defaults.max_heartbeat_timeout!, 1, 10),
    expires: boundedInteger(value.expires, defaults.expires!, 60, 86400),
    video_fps: boundedInteger(value.video_fps, defaults.video_fps!, 1, 120),
  };
  return {
    data,
    repaired: JSON.stringify(value) !== JSON.stringify(data),
    summary: "设备表单中的无效或未知字段已修复",
  };
}

const formStorage = createVersionedStorage<DeviceForm>({
  key: FORM_KEY,
  schemaVersion: 1,
  defaultValue: defaultForm,
  decode: decodeForm,
  migrate(value, fromVersion) {
    if (fromVersion !== null) throw new Error("unsupported device version");
    const decoded = decodeForm(value);
    return { ...decoded, summary: "旧版设备表单已迁移到当前版本" };
  },
});

const initial = formStorage.load();
const form = ref<DeviceForm>(initial.data);
const deviceState = ref<DState>("Disconnected");
const startedAt = ref<number | null>(null);
const deviceLive = ref(false);
const busy = ref(false);

/** 安全持久化当前表单。失败以结果返回，永不向调用方抛出。 */
export function persistForm(): StorageSaveResult {
  try {
    const candidate = decodeForm(form.value).data;
    const result = formStorage.save(candidate);
    if (result.ok) form.value = candidate;
    return result;
  } catch {
    return { ok: false, reason: "storage", category: "set" };
  }
}

export function useDevice() {
  const statusMeta = computed(() => {
    switch (deviceState.value) {
      case "Registering": return { text: "注册中", color: "var(--warning)" };
      case "Registered": return { text: "已注册", color: "var(--success)" };
      case "InCall": return { text: "推流中", color: "var(--accent)" };
      case "Failed": return { text: "注册失败", color: "var(--error)" };
      default: return { text: "未连接", color: "var(--text-tertiary)" };
    }
  });
  const startDisabled = computed(() => deviceLive.value || busy.value);
  const stopDisabled = computed(() => !deviceLive.value || busy.value);
  const startingUp = computed(() => busy.value);
  const canReport = computed(
    () => deviceState.value === "Registered" || deviceState.value === "InCall",
  );

  async function startDevice(platform: PlatformProfile | undefined): Promise<{ ok: boolean; msg: string }> {
    if (!platform) return { ok: false, msg: "请先在顶栏配置目标平台" };
    if (deviceLive.value) return { ok: false, msg: "设备实例已在运行" };
    if (busy.value) return { ok: false, msg: "正在处理上一次操作,请稍候" };
    busy.value = true;
    try {
      const persisted = persistForm();
      if (!persisted.ok) {
        return { ok: false, msg: "设备配置保存失败，未启动设备" };
      }
      const current = form.value;
      const config = {
        profile_id: platform.id,
        server_host: platform.server_host,
        server_port: platform.server_port,
        server_domain: platform.server_domain,
        inbound_identity_mode: platform.inbound_identity_mode,
        transport: platform.transport,
        signaling_encoding: platform.signaling_encoding ?? "GB18030",
        device_id: current.device_id,
        gb_version: current.gb_version,
        channel_name: current.channel_name,
        video_source: current.video_source.trim() || null,
        catalog_template: current.catalog_template,
        manufacturer: current.manufacturer,
        model: current.model,
        firmware: current.firmware,
        heartbeat_interval: current.heartbeat_interval,
        max_heartbeat_timeout: current.max_heartbeat_timeout,
        expires: current.expires,
        video_fps: current.video_fps,
      };
      const msg = await invoke<string>("start_device", { config });
      deviceLive.value = true;
      return { ok: true, msg };
    } catch (error) {
      deviceLive.value = false;
      deviceState.value = "Failed";
      return { ok: false, msg: formatError(error, "设备启动失败") };
    } finally {
      busy.value = false;
    }
  }

  async function stopDevice(): Promise<{ ok: boolean; msg: string }> {
    if (busy.value) return { ok: false, msg: "正在处理上一次操作,请稍候" };
    busy.value = true;
    try {
      await invoke<string>("stop_device");
      deviceLive.value = false;
      deviceState.value = "Disconnected";
      startedAt.value = null;
      return { ok: true, msg: "设备已停止" };
    } catch (error) {
      return { ok: false, msg: formatError(error, "设备停止失败") };
    } finally {
      busy.value = false;
    }
  }

  async function reconcile() {
    try {
      const status = await invoke<{ running: boolean }>("get_device_status");
      deviceLive.value = status.running;
      if (!status.running && deviceState.value !== "Disconnected") {
        deviceState.value = "Disconnected";
        startedAt.value = null;
      }
    } catch {
      // 后端暂不可用不改变当前 UI 状态。
    }
  }

  return {
    form, deviceState, startedAt, deviceLive,
    statusMeta, startDisabled, stopDisabled, startingUp, canReport,
    startDevice, stopDevice, reconcile,
  };
}
