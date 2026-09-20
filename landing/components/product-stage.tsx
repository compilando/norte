"use client";

import { AnimatePresence, motion, useReducedMotion } from "framer-motion";
import { useEffect, useState } from "react";
import { Brand } from "./brand";

const modes = [
  { id: "browse", label: "Two panes", shortcut: "01" },
  { id: "goto", label: "Go anywhere", shortcut: "02" },
  { id: "ai", label: "AI plan", shortcut: "03" },
  { id: "tasks", label: "Async tasks", shortcut: "04" },
  { id: "journal", label: "Timeline", shortcut: "05" },
  { id: "agent", label: "Agent control", shortcut: "06" },
] as const;

type Mode = (typeof modes)[number]["id"];
type Surface = "terminal" | "window";
type Entry = { name: string; meta: string; mode?: string; kind?: "dir" | "file" | "image" | "archive" };

const localFiles: Entry[] = [
  { name: "design-system", meta: "—", mode: "drwxr-xr-x", kind: "dir" },
  { name: "release-candidates", meta: "—", mode: "drwxr-xr-x", kind: "dir" },
  { name: "DSC_2048.jpg", meta: "8.4 MB", mode: "-rw-r--r--", kind: "image" },
  { name: "invoice-final-3.pdf", meta: "940 KB", mode: "-rw-r--r--", kind: "file" },
  { name: "launch-assets.zip", meta: "1.2 GB", mode: "-rw-r--r--", kind: "archive" },
  { name: "notes-from-call.md", meta: "12 KB", mode: "-rw-r--r--", kind: "file" },
];

const remoteFiles: Entry[] = [
  { name: "2026", meta: "—", kind: "dir" },
  { name: "campaign", meta: "—", kind: "dir" },
  { name: "hero-master.webp", meta: "2.1 MB", kind: "image" },
  { name: "norte-linux-x64", meta: "18 MB", kind: "file" },
  { name: "release-notes.md", meta: "6 KB", kind: "file" },
  { name: "source.tar.gz", meta: "4.7 MB", kind: "archive" },
];

const menuGroups = ["File", "Operate", "Mark", "Go", "Panels", "Tabs", "Find", "View", "Tools", "Help"];

const glyph = (kind: Entry["kind"]) =>
  kind === "dir" ? "▸" : kind === "image" ? "◫" : kind === "archive" ? "◇" : "·";

function FilePane({
  title,
  path,
  files,
  active = false,
  stripes = false,
  showMode = false,
}: {
  title: string;
  path: string;
  files: Entry[];
  active?: boolean;
  stripes?: boolean;
  showMode?: boolean;
}) {
  const columns = showMode ? "grid-cols-[1fr_82px_64px]" : "grid-cols-[1fr_64px]";
  return (
    <div className={`min-w-0 flex-1 ${active ? "bg-[#0c1011]" : "bg-[#090c0d]"}`}>
      <div className={`flex h-11 items-center gap-2 border-b border-line px-3 text-[10px] sm:px-4 ${active ? "text-ink" : "text-muted"}`}>
        <span className={`h-1.5 w-1.5 rounded-full ${active ? "bg-phosphor shadow-[0_0_8px_#B7FF52]" : "bg-muted/40"}`} />
        <span className="uppercase tracking-[0.14em]">{title}</span>
        <span className="ml-auto hidden truncate text-muted/70 sm:block">{path}</span>
      </div>
      <div className={`grid h-8 ${columns} items-center border-b border-line/80 px-3 text-[9px] uppercase tracking-[0.12em] text-muted/55 sm:px-4`}>
        <span>Name</span>
        {showMode && <span className="hidden sm:block">Perms</span>}
        <span className="text-right">Size</span>
      </div>
      <div className="py-1.5">
        {files.map((file, index) => (
          <div
            key={file.name}
            className={`grid h-9 ${columns} items-center px-3 text-[10px] sm:px-4 sm:text-[11px] ${
              active && index === 2
                ? "bg-phosphor/[0.11] text-ink"
                : stripes && index % 2 === 1
                  ? "bg-white/[0.022] text-muted"
                  : "text-muted"
            }`}
          >
            <span className="flex min-w-0 items-center gap-2 truncate">
              <span className={file.kind === "dir" ? "text-phosphor" : file.kind === "image" ? "text-cyan" : "text-muted/55"}>{glyph(file.kind)}</span>
              <span className="truncate">{file.name}</span>
            </span>
            {showMode && <span className="hidden font-mono text-[9px] text-muted/45 sm:block">{file.mode ?? "—"}</span>}
            <span className="text-right text-muted/55">{file.meta}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

const overlay = "absolute z-20 overflow-hidden rounded-xl border bg-[#0d1112]/95 shadow-[0_24px_90px_rgba(0,0,0,.75)] backdrop-blur-xl";

function GotoPalette() {
  const sections: [string, [string, string][]][] = [
    ["This panel's history", [["~/projects/northstar/dist", "2 min ago"]]],
    ["Places you return to", [["s3://norte-releases/2026", "41 visits"]]],
    ["Connections", [["sftp://build@atlas.internal", "connected"]]],
    ["Commands", [["Compare directories", "shift+F2"]]],
    ["Semantic index", [["docs/architecture/agent-policy.md", "0.96"]]],
  ];
  return (
    <motion.div
      initial={{ opacity: 0, y: 10, scale: 0.98 }}
      animate={{ opacity: 1, y: 0, scale: 1 }}
      exit={{ opacity: 0, scale: 0.98 }}
      className={`${overlay} inset-x-3 top-5 mx-auto max-w-xl border-white/[0.14] sm:inset-x-8 sm:top-7`}
    >
      <div className="flex items-center gap-2 border-b border-line px-4 py-3">
        <span className="font-mono text-[11px] text-phosphor">⌕</span>
        <span className="font-mono text-[11px] text-ink">north<span className="caret ml-px inline-block h-3 w-1 bg-phosphor align-middle" /></span>
        <span className="ml-auto font-mono text-[8px] uppercase tracking-[0.12em] text-muted">ctrl+g</span>
      </div>
      <div className="max-h-[248px] overflow-hidden py-1">
        {sections.map(([title, rows], group) => (
          <div key={title}>
            <p className="px-4 pb-1 pt-2.5 font-mono text-[8px] uppercase tracking-[0.13em] text-muted/60">{title}</p>
            {rows.map(([label, meta]) => (
              <div
                key={label}
                className={`flex items-center gap-3 px-4 py-2 font-mono text-[9px] sm:text-[10px] ${group === 0 ? "bg-phosphor/[0.1] text-ink" : "text-muted"}`}
              >
                <span className={group === 0 ? "text-phosphor" : "text-muted/45"}>→</span>
                <span className="min-w-0 flex-1 truncate">{label}</span>
                <span className="shrink-0 text-muted/55">{meta}</span>
              </div>
            ))}
          </div>
        ))}
      </div>
      <div className="flex items-center justify-between border-t border-line bg-white/[0.018] px-4 py-2.5 font-mono text-[8px] uppercase tracking-[0.1em] text-muted">
        <span>Seven sources, one list</span>
        <span className="text-phosphor">enter goes there</span>
      </div>
    </motion.div>
  );
}

function AiPlan() {
  const rows = [
    ["DSC_2048.jpg", "2026-08-berlin-keynote-01.jpg"],
    ["invoice-final-3.pdf", "2026-07-acme-invoice.pdf"],
    ["notes-from-call.md", "northstar-kickoff-notes.md"],
  ];
  return (
    <motion.div
      initial={{ opacity: 0, y: 12, scale: 0.98 }}
      animate={{ opacity: 1, y: 0, scale: 1 }}
      exit={{ opacity: 0, scale: 0.98 }}
      className={`${overlay} inset-x-3 top-5 mx-auto max-w-xl border-phosphor/25 sm:inset-x-8 sm:top-8`}
    >
      <div className="flex items-center border-b border-line px-4 py-3">
        <span className="mr-2 grid h-6 w-6 place-items-center rounded-full bg-phosphor/10 text-phosphor">✦</span>
        <div>
          <p className="text-[11px] font-medium text-ink">Rename with AI</p>
          <p className="text-[9px] text-muted">“Use dates and meaningful project names”</p>
        </div>
        <span className="ml-auto rounded-full border border-phosphor/20 bg-phosphor/[0.06] px-2 py-1 text-[8px] uppercase tracking-wider text-phosphor">Local model</span>
      </div>
      <div className="p-3 sm:p-4">
        <div className="mb-2 flex items-center justify-between text-[8px] uppercase tracking-[0.12em] text-muted">
          <span>Review plan</span>
          <span>3 changes · 0 applied</span>
        </div>
        {rows.map(([from, to], index) => (
          <motion.div
            key={from}
            initial={{ opacity: 0, x: -8 }}
            animate={{ opacity: 1, x: 0 }}
            transition={{ delay: index * 0.09 }}
            className="grid gap-0.5 border-t border-line/80 py-2.5 text-[9px] sm:grid-cols-[1fr_18px_1.25fr] sm:items-center sm:text-[10px]"
          >
            <span className="truncate text-muted">{from}</span>
            <span className="hidden text-phosphor/60 sm:block">→</span>
            <span className="truncate text-ink">{to}</span>
          </motion.div>
        ))}
      </div>
      <div className="flex items-center justify-between border-t border-line bg-white/[0.018] px-4 py-3">
        <span className="text-[9px] text-muted">Nothing changes until you approve.</span>
        <span className="rounded-md bg-phosphor px-3 py-1.5 text-[9px] font-semibold text-[#0a1008]">Apply plan</span>
      </div>
    </motion.div>
  );
}

function TaskPanel() {
  const tasks = [
    { name: "Copy to s3://northstar", detail: "6.2 GB / 8.9 GB", progress: 72, color: "bg-phosphor" },
    { name: "Compare releases", detail: "18,402 entries", progress: 88, color: "bg-cyan" },
    { name: "Embed project index", detail: "2,011 / 4,830 files", progress: 42, color: "bg-white/50" },
  ];
  return (
    <motion.div
      initial={{ opacity: 0, x: 30 }}
      animate={{ opacity: 1, x: 0 }}
      exit={{ opacity: 0, x: 30 }}
      className={`${overlay} bottom-3 right-3 top-3 w-[min(390px,calc(100%-24px))] border-line p-4 sm:bottom-5 sm:right-5 sm:top-5 sm:p-5`}
    >
      <div className="flex items-start justify-between">
        <div>
          <p className="text-[9px] uppercase tracking-[0.15em] text-phosphor">Task engine</p>
          <h3 className="mt-1 text-sm font-medium text-ink">Work keeps moving.</h3>
        </div>
        <span className="rounded border border-line px-2 py-1 text-[8px] text-muted">ctrl+T</span>
      </div>
      <div className="mt-5 space-y-4">
        {tasks.map((task, index) => (
          <div key={task.name}>
            <div className="flex justify-between gap-3 text-[10px]">
              <span className="truncate text-ink">{task.name}</span>
              <span className="shrink-0 text-muted">{task.progress}%</span>
            </div>
            <div className="mt-1.5 flex justify-between text-[8px] text-muted/65">
              <span>{task.detail}</span>
              <span>{index === 0 ? "running" : index === 1 ? "verifying" : "queued"}</span>
            </div>
            <div className="mt-2 h-1 overflow-hidden rounded-full bg-white/[0.06]">
              <motion.div
                initial={{ width: 0 }}
                animate={{ width: `${task.progress}%` }}
                transition={{ duration: 0.8, delay: index * 0.12 }}
                className={`h-full rounded-full ${task.color}`}
              />
            </div>
          </div>
        ))}
      </div>
      <div className="absolute bottom-4 left-4 right-4 flex items-center justify-between border-t border-line pt-3 text-[8px] text-muted sm:bottom-5 sm:left-5 sm:right-5">
        <span>3 active · 12 complete</span>
        <span className="text-phosphor">cancel / reprioritize</span>
      </div>
    </motion.div>
  );
}

function JournalPanel() {
  const rows = [
    { when: "14:02", what: "Rename · 3 files", who: "you", tone: "text-ink" },
    { when: "13:58", what: "Copy · 812 files → s3://", who: "you", tone: "text-ink" },
    { when: "13:41", what: "Organize · 14 ops", who: "agent · codex", tone: "text-amber" },
    { when: "13:12", what: "Delete → trash · 6 files", who: "plugin · cleanup", tone: "text-cyan" },
  ];
  return (
    <motion.div
      initial={{ opacity: 0, x: 30 }}
      animate={{ opacity: 1, x: 0 }}
      exit={{ opacity: 0, x: 30 }}
      className={`${overlay} bottom-3 right-3 top-3 w-[min(390px,calc(100%-24px))] border-line p-4 sm:bottom-5 sm:right-5 sm:top-5 sm:p-5`}
    >
      <div className="flex items-start justify-between">
        <div>
          <p className="text-[9px] uppercase tracking-[0.15em] text-amber">Journal timeline</p>
          <h3 className="mt-1 text-sm font-medium text-ink">Undo back to here.</h3>
        </div>
        <span className="rounded border border-line px-2 py-1 text-[8px] text-muted">↩</span>
      </div>
      <div className="mt-5">
        {rows.map((row, index) => (
          <div
            key={row.when}
            className={`grid grid-cols-[38px_1fr] gap-3 border-l py-3 pl-3 ${index === 2 ? "border-amber bg-amber/[0.06]" : "border-line"}`}
          >
            <span className="font-mono text-[9px] text-muted/70">{row.when}</span>
            <div>
              <p className="text-[10px] text-ink">{row.what}</p>
              <p className={`mt-1 font-mono text-[8px] ${row.tone}`}>{row.who}</p>
            </div>
          </div>
        ))}
      </div>
      <div className="absolute bottom-4 left-4 right-4 flex items-center justify-between border-t border-line pt-3 text-[8px] text-muted sm:bottom-5 sm:left-5 sm:right-5">
        <span>14 entries will be undone</span>
        <span className="text-amber">2 skipped · not yours</span>
      </div>
    </motion.div>
  );
}

function AgentApproval() {
  return (
    <motion.div
      initial={{ opacity: 0, y: 10, scale: 0.98 }}
      animate={{ opacity: 1, y: 0, scale: 1 }}
      exit={{ opacity: 0 }}
      className={`${overlay} inset-x-3 top-6 mx-auto max-w-lg border-[#f1f5ef]/15 sm:inset-x-8 sm:top-10`}
    >
      <div className="flex items-center gap-3 border-b border-line px-4 py-3">
        <span className="compass-pulse h-2 w-2 rounded-full bg-phosphor" />
        <div>
          <p className="text-[10px] text-ink">Agent requests access</p>
          <p className="text-[8px] text-muted">codex · session 7F2A · via MCP</p>
        </div>
        <span className="ml-auto text-[8px] uppercase tracking-wider text-muted">Awaiting you</span>
      </div>
      <div className="p-4">
        <p className="text-[10px] leading-5 text-muted">Wants to organize build artifacts and remove stale archives.</p>
        <div className="mt-3 rounded-lg border border-line bg-black/20 p-3 text-[9px] leading-5">
          <p><span className="inline-block w-16 text-muted">Scope</span><span className="text-ink">~/projects/northstar/dist/**</span></p>
          <p><span className="inline-block w-16 text-muted">Allowed</span><span className="text-ink">read, copy, move</span></p>
          <p><span className="inline-block w-16 text-muted">Delete</span><span className="text-coral">deny</span></p>
          <p><span className="inline-block w-16 text-muted">Expires</span><span className="text-ink">in 20 minutes</span></p>
        </div>
      </div>
      <div className="flex items-center justify-end gap-2 border-t border-line px-4 py-3 text-[9px]">
        <span className="rounded-md border border-line px-3 py-1.5 text-muted">Deny</span>
        <span className="rounded-md bg-phosphor px-3 py-1.5 font-semibold text-[#0a1008]">Approve scope</span>
      </div>
    </motion.div>
  );
}

function SurfaceToggle({ surface, onChange }: { surface: Surface; onChange: (next: Surface) => void }) {
  return (
    <div className="flex items-center rounded-full border border-line bg-black/40 p-0.5" role="group" aria-label="Choose a surface">
      {(["terminal", "window"] as const).map((item) => (
        <button
          key={item}
          type="button"
          onClick={() => onChange(item)}
          aria-pressed={surface === item}
          className={`rounded-full px-2.5 py-1 font-mono text-[8px] uppercase tracking-[0.1em] transition sm:text-[9px] ${
            surface === item ? "bg-phosphor text-[#0a1008]" : "text-muted hover:text-ink"
          }`}
        >
          {item === "terminal" ? "ntc" : "norte-gui"}
        </button>
      ))}
    </div>
  );
}

export function ProductStage() {
  const [mode, setMode] = useState<Mode>("browse");
  const [surface, setSurface] = useState<Surface>("terminal");
  const [touched, setTouched] = useState(false);
  const reduceMotion = useReducedMotion();
  const isWindow = surface === "window";

  useEffect(() => {
    if (reduceMotion || touched) return;
    const timer = window.setInterval(() => {
      setMode((current) => modes[(modes.findIndex((item) => item.id === current) + 1) % modes.length].id);
    }, 4600);
    return () => window.clearInterval(timer);
  }, [reduceMotion, touched]);

  const pick = (next: Mode) => {
    setTouched(true);
    setMode(next);
  };

  return (
    <div className="relative mx-auto max-w-[1180px]" aria-label="Interactive Norte product demonstration">
      <div className="absolute -inset-16 -z-10 bg-[radial-gradient(ellipse_at_center,rgba(183,255,82,.12),transparent_62%)] blur-2xl" />
      <div className={`overflow-hidden border border-white/[0.12] bg-[#090c0d] shadow-terminal ${isWindow ? "rounded-xl" : "rounded-2xl"}`}>
        <div className="flex h-12 items-center border-b border-line bg-[#111517] px-3 sm:px-4">
          {isWindow ? (
            <span className="mr-3 hidden items-center gap-1.5 sm:flex">
              {["#ff6159", "#ffbd2e", "#28c93f"].map((dot) => (
                <span key={dot} className="h-2.5 w-2.5 rounded-full opacity-70" style={{ background: dot }} />
              ))}
            </span>
          ) : (
            <Brand compact />
          )}
          <span className="ml-1 hidden font-mono text-[10px] text-muted sm:inline">
            {isWindow ? "norte — northstar" : "ntc · northstar"}
          </span>
          <div className="ml-auto flex items-center gap-3">
            <SurfaceToggle surface={surface} onChange={setSurface} />
            <span className="hidden items-center gap-1.5 sm:flex">
              <span className="h-1.5 w-1.5 rounded-full bg-phosphor shadow-[0_0_8px_#B7FF52]" />
              <span className="font-mono text-[8px] uppercase tracking-[0.12em] text-muted sm:text-[9px]">core online</span>
            </span>
          </div>
        </div>

        {isWindow && (
          <div className="hidden h-8 items-center gap-4 overflow-hidden border-b border-line bg-[#0f1315] px-4 text-[10px] text-muted md:flex">
            {menuGroups.map((group) => (
              <span key={group} className={group === "Go" ? "text-ink" : ""}>{group}</span>
            ))}
            <span className="ml-auto font-mono text-[8px] uppercase tracking-[0.1em] text-muted/60">alt opens it</span>
          </div>
        )}

        <div className="grid border-b border-line bg-[#0c1011] sm:grid-cols-3 lg:grid-cols-6">
          {modes.map((item) => (
            <button
              key={item.id}
              type="button"
              onClick={() => pick(item.id)}
              className={`relative flex h-11 items-center justify-between border-line px-3 text-left font-mono text-[9px] transition-colors sm:border-r sm:px-4 ${
                mode === item.id ? "bg-white/[0.035] text-ink" : "hidden text-muted hover:text-ink sm:flex"
              }`}
              aria-pressed={mode === item.id}
            >
              <span className="truncate">{item.label}</span>
              <span className={mode === item.id ? "text-phosphor" : "text-muted/40"}>{item.shortcut}</span>
              {mode === item.id && <motion.span layoutId="active-mode" className="absolute inset-x-0 bottom-0 h-px bg-phosphor shadow-[0_0_8px_#B7FF52]" />}
            </button>
          ))}
        </div>

        <div className={`relative flex h-[355px] overflow-hidden sm:h-[390px] ${isWindow ? "" : "terminal-scan"}`}>
          <FilePane title="Local" path="~/projects/northstar" files={localFiles} active stripes={isWindow} showMode />
          <div className="hidden w-px bg-line md:block" />
          <div className="hidden flex-1 md:flex">
            <FilePane title="Object storage" path="s3://norte-releases" files={remoteFiles} stripes={isWindow} />
          </div>
          <AnimatePresence mode="wait">
            {mode === "goto" && <GotoPalette key="goto" />}
            {mode === "ai" && <AiPlan key="ai" />}
            {mode === "tasks" && <TaskPanel key="tasks" />}
            {mode === "journal" && <JournalPanel key="journal" />}
            {mode === "agent" && <AgentApproval key="agent" />}
          </AnimatePresence>
        </div>

        {isWindow ? (
          <div className="flex h-10 items-center gap-2 overflow-hidden border-t border-line bg-[#111517] px-3 text-[9px] text-muted sm:px-4">
            {["Places", "Tree", "Viewer", "Tasks", "Timeline", "Log"].map((panel) => (
              <span
                key={panel}
                className={`rounded-md border px-2 py-1 ${panel === "Timeline" ? "border-phosphor/35 bg-phosphor/[0.07] text-ink" : "border-line/80"}`}
              >
                {panel}
              </span>
            ))}
            <span className="ml-auto hidden font-mono text-[8px] uppercase tracking-[0.09em] text-phosphor sm:inline">journal clean · undo ready</span>
          </div>
        ) : (
          <div className="flex h-10 items-center gap-4 overflow-hidden border-t border-line bg-[#111517] px-3 font-mono text-[8px] uppercase tracking-[0.09em] text-muted sm:px-4 sm:text-[9px]">
            <span><b className="mr-1 rounded bg-white/[0.08] px-1.5 py-1 font-normal text-ink">F5</b> copy</span>
            <span className="hidden sm:inline"><b className="mr-1 rounded bg-white/[0.08] px-1.5 py-1 font-normal text-ink">F6</b> move</span>
            <span className="hidden md:inline"><b className="mr-1 rounded bg-white/[0.08] px-1.5 py-1 font-normal text-ink">F9</b> menu</span>
            <span><b className="mr-1 rounded bg-white/[0.08] px-1.5 py-1 font-normal text-ink">ctrl+G</b> go</span>
            <span className="ml-auto text-phosphor">journal clean · undo ready</span>
          </div>
        )}
      </div>
    </div>
  );
}
