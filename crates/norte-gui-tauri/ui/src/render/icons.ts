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

import assistant from "../../../../norte-frontend/assets/panel-icons/assistant.svg?raw";
import diskMap from "../../../../norte-frontend/assets/panel-icons/disk-map.svg?raw";
import help from "../../../../norte-frontend/assets/panel-icons/help.svg?raw";
import settings from "../../../../norte-frontend/assets/panel-icons/settings.svg?raw";
import terminal from "../../../../norte-frontend/assets/panel-icons/terminal.svg?raw";
import log from "../../../../norte-frontend/assets/panel-icons/log.svg?raw";
import metadata from "../../../../norte-frontend/assets/panel-icons/metadata.svg?raw";
import places from "../../../../norte-frontend/assets/panel-icons/places.svg?raw";
import processes from "../../../../norte-frontend/assets/panel-icons/processes.svg?raw";
import search from "../../../../norte-frontend/assets/panel-icons/search.svg?raw";
import timeline from "../../../../norte-frontend/assets/panel-icons/timeline.svg?raw";
import tree from "../../../../norte-frontend/assets/panel-icons/tree.svg?raw";
import viewer from "../../../../norte-frontend/assets/panel-icons/viewer.svg?raw";
import layoutSplitH from "../../../../norte-frontend/assets/ui-icons/layout-split-h.svg?raw";
import layoutSplitV from "../../../../norte-frontend/assets/ui-icons/layout-split-v.svg?raw";
import layoutEqualize from "../../../../norte-frontend/assets/ui-icons/layout-equalize.svg?raw";
import layoutFlip from "../../../../norte-frontend/assets/ui-icons/layout-flip.svg?raw";
import layoutPick from "../../../../norte-frontend/assets/ui-icons/layout-pick.svg?raw";
import uiRemote from "../../../../norte-frontend/assets/ui-icons/ui-remote.svg?raw";
import uiBell from "../../../../norte-frontend/assets/ui-icons/ui-bell.svg?raw";
import uiSearch from "../../../../norte-frontend/assets/ui-icons/ui-search.svg?raw";
import fsFolder from "../../../../norte-frontend/assets/ui-icons/fs-folder.svg?raw";
import fsFile from "../../../../norte-frontend/assets/ui-icons/fs-file.svg?raw";
import fsFolderOpen from "../../../../norte-frontend/assets/ui-icons/fs-folder-open.svg?raw";
import fsDrive from "../../../../norte-frontend/assets/ui-icons/fs-drive.svg?raw";
import fsRemovable from "../../../../norte-frontend/assets/ui-icons/fs-removable.svg?raw";
import fsNetwork from "../../../../norte-frontend/assets/ui-icons/fs-network.svg?raw";
import fsHome from "../../../../norte-frontend/assets/ui-icons/fs-home.svg?raw";

// The listing's own colored icons per file type (ADR 0184). The HOST picks
// the id (`RowView.icon_kind`); here it is only drawn.
import ftArchive from "../../../../norte-frontend/assets/file-icons/archive.svg?raw";
import ftAudio from "../../../../norte-frontend/assets/file-icons/audio.svg?raw";
import ftCode from "../../../../norte-frontend/assets/file-icons/code.svg?raw";
import ftConfig from "../../../../norte-frontend/assets/file-icons/config.svg?raw";
import ftDoc from "../../../../norte-frontend/assets/file-icons/doc.svg?raw";
import ftFile from "../../../../norte-frontend/assets/file-icons/file.svg?raw";
import ftFolder from "../../../../norte-frontend/assets/file-icons/folder.svg?raw";
import ftImage from "../../../../norte-frontend/assets/file-icons/image.svg?raw";
import ftPdf from "../../../../norte-frontend/assets/file-icons/pdf.svg?raw";
import ftScript from "../../../../norte-frontend/assets/file-icons/script.svg?raw";
import ftSheet from "../../../../norte-frontend/assets/file-icons/sheet.svg?raw";
import ftSlides from "../../../../norte-frontend/assets/file-icons/slides.svg?raw";
import ftText from "../../../../norte-frontend/assets/file-icons/text.svg?raw";
import ftVideo from "../../../../norte-frontend/assets/file-icons/video.svg?raw";

const FILE_SVG: Record<string, string> = {
  archive: ftArchive,
  audio: ftAudio,
  code: ftCode,
  config: ftConfig,
  doc: ftDoc,
  file: ftFile,
  folder: ftFolder,
  image: ftImage,
  pdf: ftPdf,
  script: ftScript,
  sheet: ftSheet,
  slides: ftSlides,
  text: ftText,
  video: ftVideo,
};

const SVG = "http://www.w3.org/2000/svg";

/** An icon's pieces: `d` paths and `[cx, cy, r]` circles. */
interface Shape {
  paths: string[];
  circles?: [number, number, number][];
}

// The panels' icons are SHARED with the terminal, which rasterises the same
// files (`norte_frontend::panelbar::icon_svg`): one drawing, two frontends.
const PANEL_SVG: Record<string, string> = {
  places,
  tree,
  viewer,
  processes,
  metadata,
  log,
  "disk-map": diskMap,
  timeline,
  terminal,
  search,
  assistant,
  // The column's foot (bridge 101), not panels.
  settings,
  help,
  // The chrome's own, shared with the terminal too (`CHROME_ICONS`).
  "layout:split-h": layoutSplitH,
  "layout:split-v": layoutSplitV,
  "layout:equalize": layoutEqualize,
  "layout:flip": layoutFlip,
  "layout:pick": layoutPick,
  "ui:remote": uiRemote,
  "ui:bell": uiBell,
  "ui:search": uiSearch,
  "fs:folder": fsFolder,
  "fs:file": fsFile,
  "fs:folder-open": fsFolderOpen,
  "fs:drive": fsDrive,
  "fs:removable": fsRemovable,
  "fs:network": fsNetwork,
  "fs:home": fsHome,
  // A favorite is drawn with Places' own star.
  "fs:favorite": places,
};

// The window's own drawings, which the terminal has no use for: the title
// bar's buttons and the git history mark. Everything shared with the
// terminal is a file in `assets/` (`PANEL_SVG`).
const SHAPES: Record<string, Shape> = {
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
  const shared = PANEL_SVG[kind];
  const shape = SHAPES[kind];
  if (shared === undefined && shape === undefined) {
    return null;
  }
  const svg = doc.createElementNS(SVG, "svg");
  svg.setAttribute("class", "panelbar-icon");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("focusable", "false");
  const drawn = shared === undefined ? shape : shapeOf(shared);
  if (drawn === undefined) {
    return null;
  }
  for (const d of drawn.paths) {
    const p = doc.createElementNS(SVG, "path");
    p.setAttribute("d", d);
    svg.append(p);
  }
  for (const [cx, cy, r] of drawn.circles ?? []) {
    const c = doc.createElementNS(SVG, "circle");
    c.setAttribute("cx", String(cx));
    c.setAttribute("cy", String(cy));
    c.setAttribute("r", String(r));
    svg.append(c);
  }
  return svg;
}

/**
 * The shapes of a shared icon file, read with two patterns and not a
 * parser: the webview parses no markup (decision D11), and the files are
 * ours — `path d` and `circle cx cy r`, nothing else. Their stroke
 * attributes are for the terminal's rasteriser; here `.panelbar-icon`
 * styles the strokes.
 */
function shapeOf(src: string): Shape {
  const paths = Array.from(src.matchAll(/<path d="([^"]*)"/g), (m) => m[1] ?? "");
  const circles = Array.from(
    src.matchAll(/<circle cx="([\d.]+)" cy="([\d.]+)" r="([\d.]+)"/g),
    (m): [number, number, number] => [Number(m[1]), Number(m[2]), Number(m[3])],
  );
  return { paths, circles };
}

/** The presentation attributes a colored icon's piece may carry. */
const PAINT = [
  "d",
  "fill",
  "stroke",
  "stroke-width",
  "stroke-linecap",
  "stroke-linejoin",
];

/**
 * A listing row's own colored icon for a file-type id, or `null` for an id
 * without a drawing (the caller falls back to the plain page).
 *
 * The same two-pattern reading as `shapeOf`, and for the same reason (D11):
 * the files are ours, and each `<path>`/`<circle>` keeps only the
 * attributes in `PAINT`, so nothing in them is markup the webview runs.
 */
export function fileIcon(doc: Document, id: string): SVGSVGElement | null {
  const src = FILE_SVG[id];
  if (src === undefined) {
    return null;
  }
  const svg = doc.createElementNS(SVG, "svg");
  svg.setAttribute("class", "file-icon");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("focusable", "false");
  for (const m of src.matchAll(/<(path|circle) ([^>]*?)\/>/g)) {
    const tag = m[1] ?? "path";
    const el = doc.createElementNS(SVG, tag);
    for (const a of (m[2] ?? "").matchAll(/([a-z-]+)="([^"]*)"/g)) {
      const [name, value] = [a[1] ?? "", a[2] ?? ""];
      if (
        PAINT.includes(name) ||
        (tag === "circle" && ["cx", "cy", "r"].includes(name))
      ) {
        el.setAttribute(name, value);
      }
    }
    svg.append(el);
  }
  return svg;
}

/** A badge's figure: past a hundred, `99+`. */
export function badgeCount(n: number): string {
  return n > 99 ? "99+" : String(n);
}
