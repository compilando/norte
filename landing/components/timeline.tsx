"use client";

import { useState } from "react";

type Entry = {
  time: string;
  title: string;
  detail: string;
  by: "you" | "agent" | "plugin";
  ops: number;
  reversible: boolean;
};

const entries: Entry[] = [
  { time: "14:02", title: "Rename · 3 files", detail: "~/projects/northstar", by: "you", ops: 3, reversible: true },
  { time: "13:58", title: "Copy · 812 files", detail: "local → s3://norte-releases", by: "you", ops: 812, reversible: true },
  { time: "13:41", title: "Organize · 14 ops", detail: "dist/** into dated folders", by: "agent", ops: 14, reversible: true },
  { time: "13:33", title: "Delete → trash · 6 files", detail: "stale archives", by: "plugin", ops: 6, reversible: true },
  { time: "13:12", title: "Checksums · 402 files", detail: "sha256, read only", by: "you", ops: 0, reversible: false },
  { time: "12:47", title: "Sync · 2,140 entries", detail: "work → backup", by: "you", ops: 2140, reversible: true },
];

const badge = {
  you: { short: "you", long: "you", className: "border-line text-muted" },
  agent: { short: "agent", long: "agent · codex", className: "border-amber/35 text-amber" },
  plugin: { short: "plugin", long: "plugin · cleanup", className: "border-cyan/30 text-cyan" },
} as const;

export function Timeline() {
  const [selected, setSelected] = useState(2);

  const affected = entries.slice(0, selected + 1);
  const undone = affected.filter((entry) => entry.reversible).reduce((total, entry) => total + entry.ops, 0);
  const skipped = affected.filter((entry) => !entry.reversible).length;
  const notYours = affected.filter((entry) => entry.by !== "you").length;

  return (
    <section className="relative overflow-hidden border-y border-line bg-[#090c0d] py-24 sm:py-32 lg:py-44">
      <div className="page-grid pointer-events-none absolute inset-0 opacity-30 [mask-image:radial-gradient(circle_at_22%_50%,black,transparent_60%)]" />
      <div className="relative mx-auto grid max-w-[1440px] gap-14 px-5 sm:px-8 lg:grid-cols-[1.05fr_.95fr] lg:items-center lg:gap-20 lg:px-12">
        <div>
          <div className="inline-flex items-center gap-2 rounded-full border border-amber/25 bg-amber/[0.05] px-3 py-1.5 font-mono text-[9px] uppercase tracking-[0.15em] text-amber">
            <span className="h-1.5 w-1.5 rounded-full bg-amber" /> Journal timeline
          </div>
          <h2 className="mt-7 text-balance text-5xl font-medium leading-[.98] tracking-[-0.06em] text-ink sm:text-6xl lg:text-7xl">
            Rewind to <span className="text-amber">any point</span> of your afternoon.
          </h2>
          <p className="mt-7 max-w-xl text-base leading-7 text-muted">
            Every mutation goes through the journal—yours, a plugin&apos;s, an agent&apos;s—and the timeline lists them newest first, a batch as one row. Pick a row and Norte asks before undoing back to it, saying <b className="text-ink">how many entries it will undo</b>, how many it will skip, and how many it will leave alone because they are not yours.
          </p>
          <p className="mt-6 max-w-xl text-[13px] leading-5 text-muted/75">
            The dialog cannot over-promise: the undo carries a ceiling—the newest entry the list actually counted—so work that landed while you were reading is never swept up by a number you saw before it existed.
          </p>
          <div className="mt-9 flex flex-wrap gap-2 font-mono text-[9px] uppercase tracking-[0.1em] text-muted">
            {["hash-chained", "attributed", "batch aware", "session undo", "both frontends"].map((chip) => (
              <span key={chip} className="rounded-full border border-line px-3 py-2">{chip}</span>
            ))}
          </div>
        </div>

        <div className="relative">
          <div className="absolute -inset-10 -z-10 bg-[radial-gradient(circle,rgba(255,196,107,.08),transparent_66%)] blur-2xl" />
          <div className="overflow-hidden rounded-2xl border border-white/[0.11] bg-[#0d1112] shadow-terminal">
            <div className="flex items-center gap-2 border-b border-line px-5 py-4 font-mono text-[9px] uppercase tracking-[0.13em]">
              <span className="text-ink">Journal</span>
              <span className="text-muted">/ this machine</span>
              <span className="ml-auto text-amber">pick a row</span>
            </div>

            <div className="p-2 sm:p-3">
              {entries.map((entry, index) => {
                const inRange = index <= selected;
                const isCut = index === selected;
                return (
                  <button
                    key={entry.time}
                    type="button"
                    onClick={() => setSelected(index)}
                    aria-pressed={isCut}
                    className={`grid w-full grid-cols-[44px_1fr_auto] items-center gap-3 border-l-2 px-3 py-3 text-left transition-colors ${
                      isCut
                        ? "border-amber bg-amber/[0.09]"
                        : inRange
                          ? "border-amber/35 bg-amber/[0.03] hover:bg-amber/[0.06]"
                          : "border-line hover:bg-white/[0.02]"
                    }`}
                  >
                    <span className={`font-mono text-[9px] ${inRange ? "text-amber/80" : "text-muted/60"}`}>{entry.time}</span>
                    <span className="min-w-0">
                      <span className={`block truncate text-[11px] ${inRange ? "text-ink" : "text-muted"}`}>{entry.title}</span>
                      <span className="mt-0.5 block truncate font-mono text-[8px] text-muted/60">{entry.detail}</span>
                    </span>
                    <span className={`shrink-0 rounded-full border px-2 py-1 font-mono text-[7px] uppercase tracking-[0.1em] ${badge[entry.by].className}`}>
                      <span className="sm:hidden">{badge[entry.by].short}</span>
                      <span className="hidden sm:inline">{badge[entry.by].long}</span>
                    </span>
                  </button>
                );
              })}
            </div>

            <div className="border-t border-line bg-black/30 px-5 py-4">
              <p className="font-mono text-[9px] uppercase tracking-[0.12em] text-muted">Undo back to {entries[selected].time}</p>
              <div className="mt-3 grid grid-cols-3 gap-3">
                <div>
                  <strong className="block text-2xl font-medium tracking-[-0.05em] text-amber">{undone.toLocaleString("en-US")}</strong>
                  <span className="font-mono text-[8px] uppercase tracking-[0.1em] text-muted">operations undone</span>
                </div>
                <div>
                  <strong className="block text-2xl font-medium tracking-[-0.05em] text-ink">{skipped}</strong>
                  <span className="font-mono text-[8px] uppercase tracking-[0.1em] text-muted">skipped · read only</span>
                </div>
                <div>
                  <strong className="block text-2xl font-medium tracking-[-0.05em] text-cyan">{notYours}</strong>
                  <span className="font-mono text-[8px] uppercase tracking-[0.1em] text-muted">not yours</span>
                </div>
              </div>
              <p className="mt-4 border-t border-line pt-3 font-mono text-[8px] uppercase tracking-[0.1em] text-muted">
                Enter asks first · nothing moves until you say so
              </p>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
