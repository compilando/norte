// `Screen` painters for places (wave W10): functions with `this: Screen`,
// hooked in as properties in `render.ts`. State stays in the class.

import type { Screen } from "../render";
import type { PlacesSlotView, TreeSlotView } from "../types";
import { revealInView, badge, note, unchanged } from "./dom";
import { icon } from "./icons";
import type { SlotDom } from "./dom";

/**
 * The directory tree.
 *
 * Two different gestures on the same row: the TRIANGLE folds and unfolds,
 * and the name NAVIGATES. A single gesture would force a choice between
 * which of the two things a click means, and both are needed — looking
 * inside a branch without moving the listing is half of what a tree is for.
 *
 * The tree does not move when navigating: that is what makes keeping it open
 * useful.
 */
export function paintTree(this: Screen, dom: SlotDom, slot: TreeSlotView): void {
  // #403: the tree used to be rebuilt —an `li` and two listeners per row—
  // on every paint, so every arrow in the listing next door rebuilt a tree
  // with thousands of open branches. Same rows, cursor and generation, and
  // still this tree in the scroller: nothing to do.
  // The signature is recorded on EVERY paint, the first included: behind
  // the `&&` it was skipped while the scroller was still empty, and only a
  // second identical paint armed it.
  const same = unchanged(dom.scroller, JSON.stringify(slot));
  if (same && dom.scroller.firstElementChild?.classList.contains("tree-rows") === true) {
    return;
  }
  dom.root.setAttribute("aria-label", this.t("tree-title"));
  dom.title.textContent = this.t("tree-title");
  dom.scroller.className = "tree";
  const list = document.createElement("ul");
  list.className = "tree-rows";
  list.setAttribute("role", "tree");
  // Only a window of the branches travels (#403, bridge 97): the padding
  // stands in for the rest, so the scrollbar measures the whole tree.
  const first = slot.first ?? 0;
  const after = Math.max(0, (slot.total ?? slot.rows.length) - first - slot.rows.length);
  list.style.paddingTop = `calc(var(--cell-h) * ${String(first)})`;
  list.style.paddingBottom = `calc(var(--cell-h) * ${String(after)})`;
  for (const [n, r] of slot.rows.entries()) {
    // Numbered among ALL branches: that is what the host's actions name.
    const i = first + n;
    const row = document.createElement("li");
    row.className = "tree-row";
    row.id = `tree-row-${String(i)}`;
    row.setAttribute("role", "treeitem");
    row.setAttribute("aria-level", String(r.depth + 1));
    row.setAttribute("aria-selected", String(slot.cursor === i));
    // The indent, as a variable: CSS cannot multiply a depth that only
    // exists in the data.
    row.style.setProperty("--depth", String(r.depth));
    const mark = document.createElement("span");
    mark.className = "tree-twisty";
    if (r.children === false) {
      // A leaf carries no triangle, but it DOES carry its gap: without it
      // names at the same level do not line up and the tree stops reading
      // as one.
      mark.textContent = " ";
    } else {
      // `null` — not looked at yet — is painted as folded and not as a
      // leaf: painting "there is nothing inside" for something nobody has
      // read is a made-up answer.
      // A chevron that ROTATES on unfold, as in VS Code: the same mark in
      // two positions reads as a switch.
      mark.textContent = "›";
      mark.dataset["expanded"] = String(r.expanded);
      row.setAttribute("aria-expanded", String(r.expanded));
    }
    row.dataset["row"] = String(i);
    // The folder, open or closed depending on the branch: it is what makes
    // the column read as a tree at a glance, as in VS Code.
    const folder = icon(document, r.expanded ? "fs:folder-open" : "fs:folder");
    const name = document.createElement("span");
    name.className = "tree-name";
    name.dataset["hostile"] = String(r.hostile);
    name.textContent = r.label;
    if (r.hostile) {
      name.append(badge(this.t("hostile-name")));
    }
    if (folder !== null) {
      folder.classList.add("tree-icon");
    }
    row.append(mark, folder ?? document.createElement("span"), name);
    list.append(row);
  }
  // ONE listener for every row (#403): two closures per row were thousands
  // of listeners on a tree with big branches open, rebuilt on each change.
  list.addEventListener("click", (ev) => {
    const target = ev.target instanceof Element ? ev.target : null;
    const row = target?.closest<HTMLElement>("li.tree-row");
    const index = Number(row?.dataset["row"]);
    if (row === null || row === undefined || !Number.isInteger(index)) {
      return;
    }
    // THIS paint's generation: a branch's children arrive on their own and
    // get inserted IN THE MIDDLE, so without it a click could navigate to a
    // folder nobody clicked. The twisty of a branch folds and does not
    // navigate.
    const twisty = target?.closest<HTMLElement>(".tree-twisty");
    const folds = twisty !== null && twisty !== undefined && "expanded" in twisty.dataset;
    this.send({
      action: folds ? "tree_toggle_row" : "tree_activate_row",
      row: index,
      generation: slot.generation,
    });
  });
  list.setAttribute("aria-activedescendant", `tree-row-${String(slot.cursor)}`);
  const scrollTop = dom.scroller.scrollTop;
  dom.scroller.replaceChildren(list);
  // Revealed only when the CURSOR moved: a repaint brought by the reader's
  // own scroll must not pull the view back to it.
  if (dom.scroller.dataset["treeCursor"] !== String(slot.cursor)) {
    dom.scroller.dataset["treeCursor"] = String(slot.cursor);
    revealInView(list.querySelector(`#tree-row-${String(slot.cursor)}`) ?? undefined);
  } else {
    dom.scroller.scrollTop = scrollTop;
  }
  if (dom.scroller.dataset["treeScroll"] !== "on") {
    dom.scroller.dataset["treeScroll"] = "on";
    let pending: number | undefined;
    const report = (): void => {
      pending = undefined;
      const h = this.cell().h;
      this.send({
        action: "tree_set_visible_range",
        first: Math.floor(dom.scroller.scrollTop / h),
        count: Math.max(1, Math.ceil(dom.scroller.clientHeight / h)),
      });
    };
    dom.scroller.addEventListener("scroll", () => {
      if (pending === undefined) {
        pending = window.setTimeout(report, 50);
      }
    });
  }
}

/**
 * The places sidebar: volumes and favorites.
 *
 * A click CHOOSES AND ACTIVATES, unlike the other lists: a sidebar exists to
 * go to places, and a click that only moves a cursor forces finishing with
 * the keyboard. A header folds instead of navigating, which is what the host
 * does with it.
 */
export function paintPlaces(this: Screen, dom: SlotDom, slot: PlacesSlotView): void {
  dom.root.setAttribute("aria-label", this.t("places-title"));
  dom.title.textContent = this.t("places-title");
  dom.scroller.className = "places";
  const list = document.createElement("ul");
  list.className = "places-rows";
  list.setAttribute("role", "listbox");
  for (const [i, r] of slot.rows.entries()) {
    const row = document.createElement("li");
    row.className = "places-row";
    row.id = `place-row-${String(i)}`;
    row.dataset["row"] = r.row;
    row.setAttribute("role", "option");
    row.setAttribute("aria-selected", String(slot.cursor === i));
    row.addEventListener("click", () => {
      // THIS paint's generation. Volumes arrive on their own and get
      // inserted before the favorites, so without it a click could navigate
      // to a place nobody clicked.
      this.send({
        action: "place_activate_row",
        row: i,
        generation: slot.generation,
      });
    });
    if (r.row === "header") {
      row.setAttribute("aria-expanded", String(!r.folded));
      const mark = document.createElement("span");
      mark.className = "places-fold";
      mark.textContent = r.folded ? "▸" : "▾";
      const text = document.createElement("span");
      text.className = "places-header";
      text.textContent = r.label;
      row.append(mark, text);
    } else if (r.row === "drive") {
      // ONE line (2026-09-21 capture): icon by drive kind, the SHORT name
      // and the short free space on the right, as in the TUI. The whole
      // mount point and the space sentence go in the title.
      row.dataset["kind"] = r.kind ?? "unknown";
      if (r.free !== undefined) {
        row.dataset["line"] = "one";
      }
      row.title = [r.mount ?? "", r.detail].filter((s) => s !== "").join("\n");
      const drawing = icon(
        document,
        r.kind === "removable"
          ? "fs:removable"
          : r.kind === "network"
            ? "fs:network"
            : "fs:drive",
      );
      if (drawing !== null) {
        drawing.classList.add("places-icon");
        row.append(drawing);
      }
      const name = document.createElement("span");
      name.className = "places-name";
      name.dataset["hostile"] = String(r.hostile);
      name.textContent = r.label;
      if (r.hostile) {
        name.append(badge(this.t("hostile-name")));
      }
      const detail = document.createElement("span");
      detail.className = "places-detail";
      detail.textContent = r.free ?? r.detail;
      row.append(name, detail);
    } else {
      // A favorite, on ONE line: star and name; the target goes in the
      // title. A broken one still says why, below and in red.
      const star = icon(document, "fs:favorite");
      if (star !== null) {
        star.classList.add("places-icon");
        row.append(star);
      }
      const name = document.createElement("span");
      name.className = "places-name";
      name.textContent = r.name;
      row.append(name);
      if (r.broken === "") {
        row.dataset["line"] = "one";
        row.title = r.target;
        if (r.hostile) {
          // The target is not visible in the row, so the mark goes on the
          // name: a favorite that points to a masked name SAYS SO.
          name.dataset["hostile"] = "true";
          name.append(badge(this.t("hostile-name")));
        }
      } else {
        // A broken favorite is PAINTED with its reason: one that disappears
        // silently is a configuration failure nobody can see.
        const broken = document.createElement("span");
        broken.className = "places-broken";
        broken.textContent = r.broken;
        row.append(broken);
      }
    }
    list.append(row);
  }
  list.setAttribute("aria-activedescendant", `place-row-${String(slot.cursor)}`);
  // The empty Favorites section says how to get one (bridge 99). Under the
  // list, not as a row: rows are clicked by index, and Favorites is the
  // last section, so this sits right under its header.
  const hint = slot.favorites_hint ?? "";
  if (hint !== "") {
    const p = note(hint);
    p.classList.add("places-hint");
    dom.scroller.replaceChildren(list, p);
  } else {
    dom.scroller.replaceChildren(list);
  }
  revealInView(list.querySelector(`#place-row-${String(slot.cursor)}`) ?? undefined);
}
