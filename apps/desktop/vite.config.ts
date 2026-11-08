import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
// 版本号唯一真源：tauri.conf.json（Tauri 打包实际读它，产物文件名、安装器版本、
// .app 版本都来自这里）。前端不再硬编码，由构建期注入 __APP_VERSION__，
// 避免出现「tag 是新的、包是旧的、界面显示的又是第三个值」。
import tauriConf from "./src-tauri/tauri.conf.json";

// Tauri 期望前端跑在固定端口;开发时 Vite 提供热更新,由 Tauri 加载该地址。
//
// host 必须显式写 127.0.0.1，不能留空用默认的 "localhost"：
// Windows 上 localhost 默认解析成 ::1，Vite 就只监听 [::1]:1420；而 WebView2
// (Chromium) 访问 http://localhost:1420 时会先试 127.0.0.1，被拒后回落到 ::1，
// 这个回落在某些时刻会直接 net::ERR_ABORTED 掉整个导航——表现为窗口白屏、
// document 停在 about:blank、readyState 卡在 interactive，手动刷新都不一定好。
// 固定 IPv4 后两端完全一致，不再有解析/回落。
export default defineConfig({
  // 应用版本号，UI 各处统一引用 __APP_VERSION__，不再各自写死。
  define: {
    __APP_VERSION__: JSON.stringify(tauriConf.version),
  },
  plugins: [
    vue(),
    {
      // 给 Vite 注入的 HMR 客户端脚本加 async。
      //
      // 它在 Vite 8 里首次 transform 要 ~5s（209KB，含 inline sourcemap），而它对
      // 首屏渲染毫无作用（只是热更新 + 错误浮层）。默认它是 module script，按文档
      // 顺序执行，会挡住后面的 /src/main.ts，等于把整块 5s 记在首屏账上。
      // 加 async 后它与 main.ts 并行下载执行，界面先出来，HMR 随后接上。
      name: "uvp:async-hmr-client",
      apply: "serve",
      enforce: "post",
      transformIndexHtml(html: string) {
        return html.replace(
          /<script([^>]*?)src="\/@vite\/client"([^>]*)><\/script>/,
          '<script$1src="/@vite/client"$2 async></script>',
        );
      },
    },
  ],
  clearScreen: false,
  server: {
    host: "127.0.0.1",
    port: 1420,
    strictPort: true,
    watch: {
      // 编辑器/工具做原子写时会创建 .xxx.tmpdir 临时目录；Windows 上这些目录
      // 可能被锁住，Vite 的 chokidar 一 watch 就 EBUSY 把整个 dev server 打崩。
      // 这些临时文件不参与构建，直接忽略。
      ignored: ["**/.*.tmpdir/**", "**/*.tmp"],
    },
  },
  // CSS 用 lightningcss 转换，不用默认的 postcss。
  //
  // 原因（2026-11-05 实测，同一份代码、同一个 Windows 环境）：
  //   postcss 管道在一个 Vite 进程里**第一次**处理 CSS 要 ~8.8s（theme.css 首请求
  //   8863ms，且所有 .vue 的 scoped style 都排在它后面一起冻结）。换成 lightningcss
  //   后同一请求 2669ms。postcss 的那 8.8s 是进程级一次性初始化，之后同进程只要 1~2ms，
  //   所以它只影响“冷启动后的第一次导航”，表现就是“窗口出来了但界面要等好几秒”。
  //
  // 输出已逐条比对：color-mix(var()) / backdrop-filter / var() 全部原样保留，
  // 只有 rgba(0,0,0,.12) 这类被压成 #0000001f（等价写法），渲染结果一致。
  css: {
    transformer: "lightningcss",
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
