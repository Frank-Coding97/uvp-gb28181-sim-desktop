// ESLint 配置(flat config)。
//
// # 为什么加它
//
// 此前前端的质量闸只有 `vue-tsc --noEmit` + `vitest`,也就是"类型对、测试过"。
// 但本轮修掉的一大批问题类型检查完全看不见:
//   · `<label>` 与控件没有关联(屏幕阅读器读不出字段名);
//   · 分段控件缺 `aria-pressed`(选中状态只有视觉);
//   · `<button>` 缺 `type`;
//   · 未使用的变量与 import 堆积。
// 没有 lint,这些问题下一次改动就会重新长回来。`vuejs-accessibility` 这套规则
// 恰好能自动拦住其中大部分。
import js from "@eslint/js";
import tseslint from "typescript-eslint";
import pluginVue from "eslint-plugin-vue";
import pluginA11y from "eslint-plugin-vuejs-accessibility";

export default tseslint.config(
  {
    // 构建产物与依赖不参与检查。
    ignores: ["dist/**", "node_modules/**", "src-tauri/**"],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  ...pluginVue.configs["flat/recommended"],
  ...pluginA11y.configs["flat/recommended"],
  {
    files: ["**/*.{ts,vue}"],
    languageOptions: {
      parserOptions: {
        // .vue 里的 <script setup lang="ts"> 要交给 TS 解析器。
        parser: tseslint.parser,
        ecmaVersion: "latest",
        sourceType: "module",
      },
      globals: {
        // 浏览器/WebView 环境。只列真正用到的,避免把拼错的全局名放过去。
        window: "readonly",
        document: "readonly",
        navigator: "readonly",
        console: "readonly",
        setInterval: "readonly",
        clearInterval: "readonly",
        setTimeout: "readonly",
        clearTimeout: "readonly",
        requestAnimationFrame: "readonly",
        cancelAnimationFrame: "readonly",
        localStorage: "readonly",
        MediaStream: "readonly",
        HTMLElement: "readonly",
        HTMLVideoElement: "readonly",
        HTMLCanvasElement: "readonly",
        ResizeObserver: "readonly",
        performance: "readonly",
        matchMedia: "readonly",
        URL: "readonly",
        Blob: "readonly",
      },
    },
    rules: {
      // ── 可访问性:本轮修过的几类问题在这里被钉住 ──
      "vuejs-accessibility/label-has-for": [
        "error",
        // 允许 `for=` 或包裹两种关联方式,但**必须有一种**。
        { required: { some: ["nesting", "id"] }, allowChildren: false },
      ],
      "vuejs-accessibility/form-control-has-label": "error",
      "vuejs-accessibility/click-events-have-key-events": "error",
      "vuejs-accessibility/no-static-element-interactions": "error",

      // ── TypeScript ──
      // 下划线前缀表示"刻意不用"(如解构时跳过的字段),不报错。
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_", caughtErrors: "none" },
      ],
      // Tauri 的 invoke 返回 unknown,少数处需要断言;关掉这条比到处加 eslint-disable 干净。
      "@typescript-eslint/no-explicit-any": "warn",

      // ── Vue ──
      // 单文件组件名用 PascalCase 已由约定保证,这条对本项目只会产生噪声。
      "vue/multi-word-component-names": "off",
      // 属性换行由 Prettier 之类的格式化器决定,不在 lint 里管。
      "vue/max-attributes-per-line": "off",
      "vue/singleline-html-element-content-newline": "off",
      "vue/html-self-closing": "off",
      "vue/html-indent": "off",
      "vue/html-closing-bracket-newline": "off",
      "vue/attributes-order": "off",
      "vue/multiline-html-element-content-newline": "off",
      // 这条要求 <button> 显式写 type:漏掉会在表单里意外触发提交。
      "vue/html-button-has-type": "error",
    },
  },
  {
    // 测试文件里 vitest 的全局变量。
    files: ["**/*.test.ts"],
    languageOptions: {
      globals: { global: "readonly" },
    },
  },
);
