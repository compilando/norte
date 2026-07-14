const tasks = [
  { action: "COPY", path: "sftp://prod/logs → ~/archive", progress: 74, meta: "128 MB/s" },
  { action: "SEARCH", path: "~/work · content: journal", progress: 46, meta: "12 hits" },
  { action: "VERIFY", path: "release.tar.zst · BLAKE3", progress: 28, meta: "running" },
];

export function Speed() {
  return (
    <section id="architecture" className="py-24 sm:py-36">
      <div className="mx-auto max-w-7xl px-5 sm:px-8">
        <div className="grid gap-12 lg:grid-cols-[0.72fr_1.28fr] lg:items-center lg:gap-20">
          <div>
            <p className="font-mono text-xs uppercase tracking-[0.18em] text-phosphor">Speed is staying in flow</p>
            <h2 className="mt-5 text-balance text-4xl font-semibold tracking-[-0.045em] text-ink sm:text-6xl">Transfers run.<br /><span className="text-muted">You keep moving.</span></h2>
            <p className="mt-6 max-w-lg text-lg leading-8 text-muted">Copies, searches, hashes, and syncs become supervised tasks. Pause them. Reorder them. Cancel cleanly. The interface never becomes a progress bar.</p>
            <div className="mt-8 flex flex-wrap gap-x-6 gap-y-3 font-mono text-[11px] uppercase tracking-[0.12em] text-muted"><span>✓ cancellable</span><span>✓ resumable</span><span>✓ bounded memory</span></div>
          </div>
          <div className="overflow-hidden rounded-xl border border-line bg-surface shadow-terminal">
            <div className="flex items-center justify-between border-b border-line px-5 py-4 font-mono text-xs uppercase tracking-[0.13em] text-muted"><span>Task manager</span><span className="text-phosphor">3 running · 0 blocked</span></div>
            <div className="divide-y divide-line/70 px-4 sm:px-6">
              {tasks.map((task, index) => (
                <div key={task.action} className="py-5">
                  <div className="grid grid-cols-[58px_1fr_auto] items-center gap-3 font-mono text-[11px] sm:grid-cols-[72px_1fr_auto] sm:text-xs">
                    <span className={index === 0 ? "text-phosphor" : "text-muted"}>{task.action}</span><span className="truncate text-ink">{task.path}</span><span className="text-muted/60">{task.meta}</span>
                  </div>
                  <div className="mt-4 flex items-center gap-3"><div className="h-1 flex-1 overflow-hidden bg-line"><div className="h-full bg-phosphor" style={{ width: `${task.progress}%`, opacity: 1 - index * 0.2 }} /></div><span className="w-9 text-right font-mono text-[11px] text-muted">{task.progress}%</span></div>
                </div>
              ))}
            </div>
            <div className="flex items-center justify-between border-t border-line bg-[#0e0e10] px-5 py-4 font-mono text-[11px] text-muted"><span><b className="mr-2 rounded bg-line px-1.5 py-1 font-normal text-ink">SPACE</b>pause</span><span><b className="mr-2 rounded bg-line px-1.5 py-1 font-normal text-ink">J / K</b>priority</span><span><b className="mr-2 rounded bg-line px-1.5 py-1 font-normal text-ink">X</b>cancel</span></div>
          </div>
        </div>
      </div>
    </section>
  );
}
