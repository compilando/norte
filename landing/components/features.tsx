import { LineIcon, type IconName } from "./icons";

const features: { icon: IconName; title: string; body: string }[] = [
  { icon: "bolt", title: "Native at the core", body: "Rust underneath. Instant embedded startup. Heavy work runs as cancellable background tasks." },
  { icon: "panels", title: "One universal VFS", body: "Local disks, SFTP, object storage, and archives behave like one coherent filesystem." },
  { icon: "branch", title: "Undo the dangerous stuff", body: "A transactional journal makes mutations reversible and keeps permanent actions explicit." },
  { icon: "keyboard", title: "A keyboard language", body: "Chords, command palette, and contextual maps turn complex workflows into muscle memory." },
  { icon: "search", title: "Search you can operate", body: "Results open as a virtual pane—ready to inspect, select, copy, or delete." },
  { icon: "blocks", title: "Extensions with boundaries", body: "WASM plugins declare their permissions. No capability means no syscall." },
];

const artifactBase = "relative mt-8 h-[160px] overflow-hidden rounded-lg border border-line bg-[#0e0e10] font-mono text-[11px] shadow-[inset_0_1px_0_rgba(255,255,255,0.025)]";

function ProductArtifact({ index }: { index: number }) {
  if (index === 0) return (
    <div className={`${artifactBase} p-4`}>
      <div className="flex items-center justify-between text-[10px] uppercase tracking-[0.12em] text-muted"><span>norte-core</span><span className="flex items-center gap-2 text-phosphor"><i className="h-1.5 w-1.5 rounded-full bg-phosphor shadow-[0_0_8px_#4ADE80]" />running</span></div>
      <div className="mt-6 grid grid-cols-3 gap-2"><div className="rounded border border-line bg-surface p-2.5"><span className="text-muted">MODE</span><b className="mt-2 block font-normal text-ink">embedded</b></div><div className="rounded border border-line bg-surface p-2.5"><span className="text-muted">QUEUE</span><b className="mt-2 block font-normal text-ink">03 tasks</b></div><div className="rounded border border-phosphor/20 bg-phosphor/[0.04] p-2.5"><span className="text-muted">STATE</span><b className="mt-2 block font-normal text-phosphor">ready</b></div></div>
    </div>
  );
  if (index === 1) return (
    <div className={`${artifactBase} p-4`}>
      <div className="absolute left-[13%] right-[13%] top-[68px] h-px bg-line" />
      <div className="relative flex h-full items-center justify-between">
        {["LOCAL", "SFTP", "S3", "ZIP"].map((item, itemIndex) => <div key={item} className="z-10 text-center"><span className={`grid h-10 w-10 place-items-center rounded-md border sm:h-11 sm:w-11 ${itemIndex === 0 ? "border-phosphor bg-phosphor text-base" : "border-line bg-surface text-muted"}`}>0{itemIndex + 1}</span><span className={`mt-3 block text-[10px] ${itemIndex === 0 ? "text-phosphor" : "text-muted"}`}>{item}</span></div>)}
      </div>
    </div>
  );
  if (index === 2) return (
    <div className={`${artifactBase} p-4`}>
      <div className="space-y-3"><div className="grid grid-cols-[10px_1fr_auto] items-center gap-3"><span className="h-1.5 w-1.5 rounded-full bg-muted/50" /><span className="text-muted">move 42 entries</span><span className="text-muted">done</span></div><div className="grid grid-cols-[10px_1fr_auto] items-center gap-3"><span className="h-1.5 w-1.5 rounded-full bg-muted/50" /><span className="text-muted">verify destination</span><span className="text-muted">done</span></div><div className="grid grid-cols-[10px_1fr_auto] items-center gap-3"><span className="h-1.5 w-1.5 rounded-full bg-phosphor" /><span className="text-ink">transaction committed</span><span className="text-phosphor">undo</span></div></div>
      <div className="absolute bottom-0 left-0 right-0 border-t border-phosphor/20 bg-phosphor/[0.05] px-4 py-2.5 text-center text-[10px] uppercase tracking-[0.12em] text-phosphor">Session can be reverted safely</div>
    </div>
  );
  if (index === 3) return (
    <div className={`${artifactBase} p-4`}>
      <div className="flex items-center gap-2">{["SPACE", "C", "E"].map((item) => <kbd key={item} className="rounded border border-muted/30 bg-surface px-3 py-2 text-ink shadow-[0_2px_0_#26262B]">{item}</kbd>)}<span className="ml-auto text-muted">compare</span></div>
      <div className="mt-5 grid grid-cols-3 gap-2 border-t border-line pt-4 text-[10px]"><span><b className="mr-1 text-phosphor">C</b><i className="not-italic text-muted">copy</i></span><span><b className="mr-1 text-phosphor">E</b><i className="not-italic text-muted">compare</i></span><span><b className="mr-1 text-phosphor">S</b><i className="not-italic text-muted">sync</i></span></div>
      <p className="mt-4 text-[10px] text-muted">which-key · pane.compare</p>
    </div>
  );
  if (index === 4) return (
    <div className={`${artifactBase} p-4`}>
      <div className="flex items-center gap-2 border-b border-line pb-3 text-ink"><span className="text-lg leading-none text-phosphor">/</span><span>journal rs</span><span className="ml-auto text-[10px] text-muted">12 results</span></div>
      <div className="mt-3 space-y-2.5"><p className="flex justify-between text-muted"><span><b className="font-normal text-phosphor">src/journal.rs</b> · undo</span><span>8.1 KB</span></p><p className="flex justify-between text-muted"><span><b className="font-normal text-phosphor">tests/journal.rs</b> · commit</span><span>4.6 KB</span></p><p className="flex justify-between text-muted"><span>docs/architecture.md</span><span>2.2 KB</span></p></div>
    </div>
  );
  return (
    <div className={`${artifactBase} p-4`}>
      <div className="grid grid-cols-[1fr_auto] items-center border-b border-line pb-3 text-[10px] uppercase tracking-[0.1em] text-muted"><span>plugin.toml</span><span>capabilities</span></div>
      <div className="mt-3 space-y-2.5"><div className="flex items-center justify-between"><span className="text-ink">filesystem.read</span><span className="rounded bg-phosphor/10 px-2 py-1 text-phosphor">ALLOW</span></div><div className="flex items-center justify-between"><span className="text-ink">filesystem.write</span><span className="rounded border border-line px-2 py-1 text-muted">ASK</span></div><div className="flex items-center justify-between"><span className="text-ink">network</span><span className="rounded border border-line px-2 py-1 text-muted">DENY</span></div></div>
    </div>
  );
}

export function Features() {
  return (
    <section id="features" className="border-b border-line/70 bg-surface/30 py-24 sm:py-36">
      <div className="mx-auto max-w-7xl px-5 sm:px-8">
        <div className="mb-16 grid gap-6 lg:grid-cols-[1fr_0.55fr] lg:items-end"><div><p className="font-mono text-sm uppercase tracking-[0.15em] text-phosphor">Rebuilt from the core out</p><h2 className="mt-5 max-w-3xl text-balance text-4xl font-semibold tracking-[-0.045em] text-ink sm:text-6xl">Not a prettier file browser. <span className="text-muted">A new foundation.</span></h2></div><p className="max-w-md text-lg leading-8 text-muted lg:pb-2">Fast where you touch it. Careful where it matters. Open everywhere else.</p></div>
        <div className="grid overflow-hidden rounded-xl border border-line sm:grid-cols-2 lg:grid-cols-3">
          {features.map((feature, index) => (
            <article key={feature.title} className={`group relative min-h-[430px] overflow-hidden border-line bg-base/60 p-7 transition-colors duration-200 hover:bg-surface ${index < 3 ? "lg:border-b" : ""} ${index % 3 !== 2 ? "lg:border-r" : ""} ${index < 4 ? "max-lg:border-b" : ""} ${index % 2 === 0 ? "sm:border-r lg:border-r" : ""}`}>
              <div className="absolute right-5 top-2 font-mono text-[64px] font-semibold tracking-[-0.08em] text-white/[0.025]">0{index + 1}</div>
              <div className="relative flex items-center justify-between"><span className="grid h-10 w-10 place-items-center rounded-md border border-line bg-surface"><LineIcon name={feature.icon} className="h-5 w-5 text-muted transition-colors duration-200 group-hover:text-phosphor" /></span><span className="font-mono text-xs text-muted">0{index + 1}</span></div>
              <ProductArtifact index={index} />
              <h3 className="mt-7 font-mono text-base font-semibold text-ink">{feature.title}</h3>
              <p className="mt-3 text-[15px] leading-6 text-muted">{feature.body}</p>
              <div className="absolute inset-x-0 bottom-0 h-px origin-left scale-x-0 bg-phosphor transition-transform duration-300 group-hover:scale-x-100" />
            </article>
          ))}
        </div>
      </div>
    </section>
  );
}
