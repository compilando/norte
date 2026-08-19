const principles = [
  ["01", "One namespace", "Local disks, remote servers, object storage and archives behave like one coherent filesystem."],
  ["02", "Work in motion", "Copies, indexes, comparisons and syncs run as observable, cancellable background tasks."],
  ["03", "Control stays human", "AI proposes. Agents request. The core enforces. Every mutation leaves a path back."],
];

export function Philosophy() {
  return (
    <section className="relative py-28 sm:py-36 lg:py-48">
      <div className="mx-auto max-w-[1440px] px-5 sm:px-8 lg:px-12">
        <div className="grid gap-12 lg:grid-cols-[.7fr_1.3fr] lg:gap-20">
          <div>
            <p className="font-mono text-[10px] uppercase tracking-[0.18em] text-phosphor">A different operating model</p>
            <p className="mt-5 max-w-xs text-sm leading-6 text-muted">The file explorer stopped evolving. Your work did not.</p>
          </div>
          <h2 className="text-balance text-4xl font-medium leading-[1.02] tracking-[-0.055em] text-ink sm:text-6xl lg:text-7xl">
            Not a prettier explorer. <span className="text-muted">A programmable, governed system for everything you keep.</span>
          </h2>
        </div>
        <div className="mt-20 grid border-y border-line lg:grid-cols-3">
          {principles.map(([number, title, body], index) => (
            <article key={number} className={`relative py-8 lg:px-8 lg:py-10 ${index > 0 ? "border-t border-line lg:border-l lg:border-t-0" : ""} ${index === 0 ? "lg:pl-0" : ""}`}>
              <div className="flex items-start gap-5"><span className="font-mono text-[9px] text-phosphor">{number}</span><div><h3 className="text-lg font-medium tracking-[-0.025em] text-ink">{title}</h3><p className="mt-3 max-w-sm text-sm leading-6 text-muted">{body}</p></div></div>
            </article>
          ))}
        </div>
      </div>
    </section>
  );
}
