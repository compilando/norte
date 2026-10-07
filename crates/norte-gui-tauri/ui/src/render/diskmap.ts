// `Screen` painter for the disk map (bridge 71, phase 4): a function with
// `this: Screen`, hooked in as a property in `render.ts`.
//
// The treemap already comes LAID OUT by the host: nothing is computed here.
// This is deliberate — a layout done twice is two different layouts as soon
// as someone touches a rounding, and then the rectangle that is shown and the
// one a click resolves stop being the same one.

import type { Screen } from "../render";
import type { DiskMapSlotView } from "../types";
import type { SlotDom } from "./dom";
import { note } from "./dom";
// The SAME span that paints a plugin panel, not a copy: it is the conversion
// of a styled span to DOM, and two copies drift apart as soon as one learns
// something the other does not — a new role, another one masked.
import { tramo } from "./panel";

/**
 * Paints a slot's map: its lines and a button per rectangle.
 *
 * The click sends the CELL (`panel_click`), not the child: who each
 * rectangle is gets resolved by the host against the frame it laid out
 * itself. If the name traveled, a choice would have to be made between the
 * shape that is painted — masked, which identifies no file — and the
 * reversible one, and on top of that it would be a name anyone talking to
 * this renderer could send.
 */
export function paintDiskMap(this: Screen, dom: SlotDom, slot: DiskMapSlotView): void {
  const title = slot.measuring
    ? `${slot.title} — ${this.t("disk-map-measuring")}`
    : slot.title;
  dom.root.setAttribute("aria-label", title);
  dom.scroller.className = "disk-map";
  // The node is REUSED as long as the geometry does not change, so a slot
  // that used to be a log or a viewer arrives with the previous one's wheel
  // handler still set. Same care as the plugin panel.
  dom.scroller.onwheel = null;
  dom.title.replaceChildren(document.createTextNode(title));

  // A finished map with nothing to draw says so, as the timeline does: an
  // empty frame read as "still loading" or "broken".
  if (slot.empty !== "") {
    const empty = document.createElement("div");
    empty.className = "empty";
    empty.textContent = slot.empty;
    dom.scroller.replaceChildren(empty);
    return;
  }
  // Nothing drawn: the TITLE goes in the body too — the directory and
  // "measuring", or a failure's reason. In a dock's tab group the strip
  // hides the title, and measuring a big tree left a blank panel.
  if (slot.lines.length === 0) {
    // No title and not measuring: nothing to claim, so nothing is said.
    const text =
      title !== "" ? title : slot.measuring ? this.t("disk-map-measuring") : "";
    dom.scroller.replaceChildren(...(text === "" ? [] : [note(text)]));
    return;
  }

  // The zones, grouped by row in ONE pass: a map can carry 256 lines and 128
  // zones, and filtering the whole list for every line would mean walking it
  // 256 times to paint the same thing.
  const byRow = new Map<number, typeof slot.hits>();
  for (const h of slot.hits) {
    const row = byRow.get(h.row) ?? [];
    row.push(h);
    byRow.set(h.row, row);
  }

  const body = document.createElement("div");
  body.className = "panel-lines";
  for (const [row, line] of slot.lines.entries()) {
    const li = document.createElement("div");
    li.className = "panel-line";
    li.replaceChildren(...line.map(tramo));
    for (const hit of byRow.get(row) ?? []) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "panel-hit";
      button.style.setProperty("--hit-col", String(hit.col));
      button.style.setProperty("--hit-width", String(hit.width));
      button.addEventListener("click", () => {
        this.send({
          action: "panel_click",
          slot_id: slot.slot_id,
          row: hit.row,
          col: hit.col,
        });
      });
      li.append(button);
    }
    body.append(li);
  }
  dom.scroller.replaceChildren(body);
}
