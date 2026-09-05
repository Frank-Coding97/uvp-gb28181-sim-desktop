// 主题切换:浅色 / 暗色 / 跟随系统。偏好使用 V1 envelope 持久化。
import { computed, ref, watch } from "vue";
import { darkTheme } from "naive-ui";
import {
  createVersionedStorage,
  type DecodeResult,
  type StorageSaveResult,
} from "./lib/versionedStorage";

export type ThemePreference = "light" | "dark" | "system";

interface ThemeDataV1 {
  preference: ThemePreference;
}

const STORE_KEY = "uvp_theme";

function isPreference(value: unknown): value is ThemePreference {
  return value === "light" || value === "dark" || value === "system";
}

function decodeTheme(value: unknown): DecodeResult<ThemeDataV1> {
  if (!value || typeof value !== "object" || !isPreference((value as { preference?: unknown }).preference)) {
    throw new Error("invalid theme preference");
  }
  const data = { preference: (value as { preference: ThemePreference }).preference };
  return {
    data,
    repaired: JSON.stringify(value) !== JSON.stringify(data),
    summary: "主题配置中的未知字段已移除",
  };
}

const themeStorage = createVersionedStorage<ThemeDataV1>({
  key: STORE_KEY,
  schemaVersion: 1,
  defaultValue: () => ({ preference: "system" }),
  decode: decodeTheme,
  parseLegacyRaw: (raw) => isPreference(raw) ? raw : undefined,
  migrate(value, fromVersion) {
    if (fromVersion !== null || !isPreference(value)) throw new Error("unsupported theme version");
    return { data: { preference: value }, summary: "旧版主题偏好已迁移" };
  },
});

const initial = themeStorage.load();
const preference = ref<ThemePreference>(initial.data.preference);

function systemPrefersDark(): boolean {
  return typeof window !== "undefined" && typeof window.matchMedia === "function"
    ? window.matchMedia("(prefers-color-scheme: dark)").matches
    : false;
}

const systemDark = ref(systemPrefersDark());

if (typeof window !== "undefined" && typeof window.matchMedia === "function") {
  const query = window.matchMedia("(prefers-color-scheme: dark)");
  const onChange = (event: MediaQueryListEvent) => {
    systemDark.value = event.matches;
  };
  if (typeof query.addEventListener === "function") query.addEventListener("change", onChange);
}

export const isDark = computed(() =>
  preference.value === "system" ? systemDark.value : preference.value === "dark",
);

function apply() {
  if (typeof document === "undefined") return;
  document.documentElement.dataset.theme = isDark.value ? "dark" : "light";
  document.documentElement.style.colorScheme = isDark.value ? "dark" : "light";
}

watch(isDark, apply, { immediate: true });

export function useTheme() {
  /** 先提交存储，成功后再改变内存与界面主题。 */
  function setPreference(next: ThemePreference): StorageSaveResult {
    const result = themeStorage.save({ preference: next });
    if (result.ok) preference.value = next;
    return result;
  }

  function toggle(): StorageSaveResult {
    return setPreference(isDark.value ? "light" : "dark");
  }

  const naiveTheme = computed(() => (isDark.value ? darkTheme : null));
  const label = computed(() =>
    preference.value === "system"
      ? `跟随系统(当前${systemDark.value ? "暗色" : "浅色"})`
      : preference.value === "dark"
        ? "暗色"
        : "浅色",
  );

  return { preference, isDark, naiveTheme, label, setPreference, toggle };
}
