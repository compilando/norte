// Moving a pane by dragging its title or its tab (ADR 0138), like VS Code:
// passing over another pane shows where it would land — one of its halves, or
// the whole pane to join as a tab — and releasing sends `move_slot`. On the
// body's very edge it shows a strip along that whole side, and releasing
// sends `dock_slot`: full height or width. What happens to the tree is
// decided by the host.

import type { Screen } from "../render";
import type { DropZone } from "../types";

/** Pixels that have to move before a click turns into a drag: the title
 *  carries breadcrumbs that get clicked, and the tab is chosen with a click. */
const THRESHOLD = 6;

/** How much of the pane, from each edge, counts as "that side". The rest is
 *  the center. */
const EDGE = 0.25;

/** The chrome slots: they neither drag nor receive. */
const CHROME = new Set(["status", "tasks"]);

/** Pixels from the body's edge that mean "the whole side" (VS Code's
 *  outer drop): dropped there, the pane takes the full height or width. */
const OUTER = 24;

/** How thick the outer veil is: the share of the body a docked side takes. */
const OUTER_SHARE = 0.25;

type Target = { slot: number; zone: DropZone; rect: DOMRect; outer: boolean };

/** The body: the box around every pane that is not chrome. */
function bodyRect(screen: Screen): DOMRect | null {
  let [l, t, r, b] = [Infinity, Infinity, -Infinity, -Infinity];
  for (const [, dom] of screen.slots) {
    if (CHROME.has(dom.root.dataset["kind"] ?? "")) {
      continue;
    }
    const x = dom.root.getBoundingClientRect();
    if (x.width === 0 || x.height === 0) {
      continue;
    }
    [l, t, r, b] = [
      Math.min(l, x.left),
      Math.min(t, x.top),
      Math.max(r, x.right),
      Math.max(b, x.bottom),
    ];
  }
  return l < r && t < b ? new DOMRect(l, t, r - l, b - t) : null;
}

/** The body's side under the point, if it is within `OUTER` of it. */
function outerIn(body: DOMRect, x: number, y: number): DropZone | null {
  if (x < body.left || x >= body.right || y < body.top || y >= body.bottom) {
    return null;
  }
  const sides: [DropZone, number][] = [
    ["left", x - body.left],
    ["right", body.right - x],
    // The top band is thinner: the top panes' titles sit on it, and a drag
    // sliding along a title is not asking for the whole top.
    ["top", (y - body.top) * 3],
    ["bottom", body.bottom - y],
  ];
  const near = sides.filter(([, d]) => d < OUTER).sort((a, b) => a[1] - b[1])[0];
  return near === undefined ? null : near[0];
}

/**
 * The zone of `rect` under the point: the nearest side if it is within a
 * quarter of it, and the center otherwise.
 */
export function zoneOf(
  x: number,
  y: number,
  rect: { left: number; top: number; width: number; height: number },
): DropZone {
  const fx = rect.width > 0 ? (x - rect.left) / rect.width : 0.5;
  const fy = rect.height > 0 ? (y - rect.top) / rect.height : 0.5;
  const sides: [DropZone, number][] = [
    ["left", fx],
    ["right", 1 - fx],
    ["top", fy],
    ["bottom", 1 - fy],
  ];
  let best: [DropZone, number] = ["center", Number.POSITIVE_INFINITY];
  for (const side of sides) {
    if (side[1] < best[1]) {
      best = side;
    }
  }
  return best[1] < EDGE ? best[0] : "center";
}

/** The pane under the point and the zone, or `null` over itself, the chrome
 *  or nothing. */
function targetIn(screen: Screen, x: number, y: number, origin: number): Target | null {
  // The body's very edge first: the whole side, whoever is under it.
  const body = bodyRect(screen);
  const side = body === null ? null : outerIn(body, x, y);
  if (body !== null && side !== null) {
    return { slot: origin, zone: side, rect: body, outer: true };
  }
  for (const [id, dom] of screen.slots) {
    if (CHROME.has(dom.root.dataset["kind"] ?? "")) {
      continue;
    }
    const r = dom.root.getBoundingClientRect();
    if (x < r.left || x >= r.right || y < r.top || y >= r.bottom) {
      continue;
    }
    return id === origin
      ? null
      : { slot: id, zone: zoneOf(x, y, r), rect: r, outer: false };
  }
  return null;
}

/** The zone's rectangle, relative to the board: half of the pane, or — on
 *  the body's edge — a strip along that whole side. */
function paintVeil(veil: HTMLElement, board: DOMRect, d: Target | null): void {
  if (d === null) {
    veil.hidden = true;
    return;
  }
  veil.hidden = false;
  veil.dataset["zone"] = d.zone;
  veil.dataset["outer"] = String(d.outer);
  const r = d.rect;
  let [left, top, width, height] = [
    r.left - board.left,
    r.top - board.top,
    r.width,
    r.height,
  ];
  const share = d.outer ? OUTER_SHARE : 0.5;
  if (d.zone === "left" || d.zone === "right") {
    width = r.width * share;
    if (d.zone === "right") {
      left += r.width - width;
    }
  } else if (d.zone === "top" || d.zone === "bottom") {
    height = r.height * share;
    if (d.zone === "bottom") {
      top += r.height - height;
    }
  }
  veil.style.setProperty("left", `${String(left)}px`);
  veil.style.setProperty("top", `${String(top)}px`);
  veil.style.setProperty("width", `${String(width)}px`);
  veil.style.setProperty("height", `${String(height)}px`);
}

/**
 * Makes `handle` — a pane's title, or a tab — the spot slot `slotId` is
 * dragged from.
 *
 * Through `window` and not pointer capture: the drag leaves the handle on
 * the first pixel, and what matters is where it is released. `Esc` cancels
 * it without the key reaching the host, and the click the browser fires on
 * release over the handle is swallowed: a drag is not choosing the tab.
 */
export function makeDraggable(screen: Screen, handle: HTMLElement, slotId: number): void {
  handle.addEventListener("pointerdown", (e: PointerEvent) => {
    if (e.button !== 0) {
      return;
    }
    const doc = handle.ownerDocument;
    const win = doc.defaultView;
    if (win === null) {
      return;
    }
    const [x0, y0] = [e.clientX, e.clientY];
    let dragging = false;
    let target: Target | null = null;
    const veil = doc.createElement("div");
    veil.className = "drop-target";
    veil.hidden = true;

    const cleanup = (): void => {
      win.removeEventListener("pointermove", move);
      win.removeEventListener("pointerup", release);
      win.removeEventListener("pointercancel", cancel);
      win.removeEventListener("keydown", key, true);
      veil.remove();
      delete doc.documentElement.dataset["dragging"];
    };
    const swallowClick = (): void => {
      const swallow = (ev: Event): void => {
        ev.stopPropagation();
        ev.preventDefault();
      };
      win.addEventListener("click", swallow, { capture: true, once: true });
      // The release's click arrives RIGHT behind `pointerup`, before any
      // timer. If it does not arrive — it was released over another pane,
      // and the browser only clicks if pressed and released in the same
      // spot — the swallow is removed: otherwise it would eat the next real
      // click.
      win.setTimeout(() => {
        win.removeEventListener("click", swallow, { capture: true });
      }, 0);
    };
    const move = (ev: PointerEvent): void => {
      if (!dragging) {
        if (Math.hypot(ev.clientX - x0, ev.clientY - y0) < THRESHOLD) {
          return;
        }
        dragging = true;
        doc.documentElement.dataset["dragging"] = "slot";
        screen.root.append(veil);
      }
      target = targetIn(screen, ev.clientX, ev.clientY, slotId);
      paintVeil(veil, screen.root.getBoundingClientRect(), target);
    };
    const release = (): void => {
      cleanup();
      if (!dragging) {
        return;
      }
      swallowClick();
      if (target?.outer === true) {
        screen.send({ action: "dock_slot", slot_id: slotId, zone: target.zone });
      } else if (target !== null) {
        screen.send({
          action: "move_slot",
          slot_id: slotId,
          target: target.slot,
          zone: target.zone,
        });
      }
    };
    const key = (ev: KeyboardEvent): void => {
      if (ev.key !== "Escape") {
        return;
      }
      ev.stopImmediatePropagation();
      ev.preventDefault();
      target = null;
      dragging = false;
      cleanup();
    };
    // A pointer the system cancels (the window loses focus) does not release
    // anything: without this the listeners stayed attached and the next
    // `pointerup`, anywhere, sent a stale move.
    const cancel = (): void => {
      target = null;
      dragging = false;
      cleanup();
    };
    win.addEventListener("pointermove", move);
    win.addEventListener("pointerup", release);
    win.addEventListener("pointercancel", cancel);
    win.addEventListener("keydown", key, true);
  });
}
