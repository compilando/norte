import type { Config } from "tailwindcss";

const config: Config = {
  content: [
    "./app/**/*.{js,ts,jsx,tsx,mdx}",
    "./components/**/*.{js,ts,jsx,tsx,mdx}",
  ],
  theme: {
    extend: {
      colors: {
        base: "#07090A",
        surface: "#0E1214",
        elevated: "#151A1D",
        line: "#252B2D",
        ink: "#F1F5EF",
        muted: "#89938C",
        phosphor: "#B7FF52",
        cyan: "#87DDD7",
      },
      fontFamily: {
        sans: ["var(--font-inter)", "sans-serif"],
        mono: ["var(--font-jetbrains)", "monospace"],
      },
      boxShadow: {
        terminal: "0 42px 120px rgba(0, 0, 0, 0.58)",
        glow: "0 0 44px rgba(183, 255, 82, 0.18)",
      },
    },
  },
  plugins: [],
};

export default config;
