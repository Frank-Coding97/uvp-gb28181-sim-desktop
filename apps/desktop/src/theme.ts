// 主题切换:浅色 / 暗色 / 跟随系统。
//
// 实现方式是切 `<html data-theme>`,CSS 侧只覆盖设计令牌(见 theme.css),
// 因此所有组件里的 `var(--xxx)` 自动跟随,不需要逐个组件改样式。
// Naive UI 侧同步切 `darkTheme`,否则内建组件(Select/Modal/Menu)会与页面配色打架。
import { computed, ref, watch } from "vue";
import { darkTheme } from "naive-ui";

export type ThemePreference = "light" | "dark" | "system";

const STORE_KEY = "uvp_theme";

function loadPreference(): ThemePreference {
  try {
    const saved = localStorage.getItem(STORE_KEY);
    if (saved === "light" || saved === "dark" || saved === "system") return saved;
  } catch {
    // 忽略存储不可用
  }
  return "system";
}

/** 系统是否偏好暗色。无 matchMedia(测试环境)时按浅色。 */
function systemPrefersDark(): boolean {
  return typeof window !== "undefined" && typeof window.matchMedia === "function"
    ? window.matchMedia("(prefers-color-scheme: dark)").matches
    : false;
}

const preference = ref<ThemePreference>(loadPreference());
const systemDark = ref(systemPrefersDark());

// 跟随系统时,系统主题变化要实时生效。
if (typeof window !== "undefined" && typeof window.matchMedia === "function") {
  const query = window.matchMedia("(prefers-color-scheme: dark)");
  const onChange = (e: MediaQueryListEvent) => {
    systemDark.value = e.matches;
  };
  if (typeof query.addEventListener === "function") {
    query.addEventListener("change", onChange);
  }
}

/** 实际生效的是否暗色。 */
export const isDark = computed(() =>
  preference.value === "system" ? systemDark.value : preference.value === "dark",
);

function apply() {
  if (typeof document === "undefined") return;
  document.documentElement.dataset.theme = isDark.value ? "dark" : "light";
  // 让原生控件(滚动条、表单)也跟随。
  document.documentElement.style.colorScheme = isDark.value ? "dark" : "light";
}

watch(isDark, apply, { immediate: true });

export function useTheme() {
  function setPreference(next: ThemePreference) {
    preference.value = next;
    try {
      localStorage.setItem(STORE_KEY, next);
    } catch {
      // 忽略存储不可用
    }
  }

  /** 在浅色 / 暗色之间切换(当前是"跟随系统"时,切到与系统相反的那个)。 */
  function toggle() {
    setPreference(isDark.value ? "light" : "dark");
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
