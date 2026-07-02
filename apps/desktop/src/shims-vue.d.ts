// Vue SFC 类型声明,让 TS 识别 .vue 导入。
declare module "*.vue" {
  import type { DefineComponent } from "vue";
  const component: DefineComponent<{}, {}, any>;
  export default component;
}
