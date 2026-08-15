import { Brand } from "./brand";

export function Nav() {
  return (
    <header className="fixed inset-x-0 top-0 z-50 border-b border-white/[0.06] bg-base/70 backdrop-blur-2xl">
      <nav className="mx-auto flex h-[68px] max-w-[1440px] items-center justify-between px-5 sm:px-8 lg:px-12" aria-label="Main navigation">
        <a href="#top" aria-label="Norte home"><Brand /></a>
        <div className="hidden items-center gap-8 md:flex">
          <a href="#product" className="text-[13px] text-muted transition-colors hover:text-ink">Product</a>
          <a href="#ai" className="text-[13px] text-muted transition-colors hover:text-ink">AI</a>
          <a href="#architecture" className="text-[13px] text-muted transition-colors hover:text-ink">Architecture</a>
          <a href="https://github.com/compilando/norte#documentation" className="text-[13px] text-muted transition-colors hover:text-ink">Docs</a>
        </div>
        <a href="https://github.com/compilando/norte" className="group inline-flex h-9 items-center gap-2 rounded-full border border-line bg-white/[0.03] px-4 font-mono text-[11px] uppercase tracking-[0.08em] text-ink transition hover:border-muted/70 hover:bg-white/[0.06]">
          <span className="hidden sm:inline">View on</span> GitHub
          <span className="text-phosphor transition-transform group-hover:translate-x-0.5">↗</span>
        </a>
      </nav>
    </header>
  );
}
