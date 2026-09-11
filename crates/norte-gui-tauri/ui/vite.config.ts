import { defineConfig } from "vite";

// Assets EMPAQUETADOS y nada más: la webview de producción no habla con
// ningún servidor de desarrollo, no trae sourcemaps de terceros y no carga
// nada remoto (ADR 0066, decisión D11).
export default defineConfig({
  root: ".",
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2022",
    // Rutas RELATIVAS: la webview sirve desde `tauri://localhost`, y un
    // `/assets/...` absoluto se resuelve fuera del bundle.
    assetsDir: "assets",
    // NADA se incrusta como `data:`: la CSP de la webview no admite fuentes
    // en línea, y la de símbolos Nerd (3,8 KB) quedaba por debajo del
    // límite por defecto (4 KB), se incrustaba, y cada icono era una caja.
    // Como fichero en `assets/` carga igual que las otras.
    assetsInlineLimit: 0,
    sourcemap: false,
    // Un solo fichero por tipo: menos peticiones en el arranque frío, que es
    // una de las cosas que este spike mide.
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
