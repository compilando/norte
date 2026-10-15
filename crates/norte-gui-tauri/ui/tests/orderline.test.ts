import { describe, expect, it } from "vitest";
import { orderSpans } from "../src/render/menus";

// The host sends UTF-8 byte ranges (ADR 0185); a JS string indexes UTF-16
// units. This pins the conversion on a line that is not ASCII.
describe("order line", () => {
  it("cuts the spans by bytes, not by JS indices", () => {
    // ":mkdir año" — `ñ` is two bytes; the value runs from byte 7 to 11.
    const nodes = orderSpans(":mkdir año", [
      { start: 1, end: 6, class: "verb" },
      { start: 7, end: 11, class: "value" },
    ]);
    const spans = nodes.filter((n): n is HTMLElement => n instanceof HTMLElement);
    expect(spans.map((s) => [s.dataset["class"], s.textContent])).toEqual([
      ["verb", "mkdir"],
      ["value", "año"],
    ]);
    expect(nodes.map((n) => n.textContent).join("")).toBe(":mkdir año");
  });

  it("drops a span that overlaps or runs past the line", () => {
    const nodes = orderSpans(":ab", [
      { start: 1, end: 9, class: "verb" },
      { start: 0, end: 1, class: "pipe" },
    ]);
    expect(nodes.map((n) => n.textContent).join("")).toBe(":ab");
  });
});
