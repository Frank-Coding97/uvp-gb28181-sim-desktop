import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const [app, router, settings, channels, system] = await Promise.all([
  readFile(new URL("./App.vue", import.meta.url), "utf8"),
  readFile(new URL("./main.ts", import.meta.url), "utf8"),
  readFile(new URL("./views/Settings.vue", import.meta.url), "utf8"),
  readFile(new URL("./views/Channels.vue", import.meta.url), "utf8"),
  readFile(new URL("./views/System.vue", import.meta.url), "utf8"),
]);

const expectedMenu = [
  ["首页", "/dashboard"],
  ["设备联调", "/device"],
  ["目录管理", "/channels"],
  ["压力测试", "/scenario"],
  ["设备配置", "/device-settings"],
  ["音视频配置", "/media-settings"],
  ["网络配置", "/network-settings"],
  ["运行日志", "/system"],
  ["关于", "/about"],
];

for (const [label, path] of expectedMenu) {
  assert.match(app, new RegExp(`label: "${label}"[\\s\\S]{0,80}key: "${path}"`), `${label} 必须是一级菜单`);
}
assert.doesNotMatch(app, /label: "设置"/, "全局菜单不应继续保留设置聚合入口");
assert.doesNotMatch(app, /label: "单设备联调"|label: "多通道目录"|label: "系统信息"/, "菜单应使用确认后的桌面端名称");

assert.match(router, /path: "\/device-settings"[\s\S]{0,100}section: "device"/, "设备配置必须有独立路由");
assert.match(router, /path: "\/media-settings"[\s\S]{0,100}section: "media"/, "音视频配置必须有独立路由");
assert.match(router, /path: "\/network-settings"[\s\S]{0,100}section: "network"/, "网络配置必须有独立路由");
assert.match(router, /path: "\/about"[\s\S]{0,100}section: "about"/, "关于必须有独立路由");
assert.match(router, /path: "\/settings"[\s\S]{0,80}redirect: "\/device-settings"/, "旧设置地址必须兼容重定向");

assert.doesNotMatch(settings, /settings-nav-item|nav-caption">设置项/, "设置页内部不应再保留二级菜单");
assert.match(settings, /defineProps<[\s\S]*section:/, "配置页面必须由一级路由决定内容");
assert.match(settings, /OSD 尚未接入媒体引擎/, "OSD 能力说明必须保留在音视频配置中");
assert.match(settings, /v-else-if="activeKey === 'about'"/, "配置未加载时不得误显示关于页面");
assert.match(settings, /配置正在加载/, "配置未加载时必须显示明确的等待状态");

assert.match(channels, /<div class="page-title">目录管理<\/div>/, "通道页面应统一命名为目录管理");
assert.match(channels, /当前通道名称/, "目录管理应承载当前通道配置");
assert.match(system, /<div class="page-title">运行日志<\/div>/, "日志页面标题应与菜单一致");
