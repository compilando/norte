// The viewer's body: what a plugin's styled preview paints as.

import { describe, expect, it } from "vitest";

import { viewerBody } from "../src/render/dom";
import type { SpanView, ViewerView } from "../src/types";

function viewer(styled: SpanView[][]): ViewerView {
  return { hex: false, lines: [], styled } as unknown as ViewerView;
}

const cell = (fg: string, bg: string): SpanView => ({ text: "▀", role: null, fg, bg });

describe("viewerBody", () => {
  // #377: an image previewer paints each cell as a `▀` with two colours,
  // and the rows have to TOUCH. With the text's line height they left a
  // stripe of background between every two, and the photo came out barred.
  it("paints half-block pictures with rows that touch", () => {
    const body = viewerBody(
      viewer([
        [cell("#102030", "#203040"), cell("#304050", "#405060")],
        [cell("#506070", "#607080"), cell("#708090", "#8090a0")],
      ]),
    );
    expect(body.classList.contains("viewer-cells")).toBe(true);
  });

  it("leaves text previews with the text's line height", () => {
    const body = viewerBody(
      viewer([[{ text: "# aurora", role: "title", fg: null, bg: null }]]),
    );
    expect(body.classList.contains("viewer-cells")).toBe(false);
  });
});
