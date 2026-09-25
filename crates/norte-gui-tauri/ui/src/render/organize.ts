// `Screen` painter for the ORGANIZE review (phase 8): a function with
// `this: Screen`, hooked in as a property in `render.ts`. State stays in the
// class.

import type { Screen } from "../render";
import type { OrganizeView } from "../types";
import { badge } from "./dom";

/** The CSS class for each tree line kind. */
const CLASS = {
  new_dir: "organize-new-dir",
  existing_dir: "organize-existing-dir",
  moved: "organize-moved",
} as const;

/**
 * The ORGANIZE plan under review.
 *
 * Painted as a TREE and not as a list of pairs because what changes is the
 * directory's SHAPE: how many folders show up, which ones, and what ends up
 * inside each one. That is what is being approved, and a list of forty
 * `a.pdf → invoices/2026/a.pdf` does not let it be seen.
 *
 * Each line's kind arrives as DATA (`kind`) and is painted with a CSS class
 * AND a text marker. Both things: the color says "new" at a glance, and the
 * marker survives a monochrome theme or a screen reader. The marker goes in
 * its own element, never concatenated to the name — a file named
 * `+ invoices` cannot disguise itself as a new folder.
 */
export function paintOrganize(this: Screen, plan: OrganizeView | null): void {
  if (plan === null) {
    this.organizeRoot.replaceChildren();
    this.organizeRoot.dataset["open"] = "false";
    return;
  }
  this.organizeRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  box.className = "organize";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  const h = document.createElement("h2");
  h.id = "organize-title";
  h.textContent = this.t("modal-organize-plan");
  box.setAttribute("aria-labelledby", h.id);
  box.append(h);

  const where = document.createElement("p");
  where.className = "organize-dir";
  where.textContent = plan.dir.text;
  where.dataset["hostile"] = String(plan.dir.hostile);
  if (plan.dir.hostile) {
    where.classList.add("hostile");
    where.append(badge(this.t("hostile-name")));
  }
  box.append(where);

  // The COUNT goes at the top, next to the directory: it is what gets read
  // to decide without counting lines, and of the whole body it is the one
  // thing that cannot be lost if the screen runs short.
  const summary = document.createElement("p");
  summary.className = "organize-summary";
  summary.setAttribute("role", "status");
  summary.textContent = plan.summary;
  box.append(summary);

  const tree = document.createElement("ul");
  tree.className = "organize-tree";
  for (const line of plan.lines) {
    const row = document.createElement("li");
    row.className = `organize-line ${CLASS[line.kind]}`;
    // The indent is a style DATUM, not spaces in the text: a name that
    // starts with spaces cannot fake being further in.
    row.style.setProperty("--depth", String(line.depth));
    const mark = document.createElement("span");
    mark.className = "organize-mark";
    mark.setAttribute("aria-hidden", "true");
    mark.textContent =
      line.kind === "new_dir" ? "+" : line.kind === "existing_dir" ? "·" : "→";
    const name = document.createElement("span");
    name.className = "organize-name";
    name.textContent = line.text.text;
    name.dataset["hostile"] = String(line.text.hostile);
    if (line.text.hostile) {
      name.classList.add("hostile");
      name.append(badge(this.t("hostile-name")));
    }
    row.append(mark, name);
    tree.append(row);
  }
  box.append(tree);

  if (plan.more_note !== "") {
    // Already translated and already substituted BY THE HOST, for the same
    // reason as in the rename review: the catalogue carries the strings
    // already formatted.
    const more = document.createElement("p");
    more.className = "organize-more";
    more.textContent = plan.more_note;
    box.append(more);
  }
  if (plan.hidden_hostile) {
    const notice = document.createElement("p");
    notice.className = "organize-hidden-hostile hostile";
    notice.setAttribute("role", "alert");
    notice.textContent = this.t("modal-ai-rename-hidden-hostile");
    box.append(notice);
  }

  // Scrolling with the mouse. Approving requires having reached the end, so
  // without this the screen was one a keyboard-less reader could never
  // approve.
  if (plan.total > plan.lines.length) {
    tree.addEventListener("wheel", (e) => {
      e.preventDefault();
      this.send({ action: "organize_scroll", down: e.deltaY > 0 });
    });
  }

  const buttons = document.createElement("div");
  buttons.className = "choices";
  // The two keys, LITERAL: a `t(variable)` is a key the catalogue's sweep
  // cannot follow.
  const apply = document.createElement("button");
  apply.type = "button";
  apply.textContent = this.t("modal-ai-rename-apply");
  // Disabled until it has been read in full, which is the only condition
  // here: the token arrived WITH the plan, so there is no verdict to wait
  // for.
  apply.disabled = !plan.seen_all;
  apply.addEventListener("click", () => {
    this.send({ action: "organize_decide", approve: true });
  });
  const discard = document.createElement("button");
  discard.type = "button";
  discard.textContent = this.t("modal-ai-rename-discard");
  discard.addEventListener("click", () => {
    this.send({ action: "organize_decide", approve: false });
  });
  buttons.append(apply, discard);
  box.append(buttons);

  const footer = document.createElement("p");
  footer.className = "organize-hint";
  footer.textContent = this.t("gui-modal-ai-rename-plan-hint");
  box.append(footer);
  this.organizeRoot.replaceChildren(box);
}
