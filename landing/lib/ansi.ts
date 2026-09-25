/**
 * Reads what `tmux capture-pane -e` wrote — text plus SGR escapes — into rows
 * of styled runs. The terminal shots on the page are these runs painted as
 * text, not pictures: sharp at any zoom, selectable, and a few KB each.
 *
 * Only SGR (`ESC[…m`) is understood, because that is all capture-pane emits.
 */

export type Run = {
  text: string;
  fg?: string;
  bg?: string;
  bold?: boolean;
  dim?: boolean;
  italic?: boolean;
  underline?: boolean;
  reverse?: boolean;
};

export type Screen = { rows: Run[][]; bg?: string; fg?: string };

export type Style = Omit<Run, "text">;

// xterm's 16 colours; ntc paints truecolor, so these only cover stray escapes.
const BASIC = [
  "#000000", "#cd0000", "#00cd00", "#cdcd00", "#0000ee", "#cd00cd", "#00cdcd", "#e5e5e5",
  "#7f7f7f", "#ff0000", "#00ff00", "#ffff00", "#5c5cff", "#ff00ff", "#00ffff", "#ffffff",
];

/**
 * Cells a character takes in a terminal: 2 for CJK and emoji, 1 otherwise.
 * The page must draw those two cells as two, or every column after a 東京
 * slides one place left.
 */
export function isWide(ch: string): boolean {
  const c = ch.codePointAt(0) ?? 0;
  return (
    (c >= 0x1100 && c <= 0x115f) ||
    (c >= 0x2e80 && c <= 0xa4cf && c !== 0x303f) ||
    (c >= 0xac00 && c <= 0xd7a3) ||
    (c >= 0xf900 && c <= 0xfaff) ||
    (c >= 0xfe30 && c <= 0xfe4f) ||
    (c >= 0xff00 && c <= 0xff60) ||
    (c >= 0xffe0 && c <= 0xffe6) ||
    (c >= 0x1f300 && c <= 0x1f64f) ||
    (c >= 0x1f680 && c <= 0x1f6ff) ||
    (c >= 0x1f900 && c <= 0x1f9ff) ||
    (c >= 0x20000 && c <= 0x3fffd)
  );
}

/** Combining marks, variation selectors, joiners: they change a glyph, they take no cell. */
export function isZero(ch: string): boolean {
  const c = ch.codePointAt(0) ?? 0;
  return (c >= 0x0300 && c <= 0x036f) || (c >= 0xfe00 && c <= 0xfe0f) || c === 0x200d;
}

export function cells(text: string): number {
  let n = 0;
  for (const ch of text) n += isZero(ch) ? 0 : isWide(ch) ? 2 : 1;
  return n;
}

function hex(r: number, g: number, b: number): string {
  return `#${[r, g, b].map((v) => v.toString(16).padStart(2, "0")).join("")}`;
}

function xterm256(n: number): string {
  if (n < 16) return BASIC[n];
  if (n >= 232) {
    const v = 8 + (n - 232) * 10;
    return hex(v, v, v);
  }
  const i = n - 16;
  const level = (c: number) => (c === 0 ? 0 : 55 + c * 40);
  return hex(level(Math.floor(i / 36)), level(Math.floor(i / 6) % 6), level(i % 6));
}

function applySgr(style: Style, params: number[]): Style {
  const s = { ...style };
  if (params.length === 0) params = [0];
  for (let i = 0; i < params.length; i++) {
    const p = params[i];
    if (p === 0) {
      for (const k of Object.keys(s) as (keyof Style)[]) delete s[k];
    } else if (p === 1) s.bold = true;
    else if (p === 2) s.dim = true;
    else if (p === 3) s.italic = true;
    else if (p === 4) s.underline = true;
    else if (p === 7) s.reverse = true;
    else if (p === 22) {
      delete s.bold;
      delete s.dim;
    } else if (p === 23) delete s.italic;
    else if (p === 24) delete s.underline;
    else if (p === 27) delete s.reverse;
    else if (p === 39) delete s.fg;
    else if (p === 49) delete s.bg;
    else if ((p >= 30 && p <= 37) || (p >= 90 && p <= 97)) s.fg = BASIC[p >= 90 ? p - 82 : p - 30];
    else if ((p >= 40 && p <= 47) || (p >= 100 && p <= 107)) s.bg = BASIC[p >= 100 ? p - 92 : p - 40];
    else if (p === 38 || p === 48) {
      const key = p === 38 ? "fg" : "bg";
      if (params[i + 1] === 2) {
        s[key] = hex(params[i + 2] ?? 0, params[i + 3] ?? 0, params[i + 4] ?? 0);
        i += 4;
      } else if (params[i + 1] === 5) {
        s[key] = xterm256(params[i + 2] ?? 0);
        i += 2;
      }
    }
  }
  return s;
}

function same(a: Style, b: Style): boolean {
  return (
    a.fg === b.fg &&
    a.bg === b.bg &&
    !!a.bold === !!b.bold &&
    !!a.dim === !!b.dim &&
    !!a.italic === !!b.italic &&
    !!a.underline === !!b.underline &&
    !!a.reverse === !!b.reverse
  );
}

/** One screen. `cols` pads every row with the screen's own background. */
export function parseScreen(raw: string, cols?: number): Screen {
  const lines = raw.replace(/\n$/, "").split("\n");
  const rows: Run[][] = [];
  // capture-pane does NOT reset the style at the end of a line: it carries on.
  let style: Style = {};
  for (const line of lines) {
    const row: Run[] = [];
    let width = 0;
    const push = (text: string) => {
      if (!text) return;
      width += cells(text);
      const last = row[row.length - 1];
      if (last && same(last, style)) last.text += text;
      else row.push({ text, ...style });
    };
    const re = /\x1b\[([0-9;:]*)m/g;
    let at = 0;
    for (let m = re.exec(line); m; m = re.exec(line)) {
      push(line.slice(at, m.index));
      style = applySgr(
        style,
        m[1] === "" ? [] : m[1].split(/[;:]/).map((n) => Number(n) || 0),
      );
      at = re.lastIndex;
    }
    push(line.slice(at).replace(/\x1b\[[0-9;?]*[A-Za-z]/g, ""));
    if (cols && width < cols) row.push({ text: " ".repeat(cols - width), ...style });
    rows.push(row);
  }
  return { rows, ...dominant(rows) };
}

/** The colours most of the screen is painted in: the frame around the text. */
function dominant(rows: Run[][]): { bg?: string; fg?: string } {
  const bgs = new Map<string, number>();
  const fgs = new Map<string, number>();
  for (const row of rows)
    for (const r of row) {
      const n = cells(r.text);
      if (r.bg) bgs.set(r.bg, (bgs.get(r.bg) ?? 0) + n);
      if (r.fg && r.text.trim()) fgs.set(r.fg, (fgs.get(r.fg) ?? 0) + n);
    }
  const top = (m: Map<string, number>) => [...m].sort((a, b) => b[1] - a[1])[0]?.[0];
  return { bg: top(bgs), fg: top(fgs) };
}

/**
 * A screen as it travels to the browser: each distinct style once, and every
 * run as `[text, style index]`. A captured screen repeats a dozen styles over
 * hundreds of runs, so this is a fraction of the React tree it replaces.
 */
export type Packed = { bg?: string; fg?: string; styles: Style[]; rows: [string, number][][] };

export function pack(screen: Screen): Packed {
  const styles: Style[] = [];
  const index = new Map<string, number>();
  const rows = screen.rows.map((row) =>
    row.map(({ text, ...style }): [string, number] => {
      const key = JSON.stringify(style);
      let i = index.get(key);
      if (i === undefined) {
        i = styles.push(style) - 1;
        index.set(key, i);
      }
      return [text, i];
    }),
  );
  return { bg: screen.bg, fg: screen.fg, styles, rows };
}

/** A clip: screens separated by a form feed on a line of its own. */
export function parseReel(raw: string, cols?: number): Screen[] {
  return raw
    .split("\f\n")
    .filter((s) => s.trim())
    .map((s) => parseScreen(s, cols));
}
