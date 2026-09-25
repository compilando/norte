"use client";

import { useState } from "react";
import type { Copy } from "@/lib/i18n";
import { LINKS, RELEASE } from "@/lib/product";
import { ButtonLink } from "./button-link";

function CopyRow({ label, command, t }: { label: string; command: string; t: Copy["cta"] }) {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1800);
    } catch {
      setCopied(false);
    }
  };

  return (
    <div className="border-t border-white/[0.08] first:border-t-0">
      <p className="px-4 pt-3 font-mono text-[8px] uppercase tracking-[0.12em] text-muted sm:px-5">{label}</p>
      <div className="flex items-start gap-3 px-4 pb-3 pt-1.5 sm:px-5">
        <code className="min-w-0 flex-1 overflow-x-auto whitespace-nowrap font-mono text-[9px] text-ink sm:text-[11px]">
          <span className="mr-3 text-phosphor">$</span>
          {command}
        </code>
        <button
          type="button"
          onClick={copy}
          className="shrink-0 rounded-md border border-white/[0.14] px-2.5 py-1 font-mono text-[8px] uppercase tracking-[0.1em] text-muted transition hover:border-phosphor/60 hover:text-phosphor"
        >
          {copied ? t.copied : t.copy}
        </button>
      </div>
    </div>
  );
}

export function FinalCta({ t }: { t: Copy["cta"] }) {
  const installers: [string, string][] = [
    [t.tui, `curl --proto '=https' --tlsv1.2 -LsSf ${RELEASE.tuiInstaller} | sh`],
    [t.cli, `curl --proto '=https' --tlsv1.2 -LsSf ${RELEASE.cliInstaller} | sh`],
  ];
  return (
    <section id="download" className="scroll-mt-20 px-3 pb-3 sm:px-5 sm:pb-5">
      <div className="noise relative overflow-hidden rounded-[28px] border border-white/[0.1] bg-[#0b100d] px-4 py-24 sm:px-8 sm:py-28 lg:py-32">
        <div className="absolute inset-0 bg-[linear-gradient(90deg,rgba(7,9,10,.96),rgba(7,9,10,.58),rgba(7,9,10,.2)),url('/norte-aurora.webp')] bg-cover bg-center opacity-80" />
        <div className="page-grid absolute inset-0 opacity-30 [mask-image:linear-gradient(to_right,black,transparent)]" />

        <div className="relative mx-auto max-w-[1340px]">
          <p className="font-mono text-[10px] uppercase tracking-[0.18em] text-phosphor">{t.eyebrow}</p>
          <h2 className="mt-7 max-w-5xl text-balance text-5xl font-medium leading-[.9] tracking-[-0.075em] text-ink sm:text-8xl lg:text-[112px]">
            {t.title}
          </h2>

          <div className="mt-12 grid gap-8 lg:grid-cols-[1.15fr_.85fr] lg:gap-12">
            <div className="min-w-0">
              <p className="max-w-xl text-base leading-7 text-ink/65 sm:text-lg">{t.body}</p>

              <div className="mt-8 overflow-hidden rounded-xl border border-white/[0.12] bg-black/45 backdrop-blur-xl">
                <div className="flex items-center border-b border-white/[0.09] px-4 py-3 font-mono text-[8px] uppercase tracking-[0.12em] text-muted sm:px-5">
                  <span>{t.terminal}</span>
                  <span className="ml-auto text-phosphor">v{RELEASE.version}</span>
                </div>
                {installers.map(([label, command]) => (
                  <CopyRow key={label} label={label} command={command} t={t} />
                ))}
              </div>

              <div className="mt-6 flex flex-col gap-3 sm:flex-row">
                <ButtonLink href={RELEASE.latest} arrow>{t.all}</ButtonLink>
                <ButtonLink href={LINKS.source} variant="secondary">{t.source}</ButtonLink>
              </div>
            </div>

            <div>
              <p className="font-mono text-[9px] uppercase tracking-[0.14em] text-muted">{t.window}</p>
              <div className="mt-4 space-y-2">
                {t.packages.map(([ext, target, note]) => (
                  <a
                    key={ext}
                    href={RELEASE.latest}
                    className="group flex items-center gap-4 rounded-xl border border-white/[0.12] bg-black/40 px-4 py-4 backdrop-blur-xl transition hover:border-phosphor/50 hover:bg-black/55"
                  >
                    <span className="font-mono text-base font-semibold tracking-[-0.03em] text-ink">{ext}</span>
                    <span className="min-w-0">
                      <span className="block text-[13px] text-ink/85">{target}</span>
                      <span className="block font-mono text-[9px] text-muted">{note}</span>
                    </span>
                    <span className="ml-auto text-phosphor transition-transform group-hover:translate-x-0.5">↗</span>
                  </a>
                ))}
              </div>
              <p className="mt-5 text-[13px] leading-5 text-ink/55">{t.carries}</p>
              <p className="mt-4 border-t border-white/[0.09] pt-4 font-mono text-[9px] leading-5 text-muted">{t.platforms}</p>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
