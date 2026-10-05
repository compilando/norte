import { defineConfig } from "vitest/config";

export default defineConfig({
  // Same allowance as `vite.config.ts`: the shared panel icons.
  server: { fs: { allow: [".", "../../norte-frontend/assets/panel-icons"] } },
  test: {
    environment: "jsdom",
    include: ["tests/**/*.test.ts"],
  },
});
