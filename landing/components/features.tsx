import { LineIcon, type IconName } from "./icons";

const features: { icon: IconName; title: string; body: string }[] = [
  { icon: "bolt", title: "Blazing fast", body: "A native core keeps every action instant, even across massive directories." },
  { icon: "keyboard", title: "Keyboard-first", body: "Every operation is one shortcut away. Your hands never leave the keys." },
  { icon: "panels", title: "Dual-pane", body: "See source and destination at once. Copy and move with total confidence." },
  { icon: "search", title: "Fuzzy search", body: "Find any file in milliseconds with smart, typo-tolerant matching." },
  { icon: "blocks", title: "Extensible", body: "Shape the commander around your workflow with scripts and plugins." },
  { icon: "branch", title: "Git-aware", body: "See branch and file status directly where you make changes." },
];

export function Features() {
  return (
    <section id="features" className="border-y border-line/70 bg-surface/30 py-24 sm:py-32">
      <div className="mx-auto max-w-7xl px-5 sm:px-8">
        <div className="mb-14 max-w-2xl">
          <p className="font-mono text-xs uppercase tracking-[0.18em] text-phosphor">Everything you need. Nothing you don’t.</p>
          <h2 className="mt-5 text-3xl font-semibold tracking-[-0.035em] text-ink sm:text-5xl">Built to stay out of your way.</h2>
        </div>
        <div className="grid overflow-hidden rounded-xl border border-line sm:grid-cols-2 lg:grid-cols-3">
          {features.map((feature, index) => (
            <article key={feature.title} className={`group min-h-56 border-line bg-base/40 p-7 transition-colors duration-200 hover:bg-surface ${index < 3 ? "lg:border-b" : ""} ${index % 3 !== 2 ? "lg:border-r" : ""} ${index < 4 ? "max-lg:border-b" : ""} ${index % 2 === 0 ? "sm:border-r lg:border-r" : ""}`}>
              <LineIcon name={feature.icon} className="h-7 w-7 text-muted transition-colors duration-200 group-hover:text-phosphor" />
              <h3 className="mt-10 font-mono text-base font-semibold text-ink">{feature.title}</h3>
              <p className="mt-3 text-sm leading-6 text-muted">{feature.body}</p>
            </article>
          ))}
        </div>
      </div>
    </section>
  );
}
