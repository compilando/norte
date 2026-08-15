const renameRows = [
  ["IMG_8942.JPG", "2026-08-lanzarote-sunrise.jpg"],
  ["Screen Shot 14.22.png", "norte-agent-permissions.png"],
  ["contract_v7_FINAL.pdf", "acme-contract-signed.pdf"],
];

const semanticHits = [
  ["docs/architecture/agent-policy.md", "0.96"],
  ["notes/security-review.md", "0.89"],
  ["src/policy/evaluator.rs", "0.82"],
];

export function AiWorkflows() {
  return (
    <section id="ai" className="relative scroll-mt-16 overflow-hidden bg-phosphor py-24 text-[#0a1008] sm:py-32 lg:py-40">
      <div className="absolute inset-0 opacity-[0.08] [background-image:linear-gradient(#071008_1px,transparent_1px),linear-gradient(90deg,#071008_1px,transparent_1px)] [background-size:72px_72px]" />
      <div className="relative mx-auto max-w-[1440px] px-5 sm:px-8 lg:px-12">
        <div className="grid gap-10 lg:grid-cols-[1.1fr_.9fr] lg:items-end">
          <h2 className="max-w-5xl text-balance text-5xl font-medium leading-[.94] tracking-[-0.07em] sm:text-7xl lg:text-[92px]">AI that asks before it acts.</h2>
          <div className="max-w-lg lg:justify-self-end">
            <p className="text-base leading-7 text-[#192313]/75 sm:text-lg">Describe the outcome. Inspect the plan. Apply when it is right. Norte treats AI as a suggestion engine—not an invisible operator.</p>
            <div className="mt-6 flex flex-wrap gap-x-5 gap-y-2 font-mono text-[9px] uppercase tracking-[0.11em]"><span>Off by default</span><span>Local-only gate</span><span>Denied paths</span></div>
          </div>
        </div>

        <div className="mt-16 grid gap-4 lg:grid-cols-[1.15fr_.85fr]">
          <article className="overflow-hidden rounded-2xl bg-[#09100b] text-ink shadow-[0_30px_90px_rgba(9,16,11,.25)]">
            <div className="flex items-center border-b border-white/[0.09] px-5 py-4 font-mono text-[9px]"><span className="text-phosphor">✦</span><span className="ml-2 text-ink">AI rename plan</span><span className="ml-auto rounded-full border border-phosphor/20 px-2 py-1 text-[8px] text-phosphor">review required</span></div>
            <div className="p-5 sm:p-7">
              <div className="rounded-xl border border-white/[0.1] bg-white/[0.035] px-4 py-3 font-mono text-[10px] text-ink/80">Organize these by date and make every filename meaningful<span className="ml-0.5 inline-block h-3 w-1 bg-phosphor align-middle" /></div>
              <div className="mt-6 flex justify-between font-mono text-[8px] uppercase tracking-[0.13em] text-muted"><span>Proposed changes</span><span>0 files changed</span></div>
              <div className="mt-2">
                {renameRows.map(([from, to]) => (
                  <div key={from} className="grid gap-1 border-t border-white/[0.08] py-3 font-mono text-[9px] sm:grid-cols-[1fr_20px_1.2fr] sm:items-center sm:text-[10px]"><span className="truncate text-muted">{from}</span><span className="hidden text-phosphor sm:block">→</span><span className="truncate text-ink">{to}</span></div>
                ))}
              </div>
              <div className="mt-4 flex items-center justify-between border-t border-white/[0.09] pt-5"><span className="font-mono text-[8px] uppercase tracking-[0.1em] text-muted">validated · collision-free</span><span className="rounded-full bg-phosphor px-4 py-2 font-mono text-[9px] font-semibold uppercase tracking-[0.08em] text-[#09100b]">Apply 3 moves</span></div>
            </div>
          </article>

          <article className="flex min-h-[390px] flex-col overflow-hidden rounded-2xl bg-[#0d1510] text-ink shadow-[0_30px_90px_rgba(9,16,11,.18)]">
            <div className="flex items-center border-b border-white/[0.09] px-5 py-4 font-mono text-[9px]"><span className="text-cyan">⌕</span><span className="ml-2">Search by meaning</span><span className="ml-auto text-muted">local index</span></div>
            <div className="p-5 sm:p-7">
              <p className="font-mono text-[10px] leading-5 text-ink">“where did we define what agents can delete?”</p>
              <div className="mt-5 space-y-2">
                {semanticHits.map(([path, score], index) => (
                  <div key={path} className={`flex items-center gap-3 rounded-lg border px-3 py-3 font-mono text-[9px] ${index === 0 ? "border-phosphor/25 bg-phosphor/[0.08]" : "border-white/[0.07]"}`}><span className={index === 0 ? "text-phosphor" : "text-muted"}>{index + 1}</span><span className="min-w-0 flex-1 truncate text-ink/85">{path}</span><span className="text-muted">{score}</span></div>
                ))}
              </div>
            </div>
            <div className="mt-auto grid grid-cols-3 border-t border-white/[0.09] font-mono text-center text-[8px] uppercase tracking-[0.08em] text-muted"><span className="border-r border-white/[0.09] py-4">Local Ollama</span><span className="border-r border-white/[0.09] py-4">SQLite index</span><span className="py-4">Your data</span></div>
          </article>
        </div>

        <div className="mt-8 grid gap-5 border-t border-[#0a1008]/20 pt-7 text-sm leading-6 text-[#192313]/75 md:grid-cols-3">
          <p><b className="block text-[#0a1008]">Reviewable by construction.</b> AI generates a plan. Ordinary journaled operations apply it.</p>
          <p><b className="block text-[#0a1008]">Private on purpose.</b> Names and content only leave when your configured gate allows it.</p>
          <p><b className="block text-[#0a1008]">Useful beyond prompts.</b> Find “the thing about retry backoff” without remembering its filename.</p>
        </div>
      </div>
    </section>
  );
}
