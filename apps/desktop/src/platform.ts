// 桌面端配置的唯一持久化真相位于 Rust ConfigStore。
// 平台密码与其它联调参数一并保存在 Rust ConfigStore；localStorage 只用于一次性迁移。
import { computed, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import {
  DEFAULT_MEDIA_PROFILE,
  normalizeMediaProfile,
  type CameraMode,
  type MediaProfile,
} from "./media-profile.ts";
import type { RecordPlanSettings } from "./record-plan.mjs";
import { effective, parseEncoding } from "./signaling-charset.mjs";

export type SignalingTransport = "UDP" | "TCP";
/** 音频流(语音广播/对讲下行)传输方式,与信令传输相互独立。 */
export type AudioTransport = "UDP" | "TCP_ACTIVE" | "TCP_PASSIVE";
export type GbVersion = "V2016" | "V2022";
export type SignalingEncoding = "Gb2312" | "Gb18030" | "Utf8";
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
  /**
   * TCP 信令连接池大小(缺省由后端给出)。
   *
   * 所有虚拟设备共享这若干条连接,FD 占用与设备数量解耦;
   * UDP 模式下该字段被后端忽略。前端不暴露控件,仅在保存配置时原样透传。
   */
  signaling_tcp_connections?: number;
  audio_transport: AudioTransport;
  gb_version: GbVersion;
  signaling_encoding: SignalingEncoding;
}

export interface DeviceSettings {
  device_id: string;
  device_name: string;
  manufacturer: string;
  model: string;
  firmware: string;
  /** 硬件版本，仅 GB-2022 的 DeviceInfo 应答输出；留空表示不填。 */
  hardware_version: string;
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

export interface LocalIpOption {
  address: string;
  interface_name: string;
  address_family: "IPv4" | "IPv6";
  loopback: boolean;
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
  /** 本地 OSD 水印样式；旧后端缺省时由 normalizeOsdSettings 补齐。 */
  osd?: OsdSettings;
  /** NTP 校时配置；旧后端缺省时按默认值（关闭）展示。 */
  time_sync?: TimeSyncSettings;
  /** 定时录像周计划；旧后端缺省时按空计划（关闭）展示。 */
  record_plan?: RecordPlanSettings;
}

/** 逻辑协议时钟的 NTP 配置，与 Rust 侧 `TimeSyncSettings` 一一对应。 */
export interface TimeSyncSettings {
  ntp_enabled: boolean;
  ntp_server: string;
  ntp_port: number;
  refresh_interval_secs: number;
}

export type TimeSyncSource = "none" | "sip_date" | "ntp";

/** Rust `common::clock::ClockSnapshot` 的投影。 */
export interface ClockSnapshot {
  offset_secs: number;
  source: TimeSyncSource;
  detail: string;
  synced_at_ms: number;
  sip_offset_secs: number;
  sip_synced: boolean;
  ntp_active: boolean;
  last_error: string | null;
}

export interface ClockState {
  clock: ClockSnapshot;
  time_sync: TimeSyncSettings;
  ntp_running: boolean;
  local_epoch_ms: number;
  platform_epoch_ms: number;
  config_revision: number;
}

export interface NtpProbeResult {
  offset_secs: number;
  round_trip_ms: number;
  stratum: number;
  server: string;
  clock: ClockSnapshot;
}

/** 默认值与 Rust `TimeSyncSettings::default()` 保持一致：关闭、阿里云 NTP、10 分钟刷新。 */
export function defaultTimeSyncSettings(): TimeSyncSettings {
  return {
    ntp_enabled: false,
    ntp_server: "ntp.aliyun.com",
    ntp_port: 123,
    refresh_interval_secs: 600,
  };
}

export function normalizeTimeSyncSettings(value: Partial<TimeSyncSettings> | undefined | null): TimeSyncSettings {
  const base = defaultTimeSyncSettings();
  return {
    ntp_enabled: typeof value?.ntp_enabled === "boolean" ? value.ntp_enabled : base.ntp_enabled,
    ntp_server: typeof value?.ntp_server === "string" && value.ntp_server.trim() ? value.ntp_server : base.ntp_server,
    ntp_port: Number.isInteger(value?.ntp_port) && (value!.ntp_port as number) > 0 ? value!.ntp_port as number : base.ntp_port,
    refresh_interval_secs: Number.isInteger(value?.refresh_interval_secs) && (value!.refresh_interval_secs as number) > 0
      ? value!.refresh_interval_secs as number
      : base.refresh_interval_secs,
  };
}

/** 返回第一条错误；合法时返回 null。与 Rust 侧校验口径一致。 */
export function validateTimeSyncSettings(value: TimeSyncSettings): string | null {
  const server = value.ntp_server.trim();
  if (!server) return "NTP 服务器地址不能为空";
  if (/\s/.test(server)) return "NTP 服务器地址不能包含空白字符";
  if (!Number.isInteger(value.ntp_port) || value.ntp_port < 1 || value.ntp_port > 65535) return "NTP 端口必须在 1~65535";
  if (!Number.isInteger(value.refresh_interval_secs) || value.refresh_interval_secs < 1 || value.refresh_interval_secs > 86400) {
    return "NTP 刷新间隔必须在 1~86400 秒";
  }
  return null;
}

export type OsdSize = "small" | "medium" | "large";

export interface OsdLayer {
  enabled: boolean;
  text: string;
  size: OsdSize;
  fill_color: string;
  outline_color: string;
}

export interface OsdSettings {
  timestamp: OsdLayer;
  channel_name: OsdLayer;
  watermark: OsdLayer;
}

export interface OsdCapability {
  ffmpeg: boolean;
  filters: boolean;
  font: string | null;
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
      const version: GbVersion =
        raw.gb_version === "2016" || raw.gb_version === "V2016" ? "V2016" : "V2022";
      return {
        id,
        name: stringValue(raw.name, `平台 ${index + 1}`),
        server_host: stringValue(raw.server_host, base.profiles[0].server_host),
        server_port: positiveInt(raw.server_port, base.profiles[0].server_port),
        server_id: serverId,
        server_domain: oldDomain.length === 10 ? oldDomain : serverId.slice(0, 10),
        password: typeof raw.password === "string" ? raw.password : "",
        transport: raw.transport === "TCP" || raw.transport === "Tcp" ? "TCP" : "UDP",
        // 旧配置没有对讲流传输字段:与默认值保持一致(TCP 主动)。
        audio_transport: "TCP_ACTIVE",
        gb_version: version,
        // 字符集按版本归一:旧配置缺该字段、或存了与版本不符的值(如 2016 档配 GB18030)
        // 一律落到版本默认,避免迁移后带着对端不认的声明名出站。
        signaling_encoding: effective(parseEncoding(raw.signaling_encoding), version),
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
      hardware_version: stringValue(legacyDevice.hardware_version, base.device.hardware_version),
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

export async function saveOsdConfig(osd: OsdSettings, expectedRevision?: number): Promise<DesktopConfigV2> {
  if (!config.value) throw new Error("桌面端配置尚未加载");
  try {
    const saved = await invoke<DesktopConfigV2>("save_osd_config", {
      osd,
      expectedRevision: expectedRevision ?? config.value.config_revision,
    });
    return applyConfig(saved);
  } catch (cause) {
    error.value = String(cause);
    await refreshAfterRevisionConflict(cause);
    throw cause;
  }
}

export async function getOsdCapability(): Promise<OsdCapability> {
  return invoke<OsdCapability>("get_osd_capability");
}

/** 视频编码后端(与 Rust `media_rtp::encoder::EncoderBackend` 一致)。 */
export type EncoderBackend =
  | "nvenc"
  | "qsv"
  | "amf"
  | "media_foundation"
  | "video_toolbox"
  | "software";

export interface EncoderStatus {
  codec: "h264" | "h265";
  backend: EncoderBackend;
  hardware: boolean;
}

export const ENCODER_BACKEND_LABELS: Record<EncoderBackend, string> = {
  nvenc: "NVENC",
  qsv: "Quick Sync",
  amf: "AMD AMF",
  media_foundation: "Media Foundation",
  video_toolbox: "VideoToolbox",
  software: "CPU",
};

/** 本机 H.264 / H.265 实际使用的编码后端。首次调用会试编探测,约 1~3 秒。 */
export async function getEncoderStatus(): Promise<EncoderStatus[]> {
  return invoke<EncoderStatus[]>("get_encoder_status");
}

/**
 * 保存 NTP 校时配置。
 *
 * 与其它配置不同：设备运行中**允许**修改（校时是运行态能力，内网 NTP 地址
 * 经常要边跑边试），保存后后端会按新配置重启刷新任务。
 */
export async function saveTimeSync(
  timeSync: TimeSyncSettings,
  expectedRevision?: number,
): Promise<DesktopConfigV2> {
  if (!config.value) throw new Error("桌面端配置尚未加载");
  try {
    const saved = await invoke<DesktopConfigV2>("save_time_sync", {
      timeSync,
      expectedRevision: expectedRevision ?? config.value.config_revision,
    });
    return applyConfig(saved);
  } catch (cause) {
    error.value = String(cause);
    await refreshAfterRevisionConflict(cause);
    throw cause;
  }
}

/** 读取当前校时状态（偏移、来源、SIP 回退值、NTP 任务运行态）。 */
export async function getClockState(): Promise<ClockState> {
  return invoke<ClockState>("get_clock_state");
}

/** 手动执行一次 NTP 校时，成功后立即生效。 */
export async function syncNtpNow(): Promise<NtpProbeResult> {
  return invoke<NtpProbeResult>("sync_ntp_now");
}

/** 定时录像计划（后端 config_store::RecordPlanSettings）。 */
export interface RecordPlanState {
  plan: RecordPlanSettings;
  config_revision: number;
  /** 调度任务是否在跑（设备未启动时为 false）。 */
  scheduler_running: boolean;
  /** 今天周几（0=周一）。 */
  weekday: number;
  weekday_label: string;
  secs_of_day: number;
  /** 此刻是否落在计划时段内（已计入总开关）。 */
  in_window: boolean;
  recording: { phase: string; active_id: string | null; error: string | null } | null;
}

export async function getRecordPlan(): Promise<RecordPlanState> {
  return invoke<RecordPlanState>("get_record_plan");
}

export async function saveRecordPlan(
  plan: RecordPlanSettings,
  expectedRevision: number,
): Promise<DesktopConfigV2> {
  return invoke<DesktopConfigV2>("save_record_plan", { plan, expectedRevision });
}

/** 报警单（后端 gb28181_protocol::alarm::AlarmReport）。 */
export interface AlarmReport {
  /** 报警级别 1-4（1 最高）。 */
  priority: number;
  /** 报警方式 1-7。 */
  method: number;
  /** 报警类型 1-5；null/undefined 表示不输出 <Info>。 */
  alarm_type?: number | null;
  alarm_type_param?: string | null;
  description?: string | null;
  longitude?: number | null;
  latitude?: number | null;
  /** 报警时间往前挪的分钟数，0 表示现在。 */
  minutes_ago?: number;
}

/** 一条报警上报记录（成功与失败都记）。 */
export interface AlarmRecord {
  id: number;
  device_id: string;
  priority: number;
  method: number;
  alarm_type: number | null;
  description: string | null;
  longitude: number | null;
  latitude: number | null;
  alarm_time: string;
  captured_at_ms: number;
  /** 是否走平台 Event:Alarm 订阅对话。 */
  via_notify: boolean;
  status: number | null;
  error: string | null;
}

export async function reportAlarm(report: AlarmReport): Promise<AlarmRecord> {
  return invoke<AlarmRecord>("report_alarm", { report });
}

/** 预览报警 XML；与实际发送共用同一份组装逻辑，SN 固定为 1。 */
export async function previewAlarmXml(report: AlarmReport): Promise<string> {
  return invoke<string>("preview_alarm_xml", { report });
}

export async function listAlarmHistory(): Promise<AlarmRecord[]> {
  return invoke<AlarmRecord[]>("list_alarm_history");
}

export async function clearAlarmHistory(): Promise<void> {
  await invoke("clear_alarm_history");
}

/** 本机复位报警状态（不向平台发报文）。 */
export async function resetAlarm(): Promise<string> {
  return invoke<string>("reset_alarm");
}

/** 本地抓拍元数据（后端 media-rtp::snapshot::SnapshotMeta）。 */
export interface SnapshotMeta {
  id: string;
  file_name: string;
  path: string;
  byte_len: number;
  /** 帧被切出的时刻（毫秒纪元），不是落盘时刻。 */
  captured_at_ms: number;
  saved_at_ms: number;
  width: number | null;
  height: number | null;
}

/**
 * 抓拍当前预览帧。
 *
 * 后端取的是预览总线上的最新帧，与画面同源；预览未出帧时会直接报错，
 * 不会给出一张黑图。因此这个调用必须在预览播放中使用。
 */
export async function takeSnapshot(): Promise<SnapshotMeta> {
  return invoke<SnapshotMeta>("take_snapshot");
}

export async function listSnapshots(limit = 60): Promise<SnapshotMeta[]> {
  return invoke<SnapshotMeta[]>("list_snapshots", { limit });
}

/** 读取抓拍为 data URL（后端按实际落盘目录读，不依赖资源协议 scope）。 */
export async function readSnapshot(id: string): Promise<string> {
  return invoke<string>("read_snapshot", { id });
}

export async function deleteSnapshot(id: string): Promise<void> {
  await invoke("delete_snapshot", { id });
}

export async function exportSnapshot(id: string, targetPath: string): Promise<string> {
  return invoke<string>("export_snapshot", { id, targetPath });
}

/** 日志导出结果；`redacted` 是真实替换掉的敏感字段数量。 */
export interface LogExportResult {
  path: string;
  byte_len: number;
  redacted: number;
  line_count: number;
}

/**
 * 导出当前页签日志。
 *
 * `blocks` 是已经格式化好的文本块，脱敏在 Rust 侧完成 —— 前端不参与判定，
 * 也就不存在「某条路径忘了脱敏」的可能。
 */
export async function exportLogs(
  tab: string,
  blocks: string[],
  targetPath: string,
): Promise<LogExportResult> {
  return invoke<LogExportResult>("export_logs", { tab, blocks, targetPath });
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

export async function listLocalIpAddresses(): Promise<LocalIpOption[]> {
  return invoke<LocalIpOption[]>("list_local_ip_addresses");
}

/**
 * 查询摄像头实际支持的分辨率与帧率。
 *
 * 每次调用都会让 FFmpeg 打开一次设备，耗时在百毫秒到数秒之间，
 * 调用方应缓存结果，不要放在渲染热路径上。
 */
export async function listCameraModes(cameraIndex: number): Promise<CameraMode[]> {
  return invoke<CameraMode[]>("list_camera_modes", { cameraIndex });
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
    loadDesktopConfig, saveDesktopConfig, saveMediaConfig, saveOsdConfig, resetDesktopConfig,
    setActive, addProfile, updateProfile, removeProfile, passwordFor,
  };
}
