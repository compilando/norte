// El pintado, y SOLO el pintado.
//
// Lo que entra es lo que el host proyectó; lo que sale son nodos del DOM y
// acciones semánticas. Aquí no se ordena, no se formatea un tamaño, no se
// decide si un comando está disponible y no se compone una ruta: todo eso ya
// vino resuelto (ADR 0066, decisión D14).
//
// Dos reglas de la frontera se cumplen en cada línea de este fichero:
// el texto se pone con `textContent` —nunca HTML, porque un nombre de fichero
// es un dato— y el estilo dinámico se pone por CSSOM, porque la CSP bloquea
// el atributo `style`.

import type {
  BrowserSlotView,
  HostCatalog,
  SlotView,
  UiAction,
  ViewSnapshot,
  ProcessesSlotView,
} from "./types";
import {
  revelar,
  nota,
  OVERSCAN,
  place,
  newRow,
  updateRow,
  badge,
  emptyNode,
  errorNode,
  statusNodes,
  taskNode,
} from "./render/dom";
import type { Send, SlotDom } from "./render/dom";
import * as dialogs from "./render/dialogs";
import * as sync from "./render/sync";
import * as ai from "./render/ai";
import * as extensions from "./render/extensions";
import * as settings from "./render/settings";
import * as viewer from "./render/viewer";
import * as help from "./render/help";
import * as log from "./render/log";
import * as search from "./render/search";
import * as menus from "./render/menus";
import * as places from "./render/places";

/*
 * Los miembros son públicos a efectos de TypeScript porque los pintores de
 * `render/*` los alcanzan a través de `this: Screen`; fuera de `src/render*`
 * nadie debe tocarlos. La API de la ventana es la que usa `main.ts`.
 */
export class Screen {
  readonly slots = new Map<number, SlotDom>();
  placementsKey = "";
  /** El diálogo cuyo campo de texto ya se sembró. */
  dialogoPintado: number | null = null;
  /// El campo de texto vivo del diálogo de arriba, para REUSARLO.
  dialogoInput: HTMLInputElement | null = null;
  /** Los mandos del registro, conservados entre repintados (#326). */
  logControles: HTMLElement | null = null;
  /** El hueco al que pertenecen: otro hueco, otros mandos. */
  logPintado: number | null = null;
  /** Lo ultimo que se le dijo al host sobre cuantas filas caben. */
  logFilas: number | null = null;
  pendingLogRows: number | null = null;
  /// La página de ayuda que se pintó, para conservar su scroll.
  helpPintada: string | null = null;
  /// El `blob:` de la imagen que se está enseñando, para REVOCARLO.
  ///
  /// Un object URL sin revocar es un búfer retenido mientras viva el
  /// documento. La revocación va en el mismo sitio que el cierre, no en un
  /// `finally` que un refactor futuro pueda soltar (ADR 0069).
  imagenUrl: string | null = null;
  /// Qué imagen se pidió, para no pedir dos veces la misma ni pintar la
  /// anterior sobre el visor de ahora.
  imagenDe: string | null = null;
  /** Las líneas de visor que ya se declararon. */
  viewerRows = 0;
  /** Las columnas del cuerpo del visor que el host ya conoce. */
  viewerCols = 0;
  /** La ayuda está abierta con el CUERPO enfocado. */
  helpBodyFocused = false;
  pendingRange = new Map<number, number>();
  /** La altura que la barra de menús está reservando, ya en CSS. */
  menuBarHeight: string | null = null;
  /** Lo mismo para la barra de paneles (#324). */
  panelBarHeight: string | null = null;
  /** La última foto pintada: lo que se repinta cuando cambia algo local. */
  ultimaVista: ViewSnapshot | null = null;
  /** Aviso local de una orden rechazada en la frontera (`rejected`). */
  rechazo: string | null = null;
  /** La reserva cambió: el host tiene que oír el alto nuevo. */
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
    readonly catalog: HostCatalog,
    readonly send: Send,
    /**
     * Trae los bytes de la imagen abierta. Sin ruta: el renderer no nombra
     * ficheros, se le sirve la que el host decidió abrir (ADR 0069).
     */
    readonly fetchImage: () => Promise<ArrayBuffer> = () =>
      Promise.resolve(new ArrayBuffer(0)),
  ) {}

  /**
   * La ayuda está abierta y su cuerpo tiene el foco, así que las teclas de
   * página son del scroll del DOM y no del host.
   */
  helpBodyScrolls(): boolean {
    return this.helpBodyFocused;
  }

  /**
   * ¿Cambió lo que la barra de menús reserva desde la última vez que se
   * preguntó? Consulta que CONSUME: quien la hace vuelve a declarar el alto.
   */
  takeViewportDirty(): boolean {
    const sucio = this.viewportSucio;
    this.viewportSucio = false;
    return sucio;
  }

  /** Texto de una clave Fluent, traducido EN RUST. La clave, si no está. */
  t(key: string): string {
    return this.catalog.strings[key] ?? key;
  }

  /**
   * Una orden que el host rechazó en la frontera (no deserializa, contrato
   * roto): nunca llegó a su buzón, así que ningún estado del host la puede
   * contar. Se pinta aquí, en la barra de estado, hasta la primera orden
   * aceptada. El detalle del error va a la consola: es texto del otro
   * extremo, y la barra dice qué orden y que no la entendió.
   */
  rejected(action: UiAction, error: unknown): void {
    console.error("el host no aceptó la acción:", action.action, error);
    this.rechazo = `${this.t("gui-msg-action-rejected")}: ${action.action}`;
    this.repaintStatus();
  }

  /** Una orden aceptada retira el aviso; dice si había uno. */
  accepted(): boolean {
    if (this.rechazo === null) {
      return false;
    }
    this.rechazo = null;
    this.repaintStatus();
    return true;
  }

  /** Vuelve a pintar el hueco de estado con la última foto, si la hay. */
  repaintStatus(): void {
    if (this.ultimaVista !== null) {
      this.paint(this.ultimaVista);
    }
  }

  /** El tamaño de una celda de layout, en píxeles reales. */
  cell(): { w: number; h: number } {
    const cs = getComputedStyle(document.documentElement);
    return {
      w: Number.parseFloat(cs.getPropertyValue("--cell-w")) || 8,
      h: Number.parseFloat(cs.getPropertyValue("--cell-h")) || 20,
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
    // Que el rol EXISTA y que se MARQUE son dos preguntas. La segunda llega
    // CALCULADA del host (`layout.mark_target`): la decide el crate
    // compartido, y contarla aquí era repetir en TypeScript un número que ya
    // vive en Rust — la misma decisión en dos sitios.
    const marcarDestino = view.layout.mark_target ?? false;
    for (const p of view.layout.placements) {
      const dom = this.slots.get(p.slot_id);
      const slot = view.slots.find((s) => s.slot_id === p.slot_id);
      if (dom === undefined || slot === undefined) {
        continue;
      }
      const rol = p.role === "target" && !marcarDestino ? null : p.role;
      dom.root.dataset["role"] = rol ?? "";
      dom.root.setAttribute("aria-current", p.role === "active" ? "true" : "false");
      this.paintTabs(
        dom,
        view.layout.tabs.find((g) => g.slot_id === p.slot_id),
      );
      this.paintSlot(dom, slot, view, cell);
    }
    this.paintMenu(view.menu);
    this.paintPanelBar(view.panel_bar);
    this.paintPalette(view.palette);
    this.paintWhichKey(view.whichkey);
    this.paintHelp(view.help);
    this.paintSettings(view.settings);
    this.paintExtensions(view.extensions);
    this.paintAgents(view.agents);
    this.paintPluginOutput(view.plugin_output);
    this.paintProgramOutput(view.program_output);
    this.paintTheme(view.theme);
    this.paintPicker(view.picker);
    this.paintProfiles(view.profiles);
    this.paintLayouts(view.layouts);
    this.paintColumns(view.columns);
    this.paintSearch(view.search);
    this.paintCompare(view.compare);
    this.paintSync(view.sync);
    this.paintViewer(view.viewer);
    this.paintAiRename(view.ai_rename);
    this.paintDialogs(view.dialogs);
  }

  /** En `render/menus.ts`. */
  readonly paintPanelBar = menus.paintPanelBar;

  /** En `render/menus.ts`. */
  readonly paintMenu = menus.paintMenu;

  /** En `render/menus.ts`. */
  readonly paintPalette = menus.paintPalette;

  /** En `render/menus.ts`. */
  readonly paintWhichKey = menus.paintWhichKey;

  /** En `render/help.ts`. */
  readonly paintHelp = help.paintHelp;

  /** En `render/help.ts`. */
  readonly helpSidebar = help.helpSidebar;

  /** En `render/help.ts`. */
  readonly helpBody = help.helpBody;

  /** En `render/help.ts`. */
  readonly helpBlock = help.helpBlock;

  /** En `render/help.ts`. */
  readonly helpSpan = help.helpSpan;

  /** En `render/settings.ts`. */
  readonly paintSettings = settings.paintSettings;

  /** En `render/extensions.ts`. */
  readonly paintExtensions = extensions.paintExtensions;

  /** En `render/extensions.ts`. */
  readonly extensionDetail = extensions.extensionDetail;

  /** En `render/extensions.ts`. */
  readonly extensionPaneHead = extensions.extensionPaneHead;

  /** En `render/extensions.ts`. */
  readonly extensionCommands = extensions.extensionCommands;

  /** En `render/extensions.ts`. */
  readonly paintAgents = extensions.paintAgents;

  /** En `render/extensions.ts`. */
  readonly paintPluginOutput = extensions.paintPluginOutput;

  /** En `render/extensions.ts`. */
  readonly paintProgramOutput = extensions.paintProgramOutput;

  /** En `render/settings.ts`. */
  readonly paintTheme = settings.paintTheme;

  /** En `render/sync.ts`. */
  readonly paintSync = sync.paintSync;

  /** En `render/sync.ts`. */
  readonly syncStep = sync.syncStep;

  /** En `render/sync.ts`. */
  readonly paintCompare = sync.paintCompare;

  /** En `render/sync.ts`. */
  readonly compareFace = sync.compareFace;

  /** En `render/search.ts`. */
  readonly paintSearch = search.paintSearch;

  /** En `render/settings.ts`. */
  readonly paintColumns = settings.paintColumns;

  /** En `render/settings.ts`. */
  readonly paintProfiles = settings.paintProfiles;

  /** En `render/settings.ts`. */
  readonly paintLayouts = settings.paintLayouts;

  /** En `render/settings.ts`. */
  readonly paintPicker = settings.paintPicker;

  /** En `render/settings.ts`. */
  readonly settingsRow = settings.settingsRow;

  /** En `render/viewer.ts`. */
  readonly paintViewer = viewer.paintViewer;

  /** En `render/viewer.ts`. */
  readonly soltarImagen = viewer.soltarImagen;

  /** En `render/viewer.ts`. */
  readonly pintarImagen = viewer.pintarImagen;

  /** El `<img>` con su tamaño declarado, para que no salte al cargar. */
  nodoImagen(
    url: string,
    img: { format: string; width: number; height: number },
  ): HTMLElement {
    const el = document.createElement("img");
    el.className = "viewer-image";
    el.src = url;
    // El tamaño DECLARADO, que el host ya comparó con el presupuesto: sin
    // él la caja salta cuando la imagen carga.
    el.width = img.width;
    el.height = img.height;
    el.alt = img.format;
    return el;
  }

  /**
   * Los TIRADORES de los bordes entre huecos vecinos.
   *
   * Se rehacen con el reparto, no con cada frame: mientras el reparto no
   * cambie, el borde está donde estaba. Y salen del MISMO `placements` que
   * coloca los huecos — dos cálculos de dónde está un borde son un borde que
   * se agarra en un sitio y se mueve desde otro.
   *
   * Lo que se manda es la posición del PUNTERO en celdas, no un tamaño: qué
   * pareja se reparte y cuánto le toca a cada uno lo decide el host, que es
   * quien tiene el reparto y los mínimos (ADR 0069).
   */
  buildHandles(view: ViewSnapshot, cell: { w: number; h: number }): void {
    /** Lo que se deja agarrar a cada lado del borde, en píxeles. */
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
          // La captura es lo que hace que el arrastre siga al puntero cuando
          // se sale del tirador — que es lo que pasa siempre, porque el
          // tirador mide seis píxeles.
          el.setPointerCapture(e.pointerId);
          e.preventDefault();
        });
        el.addEventListener("pointermove", (e: PointerEvent) => {
          if (!el.hasPointerCapture(e.pointerId)) {
            return;
          }
          // Contra el ORIGEN del tablero, no contra la ventana. `#screen`
          // baja lo que midan la barra de menú y la de paneles
          // (`margin-top`), así que un `clientY` crudo le daba al host una
          // fila de más por cada fila de cromo: el borde saltaba al empezar a
          // arrastrarlo. En el eje X coincidían por casualidad —el tablero
          // empieza en la columna 0— y por eso solo se notaba en los bordes
          // horizontales.
          const origen = this.root.getBoundingClientRect();
          const cells = vertical
            ? Math.round((e.clientX - origen.left) / cell.w)
            : Math.round((e.clientY - origen.top) / cell.h);
          this.send({ action: "resize_slot", slot_id: slot, cells });
        });
        this.root.append(el);
      }
    }
  }

  rebuild(view: ViewSnapshot, cell: { w: number; h: number }): void {
    this.root.replaceChildren();
    this.slots.clear();
    for (const p of view.layout.placements) {
      const el = document.createElement("section");
      el.className = "slot";
      el.setAttribute("role", "group");
      // Quién es este hueco, en el DOM. Sin esto la única forma de dar con él
      // era su POSICIÓN entre hermanos, que es una correspondencia implícita
      // entre el orden de `placements` y el del DOM.
      el.dataset["slotId"] = String(p.slot_id);
      place(el, p, cell);
      // La barra de PESTAÑAS va encima del título: es lo que dice qué hay
      // detrás de lo que se está pintando.
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
      // El pie bajo el listado: cuentas, marcado y espacio libre, ya
      // redactado en Rust. Vacío = `[ui] pane_footer` apagado, y no ocupa.
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
    // Los tiradores, DESPUÉS de los huecos y por eso al final.
    //
    // Esta hoja de estilos no usa `z-index` en ninguna parte a propósito —lo
    // dice ella misma en el velo del menú—, así que el apilado lo decide el
    // ORDEN del documento. Se construían primero, y como un hueco también es
    // `absolute`, cada panel los tapaba: el `pointerdown` no les llegaba
    // nunca y no se podía redimensionar con el ratón. Un tirador
    // transparente de seis píxeles debajo de un panel no es un tirador.
    this.buildHandles(view, cell);
  }

  wire(slotId: number, dom: SlotDom): void {
    // Pulsar CUALQUIER parte de un panel lo enfoca: la cabecera, el hueco bajo
    // la última fila, el borde. Estaba solo en las filas, así que un panel sin
    // ninguna —o el clic en su título— se pintaba con el borde de otro.
    //
    // En CAPTURA para que el foco viaje antes que lo que haga el clic concreto
    // (ordenar, seleccionar): es el orden que el host ya ve desde el teclado.
    dom.root.addEventListener(
      "mousedown",
      () => {
        if (dom.root.dataset["role"] !== "active") {
          this.send({ action: "focus_slot", slot_id: slotId });
        }
      },
      true,
    );
    dom.header.addEventListener("mousedown", (e) => {
      const target = e.target;
      if (!(target instanceof Element)) {
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
      // Qué hace un click en la MISMA columna —invertir— lo decide el host.
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
      // El foco ya lo mandó el listener de captura del panel entero.
      if (e.shiftKey) {
        // El rango lo marca el HOST: qué entra y qué no —`..`, por ejemplo—
        // es una regla de selección compartida, no una del renderer.
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
    });
    dom.scroller.addEventListener("dblclick", (e) => {
      const target = e.target;
      if (!(target instanceof Element)) {
        return;
      }
      const rowEl = target.closest(".row");
      if (!(rowEl instanceof HTMLElement)) {
        return;
      }
      const rowKey = Number(rowEl.dataset["key"]);
      if (!Number.isNaN(rowKey)) {
        this.send({
          action: "activate",
          slot_id: slotId,
          key: rowKey,
          generation: dom.generation,
        });
      }
    });
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

  /** El scroll lo pinta el renderer; lo único que cruza es qué filas hacen falta. */
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

  /** En `render/menus.ts`. */
  readonly paintTabs = menus.paintTabs;

  paintSlot(
    dom: SlotDom,
    slot: SlotView,
    view: ViewSnapshot,
    cell: { w: number; h: number },
  ): void {
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
    if (slot.kind === "unsupported") {
      // El nombre del kind sale del fichero de disposición del usuario: si el
      // host lo enmascaró, se dice — el mismo criterio que el resto.
      this.paintAux(dom, slot.kind_name, view, slot.kind_name_hostile);
      return;
    }
    if (slot.kind === "browser") {
      this.paintBrowser(dom, slot, cell);
      return;
    }
    // Un `kind` que este renderer no conoce se pinta como lo que ES: un hueco
    // que no sabe pintar. Antes caía al listado por defecto —TypeScript ya
    // había estrechado el tipo, así que compilaba— y un hueco nuevo del host
    // se habría pintado como un listado con `rows` a `undefined`, o sea una
    // tabla vacía indistinguible de un directorio vacío.
    this.paintAux(dom, (slot as { kind: string }).kind, view);
  }

  /** En `render/places.ts`. */
  readonly paintTree = places.paintTree;

  /** En `render/places.ts`. */
  readonly paintPlaces = places.paintPlaces;

  /** En `render/viewer.ts`. */
  readonly paintMetadata = viewer.paintMetadata;

  /** En `render/viewer.ts`. */
  readonly paintMetadataBody = viewer.paintMetadataBody;

  /** En `render/viewer.ts`. */
  readonly paintPreview = viewer.paintPreview;

  /**
   * El panel de procesos: las MISMAS tareas de la franja, con su cursor.
   *
   * No hay una segunda lista: dos listas de tareas se separan, y la que se ve
   * deja de ser la que se cancela.
   */
  paintProcesses(dom: SlotDom, slot: ProcessesSlotView, view: ViewSnapshot): void {
    dom.root.setAttribute("aria-label", this.t("processes-title"));
    dom.title.textContent = this.t("processes-title");
    dom.scroller.className = "processes";
    if (view.tasks.length === 0) {
      dom.scroller.replaceChildren(nota(this.t("processes-empty")));
      return;
    }
    const lista = document.createElement("ul");
    lista.className = "processes-rows";
    lista.setAttribute("role", "listbox");
    for (const [i, t] of view.tasks.entries()) {
      const fila = document.createElement("li");
      fila.className = "processes-row";
      fila.id = `process-row-${String(i)}`;
      fila.setAttribute("role", "option");
      fila.setAttribute("aria-selected", String(slot.cursor === i));
      fila.append(taskNode(t, (k) => this.t(k)));
      lista.append(fila);
    }
    if (slot.cursor !== null) {
      lista.setAttribute("aria-activedescendant", `process-row-${String(slot.cursor)}`);
      revelar(lista.querySelector(`#process-row-${String(slot.cursor)}`) ?? undefined);
    }
    dom.scroller.replaceChildren(lista);
  }

  /** En `render/log.ts`. */
  readonly paintLog = log.paintLog;

  /** En `render/log.ts`. */
  readonly selectorDeFuente = log.selectorDeFuente;

  /** En `render/log.ts`. */
  readonly crearControlesDeRegistro = log.crearControlesDeRegistro;

  /** En `render/log.ts`. */
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
      dom.scroller.replaceChildren(
        ...statusNodes(
          view.status,
          view.connection.state,
          (k) => this.t(k),
          this.rechazo,
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
    // La ruta en su propio nodo, y no como texto suelto de la cabecera: es
    // lo ÚNICO que se puede recortar cuando no cabe. Con la ruta como texto
    // directo, una larga empujaba fuera de la vista todo lo que viniera
    // detrás —el △ de hostil y el aviso de entradas omitidas— y desaparecían
    // en silencio, que es justo lo contrario de lo que existen para hacer.
    const ruta = document.createElement("span");
    ruta.className = "title-path";
    ruta.textContent = slot.path_display;
    dom.title.replaceChildren(ruta);
    if (slot.path_hostile) {
      ruta.append(badge(this.t("hostile-name")));
    }
    // Todo lo que dice que el listado NO es lo que parece, ya redactado en
    // Rust. Va en la CABECERA y no al final de la lista: lo que falta no
    // está, así que no hay ninguna fila donde el lector pueda tropezarse con
    // ello.
    //
    // El ORDEN es la decisión, y es el mismo que la barra del terminal: los
    // AVISOS —el listado incompleto, la reinterpretación de nombres, las
    // marcas que se cayeron— van antes que el CONTADOR de lo marcado. El
    // sitio se acaba, y un aviso recortado deja de avisar mientras que un
    // contador recortado solo deja de contar.
    //
    // `role="status"` solo en los AVISOS. Lo marcado y el relleno son
    // contadores de algo que el lector acaba de hacer o que está pasando a la
    // vista: anunciarlos por voz en cada tecla convierte la región viva en
    // ruido, y entonces el aviso que sí importa llega dentro del ruido.
    const notas: [string, string, boolean][] = [
      ["slot-filling", slot.filling_note ?? "", false],
      ["slot-skipped", slot.skipped_note, true],
      ["slot-names", slot.names_note ?? "", true],
      ["slot-pruned", slot.pruned_note ?? "", true],
      ["slot-hidden", slot.hidden_note, true],
      ["slot-marked", slot.marked_note ?? "", false],
    ];
    for (const [clase, texto, esAviso] of notas) {
      if (texto === "") {
        continue;
      }
      const nota = document.createElement("span");
      nota.className = clase;
      if (esAviso) {
        nota.setAttribute("role", "status");
      }
      nota.textContent = texto;
      dom.title.append(nota);
    }
    dom.root.setAttribute("aria-label", slot.path_display);
    dom.generation = slot.generation;
    // El pie (puente 63): vacío = apagado, y entonces no ocupa fila.
    const pie = slot.footer ?? "";
    dom.footer.textContent = pie;
    dom.footer.hidden = pie === "";

    this.paintHeader(dom, slot);

    const total = slot.total_rows ?? slot.rows.length;
    dom.canvas.style.setProperty("height", `${total * cell.h}px`);
    dom.scroller.setAttribute("role", "grid");
    dom.scroller.setAttribute("tabindex", "-1");
    dom.scroller.setAttribute("aria-rowcount", String(total));
    dom.scroller.setAttribute(
      "aria-busy",
      slot.state.state === "loading" ? "true" : "false",
    );
    // Esperando (#323). Hasta aquí solo estaba el `aria-busy`, y NINGUNA
    // regla que lo pintara: contra un SFTP lento la ventana no daba señal.
    //
    // El nodo es ESTABLE y se esconde con un atributo, no se crea en cada
    // pintada. El umbral es un `animation-delay`, y una animación que empieza
    // de cero cada vez que su nodo nace nunca llega a los 250 ms: `paint()`
    // repinta todos los huecos en CADA actualización, así que con un listado
    // grande llegando por páginas —o con el otro panel trabajando— el aviso
    // no habría aparecido jamás, que es justo el caso para el que existe.
    //
    // El VERBO viene del host, del vocabulario cerrado que comparte con el
    // terminal: «conectando…» y «cargando…» no son lo mismo, y el caso que
    // destapó #323 era el primero.
    //
    // SIN «Esc cancela». La ventana no tiene camino para abortar un listado
    // en vuelo —nada limpia `en_vuelo`/`drenando` desde una tecla—, y el repo
    // tiene esa doctrina escrita tres veces en los `.ftl`: jamás una
    // affordance falsa. El día que exista el aborto, con su test de
    // cancelación limpia, la frase vuelve.
    const cargando = slot.state.state === "loading";
    dom.busy.hidden = !cargando;
    if (slot.state.state === "loading") {
      const destino = slot.state.target_display ?? "";
      const verbo = this.t(slot.state.verb_key ?? "busy-listing");
      if (destino === "") {
        // Un refresco: no va a ninguna parte, así que no se inventa un sitio.
        dom.busy.replaceChildren(verbo);
      } else {
        const yendo = document.createElement("span");
        yendo.className = "slot-busy-target";
        yendo.textContent = destino;
        dom.busy.replaceChildren(verbo, " ", yendo);
        if (slot.state.target_hostile === true) {
          // La ruta a la que se va se pinta distinta de lo que es. Es la que
          // el lector está mirando mientras espera, así que va marcada.
          dom.busy.append(badge(this.t("hostile-name")));
        }
      }
    }
    dom.title.append(dom.busy);

    if (slot.state.state === "error") {
      // Con un REINTENTO, y no solo la frase. Un hueco en error es lo que
      // queda cuando el listado no se pudo hacer, y el caso corriente al
      // reabrir es una conexión remota que pide su contraseña: sin nada que
      // pulsar, la única salida era navegar a otro sitio para poder volver.
      //
      // Reintentar es el GESTO que abre la pregunta. El host no la abre solo
      // al arrancar a propósito —restaurar una sesión no es pedir
      // conectarse—, así que este botón es la mitad que faltaba.
      const caja = errorNode(this.t(slot.state.reason_key), slot.state.detail);
      const reintentar = document.createElement("button");
      reintentar.type = "button";
      reintentar.className = "slot-retry";
      reintentar.textContent = this.t("slot-retry");
      reintentar.addEventListener("click", () => {
        this.send({ action: "refresh_slot", slot_id: slot.slot_id });
      });
      caja.append(reintentar);
      dom.canvas.replaceChildren(caja);
      dom.rows.clear();
      return;
    }

    const wanted = new Set<number>();
    // La columna de iconos la abre el HOST para el listado entero (puente
    // 62): con o sin icono, todas las filas llevan la celda. Deducirlo aquí
    // de las filas visibles la cerraría al desplazarse a una página sin
    // iconos, y correría todos los nombres.
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

  /** En `render/menus.ts`. */
  readonly paintHeader = menus.paintHeader;

  /** En `render/ai.ts`. */
  readonly paintAiRename = ai.paintAiRename;

  /** En `render/dialogs.ts`. */
  readonly campoDeDialogo = dialogs.campoDeDialogo;

  /** En `render/dialogs.ts`. */
  readonly paintDialogs = dialogs.paintDialogs;
}
