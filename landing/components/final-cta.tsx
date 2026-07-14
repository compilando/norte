import { ButtonLink } from "./button-link";
import { Platforms } from "./platforms";

export function FinalCta() {
  return (
    <section id="download" className="px-5 py-24 sm:px-8 sm:py-32">
      <div className="mx-auto max-w-3xl text-center">
        <p className="font-mono text-xs uppercase tracking-[0.18em] text-phosphor">Your files deserve a command center</p>
        <h2 className="mt-6 text-balance text-4xl font-semibold tracking-[-0.045em] text-ink sm:text-6xl">Move at full speed.<br /><span className="text-muted">Stay in control.</span></h2>
        <p className="mx-auto mt-6 max-w-xl text-lg leading-8 text-muted">One focused tool for every file, every destination, and every workflow ahead.</p>
        <ButtonLink href="#download" arrow className="mt-9 min-w-48">Download [NOMBRE]</ButtonLink>
        <div className="mt-6"><Platforms /></div>
      </div>
    </section>
  );
}
