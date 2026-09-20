import { RELEASE } from "@/lib/product";

const surfaces = [
  { name: "ntc", kind: "Terminal", command: "ntc ~/projects", copy: "The fastest path. Two panes, no mouse required, and the core running embedded—nothing to start first." },
  { name: "norte-gui", kind: "Window", command: "norte-gui ~/projects", copy: "A native desktop window over the same semantics, sharing your themes, keys, layouts and journal." },
  { name: "norte", kind: "CLI", command: "norte cp · sync · index · undo", copy: "Scriptable, composable commands for automation and headless work—plus the daemon, policy and doctor." },
  { name: "MCP", kind: "Agents", command: "norte mcp serve", copy: "A governed bridge for Claude, Codex and any MCP-compatible agent. Same core, same policy, same journal." },
];

export function Surfaces() {
  return (
    <section id="architecture" className="relative scroll-mt-16 overflow-hidden border-y border-line bg-[#090c0d] py-28 sm:py-36 lg:py-44">
      <div className="page-grid absolute inset-0 opacity-30 [mask-image:radial-gradient(circle_at_center,black,transparent_70%)]" />
      <div className="relative mx-auto max-w-[1440px] px-5 sm:px-8 lg:px-12">
        <div className="mx-auto max-w-4xl text-center">
          <p className="font-mono text-[10px] uppercase tracking-[0.17em] text-phosphor">One core · every surface</p>
          <h2 className="mt-6 text-balance text-5xl font-medium leading-[.96] tracking-[-0.065em] text-ink sm:text-7xl lg:text-8xl">Your workflow is the interface.</h2>
          <p className="mx-auto mt-7 max-w-2xl text-base leading-7 text-muted">
            Open the same file universe from one terminal or many. Run embedded, or share a daemon: the terminal, the window, the CLI and your agents speak one versioned protocol and inherit one policy.
          </p>
        </div>

        <div className="relative mx-auto mt-20 max-w-6xl">
          <div className="pointer-events-none absolute left-1/2 top-24 hidden h-20 w-[75%] -translate-x-1/2 border-x border-t border-line lg:block" />
          <div className="relative z-10 mx-auto grid h-40 w-40 place-items-center rounded-full border border-phosphor/35 bg-[#0d1210] shadow-[0_0_80px_rgba(183,255,82,.11)]">
            <div className="absolute inset-3 rounded-full border border-dashed border-phosphor/20" />
            <div className="text-center">
              <span className="font-mono text-[9px] uppercase tracking-[0.16em] text-muted">Rust</span>
              <strong className="mt-1 block text-xl font-medium text-ink">norte core</strong>
              <span className="mt-2 inline-flex items-center gap-1.5 font-mono text-[8px] text-phosphor">
                <i className="h-1 w-1 rounded-full bg-phosphor" /> healthy
              </span>
            </div>
          </div>
          <div className="mt-14 grid gap-3 sm:grid-cols-2 lg:mt-20 lg:grid-cols-4">
            {surfaces.map((surface, index) => (
              <article key={surface.name} className="group relative rounded-2xl border border-line bg-surface/80 p-5 transition duration-300 hover:-translate-y-1 hover:border-muted/60 lg:p-6">
                <div className="flex items-center justify-between">
                  <span className="font-mono text-[9px] text-muted">0{index + 1}</span>
                  <span className="font-mono text-[8px] uppercase tracking-[0.12em] text-phosphor/70">{surface.kind}</span>
                </div>
                <h3 className="mt-5 text-2xl font-medium tracking-[-0.05em] text-ink lg:text-[28px]">{surface.name}</h3>
                <code className="mt-3 block truncate font-mono text-[9px] text-phosphor">$ {surface.command}</code>
                <p className="mt-6 text-sm leading-6 text-muted">{surface.copy}</p>
              </article>
            ))}
          </div>
        </div>

        <div className="mx-auto mt-10 grid max-w-6xl grid-cols-2 gap-px overflow-hidden rounded-xl border border-line bg-line font-mono text-[8px] uppercase tracking-[0.1em] text-muted sm:grid-cols-4">
          {[`JSON-RPC wire ${RELEASE.protocol}`, "Shared configuration", "Same journal", "Policy enforced in core"].map((item) => (
            <span key={item} className="bg-base px-3 py-4 text-center">{item}</span>
          ))}
        </div>
      </div>
    </section>
  );
}
