"use client";

import { motion, useReducedMotion } from "framer-motion";

export function SignalStrip({ signals }: { signals: string[] }) {
  const reduceMotion = useReducedMotion();
  const items = [...signals, ...signals];
  return (
    <div className="mask-fade-x overflow-hidden border-y border-line/80 bg-[#0a0d0e] py-3.5" aria-label={signals.join(", ")}>
      <motion.div className="flex w-max items-center" animate={reduceMotion ? undefined : { x: [0, "-50%"] }} transition={{ duration: 34, repeat: Infinity, ease: "linear" }}>
        {items.map((signal, index) => (
          <div key={`${signal}-${index}`} aria-hidden={index >= signals.length} className="flex items-center gap-6 px-6 font-mono text-[9px] uppercase tracking-[0.15em] text-muted">
            <span>{signal}</span><span className="h-1 w-1 rotate-45 bg-phosphor/65" />
          </div>
        ))}
      </motion.div>
    </div>
  );
}
