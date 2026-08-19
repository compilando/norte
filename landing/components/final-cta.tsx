import { ButtonLink } from "./button-link";

export function FinalCta() {
  return (
    <section id="download" className="px-3 pb-3 sm:px-5 sm:pb-5">
      <div className="noise relative overflow-hidden rounded-[28px] border border-white/[0.1] bg-[#0b100d] px-5 py-24 sm:px-8 sm:py-32 lg:py-40">
        <div className="absolute inset-0 bg-[linear-gradient(90deg,rgba(7,9,10,.96),rgba(7,9,10,.58),rgba(7,9,10,.2)),url('/norte-aurora.webp')] bg-cover bg-center opacity-80" />
        <div className="page-grid absolute inset-0 opacity-30 [mask-image:linear-gradient(to_right,black,transparent)]" />
        <div className="relative mx-auto max-w-[1340px]">
          <p className="font-mono text-[10px] uppercase tracking-[0.18em] text-phosphor">Your filesystem, headed somewhere</p>
          <h2 className="mt-7 max-w-5xl text-balance text-6xl font-medium leading-[.9] tracking-[-0.075em] text-ink sm:text-8xl lg:text-[112px]">Ready to point north?</h2>
          <p className="mt-8 max-w-xl text-base leading-7 text-ink/65 sm:text-lg">Install the Linux alpha today, or build Norte anywhere Rust runs. No account. No telemetry. Just your files—under control.</p>
          <div className="mt-9 flex flex-col gap-3 sm:flex-row"><ButtonLink href="https://github.com/compilando/norte/releases/latest" arrow>Get Norte</ButtonLink><ButtonLink href="https://github.com/compilando/norte#development" variant="secondary">Build from source</ButtonLink></div>

          <div className="mt-14 max-w-3xl overflow-hidden rounded-xl border border-white/[0.12] bg-black/45 backdrop-blur-xl">
            <div className="flex items-center border-b border-white/[0.09] px-4 py-3 font-mono text-[8px] uppercase tracking-[0.12em] text-muted"><span>Linux · x86_64</span><span className="ml-auto text-phosphor">v0.3.0-alpha</span></div>
            <div className="space-y-2 overflow-x-auto px-4 py-5 font-mono text-[9px] text-ink sm:px-5 sm:text-[11px]">
              <div className="whitespace-nowrap"><span className="mr-3 text-phosphor">$</span>curl --proto &apos;=https&apos; --tlsv1.2 -LsSf https://github.com/compilando/norte/releases/latest/download/norte-tui-installer.sh | sh</div>
              <div className="whitespace-nowrap"><span className="mr-3 text-phosphor">$</span>curl --proto &apos;=https&apos; --tlsv1.2 -LsSf https://github.com/compilando/norte/releases/latest/download/norte-cli-installer.sh | sh</div>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
