import { ButtonLink } from "./button-link";
import { CommanderDemo } from "./commander-demo";
import { Platforms } from "./platforms";

export function Hero() {
  return (
    <section id="top" className="relative overflow-hidden pt-16">
      <div className="hero-grid pointer-events-none absolute inset-x-0 top-0 h-[720px] opacity-70" />
      <div className="relative mx-auto grid min-h-[820px] max-w-7xl items-center gap-16 px-5 py-24 sm:px-8 lg:grid-cols-[0.82fr_1.18fr] lg:gap-14 lg:py-20">
        <div className="max-w-xl">
          <div className="mb-7 flex items-center gap-3 font-mono text-[11px] uppercase tracking-[0.18em] text-phosphor">
            <span className="h-px w-7 bg-phosphor" />Built for flow
          </div>
          <h1 className="text-balance text-5xl font-semibold leading-[0.98] tracking-[-0.055em] text-ink sm:text-6xl lg:text-[68px]">
            The keyboard-first file commander.
          </h1>
          <p className="mt-7 max-w-lg text-lg leading-relaxed text-muted">
            Navigate, search, and move files at the speed of thought—without leaving the keyboard.
          </p>
          <div className="mt-9 flex flex-col gap-3 sm:flex-row">
            <ButtonLink href="#download" arrow className="sm:min-w-36">Download</ButtonLink>
            <ButtonLink id="source" href="#source" variant="secondary" className="sm:min-w-36">Clone source</ButtonLink>
          </div>
          <div className="mt-6"><Platforms /></div>
        </div>
        <CommanderDemo />
      </div>
    </section>
  );
}
