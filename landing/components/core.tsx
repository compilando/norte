const clients = ["TUI", "GUI", "CLI", "MCP"];

export function Core() {
  return (
    <section className="py-24 sm:py-32">
      <div className="mx-auto max-w-7xl px-5 sm:px-8">
        <div className="rounded-xl border border-line bg-surface p-6 sm:p-10 lg:p-14">
          <div className="grid gap-12 lg:grid-cols-[1fr_1.1fr] lg:items-center lg:gap-20">
            <div>
              <p className="font-mono text-xs uppercase tracking-[0.18em] text-phosphor">Headless by design</p>
              <h2 className="mt-5 text-balance text-4xl font-semibold tracking-[-0.045em] text-ink sm:text-5xl">One core.<br /><span className="text-muted">Every way in.</span></h2>
              <p className="mt-6 max-w-lg text-base leading-7 text-muted">The interface is a client, not the product boundary. TUI, GUI, scripts, and agents share the same sessions, tasks, policies, and undo history.</p>
            </div>
            <div className="font-mono">
              <div className="grid grid-cols-4 gap-2">
                {clients.map((client, index) => <div key={client} className={`rounded-md border px-2 py-4 text-center text-xs ${index === 0 ? "border-phosphor/50 bg-phosphor/10 text-phosphor" : "border-line bg-base text-muted"}`}>{client}</div>)}
              </div>
              <div className="mx-auto h-9 w-px bg-line" />
              <div className="relative rounded-lg border border-phosphor/30 bg-base px-5 py-6 shadow-glow">
                <div className="absolute left-1/2 top-0 h-px w-2/3 -translate-x-1/2 bg-gradient-to-r from-transparent via-phosphor to-transparent" />
                <div className="flex items-center justify-between"><span className="text-sm text-ink">[NOMBRE] CORE</span><span className="text-[11px] text-phosphor">RUNNING</span></div>
                <div className="mt-5 grid grid-cols-3 gap-2 text-center text-[11px] text-muted"><span className="border-t border-line pt-3">VFS</span><span className="border-t border-line pt-3">TASKS</span><span className="border-t border-line pt-3">JOURNAL</span></div>
              </div>
              <div className="mx-auto h-9 w-px bg-line" />
              <div className="grid grid-cols-4 gap-2 text-center text-[11px] text-muted"><span>LOCAL</span><span>SFTP</span><span>S3</span><span>ARCHIVE</span></div>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
