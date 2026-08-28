// 平台档案:多套上级平台连接参数,顶栏全局切换,单设备/压测共用。
//
// 安全约定(重要):**档案里不含密码**。
// 密码只存在 OS 钥匙串里(见 src-tauri/src/secrets.rs),前端仅持有
// `secretStatus[id]` 这个「是否已配置」布尔;`start_device` / `start_stress`
// 只传 profile_id,由 Rust 侧现取现用,明文不经 IPC 往返、不落 localStorage。
import { ref, computed } from "vue";
import { invoke } from "@tauri-apps/api/core";

export interface PlatformProfile {
  id: string;
  name: string;
  server_host: string;
  server_port: number;
  server_domain: string;
  transport: string; // "UDP" | "TCP"
  signaling_encoding?: string; // "GB18030"(默认) | "UTF-8"
}

const STORE_KEY = "uvp_platform_profiles";
const ACTIVE_KEY = "uvp_platform_active";

function defaultProfiles(): PlatformProfile[] {
  // 仅提供无私密信息的本地示例；真实平台地址由用户在界面中配置,密码进钥匙串。
  return [{
    id: "local",
    name: "本地示例",
    server_host: "127.0.0.1",
    server_port: 5060,
    server_domain: "34020000002000000001",
    transport: "UDP",
    signaling_encoding: "GB18030",
  }];
}

/** 剥掉历史版本可能残留在 localStorage 里的 password 字段。 */
function sanitize<T extends object>(p: T): PlatformProfile {
  const { password: _drop, ...rest } = p as T & { password?: string };
  return rest as unknown as PlatformProfile;
}

// 单例响应式状态(整个应用共享一份)。
const profiles = ref<PlatformProfile[]>(load());
const activeId = ref<string>(loadActive());
/** profile_id → 是否已在钥匙串中配置密码。 */
const secretStatus = ref<Record<string, boolean>>({});

function load(): PlatformProfile[] {
  try {
    const raw = localStorage.getItem(STORE_KEY);
    if (raw) {
      const arr = JSON.parse(raw) as object[];
      if (Array.isArray(arr) && arr.length) return arr.map(sanitize);
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
  localStorage.setItem(STORE_KEY, JSON.stringify(profiles.value.map(sanitize)));
  localStorage.setItem(ACTIVE_KEY, activeId.value);
}

/** 从引擎刷新「哪些档案已配置密码」。 */
async function refreshSecretStatus() {
  try {
    const ids = profiles.value.map((p) => p.id);
    secretStatus.value = await invoke<Record<string, boolean>>("platform_secret_status", {
      profileIds: ids,
    });
  } catch {
    // 钥匙串查询失败不阻塞 UI:按「未配置」显示,用户重新保存即可。
    secretStatus.value = {};
  }
}

/**
 * 一次性迁移:把旧版本明文存在 localStorage 里的密码搬进钥匙串,然后抹掉明文。
 * 迁移失败(钥匙串不可用)时也照样抹掉明文——宁可让用户重输,不留磁盘明文。
 */
async function migrateLegacyPasswords(): Promise<string[]> {
  let raw: string | null = null;
  try {
    raw = localStorage.getItem(STORE_KEY);
  } catch {
    return [];
  }
  if (!raw) return [];
  let arr: (PlatformProfile & { password?: string })[];
  try {
    arr = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(arr)) return [];
  const legacy = arr.filter((p) => typeof p?.password === "string" && p.password.length > 0);
  if (!legacy.length) return [];

  const migrated: string[] = [];
  for (const p of legacy) {
    try {
      await invoke("save_platform_secret", { profileId: p.id, password: p.password });
      migrated.push(p.name ?? p.id);
    } catch {
      // 忽略单条失败,继续抹除明文
    }
  }
  // 无论成功与否都重写为不含 password 的版本。
  profiles.value = arr.map(sanitize);
  persist();
  await refreshSecretStatus();
  return migrated;
}

export function usePlatform() {
  const active = computed(
    () => profiles.value.find((p) => p.id === activeId.value) ?? profiles.value[0]
  );
  /** 当前档案是否已配置密码(注册前置条件)。 */
  const activeHasSecret = computed(() => !!secretStatus.value[activeId.value]);

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
      Object.assign(p, sanitize(patch));
      persist();
    }
  }

  async function removeProfile(id: string) {
    if (profiles.value.length <= 1) return; // 至少留一个
    profiles.value = profiles.value.filter((p) => p.id !== id);
    if (activeId.value === id) activeId.value = profiles.value[0].id;
    persist();
    try {
      await invoke("delete_platform_secret", { profileId: id });
    } catch {
      // 钥匙串清理失败不影响档案删除
    }
    await refreshSecretStatus();
  }

  /** 保存密码到钥匙串。返回后端提示(区分「已持久化」与「仅本次运行有效」)。 */
  async function savePassword(id: string, password: string): Promise<string> {
    const res = await invoke<{ backend: string; message: string }>("save_platform_secret", {
      profileId: id,
      password,
    });
    await refreshSecretStatus();
    return res.message;
  }

  return {
    profiles, activeId, active, secretStatus, activeHasSecret,
    setActive, addProfile, updateProfile, removeProfile,
    savePassword, refreshSecretStatus, migrateLegacyPasswords,
  };
}
