export function Brand({ compact = false }: { compact?: boolean }) {
  return (
    <span className="inline-flex items-center gap-2.5">
      <span className="relative grid h-7 w-7 place-items-center rounded-full border border-phosphor/45 bg-phosphor/[0.07] shadow-[inset_0_0_14px_rgba(183,255,82,.08)]">
        <span className="absolute h-3.5 w-px rotate-[28deg] bg-phosphor shadow-[0_0_7px_#B7FF52]" />
        <span className="absolute h-1.5 w-1.5 -translate-y-1 rounded-full bg-phosphor" />
      </span>
      {!compact && <span className="text-[15px] font-semibold tracking-[-0.02em] text-ink">norte</span>}
    </span>
  );
}
