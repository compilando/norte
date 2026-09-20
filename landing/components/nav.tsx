import { REPO } from "@/lib/product";
import { Brand } from "./brand";

const sections = [
  ["Product", "#product"],
  ["Surfaces", "#surfaces"],
  ["AI", "#ai"],
  ["Make it yours", "#yours"],
  ["Architecture", "#architecture"],
];

export function Nav() {
  return (
    <header className="fixed inset-x-0 top-0 z-50 border-b border-white/[0.06] bg-base/70 backdrop-blur-2xl">
      <nav className="mx-auto flex h-[68px] max-w-[1440px] items-center justify-between px-5 sm:px-8 lg:px-12" aria-label="Main navigation">
        <a href="#top" aria-label="Norte home"><Brand /></a>
        <div className="hidden items-center gap-7 lg:flex">
          {sections.map(([label, href]) => (
            <a key={href} href={href} className="text-[13px] text-muted transition-colors hover:text-ink">{label}</a>
          ))}
        </div>
        <div className="flex items-center gap-2.5">
          <a
            href="#download"
            className="hidden h-9 items-center rounded-full bg-phosphor px-4 font-mono text-[11px] font-semibold uppercase tracking-[0.08em] text-[#0a1008] transition hover:bg-[#c6ff74] sm:inline-flex"
          >
            Download
          </a>
          <a
            href={REPO}
            className="group inline-flex h-9 items-center gap-2 rounded-full border border-line bg-white/[0.03] px-4 font-mono text-[11px] uppercase tracking-[0.08em] text-ink transition hover:border-muted/70 hover:bg-white/[0.06]"
          >
            GitHub
            <span className="text-phosphor transition-transform group-hover:translate-x-0.5">↗</span>
          </a>
        </div>
      </nav>
    </header>
  );
}
