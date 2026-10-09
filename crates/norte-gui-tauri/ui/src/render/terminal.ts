// `Screen` painter for the terminal panel (#362, bridge 95): a function with
// `this: Screen`, hooked in as a property in `render.ts`. State stays in the
// class.

import type { Screen } from "../render";
import type {
  TerminalColorView,
  TerminalInstanceView,
  TerminalSlotView,
  TerminalSpanView,
} from "../types";
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
  if (dom.title.dataset["terminal"] !== "true") {
    dom.title.replaceChildren(document.createTextNode(this.t("panelbar-terminal")));
    dom.title.dataset["terminal"] = "true";
  }
  placeActions.call(this, dom, slot);
  if (slot.no_shell) {
    // A blank panel and a panel with no shell look the same and are not the
    // same thing.
    dom.scroller.replaceChildren(note(this.t("terminal-none")));
    return;
  }
  // The grid and, with two or more shells, VS Code's list on the right.
  // The grid keeps its node across patches: its rows are diffed below.
  let grid = dom.scroller.querySelector<HTMLElement>(":scope > .terminal-grid");
  if (grid === null) {
    grid = document.createElement("div");
    grid.className = "terminal-grid";
    dom.scroller.replaceChildren(grid);
  }
  grid.classList.toggle("terminal-exited", slot.exited != null);
  paintGrid(grid, slot);
  const instances = slot.instances ?? [];
  const old = dom.scroller.querySelector(":scope > .terminal-list");
  const status = dom.scroller.querySelector(":scope > .terminal-status");
  status?.remove();
  if (slot.exited != null) {
    const line = document.createElement("div");
    line.className = "terminal-status";
    line.textContent = `${this.t("terminal-exited-label")} ${String(slot.exited)}`;
    dom.scroller.append(line);
  }
  if (instances.length >= 2) {
    // Rebuilt only when the LIST changed — a shell printing patches the
    // panel up to 30 times a second and says nothing new about the list —
    // and never under a rename field: rebuilding dropped the field, focus
    // fell to the document, and the rest of the name went to the shell.
    const signature = JSON.stringify([instances, slot.active, slot.list_cols]);
    const renaming = old?.querySelector(".terminal-rename") != null;
    if (old instanceof HTMLElement && (renaming || old.dataset["sig"] === signature)) {
      return;
    }
    const list = paintList.call(this, instances, slot);
    list.dataset["sig"] = signature;
    if (old === null) {
      dom.scroller.append(list);
    } else {
      old.replaceWith(list);
    }
  } else {
    old?.remove();
  }
}

/** The grid's rows, only the ones that changed. */
function paintGrid(grid: HTMLElement, slot: TerminalSlotView): void {
  // Row by row, and only the rows that changed: the whole grid used to be
  // rebuilt on every patch, up to thirty times a second while a shell
  // printed — a `top` or a build output redrew every span of every line.
  const old = grid.children;
  for (const [y, row] of slot.rows.entries()) {
    const col = slot.cursor !== null && slot.cursor[0] === y ? slot.cursor[1] : null;
    const signature = JSON.stringify([row, col]);
    const current = old[y];
    if (current instanceof HTMLElement && current.dataset["sig"] === signature) {
      continue;
    }
    const line = paintRow(row, y, slot.cursor);
    line.dataset["sig"] = signature;
    if (current === undefined) {
      grid.append(line);
    } else {
      current.replaceWith(line);
    }
  }
  while (grid.children.length > slot.rows.length) {
    grid.lastElementChild?.remove();
  }
}

/**
 * Puts the buttons where they are SEEN. The terminal's usual place is a
 * group of panels on an edge, whose title bar is hidden under the group's
 * tabs: there they go on the tab bar's right end, as VS Code puts them.
 * Rebuilt only when what they act on changed — a shell printing patches
 * the panel up to 30 times a second, and a rebuild would drop a rename
 * field halfway through.
 */
function placeActions(this: Screen, dom: SlotDom, slot: TerminalSlotView): void {
  const inGroup =
    dom.tabs.dataset["open"] === "true" && dom.tabs.dataset["panels"] === "true";
  const host = inGroup ? dom.tabs : dom.title;
  (inGroup ? dom.title : dom.tabs).querySelector(":scope > .terminal-actions")?.remove();
  const front = (slot.instances ?? []).find((i) => i.id === slot.active);
  const signature = JSON.stringify([slot.active, slot.profiles, front?.name]);
  const old = host.querySelector<HTMLElement>(":scope > .terminal-actions");
  if (old?.dataset["sig"] === signature) {
    return;
  }
  old?.remove();
  const actions = titleActions.call(this, slot);
  actions.dataset["sig"] = signature;
  host.append(actions);
}

/** `+`, the shell-profile menu, rename and close: the panel's buttons. */
function titleActions(this: Screen, slot: TerminalSlotView): HTMLElement {
  const box = document.createElement("span");
  box.className = "terminal-actions";
  const button = (text: string, key: string, run: (e: MouseEvent) => void): void => {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "terminal-action";
    b.textContent = text;
    b.title = this.t(key);
    b.setAttribute("aria-label", this.t(key));
    b.addEventListener("click", (e) => {
      e.stopPropagation();
      run(e);
    });
    box.append(b);
  };
  button("+", "terminal-new", () => {
    this.send({ action: "terminal_new", profile: null });
  });
  // The same four as the terminal's border (`+ ▾ ✎ ✕`), always there: a
  // button that comes and goes is a button nobody finds.
  const profiles = slot.profiles ?? [];
  button("▾", "terminal-shell-profiles", (e) => {
    popup.call(
      this,
      e.clientX,
      e.clientY,
      profiles.map((p) => [p, () => this.send({ action: "terminal_new", profile: p })]),
    );
  });
  const front = (slot.instances ?? []).find((i) => i.id === slot.active);
  button("✎", "menu-item-terminal-rename", () => {
    if (front !== undefined) {
      renameInTitle.call(this, front, box);
    }
  });
  button("✕", "terminal-close", () => {
    if (slot.active != null) {
      this.send({ action: "terminal_close", id: slot.active });
    }
  });
  return box;
}

/** ✎: the panel's name becomes a field for the shell in front's name. */
function renameInTitle(
  this: Screen,
  front: TerminalInstanceView,
  box: HTMLElement,
): void {
  const title = box.parentElement;
  if (title === null || title.querySelector(".terminal-rename") !== null) {
    return;
  }
  const input = document.createElement("input");
  input.className = "terminal-rename";
  // From the NAME, as everywhere else (see `renameInline`).
  input.value = front.name ?? "";
  input.setAttribute("aria-label", this.t("terminal-rename-prompt"));
  input.addEventListener("keydown", (e) => {
    // Ours: the host would take these keys for the shell.
    e.stopPropagation();
    if (e.key === "Enter") {
      this.send({ action: "terminal_rename", id: front.id, name: input.value });
      input.blur();
    } else if (e.key === "Escape") {
      input.blur();
    }
  });
  input.addEventListener("blur", () => {
    input.remove();
  });
  box.before(input);
  input.focus();
  input.select();
}

/** VS Code's list: one entry per shell, a click brings it forward. */
function paintList(
  this: Screen,
  instances: TerminalInstanceView[],
  slot: TerminalSlotView,
): HTMLElement {
  const list = document.createElement("ul");
  list.className = "terminal-list";
  list.setAttribute("role", "listbox");
  if (slot.list_cols != null && slot.list_cols > 0) {
    list.style.width = `calc(var(--cell-w) * ${String(slot.list_cols)})`;
  }
  for (const i of instances) {
    const li = document.createElement("li");
    li.className = "terminal-entry";
    li.setAttribute("role", "option");
    li.setAttribute("aria-selected", String(i.id === slot.active));
    li.dataset["id"] = String(i.id);
    if (i.color !== null) {
      li.style.borderLeftColor = `var(--term-${String(i.color)})`;
    }
    if (i.icon !== null) {
      const icon = document.createElement("span");
      icon.className = "terminal-entry-icon";
      icon.textContent = ICONS[i.icon] ?? "";
      li.append(icon);
    }
    const title = document.createElement("span");
    title.className = "terminal-entry-title";
    // `textContent`: a title can come from the program (OSC 0/2).
    title.textContent = i.title;
    li.append(title);
    if (i.unseen) {
      const dot = document.createElement("span");
      dot.className = "terminal-entry-unseen";
      dot.textContent = "●";
      li.append(dot);
    }
    if (i.exited !== null) {
      li.classList.add("terminal-entry-exited");
      li.title = `${this.t("terminal-exited-label")} ${String(i.exited)}`;
    }
    li.addEventListener("click", () => {
      this.send({ action: "terminal_select", id: i.id });
    });
    li.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      entryMenu.call(this, e.clientX, e.clientY, i, li);
    });
    list.append(li);
  }
  return list;
}

/** The glyph for each icon name the host may send. */
const ICONS: Record<string, string> = {
  terminal: "❯",
  code: "‹›",
  server: "▤",
  debug: "✱",
  package: "▣",
  star: "★",
};

/** Right button on an entry: rename, colour, icon, close. */
function entryMenu(
  this: Screen,
  x: number,
  y: number,
  i: TerminalInstanceView,
  li: HTMLElement,
): void {
  const decorate = (icon: string | null, color: number | null): void => {
    this.send({ action: "terminal_decorate", id: i.id, icon, color });
  };
  const entries: [string, () => void][] = [
    [this.t("menu-item-terminal-rename"), () => renameInline.call(this, i, li)],
    [this.t("terminal-color-none"), () => decorate(i.icon, null)],
    ...[1, 2, 3, 4, 5, 6].map((c): [string, () => void] => [
      `■ ${String(c)}`,
      () => decorate(i.icon, c),
    ]),
    [this.t("terminal-icon-none"), () => decorate(null, i.color)],
    ...Object.entries(ICONS).map(([name, glyph]): [string, () => void] => [
      `${glyph} ${name}`,
      () => decorate(name, i.color),
    ]),
    [
      this.t("menu-item-terminal-close"),
      () => this.send({ action: "terminal_close", id: i.id }),
    ],
  ];
  popup.call(this, x, y, entries);
}

/** The entry's title becomes a field: Enter names, Escape cancels. */
function renameInline(this: Screen, i: TerminalInstanceView, stale: HTMLElement): void {
  // Found again by id: a patch between the right click and this choice may
  // have rebuilt the list, and the node the menu kept is then detached.
  const li =
    document.querySelector<HTMLElement>(`.terminal-entry[data-id="${String(i.id)}"]`) ??
    stale;
  const title = li.querySelector(".terminal-entry-title");
  const input = document.createElement("input");
  input.className = "terminal-rename";
  // From the NAME, as the keyboard dialog does: starting from the program's
  // title would freeze it as a name on a plain Enter.
  input.value = i.name ?? "";
  input.setAttribute("aria-label", this.t("terminal-rename-prompt"));
  input.addEventListener("keydown", (e) => {
    // Ours: the host would take these keys for the shell.
    e.stopPropagation();
    if (e.key === "Enter") {
      this.send({ action: "terminal_rename", id: i.id, name: input.value });
      input.blur();
    } else if (e.key === "Escape") {
      input.blur();
    }
  });
  input.addEventListener("blur", () => {
    // The title comes back until the host's answer repaints the list.
    if (title !== null) {
      input.replaceWith(title);
    } else {
      input.remove();
    }
  });
  title?.replaceWith(input);
  input.focus();
  input.select();
}

/** A small menu at the pointer, styled like the tab menu. */
function popup(
  this: Screen,
  x: number,
  y: number,
  entries: [string, () => void][],
): void {
  document.querySelector(".tab-menu")?.remove();
  const box = document.createElement("ul");
  box.className = "tab-menu";
  box.setAttribute("role", "menu");
  box.style.left = `${String(x)}px`;
  box.style.top = `${String(y)}px`;
  const dismiss = (): void => {
    box.remove();
    window.removeEventListener("pointerdown", outside, true);
    window.removeEventListener("keydown", escape, true);
  };
  const outside = (e: Event): void => {
    if (!(e.target instanceof Node) || !box.contains(e.target)) {
      dismiss();
    }
  };
  const escape = (e: KeyboardEvent): void => {
    if (e.key === "Escape") {
      e.stopPropagation();
      e.preventDefault();
      dismiss();
    }
  };
  for (const [label, run] of entries) {
    const item = document.createElement("li");
    item.className = "tab-menu-item";
    item.setAttribute("role", "menuitem");
    item.textContent = label;
    item.addEventListener("click", () => {
      dismiss();
      run();
    });
    box.append(item);
  }
  document.body.append(box);
  window.addEventListener("pointerdown", outside, true);
  window.addEventListener("keydown", escape, true);
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
