import { ButtonLink } from "./button-link";

export function Nav() {
  return (
    <header className="fixed inset-x-0 top-0 z-50 border-b border-white/[0.06] bg-base/75 backdrop-blur-xl">
      <nav className="mx-auto flex h-16 max-w-7xl items-center justify-between px-5 sm:px-8" aria-label="Main navigation">
        <a href="#top" className="flex items-center gap-2.5 font-mono text-sm font-semibold tracking-tight text-ink">
          <span className="grid h-7 w-7 place-items-center rounded-md border border-phosphor/40 bg-phosphor/10 text-phosphor" aria-hidden="true">›_</span>
          [NOMBRE]
        </a>
        <div className="hidden items-center gap-7 md:flex">
          <a href="#features" className="text-sm text-muted transition-colors hover:text-ink">Features</a>
          <a href="#docs" className="text-sm text-muted transition-colors hover:text-ink">Docs</a>
          <a href="#source" className="text-sm text-muted transition-colors hover:text-ink">GitHub</a>
        </div>
        <ButtonLink href="#download" className="h-9 px-4 text-xs">Download</ButtonLink>
      </nav>
    </header>
  );
}
