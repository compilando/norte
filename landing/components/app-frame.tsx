import type { ReactNode } from "react";

/** The window a capture sits in: three dots, a title, and the capture. */
export function AppFrame({
  title,
  children,
  right,
  className = "",
}: {
  title: string;
  children: ReactNode;
  right?: ReactNode;
  className?: string;
}) {
  return (
    <div className={`overflow-hidden rounded-xl border border-white/[0.12] bg-[#0c0f10] shadow-terminal ${className}`}>
      <div className="flex h-8 items-center gap-3 border-b border-white/[0.08] bg-[#111517] px-3">
        <span className="flex gap-1.5" aria-hidden>
          <span className="h-2.5 w-2.5 rounded-full bg-[#ff5f57]/80" />
          <span className="h-2.5 w-2.5 rounded-full bg-[#febc2e]/80" />
          <span className="h-2.5 w-2.5 rounded-full bg-[#28c840]/80" />
        </span>
        <span className="min-w-0 flex-1 truncate text-center font-mono text-[10px] text-muted">{title}</span>
        <span className="flex shrink-0 items-center gap-2">{right}</span>
      </div>
      {children}
    </div>
  );
}
