import type { AnchorHTMLAttributes, ReactNode } from "react";
import { ArrowIcon } from "./icons";

type ButtonLinkProps = AnchorHTMLAttributes<HTMLAnchorElement> & {
  children: ReactNode;
  variant?: "primary" | "secondary";
  arrow?: boolean;
};

export function ButtonLink({ children, variant = "primary", arrow = false, className = "", ...props }: ButtonLinkProps) {
  const styles = variant === "primary"
    ? "border-phosphor bg-phosphor text-[#07120b] hover:bg-[#64e895] hover:shadow-glow"
    : "border-line bg-surface/60 text-ink hover:border-muted/60 hover:bg-[#19191d]";

  return (
    <a className={`inline-flex h-11 items-center justify-center gap-2 rounded-lg border px-5 font-mono text-sm font-semibold transition duration-150 ${styles} ${className}`} {...props}>
      {children}
      {arrow && <ArrowIcon className="h-4 w-4" />}
    </a>
  );
}
