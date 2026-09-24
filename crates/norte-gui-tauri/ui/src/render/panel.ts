// `Screen` painter for a PLUGIN's panel (phase 3): a function with
// `this: Screen`, hooked in as a property in `render.ts`. State stays in the
// class.

import type { Screen } from "../render";
import type { PanelSlotView, SpanView } from "../types";
import type { SlotDom } from "./dom";

/** A styled span, as the viewer paints it: the ROLE beats the color.
 *
 *  The role arrives already validated from Rust against what a plugin CAN
 *  ask for, and the text already comes masked: nothing is validated here, it
 *  is painted. Through `textContent` and never `innerHTML` — the text is a
 *  third party's. */
export function tramo(s: SpanView): HTMLElement {
  const el = document.createElement("span");
  el.textContent = s.text;
  if (s.role !== null && s.role !== undefined) {
    el.dataset["role"] = s.role;
  } else if (s.fg !== null && s.fg !== undefined) {
    el.style.color = s.fg;
  }
  if (s.bg !== null && s.bg !== undefined) {
    el.style.backgroundColor = s.bg;
  }
  return el;
}

/**
 * The panel a plugin paints: its frame, inside one of the window's borders.
 *
 * The guest does not draw, it DESCRIBES: styled lines and clickable zones.
 * The border, the title and the focus are set by this house, which is what
 * keeps a plugin from passing itself off as another panel.
 *
 * A ZONE does not run anything on its own: it sends the CELL that was
 * clicked (`panel_click`) and the host resolves which zone it was and which
 * command applies, with the same filter the terminal applies. That is why
 * `HitView` carries no command — if it did, the command would be chosen by
 * whoever talks to the renderer.
 *
 * With no frame yet — the first request in flight, or the plugin failed —
 * the border is painted with its title and nothing inside: it is known that
 * the panel is there and whose it is. What it never does is flicker, because
 * the host keeps the last frame while it asks for the next one.
 */
export function paintPanel(this: Screen, dom: SlotDom, slot: PanelSlotView): void {
  dom.root.setAttribute("aria-label", slot.title);
  dom.scroller.className = "panel-plugin";
  // The node is REUSED as long as the slot's geometry does not change, so a
  // slot that used to be a log or a viewer and becomes this panel arrives
  // with the previous one's wheel handler still set: scrolling here kept
  // sending `log_scroll` for this `slot_id`. The viewer clears it for the
  // same reason.
  dom.scroller.onwheel = null;
  dom.title.replaceChildren(document.createTextNode(slot.title));

  // The zones, grouped by row in ONE pass: the frame can carry 256 lines and
  // 128 zones, and filtering the whole list for every line meant walking the
  // list 256 times to paint the same thing.
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
    // THIS row's zones are painted on top, as buttons with no chrome: the
    // frame is text, and a zone is a region of that text that responds.
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
