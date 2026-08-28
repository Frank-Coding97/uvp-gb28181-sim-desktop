// 单测配置独立于 vite.config.ts。
//
// 为什么分开:vitest 自带一份 vite,与项目里的 Vite 8(rolldown)在插件类型上不兼容,
// 放在同一个 defineConfig 里会让 vue-tsc 报一堆插件类型冲突。
// 现有测试都是纯 TypeScript 逻辑(不挂载组件),因此这里不需要 @vitejs/plugin-vue。
import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // 纯逻辑用 node 环境;将来要测组件的话在文件头加 `// @vitest-environment jsdom`。
    environment: "node",
    include: ["src/**/*.{test,spec}.ts"],
    reporters: ["default"],
  },
});
