// The assistant panel's field (ADR 0186, bridge 118): Esc cancels a turn
// in flight, an empty Enter applies the newest order, and a sent text stays
// until the host takes it.

import { describe, expect, it } from "vitest";

import type { Screen } from "../src/render";
import { lastOrder, paintAssistant } from "../src/render/assistant";
import type { SlotDom } from "../src/render/dom";
import type { AssistantSlotView, AssistantTurnView, UiAction } from "../src/types";

function rig(): { dom: SlotDom; screen: Screen; sent: UiAction[] } {
  const root = document.createElement("section");
  root.dataset["role"] = "active";
  const dom = {
    root,
    scroller: document.createElement("div"),
    title: document.createElement("div"),
  } as unknown as SlotDom;
  const sent: UiAction[] = [];
  const screen = {
    t: (k: string) => k,
    send: (a: UiAction) => {
      sent.push(a);
    },
  } as unknown as Screen;
  return { dom, screen, sent };
}

const view = (over: Partial<AssistantSlotView>): AssistantSlotView => ({
  kind: "assistant",
  slot_id: 7,
  turns: [],
  pending: false,
  empty: "",
  ...over,
});

const order: AssistantTurnView = {
  kind: "order",
  text: "sel ext:pdf | cp >other",
  note: "",
  written_for: "",
};
const reader: AssistantTurnView = {
  kind: "reader",
  text: "copy",
  note: "",
  written_for: "",
};

function key(field: HTMLInputElement, k: string): void {
  field.dispatchEvent(
    new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true }),
  );
}

// jsdom paints nothing, so it has no `scrollIntoView`.
Element.prototype.scrollIntoView = (): void => {};

describe("the assistant's field", () => {
  it("finds the newest order", () => {
    expect(lastOrder(view({}))).toBe(-1);
    expect(lastOrder(view({ turns: [order, reader, order, reader] }))).toBe(2);
  });

  it("Esc while thinking cancels; an empty Enter applies the newest order", () => {
    const { dom, screen, sent } = rig();
    paintAssistant.call(screen, dom, view({ turns: [reader], pending: true }));
    const field = dom.scroller.querySelector("input") as HTMLInputElement;
    expect(field.readOnly).toBe(true);
    key(field, "Escape");
    expect(sent).toEqual([{ action: "assistant_cancel", slot_id: 7 }]);

    paintAssistant.call(screen, dom, view({ turns: [reader, order] }));
    key(field, "Enter");
    expect(sent.at(-1)).toEqual({ action: "assistant_apply", slot_id: 7, turn: 1 });
  });

  it("keeps the text until the host takes it", () => {
    const { dom, screen, sent } = rig();
    paintAssistant.call(screen, dom, view({}));
    const field = dom.scroller.querySelector("input") as HTMLInputElement;
    field.value = "copy";
    key(field, "Enter");
    expect(sent).toEqual([{ action: "assistant_submit", slot_id: 7, text: "copy" }]);
    expect(field.value).toBe("copy");
    paintAssistant.call(screen, dom, view({}));
    // Refused: nothing new came, so the text stays.
    expect(field.value).toBe("copy");
    paintAssistant.call(screen, dom, view({ turns: [reader], pending: true }));
    expect(field.value).toBe("");
  });

  it("the thinking row carries a cancel button", () => {
    const { dom, screen, sent } = rig();
    paintAssistant.call(screen, dom, view({ pending: true }));
    (dom.scroller.querySelector(".assistant-cancel") as HTMLButtonElement).click();
    expect(sent).toEqual([{ action: "assistant_cancel", slot_id: 7 }]);
  });
});
