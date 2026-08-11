import { defineConfig, type PluginOption } from "vite";
import vue from "@vitejs/plugin-vue";
import { visualizer } from "rollup-plugin-visualizer";

// Tauri 期望固定端口；前端构建产物输出到 dist/（tauri.conf.json 的 frontendDist）
const host = process.env.TAURI_DEV_HOST;

// 按需分析 bundle 构成：ANALYZE=1 pnpm build → 仓库根 bundle-stats.html（gitignored）。
const analyze = process.env.ANALYZE
  ? [visualizer({ filename: "bundle-stats.html", gzipSize: true }) as PluginOption]
  : [];

// Marked (pulled in by Mermaid) detects regexp lookbehind at runtime, but the
// bundler can constant-fold `new RegExp("(?<=…)")` using the build machine and
// emit an unconditional lookbehind for Safari 13. Keep the pattern dynamic so
// Catalina falls back to Marked's non-lookbehind rule as intended.
const preserveMarkedLookbehindDetection: PluginOption = {
  name: "preserve-marked-lookbehind-detection",
  enforce: "pre",
  transform(code, id) {
    if (!id.includes("/marked") || !id.endsWith("/lib/marked.esm.js")) return;
    const probe = 'new RegExp("(?<=1)(?<!1)")';
    if (!code.includes(probe)) return;
    return code.replace(
      probe,
      'new RegExp("(?" + String.fromCharCode(60) + "=1)(?" + String.fromCharCode(60) + "!1)")',
    );
  },
};

export default defineConfig({
  plugins: [preserveMarkedLookbehindDetection, vue(), ...analyze],
  // 前端源码与入口 index.html 都在 src/，故以 src 为 Vite 根目录。
  root: "src",
  // Tauri CLI 通过 env 注入，避免 vite 清屏吞掉 Rust 日志
  clearScreen: false,
  server: {
    port: 5180,
    strictPort: true,
    host: host || false,
    hmr: host
      ? { protocol: "ws", host, port: 5181 }
      : undefined,
    watch: {
      // 不监听 Rust 与 Swift 源码，减少无谓重载
      ignored: ["**/src-tauri/**", "**/Sources/**", "**/target/**"],
    },
  },
  build: {
    // Safari 13.1 = macOS 10.15 Catalina 系统 WebKit 上限。
    // es2021 会留下 ||= / &&= / ??=，在 Catalina WKWebView 里语法错误 → 空白弹窗。
    // 二进制 LC_VERSION_MIN 为 10.13，前端目标需与之对齐。
    target: ["es2019", "safari13"],
    // 输出到仓库根的 dist/（tauri.conf.json 的 frontendDist=../dist）。
    outDir: "../dist",
    emptyOutDir: true,
  },
});
