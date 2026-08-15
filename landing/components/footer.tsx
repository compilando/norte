import { Brand } from "./brand";

const groups = [
  { title: "Product", links: [["Capabilities", "#features"], ["AI workflows", "#ai"], ["Architecture", "#architecture"], ["Releases", "https://github.com/compilando/norte/releases"]] },
  { title: "Build", links: [["Documentation", "https://github.com/compilando/norte/tree/main/docs"], ["Contributing", "https://github.com/compilando/norte/blob/main/CONTRIBUTING.md"], ["Architecture map", "https://github.com/compilando/norte/blob/main/ARCHITECTURE.md"], ["Security", "https://github.com/compilando/norte/blob/main/SECURITY.md"]] },
  { title: "Open source", links: [["Source code", "https://github.com/compilando/norte"], ["Issues", "https://github.com/compilando/norte/issues"], ["Changelog", "https://github.com/compilando/norte/blob/main/CHANGELOG.md"], ["License", "https://github.com/compilando/norte#licensing"]] },
];

export function Footer() {
  return (
    <footer id="docs" className="px-5 py-16 sm:px-8 lg:px-12 lg:py-20">
      <div className="mx-auto grid max-w-[1340px] gap-14 lg:grid-cols-[1fr_1.2fr]">
        <div><a href="#top" aria-label="Back to top"><Brand /></a><p className="mt-5 max-w-sm text-sm leading-6 text-muted">The open-source file commander for people, terminals and agents.</p><p className="mt-8 font-mono text-[9px] uppercase tracking-[0.12em] text-muted">Built in Rust. No telemetry. Ever.</p></div>
        <div className="grid grid-cols-2 gap-10 sm:grid-cols-3">
          {groups.map((group) => <div key={group.title}><h3 className="font-mono text-[9px] uppercase tracking-[0.15em] text-ink">{group.title}</h3><ul className="mt-5 space-y-3">{group.links.map(([label, href]) => <li key={label}><a href={href} className="text-sm text-muted transition-colors hover:text-ink">{label}</a></li>)}</ul></div>)}
        </div>
      </div>
      <div className="mx-auto mt-16 flex max-w-[1340px] flex-col gap-4 border-t border-line pt-7 font-mono text-[8px] uppercase tracking-[0.11em] text-muted sm:flex-row sm:items-center sm:justify-between"><p>© {new Date().getFullYear()} Norte contributors</p><p>AGPL-3.0 · Protocol & providers MIT / Apache-2.0</p></div>
    </footer>
  );
}
