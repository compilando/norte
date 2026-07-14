const locations = [["LOCAL", "~/work/norte"], ["REMOTE", "sftp://prod"], ["OBJECT", "s3://releases"], ["ARCHIVE", "backup.tar.zst"]];

export function Manifesto() {
  return (
    <section className="relative overflow-hidden border-b border-line/70 py-28 sm:py-40">
      <div className="pointer-events-none absolute left-1/2 top-1/2 h-[520px] w-[520px] -translate-x-1/2 -translate-y-1/2 rounded-full bg-phosphor/[0.035] blur-[100px]" />
      <div className="relative mx-auto max-w-7xl px-5 sm:px-8">
        <p className="font-mono text-xs uppercase tracking-[0.2em] text-phosphor">The category has changed</p>
        <h2 className="mt-7 max-w-5xl text-balance text-5xl font-semibold leading-[0.98] tracking-[-0.055em] text-ink sm:text-7xl lg:text-[92px]">Files live everywhere. <span className="text-muted">Your flow shouldn&apos;t notice.</span></h2>
        <div className="mt-16 grid border-y border-line sm:grid-cols-2 lg:grid-cols-4">
          {locations.map(([type, path], index) => (
            <div key={type} className={`group py-6 sm:px-6 ${index > 0 ? "lg:border-l lg:border-line" : ""} ${index % 2 === 1 ? "sm:border-l sm:border-line" : ""} ${index > 1 ? "border-t border-line lg:border-t-0" : ""}`}>
              <div className="flex items-center justify-between font-mono text-[11px] tracking-[0.14em] text-muted/70"><span>0{index + 1}</span><span>{type}</span></div>
              <p className="mt-8 truncate font-mono text-sm text-muted transition-colors duration-200 group-hover:text-phosphor">{path}</p>
            </div>
          ))}
        </div>
        <p className="mt-8 max-w-2xl text-lg leading-8 text-muted">[NOMBRE] turns every location into a provider and every operation into the same command. Learn one interface. Command the whole filesystem.</p>
      </div>
    </section>
  );
}
