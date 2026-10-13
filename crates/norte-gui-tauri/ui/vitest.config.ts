import { defineConfig } from "vitest/config";

export default defineConfig({
  // Same allowance as `vite.config.ts`: the shared icons.
  server: {
    fs: {
      allow: [
        ".",
        "../../norte-frontend/assets/panel-icons",
        "../../norte-frontend/assets/ui-icons",
      ],
    },
  },
  test: {
    environment: "jsdom",
    include: ["tests/**/*.test.ts"],
  },
});
