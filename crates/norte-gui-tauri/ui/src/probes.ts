/**
 * What the screen SHOWS, for the end-to-end harness (`scripts/gui-e2e`).
 * Sent only when `NORTE_GUI_TRACE` is on, which only a debug build honours.
 *
 * `innerText` and not the view model: it follows the layout, so it is what a
 * person sees (spaces collapsed by a flex row were invisible to the bridge,
 * 2026-10-15). jsdom has no `innerText`; `textContent` stands in there.
 */
export function traceProbes(doc: Document): Record<string, string> {
  const text = (el: HTMLElement | null): string => el?.innerText ?? el?.textContent ?? "";
  // One line: the harness compares with `contains`, and a menu's rows
  // are separated by newlines it does not care about.
  const flat = (el: HTMLElement | null): string => text(el).replace(/\s+/g, " ").trim();
  const q = (sel: string): HTMLElement | null => doc.querySelector<HTMLElement>(sel);
  const slots = [...doc.querySelectorAll<HTMLElement>(".slot")]
    .map((s) => flat(s.querySelector<HTMLElement>(".slot-title")))
    .filter((t) => t !== "")
    .join(" | ");
  return {
    // Exact, not flattened: its spaces are what `expect-shown` checks.
    goto: text(q(".palette-query")),
    active: flat(q('.slot[data-role="active"] .slot-title')),
    cursor: flat(q('.slot[data-role="active"] .row[aria-selected="true"]')),
    slots,
    // The open menu's items: `.menu` is the bar AND the drop-down.
    menu: flat(q(".menu-items")),
    context: flat(q(".context-menu")),
    dialog: flat(q(".dialog")),
    // The overlays that are up (help, settings, theme, menu…), by id.
    open: [...doc.querySelectorAll<HTMLElement>('[id][data-open="true"]')]
      .map((el) => el.id)
      .join(" "),
    focus: describeFocus(doc),
    tone: tone(doc),
    // The scale the webview paints at (GDK_SCALE=2 → "2").
    dpr: String(doc.defaultView?.devicePixelRatio ?? ""),
  };
}

/** The focused element as `tag.class[label]`: enough to tell a dialog's
 *  button from the box's field without dumping the DOM. */
function describeFocus(doc: Document): string {
  const el = doc.activeElement;
  if (el === null || el === doc.body) {
    return "body";
  }
  const cls =
    typeof el.className === "string" && el.className !== ""
      ? `.${el.className.split(/\s+/).join(".")}`
      : "";
  const label = el.getAttribute("aria-label");
  return `${el.tagName.toLowerCase()}${cls}${label === null ? "" : `[${label}]`}`;
}

/** `dark` or `light`: the body's painted background, by its luminance. */
function tone(doc: Document): string {
  const view = doc.defaultView;
  if (view === null) {
    return "";
  }
  const bg = view.getComputedStyle(doc.body).backgroundColor;
  const m = /rgba?\((\d+),\s*(\d+),\s*(\d+)/.exec(bg);
  if (m === null) {
    return "";
  }
  const [r, g, b] = [m[1], m[2], m[3]].map(Number) as [number, number, number];
  return 0.2126 * r + 0.7152 * g + 0.0722 * b < 128 ? "dark" : "light";
}
