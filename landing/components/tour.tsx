"use client";

import { useEffect, useRef, useState } from "react";
import type { Packed } from "@/lib/ansi";
import { AppFrame } from "./app-frame";
import { TerminalScreen } from "./terminal-screen";

type Step = { scene: string; kicker: string; title: string; body: string; keys: string[] };

/**
 * The tour: the steps scroll, the screen stays. The step crossing the middle
 * of the viewport decides which capture is shown.
 */
export function Tour({ steps, screens, cols }: { steps: Step[]; screens: Record<string, Packed | null>; cols: number }) {
  const [active, setActive] = useState(0);
  const refs = useRef<(HTMLElement | null)[]>([]);

  useEffect(() => {
    const io = new IntersectionObserver(
      (entries) => {
        for (const e of entries) if (e.isIntersecting) setActive(Number((e.target as HTMLElement).dataset.step));
      },
      { rootMargin: "-48% 0px -48% 0px" },
    );
    refs.current.forEach((el) => el && io.observe(el));
    return () => io.disconnect();
  }, []);

  const shown = steps.filter((s) => screens[s.scene]);
  const step = shown[active] ?? shown[0];
  const shot = step ? screens[step.scene] : null;

  return (
    <div className="relative grid gap-10 lg:grid-cols-[.8fr_1.2fr] lg:gap-16">
      <div className="sticky top-[76px] z-10 order-first self-start lg:order-last lg:top-28">
        <AppFrame title={`ada@norte — ${step?.kicker ?? ""}`}>
          <div key={step?.scene} className="animate-[fadein_.35s_ease]">
            {shot && <TerminalScreen screen={shot} cols={cols} label={step?.title} />}
          </div>
        </AppFrame>
        <div className="mt-3 hidden gap-1 lg:flex" aria-hidden>
          {shown.map((s, i) => (
            <span key={s.scene} className={`h-0.5 flex-1 rounded-full transition-colors ${i === active ? "bg-phosphor" : "bg-white/10"}`} />
          ))}
        </div>
      </div>

      <ol className="relative">
        {shown.map((s, i) => (
          <li
            key={s.scene}
            ref={(el) => {
              refs.current[i] = el;
            }}
            data-step={i}
            className="flex min-h-[46vh] flex-col justify-center py-10 lg:min-h-[78vh]"
          >
            <p className={`font-mono text-[10px] uppercase tracking-[0.16em] transition-colors ${i === active ? "text-phosphor" : "text-muted"}`}>
              {String(i + 1).padStart(2, "0")} · {s.kicker}
            </p>
            <h3
              className={`mt-4 text-balance text-3xl font-medium leading-[1.05] tracking-[-0.045em] transition-colors sm:text-4xl ${
                i === active ? "text-ink" : "text-ink/35"
              }`}
            >
              {s.title}
            </h3>
            <p className={`mt-5 max-w-md text-base leading-7 transition-colors ${i === active ? "text-ink/70" : "text-ink/25"}`}>{s.body}</p>
            <div className="mt-6 flex flex-wrap gap-2">
              {s.keys.map((k) => (
                <kbd
                  key={k}
                  className="rounded-md border border-white/[0.14] border-b-white/[0.25] bg-white/[0.04] px-2 py-1 font-mono text-[11px] text-ink/80"
                >
                  {k}
                </kbd>
              ))}
            </div>
          </li>
        ))}
      </ol>
    </div>
  );
}
