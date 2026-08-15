import type { AnchorHTMLAttributes, ReactNode } from "react";
import { ArrowIcon } from "./icons";

type ButtonLinkProps = AnchorHTMLAttributes<HTMLAnchorElement> & {
  children: ReactNode;
  variant?: "primary" | "secondary";
  arrow?: boolean;
};

export function ButtonLink({ children, variant = "primary", arrow = false, className = "", ...props }: ButtonLinkProps) {
  const styles = variant === "primary"
    ? "border-phosphor bg-phosphor text-[#0a1008] hover:-translate-y-0.5 hover:bg-[#c6ff74] hover:shadow-glow"
    : "border-line bg-base/45 text-ink backdrop-blur-md hover:-translate-y-0.5 hover:border-muted/70 hover:bg-elevated/80";

  return (
    <a className={`inline-flex h-12 items-center justify-center gap-2 whitespace-nowrap rounded-full border px-6 font-mono text-xs font-semibold uppercase tracking-[0.08em] transition duration-200 ${styles} ${className}`} {...props}>
      {children}
      {arrow && <ArrowIcon className="h-4 w-4" />}
    </a>
  );
}
