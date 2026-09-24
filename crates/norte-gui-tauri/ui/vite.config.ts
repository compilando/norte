import { defineConfig } from "vite";

// BUNDLED assets and nothing else: the production webview doesn't talk to
// any dev server, doesn't ship third-party sourcemaps, and loads nothing
// remote (ADR 0066, decision D11).
export default defineConfig({
  root: ".",
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2022",
    // RELATIVE paths: the webview serves from `tauri://localhost`, and an
    // absolute `/assets/...` resolves outside the bundle.
    assetsDir: "assets",
    // NOTHING gets inlined as `data:`: the webview's CSP doesn't allow
    // inline fonts, and the Nerd symbols one (3.8 KB) fell under the default
    // limit (4 KB), got inlined, and every icon was a box. As a file under
    // `assets/` it loads the same as the others.
    assetsInlineLimit: 0,
    sourcemap: false,
    // One file per type: fewer requests at cold start, which is one of the
    // things this spike measures.
    rollupOptions: {
      output: {
        entryFileNames: "assets/[name].js",
        chunkFileNames: "assets/[name].js",
        assetFileNames: "assets/[name].[ext]",
      },
    },
  },
  base: "./",
  clearScreen: false,
});
