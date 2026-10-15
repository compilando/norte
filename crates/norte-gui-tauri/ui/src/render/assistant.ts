// The assistant panel's painter (bridge 116, ADR 0186). The host sends the
// turns already sanitized; this paints them with `textContent` only — model
// text is never HTML — and keeps its own input across repaints, so a turn
// landing while the reader types does not steal the caret.

import type { Screen } from "../render";
import type { AssistantSlotView } from "../types";
import type { SlotDom } from "./dom";

const inputs = new WeakMap<SlotDom, HTMLInputElement>();
const wasActive = new WeakMap<SlotDom, boolean>();
/** The latest view of each panel: the field's handlers outlive the paint
 *  that created them. */
const views = new WeakMap<SlotDom, AssistantSlotView>();
/** What the field sent: it is emptied only once the host took it — a
 *  refused send keeps the text. */
const sent = new WeakMap<SlotDom, string>();

/** The index of the newest order turn, or -1. */
export function lastOrder(slot: AssistantSlotView): number {
  for (let i = slot.turns.length - 1; i >= 0; i--) {
    if (slot.turns[i]?.kind === "order") {
      return i;
    }
  }
  return -1;
}

/** The input takes the caret when the panel becomes active, and lets go of
 *  it when it stops being — the search panel's rule. */
function followTheKeyboard(dom: SlotDom, input: HTMLInputElement): void {
  const active = dom.root.dataset["role"] === "active";
  if (active === wasActive.get(dom)) {
    return;
  }
  wasActive.set(dom, active);
  if (active && document.activeElement !== input) {
    input.focus();
  } else if (!active && document.activeElement === input) {
    input.blur();
  }
}

export function paintAssistant(
  this: Screen,
  dom: SlotDom,
  slot: AssistantSlotView,
): void {
  dom.root.setAttribute("aria-label", this.t("assistant-title"));
  dom.scroller.className = "assistant";
  dom.title.replaceChildren(document.createTextNode(this.t("assistant-title")));

  const list = document.createElement("ol");
  list.className = "assistant-turns";
  if (slot.turns.length === 0 && !slot.pending) {
    const empty = document.createElement("li");
    empty.className = "assistant-empty";
    empty.textContent = slot.empty;
    list.append(empty);
  }
  slot.turns.forEach((turn, i) => {
    const item = document.createElement("li");
    item.className = "assistant-turn";
    item.dataset["kind"] = turn.kind;
    const text = document.createElement("div");
    text.className = "assistant-text";
    text.textContent = turn.text;
    item.append(text);
    if (turn.note !== "") {
      const note = document.createElement("div");
      note.className = "assistant-note";
      note.textContent = turn.note;
      item.append(note);
    }
    if (turn.written_for !== "") {
      const w = document.createElement("div");
      w.className = "assistant-written-for";
      w.textContent = turn.written_for;
      item.append(w);
    }
    if (turn.kind === "order") {
      const apply = document.createElement("button");
      apply.type = "button";
      apply.className = "assistant-apply";
      apply.textContent = this.t("assistant-apply");
      apply.addEventListener("click", () => {
        // The turn's number, which the cap dropping older turns does not
        // shift.
        const turn = (slot.first ?? 0) + i;
        this.send({ action: "assistant_apply", slot_id: slot.slot_id, turn });
      });
      item.append(apply);
    }
    list.append(item);
  });
  if (slot.pending) {
    const thinking = document.createElement("li");
    thinking.className = "assistant-thinking";
    thinking.textContent = this.t("assistant-thinking");
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "assistant-cancel";
    cancel.textContent = this.t("assistant-cancel");
    cancel.addEventListener("click", () => {
      this.send({ action: "assistant_cancel", slot_id: slot.slot_id });
    });
    thinking.append(" ", cancel);
    list.append(thinking);
  }

  views.set(dom, slot);
  let input = inputs.get(dom);
  if (input === undefined) {
    const field = document.createElement("input");
    field.type = "text";
    field.className = "assistant-input";
    field.addEventListener("keydown", (e) => {
      const now = views.get(dom);
      if (now === undefined) {
        return;
      }
      // Esc while a turn is in flight cancels it; otherwise it goes on and
      // gives the keys back, as in the terminal.
      if (e.key === "Escape" && now.pending) {
        e.preventDefault();
        e.stopPropagation();
        this.send({ action: "assistant_cancel", slot_id: now.slot_id });
        return;
      }
      // Enter, the arrows and Tab belong to the field: let through, an
      // empty Enter would open the entry under the LISTING's cursor.
      if (!["Enter", "ArrowUp", "ArrowDown", "Tab"].includes(e.key)) {
        return;
      }
      e.preventDefault();
      e.stopPropagation();
      if (e.key !== "Enter" || now.pending) {
        return;
      }
      if (field.value.trim() === "") {
        // An empty Enter applies the newest order, as in the terminal; the
        // usual confirmation still asks.
        const index = lastOrder(now);
        if (index >= 0) {
          const turn = (now.first ?? 0) + index;
          this.send({ action: "assistant_apply", slot_id: now.slot_id, turn });
        }
        return;
      }
      sent.set(dom, field.value);
      this.send({ action: "assistant_submit", slot_id: now.slot_id, text: field.value });
    });
    inputs.set(dom, field);
    input = field;
  }
  // The host took what was sent once a paint shows a turn in flight (the
  // field cannot send while one is): until then the text stays, so a
  // refused send loses nothing. Not by counting turns — at the cap the
  // count stays put while the oldest drops.
  const pendingSend = sent.get(dom);
  if (pendingSend !== undefined && slot.pending) {
    if (input.value === pendingSend) {
      input.value = "";
    }
    sent.delete(dom);
  }
  // Read-only, not disabled, while a turn is in flight: a disabled field
  // drops the caret and hears no Esc.
  input.readOnly = slot.pending;
  input.placeholder = this.t("assistant-placeholder");
  // Only the list is replaced: taking the field out of the DOM, even to put
  // it back, drops its focus and the caret in the middle of a word.
  const old = dom.scroller.querySelector(":scope > .assistant-turns");
  if (input.parentElement === dom.scroller && old !== null) {
    old.replaceWith(list);
  } else {
    dom.scroller.replaceChildren(list, input);
  }
  list.lastElementChild?.scrollIntoView({ block: "end" });
  followTheKeyboard(dom, input);
}
