export function Speed() {
  return (
    <section className="py-24 sm:py-32">
      <div className="mx-auto max-w-7xl px-5 sm:px-8">
        <div className="relative overflow-hidden rounded-xl border border-line bg-surface px-6 py-16 sm:px-12 sm:py-20 lg:flex lg:items-end lg:justify-between">
          <div className="absolute right-0 top-0 h-px w-1/2 bg-gradient-to-l from-phosphor/70 to-transparent" />
          <div>
            <p className="font-mono text-xs uppercase tracking-[0.18em] text-phosphor">Measured in milliseconds</p>
            <p className="mt-7 font-mono text-[64px] font-medium leading-none tracking-[-0.07em] text-ink sm:text-[100px]">&lt;10<span className="ml-3 text-2xl tracking-normal text-muted sm:text-3xl">ms</span></p>
            <p className="mt-5 text-lg text-muted">to open any directory.</p>
          </div>
          <div className="mt-12 max-w-sm border-l border-line pl-6 lg:mt-0">
            <p className="text-base leading-7 text-muted">No splash screens. No indexing delay. No waiting for the interface to catch up.</p>
            <div className="mt-6 flex items-center gap-3 font-mono text-[11px] uppercase tracking-[0.14em] text-muted/70"><span className="h-1.5 w-1.5 rounded-full bg-phosphor" />Cold start benchmark</div>
          </div>
        </div>
      </div>
    </section>
  );
}
