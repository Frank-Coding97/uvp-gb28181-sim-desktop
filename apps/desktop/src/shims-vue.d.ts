// Vue SFC 类型声明,让 TS 识别 .vue 导入。
//
// 用 `Record<string, unknown>` 而不是 `{}`:后者是"任何非 null 值",
// 包括 `0` 和 `""`,等于没有约束(eslint 的 no-empty-object-type 就是在拦这个)。
declare module "*.vue" {
  import type { DefineComponent } from "vue";
  const component: DefineComponent<
    Record<string, unknown>,
    Record<string, unknown>,
    unknown
  >;
  export default component;
}
