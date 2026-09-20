import { LINKS, REPO, RELEASE } from "@/lib/product";
import { Brand } from "./brand";

const groups = [
  {
    title: "Product",
    links: [
      ["Surfaces", "#surfaces"],
      ["AI workflows", "#ai"],
      ["Capabilities", "#features"],
      ["Make it yours", "#yours"],
      ["Download", "#download"],
    ],
  },
  {
    title: "Build",
    links: [
      ["Documentation", LINKS.docs],
      ["Architecture map", LINKS.architecture],
      ["Specification", LINKS.spec],
      ["Decision records", LINKS.adr],
      ["Plugin authoring", LINKS.plugins],
    ],
  },
  {
    title: "Open source",
    links: [
      ["Source code", REPO],
      ["Releases", RELEASE.all],
      ["Changelog", LINKS.changelog],
      ["Contributing", LINKS.contributing],
      ["Security", LINKS.security],
    ],
  },
];

export function Footer() {
  return (
    <footer id="docs" className="px-5 py-16 sm:px-8 lg:px-12 lg:py-20">
      <div className="mx-auto grid max-w-[1340px] gap-14 lg:grid-cols-[1fr_1.2fr]">
        <div>
          <a href="#top" aria-label="Back to top"><Brand /></a>
          <p className="mt-5 max-w-sm text-sm leading-6 text-muted">The open-source file commander for people, terminals, windows and agents.</p>
          <p className="mt-8 font-mono text-[9px] uppercase tracking-[0.12em] text-muted">Built in Rust. No telemetry. Ever.</p>
          <p className="mt-2 font-mono text-[9px] uppercase tracking-[0.12em] text-muted/60">
            v{RELEASE.version} · protocol {RELEASE.protocol}
          </p>
        </div>
        <div className="grid grid-cols-2 gap-10 sm:grid-cols-3">
          {groups.map((group) => (
            <div key={group.title}>
              <h3 className="font-mono text-[9px] uppercase tracking-[0.15em] text-ink">{group.title}</h3>
              <ul className="mt-5 space-y-3">
                {group.links.map(([label, href]) => (
                  <li key={label}>
                    <a href={href} className="text-sm text-muted transition-colors hover:text-ink">{label}</a>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </div>
      </div>
      <div className="mx-auto mt-16 flex max-w-[1340px] flex-col gap-4 border-t border-line pt-7 font-mono text-[8px] uppercase tracking-[0.11em] text-muted sm:flex-row sm:items-center sm:justify-between">
        <p>© {new Date().getFullYear()} Norte contributors</p>
        <p>
          <a href={LINKS.licensing} className="transition-colors hover:text-ink">AGPL-3.0 · Protocol &amp; providers MIT / Apache-2.0</a>
        </p>
      </div>
    </footer>
  );
}
