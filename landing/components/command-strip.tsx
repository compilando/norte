"use client";

import { motion, useReducedMotion } from "framer-motion";

const commands = [["F5", "COPY"], ["SPACE C E", "COMPARE"], ["CTRL P", "COMMAND"], ["G G", "TOP"], ["F6", "MOVE"], ["U", "UNDO"], ["/", "SEARCH"], ["SPACE T", "TASKS"]];

export function CommandStrip() {
  const reduceMotion = useReducedMotion();
  const items = [...commands, ...commands];
  return (
    <div className="overflow-hidden border-y border-line/70 bg-surface/60 py-3 font-mono" aria-label="Keyboard commands">
      <motion.div className="flex w-max items-center" animate={reduceMotion ? undefined : { x: [0, "-50%"] }} transition={{ duration: 28, repeat: Infinity, ease: "linear" }}>
        {items.map(([key, command], index) => (
          <div key={`${key}-${index}`} className="flex items-center gap-3 px-8 text-[11px] uppercase tracking-[0.14em] text-muted">
            <kbd className="rounded border border-line bg-base px-2 py-1 text-phosphor">{key}</kbd><span>{command}</span><span className="ml-5 text-line">◆</span>
          </div>
        ))}
      </motion.div>
    </div>
  );
}
