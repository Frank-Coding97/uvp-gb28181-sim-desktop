// 应用入口：引入 Naive UI 全局配置 + vue-router。
import { createApp } from "vue";
import { createRouter, createWebHashHistory } from "vue-router";
import App from "./App.vue";

// 懒加载各页面（减少初始包体积）。
const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: "/", redirect: "/dashboard" },
    { path: "/dashboard", component: () => import("./views/Dashboard.vue") },
    { path: "/config",    component: () => import("./views/Config.vue") },
    { path: "/scenario",  component: () => import("./views/Scenario.vue") },
    { path: "/monitor",   component: () => import("./views/Monitor.vue") },
  ],
});

createApp(App).use(router).mount("#app");
