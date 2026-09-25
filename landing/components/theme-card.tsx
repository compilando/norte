"use client";

import type { ReactNode } from "react";
import { THEME_EVENT } from "./hero-stage";

/** A theme in the gallery: clicking it paints the hero in that theme. */
export function ThemeCard({ id, children }: { id: string; children: ReactNode }) {
  return (
    <button
      type="button"
      onClick={() => {
        window.dispatchEvent(new CustomEvent(THEME_EVENT, { detail: id }));
        document.getElementById("stage")?.scrollIntoView({ behavior: "smooth", block: "center" });
      }}
      className="group block w-full overflow-hidden rounded-lg border border-white/[0.1] text-left transition hover:-translate-y-1 hover:border-phosphor/50 hover:shadow-glow"
    >
      <div className="pointer-events-none">{children}</div>
      <div className="flex items-center justify-between bg-[#0c0f10] px-3 py-2 font-mono text-[10px] text-muted group-hover:text-ink">
        <span>{id}</span>
        <span className="text-phosphor opacity-0 transition group-hover:opacity-100">↑</span>
      </div>
    </button>
  );
}
