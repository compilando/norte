"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { useReducedMotion } from "framer-motion";

type Row = { label: string; meta: string; section: string; accent?: "phosphor" | "cyan" | "amber" };

const sections = [
  "A typed path",
  "This panel's history",
  "Places you return to",
  "Bookmarks",
  "Connections",
  "Commands",
  "Semantic index",
];

const rows: Row[] = [
  { label: "~/projects/northstar", meta: "typed", section: "A typed path", accent: "phosphor" },
  { label: "s3://norte-releases/2026", meta: "typed", section: "A typed path", accent: "phosphor" },
  { label: "~/projects/northstar/dist", meta: "2 min ago", section: "This panel's history" },
  { label: "~/downloads", meta: "11 min ago", section: "This panel's history" },
  { label: "~/projects/norte/crates", meta: "41 visits", section: "Places you return to" },
  { label: "/var/log", meta: "17 visits", section: "Places you return to" },
  { label: "Launch assets", meta: "~/work/launch", section: "Bookmarks", accent: "amber" },
  { label: "Nightly backups", meta: "sftp://atlas/backup", section: "Bookmarks", accent: "amber" },
  { label: "sftp://build@atlas.internal", meta: "connected", section: "Connections", accent: "cyan" },
  { label: "ftp://mirror.example.org", meta: "disconnected", section: "Connections", accent: "cyan" },
  { label: "Compare directories", meta: "shift+F2", section: "Commands" },
  { label: "Organize this directory", meta: "pane.organize", section: "Commands" },
  { label: "Hand off to the window", meta: "app.handoff", section: "Commands" },
  { label: "docs/architecture/agent-policy.md", meta: "0.96", section: "Semantic index", accent: "phosphor" },
  { label: "notes/security-review.md", meta: "0.89", section: "Semantic index", accent: "phosphor" },
];

const demoQueries = ["north", "agent", "atlas", "organ"];

/** Subsequence match, which is what the real screen does. */
function matches(haystack: string, needle: string) {
  if (!needle) return true;
  const target = haystack.toLowerCase();
  let cursor = 0;
  for (const char of needle.toLowerCase()) {
    cursor = target.indexOf(char, cursor);
    if (cursor === -1) return false;
    cursor += 1;
  }
  return true;
}

const accentClass = { phosphor: "text-phosphor", cyan: "text-cyan", amber: "text-amber" } as const;

export function Goto() {
  const [query, setQuery] = useState("");
  const [typed, setTyped] = useState(false);
  const reduceMotion = useReducedMotion();
  const timers = useRef<number[]>([]);

  useEffect(() => {
    if (typed || reduceMotion) return;
    let cancelled = false;
    let round = 0;

    const clearTimers = () => {
      timers.current.forEach(window.clearTimeout);
      timers.current = [];
    };

    const run = () => {
      if (cancelled) return;
      const word = demoQueries[round % demoQueries.length];
      round += 1;
      word.split("").forEach((_, index) => {
        timers.current.push(window.setTimeout(() => !cancelled && setQuery(word.slice(0, index + 1)), 110 * (index + 1)));
      });
      timers.current.push(window.setTimeout(() => !cancelled && setQuery(""), 110 * word.length + 2200));
      timers.current.push(window.setTimeout(run, 110 * word.length + 2900));
    };

    run();
    return () => {
      cancelled = true;
      clearTimers();
    };
  }, [typed, reduceMotion]);

  const visible = useMemo(() => rows.filter((row) => matches(`${row.label} ${row.section}`, query)), [query]);
  const grouped = useMemo(
    () => sections.map((section) => [section, visible.filter((row) => row.section === section)] as const).filter(([, list]) => list.length > 0),
    [visible],
  );

  return (
    <section className="relative py-28 sm:py-36 lg:py-44">
      <div className="mx-auto max-w-[1440px] px-5 sm:px-8 lg:px-12">
        <div className="grid gap-12 lg:grid-cols-[.92fr_1.08fr] lg:items-center lg:gap-20">
          <div>
            <p className="font-mono text-[10px] uppercase tracking-[0.17em] text-phosphor">One screen · seven sources</p>
            <h2 className="mt-6 text-balance text-5xl font-medium leading-[.98] tracking-[-0.06em] text-ink sm:text-6xl lg:text-7xl">
              Stop remembering <span className="text-muted">where it was.</span>
            </h2>
            <p className="mt-7 max-w-lg text-base leading-7 text-muted">
              One key opens one list: the path you are typing, this panel&apos;s history, the places you return to, your bookmarks, your connections, every command, and whatever the semantic index finds. Typing filters by subsequence. Enter either navigates, or runs the command through exactly the dispatch its key would.
            </p>
            <div className="mt-8 flex flex-wrap items-center gap-3">
              <kbd className="rounded-md border border-white/[0.14] bg-white/[0.04] px-3 py-2 font-mono text-[11px] text-ink shadow-[0_2px_0_#272e2b]">ctrl+G</kbd>
              <span className="font-mono text-[9px] uppercase tracking-[0.11em] text-muted">and first in the Go menu, in every preset</span>
            </div>
            <p className="mt-7 max-w-lg text-[13px] leading-5 text-muted/75">
              It replaces none of the six screens it draws from—each keeps its own key and the things only it can do. It is the one for when you cannot remember which of them held the answer.
            </p>
          </div>

          <div className="relative">
            <div className="absolute -inset-10 -z-10 bg-[radial-gradient(circle,rgba(183,255,82,.09),transparent_66%)] blur-2xl" />
            <div className="overflow-hidden rounded-2xl border border-white/[0.12] bg-[#0b0f10] shadow-terminal">
              <label className="flex items-center gap-3 border-b border-line px-4 py-4 sm:px-5">
                <span className="font-mono text-[13px] text-phosphor" aria-hidden>⌕</span>
                <input
                  value={query}
                  onChange={(event) => {
                    setTyped(true);
                    setQuery(event.target.value);
                  }}
                  onFocus={() => setTyped(true)}
                  placeholder="Type a path, a name, a command…"
                  aria-label="Filter the go-anywhere list"
                  className="w-full bg-transparent font-mono text-[12px] text-ink outline-none placeholder:text-muted/50"
                />
                {!typed && <span className="caret h-4 w-1 shrink-0 bg-phosphor" aria-hidden />}
                <span className="ml-auto hidden shrink-0 font-mono text-[8px] uppercase tracking-[0.12em] text-muted sm:inline">
                  {visible.length} of {rows.length}
                </span>
              </label>

              <div className="mask-fade-y h-[360px] overflow-y-auto py-1">
                {grouped.length === 0 ? (
                  <p className="px-5 py-10 text-center font-mono text-[10px] text-muted">Nothing matches. The real screen says so too.</p>
                ) : (
                  grouped.map(([section, list], groupIndex) => (
                    <div key={section}>
                      <p className="px-4 pb-1 pt-3 font-mono text-[8px] uppercase tracking-[0.14em] text-muted/55 sm:px-5">{section}</p>
                      {list.map((row, index) => {
                        const first = groupIndex === 0 && index === 0;
                        return (
                          <div
                            key={`${section}-${row.label}`}
                            className={`flex items-center gap-3 px-4 py-2.5 font-mono text-[10px] transition-colors sm:px-5 sm:text-[11px] ${
                              first ? "bg-phosphor/[0.1] text-ink" : "text-muted hover:bg-white/[0.02]"
                            }`}
                          >
                            <span className={first ? "text-phosphor" : accentClass[row.accent ?? "phosphor"] + " opacity-40"}>→</span>
                            <span className="min-w-0 flex-1 truncate">{row.label}</span>
                            <span className="shrink-0 text-muted/55">{row.meta}</span>
                          </div>
                        );
                      })}
                    </div>
                  ))
                )}
              </div>

              <div className="flex items-center justify-between border-t border-line bg-black/25 px-4 py-3 font-mono text-[8px] uppercase tracking-[0.1em] text-muted sm:px-5">
                <span>Arrows skip the section titles</span>
                <span className="text-phosphor">index asked from 3 characters</span>
              </div>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
