// The renderer's startup: hooks up the bridge, paints what arrives and sends
// what the user does. Nothing else lives here.

// The two BUNDLED fonts (spec 2026-09-11, V1). Only the Latin subsets and
// the weights actually used: each file is ~24KB of woff2, and Vite puts them
// in `dist/assets`, which Tauri embeds into the binary. Each `@font-face`
// carries `font-display: swap`, so the system stack paints while they load
// and nothing stays blank.
import "@fontsource/jetbrains-mono/latin-400.css";
import "@fontsource/jetbrains-mono/latin-ext-400.css";
import "@fontsource/jetbrains-mono/latin-700.css";
import "@fontsource/inter/latin-400.css";
import "@fontsource/inter/latin-ext-400.css";
import "@fontsource/inter/latin-600.css";

import { invokeMetrics, tauriPort } from "./bridge";
import type { HostPort } from "./bridge";
import { AltSolo, esParaElCampo, keyAction, keyInputOf } from "./keys";
import { Screen } from "./render";
import { montarBarraDeTitulo } from "./render/menus";
import { Session } from "./session";
import { BRIDGE_VERSION } from "./types";
import type { Appearance, HostCatalog, UiAction, WindowVerb } from "./types";

/** Spike measurements: key-to-paint latency. Read by 3.6's harness. */
export interface Metrics {
  keyToPaint: number[];
  scrollToPaint: number[];
  updates: number;
  resyncs: number;
}

export async function boot(port: HostPort, doc: Document): Promise<Metrics> {
  const screenEl = doc.getElementById("screen");
  const menuEl = doc.getElementById("menu");
  const panelBarEl = doc.getElementById("panelbar");
  const paletteEl = doc.getElementById("palette");
  const whichKeyEl = doc.getElementById("whichkey");
  const helpEl = doc.getElementById("help");
  const settingsEl = doc.getElementById("settings");
  const extensionsEl = doc.getElementById("extensions");
  const themeEl = doc.getElementById("theme");
  const pickerEl = doc.getElementById("picker");
  const profilesEl = doc.getElementById("profiles");
  const layoutsEl = doc.getElementById("layouts");
  const columnsEl = doc.getElementById("columns");
  const searchEl = doc.getElementById("search");
  const compareEl = doc.getElementById("compare");
  const syncEl = doc.getElementById("sync");
  const agentsEl = doc.getElementById("agents");
  const pluginOutputEl = doc.getElementById("plugin-output");
  const programOutputEl = doc.getElementById("program-output");
  const viewerEl = doc.getElementById("viewer");
  const dialogsEl = doc.getElementById("dialogs");
  const aiRenameEl = doc.getElementById("ai-rename");
  const organizeEl = doc.getElementById("organize");
  const splashEl = doc.getElementById("splash");
  const gotoEl = doc.getElementById("goto");
  const fatalEl = doc.getElementById("fatal");
  if (
    screenEl === null ||
    menuEl === null ||
    panelBarEl === null ||
    paletteEl === null ||
    whichKeyEl === null ||
    helpEl === null ||
    settingsEl === null ||
    extensionsEl === null ||
    themeEl === null ||
    pickerEl === null ||
    profilesEl === null ||
    layoutsEl === null ||
    columnsEl === null ||
    searchEl === null ||
    compareEl === null ||
    syncEl === null ||
    agentsEl === null ||
    pluginOutputEl === null ||
    programOutputEl === null ||
    viewerEl === null ||
    dialogsEl === null ||
    aiRenameEl === null ||
    organizeEl === null ||
    splashEl === null ||
    gotoEl === null ||
    fatalEl === null
  ) {
    throw new Error("the document is missing the renderer's anchors");
  }
  const metrics: Metrics = { keyToPaint: [], scrollToPaint: [], updates: 0, resyncs: 0 };
  const session = new Session();
  const catalog = await port.catalog();
  let currentCatalog = catalog;
  applyThemeFor(doc, catalog);
  applyAppearance(doc, catalog.appearance);
  // The window's own title bar (ADR 0136), for the screen and for the fatal
  // error. Nothing to wait for a response to: the window moves or it does
  // not, and a rejection (native bar) goes to the console and nothing else.
  const window_: ControlesDeVentana = {
    t: (k) => catalog.strings[k] ?? k,
    pedir: (verb) => {
      port.windowControl(verb).catch((e: unknown) => {
        console.warn("window_control", verb, e);
      });
    },
  };
  // (Following the desktop's scheme is hooked up further below, as soon as
  // `send` exists: since bridge 66 plugging in variables is not enough.)
  // The "I'm making you wait" threshold comes from the host, which pulls it
  // from the crate shared with the terminal. Here it is only plugged in as a
  // CSS variable: writing it into the stylesheet would be a third place
  // where the same number lives, and the first place to forget to change it.
  doc.documentElement.style.setProperty(
    "--busy-delay",
    `${String(catalog.busy_threshold_ms ?? 250)}ms`,
  );

  // What is being waited to paint, and since when. The measurement runs from
  // the gesture to the frame that shows it: anything shorter measures
  // something else.
  let pending: { at: number; what: "key" | "scroll" } | null = null;
  // The host promises actions get applied IN ORDER, and that promise holds
  // for its mailbox — not for the path that feeds it. Two loose `invoke`s
  // are two tasks racing to get in: a single click's `focus_slot` and
  // `select_row` could get reversed, and the click selected nothing, every
  // so often. A single chain of promises: at most one `dispatch` in flight,
  // and the queue drains in order.
  let queue: Promise<void> = Promise.resolve();
  let dead = false;
  const send = (action: UiAction): void => {
    if (dead) {
      // Incompatible contract or fatal failure: NOTHING more gets sent.
      // Painting a notice and continuing to dispatch keys is the worst of
      // both worlds.
      return;
    }
    queue = queue
      .then(() => port.dispatch(action))
      .then((ack) => {
        // Accepted (even if "unavailable"): the host understood it, and an
        // earlier rejection's notice no longer counts for anything.
        screen.accepted();
        if (ack.status === "unavailable") {
          // A dimmed command is not an error: the host already said why.
          console.info("not available:", screen.t(ack.reason_key));
        }
      })
      .catch((e: unknown) => {
        // Does not deserialize, or the contract is broken: the host never
        // saw it, so only the renderer can say so — on the bar, not on a
        // console nobody looks at.
        screen.rejected(action, e);
      });
  };
  const screen = new Screen(
    screenEl,
    menuEl,
    panelBarEl,
    paletteEl,
    whichKeyEl,
    helpEl,
    settingsEl,
    extensionsEl,
    themeEl,
    pickerEl,
    profilesEl,
    layoutsEl,
    columnsEl,
    searchEl,
    compareEl,
    syncEl,
    agentsEl,
    pluginOutputEl,
    programOutputEl,
    viewerEl,
    dialogsEl,
    aiRenameEl,
    organizeEl,
    splashEl,
    gotoEl,
    catalog,
    send,
    () => port.imageBytes(),
    window_.pedir,
  );

  const repaint = (): void => {
    const view = session.view();
    if (view === null) {
      return;
    }
    screen.paint(view);
    // The menu bar just reserved (or released) its row: the host lays out
    // over the height this renderer declares, and without saying so again
    // the listing's first row would end up under the bar.
    if (screen.takeViewportDirty()) {
      sendViewport(send, screen, doc);
    }
    if (pending !== null) {
      const { at, what } = pending;
      pending = null;
      requestAnimationFrame(() => {
        const dt = performance.now() - at;
        if (what === "key") {
          metrics.keyToPaint.push(dt);
        } else {
          metrics.scrollToPaint.push(dt);
        }
      });
    }
  };

  const resync = (): void => {
    metrics.resyncs += 1;
    void port.requestSnapshot();
  };

  await port.onUpdate((env) => {
    metrics.updates += 1;
    const out = session.receive(env);
    switch (out.kind) {
      case "applied":
        repaint();
        return;
      case "gap":
        // What was missing is NEVER guessed: the whole frame is requested.
        resync();
        return;
      case "incompatible":
        showFatal(
          fatalEl,
          // The RENDERER's, which is the one it was compared against.
          // `catalog` carries the host's, so it used to say `bridge 17 ≠ 17`.
          `bridge ${String(out.version)} ≠ ${String(BRIDGE_VERSION)}`,
          window_,
        );
        return;
      case "notice":
        if (out.notice.notice === "fatal") {
          dead = true;
          showFatal(fatalEl, screen.t(out.notice.key), window_);
        }
        repaint();
        return;
      case "ignored":
        return;
    }
  });
  await port.onLagged(resync);
  // The catalogue changed: today only the THEME triggers it, and what has to
  // be redone is its CSS variables. The strings are not re-applied because
  // they do not move — the language is fixed once per process — and `Screen`
  // kept its own.
  await port.onCatalog(() => {
    void port
      .catalog()
      .then((cat) => {
        currentCatalog = cat;
        applyThemeFor(doc, cat);
        applyAppearance(doc, cat.appearance);
      })
      .catch((e: unknown) => {
        console.error("could not re-read the catalogue:", e);
      });
  });

  // The initial snapshot arrives in the SAME envelope as everything else:
  // one shape on the wire is one shape to maintain.
  const first = await port.initialSnapshot();
  // The FIRST frame's outcome is not discarded: it is the only message a
  // renderer that is out of sync sees for sure, and discarding it left a
  // blank window instead of the incompatibility screen.
  const initial = session.receive(first);
  if (initial.kind === "incompatible") {
    dead = true;
    showFatal(
      fatalEl,
      `bridge ${String(initial.version)} ≠ ${String(BRIDGE_VERSION)}`,
      window_,
    );
    return metrics;
  }
  repaint();
  sendViewport(send, screen, doc);
  // The desktop switches from light to dark and the window follows (V6).
  //
  // Two things, not one. The CSS VARIABLES are plugged in on this side, on
  // the spot: the catalogue already carries both variants resolved, and
  // going through the host would cost a flicker with the wrong palette. But
  // since bridge 66 an entry's color travels BAKED into its row and is
  // resolved by the host, which has no desktop to ask: it has to be told, or
  // it paints the chrome with one variant and the NAMES with the other.
  //
  // Also sent NOW, not only on change: the host starts out assuming light
  // because it cannot know, and this first message is what corrects it
  // before anyone looks.
  const w = doc.defaultView;
  if (w !== null && typeof w.matchMedia === "function") {
    const query = w.matchMedia("(prefers-color-scheme: dark)");
    send({ action: "set_color_scheme", dark: query.matches });
    query.addEventListener("change", (e) => {
      applyTheme(doc, themeFor(currentCatalog, e.matches));
      send({ action: "set_color_scheme", dark: e.matches });
    });
  }
  // With no user `norte.toml`, the first-run wizard (spec 2026-09-10): the
  // host opens it, since it is the one that writes it; here it is only told
  // that this is the first run.
  if (catalog.first_run === true) {
    send({ action: "wizard_open" });
  }
  // And the splash screen (ADR 0115), through the same door: whether it is
  // wanted — `[ui] splash` — is known by the host, which is also the one
  // that yields the spot to the wizard. Here it is only told that this is
  // startup.
  //
  // Unless this run turned it off (`--no-splash`, `NORTE_NO_SPLASH`): that
  // is seen by the window's process, not the host, and without this door
  // every automated screenshot would come out with the splash on top.
  if (catalog.no_splash !== true) {
    send({ action: "splash_open" });
  }

  // Alt alone goes to the menu bar (bridge 68). Checked BEFORE the modifier
  // filter: for `keyInputOf` an Alt alone is not a key, and it is not one for
  // the keymap either; it is a separate gesture.
  const altSolo = new AltSolo();
  doc.addEventListener("keyup", (e) => {
    if (altSolo.arriba(e)) {
      // Without this, WebKitGTK can take focus to its own bar.
      e.preventDefault();
      send({ action: "menu_toggle" });
    }
  });
  doc.addEventListener("mousedown", () => altSolo.soltar(), true);
  doc.addEventListener("wheel", () => altSolo.soltar(), { capture: true, passive: true });
  (doc.defaultView ?? window).addEventListener("blur", () => altSolo.soltar());

  doc.addEventListener("keydown", (e) => {
    altSolo.abajo(e);
    const k = keyInputOf(e);
    if (k === null) {
      return;
    }
    // An open text field owns its keys: the ones that type and the ones
    // that EDIT. The rule lives in `keys.ts`, where it can be tested.
    if (esParaElCampo(k, e.target instanceof HTMLInputElement)) {
      return;
    }
    // The keys that scroll help also go to the host (bridge 76): it resolves
    // them with the reader's keymap and answers with a request the renderer
    // applies. See `desplazarAyuda`.
    e.preventDefault();
    pending ??= { at: performance.now(), what: "key" };
    send(keyAction(k));
  });

  let resizeHandle: number | null = null;
  (doc.defaultView ?? window).addEventListener("resize", () => {
    if (resizeHandle !== null) {
      return;
    }
    resizeHandle = requestAnimationFrame(() => {
      resizeHandle = null;
      sendViewport(send, screen, doc);
    });
  });

  if (catalog.measure) {
    // 3.6's pass: it measures itself, with synthetic events and through the
    // renderer's FULL path (event → invoke → host → event → DOM → next
    // frame). Left out are the system's event delivery and the compositor,
    // and that is stated in the report.
    await medir(doc, metrics, port, () => {
      pending ??= { at: performance.now(), what: "scroll" };
    });
  }
  return metrics;
}

/** Latencies from a scripted pass: keys first, scroll after. */
async function medir(
  doc: Document,
  metrics: Metrics,
  port: HostPort,
  markScroll: () => void,
): Promise<void> {
  const frame = (): Promise<number> =>
    new Promise((r) => {
      requestAnimationFrame(() => {
        r(performance.now());
      });
    });
  // STABLE state first: a listing of a hundred thousand entries keeps
  // arriving in the background for seconds, and measuring over that filling
  // measures the filling. It waits for the host to go quiet (two seconds
  // with no updates), capped at one minute.
  {
    let seen = metrics.updates;
    let still = 0;
    for (let i = 0; i < 600 && still < 20; i += 1) {
      await new Promise((r) => setTimeout(r, 100));
      still = metrics.updates === seen ? still + 1 : 0;
      seen = metrics.updates;
    }
  }

  // The machine's FLOOR: how long a frame takes doing nothing. Without this
  // reference, "33ms per frame" does not tell a slow renderer apart from a
  // screen refreshing at 30Hz.
  const idle: number[] = [];
  {
    let previous = await frame();
    for (let i = 0; i < 200; i += 1) {
      const now = await frame();
      idle.push(now - previous);
      previous = now;
    }
  }

  // LOCAL scroll: what a frame costs while dragging, without waiting on
  // Rust. It is what 3.6's budget calls "visible rows' work", and it is the
  // path that does NOT cross the bridge.
  const local: number[] = [];
  const localScroller = doc.querySelector(".scroller");
  if (localScroller instanceof HTMLElement) {
    let previous = await frame();
    for (let i = 0; i < 200; i += 1) {
      localScroller.scrollTop = i * 40;
      const now = await frame();
      local.push(now - previous);
      previous = now;
    }
  }

  // The samples are recorded by `repaint`: from the gesture to the frame
  // that shows it.
  for (let i = 0; i < 200; i += 1) {
    doc.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    await frame();
    await frame();
  }
  const scroller = doc.querySelector(".scroller");
  for (let i = 0; i < 200 && scroller instanceof HTMLElement; i += 1) {
    scroller.scrollTop = i * 40;
    markScroll();
    scroller.dispatchEvent(new Event("scroll"));
    await frame();
    await frame();
  }
  const keys = metrics.keyToPaint;
  const scroll = metrics.scrollToPaint;
  const send = async (what: string, samples: number[]): Promise<void> => {
    try {
      await invokeMetrics(what, samples);
    } catch {
      // Without the `metrics` feature the command does not exist: not a
      // failure, a production binary. The samples stay in `__norteMetrics`.
    }
  };
  await send("key-to-paint", keys);
  await send("scroll-to-rows", scroll);
  await send("scroll-frame-local", local);
  await send("idle-frame", idle);
  await port.dispatch({ action: "resync" });
}

function sendViewport(send: (a: UiAction) => void, screen: Screen, doc: Document): void {
  const cell = screen.cell();
  const view = doc.defaultView ?? window;
  // From the element that actually carries the layout, not the window: the
  // menu bar eats one of its rows, and measuring the window would declare to
  // the host a height it has nowhere to paint. Same with the WIDTH since the
  // activity bar eats a column on the left (bridge 84).
  const screenEl = doc.getElementById("screen");
  const height = screenEl?.clientHeight ?? view.innerHeight;
  const width = screenEl?.clientWidth ?? view.innerWidth;
  send({
    action: "set_viewport",
    width: Math.max(1, Math.floor(width / cell.w)),
    height: Math.max(1, Math.floor(height / cell.h)),
  });
}

/**
 * The variables the PREVIOUS theme left set on the root.
 *
 * Needed because the host only sends what the theme SAYS: since the chrome
 * roles (spec 2026-09-11, F2), a theme that does not define `hover` does not
 * send `--hover`, and the sheet derives it with
 * `var(--hover, var(--panel-focus-bg))` — a derivation that only kicks in
 * while the variable is UNSET. That mechanism did not used to exist and
 * every projected name was a role every preset defines, so every change
 * overwrote the whole set and just assigning was enough.
 */
let variablesPuestas: string[] = [];

/**
 * The colors come from the theme resolved IN RUST; here they are only
 * plugged in.
 *
 * The previous theme's are CLEARED before the new one's are set. Without
 * that, switching from `vscode-dark` to `gruvbox-light` left the light
 * palette on everything coming from a CORE role and the dark one on
 * everything coming from a chrome one: the command palette and the menus at
 * #202020, the row hover at #2a2d2e and the breadcrumbs at #9d9d9d, over a
 * light background with no way to fix it short of restarting.
 * `removeProperty` returns the variable to whatever `:root` says, which for
 * the chrome ones is "unset", exactly what the derivation needs.
 */
export function applyTheme(doc: Document, theme: Record<string, string>): void {
  const root = doc.documentElement;
  for (const name of variablesPuestas) {
    root.style.removeProperty(`--${name}`);
  }
  for (const [name, value] of Object.entries(theme)) {
    root.style.setProperty(`--${name}`, value);
  }
  variablesPuestas = Object.keys(theme);
}

/**
 * The theme that applies according to the desktop (spec 2026-09-11, V6): the
 * light or dark variant of `[ui] theme_light` / `theme_dark` if there is one
 * for that side, and `[ui] theme` if not. All three arrive already resolved
 * to variables: nothing here knows what a theme is, only which of the three
 * sets to plug in.
 */
export function themeFor(cat: HostCatalog, dark: boolean): Record<string, string> {
  const variant = dark ? cat.theme_dark : cat.theme_light;
  return variant ?? cat.theme;
}

/** Whether the desktop asks for dark. With no `matchMedia` (a test), no. */
function escritorioOscuro(doc: Document): boolean {
  const w = doc.defaultView;
  if (w === null || typeof w.matchMedia !== "function") {
    return false;
  }
  return w.matchMedia("(prefers-color-scheme: dark)").matches;
}

function applyThemeFor(doc: Document, cat: HostCatalog): void {
  applyTheme(doc, themeFor(cat, escritorioOscuro(doc)));
}

/** A row's height-to-size ratio. A 22px row for 14px text is what the
 *  stylesheet says based on the bundled typeface (V1); kept while scaling so
 *  a row keeps the same feel at any size. */
export const FILA_POR_TAMANO = 22 / 14;

/**
 * Fonts and motion (`[ui] font`, `mono_font`, `font_size`,
 * `reduce_motion`).
 *
 * The four were loaded, validated and offered on the settings screen with
 * nobody reading them. A `null` field is NOT written: then whatever is
 * already there rules, which for motion is whatever the desktop says.
 *
 * The size also moves `--cell-h`, and that is not an extra: this window is
 * laid out in CELLS, so a bigger glyph inside a row of the same height
 * overflows its row. `--cell-w` is MEASURED with the font set, because a
 * monospace font's width is not a proportion of the size: it depends on the
 * family, and a column computed with the wrong width misaligns the whole
 * listing.
 */
export function applyAppearance(doc: Document, ap: Appearance | undefined): void {
  if (ap === undefined) {
    return;
  }
  const root = doc.documentElement;
  if (ap.font !== null) {
    root.style.setProperty("--ui-font", ap.font);
  }
  if (ap.mono_font !== null) {
    root.style.setProperty("--mono", ap.mono_font);
  }
  if (ap.font_size !== null) {
    root.style.setProperty("--ui-font-size", `${String(ap.font_size)}px`);
    root.style.setProperty(
      "--cell-h",
      `${String(Math.round(ap.font_size * FILA_POR_TAMANO))}px`,
    );
  }
  // Only the REQUEST is ADDED: the configuration cannot contradict someone
  // who already asked for less motion on their desktop, so `false` does not
  // turn off the system's `prefers-reduced-motion` — it just stops forcing
  // it.
  if (ap.reduce_motion === true) {
    root.dataset["reduceMotion"] = "true";
  } else {
    delete root.dataset["reduceMotion"];
  }
  // The window's own title bar (ADR 0136): the menu one becomes draggable
  // and carries the window's three buttons. With the native one, neither.
  if (ap.custom_titlebar === true) {
    root.dataset["titlebar"] = "custom";
  } else {
    delete root.dataset["titlebar"];
  }
  medirCelda(doc);
}

/**
 * Measures a cell's width with whatever font is set NOW.
 *
 * A monospace font's character does not take up a fixed fraction of its
 * size: each family has its own advance. With the wrong width, every column
 * of the listing lands a little further from where the host laid it out,
 * and by the end of the row the error is several characters wide.
 *
 * SEVERAL characters are measured and divided: just one rounds to the pixel
 * and the error multiplies by the number of columns.
 */
function medirCelda(doc: Document): void {
  const ruler = doc.createElement("span");
  ruler.textContent = "M".repeat(50);
  ruler.style.cssText =
    // The SAME stack as the body (`--font-mono`), not plain `--mono`: if the
    // configuration names no font, the body paints with the bundled one and
    // measuring with `monospace` would give another family's advance.
    "position:absolute;visibility:hidden;white-space:pre;font-family:var(--font-mono, monospace);font-size:var(--ui-font-size, 14px)";
  doc.body.append(ruler);
  const width = ruler.getBoundingClientRect().width / 50;
  ruler.remove();
  if (width > 0) {
    doc.documentElement.style.setProperty("--cell-w", `${String(width)}px`);
  }
}

/** What the fatal screen needs to set up its own title bar. */
export interface ControlesDeVentana {
  t: (key: string) => string;
  pedir: (verb: WindowVerb) => void;
}

/**
 * The error screen that covers everything.
 *
 * With its OWN title bar (ADR 0136) it carries its own: it covers the menu's,
 * the window has no desktop one, and a dead daemon or a stale bundle used to
 * leave a window that could not be moved nor closed with the mouse.
 */
export function showFatal(
  el: HTMLElement,
  text: string,
  window_: ControlesDeVentana | null = null,
): void {
  el.replaceChildren();
  const doc = el.ownerDocument;
  if (window_ !== null && doc.documentElement.dataset["titlebar"] === "custom") {
    const bar = doc.createElement("nav");
    bar.className = "menubar";
    montarBarraDeTitulo(bar, window_.t, window_.pedir, false);
    el.append(bar);
  }
  el.append(text);
  el.dataset["shown"] = "true";
}

if (typeof document !== "undefined" && document.getElementById("screen") !== null) {
  void boot(tauriPort, document)
    .then((m) => {
      // Task 3.6's measurement harness reads this; nothing sensitive here.
      (window as unknown as { __norteMetrics: Metrics }).__norteMetrics = m;
    })
    .catch((e: unknown) => {
      // Without a daemon, `catalog()` rejects and `boot` throws. The process
      // mounts the window on purpose so it can SAY what happened (a binary
      // that dies in the terminal tells whoever opened it from a launcher
      // nothing at all), and without this `catch` the user saw an empty
      // window.
      const fatal = document.getElementById("fatal");
      if (fatal !== null) {
        fatal.textContent = e instanceof Error ? e.message : String(e);
        fatal.dataset["shown"] = "true";
      }
    });
}
