import type { CSSProperties, ReactNode } from "react";
import { isWide, isZero, type Packed, type Style } from "@/lib/ansi";

/**
 * A captured terminal screen painted as text. It scales with its container
 * (`cqw`): the font is sized so the screen's columns fill the width exactly,
 * which is what keeps the box drawing aligned at any size. It renders on the
 * server and in the browser alike.
 */

function css(r: Style, screen: Packed): CSSProperties | undefined {
  let fg = r.fg;
  let bg = r.bg;
  if (r.reverse) [fg, bg] = [bg ?? screen.bg, fg ?? screen.fg];
  // Dim blends the INK towards its background; opacity would fade the background too.
  if (r.dim) fg = `color-mix(in srgb, ${fg ?? screen.fg ?? "currentColor"} 55%, ${bg ?? screen.bg ?? "transparent"})`;
  const s: CSSProperties = {};
  if (fg) s.color = fg;
  if (bg && bg !== screen.bg) s.backgroundColor = bg;
  if (r.bold) s.fontWeight = 700;
  if (r.italic) s.fontStyle = "italic";
  if (r.underline) s.textDecoration = "underline";
  return Object.keys(s).length ? s : undefined;
}

/**
 * Wide characters get a two-cell box of their own; a zero-width one (a
 * combining accent, VS16) stays glued to the character it modifies.
 */
function text(t: string): ReactNode {
  if (![...t].some(isWide)) return t;
  const parts: { wide: boolean; s: string }[] = [];
  for (const ch of t) {
    const last = parts[parts.length - 1];
    if (isZero(ch) && last) last.s += ch;
    else if (isWide(ch)) parts.push({ wide: true, s: ch });
    else if (last && !last.wide) last.s += ch;
    else parts.push({ wide: false, s: ch });
  }
  return parts.map((p, i) =>
    p.wide ? (
      <span key={i} className="term-wide">
        {p.s}
      </span>
    ) : (
      p.s
    ),
  );
}

export function TerminalScreen({ screen, cols, label }: { screen: Packed; cols: number; label?: string }) {
  const styles = screen.styles.map((s) => css(s, screen));
  return (
    <div className="term" style={{ backgroundColor: screen.bg, ["--cols" as string]: cols }}>
      <pre role="img" aria-label={label} style={{ color: screen.fg }}>
        {screen.rows.map((row, i) => (
          <span key={i}>
            {row.map(([t, s], j) =>
              styles[s] ? (
                <span key={j} style={styles[s]}>
                  {text(t)}
                </span>
              ) : (
                <span key={j}>{text(t)}</span>
              ),
            )}
            {"\n"}
          </span>
        ))}
      </pre>
    </div>
  );
}
