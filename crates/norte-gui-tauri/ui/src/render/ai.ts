// `Screen` painters for ai (wave W10): functions with `this: Screen`, hooked
// in as properties in `render.ts`. State stays in the class.

import type { Screen } from "../render";
import type { AiRenameView } from "../types";
import { badge } from "./dom";

/**
 * The rename plan under review.
 *
 * The two names in each pair go in separate ELEMENTS, never concatenated
 * with an arrow: a name can contain the arrow, and the row would read as a
 * different pair than it is. The separator is set by the CSS, which a name
 * cannot write.
 */
export function paintAiRename(this: Screen, plan: AiRenameView | null): void {
  if (plan === null) {
    this.aiRenameRoot.replaceChildren();
    this.aiRenameRoot.dataset["open"] = "false";
    return;
  }
  this.aiRenameRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  box.className = "ai-rename";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  const h = document.createElement("h2");
  h.id = "ai-rename-title";
  h.textContent = this.t("modal-ai-rename-plan");
  box.setAttribute("aria-labelledby", h.id);
  box.append(h);

  const where = document.createElement("p");
  where.className = "ai-rename-dir";
  where.textContent = plan.dir.text;
  where.dataset["hostile"] = String(plan.dir.hostile);
  if (plan.dir.hostile) {
    where.classList.add("hostile");
    where.append(badge(this.t("hostile-name")));
  }
  box.append(where);

  // The VERDICT goes at the top, next to the directory: of the whole body it
  // is the one line that cannot be lost if the screen runs short.
  const status = document.createElement("p");
  status.className = "ai-rename-status";
  status.dataset["confirmable"] = String(plan.confirmable);
  status.setAttribute("role", "status");
  status.textContent = plan.status;
  box.append(status);

  const list = document.createElement("ol");
  list.className = "ai-rename-pairs";
  list.setAttribute("start", String(plan.first_visible + 1));
  for (const pair of plan.pairs) {
    const row = document.createElement("li");
    row.className = "ai-rename-pair";
    // The two names on separate LINES, and the second with its own color.
    // Putting them on the same line separated by an arrow split them with a
    // glyph a name can contain: `cap 2 → final` read as a different pair
    // than it was. The numbering is painted by the `<ol>`, which a name
    // cannot forge either.
    for (const [cls, line] of [
      ["ai-rename-from", pair.from],
      ["ai-rename-to", pair.to],
    ] as const) {
      const el = document.createElement("div");
      el.className = cls;
      el.textContent = line.text;
      el.dataset["hostile"] = String(line.hostile);
      if (line.hostile) {
        el.classList.add("hostile");
        el.append(badge(this.t("hostile-name")));
      }
      row.append(el);
    }
    list.append(row);
  }
  box.append(list);

  if (plan.more_note !== "") {
    // Already translated and already substituted BY THE HOST. Substituting
    // it here did not work: the catalogue carries the strings already
    // formatted and without arguments, and Fluent writes a missing variable
    // as `{$shown}` — with no spaces — so the `.replace` never matched and
    // the line saying how much of the plan is being shown painted two
    // identifiers.
    const more = document.createElement("p");
    more.className = "ai-rename-more";
    more.textContent = plan.more_note;
    box.append(more);
  }
  if (plan.hidden_hostile) {
    // What gets masked is said ALSO when it does not fit in the window: a
    // line's mark only exists for that line, and the altered pair can be in
    // position twelve.
    const notice = document.createElement("p");
    notice.className = "ai-rename-hidden-hostile hostile";
    notice.setAttribute("role", "alert");
    notice.textContent = this.t("modal-ai-rename-hidden-hostile");
    box.append(notice);
  }

  for (const line of plan.detail) {
    const p = document.createElement("p");
    p.className = "ai-rename-detail";
    p.textContent = line.text;
    p.dataset["hostile"] = String(line.hostile);
    if (line.hostile) {
      p.classList.add("hostile");
      p.append(badge(this.t("hostile-name")));
    }
    box.append(p);
  }

  if (plan.real_steps_note !== "") {
    // How many it REALLY renames: the planner drops the null pairs, and
    // showing only the requested ones promises too much.
    const real = document.createElement("p");
    real.className = "ai-rename-real";
    real.textContent = plan.real_steps_note;
    box.append(real);
  }

  // Buttons, and not just keys. A click is a gesture AIMED at this screen,
  // so it does not need the acknowledgment a key does; and without them a
  // mouse-only reader could not even dismiss a screen that opened on its
  // own.
  const buttons = document.createElement("div");
  buttons.className = "choices";
  // The two keys, LITERAL: a `t(variable)` is a key the catalogue's sweep
  // cannot follow, and a key that is not followed gets painted as its own
  // identifier the day it is missing.
  const apply = document.createElement("button");
  apply.type = "button";
  apply.textContent = this.t("modal-ai-rename-apply");
  apply.disabled = !plan.confirmable;
  apply.addEventListener("click", () => {
    this.send({ action: "ai_rename_decide", approve: true });
  });
  const discard = document.createElement("button");
  discard.type = "button";
  discard.textContent = this.t("modal-ai-rename-discard");
  discard.addEventListener("click", () => {
    this.send({ action: "ai_rename_decide", approve: false });
  });
  buttons.append(apply, discard);
  box.append(buttons);

  const footer = document.createElement("p");
  footer.className = "ai-rename-hint";
  footer.textContent = this.t("gui-modal-ai-rename-plan-hint");
  box.append(footer);
  this.aiRenameRoot.replaceChildren(box);
}
