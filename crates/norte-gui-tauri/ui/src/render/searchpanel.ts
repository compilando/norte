// `Screen` painter for the search panel (spec 2026-10-09, bridge 111): a
// function with `this: Screen`, hooked in as a property in `render.ts`.
//
// Two halves with different lifetimes. The CONTROLS (the inputs, the
// switches, the fold) are built once per slot and kept: they are real
// `<input>`s, and the host's view of what was typed arrives a round trip
// late, so rebuilding them on a repaint would drop the caret and the focus.
// The TREE is the host's WINDOW onto the results: rebuilt when the window
// changes, and only its highlight is moved when just the cursor did.

import type { Screen } from "../render";
import type {
  DialogFieldValue,
  DialogFieldView,
  SearchPanelRowView,
  SearchPanelSlotView,
} from "../types";
import type { SlotDom } from "./dom";
import { OVERSCAN, badge, unchanged } from "./dom";
import { markedText } from "./menus";

/** The host never answers with more rows than this. */
const WINDOW_MAX = 500;

/**
 * The glyphs of the switches that sit next to the content input, in their
 * order: case, whole word, regular expression. Whatever other toggle there
 * is (subfolders, the kinds cycle) lives in the fold and says its label.
 */
const SWITCH_GLYPHS: readonly (readonly [string, string])[] = [
  ["case", "Aa"],
  ["whole-word", "ab"],
  ["regex", ".*"],
];

interface Controls {
  /** The set of field ids it was built for: another set is another form. */
  ids: string;
  slotId: number;
  body: HTMLElement;
  rootText: HTMLElement;
  fold: HTMLButtonElement;
  filters: HTMLElement;
  inputs: Map<string, HTMLInputElement>;
  buttons: Map<string, HTMLButtonElement>;
  status: HTMLElement;
  scroll: HTMLElement;
  tree: HTMLUListElement;
  /** The row elements of the window, in order; `first` is the first one's index. */
  rows: HTMLElement[];
  first: number;
  /** The numbering the rows were painted in: a click echoes it back. */
  generation: number;
  /** The window last asked of the host: it is only asked again if it moves. */
  lastRange: { first: number; count: number } | null;
  lastCursor: number | null;
  measuring: boolean;
  /** The slot was the active one on the last paint. */
  wasActive: boolean;
}

const controlsOf = new WeakMap<SlotDom, Controls>();

/** The panel: the form, the status sentence and the results tree. */
export function paintSearchPanel(
  this: Screen,
  dom: SlotDom,
  slot: SearchPanelSlotView,
): void {
  dom.root.setAttribute("aria-label", slot.title);
  dom.scroller.className = "searchpanel";
  dom.scroller.onwheel = null;
  dom.title.replaceChildren(document.createTextNode(slot.title));

  const ids = slot.fields.map((f) => f.id).join(",");
  let c = controlsOf.get(dom);
  if (c === undefined || c.ids !== ids) {
    c = createControls(this, slot, ids);
    controlsOf.set(dom, c);
  }
  c.slotId = slot.slot_id;
  c.generation = slot.generation;
  if (dom.scroller.firstChild !== c.body || dom.scroller.childNodes.length !== 1) {
    // The node is reused by whatever slot kind comes next: put the panel back.
    dom.scroller.replaceChildren(c.body);
  }
  paintControls(this, c, slot);
  followTheKeyboard(c, dom.root.dataset["role"] === "active");

  const h = this.cell().h;
  const treeSignature = JSON.stringify([slot.first_row, slot.total_rows, slot.rows, h]);
  if (unchanged(c.tree, treeSignature)) {
    markCursor(c, slot);
  } else {
    paintTree(c, slot, h, this.t("hostile-name"));
  }
  if (slot.cursor !== c.lastCursor && revealCursor(c, slot.cursor, h)) {
    c.lastCursor = slot.cursor;
    // A cursor that moved made the host forget the window (it carries the
    // rows around the cursor until told): say it again, even unmoved.
    c.lastRange = null;
  }
  scheduleRange(this, c);
}

function createControls(
  screen: Screen,
  slot: SearchPanelSlotView,
  ids: string,
): Controls {
  const body = document.createElement("div");
  body.className = "searchpanel-body";

  const rootLine = document.createElement("div");
  rootLine.className = "searchpanel-root";
  const rootText = document.createElement("span");
  rootText.className = "searchpanel-root-text";
  const reroot = document.createElement("button");
  reroot.type = "button";
  reroot.className = "searchpanel-reroot";
  reroot.textContent = screen.t("search-panel-reroot");
  rootLine.append(rootText, reroot);

  const form = document.createElement("div");
  form.className = "searchpanel-form";
  const filters = document.createElement("div");
  filters.className = "searchpanel-filters";
  const inputs = new Map<string, HTMLInputElement>();
  const buttons = new Map<string, HTMLButtonElement>();
  const switches: [number, HTMLButtonElement][] = [];
  const lines: HTMLElement[] = [];
  const others: HTMLElement[] = [];

  const edit = (c: () => Controls, field: string, value: DialogFieldValue): void => {
    screen.send({
      action: "search_panel_edit",
      slot_id: c().slotId,
      field,
      value,
    });
  };
  // `c` is the controls object, which does not exist yet while its own
  // handlers are being made: they look it up when they run.
  const self: { c: Controls | null } = { c: null };
  const current = (): Controls => {
    if (self.c === null) {
      throw new Error("search panel handler ran before its controls existed");
    }
    return self.c;
  };

  for (const f of slot.fields) {
    const label = screen.t(f.label_key);
    if (f.kind.kind === "text") {
      const input = document.createElement("input");
      input.type = "text";
      input.className = "searchpanel-input";
      input.dataset["field"] = f.id;
      input.placeholder = label;
      input.setAttribute("aria-label", label);
      input.spellcheck = false;
      input.addEventListener("input", () => {
        edit(current, f.id, { set: "text", text: input.value });
      });
      input.addEventListener("keydown", (e) => {
        if (e.ctrlKey || e.altKey || e.metaKey) {
          return;
        }
        if (e.key === "Enter") {
          // The host searches NOW; the document's handler must not also send
          // the Enter as a key for the listing.
          e.preventDefault();
          e.stopPropagation();
          screen.send({ action: "search_panel_go", slot_id: current().slotId });
        } else if (e.key === "ArrowDown" || e.key === "Tab" || e.key === "ArrowUp") {
          // The terminal's ring: the inputs, then the results. Kept from
          // the document's handler, which would send them to the host as
          // a cursor move or a pane switch with the caret still here.
          e.preventDefault();
          e.stopPropagation();
          const back = e.key === "ArrowUp" || (e.key === "Tab" && e.shiftKey);
          // Up from the first input goes nowhere, as in the terminal.
          if (!(e.key === "ArrowUp" && stopsOf(current())[0] === input)) {
            step(current(), input, back);
          }
        } else if (e.key === "Escape") {
          // Out of the input, and on to the host, which gives the keys
          // back to the listing: typing must not keep landing here.
          input.blur();
        }
      });
      inputs.set(f.id, input);
      (f.id === "name" || f.id === "content" ? lines : others).push(input);
      continue;
    }
    const button = document.createElement("button");
    button.type = "button";
    button.setAttribute("aria-label", label);
    button.title = label;
    button.addEventListener("click", () => {
      edit(
        current,
        f.id,
        f.kind.kind === "toggle" ? { set: "toggled" } : { set: "cycled" },
      );
    });
    buttons.set(f.id, button);
    const order = SWITCH_GLYPHS.findIndex(([id]) => id === f.id);
    if (f.kind.kind === "toggle" && order >= 0) {
      button.className = "searchpanel-switch";
      button.textContent = SWITCH_GLYPHS[order]?.[1] ?? label;
      switches.push([order, button]);
    } else {
      button.className =
        f.kind.kind === "cycle" ? "searchpanel-cycle" : "searchpanel-toggle";
      others.push(button);
    }
  }
  switches.sort((a, b) => a[0] - b[0]);
  const nameInput = lines.at(0);
  const contentInput = lines.at(1);
  if (nameInput !== undefined) {
    form.append(nameInput);
  }
  if (contentInput !== undefined) {
    const row = document.createElement("div");
    row.className = "searchpanel-content-row";
    row.append(contentInput, ...switches.map(([, b]) => b));
    form.append(row);
  }

  const fold = document.createElement("button");
  fold.type = "button";
  fold.className = "searchpanel-fold";
  fold.addEventListener("click", () => {
    screen.send({ action: "search_panel_filters", slot_id: current().slotId });
  });
  filters.append(...others);

  const status = document.createElement("p");
  status.className = "searchpanel-status";
  status.setAttribute("role", "status");
  status.setAttribute("aria-live", "polite");

  const scroll = document.createElement("div");
  scroll.className = "searchpanel-scroll";
  const tree = document.createElement("ul");
  tree.className = "searchpanel-tree";
  tree.setAttribute("role", "tree");
  tree.tabIndex = -1;
  scroll.append(tree);

  tree.addEventListener("click", (e) => {
    const target = e.target instanceof Element ? e.target : null;
    const li = target?.closest<HTMLElement>("li.searchpanel-row");
    const row = Number(li?.dataset["row"]);
    if (li === null || li === undefined || !Number.isInteger(row)) {
      return;
    }
    if (target?.closest(".searchpanel-chevron") != null) {
      screen.send({
        action: "search_panel_fold",
        slot_id: current().slotId,
        row,
        open: li.dataset["open"] !== "true",
        generation: current().generation,
      });
      return;
    }
    screen.send({
      action: "search_panel_activate",
      slot_id: current().slotId,
      row,
      generation: current().generation,
    });
  });
  tree.addEventListener("keydown", (e) => {
    if (e.ctrlKey || e.altKey || e.metaKey) {
      return;
    }
    if (e.key === "Tab") {
      e.preventDefault();
      e.stopPropagation();
      step(current(), tree, e.shiftKey);
    } else if (e.key === "Escape") {
      // The host gives the keys back to the listing; the DOM focus goes too.
      tree.blur();
    }
    // The rest — arrows, pages, Enter, ←/→ — goes on to the host, which
    // moves the cursor of the active panel.
  });
  scroll.addEventListener("scroll", () => {
    measureRange(screen, current());
  });
  reroot.addEventListener("click", () => {
    screen.send({ action: "search_panel_reroot", slot_id: current().slotId });
  });

  body.append(rootLine, form, fold, filters, status, scroll);
  const c: Controls = {
    ids,
    slotId: slot.slot_id,
    body,
    rootText,
    fold,
    filters,
    inputs,
    buttons,
    status,
    scroll,
    tree,
    rows: [],
    first: 0,
    generation: slot.generation,
    lastRange: null,
    lastCursor: null,
    measuring: false,
    wasActive: false,
  };
  self.c = c;
  return c;
}

/**
 * The DOM focus follows the host's: the panel becoming the active slot (it
 * was opened, or Tab reached it) puts the caret in the name input — unless
 * a click already put the focus somewhere inside it — and losing it takes
 * the focus out, so typing does not keep landing in an input the host no
 * longer reads as the keyboard's.
 */
function followTheKeyboard(c: Controls, active: boolean): void {
  if (active === c.wasActive) {
    return;
  }
  c.wasActive = active;
  const focused = document.activeElement;
  const inside = focused !== null && c.body.contains(focused);
  if (active && !inside) {
    stopsOf(c)[0]?.focus();
  } else if (!active && inside && focused instanceof HTMLElement) {
    focused.blur();
  }
}

/** Tab's stops, in the terminal's order: the inputs in view, then the results. */
function stopsOf(c: Controls): HTMLElement[] {
  const inputs = [...c.inputs.values()].filter(
    (i) => !(c.filters.hidden && c.filters.contains(i)),
  );
  return [...inputs, c.tree];
}

/** Moves the focus from `from` to the next stop (or the previous, `back`). */
function step(c: Controls, from: HTMLElement, back: boolean): void {
  const stops = stopsOf(c);
  const at = stops.indexOf(from);
  const next = (at + (back ? stops.length - 1 : 1)) % stops.length;
  stops[next]?.focus();
}

/** Everything that is not the tree, brought in line with the host's view. */
function paintControls(screen: Screen, c: Controls, slot: SearchPanelSlotView): void {
  const rootText = screen
    .t("search-panel-root")
    .replace(/\{\s*\$root\s*\}/u, () => slot.root);
  if (c.rootText.textContent !== rootText) {
    c.rootText.textContent = rootText;
  }
  c.rootText.dataset["hostile"] = String(slot.root_hostile);
  if (slot.root_hostile) {
    c.rootText.title = screen.t("hostile-name");
  } else {
    c.rootText.removeAttribute("title");
  }
  for (const f of slot.fields) {
    paintField(screen, c, f, slot.unreadable === f.id);
  }
  const fold = screen
    .t("search-panel-filters")
    .replace(/\{\s*\$n\s*\}/u, () => String(slot.filters_set));
  if (c.fold.textContent !== fold) {
    c.fold.textContent = fold;
  }
  c.fold.setAttribute("aria-expanded", String(slot.filters_open));
  c.filters.hidden = !slot.filters_open;
  if (c.status.textContent !== slot.status) {
    c.status.textContent = slot.status;
  }
  c.status.dataset["dimmed"] = String(slot.dimmed);
  c.status.dataset["running"] = String(slot.running);
}

function paintField(
  screen: Screen,
  c: Controls,
  f: DialogFieldView,
  invalid: boolean,
): void {
  const input = c.inputs.get(f.id);
  if (input !== undefined) {
    // The caret is the user's: what the host echoes only lands on an input
    // nobody is typing in.
    if (input.value !== f.value && document.activeElement !== input) {
      input.value = f.value;
    }
    input.dataset["hostile"] = String(f.hostile);
    if (invalid) {
      input.setAttribute("aria-invalid", "true");
    } else {
      input.removeAttribute("aria-invalid");
    }
    return;
  }
  const button = c.buttons.get(f.id);
  if (button === undefined) {
    return;
  }
  if (f.kind.kind === "toggle") {
    button.setAttribute("aria-pressed", String(f.kind.on));
    if (!button.classList.contains("searchpanel-switch")) {
      const text = screen.t(f.label_key);
      if (button.textContent !== text) {
        button.textContent = text;
      }
    }
  } else if (f.kind.kind === "cycle") {
    const text = `${screen.t(f.label_key)}: ${screen.t(f.kind.value_key)}`;
    if (button.textContent !== text) {
      button.textContent = text;
    }
    button.dataset["value"] = f.kind.value_key;
  }
}

/** Rebuilds the tree's window between its two spacers. */
function paintTree(
  c: Controls,
  slot: SearchPanelSlotView,
  h: number,
  hostileLabel: string,
): void {
  const before = spacer(slot.first_row * h);
  const shown = slot.first_row + slot.rows.length;
  const after = spacer(Math.max(0, slot.total_rows - shown) * h);
  c.rows = slot.rows.map((r, i) =>
    rowNode(slot.slot_id, r, slot.first_row + i, hostileLabel),
  );
  c.first = slot.first_row;
  c.tree.replaceChildren(before, ...c.rows, after);
  markCursor(c, slot);
}

function spacer(px: number): HTMLElement {
  const li = document.createElement("li");
  li.className = "searchpanel-spacer";
  li.setAttribute("role", "presentation");
  li.setAttribute("aria-hidden", "true");
  li.style.height = `${String(px)}px`;
  return li;
}

/**
 * Moves the highlight. On the SAME nodes: a cursor that moved does not
 * justify rebuilding rows, and a rebuild under the pointer drops `:hover`
 * and the click that was in flight.
 */
function markCursor(c: Controls, slot: SearchPanelSlotView): void {
  let active: string | null = null;
  for (const [i, el] of c.rows.entries()) {
    const selected = slot.cursor === c.first + i;
    el.setAttribute("aria-selected", String(selected));
    if (selected) {
      active = el.id;
    }
  }
  if (active === null) {
    c.tree.removeAttribute("aria-activedescendant");
  } else {
    c.tree.setAttribute("aria-activedescendant", active);
  }
}

function rowNode(
  slotId: number,
  r: SearchPanelRowView,
  index: number,
  hostileLabel: string,
): HTMLElement {
  const li = document.createElement("li");
  li.className = `searchpanel-row searchpanel-${r.row}`;
  li.id = `searchpanel-${String(slotId)}-${String(index)}`;
  li.dataset["row"] = String(index);
  li.setAttribute("role", "treeitem");
  switch (r.row) {
    case "folder": {
      li.setAttribute("aria-level", "1");
      li.setAttribute("aria-expanded", String(r.open));
      li.dataset["open"] = String(r.open);
      const chevron = document.createElement("span");
      chevron.className = "searchpanel-chevron";
      chevron.setAttribute("aria-hidden", "true");
      chevron.textContent = r.open ? "▾" : "▸";
      li.append(
        chevron,
        ...nameNodes(r.path, r.hostile, hostileLabel),
        countNode(r.count),
      );
      break;
    }
    case "file":
      li.setAttribute("aria-level", "2");
      li.append(...nameNodes(r.name, r.hostile, hostileLabel), countNode(r.count));
      break;
    case "line": {
      li.setAttribute("aria-level", "3");
      const no = document.createElement("span");
      no.className = "searchpanel-lineno";
      no.textContent = String(r.line);
      const preview = document.createElement("span");
      preview.className = "searchpanel-preview";
      preview.append(markedText(r.preview, positionsOf(r.preview, r.ranges)));
      li.append(no, preview);
      break;
    }
    case "more":
      li.setAttribute("aria-level", "3");
      li.textContent = r.label;
      break;
  }
  return li;
}

/** A name, with the hostile badge IN FRONT as everywhere a name is read. */
function nameNodes(text: string, hostile: boolean, hostileLabel: string): HTMLElement[] {
  const name = document.createElement("span");
  name.className = "searchpanel-name";
  name.textContent = text;
  return hostile ? [badge(hostileLabel), name] : [name];
}

function countNode(n: number): HTMLElement {
  const count = document.createElement("span");
  count.className = "searchpanel-count";
  count.textContent = String(n);
  return count;
}

/**
 * The code points the `[start, end)` ranges cover, bounded by the text:
 * `markedText` takes indices, and a range past the end is the host's
 * rounding and not a reason to loop over it.
 */
function positionsOf(text: string, ranges: readonly [number, number][]): number[] {
  const len = Array.from(text).length;
  const out: number[] = [];
  for (const [start, end] of ranges) {
    for (let i = Math.max(0, start); i < Math.min(end, len); i += 1) {
      out.push(i);
    }
  }
  return out;
}

/**
 * Brings the cursor's row into the scroller, and nothing else moves. False
 * when it could not (no cursor, or the scroller has no height yet): the
 * caller asks again on the next paint.
 */
function revealCursor(c: Controls, cursor: number | null, h: number): boolean {
  const view = c.scroll.clientHeight;
  if (cursor === null) {
    return true;
  }
  if (view === 0) {
    return false;
  }
  const top = cursor * h;
  if (top < c.scroll.scrollTop) {
    c.scroll.scrollTop = top;
  } else if (top + h > c.scroll.scrollTop + view) {
    c.scroll.scrollTop = top + h - view;
  }
  return true;
}

/** Measures after the layout, once per paint. */
function scheduleRange(screen: Screen, c: Controls): void {
  if (c.measuring) {
    return;
  }
  c.measuring = true;
  requestAnimationFrame(() => {
    c.measuring = false;
    measureRange(screen, c);
  });
}

/**
 * The rows in view, plus a margin: sent to the host only when they moved.
 * The host decides the window (it keeps the cursor inside), so what is
 * compared is what was last ASKED, not what came back.
 */
function measureRange(screen: Screen, c: Controls): void {
  const view = c.scroll.clientHeight;
  if (view === 0) {
    return;
  }
  const h = screen.cell().h;
  const first = Math.max(0, Math.floor(c.scroll.scrollTop / h) - OVERSCAN);
  const count = Math.min(WINDOW_MAX, Math.ceil(view / h) + 2 * OVERSCAN);
  if (c.lastRange?.first === first && c.lastRange.count === count) {
    return;
  }
  c.lastRange = { first, count };
  screen.send({ action: "search_panel_visible_range", slot_id: c.slotId, first, count });
}
