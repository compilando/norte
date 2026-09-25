import type { Copy } from "@/lib/i18n";
import { LINKS, REPO, RELEASE } from "@/lib/product";
import { Brand } from "./brand";

/** The footer's link groups name their targets; this is where they point. */
const TARGETS: Record<string, string> = {
  ...LINKS,
  repo: REPO,
  releases: RELEASE.all,
};

export function Footer({ t, home }: { t: Copy["footer"]; home: string }) {
  return (
    <footer id="docs" className="px-4 py-16 sm:px-8 lg:px-12 lg:py-20">
      <div className="mx-auto grid max-w-[1340px] gap-14 lg:grid-cols-[1fr_1.2fr]">
        <div>
          <a href={home} aria-label="norte"><Brand /></a>
          <p className="mt-5 max-w-sm text-sm leading-6 text-muted">{t.tagline}</p>
          <p className="mt-8 font-mono text-[9px] uppercase tracking-[0.12em] text-muted">{t.promise}</p>
          <p className="mt-2 font-mono text-[9px] uppercase tracking-[0.12em] text-muted/60">
            v{RELEASE.version} · protocol {RELEASE.protocol}
          </p>
        </div>
        <div className="grid grid-cols-2 gap-10 sm:grid-cols-3">
          {t.groups.map(([title, links]) => (
            <div key={title}>
              <h3 className="font-mono text-[9px] uppercase tracking-[0.15em] text-ink">{title}</h3>
              <ul className="mt-5 space-y-3">
                {links.map(([label, target]) => (
                  <li key={label}>
                    <a href={TARGETS[target] ?? target} className="text-sm text-muted transition-colors hover:text-ink">{label}</a>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </div>
      </div>
      <div className="mx-auto mt-16 flex max-w-[1340px] flex-col gap-4 border-t border-line pt-7 font-mono text-[8px] uppercase tracking-[0.11em] text-muted sm:flex-row sm:items-center sm:justify-between">
        <p>© {new Date().getFullYear()} norte contributors</p>
        <p>
          <a href={LINKS.licensing} className="transition-colors hover:text-ink">{t.license}</a>
        </p>
      </div>
    </footer>
  );
}
