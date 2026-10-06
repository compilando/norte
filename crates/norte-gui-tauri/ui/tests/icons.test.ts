/// <reference types="vite/client" />
import { describe, expect, it } from "vitest";
import { icon } from "../src/render/icons";

// The panel icons come from the files the terminal rasterises too
// (`norte-frontend/assets/panel-icons`): this pins that the window draws
// THOSE strokes and not a copy of its own.
describe("panel icons", () => {
  it("draws the shared star for places", () => {
    const svg = icon(document, "places");
    expect(svg?.getAttribute("class")).toBe("panelbar-icon");
    expect(svg?.getAttribute("viewBox")).toBe("0 0 24 24");
    const paths = svg?.querySelectorAll("path") ?? [];
    expect(paths.length).toBe(1);
    expect(paths[0]?.getAttribute("d")).toMatch(/^M12 3\.5l2\.6/);
  });

  it("keeps the circles of the shared files", () => {
    expect(icon(document, "metadata")?.querySelectorAll("circle").length).toBe(1);
  });

  // The files are read with two patterns, not a parser (D11): a shape
  // written another way — attributes in another order, a `<rect>` — would
  // be dropped without a word. Every shape in every file is drawn.
  const files = import.meta.glob<string>(
    "../../../norte-frontend/assets/panel-icons/*.svg",
    {
      query: "?raw",
      import: "default",
      eager: true,
    },
  );

  it("draws every shape of every shared file", () => {
    const entries = Object.entries(files);
    expect(entries.length).toBe(8);
    for (const [path, src] of entries) {
      const kind = path.replace(/^.*\//, "").replace(/\.svg$/, "");
      const svg = icon(document, kind);
      const inFile = (
        src.match(/<(path|circle|rect|line|polyline|polygon|ellipse)\b/g) ?? []
      ).length;
      const drawn = svg?.querySelectorAll("path, circle").length ?? 0;
      expect(drawn, kind).toBe(inFile);
    }
  });

  it("still draws the window-only icons and the letter fallback", () => {
    expect(icon(document, "layout:pick")).not.toBeNull();
    expect(icon(document, "plugin:x:y")).toBeNull();
  });
});
