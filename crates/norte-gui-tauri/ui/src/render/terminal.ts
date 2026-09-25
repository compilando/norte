// `Screen` painter for the terminal panel (#362, bridge 95): a function with
// `this: Screen`, hooked in as a property in `render.ts`. State stays in the
// class.

import type { Screen } from "../render";
import type { TerminalColorView, TerminalSlotView, TerminalSpanView } from "../types";
import { note } from "./dom";
import type { SlotDom } from "./dom";

/**
 * The terminal panel: the grid the host already emulated.
 *
 * What arrives is ROWS ALREADY PAINTED, not the pty's bytes. The emulation is
 * done by `norte-term` on the host's side — the same crate the terminal uses
 * — so both frontends show the same thing by construction and not because
 * someone compared two emulators.
 *
 * **This is FOREIGN content.** It carries no theme role, and must not: what a
 * program paints inside is its own, and tinting it with the theme would lie
 * about what that program said. Ours is the frame, set by the slot.
 *
 * Nothing needs sanitizing here either, and that is not an oversight: what
 * comes out of the grid cannot carry a control byte, because the parser eats
 * the escapes and drops the C0s that do not move the cursor. It is painted
 * with `textContent`, so there is no HTML that could slip in either.
 */
export function paintTerminal(this: Screen, dom: SlotDom, slot: TerminalSlotView): void {
  dom.root.setAttribute("aria-label", this.t("panelbar-terminal"));
  dom.scroller.className = "terminal";
  dom.title.replaceChildren(document.createTextNode(this.t("panelbar-terminal")));
  if (slot.no_shell) {
    // A blank panel and a panel with no shell look the same and are not the
    // same thing.
    dom.scroller.replaceChildren(note(this.t("terminal-none")));
    return;
  }
  const rows = slot.rows.map((row, y) => paintRow(row, y, slot.cursor));
  dom.scroller.replaceChildren(...rows);
}

/** A row: its fragments, plus the cursor if it falls on it. */
function paintRow(
  row: TerminalSpanView[],
  y: number,
  cursor: [number, number] | null,
): HTMLElement {
  const line = document.createElement("div");
  line.className = "terminal-row";
  // The cursor is painted by splitting the fragment it falls on, not with a
  // layer on top: a layer positioned by columns assumes every cell is the
  // same width, and that stops being true with a wide character.
  const col = cursor !== null && cursor[0] === y ? cursor[1] : null;
  let x = 0;
  for (const span of row) {
    // `Array.from` and not `split("")`: splitting by UTF-16 units breaks an
    // emoji in half and leaves two halves that are not characters.
    const chars = Array.from(span.text);
    if (col === null || col < x || col >= x + chars.length) {
      line.append(paintSpan(span, span.text, false));
      x += chars.length;
      continue;
    }
    const cut = col - x;
    if (cut > 0) {
      line.append(paintSpan(span, chars.slice(0, cut).join(""), false));
    }
    line.append(paintSpan(span, chars[cut] ?? " ", true));
    if (cut + 1 < chars.length) {
      line.append(paintSpan(span, chars.slice(cut + 1).join(""), false));
    }
    x += chars.length;
  }
  // The cursor past the last fragment — or on an empty row — is still a
  // place it can be: without this it does not show on a freshly painted
  // prompt.
  if (col !== null && col >= x) {
    const gap = document.createElement("span");
    gap.className = "terminal-cursor";
    gap.textContent = " ";
    line.append(gap);
  }
  return line;
}

function paintSpan(span: TerminalSpanView, text: string, isCursor: boolean): HTMLElement {
  const el = document.createElement("span");
  // `textContent` and never `innerHTML`: this was written by another program.
  el.textContent = text;
  if (isCursor) {
    el.classList.add("terminal-cursor");
  }
  // `reverse` is resolved HERE, by swapping the two colors: the host sends it
  // as a flag precisely so as not to lose which one was which.
  //
  // And the swap has to work even when one of the two is NOT there. An `ls`
  // that reverses to mark something does not send colors: it sends plain
  // `SGR 7`, and what it expects is the paper flipped. Without both defaults
  // made explicit, that used to end up invisible.
  const fg = span.reverse ? span.bg : span.fg;
  const bg = span.reverse ? span.fg : span.bg;
  if (fg !== undefined) {
    el.style.color = css(fg);
  } else if (span.reverse) {
    el.style.color = "var(--bg)";
  }
  if (bg !== undefined) {
    el.style.background = css(bg);
  } else if (span.reverse) {
    el.style.background = "var(--fg)";
  }
  if (span.bold) el.style.fontWeight = "bold";
  if (span.dim) el.style.opacity = "0.65";
  if (span.italic) el.style.fontStyle = "italic";
  if (span.underline) el.style.textDecoration = "underline";
  if (span.strike) {
    el.style.textDecoration = span.underline ? "underline line-through" : "line-through";
  }
  return el;
}

/**
 * A fragment's color, as CSS.
 *
 * An index comes out as `var(--term-N)`: the palette is defined by the
 * THEME, which is the one that has to decide what blue "color 4" is. That is
 * why the host sends it unresolved — if it had resolved it, this line would
 * not exist and the panel would not obey the theme.
 *
 * A `#rrggbb` was chosen by the program and travels as-is: there is nothing
 * to decide there.
 */
function css(color: TerminalColorView): string {
  return color.kind === "indexed" ? `var(--term-${color.index})` : color.hex;
}
