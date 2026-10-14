/// <reference types="vite/client" />
import { describe, expect, it } from "vitest";
import { fileIcon, icon } from "../src/render/icons";

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
    // Eleven panels (the assistant since 2026-10-14) and the column's foot.
    expect(entries.length).toBe(13);
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

// The listing's colored file-type icons (bridge 114, ADR 0184): every
// shape of every file drawn, with its colors, and nothing but paint.
describe("file-type icons", () => {
  const files = import.meta.glob<string>(
    "../../../norte-frontend/assets/file-icons/*.svg",
    {
      query: "?raw",
      import: "default",
      eager: true,
    },
  );

  it("draws every shape of every file, colored", () => {
    const entries = Object.entries(files);
    expect(entries.length).toBe(14);
    for (const [path, src] of entries) {
      const id = path.replace(/^.*\//, "").replace(/\.svg$/, "");
      const svg = fileIcon(document, id);
      expect(svg, id).not.toBeNull();
      const shapes = (src.match(/<(path|circle) /g) ?? []).length;
      expect(svg?.querySelectorAll("path, circle").length, id).toBe(shapes);
      expect(svg?.querySelector("[fill^='#']"), id).not.toBeNull();
    }
  });

  it("keeps only paint attributes and refuses an unknown id", () => {
    const svg = fileIcon(document, "pdf");
    const first = svg?.querySelector("path");
    expect(first?.getAttribute("fill")).toBe("#8a909a");
    for (const el of svg?.querySelectorAll("*") ?? []) {
      for (const a of Array.from(el.attributes)) {
        expect([
          "d",
          "fill",
          "stroke",
          "stroke-width",
          "stroke-linecap",
          "stroke-linejoin",
          "cx",
          "cy",
          "r",
        ]).toContain(a.name);
      }
    }
    expect(fileIcon(document, "no-such-type")).toBeNull();
  });
});
