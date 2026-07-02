import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// Tauri 期望前端跑在固定端口;开发时 Vite 提供热更新,由 Tauri 加载该地址。
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  // 产物给 Tauri 打包,相对路径加载。
  base: "./",
  build: {
    target: "es2021",
    outDir: "dist",
  },
});
