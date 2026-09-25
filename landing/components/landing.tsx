import type { ReactNode } from "react";
import type { Packed } from "@/lib/ansi";
import { COPY, fill, type Lang, type Scene } from "@/lib/i18n";
import { COMMANDS, LINKS, PRESETS, RELEASE, THEMES } from "@/lib/product";
import { COLS, guiShot, presetKeys, themeAccent, tuiReel, tuiScene, tuiTheme } from "@/lib/shots";
import { AppFrame } from "./app-frame";
import { ButtonLink } from "./button-link";
import { FinalCta } from "./final-cta";
import { Footer } from "./footer";
import { HeroStage } from "./hero-stage";
import { Nav } from "./nav";
import { SignalStrip } from "./signal-strip";
import { TerminalScreen } from "./terminal-screen";
import { ThemeCard } from "./theme-card";
import { Tour } from "./tour";

function Eyebrow({ children }: { children: ReactNode }) {
  return <p className="font-mono text-[10px] uppercase tracking-[0.18em] text-phosphor">{children}</p>;
}

function H2({ children }: { children: ReactNode }) {
  return (
    <h2 className="mt-5 max-w-4xl text-balance text-4xl font-medium leading-[1.02] tracking-[-0.055em] text-ink sm:text-6xl">
      {children}
    </h2>
  );
}

function Section({ id, children, className = "" }: { id?: string; children: ReactNode; className?: string }) {
  return (
    <section id={id} className={`relative scroll-mt-20 py-24 sm:py-32 ${className}`}>
      <div className="mx-auto max-w-[1440px] px-4 sm:px-8 lg:px-12">{children}</div>
    </section>
  );
}

export function Landing({ lang }: { lang: Lang }) {
  const t = COPY[lang];
  const home = lang === "en" ? "/" : "/es";
  const screen = (scene: Scene | "grant", label: string) => {
    const s = tuiScene(lang, scene);
    return s ? <TerminalScreen screen={s} cols={COLS} label={label} /> : null;
  };

  // The hero: the clip, then one still per theme.
  const reel = tuiReel(lang);
  const themes = THEMES.flatMap((id) => {
    const s = tuiTheme(lang, id);
    if (!s) return [];
    return [{ id, screen: s, swatch: [s.bg ?? "#000", themeAccent(id) ?? s.fg ?? "#fff"] as [string, string] }];
  });
  const windows = THEMES.flatMap((id) => {
    const src = guiShot(lang, `panes-${id}`);
    return src ? [{ id, src }] : [];
  });

  const screens: Record<string, Packed | null> = {};
  for (const step of t.tour.steps) screens[step.scene] = tuiScene(lang, step.scene);
  const steps = t.tour.steps.map((s) => ({ ...s, body: fill(s.body, { commands: COMMANDS }) }));

  const commands = Object.keys(t.keys.commands);
  const keys = presetKeys(commands);
  const guiPanes = guiShot(lang, "panes-catppuccin-mocha") ?? windows[0]?.src ?? null;
  const tuiPanes = tuiScene(lang, "panes");

  return (
    <main className="min-h-screen overflow-x-clip bg-base text-ink">
      <Nav t={t.nav} home={home} />

      {/* Hero */}
      <section id="top" className="noise relative overflow-hidden pt-[68px]">
        <div className="aurora-bg pointer-events-none absolute inset-x-0 top-0 h-[860px] opacity-95" />
        <div className="page-grid pointer-events-none absolute inset-x-0 top-0 h-[1080px] opacity-45 [mask-image:linear-gradient(to_bottom,black,transparent_78%)]" />
        <div className="relative mx-auto max-w-[1440px] px-4 pb-20 pt-14 sm:px-8 sm:pt-20 lg:px-12 lg:pb-28 lg:pt-20">
          <div className="grid items-end gap-10 lg:grid-cols-[1.35fr_.65fr]">
            <div className="max-w-5xl">
              <div className="mb-7 inline-flex flex-wrap items-center gap-x-3 gap-y-2 rounded-full border border-white/[0.12] bg-black/20 px-3.5 py-2 font-mono text-[9px] uppercase tracking-[0.16em] text-ink/80 backdrop-blur-xl sm:text-[10px]">
                <span className="flex items-center gap-2">
                  <span className="h-1.5 w-1.5 rounded-full bg-phosphor shadow-[0_0_9px_#B7FF52]" />
                  {RELEASE.version}
                </span>
                <span className="text-line">/</span>
                <span className="text-muted">{t.hero.protocol} {RELEASE.protocol}</span>
                <span className="text-line">/</span>
                <span className="text-muted">{t.hero.badge}</span>
              </div>
              <h1 className="max-w-[1000px] text-balance text-[48px] font-medium leading-[0.92] tracking-[-0.075em] text-ink sm:text-[76px] lg:text-[100px] xl:text-[112px]">
                {t.hero.title[0]}
                <span className="text-phosphor">{t.hero.title[1]}</span>
              </h1>
            </div>
            <div className="rounded-2xl border border-white/[0.1] bg-black/35 p-5 backdrop-blur-md lg:mb-4 lg:bg-black/25">
              <p className="max-w-lg text-balance text-base leading-7 sm:text-lg sm:leading-8" style={{ color: "#d8ded9" }}>
                {t.hero.lede}
              </p>
              <div className="mt-7 flex flex-col gap-3 sm:flex-row lg:flex-col xl:flex-row">
                <ButtonLink href="#download" arrow>{t.hero.primary}</ButtonLink>
                <ButtonLink href={LINKS.readme} variant="secondary">{t.hero.secondary}</ButtonLink>
              </div>
              <p className="mt-5 font-mono text-[9px] uppercase tracking-[0.1em] text-[#9aa39c]">{t.hero.platforms}</p>
            </div>
          </div>

          <div id="stage" className="mx-auto mt-12 max-w-[1240px] scroll-mt-24 sm:mt-14">
            <HeroStage
              reel={reel}
              themes={themes}
              cols={COLS}
              windows={windows}
              labels={{ ...t.hero.tabs, theme: t.hero.theme, live: t.hero.live, play: t.hero.play, pause: t.hero.pause }}
            />
            <p className="mt-6 text-center font-mono text-[10px] uppercase tracking-[0.1em] text-muted">
              <span className="text-phosphor">●</span> {t.hero.caption}
            </p>
          </div>
        </div>
      </section>

      <SignalStrip signals={t.signals} />

      {/* Two frontends */}
      <Section>
        <div className="grid gap-10 lg:grid-cols-[.8fr_1.2fr] lg:items-end">
          <div>
            <Eyebrow>{t.duo.eyebrow}</Eyebrow>
            <H2>{t.duo.title}</H2>
          </div>
          <p className="max-w-xl text-lg leading-8 text-ink/65">{t.duo.body}</p>
        </div>
        <div className="mt-14 grid gap-6 lg:grid-cols-2">
          <figure className="rise">
            <AppFrame title="ntc">{tuiPanes && <TerminalScreen screen={tuiPanes} cols={COLS} label={t.duo.terminal} />}</AppFrame>
            <figcaption className="mt-3 font-mono text-[10px] text-muted">{t.duo.terminal}</figcaption>
          </figure>
          <figure className="rise">
            <AppFrame title="norte-gui">
              {guiPanes ? (
                // eslint-disable-next-line @next/next/no-img-element -- a capture, served as-is
                <img src={guiPanes} alt={t.duo.window} className="block w-full" />
              ) : (
                <div className="grid aspect-[132/38] place-items-center p-6 text-center font-mono text-[11px] text-muted">{t.duo.missing}</div>
              )}
            </AppFrame>
            <figcaption className="mt-3 font-mono text-[10px] text-muted">{t.duo.window}</figcaption>
          </figure>
        </div>
      </Section>

      {/* Tour */}
      <Section id="tour" className="border-t border-line/60">
        <Eyebrow>{t.tour.eyebrow}</Eyebrow>
        <H2>{t.tour.title}</H2>
        <div className="mt-8">
          <Tour steps={steps} screens={screens} cols={COLS} />
        </div>
      </Section>

      {/* What's new */}
      <Section id="new" className="border-t border-line/60">
        <Eyebrow>{fill(t.news.eyebrow, { version: RELEASE.label })}</Eyebrow>
        <H2>{t.news.title}</H2>
        <div className="mt-14 grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
          {t.news.items.map(([title, body, tag], i) => (
            <article
              key={title}
              className={`rise sheen relative overflow-hidden rounded-2xl border border-white/[0.09] bg-surface p-6 transition hover:border-phosphor/40 ${
                i === 0 || i === 5 ? "lg:col-span-2" : ""
              }`}
            >
              <p className="font-mono text-[10px] text-phosphor">{tag}</p>
              <h3 className="mt-6 text-xl font-medium tracking-[-0.03em] text-ink">{title}</h3>
              <p className="mt-3 text-sm leading-6 text-muted">{body}</p>
            </article>
          ))}
        </div>
      </Section>

      {/* Themes */}
      <Section id="themes" className="border-t border-line/60">
        <div className="grid gap-10 lg:grid-cols-[.8fr_1.2fr] lg:items-end">
          <div>
            <Eyebrow>{t.themes.eyebrow}</Eyebrow>
            <H2>{t.themes.title}</H2>
          </div>
          <div>
            <p className="max-w-xl text-lg leading-8 text-ink/65">{t.themes.body}</p>
            <p className="mt-3 font-mono text-[10px] uppercase tracking-[0.1em] text-muted">{t.themes.pick}</p>
          </div>
        </div>
        <div className="mt-14 grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-5">
          {themes.map(({ id, screen: s }) => (
            <div key={id} className="rise">
              <ThemeCard id={id}>
                <TerminalScreen screen={s} cols={COLS} label={id} />
              </ThemeCard>
            </div>
          ))}
        </div>
      </Section>

      {/* Keys */}
      <Section id="keys" className="border-t border-line/60">
        <div className="grid gap-10 lg:grid-cols-[.8fr_1.2fr] lg:items-end">
          <div>
            <Eyebrow>{t.keys.eyebrow}</Eyebrow>
            <H2>{t.keys.title}</H2>
          </div>
          <p className="max-w-xl text-lg leading-8 text-ink/65">{t.keys.body}</p>
        </div>
        <div className="mt-14 overflow-x-auto rounded-2xl border border-white/[0.09] bg-surface">
          <table className="w-full min-w-[760px] border-collapse text-left">
            <thead>
              <tr className="border-b border-white/[0.09]">
                <th className="px-5 py-4 font-mono text-[10px] font-normal uppercase tracking-[0.12em] text-muted">{t.keys.command}</th>
                {PRESETS.map((p) => (
                  <th key={p} className="px-3 py-4 font-mono text-[11px] font-medium text-ink">{p}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {commands.map((cmd) => (
                <tr key={cmd} className="border-b border-white/[0.05] last:border-0 hover:bg-white/[0.02]">
                  <td className="px-5 py-3 text-sm text-ink/80">{t.keys.commands[cmd]}</td>
                  {PRESETS.map((p) => (
                    <td key={p} className="px-3 py-3">
                      {keys[p][cmd] ? (
                        <kbd className="rounded border border-white/[0.12] bg-white/[0.04] px-1.5 py-0.5 font-mono text-[11px] text-ink/85">{keys[p][cmd]}</kbd>
                      ) : (
                        <span className="text-muted/50">—</span>
                      )}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </Section>

      {/* Agents */}
      <Section id="agents" className="border-t border-line/60">
        <div className="grid gap-12 lg:grid-cols-2 lg:gap-16">
          <div>
            <Eyebrow>{t.agents.eyebrow}</Eyebrow>
            <H2>{t.agents.title}</H2>
            <p className="mt-6 max-w-xl text-lg leading-8 text-ink/65">{t.agents.body}</p>
            <div className="mt-10 space-y-6">
              {t.agents.points.map(([title, body], i) => (
                <div key={title} className="flex gap-5 border-t border-line pt-6">
                  <span className="font-mono text-[10px] text-phosphor">{String(i + 1).padStart(2, "0")}</span>
                  <div>
                    <h3 className="text-lg font-medium tracking-[-0.025em] text-ink">{title}</h3>
                    <p className="mt-2 max-w-md text-sm leading-6 text-muted">{body}</p>
                  </div>
                </div>
              ))}
            </div>
          </div>
          <figure className="rise self-center">
            <AppFrame title="ada@norte — F12">{screen("grant", t.agents.grant)}</AppFrame>
            <figcaption className="mt-3 font-mono text-[10px] text-muted">{t.agents.grant}</figcaption>
          </figure>
        </div>
      </Section>

      {/* Principles */}
      <Section className="border-t border-line/60">
        <div className="grid gap-12 lg:grid-cols-[.7fr_1.3fr] lg:gap-20">
          <div>
            <Eyebrow>{t.principles.eyebrow}</Eyebrow>
            <p className="mt-5 max-w-xs text-sm leading-6 text-muted">{t.principles.lead}</p>
          </div>
          <h2 className="text-balance text-4xl font-medium leading-[1.02] tracking-[-0.055em] text-ink sm:text-6xl lg:text-7xl">
            {t.principles.title}
            <span className="text-muted">{t.principles.muted}</span>
          </h2>
        </div>
        <div className="mt-20 grid border-y border-line sm:grid-cols-2 lg:grid-cols-4">
          {t.principles.items.map(([n, title, body], i) => (
            <article key={n} className={`py-8 sm:px-6 lg:py-10 ${i > 0 ? "border-t border-line sm:border-t-0 sm:border-l" : ""} ${i === 0 ? "sm:pl-0" : ""}`}>
              <span className="font-mono text-[9px] text-phosphor">{n}</span>
              <h3 className="mt-3 text-lg font-medium tracking-[-0.025em] text-ink">{title}</h3>
              <p className="mt-3 text-sm leading-6 text-muted">{body}</p>
            </article>
          ))}
        </div>
      </Section>

      <FinalCta t={t.cta} />
      <Footer t={t.footer} home={home} />
    </main>
  );
}
