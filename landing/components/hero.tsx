import { ButtonLink } from "./button-link";
import { ProductStage } from "./product-stage";

export function Hero() {
  return (
    <section id="top" className="noise relative overflow-hidden pt-[68px]">
      <div className="aurora-bg pointer-events-none absolute inset-x-0 top-0 h-[860px] opacity-95" />
      <div className="page-grid pointer-events-none absolute inset-x-0 top-0 h-[1080px] opacity-45 [mask-image:linear-gradient(to_bottom,black,transparent_78%)]" />
      <div className="relative mx-auto max-w-[1440px] px-5 pb-24 pt-24 sm:px-8 sm:pt-32 lg:px-12 lg:pb-36 lg:pt-36">
        <div className="grid items-end gap-12 lg:grid-cols-[1.35fr_.65fr]">
          <div className="max-w-5xl">
            <div className="mb-7 inline-flex items-center gap-3 rounded-full border border-white/[0.12] bg-black/20 px-3.5 py-2 font-mono text-[9px] uppercase tracking-[0.16em] text-ink/80 backdrop-blur-xl sm:text-[10px]">
              <span className="h-1.5 w-1.5 rounded-full bg-phosphor shadow-[0_0_9px_#B7FF52]" />
              v0.3 alpha · open source
            </div>
            <h1 className="max-w-[1000px] text-balance text-[52px] font-medium leading-[0.92] tracking-[-0.075em] text-ink sm:text-[76px] lg:text-[104px] xl:text-[118px]">
              Your files have a new sense of <span className="text-phosphor">direction.</span>
            </h1>
          </div>
          <div className="rounded-2xl border border-white/[0.1] bg-black/35 p-5 backdrop-blur-md lg:mb-4 lg:bg-black/25">
            <p className="max-w-lg text-balance text-base leading-7 sm:text-lg sm:leading-8" style={{ color: "#d8ded9" }}>
              Norte is the open-source, AI-native file commander. One asynchronous Rust core for every terminal, screen and agent.
            </p>
            <div className="mt-7 flex flex-col gap-3 sm:flex-row lg:flex-col xl:flex-row">
              <ButtonLink href="https://github.com/compilando/norte/releases/latest" arrow>Install the alpha</ButtonLink>
              <ButtonLink href="https://github.com/compilando/norte" variant="secondary">Explore source</ButtonLink>
            </div>
            <p className="mt-5 font-mono text-[9px] uppercase tracking-[0.1em] text-[#9aa39c]">Linux x86_64 · macOS / Windows from source</p>
          </div>
        </div>

        <div id="product" className="mt-20 scroll-mt-24 sm:mt-28 lg:mt-32">
          <ProductStage />
        </div>

        <div className="mx-auto mt-8 flex max-w-[1180px] flex-col justify-between gap-3 px-1 font-mono text-[9px] uppercase tracking-[0.11em] text-muted sm:flex-row sm:items-center">
          <span className="flex items-center gap-2"><span className="text-phosphor">Live product model</span><span className="text-line">/</span> choose a mode above</span>
          <span>Local · SFTP · S3 · ZIP/TAR · FTP plugin</span>
        </div>
      </div>
    </section>
  );
}
