const renameRows = [
  ["IMG_8942.JPG", "2026-08-lanzarote-sunrise.jpg"],
  ["Screen Shot 14.22.png", "norte-agent-permissions.png"],
  ["contract_v7_FINAL.pdf", "acme-contract-signed.pdf"],
];

const organizeTree: [number, string, "new" | "kept" | "file"][] = [
  [0, "invoices/", "new"],
  [1, "2026/", "new"],
  [2, "acme-contract-signed.pdf", "file"],
  [2, "2026-07-acme-invoice.pdf", "file"],
  [0, "photos/", "kept"],
  [1, "2026-08-lanzarote-sunrise.jpg", "file"],
  [0, "archives/", "new"],
  [1, "launch-assets.zip", "file"],
];

const semanticHits = [
  ["docs/architecture/agent-policy.md", "0.96"],
  ["notes/security-review.md", "0.89"],
  ["src/policy/evaluator.rs", "0.82"],
];

const marker = { new: "+", kept: "=", file: "·" } as const;
const markerTone = { new: "text-phosphor", kept: "text-muted", file: "text-muted/45" } as const;

function CardHeader({ icon, title, badge, tone = "phosphor" }: { icon: string; title: string; badge: string; tone?: "phosphor" | "cyan" }) {
  return (
    <div className="flex items-center gap-2 border-b border-white/[0.09] px-5 py-4 font-mono text-[9px]">
      <span className={tone === "cyan" ? "text-cyan" : "text-phosphor"}>{icon}</span>
      <span className="text-ink">{title}</span>
      <span className={`ml-auto rounded-full border px-2 py-1 text-[8px] uppercase tracking-[0.08em] ${tone === "cyan" ? "border-cyan/25 text-cyan" : "border-phosphor/25 text-phosphor"}`}>
        {badge}
      </span>
    </div>
  );
}

export function AiWorkflows() {
  return (
    <section id="ai" className="relative scroll-mt-16 overflow-hidden bg-phosphor py-24 text-[#0a1008] sm:py-32 lg:py-40">
      <div className="absolute inset-0 opacity-[0.08] [background-image:linear-gradient(#071008_1px,transparent_1px),linear-gradient(90deg,#071008_1px,transparent_1px)] [background-size:72px_72px]" />
      <div className="relative mx-auto max-w-[1440px] px-5 sm:px-8 lg:px-12">
        <div className="grid gap-10 lg:grid-cols-[1.1fr_.9fr] lg:items-end">
          <h2 className="max-w-5xl text-balance text-5xl font-medium leading-[.94] tracking-[-0.07em] sm:text-7xl lg:text-[92px]">AI that asks before it acts.</h2>
          <div className="max-w-lg lg:justify-self-end">
            <p className="text-base leading-7 text-[#192313]/75 sm:text-lg">
              Describe the outcome. Inspect the plan. Apply when it is right. Norte treats AI as a suggestion engine—not an invisible operator.
            </p>
            <div className="mt-6 flex flex-wrap gap-x-5 gap-y-2 font-mono text-[9px] uppercase tracking-[0.11em]">
              <span>Off by default</span>
              <span>Local-only gate</span>
              <span>Denied paths</span>
              <span>Plans are batches</span>
            </div>
          </div>
        </div>

        <div className="mt-16 grid gap-4 lg:grid-cols-3">
          <article className="overflow-hidden rounded-2xl bg-[#09100b] text-ink shadow-[0_30px_90px_rgba(9,16,11,.25)]">
            <CardHeader icon="✦" title="Rename a batch" badge="review required" />
            <div className="p-5 sm:p-6">
              <div className="rounded-xl border border-white/[0.1] bg-white/[0.035] px-4 py-3 font-mono text-[10px] leading-5 text-ink/80">
                Use dates and meaningful project names
                <span className="caret ml-0.5 inline-block h-3 w-1 bg-phosphor align-middle" />
              </div>
              <div className="mt-5 flex justify-between font-mono text-[8px] uppercase tracking-[0.13em] text-muted">
                <span>Proposed changes</span>
                <span>0 files changed</span>
              </div>
              <div className="mt-1">
                {renameRows.map(([from, to]) => (
                  <div key={from} className="grid gap-0.5 border-t border-white/[0.08] py-2.5 font-mono text-[9px]">
                    <span className="truncate text-muted">{from}</span>
                    <span className="truncate text-ink">→ {to}</span>
                  </div>
                ))}
              </div>
              <div className="mt-4 flex items-center justify-between border-t border-white/[0.09] pt-4">
                <span className="font-mono text-[8px] uppercase tracking-[0.1em] text-muted">collision-free</span>
                <span className="rounded-full bg-phosphor px-3 py-1.5 font-mono text-[9px] font-semibold uppercase tracking-[0.08em] text-[#09100b]">Apply 3 moves</span>
              </div>
            </div>
          </article>

          <article className="overflow-hidden rounded-2xl bg-[#09100b] text-ink shadow-[0_30px_90px_rgba(9,16,11,.25)]">
            <CardHeader icon="⊞" title="Organize a directory" badge="one batch" />
            <div className="p-5 sm:p-6">
              <p className="font-mono text-[10px] leading-5 text-ink/80">Creates <b className="text-phosphor">3 folders</b> and moves <b className="text-phosphor">12 files</b>.</p>
              <div className="mt-4 rounded-xl border border-white/[0.08] bg-black/25 p-3 font-mono text-[9px] leading-[1.9]">
                {organizeTree.map(([depth, name, kind], index) => (
                  <div key={`${name}-${index}`} className="flex items-center gap-2" style={{ paddingLeft: depth * 14 }}>
                    <span className={markerTone[kind]}>{marker[kind]}</span>
                    <span className={kind === "file" ? "truncate text-muted" : "truncate text-ink"}>{name}</span>
                    {kind === "kept" && <span className="ml-auto shrink-0 text-[8px] uppercase tracking-wider text-muted/60">existed</span>}
                  </div>
                ))}
              </div>
              <p className="mt-4 text-[11px] leading-5 text-muted">
                A tree, not a list of pairs: what changes is the <i>shape</i> of the directory. Approving needs you to have reached the end of it.
              </p>
              <div className="mt-4 flex items-center justify-between border-t border-white/[0.09] pt-4">
                <span className="font-mono text-[8px] uppercase tracking-[0.1em] text-muted">undo puts it all back</span>
                <span className="rounded-full bg-phosphor px-3 py-1.5 font-mono text-[9px] font-semibold uppercase tracking-[0.08em] text-[#09100b]">Apply plan</span>
              </div>
            </div>
          </article>

          <article className="flex flex-col overflow-hidden rounded-2xl bg-[#0d1510] text-ink shadow-[0_30px_90px_rgba(9,16,11,.18)]">
            <CardHeader icon="⌕" title="Search by meaning" badge="local index" tone="cyan" />
            <div className="p-5 sm:p-6">
              <p className="font-mono text-[10px] leading-5 text-ink">“where did we define what agents can delete?”</p>
              <div className="mt-5 space-y-2">
                {semanticHits.map(([path, score], index) => (
                  <div
                    key={path}
                    className={`flex items-center gap-3 rounded-lg border px-3 py-2.5 font-mono text-[9px] ${index === 0 ? "border-phosphor/25 bg-phosphor/[0.08]" : "border-white/[0.07]"}`}
                  >
                    <span className={index === 0 ? "text-phosphor" : "text-muted"}>{index + 1}</span>
                    <span className="min-w-0 flex-1 truncate text-ink/85">{path}</span>
                    <span className="text-muted">{score}</span>
                  </div>
                ))}
              </div>
              <p className="mt-5 text-[11px] leading-5 text-muted">
                And when you do know what you want, ordinary search takes ten filters: a size range written the way people say it (<span className="font-mono text-ink/80">1M</span>, <span className="font-mono text-ink/80">2.5G</span>), changed in the last N days, folders to skip, whole-word content, a forced encoding.
              </p>
            </div>
            <div className="mt-auto grid grid-cols-3 border-t border-white/[0.09] text-center font-mono text-[8px] uppercase tracking-[0.08em] text-muted">
              <span className="border-r border-white/[0.09] py-4">Local Ollama</span>
              <span className="border-r border-white/[0.09] py-4">SQLite index</span>
              <span className="py-4">Your data</span>
            </div>
          </article>
        </div>

        <div className="mt-8 grid gap-5 border-t border-[#0a1008]/20 pt-7 text-sm leading-6 text-[#192313]/75 md:grid-cols-3">
          <p><b className="block text-[#0a1008]">Reviewable by construction.</b> AI produces a plan. Ordinary journaled operations apply it, under one batch id, so one undo takes it all back.</p>
          <p><b className="block text-[#0a1008]">Private on purpose.</b> Names and content leave only when your configured gate allows it. A plan can also come from a plugin instead of a model.</p>
          <p><b className="block text-[#0a1008]">Useful beyond prompts.</b> Find “the thing about retry backoff” without remembering its filename—or every file over a gigabyte, with no name at all.</p>
        </div>
      </div>
    </section>
  );
}
