import type { Config } from "tailwindcss";

const config: Config = {
  content: [
    "./app/**/*.{js,ts,jsx,tsx,mdx}",
    "./components/**/*.{js,ts,jsx,tsx,mdx}",
  ],
  theme: {
    extend: {
      colors: {
        base: "#0A0A0B",
        surface: "#131316",
        line: "#26262B",
        ink: "#E8E8EA",
        muted: "#8A8A94",
        phosphor: "#4ADE80",
      },
      fontFamily: {
        sans: ["var(--font-inter)", "sans-serif"],
        mono: ["var(--font-jetbrains)", "monospace"],
      },
      boxShadow: {
        terminal: "0 28px 80px rgba(0, 0, 0, 0.35)",
        glow: "0 0 28px rgba(74, 222, 128, 0.12)",
      },
    },
  },
  plugins: [],
};

export default config;
