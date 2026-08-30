// 应用入口：引入 vue-router。
// 注意:页面用**静态 import**(非懒加载)。Tauri 的 tauri://localhost 协议下,
// 动态 import() 生成的分包 chunk 相对路径解析失败会导致内容区空白,故全部打进主包。
import { createApp } from "vue";
import { createRouter, createWebHashHistory } from "vue-router";
import "./theme.css";
import App from "./App.vue";
import Dashboard from "./views/Dashboard.vue";
import Device from "./views/Device.vue";
import Channels from "./views/Channels.vue";
import Scenario from "./views/Scenario.vue";
import System from "./views/System.vue";
import Preview from "./views/Preview.vue";
import CameraDemo from "./views/CameraDemo.vue";
import Settings from "./views/Settings.vue";

const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: "/", redirect: "/dashboard" },
    { path: "/dashboard", component: Dashboard },
    { path: "/device",    component: Device },
    { path: "/preview",   component: Preview },
    { path: "/channels",  component: Channels },
    // 压力测试(场景编排 + 实时监控已合并到一页)。
    { path: "/scenario",  component: Scenario },
    { path: "/device-settings",  component: Settings, props: { section: "device" } },
    { path: "/media-settings",   component: Settings, props: { section: "media" } },
    { path: "/network-settings", component: Settings, props: { section: "network" } },
    { path: "/about",            component: Settings, props: { section: "about" } },
    { path: "/system",    component: System },
    { path: "/camera-demo", component: CameraDemo },
    { path: "/settings",  redirect: "/device-settings" }, // 旧设置入口兼容
    { path: "/monitor",   redirect: "/scenario" }, // 旧路由兼容,重定向到合并页
    { path: "/config",    redirect: "/device" },   // 平台配置已并入顶栏,旧路由重定向
  ],
});

createApp(App).use(router).mount("#app");
