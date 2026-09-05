// 桌面端配置的唯一持久化真相位于 Rust ConfigStore。
// 平台密码与其它联调参数一并保存在 Rust ConfigStore；localStorage 只用于一次性迁移。
import { computed, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import {
  DEFAULT_MEDIA_PROFILE,
  normalizeMediaProfile,
  type MediaProfile,
} from "./media-profile.ts";

export type SignalingTransport = "UDP" | "TCP";
export type GbVersion = "V2016" | "V2022";
export type SignalingEncoding = "Gb18030" | "Utf8";
export type BindMode = "auto" | "specific";

export interface PlatformProfile {
  id: string;
  name: string;
  server_host: string;
  server_port: number;
  server_id: string;
  server_domain: string;
  password: string;
  transport: SignalingTransport;
  gb_version: GbVersion;
  signaling_encoding: SignalingEncoding;
}

export interface DeviceSettings {
  device_id: string;
  device_name: string;
  manufacturer: string;
  model: string;
  firmware: string;
  channel_name: string;
  register_expires_secs: number;
  heartbeat_interval_secs: number;
  heartbeat_fail_threshold: number;
}

export interface NetworkSettings {
  bind_mode: BindMode;
  bind_address: string;
  sip_trace: boolean;
}

export interface CatalogNodeConfig {
  id: string;
  node_type: "AdministrativeRegion" | "System" | "Device" | "BusinessGroup" | "VirtualOrg" | "VideoChannel" | "AlarmChannel";
  name: string;
  parent_id: string;
  civil_code?: string | null;
  business_group_id?: string | null;
  status: "ON" | "OFF";
}

export interface DesktopConfigV2 {
  schema_version: 2;
  config_revision: number;
  media: MediaProfile;
  active_profile_id: string;
  profiles: PlatformProfile[];
  device: DeviceSettings;
  network: NetworkSettings;
  catalog_tree: CatalogNodeConfig[];
}

/** @deprecated Rust 配置已统一为 V2；保留类型别名兼容旧的前端调用方。 */
export type DesktopConfigV1 = DesktopConfigV2;

export interface CapabilitySnapshot {
  signaling_udp: boolean;
  signaling_tcp: boolean;
}

export interface EffectiveDeviceConfig {
  media: MediaProfile;
  profile: PlatformProfile;
  device: DeviceSettings;
  network: NetworkSettings;
  local_host: string;
  local_port: number;
  video_source: string | null;
  catalog_template: string;
  capabilities: CapabilitySnapshot;
}

type LegacyProfile = {
  id?: unknown;
  name?: unknown;
  server_host?: unknown;
  server_port?: unknown;
  server_id?: unknown;
  server_domain?: unknown;
  device_id?: string;
  password?: string;
  transport?: string;
  signaling_encoding?: string;
  gb_version?: string;
};

const LEGACY_PROFILE_KEY = "uvp_platform_profiles";
const LEGACY_ACTIVE_KEY = "uvp_platform_active";
const LEGACY_DEVICE_KEY = "uvp_device_form";
const LEGACY_SETTING_KEYS = [
  "uvp_settings_device",
  "uvp_settings_protocol",
  "uvp_settings_network",
] as const;
const OWNED_LEGACY_KEYS = [
  LEGACY_PROFILE_KEY,
  LEGACY_ACTIVE_KEY,
  LEGACY_DEVICE_KEY,
  ...LEGACY_SETTING_KEYS,
] as const;

const config = ref<DesktopConfigV2 | null>(null);
const loading = ref(false);
const error = ref("");

function readJson(key: string): Record<string, unknown> | null {
  const raw = localStorage.getItem(key);
  if (!raw) return null;
  const value = JSON.parse(raw) as unknown;
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function stringValue(value: unknown, fallback: string): string {
  return typeof value === "string" && value.trim() ? value : fallback;
}

function positiveInt(value: unknown, fallback: number): number {
  const parsed = typeof value === "number"
    ? value
    : typeof value === "string" && /^\d+$/.test(value.trim()) ? Number(value) : NaN;
  return Number.isInteger(parsed) && parsed > 0 ? parsed : fallback;
}

function migrateLegacy(base: DesktopConfigV2): DesktopConfigV2 | null {
  const rawProfiles = localStorage.getItem(LEGACY_PROFILE_KEY);
  const rawDevice = localStorage.getItem(LEGACY_DEVICE_KEY);
  const hasLegacy = rawProfiles !== null
    || rawDevice !== null
    || LEGACY_SETTING_KEYS.some((key) => localStorage.getItem(key) !== null);
  if (!hasLegacy) return null;

  let legacyProfiles: LegacyProfile[] = [];
  try {
    const parsed = rawProfiles ? JSON.parse(rawProfiles) as unknown : [];
    if (Array.isArray(parsed)) legacyProfiles = parsed as LegacyProfile[];
  } catch {
    // 损坏的单个旧 key 不阻止其余可用字段迁移；保存失败时全部 key 仍保留。
  }

  const profiles = legacyProfiles.length
    ? legacyProfiles.map((raw, index): PlatformProfile => {
      const oldDomain = stringValue(raw.server_domain, "");
      const serverId = stringValue(raw.server_id, oldDomain.length === 20
        ? oldDomain
        : base.profiles[0].server_id);
      const id = stringValue(raw.id, `profile-${index + 1}`);
      return {
        id,
        name: stringValue(raw.name, `平台 ${index + 1}`),
        server_host: stringValue(raw.server_host, base.profiles[0].server_host),
        server_port: positiveInt(raw.server_port, base.profiles[0].server_port),
        server_id: serverId,
        server_domain: oldDomain.length === 10 ? oldDomain : serverId.slice(0, 10),
        password: typeof raw.password === "string" ? raw.password : "",
        transport: raw.transport === "TCP" || raw.transport === "Tcp" ? "TCP" : "UDP",
        gb_version: raw.gb_version === "2016" || raw.gb_version === "V2016" ? "V2016" : "V2022",
        signaling_encoding: raw.signaling_encoding === "UTF-8" || raw.signaling_encoding === "Utf8"
          ? "Utf8"
          : "Gb18030",
      };
    })
    : base.profiles;

  let legacyDevice: Record<string, unknown> = {};
  let legacyProtocol: Record<string, unknown> = {};
  let legacyNetwork: Record<string, unknown> = {};
  try {
    legacyDevice = {
      ...(readJson("uvp_settings_device") ?? {}),
      ...(readJson(LEGACY_DEVICE_KEY) ?? {}),
    };
  } catch { /* 保留默认 */ }
  try { legacyProtocol = readJson("uvp_settings_protocol") ?? {}; } catch { /* 保留默认 */ }
  try { legacyNetwork = readJson("uvp_settings_network") ?? {}; } catch { /* 保留默认 */ }

  const selectedId = localStorage.getItem(LEGACY_ACTIVE_KEY);
  const activeProfileId = selectedId && profiles.some((profile) => profile.id === selectedId)
    ? selectedId
    : profiles[0].id;
  const firstLegacyProfile = legacyProfiles[0];

  return {
    ...base,
    schema_version: 2,
    active_profile_id: activeProfileId,
    profiles,
    media: normalizeMediaProfile({
      ...base.media,
      video_fps: legacyDevice.video_fps,
    }, normalizeMediaProfile(base.media, DEFAULT_MEDIA_PROFILE)),
    device: {
      ...base.device,
      device_id: stringValue(legacyDevice.device_id, stringValue(firstLegacyProfile?.device_id, base.device.device_id)),
      device_name: stringValue(legacyDevice.device_name, base.device.device_name),
      manufacturer: stringValue(legacyDevice.manufacturer, base.device.manufacturer),
      model: stringValue(legacyDevice.model, base.device.model),
      firmware: stringValue(legacyDevice.firmware, base.device.firmware),
      channel_name: stringValue(legacyDevice.channel_name, base.device.channel_name),
      register_expires_secs: positiveInt(legacyProtocol.register_expires_secs, base.device.register_expires_secs),
      heartbeat_interval_secs: positiveInt(legacyProtocol.heartbeat_interval_secs, base.device.heartbeat_interval_secs),
      heartbeat_fail_threshold: positiveInt(legacyProtocol.heartbeat_fail_threshold, base.device.heartbeat_fail_threshold),
    },
    network: {
      bind_mode: legacyNetwork.bind_mode === "specific" ? "specific" : base.network.bind_mode,
      bind_address: typeof legacyNetwork.bind_address === "string"
        ? legacyNetwork.bind_address
        : base.network.bind_address,
      sip_trace: typeof legacyNetwork.sip_trace === "boolean"
        ? legacyNetwork.sip_trace
        : base.network.sip_trace,
    },
    catalog_tree: base.catalog_tree ?? [],
  };
}

function applyConfig(next: DesktopConfigV2): DesktopConfigV2 {
  config.value = next;
  error.value = "";
  return next;
}

function clearLegacyKeys() {
  if (typeof localStorage === "undefined") return;
  for (const key of OWNED_LEGACY_KEYS) localStorage.removeItem(key);
}

export function isRevisionConflict(cause: unknown): boolean {
  const message = String(cause).toLowerCase();
  return message.includes("版本冲突") || message.includes("revision") || message.includes("conflict");
}

async function refreshAfterRevisionConflict(cause: unknown) {
  if (!isRevisionConflict(cause)) return;
  try {
    const latest = await invoke<DesktopConfigV2>("get_desktop_config");
    applyConfig(latest);
  } catch {
    // 原始保存错误仍需返回；读取失败时保留当前快照，交给页面提示用户重试。
  }
  error.value = String(cause);
}

export async function loadDesktopConfig(): Promise<DesktopConfigV2> {
  loading.value = true;
  try {
    const loaded = await invoke<DesktopConfigV2>("get_desktop_config");
    const migration = migrateLegacy(loaded);
    if (!migration) return applyConfig(loaded);

    // 保存成功是迁移提交点；异常时不会执行下面的删除，旧数据可继续重试。
    const saved = await invoke<DesktopConfigV2>("save_desktop_config", { config: migration });
    clearLegacyKeys();
    return applyConfig(saved);
  } catch (cause) {
    error.value = String(cause);
    throw cause;
  } finally {
    loading.value = false;
  }
}

export async function saveDesktopConfig(next: DesktopConfigV2): Promise<DesktopConfigV2> {
  try {
    const saved = await invoke<DesktopConfigV2>("save_desktop_config", { config: next });
    return applyConfig(saved);
  } catch (cause) {
    error.value = String(cause);
    await refreshAfterRevisionConflict(cause);
    throw cause;
  }
}

export async function saveMediaConfig(media: MediaProfile, expectedRevision?: number): Promise<DesktopConfigV2> {
  if (!config.value) throw new Error("桌面端配置尚未加载");
  try {
    const saved = await invoke<DesktopConfigV2>("save_media_config", {
      media,
      expectedRevision: expectedRevision ?? config.value.config_revision,
    });
    return applyConfig(saved);
  } catch (cause) {
    error.value = String(cause);
    await refreshAfterRevisionConflict(cause);
    throw cause;
  }
}

export async function resetDesktopConfig(): Promise<DesktopConfigV2> {
  try {
    const reset = await invoke<DesktopConfigV2>("reset_desktop_config");
    clearLegacyKeys();
    return applyConfig(reset);
  } catch (cause) {
    error.value = String(cause);
    throw cause;
  }
}

export function usePlatform() {
  const profiles = computed(() => config.value?.profiles ?? []);
  const activeId = computed(() => config.value?.active_profile_id ?? "");
  const active = computed(() => profiles.value.find((profile) => profile.id === activeId.value));

  async function setActive(id: string) {
    if (!config.value || !profiles.value.some((profile) => profile.id === id)) return;
    await saveDesktopConfig({ ...config.value, active_profile_id: id });
  }

  async function addProfile(profile: Omit<PlatformProfile, "id">): Promise<string> {
    if (!config.value) throw new Error("桌面端配置尚未加载");
    const id = `p-${Date.now()}`;
    await saveDesktopConfig({
      ...config.value,
      active_profile_id: id,
      profiles: [...config.value.profiles, { ...profile, id }],
    });
    return id;
  }

  async function updateProfile(id: string, patch: Partial<PlatformProfile>) {
    if (!config.value) throw new Error("桌面端配置尚未加载");
    await saveDesktopConfig({
      ...config.value,
      profiles: config.value.profiles.map((profile) => profile.id === id
        ? { ...profile, ...patch, id }
        : profile),
    });
  }

  async function removeProfile(id: string) {
    if (!config.value || config.value.profiles.length <= 1) return;
    const profiles = config.value.profiles.filter((profile) => profile.id !== id);
    await saveDesktopConfig({
      ...config.value,
      profiles,
      active_profile_id: config.value.active_profile_id === id ? profiles[0].id : config.value.active_profile_id,
    });
  }

  function passwordFor(profileId: string): string {
    return profiles.value.find((profile) => profile.id === profileId)?.password ?? "";
  }

  return {
    config, profiles, activeId, active, loading, error,
    loadDesktopConfig, saveDesktopConfig, saveMediaConfig, resetDesktopConfig,
    setActive, addProfile, updateProfile, removeProfile, passwordFor,
  };
}
