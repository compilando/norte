const groups = [
  { title: "Product", links: ["Features", "Architecture", "Roadmap"] },
  { title: "Build", links: ["Documentation", "Plugin SDK", "Source code"] },
  { title: "Connect", links: ["Changelog", "Community", "Contact"] },
];

export function Footer() {
  return (
    <footer id="docs" className="border-t border-line py-14">
      <div className="mx-auto grid max-w-7xl gap-12 px-5 sm:px-8 lg:grid-cols-[1fr_1.4fr]">
        <div>
          <a href="#top" className="font-mono text-sm font-semibold text-ink">[NOMBRE]<span className="text-phosphor">_</span></a>
          <p className="mt-4 max-w-xs text-sm leading-6 text-muted">Command every file. Trust every move.</p>
        </div>
        <div className="grid grid-cols-2 gap-8 sm:grid-cols-3">
          {groups.map((group) => (
            <div key={group.title}>
              <h3 className="font-mono text-[11px] uppercase tracking-[0.16em] text-ink">{group.title}</h3>
              <ul className="mt-5 space-y-3">
                {group.links.map((link) => <li key={link}><a href="#" className="text-sm text-muted transition-colors hover:text-ink">{link}</a></li>)}
              </ul>
            </div>
          ))}
        </div>
      </div>
      <div className="mx-auto mt-14 flex max-w-7xl flex-col gap-4 border-t border-line px-5 pt-7 font-mono text-[11px] uppercase tracking-[0.1em] text-muted sm:flex-row sm:items-center sm:justify-between sm:px-8">
        <p>© {new Date().getFullYear()} [NOMBRE]. No telemetry. Ever.</p>
        <div className="flex gap-5"><a href="#" className="hover:text-ink">GitHub</a><a href="#" className="hover:text-ink">X / Twitter</a><a href="#" className="hover:text-ink">License</a></div>
      </div>
    </footer>
  );
}
