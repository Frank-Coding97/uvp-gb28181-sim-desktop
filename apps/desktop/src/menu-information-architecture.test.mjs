import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const [app, router, settings, channels, system, dashboard, ptz] = await Promise.all([
  readFile(new URL("./App.vue", import.meta.url), "utf8"),
  readFile(new URL("./main.ts", import.meta.url), "utf8"),
  readFile(new URL("./views/Settings.vue", import.meta.url), "utf8"),
  readFile(new URL("./views/Channels.vue", import.meta.url), "utf8"),
  readFile(new URL("./views/System.vue", import.meta.url), "utf8"),
  readFile(new URL("./views/Dashboard.vue", import.meta.url), "utf8"),
  readFile(new URL("./views/Ptz.vue", import.meta.url), "utf8"),
]);

const expectedMenu = [
  ["首页", "/dashboard"],
  ["云台控制", "/ptz"],
  ["目录管理", "/channels"],
  ["压力测试", "/scenario"],
  ["设备配置", "/device-settings"],
  ["音视频配置", "/media-settings"],
  ["网络配置", "/network-settings"],
  ["日志", "/logs"],
  ["关于", "/about"],
];

for (const [label, path] of expectedMenu) {
  assert.match(app, new RegExp(`label: "${label}"[\\s\\S]{0,80}key: "${path}"`), `${label} 必须是一级菜单`);
}
assert.doesNotMatch(app, /label: "设置"/, "全局菜单不应继续保留设置聚合入口");
assert.doesNotMatch(app, /label: "设备联调"/, "设备联调不应继续占用一级菜单");
assert.doesNotMatch(app, /label: "单设备联调"|label: "多通道目录"|label: "系统信息"/, "菜单应使用确认后的桌面端名称");

assert.match(router, /path: "\/device-settings"[\s\S]{0,100}section: "device"/, "设备配置必须有独立路由");
assert.match(router, /path: "\/media-settings"[\s\S]{0,100}section: "media"/, "音视频配置必须有独立路由");
assert.match(router, /path: "\/network-settings"[\s\S]{0,100}section: "network"/, "网络配置必须有独立路由");
assert.match(router, /path: "\/about"[\s\S]{0,100}section: "about"/, "关于必须有独立路由");
assert.match(router, /path: "\/settings"[\s\S]{0,80}redirect: "\/device-settings"/, "旧设置地址必须兼容重定向");
assert.match(router, /path: "\/device"[\s\S]{0,80}redirect: "\/dashboard"/, "旧设备联调地址必须回到首页");
assert.match(router, /path: "\/system"[\s\S]{0,80}redirect: "\/logs"/, "旧运行日志地址必须兼容重定向");
assert.match(router, /path: "\/ptz"[\s\S]{0,80}component: Ptz/, "云台控制必须有独立一级路由");

assert.doesNotMatch(settings, /settings-nav-item|nav-caption">设置项/, "设置页内部不应再保留二级菜单");
assert.match(settings, /defineProps<[\s\S]*section:/, "配置页面必须由一级路由决定内容");
assert.match(settings, /OSD 尚未接入媒体引擎/, "OSD 能力说明必须保留在音视频配置中");
assert.match(settings, /v-else-if="activeKey === 'about'"/, "配置未加载时不得误显示关于页面");
assert.match(settings, /配置正在加载/, "配置未加载时必须显示明确的等待状态");

assert.match(channels, /<div class="page-title">目录管理<\/div>/, "通道页面应统一命名为目录管理");
assert.match(channels, /当前通道名称/, "目录管理应承载当前通道配置");
assert.match(system, /<div class="page-title">日志<\/div>/, "日志页面标题应与菜单一致");
assert.match(system, /SIP 日志/, "日志页必须承载 SIP 信令日志");
assert.match(system, /系统日志/, "日志页必须承载系统运行日志");
assert.match(system, /平台命令/, "平台命令时间线必须迁移到日志页");
assert.match(ptz, /<div class="page-title">云台控制<\/div>/, "云台控制必须成为独立页面");
assert.match(ptz, /ptz_action/, "云台页必须继续消费平台 PTZ 命令");
assert.match(dashboard, /设备状态真相/, "设备运行时真相必须迁移到首页");
