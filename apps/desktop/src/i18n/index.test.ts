import { describe, it, expect, beforeEach, vi } from "vitest";
import { useI18n } from "./index";

class MemoryStorage {
  map = new Map<string, string>();
  getItem(key: string) {
    return this.map.get(key) ?? null;
  }
  setItem(key: string, value: string) {
    this.map.set(key, value);
  }
  removeItem(key: string) {
    this.map.delete(key);
  }
  clear() {
    this.map.clear();
  }
}

describe("i18n 基础设施", () => {
  let storage: MemoryStorage;

  beforeEach(() => {
    storage = new MemoryStorage();
    vi.stubGlobal("localStorage", storage);
  });

  it("默认采用中文语言包", () => {
    const { locale, t } = useI18n();
    expect(locale.value).toBe("zh-CN");
    expect(t("common.confirm")).toBe("确认");
    expect(t("nav.scenario")).toBe("压力测试");
  });

  it("切换到英文语言包并能正确翻译", () => {
    const { setLocale, t, locale } = useI18n();
    setLocale("en-US");
    expect(locale.value).toBe("en-US");
    expect(t("common.confirm")).toBe("Confirm");
    expect(t("nav.scenario")).toBe("Stress Test");
    expect(storage.getItem("uvp_locale")).toBe("en-US");
  });

  it("查询不存在的 key 时原样回显", () => {
    const { t } = useI18n();
    expect(t("non.existent.key")).toBe("non.existent.key");
  });
});
