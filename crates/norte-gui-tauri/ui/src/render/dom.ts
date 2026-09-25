// The renderer's DOM helpers, shared by `render.ts` and by the `render/*`
// painters (wave W10: the single 4,400-line file).

import type {
  CompareRowView,
  RowView,
  SlotPlacement,
  StatusItemView,
  StatusView,
  TaskView,
  UiAction,
  ViewerView,
} from "../types";

/**
 * Scrolls just enough for `el` to be visible, if the environment knows how.
 *
 * `scrollIntoView` does not exist in jsdom, where the renderer's tests run:
 * without the guard, checking a list's paint brought down a test over a call
 * that has nothing to do with painting.
 */
export function revealInView(el: Element | undefined): void {
  if (el instanceof HTMLElement && typeof el.scrollIntoView === "function") {
    el.scrollIntoView({ block: "nearest" });
  }
}

/** A paragraph with a sentence the host already wrote. */
export function note(text: string): HTMLElement {
  const p = document.createElement("p");
  p.className = "slot-note";
  p.textContent = text;
  return p;
}

/**
 * A viewer scrollbar, or `null` if everything fits.
 *
 * OUR OWN and not the browser's: the host only sends the visible window, so
 * the `pre` measures exactly what is shown and `overflow` has nothing to
 * scroll. Without a bar, the viewer only said "there's more" in the header's
 * count, and sideways it said nothing at all — and the viewer does not wrap,
 * so a file cut off on the right reads as a short file.
 *
 * It does not drag: it is an INDICATOR. Dragging it would require
 * translating pixels to lines on the renderer's side, which is exactly what
 * the host already does for the wheel. That is why it is `aria-hidden` and
 * NOT `role="scrollbar"`: that role promises a control that does not exist
 * and requires an `aria-controls` that is not there. Whoever cannot see it
 * reads the position in the header's marks, which say it in words.
 *
 * `visible === 0` is the measurement from BEFORE painting — the body does
 * not have a height yet — and then nothing is drawn: with `Math.max(1, 0)` a
 * one-pixel thumb used to show up for one frame.
 */
export function viewerBar(
  vertical: boolean,
  total: number,
  first: number,
  visible: number,
): HTMLElement | null {
  if (visible <= 0 || total <= visible) {
    return null;
  }
  const bar = document.createElement("div");
  bar.className = vertical ? "viewer-bar viewer-bar-v" : "viewer-bar viewer-bar-h";
  bar.setAttribute("aria-hidden", "true");
  const thumb = document.createElement("div");
  thumb.className = "viewer-thumb";
  const length = visible / total;
  const where = Math.min(1, Math.max(0, first / (total - visible)));
  const pct = (x: number): string => `${(x * 100).toFixed(2)}%`;
  if (vertical) {
    thumb.style.height = pct(length);
    thumb.style.top = pct((1 - length) * where);
  } else {
    thumb.style.width = pct(length);
    thumb.style.left = pct((1 - length) * where);
  }
  bar.append(thumb);
  return bar;
}

/**
 * A viewer's body: the lines, or the styled fragments if a plugin put them
 * there. Shared by the full-screen viewer and the docked one (#291): it is
 * the same viewer elsewhere, and two bodies drift apart.
 *
 * Always `textContent`: the text was written by a plugin. The role goes in
 * `data-role`, which the stylesheet maps to the theme's variables, and its
 * own color only when there is no role — the reader's theme rules over the
 * plugin's fixed palette. The background has no role to set it: half a block
 * with no background is half an image (bridge 50).
 */
export function viewerBody(viewer: ViewerView): HTMLElement {
  const body = document.createElement("pre");
  body.className = viewer.hex ? "viewer-body hexview" : "viewer-body";
  if (viewer.styled.length === 0) {
    body.textContent = viewer.lines.join("\n");
    return body;
  }
  for (const line of viewer.styled) {
    const row = document.createElement("div");
    row.className = "viewer-line";
    for (const s of line) {
      const el = document.createElement("span");
      el.className = "viewer-span";
      el.textContent = s.text;
      if (s.role !== null) {
        el.dataset["role"] = s.role;
      } else if (s.fg !== null) {
        el.style.color = s.fg;
      }
      if (s.bg !== null) {
        el.style.backgroundColor = s.bg;
      }
      row.append(el);
    }
    body.append(row);
  }
  return body;
}

/**
 * Extra rows requested above and below the visible slot.
 *
 * The engine moves the scroll on the spot and the new rows arrive from the
 * host a round trip later: whatever falls outside this margin shows up BLANK
 * during that trip, and with the wheel or the trackpad that is a flicker at
 * the edge. Eight rows used to run out in two wheel notches; twenty-four
 * cover a normal gesture for a few more KB per response.
 */
export const OVERSCAN = 24;

/** The last signature each node was painted with. */
const signatures = new WeakMap<object, string>();

/**
 * Has `node` already been painted with exactly this data? If not, records
 * the new signature and answers `false`, and the caller repaints.
 *
 * Exists because the host sends the WHOLE view on every update and `paint()`
 * dumps it whole: every response to a scroll used to redo the menu bar, the
 * panel bar, the key bar, the tabs, the title, the header and every visible
 * row, even if only the edge rows had changed. Redoing an identical node is
 * not free nor invisible: the layout gets recomputed, `:hover` is lost and
 * comes back, and on WebKitGTK it shows as a subtle flicker while scrolling.
 */
/** Which row (the object) and where each node was painted, for `updateRow`. */
const paintedRows = new WeakMap<
  HTMLElement,
  { row: RowView; selected: boolean; index: number; rowH: number; iconColumn: boolean }
>();

export function unchanged(node: object, signature: string): boolean {
  if (signatures.get(node) === signature) {
    return true;
  }
  signatures.set(node, signature);
  return false;
}

export type Send = (action: UiAction) => void;

export interface SlotDom {
  root: HTMLElement;
  /** The tab bar, empty when the slot is not in a group. */
  tabs: HTMLElement;
  title: HTMLElement;
  header: HTMLElement;
  scroller: HTMLElement;
  canvas: HTMLElement;
  /** The listing's footer (counts, marked, free space). Empty = hidden. */
  footer: HTMLElement;
  rows: Map<number, HTMLElement>;
  /**
   * The "waiting" notice, STABLE. Not created on every paint because its
   * threshold is an `animation-delay`, and an animation that starts from
   * zero every time its node is born never reaches 250 ms: `paint()`
   * repaints every slot on every update, so the notice would never have
   * shown up in the slow cases, which are what it exists for.
   */
  busy: HTMLElement;
  lastRange: { first: number; count: number } | null;
  /**
   * The generation that was PAINTED. Every row action carries it: without
   * it the key is an index, and an index from the previous screen names a
   * different file. The host compares it and answers `stale` if it does not
   * match.
   */
  generation: number;
}

/**
 * The mark ruler's color: the mark's, lightened toward the text so a
 * three-pixel band can be read. From two theme roles, with no variable of
 * its own the theme would not feed.
 */
export const MARK_RULER_COLOR = "color-mix(in srgb, var(--mark-bg) 55%, var(--fg))";

/**
 * The mark ruler as a background image (ADR 0135): one band per run of
 * consecutive marked spans, as a percentage of the height. Empty string =
 * no ruler.
 *
 * A BACKGROUND of the scrollable and not a node: the background of an
 * element with scroll stays still while its content moves, which is exactly
 * what a ruler for the whole listing has to do, and this way nothing needs
 * measuring when painting.
 */
export function markRulerImage(runs: readonly number[], spans: number): string {
  if (runs.length === 0 || spans <= 0) {
    return "";
  }
  // Four decimals: more than enough for any screen height, and without them
  // `11/20` comes out `55.00000000000001%`.
  const pct = (t: number): string =>
    `${String(Number(((Math.min(t, spans) / spans) * 100).toFixed(4)))}%`;
  const c = MARK_RULER_COLOR;
  const stops: string[] = ["transparent 0%"];
  let i = 0;
  while (i < runs.length) {
    const from = runs[i] ?? 0;
    let to = from;
    // Runs: consecutive spans are ONE band, not two hundred stops.
    while (i + 1 < runs.length && runs[i + 1] === to + 1) {
      to += 1;
      i += 1;
    }
    stops.push(
      `transparent ${pct(from)}`,
      `${c} ${pct(from)}`,
      `${c} ${pct(to + 1)}`,
      `transparent ${pct(to + 1)}`,
    );
    i += 1;
  }
  return `linear-gradient(to bottom, ${stops.join(", ")})`;
}

export function place(
  el: HTMLElement,
  p: SlotPlacement,
  cell: { w: number; h: number },
): void {
  el.style.setProperty("left", `${p.x * cell.w}px`);
  el.style.setProperty("top", `${p.y * cell.h}px`);
  el.style.setProperty("width", `${p.width * cell.w}px`);
  el.style.setProperty("height", `${p.height * cell.h}px`);
}

export function newRow(dom: SlotDom, slotId: number, key: number): HTMLElement {
  const el = document.createElement("div");
  el.className = "row";
  // STABLE id: `aria-activedescendant` points to it, and an id that changes
  // on repaint leaves the screen reader pointing at a node that is no longer
  // there.
  el.id = `row-${String(slotId)}-${String(key)}`;
  el.setAttribute("role", "row");
  el.dataset["key"] = String(key);
  dom.rows.set(key, el);
  dom.canvas.append(el);
  return el;
}

export function updateRow(
  el: HTMLElement,
  row: RowView,
  index: number,
  rowH: number,
  // The icon column is open in this slot (bridge 62): SOME row has an icon,
  // so all of them carry the cell, empty or not, so names stay aligned.
  // Decided by whoever paints the slot, not by the row.
  iconColumn = false,
): void {
  // Everything this function reads is in the signature: the whole row, its
  // position, the height and the icon column. If nothing changed, the node
  // already says what it has to say.
  //
  // First by IDENTITY, which costs nothing: the session replaces the rows
  // when a batch arrives and only mutates `selected` in place (the cursor's
  // patch), so the same object with the same `selected` is the same row.
  // Serializing every visible row on every repaint — and every patch
  // repaints — was wasted work in the most common case.
  const previous = paintedRows.get(el);
  if (
    previous !== undefined &&
    previous.row === row &&
    previous.selected === row.selected &&
    previous.index === index &&
    previous.rowH === rowH &&
    previous.iconColumn === iconColumn
  ) {
    return;
  }
  paintedRows.set(el, { row, selected: row.selected, index, rowH, iconColumn });
  if (unchanged(el, JSON.stringify([row, index, rowH, iconColumn]))) {
    return;
  }
  el.style.setProperty("top", `${index * rowH}px`);
  el.setAttribute("aria-rowindex", String(index + 1));
  el.setAttribute("aria-selected", String(row.selected));
  el.dataset["marked"] = String(row.marked);
  // The PAINTED row's parity, for the row stripes (spec 2026-09-20). Always
  // set, whether the setting is on or not: whoever decides if it shows is
  // the container (`data-stripes`), so a row recycled by scroll does not
  // drag along the band of the position it used to occupy.
  el.dataset["odd"] = String(index % 2 === 1);
  el.className = `row kind-${row.kind}`;
  const name = document.createElement("span");
  name.className = row.hostile ? "cell-name hostile" : "cell-name";
  name.setAttribute("role", "gridcell");
  name.textContent = row.display_name;
  // The color the THEME gives this entry (bridge 66). Applied inline
  // because the value comes resolved from the host: `[files.ext]` is an
  // open set and there is no CSS class that could represent it.
  //
  // Not painted under the CURSOR, and that mirrors the terminal: there the
  // selected row's style is applied with `highlight_style`, which OVERRIDES
  // the item's when the theme gives `selection` a foreground — and all ten
  // presets do. Without this exception, a dark blue directory over
  // vscode-dark's #04395e would end up unreadable in exactly the row the
  // reader is looking at.
  if (row.name_color !== "" && !row.selected) {
    name.style.color = row.name_color;
  }
  if (row.name_bold) {
    name.style.fontWeight = "bold";
  }
  // `dim` is the terminal attribute, not a theme opacity: 0.6 is what
  // ratatui paints for `Modifier::DIM` in practice. The retro presets dim
  // `zip`/`tar`/`gz` this way, and without this they came out dim in `ntc`
  // and at full brightness here.
  if (row.name_dim) {
    name.style.opacity = "0.6";
  }
  if (row.name_italic) {
    name.style.fontStyle = "italic";
  }
  if (row.name_underline) {
    name.style.textDecoration = "underline";
  }
  // The name and what decorates it, together and on the LEFT; the column
  // cells follow on the right. The block is what grows, so the name can be
  // ellipsis-truncated WITHOUT taking the badge down with it: the TUI, which
  // cannot do that, has to drop the whole decoration when the name does not
  // fit.
  const block = document.createElement("span");
  block.className = "name-block";
  if (iconColumn) {
    // The icon, BEFORE the name and in its own fixed-width node: it is a
    // plugin's text, and the cell exists even when this row has no icon,
    // which is what keeps the column in place.
    const icon = document.createElement("span");
    icon.className = "cell-icon";
    // With the ENTRY's color, which is ADR 0105's decision: an icon says
    // what the row IS, not what state it is in, so it follows the same
    // color as its name. The terminal has done this ever since
    // (`norte-tui/src/ui/pane.rs`); here there was no entry color to follow
    // until bridge 66, and leaving it loose now would have split the two
    // surfaces right when giving them color.
    if (row.name_color !== "" && !row.selected) {
      icon.style.color = row.name_color;
    }
    icon.dataset["hostile"] = String(row.icon_hostile);
    icon.textContent = row.icon;
    if (row.icon_hostile) {
      icon.append(badge("△"));
    }
    block.append(icon);
  }
  block.append(name);
  const nodes: Node[] = [block];
  if (row.hostile) {
    // A name that paints different from the real one is SAID. Never hidden.
    name.append(badge("△"));
  }
  if (row.badge !== "") {
    // What a PLUGIN says about this row, INSIDE the name's block and right
    // behind it, as in the TUI. Loose between the name and the first cell it
    // floated to the right — `.cell-name` is `flex: 1` — and read as part of
    // the size column: the same badge said two different things depending
    // on who painted it.
    //
    // In its own NODE, not in the same text: they are two data points from
    // two origins, and `unicode-bidi: isolate` does not separate two things
    // that are concatenated.
    //
    // The color comes from the ROLE, the theme's closed vocabulary: a plugin
    // does not choose its own.
    const mark = document.createElement("span");
    mark.className = "cell-badge";
    mark.dataset["role"] = row.badge_role;
    mark.dataset["hostile"] = String(row.badge_hostile);
    mark.textContent = row.badge;
    if (row.badge_hostile) {
      mark.append(badge("△"));
    }
    block.append(mark);
  }
  for (const c of row.cells) {
    const cell = document.createElement("span");
    cell.className = "cell";
    cell.setAttribute("role", "gridcell");
    cell.textContent = c.text ?? "";
    // Width and alignment are DECLARED by the slot's header as variables on
    // its root (bridge 64); a cell only reads them. With no variable, `auto`
    // and `left`: the usual.
    const v = colVar(c.column);
    cell.style.width = `var(${v}, auto)`;
    cell.style.textAlign = `var(${v}-align, left)`;
    // A column the header DROPPED for not fitting (`${v}-show: none`) leaves
    // every row at once, without repainting them.
    cell.style.display = `var(${v}-show, block)`;
    nodes.push(cell);
  }
  // The mark checkbox goes FIRST: a reserved gap that paints on mouseover or
  // when the row is marked (see `.row-check`).
  const check = document.createElement("span");
  check.className = "row-check";
  check.setAttribute("aria-hidden", "true");
  check.textContent = row.marked ? "☑" : "☐";
  // The bar for the task that has THIS file in hand (bridge 69, ADR 0115).
  // Behind the text and not between the cells: the row already says what it
  // is, and progress is a state of its own, not one more column.
  // `aria-hidden` because the process dashboard is the one that announces
  // it; repeating it per row would turn a long copy into a chant for a
  // screen reader.
  const background: Node[] = [];
  if (row.progress !== null && row.progress !== undefined) {
    const bar = document.createElement("span");
    bar.className = "row-progress";
    bar.setAttribute("aria-hidden", "true");
    bar.style.setProperty(
      "--pct",
      `${String(Math.max(0, Math.min(100, row.progress)))}%`,
    );
    // FIRST among its siblings, which is what leaves it underneath: this
    // sheet has no `z-index` anywhere, so stacking is document order, and a
    // sheet painted last would tint the name the row is stating.
    background.push(bar);
  }
  el.replaceChildren(...background, check, ...nodes);
}

/**
 * The CSS variable name carrying a column's width on its slot's root
 * (`--colw-<id>`), and with the `-align` suffix, its alignment. A column's
 * id is an open set (`attr:posix.mode`, `plugin:git-status`) and a variable
 * name allows neither `:` nor `.`: they are substituted. Two ids that
 * collide after that share a width, and that is a case the configuration
 * does not produce.
 */
export function colVar(id: string): string {
  return `--colw-${id.replace(/[^A-Za-z0-9_-]/g, "_")}`;
}

/** A small header label (level, filter, log source). */
export function chip(text: string): HTMLElement {
  const c = document.createElement("span");
  c.className = "chip";
  c.textContent = text;
  return c;
}

export function badge(text: string): HTMLElement {
  const b = document.createElement("span");
  b.className = "hostile-badge";
  b.textContent = text;
  return b;
}

export function emptyNode(text: string): HTMLElement {
  const d = document.createElement("div");
  d.className = "empty";
  d.textContent = text;
  return d;
}

export function errorNode(text: string, detail: string | null): HTMLElement {
  const d = document.createElement("div");
  d.className = "error";
  d.setAttribute("role", "alert");
  d.textContent = detail === null ? text : `${text}: ${detail}`;
  return d;
}

/** The two glyphs in the middle of a compared row, already translated by
 *  the host: the verdict and how confident it is. */
export function verdict(r: CompareRowView, tr: (k: string) => string): HTMLElement {
  const el = document.createElement("span");
  el.className = "compare-verdict";
  el.textContent = r.verdict;
  const conf = document.createElement("span");
  conf.className = "compare-confidence";
  conf.textContent = r.confidence;
  el.append(conf);
  if (r.reason !== null) {
    const reason = document.createElement("span");
    reason.className = "compare-reason";
    reason.textContent = r.reason;
    el.append(reason);
  }
  el.title = tr("compare-title");
  return el;
}

export function statusNodes(
  status: StatusView,
  connection: string,
  tr: (k: string) => string,
  rejection: string | null = null,
  items: StatusItemView[] = [],
  onItem: ((id: string) => void) | null = null,
): Node[] {
  const nodes: Node[] = [];
  if (rejection !== null) {
    // In front of everything: it is the only thing on this bar the host
    // does not know.
    const el = document.createElement("span");
    el.className = "banner rejected";
    el.textContent = rejection;
    nodes.push(el);
  }
  for (const b of status.banners) {
    const el = document.createElement("span");
    el.className = "banner";
    el.textContent = b.text;
    if (b.subject !== null) {
      // The connection, in its own element and labeled. NEVER as
      // `scheme://host` inside the sentence: a host can be named
      // `bank.example@evil.example` without carrying a single character
      // that gets masked, and it would read there as a legitimate host's
      // userinfo.
      const subject = document.createElement("span");
      subject.className = "banner-subject";
      subject.dataset["hostile"] = String(b.subject.hostile);
      const scheme = document.createElement("span");
      scheme.className = "banner-scheme";
      scheme.textContent = b.subject.scheme;
      const host = document.createElement("span");
      host.className = "banner-host";
      host.textContent = b.subject.host;
      subject.append(scheme, host);
      // The REASON, in its own element and for the same cause as the
      // connection: it is text already translated by the host, and it is
      // not interpolated into the sentence. Without it, a reason the host
      // does not know used to read the same as "plaintext FTP" — a security
      // warning stating a cause nobody gave.
      const reason = document.createElement("span");
      reason.className = "banner-reason";
      reason.textContent = b.subject.reason;
      subject.append(reason);
      // The detail only comes with an unknown reason, and it already
      // arrives masked and bounded: it is text from the other end.
      if (b.subject.detail !== undefined && b.subject.detail !== "") {
        const detail = document.createElement("span");
        detail.className = "banner-detail";
        detail.textContent = b.subject.detail;
        subject.append(detail);
      }
      if (b.subject.hostile) {
        subject.classList.add("hostile");
        subject.append(badge(tr("hostile-name")));
      }
      el.append(subject);
    }
    nodes.push(el);
  }
  if (connection !== "connected") {
    const el = document.createElement("span");
    el.className = "banner";
    el.textContent = connection;
    nodes.push(el);
  }
  // The ephemeral message is a TOAST (spec 2026-09-11, V5): with its own
  // class, the stylesheet pulls it out of the bar to the bottom-right corner
  // while it lasts; the host expires it (`[ui] notice_seconds`) and then the
  // node is left empty and unpainted. It stays inside the bar's live region,
  // so a screen reader announces it all the same.
  const msg = document.createElement("span");
  msg.className = "status-message";
  msg.textContent = status.message ?? "";
  nodes.push(msg);
  if (status.pending !== null) {
    const p = document.createElement("span");
    p.className = "pending";
    const count = status.pending.count;
    p.textContent =
      count === null
        ? status.pending.chords
        : `${String(count)} ${status.pending.chords}`;
    nodes.push(p);
  }
  // The RIGHT half (ADR 0132, bridge 85): the elements the host already
  // chose, worded and trimmed, in its order. Unread notices are one of them
  // (`notices`). A click returns the ID; the host runs the command.
  if (items.length > 0) {
    const right = document.createElement("span");
    right.className = "status-items";
    for (const it of items) {
      const el = document.createElement(it.clickable ? "button" : "span");
      el.className = "status-item";
      el.dataset["id"] = it.id;
      el.textContent = it.text;
      el.title = it.tooltip;
      if (it.progress !== undefined) {
        // The thin bar (ADR 0146): behind the text, slim. With no percentage
        // it animates instead of painting empty: "unknown" is not 0%.
        const bar = document.createElement("span");
        bar.className = "status-bar";
        bar.dataset["phase"] = it.progress.phase;
        bar.setAttribute("role", "progressbar");
        bar.setAttribute("aria-valuemin", "0");
        bar.setAttribute("aria-valuemax", "100");
        const fill = document.createElement("span");
        fill.className = "status-bar-fill";
        if (it.progress.percent === null) {
          bar.dataset["indeterminate"] = "true";
        } else {
          bar.setAttribute("aria-valuenow", String(it.progress.percent));
          fill.style.width = `${String(it.progress.percent)}%`;
        }
        bar.append(fill);
        el.append(bar);
      }
      if (el instanceof HTMLButtonElement) {
        el.type = "button";
        if (onItem !== null) {
          el.addEventListener("click", () => {
            onItem(it.id);
          });
        }
      }
      right.append(el);
    }
    nodes.push(right);
  }
  return nodes;
}

export function taskNode(t: TaskView, tr: (k: string) => string): HTMLElement {
  const el = document.createElement("div");
  el.className = "task";
  el.setAttribute("role", "listitem");
  const kind = document.createElement("span");
  // With its `gui-` prefix, which is how they are named in the catalogue:
  // without it ALL of them fell to `?? key` and every task on the dashboard
  // read `task-kind-copy`.
  kind.textContent = tr(`gui-task-kind-${t.kind}`);
  const state = document.createElement("span");
  state.setAttribute("role", "progressbar");
  state.setAttribute("aria-valuemin", "0");
  state.setAttribute("aria-valuemax", "100");
  if (t.percent !== null) {
    state.setAttribute("aria-valuenow", String(t.percent));
  }
  state.textContent = t.percent === null ? t.state : `${t.state} ${String(t.percent)}%`;
  const detail = document.createElement("span");
  detail.textContent = t.detail ?? "";
  detail.dataset["hostile"] = String(t.detail_hostile);
  el.append(kind, state, detail);
  if (t.detail_hostile) {
    // The file in progress paints different from what it is: it is said,
    // same as in a listing row. Without a badge, a masked name reads as the
    // real one.
    detail.classList.add("hostile");
    el.append(badge(tr("hostile-name")));
  }
  if (t.foreign) {
    el.append(badge(tr("gui-task-foreign")));
  }
  return el;
}
