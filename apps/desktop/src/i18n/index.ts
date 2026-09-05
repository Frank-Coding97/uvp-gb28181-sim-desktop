import { computed, ref } from "vue";
import type { SupportedLocale, TranslationSchema } from "./types";
import { zhCN } from "./locales/zh-CN";
import { enUS } from "./locales/en-US";

const LOCALE_STORE_KEY = "uvp_locale";
const translations: Record<SupportedLocale, TranslationSchema> = {
  "zh-CN": zhCN,
  "en-US": enUS,
};

function getInitialLocale(): SupportedLocale {
  try {
    const saved = localStorage.getItem(LOCALE_STORE_KEY);
    if (saved === "en-US" || saved === "zh-CN") {
      return saved;
    }
  } catch {
    // 忽略无法访问 localStorage 时的错误
  }
  return "zh-CN";
}

const currentLocale = ref<SupportedLocale>(getInitialLocale());

export function useI18n() {
  function setLocale(locale: SupportedLocale) {
    currentLocale.value = locale;
    try {
      localStorage.setItem(LOCALE_STORE_KEY, locale);
    } catch {
      // 容错处理
    }
  }

  const messages = computed(() => translations[currentLocale.value] || zhCN);

  function t(path: string): string {
    const keys = path.split(".");
    let current: unknown = messages.value;
    for (const key of keys) {
      if (current && typeof current === "object" && key in current) {
        current = (current as Record<string, unknown>)[key];
      } else {
        return path;
      }
    }
    return typeof current === "string" ? current : path;
  }

  return {
    locale: currentLocale,
    setLocale,
    messages,
    t,
  };
}
