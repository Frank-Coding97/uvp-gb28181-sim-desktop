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
    chunkSizeWarningLimit: 700,
    // Vite 8 / Rolldown：按稳定依赖域拆分静态 vendor chunk。页面仍保持静态
    // import，避免 Tauri 自定义协议下按路由懒加载造成空白，同时降低单文件解析成本。
    rolldownOptions: {
      output: {
        codeSplitting: {
          groups: [
            { name: "charts", test: /node_modules[\\/](echarts|zrender)[\\/]/, priority: 30 },
            { name: "ui", test: /node_modules[\\/](naive-ui|vueuc|treemate|css-render|vdirs|vooks|date-fns|date-fns-tz)[\\/]/, priority: 20 },
            { name: "framework", test: /node_modules[\\/](@vue|vue|vue-router)[\\/]/, priority: 15 },
            { name: "vendor", test: /node_modules[\\/]/, priority: 10 },
          ],
        },
      },
    },
  },
});
