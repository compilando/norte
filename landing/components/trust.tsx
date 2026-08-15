const proof = [
  { value: "0", label: "accounts required", body: "Install it. Point it at your files. Work." },
  { value: "0", label: "telemetry events", body: "Diagnostics stay on your machine. There is no opt-in funnel." },
  { value: "100%", label: "inspectable", body: "Core, clients, protocol, providers and policy live in the open." },
];

export function Trust() {
  return (
    <section className="py-28 sm:py-36 lg:py-48">
      <div className="mx-auto max-w-[1440px] px-5 sm:px-8 lg:px-12">
        <div className="grid gap-16 lg:grid-cols-[.9fr_1.1fr] lg:items-start lg:gap-24">
          <div>
            <p className="font-mono text-[10px] uppercase tracking-[0.17em] text-phosphor">Open source is the trust model</p>
            <h2 className="mt-6 text-balance text-5xl font-medium leading-[.98] tracking-[-0.06em] sm:text-7xl">Your files are not a growth strategy.</h2>
            <p className="mt-7 max-w-xl text-base leading-7 text-muted">Norte has no account, cloud dependency or telemetry switch hidden in settings. The application is AGPL; its protocol and provider surfaces use permissive licenses so others can build on them.</p>
            <a href="https://github.com/compilando/norte" className="mt-8 inline-flex items-center gap-2 border-b border-phosphor/50 pb-1 font-mono text-[10px] uppercase tracking-[0.1em] text-ink transition hover:border-phosphor hover:text-phosphor">Read every line on GitHub <span>↗</span></a>
          </div>

          <div className="overflow-hidden rounded-2xl border border-line bg-surface/55">
            {proof.map((item, index) => (
              <div key={item.label} className={`grid gap-4 p-6 sm:grid-cols-[110px_1fr] sm:items-center sm:p-7 ${index > 0 ? "border-t border-line" : ""}`}>
                <strong className="text-4xl font-medium tracking-[-0.06em] text-phosphor">{item.value}</strong><div><p className="font-mono text-[9px] uppercase tracking-[0.13em] text-ink">{item.label}</p><p className="mt-2 text-sm leading-6 text-muted">{item.body}</p></div>
              </div>
            ))}
          </div>
        </div>

        <div className="mt-20 overflow-hidden rounded-2xl border border-line bg-[#0b0f10] shadow-terminal">
          <div className="flex items-center border-b border-line px-5 py-4 font-mono text-[9px] uppercase tracking-[0.12em]"><span className="text-muted">policy.toml</span><span className="ml-auto text-phosphor">enforced by core</span></div>
          <div className="grid lg:grid-cols-2">
            <pre className="overflow-x-auto p-6 font-mono text-[10px] leading-7 sm:p-8 sm:text-[11px]"><code><span className="text-muted">[[rule]]</span>{"\n"}<span className="text-cyan">agent</span>   = <span className="text-ink">&quot;*&quot;</span>{"\n"}<span className="text-cyan">path</span>    = <span className="text-ink">&quot;~/projects/**&quot;</span>{"\n"}<span className="text-cyan">write</span>   = <span className="text-phosphor">&quot;ask&quot;</span>{"\n"}<span className="text-cyan">delete</span>  = <span className="text-[#ff9f87]">&quot;deny&quot;</span>{"\n"}<span className="text-cyan">expires</span> = <span className="text-ink">&quot;20m&quot;</span></code></pre>
            <div className="border-t border-line p-6 lg:border-l lg:border-t-0 lg:p-8"><p className="font-mono text-[9px] uppercase tracking-[0.13em] text-muted">The boundary is the product</p><h3 className="mt-5 max-w-lg text-3xl font-medium tracking-[-0.045em] text-ink">Agents never receive a secret backdoor to your filesystem.</h3><p className="mt-5 max-w-lg text-sm leading-6 text-muted">Every request crosses the same core as the human clients: scoped access, explicit approvals, expiring grants, attribution and session-level undo.</p><div className="mt-7 flex flex-wrap gap-2 font-mono text-[8px] uppercase tracking-[0.1em] text-muted"><span className="rounded-full border border-line px-3 py-2">fail closed</span><span className="rounded-full border border-line px-3 py-2">hash-chained audit</span><span className="rounded-full border border-line px-3 py-2">human override</span></div></div>
          </div>
        </div>
      </div>
    </section>
  );
}
