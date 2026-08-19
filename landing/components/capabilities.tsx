function VfsMap() {
  return (
    <div className="relative mt-8 h-40 overflow-hidden rounded-xl border border-line bg-black/20 p-4 font-mono">
      <div className="absolute left-8 right-8 top-1/2 h-px bg-line" />
      <div className="relative flex h-full items-center justify-between">
        {["LOCAL", "SFTP", "S3", "ZIP"].map((item, index) => <div key={item} className="z-10 text-center"><span className={`grid h-10 w-10 place-items-center rounded-full border text-[9px] ${index === 0 ? "border-phosphor bg-phosphor text-base" : "border-line bg-surface text-muted"}`}>{index + 1}</span><span className={`mt-3 block text-[8px] ${index === 0 ? "text-phosphor" : "text-muted"}`}>{item}</span></div>)}
      </div>
    </div>
  );
}

function DiffArtifact() {
  return (
    <div className="mt-8 overflow-hidden rounded-xl border border-line bg-black/20 font-mono text-[9px]">
      <div className="grid grid-cols-[26px_1fr_auto] gap-2 border-b border-line px-3 py-2 text-muted"><span /><span>path</span><span>verdict</span></div>
      {[["=", "src/core.rs", "same"], ["→", "dist/norte", "copy"], ["!", "docs/index.md", "newer"]].map(([mark, path, state], index) => <div key={path} className={`grid grid-cols-[26px_1fr_auto] gap-2 px-3 py-2.5 ${index < 2 ? "border-b border-line/70" : ""}`}><span className={index === 1 ? "text-phosphor" : "text-muted"}>{mark}</span><span className="text-ink/80">{path}</span><span className="text-muted">{state}</span></div>)}
    </div>
  );
}

function KeyArtifact() {
  return (
    <div className="mt-8 rounded-xl border border-line bg-black/20 p-4 font-mono">
      <div className="flex items-center gap-2">{["SPACE", "C", "E"].map((key) => <kbd key={key} className="rounded-md border border-white/[0.13] bg-white/[0.04] px-3 py-2 text-[9px] text-ink shadow-[0_2px_0_#272e2b]">{key}</kbd>)}<span className="ml-auto text-[9px] text-muted">compare</span></div>
      <div className="mt-5 flex justify-between border-t border-line pt-4 text-[8px] text-muted"><span><b className="text-phosphor">VIM</b> preset</span><span>which-key active</span></div>
    </div>
  );
}

function PluginArtifact() {
  return (
    <div className="mt-8 space-y-2 rounded-xl border border-line bg-black/20 p-3 font-mono text-[9px]">
      {["filesystem: read", "ui: decorate", "network: denied"].map((item, index) => <div key={item} className="flex items-center justify-between rounded-lg border border-line/70 px-3 py-2.5"><span className="text-muted">{item}</span><span className={index < 2 ? "text-phosphor" : "text-[#ff9f87]"}>{index < 2 ? "allow" : "deny"}</span></div>)}
    </div>
  );
}

export function Capabilities() {
  return (
    <section id="features" className="py-28 sm:py-36 lg:py-48">
      <div className="mx-auto max-w-[1440px] px-5 sm:px-8 lg:px-12">
        <div className="grid gap-8 lg:grid-cols-[1fr_.65fr] lg:items-end">
          <div><p className="font-mono text-[10px] uppercase tracking-[0.17em] text-phosphor">Deceptively capable</p><h2 className="mt-6 max-w-4xl text-balance text-5xl font-medium leading-[.98] tracking-[-0.06em] sm:text-7xl">The workflows you know. <span className="text-muted">The system they deserved.</span></h2></div>
          <p className="max-w-lg text-base leading-7 text-muted lg:justify-self-end">Dual-pane speed, modern search, remote providers, comparison, sync and plugins—all running through the same governed core.</p>
        </div>

        <div className="mt-16 grid gap-4 md:grid-cols-2 lg:grid-cols-12">
          <article className="rounded-2xl border border-line bg-surface/70 p-6 lg:col-span-7 lg:p-8"><span className="font-mono text-[9px] uppercase tracking-[0.15em] text-phosphor">Universal VFS</span><h3 className="mt-4 text-2xl font-medium tracking-[-0.04em]">Every location feels local.</h3><p className="mt-3 max-w-xl text-sm leading-6 text-muted">Move between a disk, an SFTP host, S3 or the inside of an archive without relearning the tool.</p><VfsMap /></article>
          <article className="rounded-2xl border border-line bg-surface/70 p-6 lg:col-span-5 lg:p-8"><span className="font-mono text-[9px] uppercase tracking-[0.15em] text-cyan">Compare + sync</span><h3 className="mt-4 text-2xl font-medium tracking-[-0.04em]">See the delta. Approve the plan.</h3><p className="mt-3 text-sm leading-6 text-muted">Stream huge comparisons with bounded memory, then turn the result into a reviewable, undoable sync.</p><DiffArtifact /></article>
          <article className="rounded-2xl border border-line bg-surface/70 p-6 lg:col-span-4 lg:p-8"><span className="font-mono text-[9px] uppercase tracking-[0.15em] text-phosphor">Keyboard language</span><h3 className="mt-4 text-2xl font-medium tracking-[-0.04em]">Muscle memory, yours.</h3><p className="mt-3 text-sm leading-6 text-muted">Orthodox, Vim, CUA, FAR, Norton and more. Rebind every command.</p><KeyArtifact /></article>
          <article className="relative overflow-hidden rounded-2xl border border-line bg-[linear-gradient(135deg,#151a1d,#0b0f10)] p-6 lg:col-span-4 lg:p-8"><div className="absolute -right-12 -top-12 h-40 w-40 rounded-full bg-phosphor/[0.08] blur-3xl" /><span className="font-mono text-[9px] uppercase tracking-[0.15em] text-phosphor">Journal + undo</span><h3 className="mt-4 text-2xl font-medium tracking-[-0.04em]">Every move leaves a trail.</h3><p className="mt-3 text-sm leading-6 text-muted">Mutations are attributed to you, a plugin or an agent. Revert an entire session—even after the agent is gone.</p><div className="mt-8 rounded-xl border border-phosphor/20 bg-phosphor/[0.05] p-4 font-mono text-[9px]"><div className="flex justify-between text-muted"><span>agent / build-cleanup</span><span>14 ops</span></div><div className="mt-5 flex items-center justify-between"><span className="text-ink">session committed</span><span className="text-phosphor">undo ready ↩</span></div></div></article>
          <article className="rounded-2xl border border-line bg-surface/70 p-6 lg:col-span-4 lg:p-8"><span className="font-mono text-[9px] uppercase tracking-[0.15em] text-cyan">WASM plugins</span><h3 className="mt-4 text-2xl font-medium tracking-[-0.04em]">Extensible. Not exposed.</h3><p className="mt-3 text-sm leading-6 text-muted">Plugins declare capabilities and wait for your approval. No permission means no syscall.</p><PluginArtifact /></article>
        </div>
      </div>
    </section>
  );
}
