import { ButtonLink } from "./button-link";
import { CommanderDemo } from "./commander-demo";
import { Platforms } from "./platforms";

const proof = ["RUST CORE", "ZERO TELEMETRY", "OPEN PROTOCOL", "CROSS-PLATFORM"];

export function Hero() {
  return (
    <section id="top" className="relative overflow-hidden pt-16">
      <div className="hero-grid pointer-events-none absolute inset-x-0 top-0 h-[980px] opacity-80" />
      <div className="pointer-events-none absolute left-1/2 top-[350px] h-[600px] w-[900px] -translate-x-1/2 rounded-full bg-phosphor/[0.045] blur-[130px]" />
      <div className="relative mx-auto max-w-7xl px-5 pb-24 pt-24 sm:px-8 sm:pt-28 lg:pb-32">
        <div className="mx-auto max-w-5xl text-center">
          <div className="mb-7 inline-flex items-center gap-3 rounded-full border border-phosphor/20 bg-phosphor/[0.06] px-4 py-2 font-mono text-[11px] uppercase tracking-[0.16em] text-phosphor"><span className="h-1.5 w-1.5 rounded-full bg-phosphor shadow-[0_0_10px_#4ADE80]" />A new-generation file commander</div>
          <h1 className="text-balance text-5xl font-semibold leading-[0.92] tracking-[-0.065em] text-ink sm:text-7xl lg:text-[96px]">Command every file.<br /><span className="text-muted">Trust every move.</span></h1>
          <p className="mx-auto mt-8 max-w-2xl text-lg leading-8 text-muted sm:text-xl">Native speed. One universal filesystem. A journal behind every move. Built for the keyboard—and the agent era.</p>
          <div className="mt-9 flex flex-col justify-center gap-3 sm:flex-row">
            <ButtonLink href="#download" arrow className="sm:min-w-48">Download [NOMBRE]</ButtonLink>
            <ButtonLink id="source" href="#source" variant="secondary" className="sm:min-w-40">Clone source</ButtonLink>
          </div>
          <div className="mt-6"><Platforms /></div>
        </div>
        <div className="relative mx-auto mt-16 max-w-6xl sm:mt-20"><div className="absolute -inset-x-16 bottom-[-60px] top-1/3 -z-10 bg-[radial-gradient(ellipse_at_center,rgba(74,222,128,0.09),transparent_65%)]" /><CommanderDemo /></div>
        <div className="mx-auto mt-12 grid max-w-5xl grid-cols-2 border-y border-line/70 sm:grid-cols-4">
          {proof.map((item, index) => <div key={item} className={`py-4 text-center font-mono text-[11px] uppercase tracking-[0.12em] text-muted ${index > 0 ? "sm:border-l sm:border-line/70" : ""} ${index % 2 === 1 ? "border-l border-line/70" : ""} ${index > 1 ? "border-t border-line/70 sm:border-t-0" : ""}`}>{item}</div>)}
        </div>
      </div>
    </section>
  );
}
