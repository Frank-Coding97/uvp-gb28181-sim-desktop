// 平台档案:多套上级平台连接参数,顶栏全局切换,单设备/压测共用。
// 密码只存在 OS 钥匙串；localStorage 中的 V1 配置始终是严格白名单数据。
import { computed, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import {
  createVersionedStorage,
  redactSensitiveText,
  type DecodeResult,
  type StorageSaveResult,
} from "./lib/versionedStorage";

export type InboundIdentityMode = "compatible" | "strict";
export type PlatformTransport = "UDP" | "TCP";
export type SignalingEncoding = "GB18030" | "UTF-8";

export interface PlatformProfile {
  id: string;
  name: string;
  server_host: string;
  server_port: number;
  server_domain: string;
  inbound_identity_mode: InboundIdentityMode;
  transport: PlatformTransport;
  signaling_encoding?: SignalingEncoding;
}

interface PlatformDataV1 {
  profiles: PlatformProfile[];
  activeProfileId: string;
}

interface LegacyPassword {
  profileId: string;
  profileName: string;
  password: string;
}

const STORE_KEY = "uvp_platform_profiles";
const ACTIVE_KEY = "uvp_platform_active";
const MAX_ID_LENGTH = 128;
const MAX_NAME_LENGTH = 100;
let pendingLegacyPasswords: LegacyPassword[] = [];

function defaultProfiles(): PlatformProfile[] {
  return [{
    id: "local",
    name: "本地示例",
    server_host: "127.0.0.1",
    server_port: 5060,
    server_domain: "34020000002000000001",
    inbound_identity_mode: "compatible",
    transport: "UDP",
    signaling_encoding: "GB18030",
  }];
}

function defaultData(): PlatformDataV1 {
  const list = defaultProfiles();
  return { profiles: list, activeProfileId: list[0].id };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isLiteralIpv4(value: string): boolean {
  const parts = value.split(".");
  return parts.length === 4 && parts.every((part) => {
    if (!/^\d{1,3}$/.test(part)) return false;
    const octet = Number(part);
    return octet >= 0 && octet <= 255;
  });
}

function decodeProfile(value: unknown): PlatformProfile | null {
  if (!isRecord(value)) return null;
  const id = typeof value.id === "string" ? value.id.trim() : "";
  const name = typeof value.name === "string" ? value.name.trim() : "";
  const serverHost = typeof value.server_host === "string" ? value.server_host.trim() : "";
  const serverDomain = typeof value.server_domain === "string" ? value.server_domain.trim() : "";
  const serverPort = value.server_port;

  if (!id || id.length > MAX_ID_LENGTH || !name || name.length > MAX_NAME_LENGTH) return null;
  if (!isLiteralIpv4(serverHost)) return null;
  if (!Number.isInteger(serverPort) || (serverPort as number) < 1 || (serverPort as number) > 65535) return null;
  if (!/^\d{20}$/.test(serverDomain)) return null;

  const transport: PlatformTransport = value.transport === "TCP" ? "TCP" : value.transport === "UDP" ? "UDP" : "UDP";
  const signalingEncoding: SignalingEncoding = value.signaling_encoding === "UTF-8" ? "UTF-8" : "GB18030";
  const inboundIdentityMode: InboundIdentityMode = value.inbound_identity_mode === "strict" ? "strict" : "compatible";

  return {
    id,
    name,
    server_host: serverHost,
    server_port: serverPort as number,
    server_domain: serverDomain,
    inbound_identity_mode: inboundIdentityMode,
    transport,
    signaling_encoding: signalingEncoding,
  };
}

function decodeData(value: unknown): DecodeResult<PlatformDataV1> {
  if (!isRecord(value) || !Array.isArray(value.profiles)) throw new Error("invalid platform data");
  const seen = new Set<string>();
  const decoded: PlatformProfile[] = [];
  for (const candidate of value.profiles) {
    const profile = decodeProfile(candidate);
    if (!profile || seen.has(profile.id)) continue;
    seen.add(profile.id);
    decoded.push(profile);
  }

  const profiles = decoded.length ? decoded : defaultProfiles();
  const requestedActive = typeof value.activeProfileId === "string" ? value.activeProfileId : "";
  const activeProfileId = profiles.some((profile) => profile.id === requestedActive)
    ? requestedActive
    : profiles[0].id;
  const data = { profiles, activeProfileId };
  return {
    data,
    repaired: JSON.stringify(value) !== JSON.stringify(data),
    summary: "平台档案中的无效、重复或未知字段已修复",
  };
}

function readLegacyActiveId(): string {
  try {
    return localStorage.getItem(ACTIVE_KEY) ?? "";
  } catch {
    return "";
  }
}

function migrateLegacy(value: unknown, fromVersion: number | null): DecodeResult<PlatformDataV1> {
  if (fromVersion !== null || !Array.isArray(value)) throw new Error("unsupported platform version");

  pendingLegacyPasswords = [];
  for (const candidate of value) {
    if (!isRecord(candidate)) continue;
    const profileId = typeof candidate.id === "string" ? candidate.id.trim() : "";
    const password = typeof candidate.password === "string" ? candidate.password : "";
    if (profileId && password) {
      pendingLegacyPasswords.push({
        profileId,
        profileName: typeof candidate.name === "string" && candidate.name.trim()
          ? candidate.name.trim()
          : profileId,
        password,
      });
    }
  }

  const decoded = decodeData({ profiles: value, activeProfileId: readLegacyActiveId() });
  return {
    ...decoded,
    summary: "旧版平台档案已迁移，敏感字段已从本地配置移除",
  };
}

const profileStorage = createVersionedStorage<PlatformDataV1>({
  key: STORE_KEY,
  schemaVersion: 1,
  defaultValue: defaultData,
  decode: decodeData,
  migrate: migrateLegacy,
  redactBackup: (raw) => redactSensitiveText(raw, ["password"]),
  backupBeforeRepair: true,
});

const initial = profileStorage.load();
let legacyPlaintextCleared = initial.migrated && initial.canonicalSaved;
if (legacyPlaintextCleared) {
  try {
    localStorage.removeItem(ACTIVE_KEY);
  } catch {
    // canonical 已提交；旧 active key 删除失败不会影响新数据的一致性。
  }
}

const profiles = ref<PlatformProfile[]>(initial.data.profiles);
const activeId = ref<string>(initial.data.activeProfileId);
/** profile_id → 是否已在钥匙串中配置密码。 */
const secretStatus = ref<Record<string, boolean>>({});

function persistCandidate(data: PlatformDataV1): StorageSaveResult {
  return profileStorage.save(data);
}

function removeLegacyActiveAfterCommit(result: StorageSaveResult): void {
  if (!result.ok) return;
  try {
    localStorage.removeItem(ACTIVE_KEY);
  } catch {
    // legacy key 不再参与读取，不影响 canonical 数据。
  }
}

function generateProfileId(): string {
  try {
    if (typeof globalThis.crypto?.randomUUID === "function") return globalThis.crypto.randomUUID();
  } catch {
    // WebView 不支持或禁用 crypto 时使用不含业务信息的 fallback。
  }
  return `p-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

/** 从引擎刷新「哪些档案已配置密码」。 */
async function refreshSecretStatus() {
  try {
    const ids = profiles.value.map((profile) => profile.id);
    secretStatus.value = await invoke<Record<string, boolean>>("platform_secret_status", {
      profileIds: ids,
    });
  } catch {
    secretStatus.value = {};
  }
}

/**
 * 旧裸数组密码已在模块加载时提取到内存。逐项搬入钥匙串后立即丢弃内存副本，
 * 并再次尝试写 canonical，确保任何写入都不可能包含 password。
 */
async function migrateLegacyPasswords(): Promise<string[]> {
  if (!pendingLegacyPasswords.length) return [];
  const legacy = pendingLegacyPasswords;
  pendingLegacyPasswords = [];
  const migrated: string[] = [];
  for (const item of legacy) {
    try {
      await invoke("save_platform_secret", {
        profileId: item.profileId,
        password: item.password,
      });
      migrated.push(item.profileName);
    } catch {
      // 单条失败不阻止清除内存明文与后续条目迁移。
    }
  }
  if (!legacyPlaintextCleared) {
    const cleanupResult = persistCandidate({
      profiles: profiles.value,
      activeProfileId: activeId.value,
    });
    legacyPlaintextCleared = cleanupResult.ok;
    removeLegacyActiveAfterCommit(cleanupResult);
  }
  await refreshSecretStatus();
  // 只有 canonical 已确认提交，调用方才能宣称“本地明文已清除”。
  return legacyPlaintextCleared ? migrated : [];
}

export function usePlatform() {
  const active = computed(
    () => profiles.value.find((profile) => profile.id === activeId.value) ?? profiles.value[0],
  );
  const activeHasSecret = computed(() => !!secretStatus.value[activeId.value]);

  function setActive(id: string): boolean {
    if (!profiles.value.some((profile) => profile.id === id)) return false;
    const result = persistCandidate({ profiles: profiles.value, activeProfileId: id });
    if (!result.ok) return false;
    activeId.value = id;
    removeLegacyActiveAfterCommit(result);
    return true;
  }

  function addProfile(input: Omit<PlatformProfile, "id">): string {
    let id = generateProfileId();
    while (profiles.value.some((profile) => profile.id === id)) id = generateProfileId();
    const profile = decodeProfile({
      id,
      name: input.name,
      server_host: input.server_host,
      server_port: input.server_port,
      server_domain: input.server_domain,
      inbound_identity_mode: input.inbound_identity_mode,
      transport: input.transport,
      signaling_encoding: input.signaling_encoding,
    });
    if (!profile) return "";
    const candidate = [...profiles.value, profile];
    const result = persistCandidate({ profiles: candidate, activeProfileId: id });
    if (!result.ok) return "";
    profiles.value = candidate;
    activeId.value = id;
    removeLegacyActiveAfterCommit(result);
    return id;
  }

  function updateProfile(id: string, patch: Partial<PlatformProfile>): boolean {
    const index = profiles.value.findIndex((profile) => profile.id === id);
    if (index < 0) return false;
    const current = profiles.value[index];
    const updated = decodeProfile({
      id: current.id,
      name: patch.name ?? current.name,
      server_host: patch.server_host ?? current.server_host,
      server_port: patch.server_port ?? current.server_port,
      server_domain: patch.server_domain ?? current.server_domain,
      inbound_identity_mode: patch.inbound_identity_mode ?? current.inbound_identity_mode,
      transport: patch.transport ?? current.transport,
      signaling_encoding: patch.signaling_encoding ?? current.signaling_encoding,
    });
    if (!updated) return false;
    const candidate = profiles.value.map((profile, candidateIndex) =>
      candidateIndex === index ? updated : profile,
    );
    const result = persistCandidate({ profiles: candidate, activeProfileId: activeId.value });
    if (!result.ok) return false;
    profiles.value = candidate;
    removeLegacyActiveAfterCommit(result);
    return true;
  }

  async function removeProfile(id: string): Promise<boolean> {
    if (profiles.value.length <= 1 || !profiles.value.some((profile) => profile.id === id)) return false;
    const candidate = profiles.value.filter((profile) => profile.id !== id);
    const nextActiveId = activeId.value === id ? candidate[0].id : activeId.value;
    const result = persistCandidate({ profiles: candidate, activeProfileId: nextActiveId });
    if (!result.ok) return false;
    profiles.value = candidate;
    activeId.value = nextActiveId;
    removeLegacyActiveAfterCommit(result);
    try {
      await invoke("delete_platform_secret", { profileId: id });
    } catch {
      // 钥匙串清理失败不回滚已经提交的非敏感档案。
    }
    await refreshSecretStatus();
    return true;
  }

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
