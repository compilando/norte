// The painting, and ONLY the painting.
//
// What comes in is what the host projected; what goes out is DOM nodes and
// semantic actions. Nothing is sorted here, no size is formatted, no
// decision is made about whether a command is available, and no path is
// composed: all of that already arrived resolved (ADR 0066, decision D14).
//
// Two boundary rules hold on every line of this file: text is set with
// `textContent` — never HTML, because a file name is data — and dynamic
// style is set through CSSOM, because the CSP blocks the `style` attribute.

import type {
  BrowserSlotView,
  HostCatalog,
  SlotView,
  UiAction,
  ViewerView,
  ViewSnapshot,
  ProcessesSlotView,
  WindowVerb,
} from "./types";
import { MARK_RULER_SPANS } from "./types";
import {
  markRulerImage,
  revelar,
  nota,
  OVERSCAN,
  colVar,
  place,
  newRow,
  updateRow,
  badge,
  sinCambios,
  emptyNode,
  errorNode,
  statusNodes,
  taskNode,
} from "./render/dom";
import type { Send, SlotDom } from "./render/dom";
import { hacerArrastrable } from "./render/mover";
import * as dialogs from "./render/dialogs";
import * as sync from "./render/sync";
import * as ai from "./render/ai";
import * as organize from "./render/organize";
import * as extensions from "./render/extensions";
import * as settings from "./render/settings";
import * as viewer from "./render/viewer";
import * as help from "./render/help";
import * as splash from "./render/splash";
import * as log from "./render/log";
import * as diskMap from "./render/diskmap";
import * as terminal from "./render/terminal";
import * as timeline from "./render/timeline";
import * as panelPlugin from "./render/panel";
import * as search from "./render/search";
import * as menus from "./render/menus";
import * as places from "./render/places";

/**
 * How far apart two clicks can be and still count as ONE double click, in
 * ms. The desktop's own interval cannot be read from a webview; 400ms is
 * what GNOME and KDE use by default.
 */
const DOBLE_CLIC_MS = 400;

/*
 * The members are public for TypeScript's sake because `render/*`'s
 * painters reach them through `this: Screen`; outside `src/render*` nobody
 * should touch them. The window's API is the one `main.ts` uses.
 */
export class Screen {
  readonly slots = new Map<number, SlotDom>();
  placementsKey = "";
  /** The dialog whose text field has already been seeded. */
  dialogoPintado: number | null = null;
  /// The live text field of the dialog above, to REUSE it.
  dialogoInput: HTMLInputElement | null = null;
  /** The LIVE nodes of a form-dialog, by field id (bridge 91).
   *
   *  The dialog box is rebuilt whole on every patch and every keystroke
   *  produces one. Reusing the node — instead of creating another and
   *  seeding it with what the host sent — is what keeps the masked
   *  PROJECTION from going back to the host as if it were what was typed,
   *  and it also keeps the caret. It is the same thing `dialogoInput` does
   *  for the single-field dialog; with a dozen controls a map is needed. */
  dialogoCampos: Map<string, HTMLInputElement | HTMLButtonElement> = new Map();
  /**
   * Settings' search bar, kept between repaints.
   *
   * Every keystroke in the search box triggers a host patch, i.e. a repaint:
   * if the `<input>` were recreated, it would be destroyed on the first
   * character and the focus and the caret would go with it. It is this
   * window's third spot with the same bug — a dialog's field and the log's
   * filter were the other two — and the same cure: keep the node.
   */
  settingsBarra: HTMLElement | null = null;
  /** The log's controls, kept between repaints (#326). */
  logControles: HTMLElement | null = null;
  /** The slot they belong to: a different slot, different controls. */
  logPintado: number | null = null;
  /** The last thing the host was told about how many rows fit. */
  logFilas: number | null = null;
  pendingLogRows: number | null = null;
  /// The help page that was painted, to keep its scroll.
  helpPintada: string | null = null;
  /// The last help-scroll request that was already applied (bridge 76).
  helpScrollSeq = 0;

  /**
   * `brief` mode's deadline, if one is armed.
   *
   * On the class and not the module: two `Screen`s in the same process —
   * both test files mount their own — would share a timer and step on each
   * other. And it is armed ONCE per appearance, not on every repaint: the
   * host sends the whole view on every patch, and re-arming it on each one
   * turned "1.2 seconds" into "1.2 seconds after the last patch", which
   * during startup is exactly when they keep arriving nonstop.
   */
  splashPlazo: ReturnType<typeof setTimeout> | null = null;

  /** The splash screen is already up: its deadline, if there was one, is already running. */
  splashPuesto = false;
  /// The `blob:` of the image being shown, to REVOKE it.
  ///
  /// An object URL that is not revoked is a buffer held for as long as the
  /// document lives. The revocation goes in the same place as the closing,
  /// not in a `finally` a future refactor could drop (ADR 0069).
  imagenUrl: string | null = null;
  /// Which image was requested, so as not to request the same one twice nor
  /// paint the previous one over the current viewer.
  imagenDe: string | null = null;
  /// The viewer that is on screen and the window size it was painted with.
  /// Every patch repaints the whole screen, and the viewer used to be
  /// rebuilt with every one — an advancing task, a notice — and on top of
  /// that it forced a reflow to measure its body. The session replaces
  /// `viewer` with a different object when it changes, so the SAME object is
  /// the same viewer.
  visorPintado: { viewer: ViewerView; firma: string } | null = null;
  /// Which object each overlay was last painted with, and with what window
  /// size and cell size (`paint`).
  capasPintadas = new Map<string, unknown>();
  capasFirma = "";
  /** The viewer lines already declared. */
  viewerRows = 0;
  /** The viewer body columns the host already knows. */
  viewerCols = 0;
  /** Help is open with the BODY focused. */
  helpBodyFocused = false;
  pendingRange = new Map<number, number>();
  /** The height the menu bar is reserving, already in CSS. */
  menuBarHeight: string | null = null;
  /** The same for the panel bar (#324). */
  panelBarHeight: string | null = null;
  /** And the width it reserves when it is the activity bar (bridge 84). */
  activityWidth: string | null = null;
  /** The last frame painted: what gets repainted when something local changes. */
  ultimaVista: ViewSnapshot | null = null;
  /** Local notice for a command rejected at the boundary (`rejected`). */
  rechazo: string | null = null;
  /** The last click on a row, to count the double click here instead of
   *  depending on the engine's `dblclick` event (see a row's `mousedown`).
   *  `null` = none pending a match. */
  ultimoClic: { slot: number; key: number; at: number } | null = null;
  /** The reservation changed: the host has to hear the new height. */
  viewportSucio = false;

  constructor(
    readonly root: HTMLElement,
    readonly menuRoot: HTMLElement,
    readonly panelBarRoot: HTMLElement,
    readonly paletteRoot: HTMLElement,
    readonly whichKeyRoot: HTMLElement,
    readonly helpRoot: HTMLElement,
    readonly settingsRoot: HTMLElement,
    readonly extensionsRoot: HTMLElement,
    readonly themeRoot: HTMLElement,
    readonly pickerRoot: HTMLElement,
    readonly profilesRoot: HTMLElement,
    readonly layoutsRoot: HTMLElement,
    readonly columnsRoot: HTMLElement,
    readonly searchRoot: HTMLElement,
    readonly compareRoot: HTMLElement,
    readonly syncRoot: HTMLElement,
    readonly agentsRoot: HTMLElement,
    readonly pluginOutputRoot: HTMLElement,
    readonly programOutputRoot: HTMLElement,
    readonly viewerRoot: HTMLElement,
    readonly dialogsRoot: HTMLElement,
    readonly aiRenameRoot: HTMLElement,
    readonly organizeRoot: HTMLElement,
    readonly splashRoot: HTMLElement,
    readonly gotoRoot: HTMLElement,
    readonly catalog: HostCatalog,
    readonly send: Send,
    /**
     * Fetches the open image's bytes. No path: the renderer does not name
     * files, it is served the one the host decided to open (ADR 0069).
     */
    readonly fetchImage: () => Promise<ArrayBuffer> = () =>
      Promise.resolve(new ArrayBuffer(0)),
    /** The window's own title bar (ADR 0136): what it asks the window for. */
    readonly windowControl: (verb: WindowVerb) => void = () => undefined,
  ) {}

  /**
   * Did what the menu bar reserves change since it was last asked? A
   * consuming query: whoever calls it declares the height again.
   */
  takeViewportDirty(): boolean {
    const dirty = this.viewportSucio;
    this.viewportSucio = false;
    return dirty;
  }

  /** A Fluent key's text, translated IN RUST. The key itself, if it is missing. */
  t(key: string): string {
    return this.catalog.strings[key] ?? key;
  }

  /**
   * A command the host rejected at the boundary (does not deserialize,
   * broken contract): it never reached its mailbox, so no host state can
   * account for it. Painted here, on the status bar, until the first
   * accepted command. The error's detail goes to the console: it is text
   * from the other end, and the bar says which command and that it was not
   * understood.
   */
  rejected(action: UiAction, error: unknown): void {
    console.error("the host did not accept the action:", action.action, error);
    this.rechazo = `${this.t("gui-msg-action-rejected")}: ${action.action}`;
    this.repaintStatus();
  }

  /** An accepted command withdraws the notice; says whether there was one. */
  accepted(): boolean {
    if (this.rechazo === null) {
      return false;
    }
    this.rechazo = null;
    this.repaintStatus();
    return true;
  }

  /** Repaints the status slot with the last frame, if there is one. */
  repaintStatus(): void {
    if (this.ultimaVista !== null) {
      this.paint(this.ultimaVista);
    }
  }

  /** A layout cell's size, in real pixels. */
  cell(): { w: number; h: number } {
    const cs = getComputedStyle(document.documentElement);
    return {
      w: Number.parseFloat(cs.getPropertyValue("--cell-w")) || 8,
      h: Number.parseFloat(cs.getPropertyValue("--cell-h")) || 22,
    };
  }

  paint(view: ViewSnapshot): void {
    this.ultimaVista = view;
    const cell = this.cell();
    const key = view.layout.placements
      .map((p) => `${p.slot_id}:${p.x},${p.y},${p.width},${p.height}`)
      .join("|");
    if (key !== this.placementsKey) {
      this.rebuild(view, cell);
      this.placementsKey = key;
    }
    // Whether the role EXISTS and whether it gets MARKED are two questions.
    // The second arrives COMPUTED from the host (`layout.mark_target`): it
    // is decided by the shared crate, and counting it here would repeat in
    // TypeScript a number that already lives in Rust — the same decision in
    // two places.
    const markTarget = view.layout.mark_target ?? false;
    for (const p of view.layout.placements) {
      const dom = this.slots.get(p.slot_id);
      const slot = view.slots.find((s) => s.slot_id === p.slot_id);
      if (dom === undefined || slot === undefined) {
        continue;
      }
      const role = p.role === "target" && !markTarget ? null : p.role;
      dom.root.dataset["role"] = role ?? "";
      dom.root.setAttribute("aria-current", p.role === "active" ? "true" : "false");
      this.paintTabs(
        dom,
        view.layout.tabs.find((g) => g.slot_id === p.slot_id),
      );
      this.paintSlot(dom, slot, view, cell);
    }
    this.paintMenu(view.menu, view.layout_buttons ?? []);
    this.paintPanelBar(view.panel_bar);
    // The overlays, each ONLY if its data changed. Every patch repaints the
    // whole screen and all of them used to rebuild their DOM on every one:
    // with help or settings open, an advancing task redid the dialog several
    // times a second. The session replaces an overlay's object when its
    // change arrives, so the SAME object is the same overlay; and a window
    // or cell size change repaints all of them, because several measure what
    // fits.
    const signature = `${String(window.innerWidth)}x${String(window.innerHeight)}|${String(cell.w)}x${String(cell.h)}`;
    if (signature !== this.capasFirma) {
      this.capasFirma = signature;
      this.capasPintadas.clear();
    }
    const layer = <T>(key: string, value: T, painter: (v: T) => void): void => {
      if (this.capasPintadas.has(key) && this.capasPintadas.get(key) === value) {
        return;
      }
      this.capasPintadas.set(key, value);
      painter.call(this, value);
    };
    layer("palette", view.palette, this.paintPalette);
    layer("goto", view.goto ?? null, this.paintGoto);
    layer("wizard", view.wizard ?? null, this.paintWizard);
    layer("whichkey", view.whichkey, this.paintWhichKey);
    layer("help", view.help, this.paintHelp);
    layer("settings", view.settings, this.paintSettings);
    layer("extensions", view.extensions, this.paintExtensions);
    layer("agents", view.agents, this.paintAgents);
    layer("plugin_output", view.plugin_output, this.paintPluginOutput);
    layer("program_output", view.program_output, this.paintProgramOutput);
    layer("theme", view.theme, this.paintTheme);
    layer("picker", view.picker, this.paintPicker);
    layer("profiles", view.profiles, this.paintProfiles);
    layer("layouts", view.layouts, this.paintLayouts);
    layer("columns", view.columns, this.paintColumns);
    layer("search", view.search, this.paintSearch);
    layer("compare", view.compare, this.paintCompare);
    layer("sync", view.sync, this.paintSync);
    this.paintViewer(view.viewer);
    layer("ai_rename", view.ai_rename, this.paintAiRename);
    layer("organize", view.organize, this.paintOrganize);
    layer("dialogs", view.dialogs, this.paintDialogs);
    // THE LAST ONE: the splash screen goes in front of everything else, and
    // on this sheet stacking is document order.
    this.paintSplash(view.splash ?? null);
  }

  /** In `render/menus.ts`. */
  readonly paintPanelBar = menus.paintPanelBar;

  /** In `render/menus.ts`. */
  readonly paintMenu = menus.paintMenu;

  /** In `render/menus.ts`. */
  readonly paintPalette = menus.paintPalette;
  readonly paintGoto = menus.paintGoto;
  readonly paintWizard = menus.paintWizard;

  /** In `render/menus.ts`. */
  readonly paintWhichKey = menus.paintWhichKey;

  /** In `render/splash.ts`. */
  readonly paintSplash = splash.paintSplash;

  /** In `render/help.ts`. */
  readonly paintHelp = help.paintHelp;

  /** In `render/help.ts`. */
  readonly desplazarAyuda = help.desplazarAyuda;

  /** In `render/help.ts`. */
  readonly helpSidebar = help.helpSidebar;

  /** In `render/help.ts`. */
  readonly helpBody = help.helpBody;

  /** In `render/help.ts`. */
  readonly helpBlock = help.helpBlock;

  /** In `render/help.ts`. */
  readonly helpSpan = help.helpSpan;

  /** In `render/settings.ts`. */
  readonly paintSettings = settings.paintSettings;

  /** In `render/extensions.ts`. */
  readonly paintExtensions = extensions.paintExtensions;

  /** In `render/extensions.ts`. */
  readonly extensionDetail = extensions.extensionDetail;

  /** In `render/extensions.ts`. */
  readonly extensionPaneHead = extensions.extensionPaneHead;

  /** In `render/extensions.ts`. */
  readonly extensionCommands = extensions.extensionCommands;

  /** In `render/extensions.ts`. */
  readonly paintAgents = extensions.paintAgents;

  /** In `render/extensions.ts`. */
  readonly paintPluginOutput = extensions.paintPluginOutput;

  /** In `render/extensions.ts`. */
  readonly paintProgramOutput = extensions.paintProgramOutput;

  /** In `render/settings.ts`. */
  readonly paintTheme = settings.paintTheme;

  /** In `render/sync.ts`. */
  readonly paintSync = sync.paintSync;

  /** In `render/sync.ts`. */
  readonly syncStep = sync.syncStep;

  /** In `render/sync.ts`. */
  readonly paintCompare = sync.paintCompare;

  /** In `render/sync.ts`. */
  readonly compareFace = sync.compareFace;

  /** In `render/search.ts`. */
  readonly paintSearch = search.paintSearch;

  /** In `render/settings.ts`. */
  readonly paintColumns = settings.paintColumns;

  /** In `render/settings.ts`. */
  readonly paintProfiles = settings.paintProfiles;

  /** In `render/settings.ts`. */
  readonly paintLayouts = settings.paintLayouts;

  /** In `render/settings.ts`. */
  readonly paintPicker = settings.paintPicker;

  /** In `render/settings.ts`. */
  readonly settingsRow = settings.settingsRow;

  /** In `render/viewer.ts`. */
  readonly paintViewer = viewer.paintViewer;

  /** In `render/viewer.ts`. */
  readonly soltarImagen = viewer.soltarImagen;

  /** In `render/viewer.ts`. */
  readonly pintarImagen = viewer.pintarImagen;

  /** The `<img>` with its size declared, so it does not jump on load. */
  nodoImagen(
    url: string,
    img: { format: string; width: number; height: number },
    zoom = 100,
  ): HTMLElement {
    const el = document.createElement("img");
    el.className = "viewer-image";
    el.src = url;
    // The DECLARED size, which the host already compared against the
    // budget: without it the box jumps when the image loads.
    el.width = img.width;
    el.height = img.height;
    el.alt = img.format;
    // The ZOOM (bridge 80). It is a percentage of the FITTED size, and
    // fitted is decided by the sheet (`max-width/max-height: 100%`), so here
    // only the cap is multiplied: `--zoom: 1.5` lets the image reach 150% of
    // the slot, and the slot takes care of letting it overflow and scroll.
    //
    // As a variable and not as `transform: scale()`: scaling leaves the slot
    // believing the image still measures what it used to, so no scrollbar
    // appears and whatever spills out is unreachable.
    el.style.setProperty("--zoom", String(zoom / 100));
    if (zoom <= 100) {
      return el;
    }
    // Enlarged, the image does not fit, and without a box that overflows,
    // whatever spills out is nowhere. The box only exists when needed: at
    // fitted size it is one more node between the slot and the picture.
    const box = document.createElement("div");
    box.className = "viewer-image-box";
    box.append(el);
    return box;
  }

  /**
   * The GRIPS on the borders between neighboring slots.
   *
   * Rebuilt with the layout, not on every frame: as long as the layout does
   * not change, the border is where it was. And they come from the SAME
   * `placements` that positions the slots — two calculations of where a
   * border is are a border grabbed in one place and moved from another.
   *
   * What gets sent is the POINTER's position in cells, not a size: which
   * pair splits and how much each gets is decided by the host, which is the
   * one that has the layout and the minimums (ADR 0069).
   */
  buildHandles(view: ViewSnapshot, cell: { w: number; h: number }): void {
    /** How much of each side of the border can be grabbed, in pixels. */
    const AGARRE = 6;
    for (const a of view.layout.placements) {
      for (const b of view.layout.placements) {
        const vertical =
          b.x === a.x + a.width && b.y < a.y + a.height && a.y < b.y + b.height;
        const horizontal =
          b.y === a.y + a.height && b.x < a.x + a.width && a.x < b.x + b.width;
        if (!vertical && !horizontal) {
          continue;
        }
        const el = document.createElement("div");
        el.className = vertical ? "resize-handle col" : "resize-handle row";
        if (vertical) {
          el.style.setProperty("left", `${(a.x + a.width) * cell.w - AGARRE / 2}px`);
          el.style.setProperty("top", `${Math.max(a.y, b.y) * cell.h}px`);
          el.style.setProperty("width", `${AGARRE}px`);
          el.style.setProperty(
            "height",
            `${(Math.min(a.y + a.height, b.y + b.height) - Math.max(a.y, b.y)) * cell.h}px`,
          );
        } else {
          el.style.setProperty("top", `${(a.y + a.height) * cell.h - AGARRE / 2}px`);
          el.style.setProperty("left", `${Math.max(a.x, b.x) * cell.w}px`);
          el.style.setProperty("height", `${AGARRE}px`);
          el.style.setProperty(
            "width",
            `${(Math.min(a.x + a.width, b.x + b.width) - Math.max(a.x, b.x)) * cell.w}px`,
          );
        }
        const slot = a.slot_id;
        el.addEventListener("pointerdown", (e: PointerEvent) => {
          e.preventDefault();
          this.arrastrarBorde(slot, vertical, cell);
        });
        this.root.append(el);
      }
    }
  }

  /**
   * A border's drag, from the moment it is grabbed until it is released.
   *
   * Through `window` and NOT through pointer capture on the grip: every
   * step of the drag changes the layout, every layout change rebuilds the
   * slots and the grips (`rebuild`), and the grabbed grip disappeared along
   * with its capture after the FIRST step — the border moved one cell, or
   * none, and stayed there. The slot is named by id, which survives any
   * rebuild.
   */
  arrastrarBorde(slot: number, vertical: boolean, cell: { w: number; h: number }): void {
    const root = document.documentElement;
    root.dataset["dragging"] = vertical ? "border-col" : "border-row";
    let last = Number.NaN;
    const move = (e: PointerEvent): void => {
      // Against the board's ORIGIN, not the window. `#screen` gets pushed
      // down by whatever the menu bar and the panel bar measure
      // (`margin-top`), so a raw `clientY` gave the host one row too many
      // for every row of chrome: the border jumped the moment you started
      // dragging it.
      const origin = this.root.getBoundingClientRect();
      const cells = vertical
        ? Math.round((e.clientX - origin.left) / cell.w)
        : Math.round((e.clientY - origin.top) / cell.h);
      // One command per CELL, not per pixel: the host cannot tell the
      // difference and the bridge does not fill up with duplicates.
      if (cells === last) {
        return;
      }
      last = cells;
      this.send({ action: "resize_slot", slot_id: slot, cells });
    };
    const release = (): void => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", release);
      window.removeEventListener("pointercancel", release);
      delete root.dataset["dragging"];
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", release);
    window.addEventListener("pointercancel", release);
  }

  rebuild(view: ViewSnapshot, cell: { w: number; h: number }): void {
    this.root.replaceChildren();
    this.slots.clear();
    for (const p of view.layout.placements) {
      const el = document.createElement("section");
      el.className = "slot";
      el.setAttribute("role", "group");
      // Who this slot is, in the DOM. Without this the only way to find it
      // was its POSITION among siblings, an implicit correspondence between
      // `placements`'s order and the DOM's.
      el.dataset["slotId"] = String(p.slot_id);
      place(el, p, cell);
      // The TAB bar goes above the title: it is what says what is behind
      // what is being painted.
      const tabs = document.createElement("div");
      tabs.className = "slot-tabs";
      tabs.dataset["open"] = "false";
      const title = document.createElement("header");
      title.className = "slot-title";
      const header = document.createElement("div");
      header.className = "slot-columns";
      header.setAttribute("role", "row");
      const scroller = document.createElement("div");
      scroller.className = "scroller";
      const canvas = document.createElement("div");
      canvas.className = "canvas";
      scroller.append(canvas);
      // The footer under the listing: counts, marked and free space, already
      // worded in Rust. Empty = `[ui] pane_footer` is off, and it takes no
      // space.
      const footer = document.createElement("footer");
      footer.className = "slot-footer";
      footer.hidden = true;
      el.append(tabs, title, header, scroller, footer);
      this.root.append(el);
      const busy = document.createElement("p");
      busy.className = "slot-busy";
      busy.hidden = true;
      const dom: SlotDom = {
        root: el,
        tabs,
        title,
        header,
        scroller,
        canvas,
        footer,
        busy,
        rows: new Map(),
        lastRange: null,
        generation: 0,
      };
      this.slots.set(p.slot_id, dom);
      this.wire(p.slot_id, dom);
    }
    // The grips, AFTER the slots, and that is why last.
    //
    // This stylesheet does not use `z-index` anywhere on purpose — it says
    // so itself in the menu's veil — so stacking is decided by document
    // ORDER. They used to be built first, and since a slot is also
    // `absolute`, every panel covered them: `pointerdown` never reached
    // them and resizing with the mouse did not work. A transparent
    // six-pixel grip under a panel is not a grip.
    this.buildHandles(view, cell);
  }

  wire(slotId: number, dom: SlotDom): void {
    // Clicking ANY part of a panel focuses it: the header, the gap under the
    // last row, the border. It used to be only on the rows, so a panel with
    // none — or a click on its title — painted with another one's border.
    //
    // In CAPTURE so focus travels before whatever the specific click does
    // (sorting, selecting): it is the order the host already sees from the
    // keyboard.
    dom.root.addEventListener(
      "mousedown",
      () => {
        if (dom.root.dataset["role"] !== "active") {
          this.send({ action: "focus_slot", slot_id: slotId });
        }
      },
      true,
    );
    // The mouse's SIDE buttons are back and forward in navigation history
    // (spec 2026-09-15 D1), every desktop manager's convention. On
    // `mouseup`, because the capturing `mousedown` above has already
    // focused the panel and the host only accepts the active slot's trail;
    // and with `preventDefault`, so the webview does not mistake them for
    // page navigation.
    // Dragging the panel by its TITLE moves it (ADR 0138).
    hacerArrastrable(this, dom.title, slotId);
    dom.root.addEventListener("mouseup", (e) => {
      if (e.button !== 3 && e.button !== 4) {
        return;
      }
      e.preventDefault();
      this.send({ action: "history", slot_id: slotId, back: e.button === 3 });
    });
    dom.header.addEventListener("mousedown", (e) => {
      const target = e.target;
      if (!(target instanceof Element)) {
        return;
      }
      // The grip comes BEFORE sorting: it is inside the header that sorts,
      // and a drag is not a click.
      const grip = target.closest(".col-grip");
      if (grip instanceof HTMLElement) {
        e.preventDefault();
        this.dragColumn(slotId, dom, grip, e);
        return;
      }
      const col = target.closest('[data-sortable="true"]');
      if (!(col instanceof HTMLElement)) {
        return;
      }
      const id = col.dataset["column"];
      if (id === undefined) {
        return;
      }
      e.preventDefault();
      // What a click on the SAME column does — reverse — is decided by the
      // host.
      this.send({ action: "sort_by", slot_id: slotId, column: id });
    });
    dom.scroller.addEventListener("scroll", () => {
      this.scheduleRange(slotId, dom);
    });
    dom.scroller.addEventListener("mousedown", (e) => {
      const target = e.target;
      if (!(target instanceof Element)) {
        return;
      }
      const rowEl = target.closest(".row");
      if (!(rowEl instanceof HTMLElement)) {
        return;
      }
      const rowKey = Number(rowEl.dataset["key"]);
      if (Number.isNaN(rowKey)) {
        return;
      }
      e.preventDefault();
      // The focus was already sent by the whole panel's capturing listener.
      // The mark checkbox toggles the mark with no modifier: it is the same
      // as Ctrl+click, said with the mouse alone.
      if (target.closest(".row-check") !== null) {
        this.send({
          action: "toggle_mark",
          slot_id: slotId,
          key: rowKey,
          generation: dom.generation,
        });
        return;
      }
      if (e.shiftKey) {
        // The range is marked by the HOST: what is included and what is not
        // — `..`, for instance — is a shared selection rule, not the
        // renderer's.
        const from = this.cursorOf(slotId);
        if (from !== null) {
          this.send({
            action: "mark_range",
            slot_id: slotId,
            from,
            to: rowKey,
            generation: dom.generation,
          });
          return;
        }
      }
      if (e.ctrlKey || e.metaKey) {
        this.send({
          action: "toggle_mark",
          slot_id: slotId,
          key: rowKey,
          generation: dom.generation,
        });
        return;
      }
      this.send({
        action: "select_row",
        slot_id: slotId,
        key: rowKey,
        generation: dom.generation,
      });
      // The DOUBLE click is counted HERE, and the engine's `dblclick` event
      // is not listened to: that event is the only door through which a
      // directory used to be entered with the mouse, and it depends on how
      // the webview interprets a sequence of clicks on a row that also
      // repaints in between. Two `mousedown`s on the SAME row within
      // `DOBLE_CLIC_MS` are a double click, whether the engine says so or
      // not; it is the same thing the terminal does, which also counts them
      // itself.
      //
      // Placed after `select_row` on purpose: the cursor stays where the
      // double click happened, and the host receives the two actions in
      // order.
      const now = Date.now();
      const previous = this.ultimoClic;
      this.ultimoClic = { slot: slotId, key: rowKey, at: now };
      if (
        previous !== null &&
        previous.slot === slotId &&
        previous.key === rowKey &&
        now - previous.at <= DOBLE_CLIC_MS
      ) {
        // A burst's third click does not activate again.
        this.ultimoClic = null;
        this.send({
          action: "activate",
          slot_id: slotId,
          key: rowKey,
          generation: dom.generation,
        });
      }
    });
  }

  /**
   * Drags a header's border (bridge 64).
   *
   * While it lasts, only the width variable on the slot's root moves: the
   * header and the cells read it and nothing gets repainted. On release,
   * the width in CELLS — rounded over `--cell-w` — goes to the host, which
   * bounds it, saves it in `[ui.columns] spec.width` and returns every
   * slot's header. The mouse is captured on the document: a fast drag
   * leaves the six-pixel grip on the very first move.
   */
  dragColumn(slotId: number, dom: SlotDom, grip: HTMLElement, start: MouseEvent): void {
    const id = grip.dataset["grip"];
    const col = grip.parentElement;
    if (id === undefined || col === null) {
      return;
    }
    const v = colVar(id);
    const cellW = this.cell().w;
    const start_ = col.getBoundingClientRect().width;
    const x0 = start.clientX;
    let width = start_;
    col.dataset["resizing"] = "true";
    const doc = dom.root.ownerDocument;
    const move = (e: MouseEvent): void => {
      width = Math.max(cellW, start_ + (e.clientX - x0));
      dom.root.style.setProperty(v, `${String(width)}px`);
    };
    const release = (): void => {
      doc.removeEventListener("mousemove", move);
      doc.removeEventListener("mouseup", release);
      delete col.dataset["resizing"];
      const cells = Math.max(1, Math.round(width / cellW));
      this.send({ action: "resize_column", slot_id: slotId, column: id, cells });
    };
    doc.addEventListener("mousemove", move);
    doc.addEventListener("mouseup", release);
  }

  cursorOf(slotId: number): number | null {
    const dom = this.slots.get(slotId);
    if (dom === undefined) {
      return null;
    }
    for (const [key, el] of dom.rows) {
      if (el.getAttribute("aria-selected") === "true") {
        return key;
      }
    }
    return null;
  }

  /** The scroll is painted by the renderer; the only thing that crosses is which rows are needed. */
  scheduleRange(slotId: number, dom: SlotDom): void {
    const pending = this.pendingRange.get(slotId);
    if (pending !== undefined) {
      return;
    }
    const handle = requestAnimationFrame(() => {
      this.pendingRange.delete(slotId);
      const { h } = this.cell();
      const first = Math.max(0, Math.floor(dom.scroller.scrollTop / h) - OVERSCAN);
      const count = Math.ceil(dom.scroller.clientHeight / h) + OVERSCAN * 2;
      if (dom.lastRange?.first === first && dom.lastRange.count === count) {
        return;
      }
      dom.lastRange = { first, count };
      this.send({ action: "set_visible_range", slot_id: slotId, first, count });
    });
    this.pendingRange.set(slotId, handle);
  }

  /** In `render/menus.ts`. */
  readonly paintTabs = menus.paintTabs;

  paintSlot(
    dom: SlotDom,
    slot: SlotView,
    view: ViewSnapshot,
    cell: { w: number; h: number },
  ): void {
    // The KIND on the slot, for the stylesheet: the status bar and the task
    // strip are a ROW, with no title nor frame — with the title on top, the
    // whole row got eaten by the title and the bar was not visible
    // (2026-09-21 capture).
    dom.root.dataset["kind"] = slot.kind === "unsupported" ? slot.kind_name : slot.kind;
    if (slot.kind === "places") {
      this.paintPlaces(dom, slot);
      return;
    }
    if (slot.kind === "tree") {
      this.paintTree(dom, slot);
      return;
    }
    if (slot.kind === "metadata") {
      this.paintMetadata(dom, slot);
      return;
    }
    if (slot.kind === "preview") {
      this.paintPreview(dom, slot);
      return;
    }
    if (slot.kind === "processes") {
      this.paintProcesses(dom, slot, view);
      return;
    }
    if (slot.kind === "log") {
      this.paintLog(dom, slot);
      return;
    }
    if (slot.kind === "panel") {
      this.paintPanel(dom, slot);
      return;
    }
    if (slot.kind === "disk_map") {
      this.paintDiskMap(dom, slot);
      return;
    }
    if (slot.kind === "timeline") {
      this.paintTimeline(dom, slot);
      return;
    }
    if (slot.kind === "terminal") {
      this.paintTerminal(dom, slot);
      return;
    }
    if (slot.kind === "unsupported") {
      // The kind's name comes from the user's layout file: if the host
      // masked it, it is said — the same criterion as everywhere else.
      this.paintAux(dom, slot.kind_name, view, slot.kind_name_hostile);
      return;
    }
    if (slot.kind === "browser") {
      this.paintBrowser(dom, slot, cell);
      return;
    }
    // A `kind` this renderer does not know is painted as what it IS: a slot
    // that does not know how to paint. It used to fall back to the default
    // listing — TypeScript had already narrowed the type, so it compiled —
    // and a new host kind would have painted as a listing with `rows` set
    // to `undefined`, i.e. an empty table indistinguishable from an empty
    // directory.
    this.paintAux(dom, (slot as { kind: string }).kind, view);
  }

  /** In `render/places.ts`. */
  readonly paintTree = places.paintTree;

  /** In `render/places.ts`. */
  readonly paintPlaces = places.paintPlaces;

  /** In `render/viewer.ts`. */
  readonly paintMetadata = viewer.paintMetadata;

  /** In `render/viewer.ts`. */
  readonly paintMetadataBody = viewer.paintMetadataBody;

  /** In `render/viewer.ts`. */
  readonly paintPreview = viewer.paintPreview;

  /**
   * The process panel: the SAME tasks as the strip, with its cursor.
   *
   * There is no second list: two task lists drift apart, and the one you
   * see stops being the one that gets canceled.
   */
  paintProcesses(dom: SlotDom, slot: ProcessesSlotView, view: ViewSnapshot): void {
    dom.root.setAttribute("aria-label", this.t("processes-title"));
    dom.title.textContent = this.t("processes-title");
    dom.scroller.className = "processes";
    if (view.tasks.length === 0) {
      dom.scroller.replaceChildren(nota(this.t("processes-empty")));
      return;
    }
    const list = document.createElement("ul");
    list.className = "processes-rows";
    list.setAttribute("role", "listbox");
    for (const [i, t] of view.tasks.entries()) {
      const row = document.createElement("li");
      row.className = "processes-row";
      row.id = `process-row-${String(i)}`;
      row.setAttribute("role", "option");
      row.setAttribute("aria-selected", String(slot.cursor === i));
      row.append(taskNode(t, (k) => this.t(k)));
      list.append(row);
    }
    if (slot.cursor !== null) {
      list.setAttribute("aria-activedescendant", `process-row-${String(slot.cursor)}`);
      revelar(list.querySelector(`#process-row-${String(slot.cursor)}`) ?? undefined);
    }
    dom.scroller.replaceChildren(list);
  }

  /** In `render/log.ts`. */
  readonly paintLog = log.paintLog;
  readonly paintTerminal = terminal.paintTerminal;

  /** In `render/panel.ts`. */
  readonly paintPanel = panelPlugin.paintPanel;

  /** In `render/diskmap.ts`. */
  readonly paintDiskMap = diskMap.paintDiskMap;
  /** In `render/timeline.ts`. */
  readonly paintTimeline = timeline.paintTimeline;

  /** In `render/log.ts`. */
  readonly selectorDeFuente = log.selectorDeFuente;

  /** In `render/log.ts`. */
  readonly crearControlesDeRegistro = log.crearControlesDeRegistro;

  /** In `render/log.ts`. */
  readonly scheduleLogRows = log.scheduleLogRows;

  paintAux(
    dom: SlotDom,
    kindName: string,
    view: ViewSnapshot,
    kindHostile = false,
  ): void {
    dom.root.setAttribute("aria-label", kindName);
    dom.title.textContent = "";
    if (kindHostile) {
      dom.title.replaceChildren(
        document.createTextNode(kindName),
        badge(this.t("hostile-name")),
      );
    }
    if (kindName === "status") {
      dom.scroller.className = "statusbar";
      dom.scroller.setAttribute("role", "status");
      dom.scroller.setAttribute("aria-live", "polite");
      // The right-side elements (ADR 0132) come back by ID: the host
      // resolves the command against its current list and runs it through
      // the same dispatch as the key.
      dom.scroller.replaceChildren(
        ...statusNodes(
          view.status,
          view.connection.state,
          (k) => this.t(k),
          this.rechazo,
          view.status_items ?? [],
          (id) => {
            this.send({ action: "status_item_activate", id });
          },
        ),
      );
      return;
    }
    if (kindName === "tasks") {
      dom.scroller.className = "tasks";
      dom.scroller.setAttribute("role", "list");
      dom.scroller.replaceChildren(
        ...view.tasks.map((t) => taskNode(t, (k) => this.t(k))),
      );
      return;
    }
    dom.scroller.className = "empty";
    dom.scroller.textContent = kindName;
  }

  paintBrowser(
    dom: SlotDom,
    slot: BrowserSlotView,
    cell: { w: number; h: number },
  ): void {
    // The path in its own node, and not as the header's loose text: it is
    // the ONLY thing that can be truncated when it does not fit. With the
    // path as direct text, a long one pushed out of view everything that
    // came after it — the hostile △ and the skipped-entries notice — and
    // they disappeared silently, exactly the opposite of what they exist to
    // do. The title is only rebuilt if what it says changed. The "waiting"
    // notice is its last child and is handled apart (further below): it has
    // its own threshold and a node that is not recreated.
    const titleSignature = JSON.stringify([
      slot.path_display,
      slot.path_segments ?? null,
      slot.path_hostile,
      slot.generation,
      slot.filling_note ?? "",
      slot.skipped_note,
      slot.names_note ?? "",
      slot.pruned_note ?? "",
      slot.hidden_note,
      slot.marked_note ?? "",
    ]);
    if (!sinCambios(dom.title, titleSignature)) {
      this.paintBrowserTitle(dom, slot);
    }
    dom.root.setAttribute("aria-label", slot.path_display);
    dom.generation = slot.generation;
    if (
      !sinCambios(
        dom.footer,
        JSON.stringify([slot.footer ?? "", slot.used_ratio ?? null]),
      )
    ) {
      this.paintBrowserFooter(dom, slot);
    }
    // The thin line on the bottom border (ADR 0148): two pixels that say
    // something is arriving HERE, with no text and without taking a row
    // away from the listing.
    const incoming = slot.progress ?? null;
    if (incoming === null) {
      delete dom.root.dataset["progress"];
      dom.root.style.removeProperty("--slot-progress");
    } else {
      dom.root.dataset["progress"] = String(incoming);
      dom.root.style.setProperty("--slot-progress", `${String(incoming)}%`);
    }
    this.paintBrowserRest(dom, slot, cell);
  }

  /** The path with its breadcrumbs and the listing's notices, in the slot's title. */
  paintBrowserTitle(dom: SlotDom, slot: BrowserSlotView): void {
    const path = document.createElement("span");
    path.className = "title-path";
    const crumbs = slot.path_segments ?? [];
    if (crumbs.length === 0) {
      path.textContent = slot.path_display;
    } else {
      // BREADCRUMBS (bridge 65): one button per segment, with a separator;
      // the last one is the current directory and does not navigate. The
      // whole path stays in the node's `title` and in the slot's
      // `aria-label`, for whoever wants to read or copy it whole.
      path.title = slot.path_display;
      for (const [i, segment] of crumbs.entries()) {
        if (i > 0) {
          const sep = document.createElement("span");
          sep.className = "crumb-sep";
          sep.setAttribute("aria-hidden", "true");
          sep.textContent = "›";
          path.append(sep);
        }
        const crumb = document.createElement("button");
        crumb.type = "button";
        crumb.className = "crumb";
        crumb.textContent = segment;
        const current = i === crumbs.length - 1;
        crumb.dataset["current"] = String(current);
        // The root (the scheme, `⟨file⟩`) is painted dimmed (phase D): it
        // says which provider the path belongs to, and it is the breadcrumb
        // that changes least.
        crumb.dataset["root"] = String(i === 0);
        crumb.disabled = current;
        if (!current) {
          crumb.addEventListener("click", () => {
            // With the listing generation that painted these breadcrumbs: if
            // the slot navigated meanwhile, the depth talked about a
            // different path and the host rejects it instead of
            // reinterpreting it.
            this.send({
              action: "breadcrumb_activate",
              slot_id: slot.slot_id,
              depth: i,
              generation: slot.generation,
            });
          });
        }
        path.append(crumb);
      }
    }
    dom.title.replaceChildren(path);
    if (slot.path_hostile) {
      path.append(badge(this.t("hostile-name")));
    }
    // Everything that says the listing is NOT what it looks like, already
    // worded in Rust. Goes in the HEADER and not at the end of the list:
    // what is missing is not there, so there is no row where the reader
    // could stumble over it.
    //
    // The ORDER is the decision, and it is the same as the terminal's bar:
    // WARNINGS — the incomplete listing, the reinterpretation of names, the
    // marks that fell off — come before the marked COUNT. Room runs out,
    // and a truncated warning stops warning while a truncated count only
    // stops counting.
    //
    // `role="status"` only on the WARNINGS. The marked and the fill counts
    // are counters of something the reader just did or that is happening in
    // view: announcing them by voice on every keystroke turns the live
    // region into noise, and then the warning that does matter arrives
    // inside the noise.
    const notes: [string, string, boolean][] = [
      ["slot-filling", slot.filling_note ?? "", false],
      ["slot-skipped", slot.skipped_note, true],
      ["slot-names", slot.names_note ?? "", true],
      ["slot-pruned", slot.pruned_note ?? "", true],
      ["slot-hidden", slot.hidden_note, true],
      ["slot-marked", slot.marked_note ?? "", false],
    ];
    for (const [cls, text, isWarning] of notes) {
      if (text === "") {
        continue;
      }
      const note = document.createElement("span");
      note.className = cls;
      if (isWarning) {
        note.setAttribute("role", "status");
      }
      note.textContent = text;
      dom.title.append(note);
    }
    dom.title.append(dom.busy);
  }

  /** The slot's footer: counts and the space indicator. */
  paintBrowserFooter(dom: SlotDom, slot: BrowserSlotView): void {
    // The footer (bridge 63): empty = off, and then it takes no row.
    const footer = slot.footer ?? "";
    dom.footer.textContent = footer;
    dom.footer.hidden = footer === "";
    // The space indicator (bridge 65): two pixels under the footer's text,
    // filled up to the volume's used space. No data, no bar.
    const used = slot.used_ratio ?? null;
    if (footer !== "" && used !== null) {
      const gauge = document.createElement("span");
      gauge.className = "slot-gauge";
      gauge.setAttribute("role", "progressbar");
      gauge.setAttribute("aria-valuemin", "0");
      gauge.setAttribute("aria-valuemax", "100");
      const pct = Math.round(Math.min(1, Math.max(0, used)) * 100);
      gauge.setAttribute("aria-valuenow", String(pct));
      gauge.dataset["level"] = pct >= 90 ? "critical" : pct >= 75 ? "high" : "normal";
      const fill = document.createElement("i");
      fill.style.width = `${String(pct)}%`;
      gauge.append(fill);
      dom.footer.append(gauge);
    }
  }

  /** The column header, the listing's state and the rows. */
  paintBrowserRest(
    dom: SlotDom,
    slot: BrowserSlotView,
    cell: { w: number; h: number },
  ): void {
    this.paintHeader(dom, slot);

    const total = slot.total_rows ?? slot.rows.length;
    dom.canvas.style.setProperty("height", `${total * cell.h}px`);
    // The mark ruler (ADR 0135): where the ones that are out of view are.
    const ruler = markRulerImage(slot.mark_ruler ?? [], MARK_RULER_SPANS);
    if (ruler === "") {
      dom.scroller.style.removeProperty("--mark-ruler");
      delete dom.scroller.dataset["ruler"];
    } else {
      dom.scroller.style.setProperty("--mark-ruler", ruler);
      dom.scroller.dataset["ruler"] = "true";
    }
    dom.scroller.setAttribute("role", "grid");
    dom.scroller.setAttribute("tabindex", "-1");
    dom.scroller.setAttribute("aria-rowcount", String(total));
    dom.scroller.setAttribute(
      "aria-busy",
      slot.state.state === "loading" ? "true" : "false",
    );
    // Waiting (#323). Until now there was only `aria-busy`, and NO rule that
    // painted it: against a slow SFTP the window gave no sign.
    //
    // The node is STABLE and hidden with an attribute, not created on every
    // paint. The threshold is an `animation-delay`, and an animation that
    // starts from zero every time its node is born never reaches 250ms:
    // `paint()` repaints every slot on EVERY update, so with a large listing
    // arriving in pages — or with the other pane working — the notice would
    // never have shown up, which is exactly the case it exists for.
    //
    // The VERB comes from the host, from the closed vocabulary it shares
    // with the terminal: "connecting…" and "loading…" are not the same, and
    // the case that uncovered #323 was the first.
    //
    // NO "Esc cancels". The window has no way to abort a listing in flight —
    // nothing clears `en_vuelo`/`drenando` from a key — and the repo has
    // that doctrine written three times in the `.ftl`s: never a fake
    // affordance. The day the abort exists, with its clean-cancellation
    // test, the sentence comes back.
    const loading = slot.state.state === "loading";
    dom.busy.hidden = !loading;
    if (slot.state.state === "loading") {
      const target = slot.state.target_display ?? "";
      const verb = this.t(slot.state.verb_key ?? "busy-listing");
      if (target === "") {
        // A refresh: it is not going anywhere, so no place is made up.
        dom.busy.replaceChildren(verb);
      } else {
        const going = document.createElement("span");
        going.className = "slot-busy-target";
        going.textContent = target;
        dom.busy.replaceChildren(verb, " ", going);
        if (slot.state.target_hostile === true) {
          // The path being navigated to paints different from what it is. It
          // is what the reader is looking at while they wait, so it is
          // marked.
          dom.busy.append(badge(this.t("hostile-name")));
        }
      }
    }

    if (slot.state.state === "error") {
      // With a RETRY, and not just the sentence. An error slot is what is
      // left when the listing could not be built, and the common case on
      // reopening is a remote connection asking for its password: with
      // nothing to click, the only way out was navigating somewhere else so
      // as to be able to come back.
      //
      // Retrying is the GESTURE that opens the question. The host does not
      // open it just by starting up on purpose — restoring a session is not
      // asking to connect — so this button is the missing half.
      const box = errorNode(this.t(slot.state.reason_key), slot.state.detail);
      const retry = document.createElement("button");
      retry.type = "button";
      retry.className = "slot-retry";
      retry.textContent = this.t("slot-retry");
      retry.addEventListener("click", () => {
        this.send({ action: "refresh_slot", slot_id: slot.slot_id });
      });
      box.append(retry);
      dom.canvas.replaceChildren(box);
      dom.rows.clear();
      return;
    }

    // The row stripes (bridge 80): turned on by the CONTAINER, not the row.
    // Every row always carries its parity, so a row recycled by scrolling
    // does not drag along the band from the spot it used to occupy.
    dom.canvas.dataset["stripes"] = String(this.ultimaVista?.row_stripes ?? false);
    const wanted = new Set<number>();
    // The icon column is opened by the HOST for the whole listing (bridge
    // 62): with or without an icon, every row carries the cell. Deducing it
    // here from the visible rows would close it when scrolling to a page
    // with no icons, and it would shift every name.
    for (const [i, row] of slot.rows.entries()) {
      const index = slot.first_visible + i;
      wanted.add(row.key);
      const el = dom.rows.get(row.key) ?? newRow(dom, slot.slot_id, row.key);
      updateRow(el, row, index, cell.h, slot.icon_column);
    }
    for (const [key, el] of dom.rows) {
      if (!wanted.has(key)) {
        el.remove();
        dom.rows.delete(key);
      }
    }
    if (slot.cursor !== null) {
      const el = dom.rows.get(slot.cursor);
      if (el !== undefined) {
        dom.scroller.setAttribute("aria-activedescendant", el.id);
      }
    }
    if (total === 0) {
      dom.canvas.replaceChildren(emptyNode(this.t("listing-empty")));
      dom.rows.clear();
    }
  }

  /** In `render/menus.ts`. */
  readonly paintHeader = menus.paintHeader;

  /** In `render/ai.ts`. */
  readonly paintAiRename = ai.paintAiRename;

  /** In `render/organize.ts`. */
  readonly paintOrganize = organize.paintOrganize;

  /** In `render/dialogs.ts`. */
  readonly campoDeDialogo = dialogs.campoDeDialogo;

  /** In `render/dialogs.ts`. */
  readonly paintDialogs = dialogs.paintDialogs;
}
