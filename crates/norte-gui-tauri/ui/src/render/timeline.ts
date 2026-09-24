// `Screen` painter for the journal's timeline (bridge 78, #359): a function
// with `this: Screen`, hooked in as a property in `render.ts`.
//
// Rows arrive ALREADY paintable: the host grouped the batches, formatted the
// time and translated the tail. The only thing decided here is how a point
// looks, which is the only thing that really changes between a terminal and
// a window.

import type { Screen } from "../render";
import type { TimelineSlotView } from "../types";
import type { SlotDom } from "./dom";
import { badge, revelar } from "./dom";

/**
 * Paints a slot's timeline: one row per mutation — or per batch — from
 * newest to oldest, with the cursor over the point it would revert to, and
 * at the bottom what an `Enter` there would do.
 */
export function paintTimeline(this: Screen, dom: SlotDom, slot: TimelineSlotView): void {
  dom.root.setAttribute("aria-label", slot.title);
  dom.scroller.className = "timeline";
  // The node is REUSED as long as the geometry does not change: a slot that
  // used to be something else arrives with the previous one's wheel handler
  // still set.
  dom.scroller.onwheel = null;
  dom.title.replaceChildren(document.createTextNode(slot.title));

  if (slot.rows.length === 0) {
    const empty = document.createElement("div");
    empty.className = "empty";
    empty.textContent = slot.empty;
    dom.scroller.replaceChildren(empty);
    return;
  }

  const list = document.createElement("ul");
  list.className = "timeline-rows";
  list.setAttribute("role", "listbox");
  for (const [i, r] of slot.rows.entries()) {
    const row = document.createElement("li");
    row.className = "timeline-row";
    row.id = `timeline-${String(slot.slot_id)}-${String(i)}`;
    row.setAttribute("role", "option");
    row.setAttribute("aria-selected", String(slot.cursor === i));
    const time = document.createElement("span");
    time.className = "timeline-time";
    time.textContent = r.time;
    // The dot carries the actor's COLOR: "me" versus "something in my name".
    // Yours gets undone from here; an agent's, through another door.
    const dot = document.createElement("span");
    dot.className = "timeline-dot";
    dot.dataset["actor"] = r.actor === "user" || r.actor === "agent" ? r.actor : "other";
    dot.setAttribute("aria-hidden", "true");
    dot.textContent = "●";
    row.append(time, dot);
    if (r.hostile) {
      // IN FRONT, as on every surface where something gets decided: the
      // server already masked the name, and this is what keeps it from being
      // read as trustworthy.
      row.append(badge(this.t("hostile-name")));
    }
    const verb = document.createElement("span");
    verb.className = "timeline-op";
    verb.textContent = r.op;
    const path = document.createElement("span");
    path.className = "timeline-path";
    path.textContent = r.path;
    // The column truncates from the end: the whole path, on hover.
    row.title = `${r.op} ${r.path}`;
    row.append(verb, path);
    if (r.tail !== "") {
      const tail = document.createElement("span");
      tail.className = "timeline-tail";
      tail.textContent = r.tail;
      row.append(tail);
    }
    list.append(row);
  }
  if (slot.cursor !== null) {
    list.setAttribute(
      "aria-activedescendant",
      `timeline-${String(slot.slot_id)}-${String(slot.cursor)}`,
    );
  }
  const footer = document.createElement("div");
  footer.className = "timeline-footer";
  footer.textContent = slot.footer;
  dom.scroller.replaceChildren(list, footer);
  // The cursor's row, in view: the next page is requested on reaching the
  // last one loaded, and a cursor that moves down unseen does not know where
  // it is.
  revelar(list.querySelector('[aria-selected="true"]') ?? undefined);
}
