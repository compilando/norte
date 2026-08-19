"use client";

import { motion, useReducedMotion } from "framer-motion";

const tasks = [
  { icon: "↗", title: "Copy 8.9 GB", route: "local → S3", value: 72, state: "running" },
  { icon: "≋", title: "Compare trees", route: "work → backup", value: 88, state: "verifying" },
  { icon: "✦", title: "Embed index", route: "4,830 files", value: 42, state: "queued" },
  { icon: "⇄", title: "Sync release", route: "SFTP → local", value: 100, state: "complete" },
];

export function AsyncEngine() {
  const reduceMotion = useReducedMotion();
  return (
    <section className="relative border-y border-line bg-[#090c0d] py-24 sm:py-32 lg:py-40">
      <div className="page-grid pointer-events-none absolute inset-0 opacity-35 [mask-image:radial-gradient(circle_at_78%_45%,black,transparent_58%)]" />
      <div className="relative mx-auto grid max-w-[1440px] gap-16 px-5 sm:px-8 lg:grid-cols-[.82fr_1.18fr] lg:items-center lg:gap-24 lg:px-12">
        <div>
          <div className="inline-flex items-center gap-2 rounded-full border border-line bg-surface/70 px-3 py-1.5 font-mono text-[9px] uppercase tracking-[0.15em] text-muted"><span className="h-1.5 w-1.5 rounded-full bg-phosphor" /> Async by design</div>
          <h2 className="mt-7 text-balance text-5xl font-medium leading-[.98] tracking-[-0.06em] text-ink sm:text-6xl lg:text-7xl">Nothing freezes.<br /><span className="text-muted">Everything reports.</span></h2>
          <p className="mt-7 max-w-lg text-base leading-7 text-muted">Launch heavy work and keep navigating. Norte schedules every transfer, comparison, sync and index as a first-class task—with live progress, clean cancellation and conflict queues.</p>
          <div className="mt-9 flex flex-wrap gap-2 font-mono text-[9px] uppercase tracking-[0.1em] text-muted">
            {["observable", "cancellable", "resumable copies", "conflict-safe"].map((item) => <span key={item} className="rounded-full border border-line px-3 py-2">{item}</span>)}
          </div>
        </div>

        <div className="relative">
          <div className="absolute -inset-12 bg-[radial-gradient(circle,rgba(183,255,82,.08),transparent_65%)] blur-xl" />
          <div className="relative overflow-hidden rounded-2xl border border-white/[0.11] bg-[#0d1112] shadow-terminal">
            <div className="flex h-12 items-center border-b border-line px-4 font-mono text-[9px] uppercase tracking-[0.13em]"><span className="text-ink">Task center</span><span className="ml-2 text-muted">/ live</span><span className="ml-auto text-phosphor">3 active</span></div>
            <div className="divide-y divide-line/80">
              {tasks.map((task, index) => (
                <motion.div key={task.title} initial={reduceMotion ? false : { opacity: 0, y: 8 }} whileInView={{ opacity: 1, y: 0 }} viewport={{ once: true }} transition={{ delay: index * 0.08 }} className="grid grid-cols-[34px_1fr_auto] gap-3 px-4 py-4 sm:grid-cols-[38px_1fr_1fr_auto] sm:items-center sm:px-5">
                  <span className={`grid h-8 w-8 place-items-center rounded-lg border ${index === 0 ? "border-phosphor/25 bg-phosphor/[0.07] text-phosphor" : "border-line bg-white/[0.02] text-muted"}`}>{task.icon}</span>
                  <div><p className="text-xs text-ink">{task.title}</p><p className="mt-1 font-mono text-[8px] text-muted">{task.route}</p></div>
                  <div className="col-span-2 ml-[46px] sm:col-span-1 sm:ml-0">
                    <div className="h-1 overflow-hidden rounded-full bg-white/[0.06]"><motion.div initial={{ width: 0 }} whileInView={{ width: `${task.value}%` }} viewport={{ once: true }} transition={{ duration: 1, delay: index * 0.09 }} className={`relative h-full rounded-full ${index === 1 ? "bg-cyan" : task.value === 100 ? "bg-muted/60" : "bg-phosphor"}`} /></div>
                  </div>
                  <span className={`row-start-1 font-mono text-[8px] uppercase tracking-[0.1em] sm:row-auto ${task.state === "running" ? "text-phosphor" : "text-muted"}`}>{task.state}</span>
                </motion.div>
              ))}
            </div>
            <div className="flex items-center justify-between border-t border-line bg-black/20 px-5 py-3 font-mono text-[8px] uppercase tracking-[0.1em] text-muted"><span>Tokio scheduler · 6 workers</span><span className="text-ink">esc cancels safely</span></div>
          </div>
          <div className="absolute -bottom-5 left-8 right-8 -z-10 h-12 rounded-full bg-phosphor/10 blur-2xl" />
        </div>
      </div>
    </section>
  );
}
