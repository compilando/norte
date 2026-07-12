import { ButtonLink } from "./button-link";
import { Platforms } from "./platforms";

export function FinalCta() {
  return (
    <section id="download" className="px-5 py-24 sm:px-8 sm:py-32">
      <div className="mx-auto max-w-3xl text-center">
        <p className="font-mono text-xs uppercase tracking-[0.18em] text-phosphor">Your files. At full speed.</p>
        <h2 className="mt-6 text-4xl font-semibold tracking-[-0.045em] text-ink sm:text-6xl">Stop navigating.<br />Start commanding.</h2>
        <p className="mx-auto mt-6 max-w-xl text-lg leading-8 text-muted">A focused file commander for people who would rather move fast than reach for a mouse.</p>
        <ButtonLink href="#download" arrow className="mt-9 min-w-40">Download</ButtonLink>
        <div className="mt-6"><Platforms /></div>
      </div>
    </section>
  );
}
