"use client";

import { AnimatePresence, motion, useReducedMotion } from "framer-motion";
import { useEffect, useMemo, useState } from "react";

type FileKind = "folder" | "rust" | "config" | "markdown" | "lock";

type FileEntry = {
  name: string;
  kind: FileKind;
  size: string;
  date: string;
};

const rootFiles: FileEntry[] = [
  { name: "src", kind: "folder", size: "—", date: "09:42" },
  { name: "tests", kind: "folder", size: "—", date: "09:38" },
  { name: "Cargo.toml", kind: "config", size: "2.4 KB", date: "09:31" },
  { name: "README.md", kind: "markdown", size: "6.8 KB", date: "Yesterday" },
  { name: "Cargo.lock", kind: "lock", size: "44 KB", date: "Yesterday" },
];

const srcFiles: FileEntry[] = [
  { name: "..", kind: "folder", size: "—", date: "" },
  { name: "lib.rs", kind: "rust", size: "8.1 KB", date: "09:44" },
  { name: "main.rs", kind: "rust", size: "3.7 KB", date: "09:40" },
  { name: "commands.rs", kind: "rust", size: "12 KB", date: "09:36" },
  { name: "config.rs", kind: "rust", size: "5.2 KB", date: "Yesterday" },
];

const destinationFiles: FileEntry[] = [
  { name: "releases", kind: "folder", size: "—", date: "09:10" },
  { name: "notes.md", kind: "markdown", size: "1.2 KB", date: "Yesterday" },
  { name: "archive", kind: "folder", size: "—", date: "Jul 08" },
  { name: ".gitignore", kind: "config", size: "112 B", date: "Jul 08" },
];

const glyphs: Record<FileKind, string> = {
  folder: "▸",
  rust: "◇",
  config: "≡",
  markdown: "#",
  lock: "·",
};

function FileRow({ file, selected = false, copied = false }: { file: FileEntry; selected?: boolean; copied?: boolean }) {
  return (
    <motion.div
      layout
      initial={copied ? { opacity: 0, x: -16, backgroundColor: "rgba(74,222,128,0.35)" } : false}
      animate={{ opacity: 1, x: 0, backgroundColor: copied ? ["rgba(74,222,128,0.35)", "rgba(74,222,128,0.06)"] : "rgba(0,0,0,0)" }}
      transition={{ duration: copied ? 0.55 : 0.18 }}
      className={`grid h-8 grid-cols-[minmax(0,1fr)_58px_62px] items-center px-2 text-[10px] sm:grid-cols-[minmax(0,1fr)_68px_72px] sm:text-[11px] ${selected ? "bg-phosphor/[0.12] text-ink" : "text-muted"}`}
    >
      <span className="flex min-w-0 items-center gap-2">
        <span className={file.kind === "folder" || selected ? "text-phosphor" : "text-muted/70"}>{glyphs[file.kind]}</span>
        <span className="truncate">{file.name}</span>
        {selected && <motion.span className="h-3.5 w-1.5 bg-phosphor" animate={{ opacity: [1, 1, 0, 0] }} transition={{ duration: 0.9, repeat: Infinity }} />}
      </span>
      <span className="text-right text-muted/70">{file.size}</span>
      <span className="text-right text-muted/50">{file.date}</span>
    </motion.div>
  );
}

function Pane({ path, files, activeIndex, copiedFile }: { path: string; files: FileEntry[]; activeIndex?: number; copiedFile?: FileEntry }) {
  return (
    <div className="min-w-0 flex-1 overflow-hidden bg-[#0e0e10]">
      <div className="flex h-10 items-center border-b border-line bg-surface px-3 text-[10px] sm:text-[11px]">
        <span className="mr-2 text-phosphor">~</span>
        <AnimatePresence mode="wait">
          <motion.span key={path} initial={{ opacity: 0, y: -4 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0 }} className="truncate text-ink/90">
            {path}
          </motion.span>
        </AnimatePresence>
      </div>
      <div className="grid h-7 grid-cols-[minmax(0,1fr)_58px_62px] items-center border-b border-line/70 px-2 text-[9px] uppercase tracking-wider text-muted/40 sm:grid-cols-[minmax(0,1fr)_68px_72px]">
        <span>Name</span><span className="text-right">Size</span><span className="text-right">Modified</span>
      </div>
      <div className="py-1">
        {files.map((file, index) => <FileRow key={`${path}-${file.name}`} file={file} selected={index === activeIndex} />)}
        <AnimatePresence>{copiedFile && <FileRow key="copied-file" file={copiedFile} copied />}</AnimatePresence>
      </div>
    </div>
  );
}

const shortcuts = [["F3", "View"], ["F4", "Edit"], ["F5", "Copy"], ["F6", "Move"], ["F7", "Mkdir"], ["F8", "Delete"]];

export function CommanderDemo() {
  const reduceMotion = useReducedMotion();
  const [step, setStep] = useState(0);

  useEffect(() => {
    if (reduceMotion) return;
    const timer = window.setInterval(() => setStep((current) => (current + 1) % 7), 850);
    return () => window.clearInterval(timer);
  }, [reduceMotion]);

  const inSrc = step >= 3;
  const activeIndex = useMemo(() => {
    if (step === 0) return 4;
    if (step === 1) return 2;
    if (step === 2) return 0;
    if (step === 3) return 0;
    return 1;
  }, [step]);
  const copying = step === 5;
  const copied = step >= 5;

  return (
    <div className="relative w-full" aria-label="Animated dual-pane file commander demonstration">
      <div className="absolute -inset-8 -z-10 bg-phosphor/[0.035] blur-3xl" />
      <div className="overflow-hidden rounded-xl border border-line bg-surface shadow-terminal">
        <div className="flex h-11 items-center justify-between border-b border-line bg-[#161619] px-4">
          <div className="flex gap-1.5" aria-hidden="true"><span className="h-2 w-2 rounded-full bg-muted/30" /><span className="h-2 w-2 rounded-full bg-muted/30" /><span className="h-2 w-2 rounded-full bg-phosphor/70" /></div>
          <span className="font-mono text-[9px] uppercase tracking-[0.2em] text-muted/60">[NOMBRE] · workspace</span>
          <span className="font-mono text-[9px] text-phosphor">NORMAL</span>
        </div>
        <div className="relative flex h-[266px] font-mono sm:h-[292px]">
          <Pane path={inSrc ? "/home/alex/project/src" : "/home/alex/project"} files={inSrc ? srcFiles : rootFiles} activeIndex={activeIndex} />
          <div className="hidden w-px bg-line sm:block" />
          <div className="hidden flex-1 sm:flex">
            <Pane path="/home/alex/build" files={destinationFiles} copiedFile={copied ? srcFiles[1] : undefined} />
          </div>
          <AnimatePresence>
            {copying && (
              <motion.div initial={{ opacity: 0 }} animate={{ opacity: [0, 0.9, 0] }} exit={{ opacity: 0 }} transition={{ duration: 0.65 }} className="pointer-events-none absolute inset-0 bg-phosphor/[0.09]" />
            )}
          </AnimatePresence>
        </div>
        <div className="flex h-10 items-center justify-between gap-1 overflow-hidden border-t border-line bg-[#161619] px-2 font-mono">
          {shortcuts.map(([key, label]) => (
            <span key={key} className={`whitespace-nowrap text-[8px] text-muted sm:text-[9px] ${copying && key === "F5" ? "text-phosphor" : ""}`}>
              <b className={`mr-1 rounded-sm px-1 py-0.5 font-medium ${copying && key === "F5" ? "bg-phosphor text-base" : "bg-line text-ink"}`}>{key}</b>
              <span className="hidden lg:inline">{label}</span>
            </span>
          ))}
        </div>
      </div>
      <div className="mt-3 flex items-center justify-between px-1 font-mono text-[9px] uppercase tracking-[0.14em] text-muted/50">
        <span><span className="mr-2 inline-block h-1.5 w-1.5 rounded-full bg-phosphor shadow-[0_0_8px_#4ADE80]" />Live session</span>
        <span>60 fps · 4.2 MB</span>
      </div>
    </div>
  );
}
