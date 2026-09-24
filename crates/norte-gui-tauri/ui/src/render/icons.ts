// The activity bar's icons (spec 2026-09-21): one per stock panel, drawn here
// with simple strokes over a 24 grid.
//
// They are OUR OWN and not from a foreign icon set: six strokes are not
// worth a dependency nor one more license to audit. They paint with
// `currentColor`, so the button's state — closed, open, holding the keyboard
// — and the theme color them without this table knowing anything about them.
//
// A kind that is not here — a plugin's, or a new one that got forgotten —
// does not go without a button: the renderer paints its LETTER, which is
// what is already known about it.

const SVG = "http://www.w3.org/2000/svg";

/** An icon's pieces: `d` paths and `[cx, cy, r]` circles. */
interface Shape {
  paths: string[];
  circles?: [number, number, number][];
}

const SHAPES: Record<string, Shape> = {
  // Places: a star, what is marked as a favorite.
  places: {
    paths: ["M12 3.5l2.6 5.3 5.9.9-4.3 4.1 1 5.8L12 16.9l-5.2 2.7 1-5.8-4.3-4.1 5.9-.9z"],
  },
  // Tree: a folder on top and two branches hanging from it.
  tree: {
    paths: ["M4 4h6v5H4z", "M14 11h6v4h-6z", "M14 17h6v4h-6z", "M7 9v10h7", "M7 13h7"],
  },
  // Viewer: an eye.
  viewer: {
    paths: ["M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"],
    circles: [[12, 12, 3]],
  },
  // Processes: the pulse of something working.
  processes: { paths: ["M3 12h4l2.5-6 5 12 2.5-6h4"] },
  // Details: the "i" of information.
  metadata: { paths: ["M12 11v6", "M12 7.5v.5"], circles: [[12, 12, 9]] },
  // Log: lines of text piling up.
  log: { paths: ["M5 6h14", "M5 10h14", "M5 14h10", "M5 18h7"] },
  // Disk map: a cheese with a slice out.
  "disk-map": { paths: ["M11 4a8 8 0 1 0 9 9h-9z", "M14 3.5a7 7 0 0 1 6.5 6.5H14z"] },
  // Timeline: a clock.
  timeline: { paths: ["M12 7v5l3.5 2"], circles: [[12, 12, 9]] },
  // The layout buttons (ADR 0133), with the `layout:` prefix so they do not
  // cross with a panel kind.
  // Split side by side: a frame with a vertical line.
  "layout:split-h": { paths: ["M3.5 5h17v14h-17z", "M12 5v14"] },
  // Split top and bottom: the line horizontal.
  "layout:split-v": { paths: ["M3.5 5h17v14h-17z", "M3.5 12h17"] },
  // Equalize: the line in the middle and two equal halves on each side.
  "layout:equalize": {
    paths: ["M3.5 5h17v14h-17z", "M12 5v14", "M6.5 12h3", "M14.5 12h3"],
  },
  // Flip (ADR 0138): the split frame, with an arrow curving around.
  "layout:flip": {
    paths: [
      "M3.5 5h17v14h-17z",
      "M12 5v14",
      "M7 15.5a5 5 0 0 1 10 0",
      "M15 13.5l2 2 2-2",
    ],
  },
  // Pick a layout: a grid of four.
  "layout:pick": {
    paths: ["M4 4h7v7H4z", "M13 4h7v7h-7z", "M4 13h7v7H4z", "M13 13h7v7h-7z"],
  },
  // The buttons for a window with its own title bar (ADR 0136), `window:`
  // prefix. Thin and small, like the desktop's.
  // Minimize: a line at the bottom.
  "window:minimize": { paths: ["M7 12.5h10"] },
  // Maximize: a square.
  "window:toggle_maximize": { paths: ["M7.5 7.5h9v9h-9z"] },
  // Close: the cross.
  "window:close": { paths: ["M7.5 7.5l9 9", "M16.5 7.5l-9 9"] },
  // Git history: a branch that splits off and comes back.
  gitlog: {
    paths: ["M6 8v8", "M18 8.5c0 5-6 4-11 7"],
    circles: [
      [6, 5.5, 2.5],
      [6, 18.5, 2.5],
      [18, 6, 2.5],
    ],
  },
  // What is in the tree and in the places (`fs:` prefix).
  // Closed folder: the tab at the top left.
  "fs:folder": { paths: ["M3.5 6.5h6l2 2h9v10h-17z"] },
  // Open folder: the lid tilted forward.
  "fs:folder-open": { paths: ["M3.5 18.5v-12h6l2 2h8v2", "M3.5 18.5l3-8h15l-3 8z"] },
  // A drive: the box with its light.
  "fs:drive": { paths: ["M3.5 7.5h17v9h-17z", "M3.5 13h17"], circles: [[17, 15, 0.6]] },
  // A removable drive: the connector.
  "fs:removable": { paths: ["M8 3.5h8v5H8z", "M6 8.5h12v12H6z"] },
  // Something over the network: the globe.
  "fs:network": {
    paths: ["M3 12h18", "M12 3c3 3 3 15 0 18", "M12 3c-3 3-3 15 0 18"],
    circles: [[12, 12, 9]],
  },
  // The house.
  "fs:home": { paths: ["M4 11l8-7 8 7", "M6 9.5v10h12v-10"] },
  // A favorite: the star from "places".
  "fs:favorite": {
    paths: ["M12 3.5l2.6 5.3 5.9.9-4.3 4.1 1 5.8L12 16.9l-5.2 2.7 1-5.8-4.3-4.1 5.9-.9z"],
  },
};

/**
 * The icon for `id` — a panel kind, a `layout:*` button or an `fs:*`
 * filesystem thing — or `null` if it does not have one of its own.
 *
 * Decorative for accessibility (`aria-hidden`): the name goes next to it or
 * in the label, and a reader that also read the drawing would say the same
 * thing twice.
 */
export function icon(doc: Document, kind: string): SVGSVGElement | null {
  const shape = SHAPES[kind];
  if (shape === undefined) {
    return null;
  }
  const svg = doc.createElementNS(SVG, "svg");
  svg.setAttribute("class", "panelbar-icon");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("focusable", "false");
  for (const d of shape.paths) {
    const p = doc.createElementNS(SVG, "path");
    p.setAttribute("d", d);
    svg.append(p);
  }
  for (const [cx, cy, r] of shape.circles ?? []) {
    const c = doc.createElementNS(SVG, "circle");
    c.setAttribute("cx", String(cx));
    c.setAttribute("cy", String(cy));
    c.setAttribute("r", String(r));
    svg.append(c);
  }
  return svg;
}

/** A badge's figure: past a hundred, `99+`. */
export function badgeCount(n: number): string {
  return n > 99 ? "99+" : String(n);
}
