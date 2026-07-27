// 应用入口:引入 vue-router。
// 注意:页面用静态 import(非懒加载)。Tauri 的 tauri://localhost 协议下,
// 动态 import() 生成的分包 chunk 相对路径解析失败会导致内容区空白,故全部打进主包。
import { createApp } from "vue";
import { createRouter, createWebHashHistory } from "vue-router";
import "./theme.css";
import App from "./App.vue";
import Simulator from "./views/Simulator.vue";

const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: "/", redirect: "/simulator" },
    { path: "/simulator", component: Simulator },
    { path: "/:pathMatch(.*)*", redirect: "/simulator" },
  ],
});

createApp(App).use(router).mount("#app");
