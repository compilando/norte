import type { SVGProps } from "react";

export type IconName = "bolt" | "keyboard" | "panels" | "search" | "blocks" | "branch";

const paths: Record<IconName, React.ReactNode> = {
  bolt: <path d="m13 2-8 11h7l-1 9 8-12h-7l1-8Z" />,
  keyboard: <><rect x="3" y="6" width="18" height="12" rx="2" /><path d="M7 10h.01M11 10h.01M15 10h.01M18 10h.01M7 14h.01M11 14h6" /></>,
  panels: <><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M12 4v16M7 8h1M16 8h1M7 12h2M15 12h2" /></>,
  search: <><circle cx="11" cy="11" r="7" /><path d="m20 20-4-4M8 11h6" /></>,
  blocks: <><rect x="4" y="4" width="6" height="6" rx="1" /><rect x="14" y="4" width="6" height="6" rx="1" /><rect x="4" y="14" width="6" height="6" rx="1" /><path d="M17 14v6M14 17h6" /></>,
  branch: <><circle cx="6" cy="5" r="2" /><circle cx="18" cy="7" r="2" /><circle cx="6" cy="19" r="2" /><path d="M6 7v10M8 7c5 0 4 0 8 0" /></>,
};

export function LineIcon({ name, ...props }: { name: IconName } & SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" {...props}>
      {paths[name]}
    </svg>
  );
}

export function ArrowIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 16 16" fill="none" aria-hidden="true" {...props}>
      <path d="M3 8h10M9 4l4 4-4 4" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}
