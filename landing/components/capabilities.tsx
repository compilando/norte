const providers = [
  { label: "LOCAL", note: "disk" },
  { label: "SFTP", note: "ssh" },
  { label: "FTP", note: "wasm" },
  { label: "S3", note: "object" },
  { label: "ZIP", note: "archive" },
  { label: "RAR", note: "archive" },
];

const treemap = [
  { name: "target", share: 46, tone: "bg-phosphor/85", size: "58.1 GB" },
  { name: "node_modules", share: 22, tone: "bg-cyan/75", size: "27.4 GB" },
  { name: "assets", share: 14, tone: "bg-amber/70", size: "17.2 GB" },
  { name: ".git", share: 11, tone: "bg-white/25", size: "13.9 GB" },
  { name: "src", share: 7, tone: "bg-white/12", size: "8.4 GB" },
];

const pluginKinds = ["previewer", "decorator", "renamer", "organizer", "thumbnail", "provider", "panel", "hook"];

function VfsMap() {
  return (
    <div className="relative mt-8 overflow-hidden rounded-xl border border-line bg-black/20 p-5 font-mono">
      <div className="absolute inset-x-9 top-[44px] h-px bg-line" />
      <div className="relative flex items-start justify-between gap-1">
        {providers.map((item, index) => (
          <div key={item.label} className="z-10 text-center">
            <span className={`mx-auto grid h-9 w-9 place-items-center rounded-full border text-[8px] ${index === 0 ? "border-phosphor bg-phosphor text-base" : "border-line bg-surface text-muted"}`}>
              {index + 1}
            </span>
            <span className={`mt-3 block text-[8px] ${index === 0 ? "text-phosphor" : "text-muted"}`}>{item.label}</span>
            <span className="mt-1 block text-[7px] uppercase tracking-[0.1em] text-muted/45">{item.note}</span>
          </div>
        ))}
      </div>
      <p className="mt-5 border-t border-line pt-4 text-[8px] uppercase tracking-[0.11em] text-muted/70">
        WebDAV and anything else arrive as a provider plugin
      </p>
    </div>
  );
}

function DiffArtifact() {
  const rows: [string, string, string, string][] = [
    ["=", "src/core.rs", "same", "text-muted"],
    ["→", "dist/norte", "copy", "text-phosphor"],
    ["!", "docs/index.md", "newer here", "text-amber"],
    ["←", "notes/todo.md", "pull", "text-cyan"],
  ];
  return (
    <div className="mt-8 overflow-hidden rounded-xl border border-line bg-black/20 font-mono text-[9px]">
      <div className="grid grid-cols-[26px_1fr_auto] gap-2 border-b border-line px-3 py-2 text-muted">
        <span />
        <span>path</span>
        <span>verdict</span>
      </div>
      {rows.map(([mark, path, state, tone], index) => (
        <div key={path} className={`grid grid-cols-[26px_1fr_auto] gap-2 px-3 py-2.5 ${index < rows.length - 1 ? "border-b border-line/70" : ""}`}>
          <span className={tone}>{mark}</span>
          <span className="truncate text-ink/80">{path}</span>
          <span className="text-muted">{state}</span>
        </div>
      ))}
    </div>
  );
}

function DiskMap() {
  return (
    <div className="mt-8 rounded-xl border border-line bg-black/20 p-4 font-mono">
      <div className="flex h-24 gap-1 overflow-hidden rounded-lg">
        {treemap.map((block, index) => (
          <div
            key={block.name}
            className={`relative flex flex-col justify-end overflow-hidden rounded p-2 ${block.tone} ${index === 0 ? "ring-1 ring-phosphor" : ""}`}
            style={{ flex: block.share }}
          >
            <span className="truncate text-[8px] font-medium text-[#07090a]">{index < 3 ? block.name : ""}</span>
          </div>
        ))}
      </div>
      <div className="mt-4 space-y-1.5 border-t border-line pt-3 text-[8px]">
        {treemap.slice(0, 3).map((block) => (
          <div key={block.name} className="flex justify-between text-muted">
            <span>{block.name}</span>
            <span className="text-ink/75">{block.size}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

function ViewerArtifact() {
  return (
    <div className="mt-8 overflow-hidden rounded-xl border border-line bg-black/20 font-mono text-[9px]">
      <div className="flex items-center gap-2 border-b border-line px-3 py-2 text-muted">
        <span className="text-cyan">◫</span>
        <span className="truncate text-ink/80">hero-master.webp</span>
        <span className="ml-auto text-[8px] uppercase tracking-wider text-muted/60">2048 × 1152</span>
      </div>
      <div className="relative h-[74px] bg-[linear-gradient(120deg,#132a2a,#1b3a2a_40%,#2a3a18_72%,#0e1a16)]">
        <div className="terminal-scan absolute inset-0" />
        <span className="absolute bottom-2 left-3 rounded bg-black/55 px-1.5 py-0.5 text-[7px] uppercase tracking-wider text-phosphor">sixel · kitty · iterm</span>
      </div>
      <div className="grid grid-cols-4 divide-x divide-line border-t border-line text-center text-[7px] uppercase tracking-[0.08em] text-muted">
        {["image", "markdown", "syntax", "hex"].map((tab, index) => (
          <span key={tab} className={`py-2.5 ${index === 0 ? "text-phosphor" : ""}`}>{tab}</span>
        ))}
      </div>
    </div>
  );
}

function PluginArtifact() {
  return (
    <div className="mt-8 rounded-xl border border-line bg-black/20 p-3 font-mono text-[9px]">
      <div className="space-y-2">
        {[
          ["filesystem: read", "allow"],
          ["ui: decorate", "allow"],
          ["network: any host", "deny"],
        ].map(([item, verdict]) => (
          <div key={item} className="flex items-center justify-between rounded-lg border border-line/70 px-3 py-2.5">
            <span className="text-muted">{item}</span>
            <span className={verdict === "allow" ? "text-phosphor" : "text-coral"}>{verdict}</span>
          </div>
        ))}
      </div>
      <div className="mt-4 flex flex-wrap gap-1.5 border-t border-line pt-3.5 text-[7px] uppercase tracking-[0.08em] text-muted/70">
        {pluginKinds.map((kind) => (
          <span key={kind} className="rounded border border-line/70 px-1.5 py-1">{kind}</span>
        ))}
      </div>
    </div>
  );
}

export function Capabilities() {
  return (
    <section id="features" className="py-28 sm:py-36 lg:py-48">
      <div className="mx-auto max-w-[1440px] px-5 sm:px-8 lg:px-12">
        <div className="grid gap-8 lg:grid-cols-[1fr_.65fr] lg:items-end">
          <div>
            <p className="font-mono text-[10px] uppercase tracking-[0.17em] text-phosphor">Deceptively capable</p>
            <h2 className="mt-6 max-w-4xl text-balance text-5xl font-medium leading-[.98] tracking-[-0.06em] sm:text-7xl">
              The workflows you know. <span className="text-muted">The system they deserved.</span>
            </h2>
          </div>
          <p className="max-w-lg text-base leading-7 text-muted lg:justify-self-end">
            Dual-pane speed, remote providers, comparison, sync, a viewer that shows the file, a map of where your disk went, and plugins that cannot reach past their permissions—all through the same governed core.
          </p>
        </div>

        <div className="mt-16 grid gap-4 md:grid-cols-2 lg:grid-cols-12">
          <article className="rounded-2xl border border-line bg-surface/70 p-6 lg:col-span-7 lg:p-8">
            <span className="font-mono text-[9px] uppercase tracking-[0.15em] text-phosphor">Universal VFS</span>
            <h3 className="mt-4 text-2xl font-medium tracking-[-0.04em]">Every location feels local.</h3>
            <p className="mt-3 max-w-xl text-sm leading-6 text-muted">
              Move between a disk, an SSH host, an FTP mirror, an S3 bucket or the inside of an archive without relearning the tool. Copies resume, cancelling leaves a marked partial and never an unmarked one.
            </p>
            <VfsMap />
          </article>

          <article className="rounded-2xl border border-line bg-surface/70 p-6 lg:col-span-5 lg:p-8">
            <span className="font-mono text-[9px] uppercase tracking-[0.15em] text-cyan">Compare + sync</span>
            <h3 className="mt-4 text-2xl font-medium tracking-[-0.04em]">See the delta. Approve the plan.</h3>
            <p className="mt-3 text-sm leading-6 text-muted">
              Stream huge comparisons with bounded memory, then turn the result into a reviewable, undoable sync—in either direction.
            </p>
            <DiffArtifact />
          </article>

          <article className="relative overflow-hidden rounded-2xl border border-line bg-[linear-gradient(135deg,#151a1d,#0b0f10)] p-6 lg:col-span-4 lg:p-8">
            <div className="absolute -right-12 -top-12 h-40 w-40 rounded-full bg-phosphor/[0.08] blur-3xl" />
            <span className="font-mono text-[9px] uppercase tracking-[0.15em] text-phosphor">Disk map</span>
            <h3 className="mt-4 text-2xl font-medium tracking-[-0.04em]">Where did my space go?</h3>
            <p className="mt-3 text-sm leading-6 text-muted">
              A listing sorted by size cannot answer that—a folder weighs its own node, not its contents. <code className="font-mono text-[12px] text-phosphor">alt+z</code> sizes every child by its whole subtree, and says so when it could not read one.
            </p>
            <DiskMap />
          </article>

          <article className="rounded-2xl border border-line bg-surface/70 p-6 lg:col-span-4 lg:p-8">
            <span className="font-mono text-[9px] uppercase tracking-[0.15em] text-cyan">The viewer</span>
            <h3 className="mt-4 text-2xl font-medium tracking-[-0.04em]">A picture is a picture.</h3>
            <p className="mt-3 text-sm leading-6 text-muted">
              In the terminal too: Norte asks your emulator once what it speaks and draws the image, rather than a hex dump. Markdown reads as text, code arrives highlighted, and the hex view is still one key away.
            </p>
            <ViewerArtifact />
          </article>

          <article className="rounded-2xl border border-line bg-surface/70 p-6 lg:col-span-4 lg:p-8">
            <span className="font-mono text-[9px] uppercase tracking-[0.15em] text-phosphor">WASM plugins</span>
            <h3 className="mt-4 text-2xl font-medium tracking-[-0.04em]">Extensible. Not exposed.</h3>
            <p className="mt-3 text-sm leading-6 text-muted">
              Eight kinds of extension, from a previewer to one that paints a whole panel. Each declares its capabilities and waits for your approval—installing is not consenting, and no permission means no syscall.
            </p>
            <PluginArtifact />
          </article>
        </div>
      </div>
    </section>
  );
}
