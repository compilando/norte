// The assistant panel's painter (bridge 116, ADR 0186). The host sends the
// turns already sanitized; this paints them with `textContent` only — model
// text is never HTML — and keeps its own input across repaints, so a turn
// landing while the reader types does not steal the caret.

import type { Screen } from "../render";
import type { AssistantSlotView } from "../types";
import type { SlotDom } from "./dom";

const inputs = new WeakMap<SlotDom, HTMLInputElement>();
const wasActive = new WeakMap<SlotDom, boolean>();

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
        this.send({ action: "assistant_apply", slot_id: slot.slot_id, turn: i });
      });
      item.append(apply);
    }
    list.append(item);
  });
  if (slot.pending) {
    const thinking = document.createElement("li");
    thinking.className = "assistant-thinking";
    thinking.textContent = this.t("assistant-thinking");
    list.append(thinking);
  }

  let input = inputs.get(dom);
  if (input === undefined) {
    const field = document.createElement("input");
    field.type = "text";
    field.className = "assistant-input";
    field.addEventListener("keydown", (e) => {
      // Enter, the arrows and Tab belong to the field: let through, an
      // empty Enter would open the entry under the LISTING's cursor.
      if (!["Enter", "ArrowUp", "ArrowDown", "Tab"].includes(e.key)) {
        return;
      }
      e.preventDefault();
      e.stopPropagation();
      if (e.key !== "Enter" || field.value.trim() === "" || field.disabled) {
        return;
      }
      this.send({ action: "assistant_submit", slot_id: slot.slot_id, text: field.value });
      field.value = "";
    });
    inputs.set(dom, field);
    input = field;
  }
  input.disabled = slot.pending;
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
