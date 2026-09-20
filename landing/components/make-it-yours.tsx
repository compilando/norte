"use client";

import { useState } from "react";

/**
 * The palettes are the ones norte actually ships, read out of
 * `crates/norte-theme/presets/*.toml`, so the swatches are not an impression of
 * the product — they are its values.
 */
type Theme = {
  id: string;
  label: string;
  bg: string;
  fg: string;
  pane: string;
  paneFocus: string;
  stripe: string;
  accent: string;
  dir: string;
  selFg: string;
  selBg: string;
  statusFg: string;
  statusBg: string;
};

const themes: Theme[] = [
  { id: "default", label: "default", bg: "#1c1c1c", fg: "#d0d0d0", pane: "#1e1e1e", paneFocus: "#252526", stripe: "#232323", accent: "#5fafd7", dir: "#5fafd7", selFg: "#1c1c1c", selBg: "#5fafd7", statusFg: "#1c1c1c", statusBg: "#5fafd7" },
  { id: "nord", label: "nord", bg: "#2e3440", fg: "#d8dee9", pane: "#3b4252", paneFocus: "#434c5e", stripe: "#414859", accent: "#88c0d0", dir: "#81a1c1", selFg: "#2e3440", selBg: "#88c0d0", statusFg: "#2e3440", statusBg: "#88c0d0" },
  { id: "gruvbox-dark", label: "gruvbox dark", bg: "#282828", fg: "#ebdbb2", pane: "#3c3836", paneFocus: "#504945", stripe: "#423e3b", accent: "#83a598", dir: "#83a598", selFg: "#282828", selBg: "#83a598", statusFg: "#282828", statusBg: "#83a598" },
  { id: "gruvbox-light", label: "gruvbox light", bg: "#fbf1c7", fg: "#3c3836", pane: "#f2e5bc", paneFocus: "#ebdbb2", stripe: "#ece0b6", accent: "#af3a03", dir: "#076678", selFg: "#fbf1c7", selBg: "#b57614", statusFg: "#fbf1c7", statusBg: "#b57614" },
  { id: "catppuccin-mocha", label: "catppuccin mocha", bg: "#1e1e2e", fg: "#cdd6f4", pane: "#313244", paneFocus: "#45475a", stripe: "#363849", accent: "#89b4fa", dir: "#89b4fa", selFg: "#1e1e2e", selBg: "#89b4fa", statusFg: "#1e1e2e", statusBg: "#89b4fa" },
  { id: "catppuccin-latte", label: "catppuccin latte", bg: "#eff1f5", fg: "#4c4f69", pane: "#e6e9ef", paneFocus: "#dce0e8", stripe: "#dee2ea", accent: "#1e66f5", dir: "#1e66f5", selFg: "#eff1f5", selBg: "#1e66f5", statusFg: "#eff1f5", statusBg: "#1e66f5" },
  { id: "vscode-dark", label: "vscode dark", bg: "#1f1f1f", fg: "#cccccc", pane: "#181818", paneFocus: "#1f1f1f", stripe: "#252525", accent: "#4daafc", dir: "#4daafc", selFg: "#ffffff", selBg: "#04395e", statusFg: "#cccccc", statusBg: "#181818" },
  { id: "vscode-light", label: "vscode light", bg: "#ffffff", fg: "#3b3b3b", pane: "#f8f8f8", paneFocus: "#ffffff", stripe: "#ececec", accent: "#005fb8", dir: "#005fb8", selFg: "#000000", selBg: "#e8e8e8", statusFg: "#3b3b3b", statusBg: "#f8f8f8" },
  { id: "retro-crt", label: "retro crt", bg: "#0a0f0a", fg: "#2ee65c", pane: "#0d140d", paneFocus: "#122012", stripe: "#111a11", accent: "#7bff9e", dir: "#7bff9e", selFg: "#0a0f0a", selBg: "#2ee65c", statusFg: "#0a0f0a", statusBg: "#2ee65c" },
  { id: "retro-crt-amber", label: "retro crt amber", bg: "#100c04", fg: "#ffb000", pane: "#141006", paneFocus: "#1d1708", stripe: "#1a1509", accent: "#ffd070", dir: "#ffd070", selFg: "#100c04", selBg: "#ffb000", statusFg: "#100c04", statusBg: "#ffb000" },
];

const presets = ["orthodox", "vim", "cua", "krusader", "far", "norton", "total-commander"];

const rows = [
  { name: "design-system", size: "—", dir: true },
  { name: "release-candidates", size: "—", dir: true },
  { name: "DSC_2048.jpg", size: "8.4 MB", dir: false },
  { name: "invoice-final-3.pdf", size: "940 KB", dir: false },
  { name: "launch-assets.zip", size: "1.2 GB", dir: false },
  { name: "notes-from-call.md", size: "12 KB", dir: false },
];

function ThemedPane({ theme }: { theme: Theme }) {
  return (
    <div className="overflow-hidden rounded-xl border border-white/[0.1] font-mono text-[10px] shadow-card" style={{ background: theme.bg, color: theme.fg }}>
      <div className="flex h-9 items-center gap-2 px-3" style={{ background: theme.paneFocus }}>
        <span className="h-1.5 w-1.5 rounded-full" style={{ background: theme.accent }} />
        <span style={{ color: theme.accent }}>~/projects/northstar</span>
        <span className="ml-auto text-[8px] opacity-60">6 items</span>
      </div>
      <div className="px-1 py-1" style={{ background: theme.pane }}>
        {rows.map((row, index) => {
          const selected = index === 2;
          return (
            <div
              key={row.name}
              className="flex items-center gap-2 rounded px-2 py-[5px]"
              style={{
                background: selected ? theme.selBg : index % 2 === 1 ? theme.stripe : "transparent",
                color: selected ? theme.selFg : row.dir ? theme.dir : theme.fg,
                fontWeight: row.dir ? 600 : 400,
              }}
            >
              <span className="w-2 opacity-70">{row.dir ? "▸" : "·"}</span>
              <span className="min-w-0 flex-1 truncate">{row.name}</span>
              <span className="shrink-0 text-[9px] opacity-70">{row.size}</span>
            </div>
          );
        })}
      </div>
      <div className="flex h-7 items-center justify-between px-3 text-[8px] uppercase tracking-[0.1em]" style={{ background: theme.statusBg, color: theme.statusFg }}>
        <span>{theme.label}</span>
        <span>F5 copy · F6 move · F9 menu</span>
      </div>
    </div>
  );
}

export function MakeItYours() {
  const [active, setActive] = useState(themes[1]);

  return (
    <section id="yours" className="relative scroll-mt-16 py-28 sm:py-36 lg:py-44">
      <div className="mx-auto max-w-[1440px] px-5 sm:px-8 lg:px-12">
        <div className="grid gap-8 lg:grid-cols-[1fr_.6fr] lg:items-end">
          <div>
            <p className="font-mono text-[10px] uppercase tracking-[0.17em] text-phosphor">Make it yours</p>
            <h2 className="mt-6 max-w-3xl text-balance text-5xl font-medium leading-[.98] tracking-[-0.06em] sm:text-7xl">
              A tool you keep for years <span className="text-muted">should look and feel like yours.</span>
            </h2>
          </div>
          <p className="max-w-md text-base leading-7 text-muted lg:justify-self-end">
            Ten themes ship in the box, and both frontends wear them. Your keyboard is one of seven presets—or entirely your own, across a vocabulary of 180 named commands.
          </p>
        </div>

        <div className="mt-16 grid gap-4 lg:grid-cols-[1.15fr_.85fr]">
          <article className="rounded-2xl border border-line bg-surface/70 p-6 lg:p-8">
            <div className="flex flex-wrap items-center justify-between gap-3">
              <span className="font-mono text-[9px] uppercase tracking-[0.15em] text-phosphor">Themes · live</span>
              <code className="rounded-md border border-line px-2.5 py-1 font-mono text-[9px] text-muted">[ui] theme = &quot;{active.id}&quot;</code>
            </div>

            <div className="mt-6" role="group" aria-label="Choose a bundled theme">
              <div className="flex flex-wrap gap-2">
                {themes.map((theme) => (
                  <button
                    key={theme.id}
                    type="button"
                    onClick={() => setActive(theme)}
                    aria-pressed={active.id === theme.id}
                    title={theme.label}
                    className={`group flex items-center gap-2 rounded-full border px-2.5 py-1.5 font-mono text-[9px] transition ${
                      active.id === theme.id ? "border-phosphor/70 bg-phosphor/[0.08] text-ink" : "border-line text-muted hover:border-muted/60 hover:text-ink"
                    }`}
                  >
                    <span className="flex h-3.5 w-3.5 overflow-hidden rounded-full border border-white/10">
                      <span className="w-1/2" style={{ background: theme.bg }} />
                      <span className="w-1/2" style={{ background: theme.accent }} />
                    </span>
                    {theme.label}
                  </button>
                ))}
              </div>
            </div>

            <div className="mt-7">
              <ThemedPane theme={active} />
            </div>

            <p className="mt-6 text-sm leading-6 text-muted">
              Not one of these? <code className="font-mono text-[12px] text-phosphor">norte theme import</code> turns a Visual Studio Code colour theme into one of norte&apos;s, and a <code className="font-mono text-[12px] text-ink">.toml</code> of your own is loaded by name or by path. The window paints the same file colours, and can follow the desktop&apos;s light/dark preference.
            </p>
          </article>

          <div className="grid gap-4">
            <article className="rounded-2xl border border-line bg-surface/70 p-6 lg:p-8">
              <span className="font-mono text-[9px] uppercase tracking-[0.15em] text-cyan">Seven keymaps</span>
              <h3 className="mt-4 text-2xl font-medium tracking-[-0.04em]">Muscle memory, yours.</h3>
              <p className="mt-3 text-sm leading-6 text-muted">
                Four of them are transcriptions of the real managers, checked against their documentation—not chords we invented. Rebind any of the 180 commands; which-key shows you the way out.
              </p>
              <div className="mt-6 flex flex-wrap gap-2">
                {presets.map((preset) => (
                  <span key={preset} className="rounded-full border border-line px-3 py-2 font-mono text-[9px] text-muted">{preset}</span>
                ))}
              </div>
              <div className="mt-6 flex items-center gap-2 border-t border-line pt-5">
                {["SPACE", "C", "E"].map((key) => (
                  <kbd key={key} className="rounded-md border border-white/[0.13] bg-white/[0.04] px-3 py-2 font-mono text-[9px] text-ink shadow-[0_2px_0_#272e2b]">{key}</kbd>
                ))}
                <span className="ml-auto font-mono text-[9px] text-muted">compare · vim preset</span>
              </div>
            </article>

            <article className="grid gap-px overflow-hidden rounded-2xl border border-line bg-line sm:grid-cols-2">
              {[
                ["5 layouts", "orthodox, explorer, krusader, full, simple"],
                ["2 languages", "English and Spanish, keys named in yours"],
                ["File icons", "Nerd Font or Seti, as a column left of the name"],
                ["Profiles", "A fourth configuration layer, by name"],
              ].map(([title, body]) => (
                <div key={title} className="bg-base p-5">
                  <h4 className="text-[15px] font-medium tracking-[-0.02em] text-ink">{title}</h4>
                  <p className="mt-2 text-[13px] leading-5 text-muted">{body}</p>
                </div>
              ))}
            </article>
          </div>
        </div>
      </div>
    </section>
  );
}
