// Vue SFC 类型声明,让 TS 识别 .vue 导入。
declare module "*.vue" {
  import type { DefineComponent } from "vue";
  const component: DefineComponent<{}, {}, any>;
  export default component;
}

// 构建期注入的应用版本号（来自 src-tauri/tauri.conf.json 的 version，见 vite.config.ts）。
// 全局只此一处定义，UI 不要另行硬编码版本串。
declare const __APP_VERSION__: string;
