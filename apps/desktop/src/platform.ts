// 平台档案:多套上级平台连接参数,顶栏全局切换,单设备/压测共用。
// 存 localStorage,provide/inject 下发给各页,取代原先各页硬编码 + 独立平台配置页。
import { ref, computed } from "vue";

export type SignalingTransport = "UDP" | "TCP";
export type AudioTransport = "UDP" | "TCP_ACTIVE" | "TCP_PASSIVE";

export interface PlatformProfile {
  id: string;
  name: string;
  server_host: string;
  server_port: number;
  server_id: string;
  server_domain: string;
  device_id: string;
  password: string;
  transport: SignalingTransport;
  audio_transport: AudioTransport;
  signaling_encoding?: string; // "GB18030"(默认) | "UTF-8"
}

const STORE_KEY = "uvp_platform_profiles";
const ACTIVE_KEY = "uvp_platform_active";

function defaultProfiles(): PlatformProfile[] {
  // 仅提供无私密信息的本地示例；真实平台地址和密码由用户在界面中配置。
  return [{
    id: "local",
    name: "本地示例",
    server_host: "127.0.0.1",
    server_port: 5060,
    server_id: "34020000002000000001",
    server_domain: "3402000000",
    device_id: "35020000001310000001",
    password: "change-me",
    transport: "UDP",
    audio_transport: "TCP_ACTIVE",
    signaling_encoding: "GB18030",
  }];
}

function normalizeProfile(raw: Partial<PlatformProfile> & Record<string, unknown>, index: number): PlatformProfile {
  const legacyDomain = typeof raw.server_domain === "string" ? raw.server_domain : "";
  const serverId = typeof raw.server_id === "string" && raw.server_id
    ? raw.server_id
    : legacyDomain.length === 20
      ? legacyDomain
      : "34020000002000000001";
  const serverDomain = typeof raw.server_id === "string" && raw.server_id
    ? legacyDomain
    : serverId.slice(0, 10);
  const transport: SignalingTransport = raw.transport === "TCP" ? "TCP" : "UDP";
  const audioTransport: AudioTransport = ["UDP", "TCP_ACTIVE", "TCP_PASSIVE"].includes(String(raw.audio_transport))
    ? raw.audio_transport as AudioTransport
    : "TCP_ACTIVE";

  return {
    id: typeof raw.id === "string" && raw.id ? raw.id : `profile-${index + 1}`,
    name: typeof raw.name === "string" && raw.name ? raw.name : `平台 ${index + 1}`,
    server_host: typeof raw.server_host === "string" ? raw.server_host : "",
    server_port: typeof raw.server_port === "number" ? raw.server_port : 5060,
    server_id: serverId,
    server_domain: serverDomain,
    device_id: typeof raw.device_id === "string" ? raw.device_id : "",
    password: typeof raw.password === "string" ? raw.password : "",
    transport,
    audio_transport: audioTransport,
    signaling_encoding: raw.signaling_encoding === "UTF-8" ? "UTF-8" : "GB18030",
  };
}

// 单例响应式状态(整个应用共享一份)。
const profiles = ref<PlatformProfile[]>(load());
const activeId = ref<string>(loadActive());

function load(): PlatformProfile[] {
  try {
    const raw = localStorage.getItem(STORE_KEY);
    if (raw) {
      const arr = JSON.parse(raw) as Array<Partial<PlatformProfile> & Record<string, unknown>>;
      if (Array.isArray(arr) && arr.length) return arr.map(normalizeProfile);
    }
  } catch {
    // 忽略损坏数据,回退默认
  }
  return defaultProfiles();
}

function loadActive(): string {
  const saved = localStorage.getItem(ACTIVE_KEY);
  const list = load();
  if (saved && list.some((p) => p.id === saved)) return saved;
  return list[0]?.id ?? "";
}

function persist() {
  localStorage.setItem(STORE_KEY, JSON.stringify(profiles.value));
  localStorage.setItem(ACTIVE_KEY, activeId.value);
}

export function usePlatform() {
  const active = computed(
    () => profiles.value.find((p) => p.id === activeId.value) ?? profiles.value[0]
  );

  function setActive(id: string) {
    activeId.value = id;
    persist();
  }

  function addProfile(p: Omit<PlatformProfile, "id">) {
    const id = `p-${Date.now()}`;
    profiles.value.push({ ...p, id });
    activeId.value = id;
    persist();
    return id;
  }

  function updateProfile(id: string, patch: Partial<PlatformProfile>) {
    const p = profiles.value.find((x) => x.id === id);
    if (p) {
      Object.assign(p, patch);
      persist();
    }
  }

  function removeProfile(id: string) {
    if (profiles.value.length <= 1) return; // 至少留一个
    profiles.value = profiles.value.filter((p) => p.id !== id);
    if (activeId.value === id) activeId.value = profiles.value[0].id;
    persist();
  }

  return { profiles, activeId, active, setActive, addProfile, updateProfile, removeProfile };
}
