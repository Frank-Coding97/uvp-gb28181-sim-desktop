// 应用入口：引入 vue-router。
// 注意:页面用**静态 import**(非懒加载)。Tauri 的 tauri://localhost 协议下,
// 动态 import() 生成的分包 chunk 相对路径解析失败会导致内容区空白,故全部打进主包。
import { createApp } from "vue";
import { createRouter, createWebHashHistory } from "vue-router";
import "./theme.css";
import App from "./App.vue";
import Dashboard from "./views/Dashboard.vue";
import Device from "./views/Device.vue";
import Config from "./views/Config.vue";
import Scenario from "./views/Scenario.vue";
import Monitor from "./views/Monitor.vue";

const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: "/", redirect: "/dashboard" },
    { path: "/dashboard", component: Dashboard },
    { path: "/device",    component: Device },
    { path: "/config",    component: Config },
    { path: "/scenario",  component: Scenario },
    { path: "/monitor",   component: Monitor },
  ],
});

createApp(App).use(router).mount("#app");
