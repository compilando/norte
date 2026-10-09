// The right-click menu (spec 2026-10-09, bridge 107): a function with
// `this: Screen`, hooked in as a property in `render.ts`.
//
// The menu is the HOST's: which entries, which can run, where the cursor is,
// and every key (Escape, arrows, Enter) are decided there. This file only
// paints a `ContextMenuView` and reports where the pointer went — hover,
// click, a press outside, the window losing focus. A menu decided here would
// be a second dispatcher.

import type { Screen } from "../render";
import type { ContextMenuView } from "../types";
import { dismissPopupMenu, placeInsideWindow } from "./menus";

/** Takes down the painted menu's window listeners; `null` with none. */
let teardown: (() => void) | null = null;

const guarded = new WeakSet<Document>();

/**
 * Keeps the webview's own context menu from ever opening, except inside a
 * text field (where Cut/Copy/Paste are wanted).
 *
 * WebKitGTK's would offer Back, Reload and Inspect Element over a file
 * manager's rows: none of them means anything here, and the real menu is the
 * host's. Installed ONCE from `main.ts`; idempotent per document so a test
 * calling it again does not stack listeners.
 */
export function installContextMenuGuard(doc: Document): void {
  if (guarded.has(doc)) {
    return;
  }
  guarded.add(doc);
  doc.addEventListener("contextmenu", (e) => {
    const t = e.target;
    if (t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement) {
      return;
    }
    e.preventDefault();
  });
}

/** Tells the host to close its context menu, if one is painted. Used by the
 *  other pop-up menus when they open: two menus at once is never right. */
export function closeHostMenu(this: Screen): void {
  if (document.querySelector(".context-menu") !== null) {
    this.send({ action: "context_menu_close" });
  }
}

/** Where a menu asked by KEY goes: under the focused panel's cursor row. */
function cursorAnchor(): { x: number; y: number } {
  // `.row`, `.places-row` and `.tree-row` all carry `aria-selected`.
  const row = document.querySelector('[aria-current="true"] [aria-selected="true"]');
  if (row === null) {
    return { x: 0, y: 0 };
  }
  const r = row.getBoundingClientRect();
  return { x: r.left + 16, y: r.bottom };
}

/**
 * Paints the host's context menu, or takes it down (`null`).
 *
 * Every string is written with `textContent`: the header carries a file's
 * name, which is hostile input. A disabled entry still shows, with its
 * reason, and still sends its click: the host decides what that means.
 */
export function paintContextMenu(this: Screen, menu: ContextMenuView | null): void {
  teardown?.();
  teardown = null;
  document.querySelector(".context-menu")?.remove();
  if (menu === null) {
    return;
  }
  // The other way round from `closeHostMenu`: a menu opened by KEY over a
  // tab's or a terminal's popup takes that popup down, listeners included.
  dismissPopupMenu();
  const box = document.createElement("div");
  box.className = "context-menu";
  box.setAttribute("role", "menu");
  const header = document.createElement("div");
  header.className = "context-menu-header";
  header.textContent = menu.header;
  box.append(header);
  for (const [i, item] of menu.items.entries()) {
    if (item.section !== null) {
      const rule = document.createElement("div");
      rule.className = "context-menu-rule";
      rule.setAttribute("role", "separator");
      box.append(rule);
      if (item.section !== "") {
        const title = document.createElement("div");
        title.className = "context-menu-section";
        title.textContent = item.section;
        box.append(title);
      }
    }
    const el = document.createElement("div");
    el.className = "context-menu-item";
    el.setAttribute("role", "menuitem");
    el.dataset["role"] = item.role;
    el.setAttribute("aria-selected", String(i === menu.cursor));
    el.setAttribute("aria-disabled", String(!item.enabled));
    const line = document.createElement("div");
    line.className = "context-menu-line";
    const label = document.createElement("span");
    label.className = "context-menu-label";
    label.textContent = item.label;
    const chord = document.createElement("span");
    chord.className = "context-menu-chord";
    chord.textContent = item.chord;
    line.append(label, chord);
    el.append(line);
    if (item.reason !== "") {
      const reason = document.createElement("div");
      reason.className = "context-menu-reason";
      reason.textContent = item.reason;
      el.append(reason);
    }
    el.addEventListener("mousemove", () => {
      if (i !== menu.cursor) {
        this.send({ action: "context_menu_point_row", row: i });
      }
    });
    el.addEventListener("click", () => {
      this.send({ action: "context_menu_activate_row", row: i });
    });
    box.append(el);
  }
  document.body.append(box);
  const anchor =
    menu.x !== null && menu.y !== null ? { x: menu.x, y: menu.y } : cursorAnchor();
  placeInsideWindow(box, anchor.x, anchor.y);

  const close = (): void => {
    this.send({ action: "context_menu_close" });
  };
  const outside = (e: Event): void => {
    if (!(e.target instanceof Node) || !box.contains(e.target)) {
      close();
    }
  };
  // A scroll of the slot under it moves the rows the menu was opened on
  // away from it (spec §5). The wheel in CAPTURE, on the window: listings
  // scroll in their own scrollers, whose `scroll` does not bubble. A wheel
  // INSIDE the menu is the menu scrolling (a tall one does), not a close:
  // the same "outside" test as a press.
  window.addEventListener("pointerdown", outside, true);
  window.addEventListener("wheel", outside, { capture: true, passive: true });
  window.addEventListener("blur", close);
  window.addEventListener("resize", close);
  teardown = () => {
    window.removeEventListener("pointerdown", outside, true);
    window.removeEventListener("wheel", outside, true);
    window.removeEventListener("blur", close);
    window.removeEventListener("resize", close);
  };
}
