// 平台档案:多套上级平台连接参数,顶栏全局切换,单设备/压测共用。
// 存 localStorage,provide/inject 下发给各页,取代原先各页硬编码 + 独立平台配置页。
import { ref, computed } from "vue";

export interface PlatformProfile {
  id: string;
  name: string;
  server_host: string;
  server_port: number;
  server_domain: string;
  password: string;
  transport: string; // "UDP" | "TCP"
  signaling_encoding?: string; // "GB18030"(默认) | "UTF-8"
}

const STORE_KEY = "uvp_platform_profiles";
const ACTIVE_KEY = "uvp_platform_active";

function defaultProfiles(): PlatformProfile[] {
  return [
    {
      id: "wvp-test",
      name: "WVP-测试",
      server_host: "192.168.10.222",
      server_port: 8160,
      server_domain: "3502000000",
      password: "wvp_sip_password",
      transport: "UDP",
      signaling_encoding: "GB18030",
    },
    {
      id: "local",
      name: "本地",
      server_host: "127.0.0.1",
      server_port: 5060,
      server_domain: "34020000002000000001",
      password: "12345678",
      transport: "UDP",
      signaling_encoding: "GB18030",
    },
  ];
}

// 单例响应式状态(整个应用共享一份)。
const profiles = ref<PlatformProfile[]>(load());
const activeId = ref<string>(loadActive());

function load(): PlatformProfile[] {
  try {
    const raw = localStorage.getItem(STORE_KEY);
    if (raw) {
      const arr = JSON.parse(raw) as PlatformProfile[];
      if (Array.isArray(arr) && arr.length) return arr;
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
