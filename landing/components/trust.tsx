const journal = [
  ["10:31:04", "agent", "proposed 12 file moves", "review"],
  ["10:31:18", "you", "approved scoped plan", "allow"],
  ["10:31:19", "core", "11 moved · 1 skipped", "done"],
  ["10:32:02", "you", "reverted agent session", "undo"],
];

export function Trust() {
  return (
    <section className="border-y border-line/70 bg-surface/30 py-24 sm:py-32">
      <div className="mx-auto max-w-7xl px-5 sm:px-8">
        <div className="mx-auto max-w-3xl text-center">
          <p className="font-mono text-xs uppercase tracking-[0.18em] text-phosphor">Built for the agent era</p>
          <h2 className="mt-6 text-balance text-4xl font-semibold tracking-[-0.05em] text-ink sm:text-6xl">Let agents touch files.<br /><span className="text-muted">Never let go.</span></h2>
          <p className="mx-auto mt-6 max-w-2xl text-lg leading-8 text-muted">Agents operate through the same core as you—with scoped access, human approvals, a complete audit trail, and session-level undo.</p>
        </div>

        <div className="mt-16 grid overflow-hidden rounded-xl border border-line bg-base lg:grid-cols-[0.8fr_1.2fr]">
          <div className="border-b border-line p-6 lg:border-b-0 lg:border-r lg:p-8">
            <div className="flex items-center justify-between font-mono text-xs uppercase tracking-[0.12em]"><span className="text-muted">policy.toml</span><span className="text-phosphor">enforced</span></div>
            <pre className="mt-10 overflow-x-auto font-mono text-[11px] leading-7 sm:text-xs"><code><span className="text-muted">[[rule]]</span>{"\n"}<span className="text-phosphor">agent</span>   = <span className="text-ink">&quot;*&quot;</span>{"\n"}<span className="text-phosphor">path</span>    = <span className="text-ink">&quot;~/projects/**&quot;</span>{"\n"}<span className="text-phosphor">write</span>   = <span className="text-ink">&quot;ask&quot;</span>{"\n"}<span className="text-phosphor">delete</span>  = <span className="text-ink">&quot;deny&quot;</span>{"\n"}<span className="text-phosphor">network</span> = <span className="text-ink">false</span></code></pre>
            <div className="mt-10 border-l-2 border-phosphor/50 pl-4 font-mono text-xs leading-6 text-muted">Policy is evaluated by the core.<br />The client cannot bypass it.</div>
          </div>
          <div className="p-6 lg:p-8">
            <div className="flex items-center justify-between font-mono text-xs uppercase tracking-[0.12em]"><span className="text-muted">Session journal</span><span className="text-muted">agent / build-cleanup</span></div>
            <div className="mt-8">
              {journal.map(([time, actor, action, status], index) => (
                <div key={time} className="grid grid-cols-[62px_1fr_auto] gap-3 border-b border-line/70 py-4 font-mono text-[11px] sm:grid-cols-[78px_64px_1fr_auto] sm:text-xs">
                  <span className="text-muted/70">{time}</span><span className="hidden text-muted sm:block">{actor}</span><span className={index === journal.length - 1 ? "text-ink" : "text-muted"}>{action}</span><span className={`uppercase ${status === "deny" || status === "undo" ? "text-phosphor" : "text-muted"}`}>{status}</span>
                </div>
              ))}
            </div>
            <div className="mt-7 flex flex-wrap gap-2 font-mono text-[11px] uppercase tracking-[0.1em] text-muted"><span className="rounded border border-line px-2.5 py-1.5">Hash-chained</span><span className="rounded border border-line px-2.5 py-1.5">Exportable</span><span className="rounded border border-phosphor/30 bg-phosphor/5 px-2.5 py-1.5 text-phosphor">Undo ready</span></div>
          </div>
        </div>
        <p className="mt-5 text-center font-mono text-[11px] uppercase tracking-[0.11em] text-muted">Agent workflows are part of the [NOMBRE] roadmap</p>
      </div>
    </section>
  );
}
