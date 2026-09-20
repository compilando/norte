"use client";

import { motion, useReducedMotion } from "framer-motion";

const shared = [
  ["One command vocabulary", "180 named commands. The palette, the menus, the help and every keymap read the same table."],
  ["One configuration", "One norte.toml: your themes, layouts and profiles. Change it once, both surfaces obey."],
  ["One journal", "What the window did, the terminal can undo. Attribution travels with the entry."],
  ["One policy", "Agents, plugins and both frontends cross the same gate in the core."],
];

function MiniPane({ label, marked }: { label: string; marked: number[] }) {
  const rows = ["design-system", "DSC_2048.jpg", "invoice-final-3.pdf", "launch-assets.zip", "notes-from-call.md"];
  return (
    <div className="space-y-px">
      <p className="pb-1.5 font-mono text-[8px] uppercase tracking-[0.13em] text-muted/60">{label}</p>
      {rows.map((row, index) => (
        <div
          key={row}
          className={`flex items-center gap-2 rounded px-2 py-1.5 font-mono text-[9px] ${
            marked.includes(index) ? "bg-phosphor/[0.13] text-phosphor" : "text-muted/70"
          }`}
        >
          <span className="w-2">{marked.includes(index) ? "▪" : ""}</span>
          <span className="truncate">{row}</span>
        </div>
      ))}
    </div>
  );
}

function SurfaceCard({
  kind,
  title,
  command,
  copy,
  points,
}: {
  kind: "terminal" | "window";
  title: string;
  command: string;
  copy: string;
  points: string[];
}) {
  const isWindow = kind === "window";
  return (
    <article className={`relative overflow-hidden rounded-2xl border p-6 lg:p-8 ${isWindow ? "border-cyan/25 bg-[#0b1214]" : "border-phosphor/25 bg-[#0b100c]"}`}>
      <div className={`absolute -right-16 -top-16 h-48 w-48 rounded-full blur-3xl ${isWindow ? "bg-cyan/[0.09]" : "bg-phosphor/[0.09]"}`} />
      <div className="relative">
        <div className="flex items-center justify-between">
          <span className={`font-mono text-[9px] uppercase tracking-[0.15em] ${isWindow ? "text-cyan" : "text-phosphor"}`}>
            {isWindow ? "Desktop window" : "Terminal"}
          </span>
          <span className="rounded-full border border-line px-2.5 py-1 font-mono text-[8px] uppercase tracking-[0.1em] text-muted">supported</span>
        </div>
        <h3 className="mt-5 text-3xl font-medium tracking-[-0.045em] text-ink">{title}</h3>
        <code className={`mt-3 block font-mono text-[10px] ${isWindow ? "text-cyan" : "text-phosphor"}`}>$ {command}</code>
        <p className="mt-5 max-w-md text-sm leading-6 text-muted">{copy}</p>
        <ul className="mt-6 space-y-2">
          {points.map((point) => (
            <li key={point} className="flex gap-2.5 text-[13px] leading-5 text-muted">
              <span className={isWindow ? "text-cyan" : "text-phosphor"}>·</span>
              <span>{point}</span>
            </li>
          ))}
        </ul>
      </div>
    </article>
  );
}

export function Parity() {
  const reduceMotion = useReducedMotion();
  return (
    <section id="surfaces" className="relative scroll-mt-16 overflow-hidden border-y border-line bg-[#080b0c] py-24 sm:py-32 lg:py-44">
      <div className="page-grid pointer-events-none absolute inset-0 opacity-30 [mask-image:radial-gradient(circle_at_50%_18%,black,transparent_62%)]" />
      <div className="relative mx-auto max-w-[1440px] px-5 sm:px-8 lg:px-12">
        <div className="max-w-4xl">
          <p className="font-mono text-[10px] uppercase tracking-[0.17em] text-phosphor">New in this cycle</p>
          <h2 className="mt-6 text-balance text-5xl font-medium leading-[.96] tracking-[-0.065em] text-ink sm:text-7xl lg:text-[86px]">
            Start in the terminal. <span className="text-muted">Finish in the window.</span>
          </h2>
          <p className="mt-7 max-w-2xl text-base leading-7 text-muted">
            Norte is not a TUI with a graphical port bolted on. Both frontends are clients of the same core over the same versioned protocol, and either one can hand the live screen to the other.
          </p>
        </div>

        <div className="mt-16 grid gap-4 lg:grid-cols-2">
          <SurfaceCard
            kind="terminal"
            title="ntc"
            command="ntc ~/projects"
            copy="Two panes, no mouse required, and the deepest surface in the product. It runs the core embedded, so there is nothing to start first."
            points={[
              "Images render as images, not as a hex dump",
              "Mouse, drag-to-resize columns and a which-key hint bar",
              "F9 menus, a command palette and a reference sheet",
            ]}
          />
          <SurfaceCard
            kind="window"
            title="norte-gui"
            command="norte-gui ~/projects"
            copy="A native window over the same semantics, packaged as a .deb, .rpm or AppImage that carries the daemon with it. Themes, keys and layout are the ones you already configured."
            points={[
              "Places sidebar, tree, docked viewer, tasks, log and timeline",
              "Ten menu groups, opened with Alt, and real focusable buttons",
              "Starts its own daemon, so a clean install has something to talk to",
            ]}
          />
        </div>

        <div className="mt-4 overflow-hidden rounded-2xl border border-line bg-surface/60">
          <div className="flex flex-wrap items-center gap-3 border-b border-line px-6 py-4 sm:px-8">
            <span className="font-mono text-[9px] uppercase tracking-[0.15em] text-amber">Handoff</span>
            <h3 className="text-lg font-medium tracking-[-0.03em] text-ink">Your screen follows you across.</h3>
            <code className="ml-auto rounded-md border border-line px-2.5 py-1 font-mono text-[9px] text-muted">app.handoff</code>
          </div>

          <div className="grid gap-8 p-6 sm:p-8 lg:grid-cols-[1fr_1.1fr] lg:gap-12">
            <div>
              <p className="max-w-lg text-sm leading-6 text-muted">
                What travels is what you were looking at—tabs, directories, cursor, history—and also what you had <b className="text-ink">marked</b>, which is the one part a <code className="font-mono text-[12px] text-phosphor">cd</code> cannot rebuild. Marks travel by path, never by index: a list that reordered itself would otherwise hand back a selection nobody made, under a cursor about to press delete.
              </p>
              <div className="mt-7 flex flex-wrap items-center gap-2 font-mono text-[9px] uppercase tracking-[0.1em]">
                {["write", "release", "launch"].map((step, index) => (
                  <span key={step} className="flex items-center gap-2">
                    <span className="rounded-full border border-line bg-black/30 px-3 py-2 text-muted">
                      <b className="mr-1.5 font-normal text-phosphor">{index + 1}</b>
                      {step}
                    </span>
                    {index < 2 && <span className="text-line">→</span>}
                  </span>
                ))}
              </div>
              <p className="mt-5 max-w-lg text-[13px] leading-5 text-muted/80">
                Each step gates the next. When any of them fails, nothing happens and the process stays exactly where it was—the cheapest failure available.
              </p>
            </div>

            <div className="relative grid grid-cols-[1fr_auto_1fr] items-center gap-3 rounded-xl border border-line bg-black/25 p-4 sm:gap-5 sm:p-6">
              <div className="rounded-lg border border-phosphor/20 bg-[#0b100c] p-3">
                <MiniPane label="ntc" marked={[1, 3]} />
              </div>
              <div className="relative h-px w-10 overflow-hidden bg-line sm:w-16">
                <motion.span
                  className={`absolute inset-y-0 left-0 w-1/3 bg-phosphor shadow-[0_0_10px_#B7FF52] ${reduceMotion ? "" : "handoff-beam"}`}
                  aria-hidden
                />
              </div>
              <div className="rounded-lg border border-cyan/20 bg-[#0b1214] p-3">
                <MiniPane label="norte-gui" marked={[1, 3]} />
              </div>
              <p className="col-span-3 pt-2 text-center font-mono text-[8px] uppercase tracking-[0.12em] text-muted">
                same cursor · same marks · same history
              </p>
            </div>
          </div>
        </div>

        <div className="mt-12 grid gap-px overflow-hidden rounded-2xl border border-line bg-line md:grid-cols-2 lg:grid-cols-4">
          {shared.map(([title, body]) => (
            <div key={title} className="bg-base p-6">
              <h4 className="text-[15px] font-medium tracking-[-0.02em] text-ink">{title}</h4>
              <p className="mt-2.5 text-[13px] leading-5 text-muted">{body}</p>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
