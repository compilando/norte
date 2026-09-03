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
  AiRenameView,
  BrowserSlotView,
  CompareFaceView,
  CompareRowView,
  CompareView,
  SyncStepView,
  SyncView,
  DialogLine,
  DialogView,
  HostCatalog,
  RowView,
  SlotPlacement,
  SlotView,
  StatusView,
  TaskView,
  UiAction,
  ViewSnapshot,
  HelpBlockView,
  HelpSpanView,
  HelpView,
  MenuView,
  PanelBarView,
  PaletteView,
  ExtensionsView,
  TabGroupView,
  AgentsView,
  ExtensionCommandView,
  ExtensionOutputView,
  ColumnsPickerView,
  LayoutPickerView,
  ProfilePickerView,
  MetadataSlotView,
  PreviewSlotView,
  ProgramOutputView,
  SearchView,
  PlacesSlotView,
  TreeSlotView,
  PickerView,
  LogSlotView,
  ProcessesSlotView,
  SettingsView,
  ThemeView,
  ViewerView,
  WhichKeyView,
} from "./types";

/**
 * Desplaza lo justo para que `el` se vea, si el entorno sabe hacerlo.
 *
 * `scrollIntoView` no existe en jsdom, donde corren los tests del renderer:
 * sin la guarda, comprobar el pintado de una lista tumbaba el test en una
 * llamada que no es del pintado.
 */
function revelar(el: Element | undefined): void {
  if (el instanceof HTMLElement && typeof el.scrollIntoView === "function") {
    el.scrollIntoView({ block: "nearest" });
  }
}

/** Un párrafo con una frase que el host ya escribió. */
function nota(texto: string): HTMLElement {
  const p = document.createElement("p");
  p.className = "slot-note";
  p.textContent = texto;
  return p;
}

/**
 * El cuerpo de un visor: las líneas, o los fragmentos con estilo si un
 * plugin los puso. Lo comparten el visor a pantalla completa y el acoplado
 * (#291): es el mismo visor en otro sitio, y dos cuerpos divergen.
 *
 * Siempre `textContent`: el texto lo escribió un plugin. El rol va en
 * `data-role`, que la hoja de estilos mapea a las variables del tema, y el
 * color propio solo cuando no hay rol — el tema del lector manda sobre la
 * paleta fija del plugin. El fondo no tiene rol que lo mande: un medio
 * bloque sin fondo es media imagen (puente 50).
 */
function viewerBody(viewer: ViewerView): HTMLElement {
  const body = document.createElement("pre");
  body.className = viewer.hex ? "viewer-body hexview" : "viewer-body";
  if (viewer.styled.length === 0) {
    body.textContent = viewer.lines.join("\n");
    return body;
  }
  for (const linea of viewer.styled) {
    const fila = document.createElement("div");
    fila.className = "viewer-line";
    for (const s of linea) {
      const el = document.createElement("span");
      el.className = "viewer-span";
      el.textContent = s.text;
      if (s.role !== null) {
        el.dataset["role"] = s.role;
      } else if (s.fg !== null) {
        el.style.color = s.fg;
      }
      if (s.bg !== null) {
        el.style.backgroundColor = s.bg;
      }
      fila.append(el);
    }
    body.append(fila);
  }
  return body;
}

/** Filas de más que se piden por arriba y por abajo del hueco visible. */
const OVERSCAN = 8;

type Send = (action: UiAction) => void;

interface SlotDom {
  root: HTMLElement;
  /** La barra de pestañas, vacía cuando el hueco no está en un grupo. */
  tabs: HTMLElement;
  title: HTMLElement;
  header: HTMLElement;
  scroller: HTMLElement;
  canvas: HTMLElement;
  rows: Map<number, HTMLElement>;
  lastRange: { first: number; count: number } | null;
  /**
   * La generación que se PINTÓ. Toda acción de fila la lleva: sin ella la
   * clave es un índice, y un índice de la pantalla anterior nombra otro
   * fichero. El host la compara y responde `stale` si no coincide.
   */
  generation: number;
}

export class Screen {
  private readonly slots = new Map<number, SlotDom>();
  private placementsKey = "";
  /** El diálogo cuyo campo de texto ya se sembró. */
  private dialogoPintado: number | null = null;
  /// El campo de texto vivo del diálogo de arriba, para REUSARLO.
  private dialogoInput: HTMLInputElement | null = null;
  /** Los mandos del registro, conservados entre repintados (#326). */
  private logControles: HTMLElement | null = null;
  /** El hueco al que pertenecen: otro hueco, otros mandos. */
  private logPintado: number | null = null;
  /** Lo ultimo que se le dijo al host sobre cuantas filas caben. */
  private logFilas: number | null = null;
  private pendingLogRows: number | null = null;
  /// La página de ayuda que se pintó, para conservar su scroll.
  private helpPintada: string | null = null;
  /// El `blob:` de la imagen que se está enseñando, para REVOCARLO.
  ///
  /// Un object URL sin revocar es un búfer retenido mientras viva el
  /// documento. La revocación va en el mismo sitio que el cierre, no en un
  /// `finally` que un refactor futuro pueda soltar (ADR 0069).
  private imagenUrl: string | null = null;
  /// Qué imagen se pidió, para no pedir dos veces la misma ni pintar la
  /// anterior sobre el visor de ahora.
  private imagenDe: string | null = null;
  /** Las líneas de visor que ya se declararon. */
  private viewerRows = 0;
  /** La ayuda está abierta con el CUERPO enfocado. */
  private helpBodyFocused = false;
  private pendingRange = new Map<number, number>();
  /** La altura que la barra de menús está reservando, ya en CSS. */
  private menuBarHeight: string | null = null;
  /** Lo mismo para la barra de paneles (#324). */
  private panelBarHeight: string | null = null;
  /** La reserva cambió: el host tiene que oír el alto nuevo. */
  private viewportSucio = false;

  constructor(
    private readonly root: HTMLElement,
    private readonly menuRoot: HTMLElement,
    private readonly panelBarRoot: HTMLElement,
    private readonly paletteRoot: HTMLElement,
    private readonly whichKeyRoot: HTMLElement,
    private readonly helpRoot: HTMLElement,
    private readonly settingsRoot: HTMLElement,
    private readonly extensionsRoot: HTMLElement,
    private readonly themeRoot: HTMLElement,
    private readonly pickerRoot: HTMLElement,
    private readonly profilesRoot: HTMLElement,
    private readonly layoutsRoot: HTMLElement,
    private readonly columnsRoot: HTMLElement,
    private readonly searchRoot: HTMLElement,
    private readonly compareRoot: HTMLElement,
    private readonly syncRoot: HTMLElement,
    private readonly agentsRoot: HTMLElement,
    private readonly pluginOutputRoot: HTMLElement,
    private readonly programOutputRoot: HTMLElement,
    private readonly viewerRoot: HTMLElement,
    private readonly dialogsRoot: HTMLElement,
    private readonly aiRenameRoot: HTMLElement,
    private readonly catalog: HostCatalog,
    private readonly send: Send,
    /**
     * Trae los bytes de la imagen abierta. Sin ruta: el renderer no nombra
     * ficheros, se le sirve la que el host decidió abrir (ADR 0069).
     */
    private readonly fetchImage: () => Promise<ArrayBuffer> = () =>
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

  /** El tamaño de una celda de layout, en píxeles reales. */
  cell(): { w: number; h: number } {
    const cs = getComputedStyle(document.documentElement);
    return {
      w: Number.parseFloat(cs.getPropertyValue("--cell-w")) || 8,
      h: Number.parseFloat(cs.getPropertyValue("--cell-h")) || 20,
    };
  }

  paint(view: ViewSnapshot): void {
    const cell = this.cell();
    const key = view.layout.placements
      .map((p) => `${p.slot_id}:${p.x},${p.y},${p.width},${p.height}`)
      .join("|");
    if (key !== this.placementsKey) {
      this.rebuild(view, cell);
      this.placementsKey = key;
    }
    for (const p of view.layout.placements) {
      const dom = this.slots.get(p.slot_id);
      const slot = view.slots.find((s) => s.slot_id === p.slot_id);
      if (dom === undefined || slot === undefined) {
        continue;
      }
      dom.root.dataset["role"] = p.role ?? "";
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

  /**
   * La barra de paneles (#324): un botón por panel que se abre y se cierra,
   * con su estado y su marca de novedad.
   *
   * Los botones vienen DECIDIDOS del host —qué hay, en qué orden, con qué
   * letra— porque la decisión es de `norte-frontend` y la TUI pinta la
   * misma (ADR 0077). Aquí solo se pintan y se pulsan; un click vuelve como
   * el índice del botón, nunca como un comando (ADR 0069).
   *
   * Reserva su fila igual que la barra de menús: el host reparte sobre el
   * alto que este renderer declara, y una fila flotante taparía la primera
   * del listado.
   */
  private paintPanelBar(bar: PanelBarView): void {
    const alto = bar.bar ? "var(--cell-h)" : "0px";
    if (this.panelBarHeight !== alto) {
      document.documentElement.style.setProperty("--panelbar-h", alto);
      this.panelBarHeight = alto;
      this.viewportSucio = true;
    }
    if (!bar.bar) {
      this.panelBarRoot.replaceChildren();
      return;
    }
    const fila = document.createElement("nav");
    fila.className = "panelbar";
    fila.setAttribute("role", "toolbar");
    fila.setAttribute("aria-label", this.t("panelbar-label"));
    for (const [i, b] of bar.buttons.entries()) {
      const boton = document.createElement("button");
      boton.type = "button";
      boton.className = "panelbar-button";
      boton.dataset["kind"] = b.kind;
      boton.dataset["state"] = b.state;
      // `aria-pressed` es lo que un lector de pantalla entiende por «este
      // panel está abierto»; el foco del teclado va aparte, en el estado.
      boton.setAttribute("aria-pressed", String(b.state !== "closed"));
      boton.title = b.chord === "—" ? b.label : `${b.label} (${b.chord})`;
      const letra = document.createElement("span");
      letra.className = "panelbar-letter";
      letra.textContent = b.letter;
      const nombre = document.createElement("span");
      nombre.className = "panelbar-name";
      nombre.textContent = b.label;
      boton.append(letra, nombre);
      if (b.attention) {
        // La marca es un span APARTE y el botón conserva el estilo de su
        // estado: pintarlo entero de aviso le quitaría al lector la
        // respuesta a «¿a dónde van mis teclas?» justo cuando más la busca.
        const marca = document.createElement("span");
        marca.className = "panelbar-attention";
        marca.textContent = "·";
        marca.setAttribute("aria-label", this.t("panelbar-attention"));
        boton.append(marca);
      }
      boton.addEventListener("click", () => {
        this.send({ action: "panelbar_activate", button: i });
      });
      fila.append(boton);
    }
    this.panelBarRoot.replaceChildren(fila);
  }

  /**
   * La barra de menús, y el desplegable si hay uno abierto.
   *
   * Las mismas órdenes que el teclado, ordenadas por tema. No añade
   * capacidades: añade una forma de encontrarlas, para quien no sabe el
   * nombre de lo que busca.
   *
   * Una entrada apagada SIGUE saliendo, atenuada: esconder lo que esta
   * ventana no hace convertiría una limitación en un misterio.
   */
  private paintMenu(menu: MenuView): void {
    // La fila que la barra ocupa sale del CSS y entra en el reparto: el host
    // reparte sobre el alto que este renderer le declare, así que si la barra
    // no reservara su fila taparía la primera del listado — el mismo bug que
    // el TUI tuvo con el visor a pantalla completa.
    const alto = menu.bar ? "var(--cell-h)" : "0px";
    if (this.menuBarHeight !== alto) {
      document.documentElement.style.setProperty("--menubar-h", alto);
      this.menuBarHeight = alto;
      this.viewportSucio = true;
    }
    if (!menu.bar && menu.open === null) {
      this.menuRoot.replaceChildren();
      this.menuRoot.dataset["open"] = "false";
      return;
    }
    this.menuRoot.dataset["open"] = String(menu.open !== null);
    const barra = document.createElement("nav");
    barra.className = "menubar";
    barra.setAttribute("role", "menubar");
    barra.setAttribute("aria-label", this.t("menu-bar-label"));
    for (const [i, titulo] of menu.titles.entries()) {
      const boton = document.createElement("button");
      boton.type = "button";
      boton.className = "menubar-title";
      boton.id = `menu-title-${String(i)}`;
      boton.textContent = titulo;
      boton.setAttribute("role", "menuitem");
      boton.setAttribute("aria-haspopup", "true");
      boton.setAttribute("aria-expanded", String(menu.open === i));
      boton.addEventListener("click", () => {
        this.send({ action: "menu_open", menu: i });
      });
      barra.append(boton);
    }
    const caja = document.createElement("div");
    caja.className = "menu";
    caja.append(barra);

    if (menu.open !== null) {
      const lista = document.createElement("ul");
      lista.className = "menu-items";
      lista.setAttribute("role", "menu");
      // El desplegable cuelga de SU título, no del borde de la ventana: un
      // menú que se abre siempre a la izquierda no dice de cuál es.
      lista.style.setProperty("--menu-open", String(menu.open));
      for (const [i, item] of menu.items.entries()) {
        const fila = document.createElement("li");
        fila.className = "menu-item";
        fila.id = `menu-item-${String(i)}`;
        fila.setAttribute("role", "menuitem");
        fila.setAttribute("aria-disabled", String(!item.enabled));
        fila.dataset["enabled"] = String(item.enabled);
        fila.dataset["current"] = String(menu.cursor === i);
        const label = document.createElement("span");
        label.className = "menu-label";
        label.textContent = item.label;
        const chord = document.createElement("span");
        chord.className = "menu-chord";
        chord.textContent = item.chord;
        fila.append(label, chord);
        fila.addEventListener("mousemove", () => {
          this.send({ action: "menu_point_row", row: i });
        });
        fila.addEventListener("click", () => {
          this.send({ action: "menu_activate_row", row: i });
        });
        lista.append(fila);
      }
      lista.setAttribute("aria-activedescendant", `menu-item-${String(menu.cursor)}`);
      caja.append(lista);
      // Un click FUERA cierra, que es lo que hace un menú en todas partes.
      // El velo va DETRÁS del desplegable en el DOM y sin `z-index`, igual
      // que el resto de esta pantalla.
      const velo = document.createElement("div");
      velo.className = "menu-veil";
      velo.addEventListener("click", () => {
        this.send({ action: "menu_close" });
      });
      caja.prepend(velo);
    }
    this.menuRoot.replaceChildren(caja);
  }

  /** La paleta de comandos. */
  private paintPalette(palette: PaletteView | null): void {
    if (palette === null) {
      this.paletteRoot.replaceChildren();
      this.paletteRoot.dataset["open"] = "false";
      return;
    }
    this.paletteRoot.dataset["open"] = "true";
    const caja = document.createElement("section");
    caja.className = "palette";
    // Modal: mientras está abierta, las teclas son suyas — y el host lo
    // sabe, así que el lector de pantalla debe saberlo también.
    caja.setAttribute("role", "dialog");
    caja.setAttribute("aria-modal", "true");
    caja.setAttribute("aria-label", this.t("palette-title"));

    const query = document.createElement("div");
    query.className = "palette-query";
    query.textContent = palette.query;
    const cuenta = document.createElement("span");
    cuenta.className = "palette-count";
    cuenta.textContent = `${String(palette.rows.length)}/${String(palette.total)}`;
    query.append(cuenta);
    caja.append(query);

    const lista = document.createElement("ul");
    lista.className = "palette-rows";
    lista.setAttribute("role", "listbox");
    for (const [i, r] of palette.rows.entries()) {
      const fila = document.createElement("li");
      fila.className = "palette-row";
      fila.id = `palette-row-${String(i)}`;
      fila.setAttribute("role", "option");
      fila.setAttribute("aria-selected", String(palette.cursor === i));
      fila.dataset["enabled"] = String(r.enabled);
      fila.dataset["hostile"] = String(r.hostile);
      const texto = document.createElement("span");
      texto.className = "palette-text";
      texto.textContent = r.text;
      const desc = document.createElement("span");
      desc.className = "palette-desc";
      desc.textContent = r.desc;
      const chord = document.createElement("span");
      chord.className = "palette-chord";
      chord.textContent = r.chord;
      fila.append(texto, desc, chord);
      if (r.hostile) {
        // Solo una fila de PLUGIN puede serlo, y esta es la pantalla donde
        // se elige qué código de tercero correr: un texto enmascarado que
        // viaja sin decirlo se lee como fiel.
        fila.append(badge(this.t("hostile-name")));
      }
      lista.append(fila);
    }
    if (palette.cursor !== null) {
      lista.setAttribute(
        "aria-activedescendant",
        `palette-row-${String(palette.cursor)}`,
      );
    }
    if (palette.rows.length === 0) {
      const vacio = document.createElement("li");
      vacio.className = "empty";
      vacio.textContent = this.t("palette-empty");
      lista.append(vacio);
    }
    caja.append(lista);
    this.paletteRoot.replaceChildren(caja);
  }

  /** Lo que puede seguir a un prefijo a medias. */
  private paintWhichKey(panel: WhichKeyView | null): void {
    if (panel === null) {
      this.whichKeyRoot.replaceChildren();
      this.whichKeyRoot.dataset["open"] = "false";
      return;
    }
    this.whichKeyRoot.dataset["open"] = "true";
    const caja = document.createElement("section");
    caja.className = "whichkey";
    // No es un diálogo: no captura el foco ni espera respuesta. Es una ayuda
    // que aparece mientras la secuencia está a medias.
    caja.setAttribute("role", "group");
    caja.setAttribute("aria-label", panel.title);

    const titulo = document.createElement("header");
    titulo.className = "whichkey-title";
    titulo.textContent = panel.title;
    caja.append(titulo);

    const lista = document.createElement("ul");
    lista.className = "whichkey-rows";
    for (const r of panel.rows) {
      const fila = document.createElement("li");
      fila.className = "whichkey-row";
      fila.dataset["enabled"] = String(r.enabled);
      const chord = document.createElement("span");
      chord.className = "whichkey-chord";
      chord.textContent = r.chord;
      const label = document.createElement("span");
      label.className = "whichkey-label";
      // `opens_sequence` se MARCA en vez de nombrar un comando que la tecla
      // no ejecuta; el motivo de un atajo apagado ya viene traducido.
      label.textContent = r.opens_sequence ? `${r.label}…` : r.label;
      fila.append(chord, label);
      if (!r.enabled && r.reason !== "") {
        const motivo = document.createElement("span");
        motivo.className = "whichkey-reason";
        motivo.textContent = r.reason;
        fila.append(motivo);
      }
      lista.append(fila);
    }
    caja.append(lista);
    this.whichKeyRoot.replaceChildren(caja);
  }

  /**
   * La ayuda (F1).
   *
   * Todo lo que se pinta aquí llega ya resuelto: los bloques son un
   * vocabulario CERRADO, las marcas del corpus vienen convertidas en la
   * tecla de ESTE lector y los motivos de una fila apagada vienen
   * traducidos. Por eso cada bloque se construye con `createElement` y
   * `textContent` y nunca con `innerHTML`: un `help.md` de un plugin es
   * texto de tercero, y la única razón por la que se puede pintar es que
   * jamás se interpreta como marcado.
   */
  private paintHelp(help: HelpView | null): void {
    if (help === null) {
      this.helpRoot.replaceChildren();
      this.helpRoot.dataset["open"] = "false";
      this.helpBodyFocused = false;
      this.helpPintada = null;
      return;
    }
    // Dónde iba leyendo, para devolvérselo. El cuerpo se reconstruye entero
    // en CADA parche —y mover el cursor de la lateral es un parche—, así que
    // sin esto leer media página y pulsar `↓` devolvía el scroll a cero.
    // Solo dentro de la MISMA página: cambiar de página empieza arriba, que
    // es lo que hace cualquier lector.
    const scroll =
      this.helpPintada === help.topic_id
        ? (this.helpRoot.querySelector(".help-body")?.scrollTop ?? 0)
        : 0;
    this.helpPintada = help.topic_id;
    this.helpRoot.dataset["open"] = "true";
    this.helpBodyFocused = help.focus === "body";
    const caja = document.createElement("section");
    caja.className = "help";
    // Modal: mientras está abierta, las teclas son suyas — y el host lo
    // sabe, así que el lector de pantalla debe saberlo también.
    caja.setAttribute("role", "dialog");
    caja.setAttribute("aria-modal", "true");
    caja.setAttribute("aria-label", this.t("help-title"));

    caja.append(this.helpSidebar(help), this.helpBody(help));

    const pie = document.createElement("footer");
    pie.className = "help-hint";
    pie.textContent = this.t("help-hint-gui");
    caja.append(pie);
    this.helpRoot.replaceChildren(caja);
    if (scroll > 0) {
      const cuerpo = this.helpRoot.querySelector(".help-body");
      if (cuerpo instanceof HTMLElement) {
        cuerpo.scrollTop = scroll;
      }
    }
  }

  /** La lateral: cabeceras de grupo y páginas. */
  private helpSidebar(help: HelpView): HTMLElement {
    const nav = document.createElement("nav");
    nav.className = "help-topics";
    nav.dataset["focused"] = String(help.focus === "topics");
    if (help.filtering) {
      const filtro = document.createElement("div");
      filtro.className = "help-filter";
      filtro.textContent = `/${help.filter}`;
      nav.append(filtro);
    }
    const lista = document.createElement("ul");
    lista.setAttribute("role", "listbox");
    lista.className = "help-topic-rows";
    for (const [i, r] of help.sidebar.entries()) {
      const fila = document.createElement("li");
      fila.id = `help-topic-${String(i)}`;
      if (r.row === "group") {
        // Una cabecera NO es elegible: `presentation` la saca del recuento
        // de opciones que un lector de pantalla anuncia.
        fila.className = "help-group";
        fila.setAttribute("role", "presentation");
        fila.textContent = r.label;
      } else {
        fila.className = "help-topic";
        fila.setAttribute("role", "option");
        fila.setAttribute("aria-selected", String(help.cursor === i));
        fila.dataset["current"] = String(r.current);
        fila.textContent = r.title;
        fila.addEventListener("click", () => {
          this.send({ action: "help_select_topic", row: i });
        });
      }
      lista.append(fila);
    }
    lista.setAttribute("aria-activedescendant", `help-topic-${String(help.cursor)}`);
    nav.append(lista);
    // La lateral es más larga que su caja: sin esto, pasar del pliegue mueve
    // un cursor que no se ve.
    revelar(lista.children[help.cursor]);
    return nav;
  }

  /** El cuerpo: la prosa de la página y lo que se puede ejecutar en ella. */
  private helpBody(help: HelpView): HTMLElement {
    const cuerpo = document.createElement("article");
    cuerpo.className = "help-body";
    cuerpo.dataset["focused"] = String(help.focus === "body");
    // Enfocable: es lo que hace que las teclas de página desplacen ESTA caja
    // y no la ventana. `-1` porque al orden de tabulación se entra con la
    // tecla que la propia ayuda usa para cambiar de mitad.
    cuerpo.setAttribute("tabindex", "-1");

    const titulo = document.createElement("h1");
    titulo.textContent = help.title;
    cuerpo.append(titulo);
    if (help.badge !== null) {
      // La procedencia de una página de tercero. Siempre visible en una
      // página de plugin: una línea que aparece a veces enseña lo contrario
      // de la verdad cuando falta.
      const badge = document.createElement("p");
      badge.className = "help-badge";
      badge.textContent = help.badge;
      cuerpo.append(badge);
    }
    for (const b of help.blocks) {
      cuerpo.append(this.helpBlock(b));
    }
    if (help.actions.length > 0) {
      const lista = document.createElement("ul");
      lista.className = "help-actions";
      lista.setAttribute("role", "listbox");
      for (const [i, a] of help.actions.entries()) {
        const fila = document.createElement("li");
        fila.className = "help-action";
        fila.id = `help-action-${String(i)}`;
        fila.setAttribute("role", "option");
        fila.setAttribute("aria-selected", String(help.action_cursor === i));
        fila.dataset["enabled"] = String(a.enabled);
        const chord = document.createElement("span");
        chord.className = "help-action-chord";
        chord.textContent = a.chord;
        const label = document.createElement("span");
        label.className = "help-action-label";
        label.textContent = a.label;
        fila.append(chord, label);
        if (a.opens_topic) {
          // La flecha es lo ÚNICO que distingue «abre una página» de «corre
          // un comando», así que va en su propio nodo —pegada al texto queda
          // en la misma corrida bidi que la etiqueta y puede acabar delante—
          // pero DENTRO de la etiqueta: como hermana suya, el reparto flex la
          // mandaba al otro extremo de la fila, lejos de lo que califica.
          const abre = document.createElement("span");
          abre.className = "help-action-opens";
          abre.textContent = "→";
          label.append(abre);
        }
        if (!a.enabled && a.reason !== "") {
          const motivo = document.createElement("span");
          motivo.className = "help-action-reason";
          motivo.textContent = a.reason;
          fila.append(motivo);
        }
        if (a.enabled) {
          fila.addEventListener("click", () => {
            this.send({ action: "help_activate", index: i });
          });
        }
        lista.append(fila);
      }
      if (help.action_cursor !== null) {
        lista.setAttribute(
          "aria-activedescendant",
          `help-action-${String(help.action_cursor)}`,
        );
        revelar(lista.children[help.action_cursor]);
      }
      cuerpo.append(lista);
    }
    return cuerpo;
  }

  /** Un bloque del corpus, en su elemento semántico. */
  private helpBlock(b: HelpBlockView): HTMLElement {
    switch (b.block) {
      case "heading": {
        // El nivel viene acotado a 1..=3 por el host, y el título de la
        // página ya ocupa el `h1`: un encabezado del cuerpo empieza en `h2`.
        const nivel = Math.min(3, Math.max(1, b.level)) + 1;
        const h = document.createElement(`h${String(nivel)}`);
        h.textContent = b.text;
        return h;
      }
      case "paragraph": {
        const p = document.createElement("p");
        p.append(...b.spans.map((s) => this.helpSpan(s)));
        return p;
      }
      case "bullets": {
        const ul = document.createElement("ul");
        ul.className = "help-bullets";
        for (const item of b.items) {
          const li = document.createElement("li");
          li.append(...item.map((s) => this.helpSpan(s)));
          ul.append(li);
        }
        return ul;
      }
      case "code": {
        const pre = document.createElement("pre");
        pre.className = "help-code";
        if (b.lang !== null) {
          pre.dataset["lang"] = b.lang;
        }
        const code = document.createElement("code");
        code.textContent = b.text;
        pre.append(code);
        return pre;
      }
      case "table": {
        const tabla = document.createElement("table");
        tabla.className = "help-table";
        const thead = document.createElement("thead");
        const cabecera = document.createElement("tr");
        for (const c of b.header) {
          const th = document.createElement("th");
          th.setAttribute("scope", "col");
          th.textContent = c;
          cabecera.append(th);
        }
        thead.append(cabecera);
        const tbody = document.createElement("tbody");
        for (const r of b.rows) {
          const tr = document.createElement("tr");
          for (const c of r) {
            const td = document.createElement("td");
            td.textContent = c;
            tr.append(td);
          }
          tbody.append(tr);
        }
        tabla.append(thead, tbody);
        return tabla;
      }
      case "callout": {
        const aside = document.createElement("aside");
        aside.className = "help-callout";
        aside.dataset["kind"] = b.kind;
        const etiqueta = document.createElement("span");
        etiqueta.className = "help-callout-kind";
        etiqueta.textContent = this.t(`help-callout-${b.kind}`);
        aside.append(etiqueta);
        aside.append(...b.spans.map((s) => this.helpSpan(s)));
        return aside;
      }
      case "keys": {
        const tabla = document.createElement("table");
        tabla.className = "help-keys";
        const tbody = document.createElement("tbody");
        for (const r of b.rows) {
          const tr = document.createElement("tr");
          tr.dataset["enabled"] = String(r.enabled);
          const chord = document.createElement("th");
          chord.setAttribute("scope", "row");
          chord.className = "help-key-chord";
          chord.textContent = r.chord;
          const label = document.createElement("td");
          label.className = "help-key-label";
          label.textContent = r.label;
          tr.append(chord, label);
          // Atenuar sin decir por qué deja al lector adivinando si la
          // ventana está rota. El motivo va en su PROPIA celda y no pegado
          // al texto: compuestos en banda, el guion y el motivo quedan en la
          // misma corrida bidi que la etiqueta, y una etiqueta que acabe en
          // RTL fuerte se los lleva al lado que no es.
          if (!r.enabled && r.reason !== "") {
            const motivo = document.createElement("td");
            motivo.className = "help-key-reason";
            motivo.textContent = r.reason;
            tr.append(motivo);
          }
          tbody.append(tr);
        }
        tabla.append(tbody);
        return tabla;
      }
    }
  }

  /** Un fragmento en línea. */
  private helpSpan(s: HelpSpanView): HTMLElement {
    switch (s.span) {
      case "text": {
        const span = document.createElement("span");
        span.textContent = s.text;
        return span;
      }
      case "strong": {
        const el = document.createElement("strong");
        el.textContent = s.text;
        return el;
      }
      case "emph": {
        const el = document.createElement("em");
        el.textContent = s.text;
        return el;
      }
      case "code": {
        const el = document.createElement("code");
        el.textContent = s.text;
        return el;
      }
      case "command": {
        // `kbd` solo cuando es una TECLA de verdad: cuando el comando no
        // tiene atajo, lo que viaja es su nombre, y pintarlo como una tecla
        // sería enseñar una que no existe.
        const el = document.createElement(s.is_chord ? "kbd" : "span");
        el.className = s.is_chord ? "help-chord" : "help-cmd";
        el.textContent = s.text;
        return el;
      }
      case "link": {
        // NO es un control: una marca `[[topic]]` en la prosa no está en la
        // lista de acciones —esa la forman los comandos de la página y sus
        // «ver también»—, así que no hay nada que activar. Un `button` que
        // no hace nada es peor que un texto que se lee como enlace, y es la
        // misma decisión que tomó el TUI.
        const el = document.createElement("span");
        el.className = "help-link";
        el.textContent = s.text;
        return el;
      }
    }
  }

  /**
   * Los ajustes (F11), en solo lectura.
   *
   * Dos clases de sección y ninguna decisión aquí: el host manda el registro
   * con su valor ya resuelto y las ubicaciones ya saneadas. Lo único que este
   * método sabe es que una fila de ruta que falta se dice, y que la lista es
   * un `listbox` con un cursor que el host lleva.
   */
  private paintSettings(settings: SettingsView | null): void {
    if (settings === null) {
      this.settingsRoot.replaceChildren();
      this.settingsRoot.dataset["open"] = "false";
      return;
    }
    this.settingsRoot.dataset["open"] = "true";
    const caja = document.createElement("section");
    caja.className = "settings";
    caja.setAttribute("role", "dialog");
    caja.setAttribute("aria-modal", "true");
    caja.setAttribute("aria-label", this.t("settings-title"));

    const titulo = document.createElement("h1");
    titulo.textContent = this.t("settings-title");
    caja.append(titulo);
    if (settings.read_only) {
      // Un AVISO y no un botón apagado: apagar un control invita a probarlo,
      // y esta ventana no escribe ajustes todavía.
      const nota = document.createElement("p");
      nota.className = "settings-note";
      nota.setAttribute("role", "note");
      nota.textContent = this.t("settings-read-only");
      caja.append(nota);
    }

    const lista = document.createElement("ul");
    lista.className = "settings-rows";
    lista.setAttribute("role", "listbox");
    // El cursor cuenta filas ELEGIBLES: las cabeceras no entran, así que el
    // índice se lleva aparte del recorrido de las secciones.
    let i = 0;
    for (const sec of settings.sections) {
      const cabecera = document.createElement("li");
      cabecera.className = "settings-group";
      cabecera.setAttribute("role", "presentation");
      cabecera.textContent = sec.title;
      // Si TODAS las filas de la sección piden reiniciar, se dice UNA vez en
      // su cabecera. Cinco insignias idénticas no informan de nada: hacen
      // ruido justo encima de lo que sí varía, que es el valor.
      const todas =
        sec.section === "settings" &&
        sec.rows.length > 0 &&
        sec.rows.every((r) => r.restart_required);
      if (todas) {
        const marca = document.createElement("span");
        marca.className = "settings-badge";
        marca.textContent = this.t("settings-restart-badge");
        cabecera.append(" ", marca);
      }
      lista.append(cabecera);
      // El `switch` va FUERA del bucle de filas: dentro, TypeScript no
      // puede estrechar el tipo de la fila a partir de la sección, y una
      // fila de ruta y una de ajuste no comparten ni un campo.
      if (sec.section === "settings") {
        for (const r of sec.rows) {
          const fila = this.settingsRow(i, settings.cursor);
          const nombre = document.createElement("span");
          nombre.className = "settings-name";
          nombre.textContent = r.name;
          const valor = document.createElement("span");
          valor.className = "settings-value";
          valor.dataset["hostile"] = String(r.hostile);
          valor.textContent = r.value;
          if (r.hostile) {
            // La fila de RUTA de esta misma lista siempre lo dijo; la de
            // ajuste no, y las dos pintan en la misma columna.
            valor.append(badge(this.t("hostile-name")));
          }
          fila.append(nombre, valor);
          if (r.restart_required && !todas) {
            const marca = document.createElement("span");
            marca.className = "settings-badge";
            marca.textContent = this.t("settings-restart-badge");
            fila.append(marca);
          }
          const desc = document.createElement("span");
          desc.className = "settings-desc";
          desc.textContent = r.desc;
          fila.append(desc);
          lista.append(fila);
          i += 1;
        }
      } else {
        for (const r of sec.rows) {
          const fila = this.settingsRow(i, settings.cursor);
          const nombre = document.createElement("span");
          nombre.className = "settings-name";
          nombre.textContent = r.label;
          const valor = document.createElement("span");
          valor.className = "settings-value";
          valor.dataset["hostile"] = String(r.hostile);
          valor.textContent = r.display;
          fila.append(nombre, valor);
          if (r.hostile) {
            valor.append(badge(this.t("hostile-name")));
          }
          if (r.missing) {
            // Que un sitio no exista es un HECHO del diagnóstico y no un
            // error: una capa que nadie ha creado es lo normal.
            const falta = document.createElement("span");
            falta.className = "settings-missing";
            falta.textContent = this.t("settings-path-missing");
            fila.append(falta);
          }
          lista.append(fila);
          i += 1;
        }
      }
    }
    lista.setAttribute(
      "aria-activedescendant",
      `settings-row-${String(settings.cursor)}`,
    );
    caja.append(lista);
    this.settingsRoot.replaceChildren(caja);
    revelar(lista.querySelector(`#settings-row-${String(settings.cursor)}`) ?? undefined);
  }

  /**
   * El gestor de extensiones (F12), en solo lectura.
   *
   * Las capabilities van en la FILA y no escondidas tras un gesto: son la
   * decisión que un humano aprueba, y esta ventana la enseña sin poder
   * tomarla. No hay ni un control para aprobar o encender: lo que no está no
   * se pulsa por accidente.
   */
  private paintExtensions(ext: ExtensionsView | null): void {
    if (ext === null) {
      this.extensionsRoot.replaceChildren();
      this.extensionsRoot.dataset["open"] = "false";
      return;
    }
    this.extensionsRoot.dataset["open"] = "true";
    const caja = document.createElement("section");
    caja.className = "extensions";
    caja.setAttribute("role", "dialog");
    caja.setAttribute("aria-modal", "true");
    caja.setAttribute("aria-label", this.t("ext-title"));

    const titulo = document.createElement("h1");
    titulo.textContent = this.t("ext-title");
    caja.append(titulo);

    if (ext.loading) {
      // «Cargando» y «ninguna» no son lo mismo, y una lista vacía sin este
      // aviso se lee como lo segundo.
      const cargando = document.createElement("p");
      cargando.className = "extensions-note";
      cargando.setAttribute("role", "status");
      cargando.textContent = this.t("ext-loading");
      caja.append(cargando);
    } else if (ext.rows.length === 0) {
      const vacio = document.createElement("p");
      vacio.className = "extensions-note";
      vacio.textContent = this.t("ext-empty");
      caja.append(vacio);
    }

    const lista = document.createElement("ul");
    lista.className = "extensions-rows";
    lista.setAttribute("role", "listbox");
    for (const [i, r] of ext.rows.entries()) {
      const fila = document.createElement("li");
      fila.className = "extensions-row";
      fila.id = `extension-row-${String(i)}`;
      fila.setAttribute("role", "option");
      fila.setAttribute("aria-selected", String(ext.cursor === i));
      fila.addEventListener("click", () => {
        this.send({ action: "extension_select_row", row: i });
      });

      const nombre = document.createElement("span");
      nombre.className = "extensions-name";
      nombre.textContent = r.name;
      const version = document.createElement("span");
      version.className = "extensions-version";
      version.textContent = r.version;
      fila.append(nombre, version);

      const estado = document.createElement("span");
      estado.className = "extensions-state";
      // DOS hechos independientes, y se dicen los dos: una extensión
      // aprobada pero apagada no es lo mismo que una sin aprobar.
      estado.dataset["approved"] = String(r.approved);
      estado.dataset["enabled"] = String(r.enabled);
      estado.textContent = r.approved
        ? this.t(r.enabled ? "ext-state-on" : "ext-state-off")
        : this.t("ext-unapproved");
      fila.append(estado);

      const meta = document.createElement("span");
      meta.className = "extensions-meta";
      const trozos = [r.category];
      if (r.publisher !== "") {
        trozos.push(r.publisher);
      }
      meta.textContent = trozos.join(" · ");
      fila.append(meta);

      if (r.description !== "") {
        const desc = document.createElement("span");
        desc.className = "extensions-desc";
        desc.textContent = r.description;
        fila.append(desc);
      }

      const caps = document.createElement("ul");
      caps.className = "extensions-caps";
      for (const c of r.capabilities) {
        const cap = document.createElement("li");
        cap.className = "extensions-cap";
        cap.textContent = c;
        caps.append(cap);
      }
      if (r.capabilities.length > 0) {
        fila.append(caps);
      }
      lista.append(fila);
    }
    if (ext.rows.length > 0) {
      lista.setAttribute("aria-activedescendant", `extension-row-${String(ext.cursor)}`);
    }
    caja.append(lista);

    if (ext.detail !== null) {
      // La ficha se titula con el NOMBRE de su extensión, no con «sus
      // ajustes» a secas: con la lista desplazada, la fila elegida puede no
      // estar a la vista y la ficha se quedaba sin dueño visible.
      const suya = ext.rows.find((r) => r.id === ext.detail?.id);
      caja.append(this.extensionDetail(ext.detail, suya?.name ?? ""));
    }
    if (ext.errors.length > 0) {
      const errores = document.createElement("ul");
      errores.className = "extensions-errors";
      for (const e of ext.errors) {
        const li = document.createElement("li");
        const dir = document.createElement("span");
        dir.className = "extensions-error-dir";
        dir.dataset["hostile"] = String(e.hostile);
        dir.textContent = e.dir;
        if (e.hostile) {
          dir.append(badge(this.t("hostile-name")));
        }
        const motivo = document.createElement("span");
        motivo.className = "extensions-error-reason";
        motivo.dataset["hostile"] = String(e.reason_hostile);
        motivo.textContent = e.reason;
        if (e.reason_hostile) {
          // El motivo lo escribe el core, pero CITA el manifiesto del plugin
          // y a veces un `Path::display()`.
          motivo.append(badge(this.t("hostile-name")));
        }
        li.append(dir, motivo);
        errores.append(li);
      }
      caja.append(errores);
    }
    this.extensionsRoot.replaceChildren(caja);
    revelar(lista.querySelector(`#extension-row-${String(ext.cursor)}`) ?? undefined);
  }

  /** La ficha de una extensión: sus claves `[config]` con su valor. */
  private extensionDetail(d: ExtensionsView["detail"], nombre: string): HTMLElement {
    const ficha = document.createElement("article");
    ficha.className = "extensions-detail";
    if (d === null) {
      return ficha;
    }
    const titulo = document.createElement("h2");
    titulo.textContent = this.t("ext-config-title");
    if (nombre !== "") {
      const suya = document.createElement("span");
      suya.className = "extensions-detail-of";
      suya.textContent = nombre;
      titulo.append(" · ", suya);
    }
    ficha.append(titulo);
    if (d.config.length === 0) {
      const nada = document.createElement("p");
      nada.className = "extensions-note";
      nada.textContent = this.t("ext-config-none");
      ficha.append(nada);
      return ficha;
    }
    const tabla = document.createElement("table");
    tabla.className = "extensions-config";
    const tbody = document.createElement("tbody");
    for (const [i, k] of d.config.entries()) {
      const tr = document.createElement("tr");
      tr.id = `extension-key-${String(i)}`;
      // Cuál está elegida y cuál se puede editar: sin lo segundo, la
      // pantalla ofrece `Enter` sobre una clave de un tipo que este build no
      // conoce y el lector concluye que la escritura falló.
      tr.dataset["current"] = String(i === d.cursor);
      tr.dataset["editable"] = String(k.editable);
      // Un valor que NO es el del esquema se marca: es lo único que
      // distingue «así viene» de «así lo dejaste».
      tr.dataset["changed"] = String(k.value !== k.default);
      const clave = document.createElement("th");
      clave.setAttribute("scope", "row");
      clave.className = "extensions-key";
      clave.textContent = k.key;
      const valor = document.createElement("td");
      valor.className = "extensions-key-value";
      valor.dataset["hostile"] = String(k.hostile);
      if (i === d.cursor && d.editing !== null) {
        // Lo que se está TECLEANDO, en su propio nodo y marcado: sustituye
        // al valor porque es lo que se va a escribir, no lo que hay.
        const buf = document.createElement("span");
        buf.className = "extensions-key-editing";
        buf.dataset["hostile"] = String(d.editing_hostile);
        buf.textContent = d.editing;
        valor.append(buf);
        if (d.editing_hostile) {
          valor.append(badge(this.t("hostile-name")));
        }
      } else {
        valor.textContent = k.value;
      }
      if (k.hostile && d.editing === null) {
        // Lo que se pinta difiere de lo que es, y lo escribe el plugin: se
        // dice, igual que en un nombre de fichero.
        valor.append(badge(this.t("hostile-name")));
      }
      const tipo = document.createElement("td");
      tipo.className = "extensions-key-kind";
      // El tipo y el dominio, cada uno en su nodo: unirlos en uno solo deja
      // que un valor de `enum` con letras RTL reordene el par entero, y el
      // `unicode-bidi: isolate` del contenedor solo separa HERMANOS.
      const kindSpan = document.createElement("span");
      kindSpan.className = "extensions-key-kind-name";
      kindSpan.textContent = k.kind;
      tipo.append(kindSpan);
      if (k.domain !== "") {
        const sep = document.createElement("span");
        sep.className = "sep";
        sep.textContent = " · ";
        const dom = document.createElement("span");
        dom.className = "extensions-key-domain";
        dom.textContent = k.domain;
        tipo.append(sep, dom);
      }
      const desc = document.createElement("td");
      desc.className = "extensions-key-desc";
      desc.textContent = k.description;
      tr.append(clave, valor, tipo, desc);
      tbody.append(tr);
    }
    tabla.append(tbody);
    ficha.append(tabla);
    ficha.append(this.extensionCommands(d.commands));
    return ficha;
  }

  /**
   * Los comandos que aporta una extensión.
   *
   * Se LISTAN y no se lanzan desde aquí: la paleta es la puerta —la misma
   * que en el TUI—, y tener dos deja dos respuestas a qué significa que uno
   * falle. El `id` no se pinta: el manifiesto no le valida charset.
   */
  private extensionCommands(cmds: ExtensionCommandView[]): HTMLElement {
    const caja = document.createElement("div");
    caja.className = "extensions-commands";
    if (cmds.length === 0) {
      return caja;
    }
    const titulo = document.createElement("h3");
    titulo.textContent = this.t("ext-commands-title");
    const lista = document.createElement("ul");
    for (const c of cmds) {
      const li = document.createElement("li");
      li.className = "extensions-command";
      li.dataset["hostile"] = String(c.hostile);
      li.textContent = c.title;
      if (c.hostile) {
        li.append(badge(this.t("hostile-name")));
      }
      lista.append(li);
    }
    caja.append(titulo, lista);
    return caja;
  }

  /**
   * Las sesiones de agente que esta ventana ha visto pedir permiso.
   *
   * La NOTA va dentro del panel y no en la documentación: esta lista no es
   * el censo de agentes del sistema —no hay método que lo dé—, y una lista
   * vacía sin esa frase se lee como «ningún agente ha tocado nada».
   */
  private paintAgents(agents: AgentsView | null): void {
    if (agents === null) {
      if (this.agentsRoot.dataset["open"] === "true") {
        this.agentsRoot.replaceChildren();
        this.agentsRoot.dataset["open"] = "false";
      }
      return;
    }
    this.agentsRoot.dataset["open"] = "true";
    const caja = document.createElement("section");
    caja.className = "agents";
    caja.setAttribute("role", "dialog");
    caja.setAttribute("aria-modal", "true");
    caja.setAttribute("aria-label", this.t("agents-title"));
    const titulo = document.createElement("h2");
    titulo.textContent = this.t("agents-title");
    const nota = document.createElement("p");
    nota.className = "agents-note";
    nota.textContent = agents.note;
    caja.append(titulo, nota);
    if (agents.forgotten > 0) {
      // Lo OLVIDADO se dice: el id de sesión lo elige el agente, así que
      // inundar la lista para empujar fuera a una concreta está a su
      // alcance, y una lista recortada que se presenta como completa es lo
      // que convierte eso en «esa sesión no existe».
      const podadas = document.createElement("p");
      podadas.className = "agents-forgotten";
      podadas.setAttribute("role", "status");
      podadas.textContent = String(agents.forgotten);
      podadas.dataset["forgotten"] = String(agents.forgotten);
      caja.append(podadas);
    }
    if (agents.rows.length === 0) {
      // La frase la compone el HOST: una lista vacía significa cosas
      // distintas según si esta ventana escucha las peticiones.
      const vacio = document.createElement("p");
      vacio.className = "agents-empty";
      vacio.textContent = agents.empty;
      caja.append(vacio);
      this.agentsRoot.replaceChildren(caja);
      return;
    }
    const lista = document.createElement("ul");
    lista.className = "agents-rows";
    lista.setAttribute("role", "listbox");
    for (const [i, r] of agents.rows.entries()) {
      const li = document.createElement("li");
      li.className = "agents-row";
      li.id = `agent-row-${String(i)}`;
      li.setAttribute("role", "option");
      li.setAttribute("aria-selected", String(agents.cursor === i));
      li.addEventListener("click", () => {
        // La generación viaja con el clic: la lista se reordena sola, y un
        // clic contra la de antes elige otra fila — aquí «esta fila» es de
        // quién se deshace el trabajo.
        this.send({
          action: "agent_select_row",
          row: i,
          generation: agents.generation,
        });
      });
      // El id y el último op, cada uno aislado y con su bandera: el id es
      // una clave opaca del daemon y puede traer letras RTL que reordenarían
      // la fila entera.
      const id = document.createElement("span");
      id.className = "agents-session";
      id.dataset["hostile"] = String(r.session_hostile);
      id.textContent = r.session;
      li.append(id);
      if (r.session_hostile) {
        li.append(badge(this.t("hostile-name")));
      }
      const op = document.createElement("span");
      op.className = "agents-op";
      op.dataset["hostile"] = String(r.last_op_hostile);
      op.textContent = r.last_op;
      li.append(op);
      if (r.last_op_hostile) {
        li.append(badge(this.t("hostile-name")));
      }
      // Pidió N y se le aprobaron M: no son lo mismo cuando contestó otra
      // ventana, cuando se denegó, o cuando caducó.
      const cuentas = document.createElement("span");
      cuentas.className = "agents-counts";
      cuentas.textContent = r.counts;
      li.append(cuentas);
      li.dataset["undoing"] = String(r.undoing);
      lista.append(li);
    }
    lista.setAttribute("aria-activedescendant", `agent-row-${String(agents.cursor)}`);
    caja.append(lista);
    this.agentsRoot.replaceChildren(caja);
    revelar(lista.querySelector(`#agent-row-${String(agents.cursor)}`) ?? undefined);
  }

  /**
   * Lo que imprimió un comando de extensión.
   *
   * Todo aquí lo escribe un tercero, y las tres cosas se dicen: quién
   * imprimió, qué comando, y si la salida se cortó — que el receptor no
   * puede deducir, porque el texto le llega ya corto.
   */
  private paintPluginOutput(output: ExtensionOutputView | null): void {
    if (output === null) {
      if (this.pluginOutputRoot.dataset["open"] === "true") {
        this.pluginOutputRoot.replaceChildren();
        this.pluginOutputRoot.dataset["open"] = "false";
      }
      return;
    }
    this.pluginOutputRoot.dataset["open"] = "true";
    const caja = document.createElement("section");
    caja.className = "plugin-output";
    caja.setAttribute("role", "dialog");
    caja.setAttribute("aria-modal", "true");
    caja.setAttribute("aria-label", this.t("plugin-output-title"));
    const titulo = document.createElement("h2");
    titulo.textContent = this.t("plugin-output-title");
    const quien = document.createElement("p");
    quien.className = "plugin-output-who";
    // Quién y qué, cada uno en su nodo y con SU bandera: unirlos en una
    // frase deja que un título de tercero con letras RTL reordene el par
    // entero, y una bandera para los dos acaba describiendo al otro.
    const plugin = document.createElement("span");
    plugin.className = "plugin-output-plugin";
    plugin.dataset["hostile"] = String(output.plugin.hostile);
    plugin.textContent = output.plugin.text;
    quien.append(plugin);
    if (output.plugin.hostile) {
      quien.append(badge(this.t("hostile-name")));
    }
    // El id reverse-DNS, que el core SÍ valida: dos extensiones pueden
    // llamarse igual y el nombre lo escribe el manifiesto.
    const ident = document.createElement("span");
    ident.className = "plugin-output-id";
    ident.textContent = output.plugin_id;
    quien.append(ident);
    if (output.command.text !== "") {
      const cmd = document.createElement("span");
      cmd.className = "plugin-output-command";
      cmd.dataset["hostile"] = String(output.command.hostile);
      cmd.textContent = output.command.text;
      quien.append(cmd);
      if (output.command.hostile) {
        quien.append(badge(this.t("hostile-name")));
      }
    }
    const cuerpo = document.createElement("pre");
    cuerpo.className = "plugin-output-text";
    cuerpo.dataset["hostile"] = String(output.text_hostile);
    // Vacío se DICE: un panel en blanco se lee como que no llegó a correr.
    cuerpo.textContent =
      output.lines.length === 0 ? this.t("plugin-output-empty") : output.lines.join("\n");
    caja.append(titulo, quien, cuerpo);
    if (output.text_hostile) {
      caja.append(badge(this.t("hostile-name")));
    }
    if (output.truncated) {
      const corte = document.createElement("p");
      corte.className = "plugin-output-truncated";
      corte.setAttribute("role", "status");
      corte.textContent = this.t("plugin-output-truncated");
      caja.append(corte);
    }
    this.pluginOutputRoot.replaceChildren(caja);
  }

  /**
   * La salida de un programa que quien hospeda corrió esperándolo (#312):
   * el comparador de dos ficheros. La misma caja que la salida de una
   * extensión —es la misma clase de texto, de otro programa— con el
   * comando que corrió y, si no arrancó, dicho.
   */
  private paintProgramOutput(output: ProgramOutputView | null): void {
    if (output === null) {
      if (this.programOutputRoot.dataset["open"] === "true") {
        this.programOutputRoot.replaceChildren();
        this.programOutputRoot.dataset["open"] = "false";
      }
      return;
    }
    this.programOutputRoot.dataset["open"] = "true";
    const caja = document.createElement("section");
    caja.className = "plugin-output program-output";
    caja.setAttribute("role", "dialog");
    caja.setAttribute("aria-modal", "true");
    caja.setAttribute("aria-label", this.t(output.title_key));
    const titulo = document.createElement("h2");
    titulo.textContent = this.t(output.title_key);
    const quien = document.createElement("p");
    quien.className = "plugin-output-who";
    const cmd = document.createElement("span");
    cmd.className = "plugin-output-command program-output-command";
    cmd.dataset["hostile"] = String(output.command.hostile);
    cmd.textContent = output.command.text;
    quien.append(cmd);
    if (output.command.hostile) {
      quien.append(badge(this.t("hostile-name")));
    }
    caja.append(titulo, quien);
    if (output.failed) {
      const no = document.createElement("p");
      no.className = "program-output-failed";
      no.setAttribute("role", "alert");
      no.textContent = this.t("program-output-failed");
      caja.append(no);
    }
    const cuerpo = document.createElement("pre");
    cuerpo.className = "plugin-output-text";
    cuerpo.dataset["hostile"] = String(output.text_hostile);
    cuerpo.textContent =
      output.lines.length === 0 ? this.t("plugin-output-empty") : output.lines.join("\n");
    caja.append(cuerpo);
    if (output.text_hostile) {
      caja.append(badge(this.t("hostile-name")));
    }
    if (output.truncated) {
      const corte = document.createElement("p");
      corte.className = "plugin-output-truncated";
      corte.setAttribute("role", "status");
      corte.textContent = this.t("plugin-output-truncated");
      caja.append(corte);
    }
    this.programOutputRoot.replaceChildren(caja);
  }

  /**
   * El tema por dentro (F9).
   *
   * Cada rol con su color como MUESTRA, no como texto: un `#2d4f8a` no le
   * dice nada a nadie hasta que se ve al lado del cuadrado que pinta.
   */
  private paintTheme(theme: ThemeView | null): void {
    if (theme === null) {
      this.themeRoot.replaceChildren();
      this.themeRoot.dataset["open"] = "false";
      return;
    }
    this.themeRoot.dataset["open"] = "true";
    const caja = document.createElement("section");
    caja.className = "theme";
    caja.setAttribute("role", "dialog");
    caja.setAttribute("aria-modal", "true");
    caja.setAttribute("aria-label", this.t("theme-title"));

    const titulo = document.createElement("h1");
    titulo.textContent = `${this.t("theme-title")} · ${theme.name}`;
    caja.append(titulo);

    // La lista de temas, con el cursor. Moverse por ella previsualiza EN
    // VIVO: los colores de la ventana entera ya han cambiado cuando esto se
    // pinta, así que lo que hay debajo es el tema señalado.
    if (theme.choices.length > 0) {
      const elegir = document.createElement("ul");
      elegir.className = "theme-choices";
      elegir.setAttribute("role", "listbox");
      for (const [i, nombre] of theme.choices.entries()) {
        const fila = document.createElement("li");
        fila.className = "theme-choice";
        fila.id = `theme-choice-${String(i)}`;
        fila.setAttribute("role", "option");
        fila.setAttribute("aria-selected", String(theme.cursor === i));
        fila.textContent = nombre;
        elegir.append(fila);
      }
      elegir.setAttribute(
        "aria-activedescendant",
        `theme-choice-${String(theme.cursor)}`,
      );
      caja.append(elegir);
    }

    if (theme.unsupported_effects.length > 0) {
      // Se NOMBRAN. Un tema retro que se ve idéntico a los demás se lee como
      // roto, y el usuario va a buscar el bug donde no está.
      const aviso = document.createElement("p");
      aviso.className = "theme-effects";
      aviso.setAttribute("role", "note");
      const hostil = theme.unsupported_effects.some((e) => e.hostile);
      aviso.textContent = `${this.t("theme-effects-unsupported")} ${theme.unsupported_effects
        .map((e) => e.key)
        .join(" · ")}`;
      aviso.dataset["hostile"] = String(hostil);
      if (hostil) {
        // Las claves salen del fichero de tema: si se enmascararon, se dice.
        aviso.classList.add("hostile");
        aviso.append(badge(this.t("hostile-name")));
      }
      caja.append(aviso);
    }

    const sub = document.createElement("h2");
    sub.textContent = this.t("theme-roles");
    caja.append(sub);

    const lista = document.createElement("ul");
    lista.className = "theme-roles";
    for (const r of theme.roles) {
      const fila = document.createElement("li");
      fila.className = "theme-role";
      const muestra = document.createElement("span");
      muestra.className = "theme-swatch";
      // Por CSSOM y no por atributo `style`: la CSP lo bloquea.
      muestra.style.setProperty("background-color", r.color);
      const nombre = document.createElement("span");
      nombre.className = "theme-role-name";
      nombre.textContent = r.role;
      const hex = document.createElement("span");
      hex.className = "theme-role-hex";
      hex.textContent = r.color;
      fila.append(muestra, nombre, hex);
      lista.append(fila);
    }
    caja.append(lista);
    this.themeRoot.replaceChildren(caja);
  }

  /**
   * La búsqueda por el subárbol, con lo que lleva encontrado.
   *
   * Los resultados se pueden recorrer y usar ANTES de que termine, que es la
   * mitad del valor de buscar en un árbol grande. La frase de estado la
   * compone el host: dice cuántos van y si sigue.
   */
  /** El panel de sincronización: el PLAN. Se pinta en el mismo hueco que el
   *  de diferencias — son dos pantallas enteras y no coinciden. */
  private paintSync(sync: SyncView | null): void {
    if (sync === null) {
      if (this.syncRoot.dataset["open"] === "true") {
        this.syncRoot.replaceChildren();
        this.syncRoot.dataset["open"] = "false";
      }
      return;
    }
    this.syncRoot.dataset["open"] = "true";
    const caja = document.createElement("section");
    caja.className = "sync";
    caja.setAttribute("role", "dialog");
    caja.setAttribute("aria-modal", "true");
    caja.setAttribute("aria-label", this.t("sync-title"));

    // El MODO, arriba y en su propio elemento: un espejo borra en el destino
    // y una actualización no, y quien aprueba tiene que verlo antes.
    const modo = document.createElement("p");
    modo.className = "sync-mode";
    modo.dataset["mode"] = sync.mode;
    // Claves LITERALES: una interpolada no la ve el barrido que comprueba
    // que toda clave existe, y una clave que falta se pinta como su propio
    // identificador.
    modo.textContent =
      sync.mode === "mirror"
        ? this.t("gui-sync-mode-mirror")
        : this.t("gui-sync-mode-update");
    caja.append(modo);

    const raices = document.createElement("div");
    raices.className = "sync-roots";
    for (const raiz of [sync.source, sync.dest]) {
      const r = document.createElement("span");
      r.className = "sync-root";
      r.dataset["hostile"] = String(raiz.hostile);
      r.textContent = raiz.text;
      if (raiz.hostile) {
        r.append(badge(this.t("hostile-name")));
      }
      raices.append(r);
    }
    caja.append(raices);

    if (sync.summary.length > 0) {
      // El RESUMEN, arriba: cuántos pasos no se pueden deshacer, cuántos
      // bytes, qué no se pudo leer. Es lo que se lee antes de aprobar, y
      // debajo de la lista no lo lee nadie.
      const resumen = document.createElement("ul");
      resumen.className = "sync-summary";
      for (const linea of sync.summary) {
        const li = document.createElement("li");
        li.textContent = linea;
        resumen.append(li);
      }
      caja.append(resumen);
    }
    if (sync.blockers.length > 0) {
      // Lo que IMPIDE aplicar va como ALERTA y arriba: un plan que no se
      // puede ejecutar tiene que decir por qué antes que enseñar sus pasos.
      const lista = document.createElement("ul");
      lista.className = "sync-blockers";
      lista.setAttribute("role", "alert");
      for (const b of sync.blockers) {
        const li = document.createElement("li");
        const que = document.createElement("span");
        que.className = "sync-blocker-label";
        que.textContent = b.label;
        // La ruta en su propio elemento: «el destino es de solo lectura» sin
        // decir CUÁL manda a buscar el problema a ciegas.
        const donde = document.createElement("span");
        donde.className = "sync-blocker-path";
        donde.dataset["hostile"] = String(b.path_hostile);
        donde.textContent = b.path;
        li.append(que, donde);
        if (b.path_hostile) {
          li.append(badge(this.t("hostile-name")));
        }
        lista.append(li);
      }
      if (sync.blockers_total > sync.blockers.length) {
        // El wire recorta la lista: que hay cuarenta mil y se enseñan
        // doscientos cincuenta y seis tiene que decirse.
        const mas = document.createElement("li");
        mas.className = "sync-blockers-more";
        mas.textContent = `${String(sync.blockers.length)} / ${String(sync.blockers_total)}`;
        lista.append(mas);
      }
      caja.append(lista);
    }

    const pasos = document.createElement("ol");
    pasos.className = "sync-steps";
    pasos.setAttribute("role", "list");
    // La numeración arranca donde arranca la VENTANA: la lista no es el plan
    // entero, y pintarla desde uno la haría pasar por él.
    pasos.setAttribute("start", String(sync.first_visible + 1));
    if (sync.total > sync.steps.length) {
      pasos.dataset["window"] = `${String(sync.first_visible + 1)}-${String(
        sync.first_visible + sync.steps.length,
      )}/${String(sync.total)}`;
    }
    for (const p of sync.steps) {
      pasos.append(this.syncStep(p));
    }
    caja.append(pasos);

    if (sync.failures.length > 0) {
      // Lo que FALLÓ, uno a uno: el recuento va en el estado, y «3 fallaron»
      // sin decir cuáles no se puede arreglar.
      const fallos = document.createElement("ul");
      fallos.className = "sync-failures";
      fallos.setAttribute("role", "alert");
      for (const f of sync.failures) {
        const li = document.createElement("li");
        li.dataset["anchor"] = f.anchor;
        const causa = document.createElement("span");
        causa.className = "sync-failure-cause";
        causa.textContent = f.cause;
        const ruta = document.createElement("span");
        ruta.className = "sync-failure-path";
        ruta.dataset["hostile"] = String(f.path_hostile);
        ruta.textContent = f.path;
        li.append(causa, ruta);
        if (f.anchor_label !== "") {
          const ancla = document.createElement("span");
          ancla.className = "sync-failure-anchor";
          ancla.textContent = f.anchor_label;
          li.append(ancla);
        }
        if (f.path_hostile) {
          li.append(badge(this.t("hostile-name")));
        }
        fallos.append(li);
      }
      caja.append(fallos);
    }
    if (sync.confirming !== null) {
      // La SEGUNDA pregunta, como alerta y con su propio elemento: es la
      // última pantalla donde todavía se puede decir que no.
      const pregunta = document.createElement("p");
      pregunta.className = "sync-confirm";
      pregunta.setAttribute("role", "alertdialog");
      pregunta.textContent = sync.confirming;
      caja.append(pregunta);
    }
    const estado = document.createElement("p");
    estado.className = "sync-status";
    estado.setAttribute("role", "status");
    estado.setAttribute("aria-live", "polite");
    estado.dataset["running"] = String(sync.running);
    estado.dataset["approvable"] = String(sync.can_approve);
    estado.dataset["cancelRequested"] = String(sync.cancel_requested);
    estado.textContent = sync.status;
    caja.append(estado);

    const pie = document.createElement("p");
    pie.className = "sync-hint";
    pie.textContent = sync.hint;
    caja.append(pie);
    this.syncRoot.replaceChildren(caja);
  }

  /** Un paso del plan: qué hace, sobre qué, y si el deshacer lo devuelve. */
  private syncStep(p: SyncStepView): HTMLElement {
    const li = document.createElement("li");
    li.className = "sync-step";
    li.id = `sync-step-${String(p.id)}`;
    li.dataset["anchor"] = p.anchor;
    const kind = document.createElement("span");
    kind.className = "sync-step-kind";
    kind.textContent = p.kind;
    const ruta = document.createElement("span");
    ruta.className = "sync-step-path";
    ruta.dataset["hostile"] = String(p.path_hostile);
    ruta.textContent = p.path;
    li.append(kind, ruta);
    if (p.anchor_label !== "") {
      // El ancla se DICE, no se deduce de un `data-anchor` que nadie lee.
      const ancla = document.createElement("span");
      ancla.className = "sync-step-anchor";
      ancla.textContent = p.anchor_label;
      li.append(ancla);
    }
    if (p.path_hostile) {
      li.append(badge(this.t("hostile-name")));
    }
    if (p.dest_path !== null) {
      // La ortografía del DESTINO en su propio elemento: la escritura cae
      // sobre ESTA, y juntarlas en una celda deja que un nombre imite a otro.
      const dest = document.createElement("span");
      dest.className = "sync-step-dest";
      dest.dataset["hostile"] = String(p.dest_path_hostile);
      dest.textContent = p.dest_path;
      li.append(dest);
      if (p.dest_path_hostile) {
        li.append(badge(this.t("hostile-name")));
      }
      if (p.twins) {
        // Las dos se rinden IGUAL: sin decirlo, el panel parece repetirse.
        const gemelas = document.createElement("span");
        gemelas.className = "sync-step-twins";
        gemelas.textContent = this.t("sync-dest-twin");
        li.append(gemelas);
      }
    }
    const undo = document.createElement("span");
    undo.className = "sync-step-undo";
    undo.textContent = p.undo;
    li.append(undo);
    if (p.reason !== "") {
      const por = document.createElement("span");
      por.className = "sync-step-reason";
      por.textContent = p.reason;
      li.append(por);
    }
    return li;
  }

  /** El panel de diferencias. Comparte hueco con la búsqueda: los dos son
   *  pantallas enteras y no se pintan a la vez. */
  private paintCompare(compare: CompareView | null): void {
    if (compare === null) {
      if (this.compareRoot.dataset["open"] === "true") {
        this.compareRoot.replaceChildren();
        this.compareRoot.dataset["open"] = "false";
      }
      return;
    }
    this.compareRoot.dataset["open"] = "true";
    const caja = document.createElement("section");
    caja.className = "compare";
    caja.setAttribute("role", "dialog");
    caja.setAttribute("aria-modal", "true");
    caja.setAttribute("aria-label", this.t("compare-title"));

    const cabecera = document.createElement("div");
    cabecera.className = "compare-roots";
    for (const [texto, hostil] of [
      [compare.left, compare.left_hostile],
      [compare.right, compare.right_hostile],
    ] as [string, boolean][]) {
      const raiz = document.createElement("span");
      raiz.className = "compare-root";
      raiz.dataset["hostile"] = String(hostil);
      raiz.textContent = texto;
      if (hostil) {
        raiz.append(badge(this.t("hostile-name")));
      }
      cabecera.append(raiz);
    }
    caja.append(cabecera);

    const filtros = document.createElement("div");
    filtros.className = "compare-filters";
    for (const f of compare.filters) {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "compare-filter";
      b.dataset["hidden"] = String(f.hidden);
      b.setAttribute("aria-pressed", String(!f.hidden));
      b.textContent = `${f.label} (${String(f.count)})`;
      b.addEventListener("click", () => {
        this.send({ action: "compare_toggle_filter", category: f.id });
      });
      filtros.append(b);
    }
    caja.append(filtros);

    const lista = document.createElement("ul");
    lista.className = "compare-rows";
    lista.setAttribute("role", "listbox");
    for (const r of compare.rows) {
      const fila = document.createElement("li");
      fila.className = "compare-row";
      // El id, no la posición: es la identidad de la fila y lo que el host
      // espera de vuelta.
      fila.id = `compare-row-${String(r.id)}`;
      fila.dataset["category"] = r.category;
      fila.setAttribute("role", "option");
      fila.setAttribute("aria-selected", String(compare.selected === r.id));
      fila.addEventListener("click", () => {
        this.send({ action: "compare_select_row", id: r.id });
      });
      fila.addEventListener("dblclick", () => {
        this.send({ action: "compare_activate_row", id: r.id });
      });
      fila.append(
        this.compareFace(r.left),
        veredicto(r, (k) => this.t(k)),
        this.compareFace(r.right),
      );
      if (r.paired_under !== null) {
        // Frase en su propia línea, JAMÁS pegada al nombre: lo que se pega a
        // un nombre lo puede falsificar un nombre.
        const nota = document.createElement("p");
        nota.className = "compare-paired-under";
        nota.textContent = r.paired_under;
        fila.append(nota);
      }
      lista.append(fila);
    }
    if (compare.selected !== null) {
      lista.setAttribute(
        "aria-activedescendant",
        `compare-row-${String(compare.selected)}`,
      );
    }
    caja.append(lista);

    const estado = document.createElement("p");
    estado.className = "compare-status";
    estado.setAttribute("role", "status");
    estado.setAttribute("aria-live", "polite");
    estado.dataset["running"] = String(compare.running);
    estado.textContent = compare.status;
    caja.append(estado);
    this.compareRoot.replaceChildren(caja);
  }

  /** Una cara de una fila comparada, o el hueco de un huérfano. */
  private compareFace(face: CompareFaceView | null): HTMLElement {
    const el = document.createElement("span");
    el.className = "compare-face";
    if (face === null) {
      // Vacío y DICHO: un huérfano no tiene nada de este lado, y una celda
      // en blanco sin más se lee como un fichero sin nombre.
      el.dataset["absent"] = "true";
      el.textContent = "—";
      return el;
    }
    el.dataset["dir"] = String(face.is_dir);
    el.dataset["hostile"] = String(face.hostile);
    const nombre = document.createElement("span");
    nombre.className = "compare-name";
    nombre.textContent = face.name;
    el.append(nombre);
    if (face.hostile) {
      el.append(badge(this.t("hostile-name")));
    }
    // Tamaño y fecha solo cuando se saben: vacío es AUSENCIA, no cero.
    for (const [clase, texto] of [
      ["compare-size", face.size],
      ["compare-mtime", face.mtime],
    ] as [string, string][]) {
      if (texto === "") {
        continue;
      }
      const celda = document.createElement("span");
      celda.className = clase;
      celda.textContent = texto;
      el.append(celda);
    }
    return el;
  }

  private paintSearch(search: SearchView | null): void {
    if (search === null) {
      this.searchRoot.replaceChildren();
      this.searchRoot.dataset["open"] = "false";
      return;
    }
    this.searchRoot.dataset["open"] = "true";
    const caja = document.createElement("section");
    caja.className = "search";
    caja.setAttribute("role", "dialog");
    caja.setAttribute("aria-modal", "true");
    // Una búsqueda por significado no recorre un subárbol: su alcance es el
    // índice entero, y titularla como la otra prometería lo que no hay.
    const rotulo = search.semantic
      ? this.t("search-title-semantic")
      : this.t("search-title");
    caja.setAttribute("aria-label", rotulo);

    const titulo = document.createElement("h1");
    titulo.textContent = `${rotulo} · ${search.query}`;
    caja.append(titulo);

    if (search.semantic) {
      const alcance = document.createElement("p");
      alcance.className = "search-root";
      alcance.textContent = this.t("modal-semantic-scope");
      caja.append(alcance);
    } else {
      const donde = document.createElement("p");
      donde.className = "search-root";
      donde.dataset["hostile"] = String(search.root_hostile);
      donde.textContent = search.root;
      if (search.root_hostile) {
        donde.append(badge(this.t("hostile-name")));
      }
      caja.append(donde);
    }

    const estado = document.createElement("p");
    estado.className = "search-status";
    estado.dataset["running"] = String(search.running);
    // `status` mientras corre: un lector de pantalla anuncia el avance sin
    // robarle el foco a lo que el usuario esté haciendo.
    estado.setAttribute("role", "status");
    estado.setAttribute("aria-live", "polite");
    estado.textContent = search.status;
    caja.append(estado);

    const lista = document.createElement("ul");
    lista.className = "search-rows";
    lista.setAttribute("role", "listbox");
    for (const [i, r] of search.rows.entries()) {
      const fila = document.createElement("li");
      fila.className = "search-row";
      fila.id = `search-row-${String(i)}`;
      fila.setAttribute("role", "option");
      fila.setAttribute("aria-selected", String(search.cursor === i));
      fila.dataset["dir"] = String(r.is_dir);
      fila.addEventListener("click", () => {
        this.send({ action: "search_activate_row", row: i });
      });
      const nombre = document.createElement("span");
      nombre.className = "search-name";
      nombre.dataset["hostile"] = String(r.hostile);
      nombre.textContent = r.name;
      if (r.hostile) {
        nombre.append(badge(this.t("hostile-name")));
      }
      const padre = document.createElement("span");
      padre.className = "search-parent";
      padre.dataset["hostile"] = String(r.parent_hostile);
      padre.textContent = r.parent;
      fila.append(nombre, padre);
      if (r.score !== null) {
        // El parecido, en su propia celda: sin él, un 0,91 y un 0,42 se leen
        // igual de buenos y el orden parece arbitrario. Dos decimales, que es
        // lo que distingue sin fingir precisión.
        const parecido = document.createElement("span");
        parecido.className = "search-score";
        parecido.textContent = r.score.toFixed(2);
        fila.append(parecido);
      }
      lista.append(fila);
    }
    if (search.cursor !== null) {
      lista.setAttribute("aria-activedescendant", `search-row-${String(search.cursor)}`);
    }
    caja.append(lista);
    this.searchRoot.replaceChildren(caja);
    if (search.cursor !== null) {
      revelar(lista.querySelector(`#search-row-${String(search.cursor)}`) ?? undefined);
    }
  }

  /**
   * El selector de disposiciones, con la FORMA de la elegida al lado.
   *
   * La miniatura llega como líneas de texto pintadas por el mismo motor que
   * reparte la pantalla de verdad, así que no puede mentir sobre lo que va a
   * salir. Aquí solo se pone en un `<pre>`.
   */
  /**
   * El selector de COLUMNAS: qué se pinta, en qué orden y con qué formato.
   *
   * Dice en su título el ALCANCE —un esquema o todos— y en su pie que lo
   * elegido vale para ESTA ventana y no se guarda: esta fase no escribe
   * configuración, y callarlo dejaría al usuario creyendo que acaba de
   * configurar norte.
   */
  private paintColumns(columns: ColumnsPickerView | null): void {
    if (columns === null) {
      this.columnsRoot.replaceChildren();
      this.columnsRoot.dataset["open"] = "false";
      return;
    }
    this.columnsRoot.dataset["open"] = "true";
    const caja = document.createElement("section");
    caja.className = "columns-picker";
    caja.setAttribute("role", "dialog");
    caja.setAttribute("aria-modal", "true");
    caja.setAttribute("aria-label", columns.title);

    const titulo = document.createElement("h1");
    titulo.textContent = columns.title;
    caja.append(titulo);

    const lista = document.createElement("ul");
    lista.className = "columns-rows";
    lista.setAttribute("role", "listbox");
    for (const [i, r] of columns.rows.entries()) {
      const fila = document.createElement("li");
      fila.className = "columns-row";
      fila.id = `columns-row-${String(i)}`;
      fila.setAttribute("role", "option");
      fila.setAttribute("aria-selected", String(columns.cursor === i));
      // Encendida o no, y si se puede tocar: las dos cosas al lector de
      // pantalla, no solo al que mira.
      fila.setAttribute("aria-checked", String(r.enabled));
      fila.dataset["enabled"] = String(r.enabled);
      fila.dataset["fixed"] = String(r.fixed);

      const marca = document.createElement("span");
      marca.className = "columns-check";
      marca.textContent = r.enabled ? "☑" : "☐";
      const nombre = document.createElement("span");
      nombre.className = "columns-label";
      nombre.dataset["hostile"] = String(r.hostile);
      nombre.textContent = r.label;
      if (r.hostile) {
        nombre.append(badge(this.t("hostile-name")));
      }
      fila.append(marca, nombre);
      if (r.format !== "") {
        // El formato vigente. Bloqueado = lo fija un ajuste del esquema y
        // aquí no se cicla; se pinta apagado en vez de desaparecer, porque
        // una tecla que no hace nada y no dice por qué es peor.
        const fmt = document.createElement("span");
        fmt.className = "columns-format";
        fmt.dataset["locked"] = String(r.format_locked);
        fmt.textContent = r.format;
        fila.append(fmt);
      }
      lista.append(fila);
    }
    if (columns.cursor < columns.rows.length) {
      lista.setAttribute(
        "aria-activedescendant",
        `columns-row-${String(columns.cursor)}`,
      );
    }
    caja.append(lista);

    const nota = document.createElement("p");
    nota.className = "columns-note";
    nota.setAttribute("role", "note");
    nota.textContent = columns.note;
    caja.append(nota);

    const pie = document.createElement("footer");
    pie.className = "columns-hint";
    // Del HOST (#287): los verbos `dialog.*` se pueden reatar, y una cadena
    // de aquí que nombre teclas concretas deja de ser cierta en cuanto
    // alguien lo hace.
    pie.textContent = columns.hint;
    caja.append(pie);
    this.columnsRoot.replaceChildren(caja);
  }

  /**
   * El selector de PERFILES (ADR 0079).
   *
   * Una fila que no se puede cargar se ENSEÑA con su motivo en vez de
   * desaparecer: esconder un directorio que el lector creó es peor que
   * enseñarlo roto. Y los dos avisos que la spec pide por su nombre —qué
   * otra cosa se llama igual, y qué perfil no puede guardar estado— van en
   * la fila, no en una nota al pie que nadie asocia.
   */
  private paintProfiles(profiles: ProfilePickerView | null): void {
    if (profiles === null) {
      this.profilesRoot.replaceChildren();
      this.profilesRoot.dataset["open"] = "false";
      return;
    }
    this.profilesRoot.dataset["open"] = "true";
    const caja = document.createElement("section");
    caja.className = "profiles";
    caja.setAttribute("role", "dialog");
    caja.setAttribute("aria-modal", "true");
    caja.setAttribute("aria-label", this.t("profile-picker-title"));

    const titulo = document.createElement("h1");
    titulo.textContent = this.t("profile-picker-title");
    caja.append(titulo);

    const lista = document.createElement("ul");
    lista.className = "profiles-rows";
    lista.setAttribute("role", "listbox");
    for (const [i, r] of profiles.rows.entries()) {
      const fila = document.createElement("li");
      fila.className = "profiles-row";
      fila.id = `profile-row-${String(i)}`;
      fila.setAttribute("role", "option");
      fila.setAttribute("aria-selected", String(profiles.cursor === i));
      fila.dataset["active"] = String(r.active);
      fila.dataset["broken"] = String(r.problem !== "");
      fila.addEventListener("click", () => {
        this.send({
          action: "profile_activate_row",
          row: i,
          generation: profiles.generation,
        });
      });
      const nombre = document.createElement("span");
      nombre.className = "profiles-name";
      nombre.textContent = r.name;
      if (r.name_hostile) {
        // El nombre son bytes de un directorio: si se enmascaró, se dice.
        nombre.append(badge(this.t("hostile-name")));
      }
      fila.append(nombre);
      if (r.title !== null) {
        const t = document.createElement("span");
        t.className = "profiles-title";
        t.textContent = r.title;
        fila.append(t);
      }
      // El orden de las notas es el del terminal: primero por qué NO carga,
      // luego lo que no podrá guardar, y al final el choque de nombre. De
      // más grave a menos.
      const nota =
        r.problem !== ""
          ? r.problem
          : r.no_state
            ? this.t("profile-picker-no-state")
            : r.clash;
      if (nota !== "") {
        const n = document.createElement("span");
        n.className = "profiles-note";
        n.textContent = nota;
        fila.append(n);
      }
      lista.append(fila);
    }
    lista.setAttribute("aria-activedescendant", `profile-row-${String(profiles.cursor)}`);
    if (profiles.rows.length === 0) {
      const vacio = document.createElement("li");
      vacio.className = "empty";
      vacio.textContent = this.t("profile-picker-empty");
      lista.append(vacio);
    }
    caja.append(lista);
    this.profilesRoot.replaceChildren(caja);
  }

  private paintLayouts(layouts: LayoutPickerView | null): void {
    if (layouts === null) {
      this.layoutsRoot.replaceChildren();
      this.layoutsRoot.dataset["open"] = "false";
      return;
    }
    this.layoutsRoot.dataset["open"] = "true";
    const caja = document.createElement("section");
    caja.className = "layouts";
    caja.setAttribute("role", "dialog");
    caja.setAttribute("aria-modal", "true");
    caja.setAttribute("aria-label", layouts.title);

    const titulo = document.createElement("h1");
    titulo.textContent = layouts.title;
    caja.append(titulo);

    const cuerpo = document.createElement("div");
    cuerpo.className = "layouts-body";
    const lista = document.createElement("ul");
    lista.className = "layouts-rows";
    lista.setAttribute("role", "listbox");
    for (const [i, r] of layouts.rows.entries()) {
      const fila = document.createElement("li");
      fila.className = "layouts-row";
      fila.id = `layout-row-${String(i)}`;
      fila.setAttribute("role", "option");
      fila.setAttribute("aria-selected", String(layouts.cursor === i));
      fila.dataset["broken"] = String(r.broken);
      fila.addEventListener("click", () => {
        this.send({ action: "layout_activate_row", row: i });
      });
      const nombre = document.createElement("span");
      nombre.className = "layouts-name";
      nombre.dataset["hostile"] = String(r.hostile);
      nombre.textContent = r.name;
      if (r.hostile) {
        nombre.append(badge(this.t("hostile-name")));
      }
      fila.append(nombre);
      if (r.factory) {
        const marca = document.createElement("span");
        marca.className = "layouts-tag";
        marca.textContent = this.t("layout-picker-factory");
        fila.append(marca);
      }
      if (r.shares_keymap_name) {
        // Se AVISA: elegir esta disposición no cambia ni una tecla, y sin la
        // línea la coincidencia de nombre es una trampa.
        const aviso = document.createElement("span");
        aviso.className = "layouts-warn";
        aviso.textContent = this.t("layout-picker-shares-keymap");
        fila.append(aviso);
      }
      lista.append(fila);
    }
    lista.setAttribute("aria-activedescendant", `layout-row-${String(layouts.cursor)}`);
    cuerpo.append(lista);

    if (layouts.problem === "") {
      const vista = document.createElement("pre");
      vista.className = "layouts-preview";
      vista.setAttribute("aria-hidden", "true");
      vista.textContent = layouts.preview.join("\n");
      cuerpo.append(vista);
    } else {
      const roto = document.createElement("p");
      roto.className = "layouts-problem";
      roto.textContent = layouts.problem;
      roto.dataset["hostile"] = String(layouts.problem_hostile);
      if (layouts.problem_hostile) {
        roto.classList.add("hostile");
        roto.append(badge(this.t("hostile-name")));
      }
      cuerpo.append(roto);
    }
    caja.append(cuerpo);
    this.layoutsRoot.replaceChildren(caja);
    revelar(lista.querySelector(`#layout-row-${String(layouts.cursor)}`) ?? undefined);
  }

  /** El selector de volúmenes. */
  private paintPicker(picker: PickerView | null): void {
    if (picker === null) {
      this.pickerRoot.replaceChildren();
      this.pickerRoot.dataset["open"] = "false";
      return;
    }
    this.pickerRoot.dataset["open"] = "true";
    const caja = document.createElement("section");
    caja.className = "picker";
    caja.setAttribute("role", "dialog");
    caja.setAttribute("aria-modal", "true");
    caja.setAttribute("aria-label", picker.title);

    const titulo = document.createElement("h1");
    titulo.textContent = picker.title;
    caja.append(titulo);

    if (picker.empty !== "") {
      // La frase la escribe el host: distingue «todavía preguntando» de «no
      // hay ninguno», que es la distinción que una lista vacía se come.
      const vacio = document.createElement("p");
      vacio.className = "picker-empty";
      vacio.setAttribute("role", "status");
      vacio.textContent = picker.empty;
      caja.append(vacio);
    }

    const lista = document.createElement("ul");
    lista.className = "picker-rows";
    lista.setAttribute("role", "listbox");
    for (const [i, r] of picker.rows.entries()) {
      const fila = document.createElement("li");
      fila.className = "picker-row";
      fila.id = `picker-row-${String(i)}`;
      fila.setAttribute("role", "option");
      fila.setAttribute("aria-selected", String(picker.cursor === i));
      fila.addEventListener("click", () => {
        // La generación de ESTA pintada: si la lista cambió entre el
        // pintado y el click, el host lo rechaza en vez de elegir otra fila.
        this.send({
          action: "picker_select_row",
          row: i,
          generation: picker.generation,
        });
      });
      const label = document.createElement("span");
      label.className = "picker-label";
      label.dataset["hostile"] = String(r.hostile);
      label.textContent = r.label;
      if (r.hostile) {
        label.append(badge(this.t("hostile-name")));
      }
      const detalle = document.createElement("span");
      detalle.className = "picker-detail";
      detalle.textContent = r.detail;
      fila.append(label, detalle);
      lista.append(fila);
    }
    if (picker.cursor !== null) {
      lista.setAttribute("aria-activedescendant", `picker-row-${String(picker.cursor)}`);
      revelar(lista.querySelector(`#picker-row-${String(picker.cursor)}`) ?? undefined);
    }
    caja.append(lista);
    this.pickerRoot.replaceChildren(caja);
  }

  /** El `<li>` de una fila de ajustes, con su cursor y su click. */
  private settingsRow(i: number, cursor: number): HTMLElement {
    const fila = document.createElement("li");
    fila.className = "settings-row";
    fila.id = `settings-row-${String(i)}`;
    fila.setAttribute("role", "option");
    fila.setAttribute("aria-selected", String(cursor === i));
    fila.addEventListener("click", () => {
      this.send({ action: "settings_select_row", row: i });
    });
    return fila;
  }

  /** El visor tapa la pantalla mientras está abierto. */
  private paintViewer(viewer: ViewerView | null): void {
    if (viewer === null) {
      this.viewerRoot.replaceChildren();
      this.viewerRoot.dataset["open"] = "false";
      this.viewerRows = 0;
      // El visor se cerró: se SUELTA el búfer. Un object URL sin revocar
      // retiene sus bytes mientras viva el documento.
      this.soltarImagen();
      return;
    }
    this.viewerRoot.dataset["open"] = "true";
    const box = document.createElement("section");
    box.className = "viewer";
    box.setAttribute("role", "document");
    box.setAttribute("aria-label", viewer.path_display);

    const head = document.createElement("header");
    head.className = "viewer-head";
    // La ruta en su propio nodo, como en la cabecera de un hueco y por el
    // mismo motivo: suelta como texto es un item de flex anónimo que no se
    // encoge, así que empujaba fuera de la vista lo que viniera detrás —el
    // «via …» y el aviso de decodificación con pérdida— y salían cortados.
    const ruta = document.createElement("span");
    ruta.className = "viewer-path";
    ruta.textContent = viewer.path_display;
    head.append(ruta);
    if (viewer.path_hostile) {
      ruta.append(badge(this.t("hostile-name")));
    }
    const meta = document.createElement("span");
    meta.className = "viewer-meta";
    // Cada marca es un DATO que el host resolvió: encoding, fin de línea, si
    // lo forzó el usuario, si la decodificación tuvo errores, si el fichero
    // seguía. Ninguna se calcula aquí.
    const marcas = [viewer.encoding, viewer.eol];
    if (viewer.hex) {
      marcas.push("hex");
    }
    if (viewer.forced) {
      marcas.push(this.t("viewer-forced"));
    }
    if (viewer.had_errors) {
      // `viewer-lossy`, que es como se llama esta marca en el catálogo desde
      // que existe el visor del TUI: inventar `viewer-errors` fue pedir una
      // clave que no está, y `t` contesta con la clave misma.
      marcas.push(this.t("viewer-lossy"));
    }
    if (viewer.truncated) {
      marcas.push(this.t("viewer-truncated"));
    }
    meta.textContent = marcas.join(" · ");
    head.append(meta);
    if (viewer.preview_by !== "") {
      // Lo que se enseña lo produjo un PLUGIN. En su propio nodo y con su
      // propio color: un previewer puede enseñar cualquier cosa —ese es su
      // trabajo— y quien mira tiene derecho a saber que no está viendo los
      // bytes del fichero.
      const via = document.createElement("span");
      via.className = "viewer-via";
      via.textContent = viewer.preview_by;
      head.append(via);
      if (viewer.preview_lossy) {
        // La decodificación que se le DIO al previewer fue con pérdida: los
        // `?` de su salida vienen de ahí y no del fichero. Aparte de
        // `had_errors`, que es el de la vista cruda: son dos decodificaciones
        // y confundirlas culpa al fichero de lo que hizo la lectura.
        const aviso = document.createElement("span");
        aviso.className = "viewer-via-lossy";
        aviso.textContent = this.t("viewer-plugin-preview-lossy");
        head.append(aviso);
      }
    }

    if (viewer.image_refused !== "") {
      // Se reconoció una imagen y esta ventana se NIEGA a pintarla. Se dice,
      // en vez de caer en silencio al hexview: un fichero que el usuario
      // sabe que es una foto y que aparece como bytes sin una palabra parece
      // norte roto, no norte prudente.
      const no = document.createElement("p");
      no.className = "viewer-image-refused";
      no.setAttribute("role", "status");
      no.textContent = viewer.image_refused;
      head.append(no);
    }

    const body = viewerBody(viewer);
    body.setAttribute("tabindex", "-1");
    body.setAttribute("aria-describedby", `viewer-meta-${String(viewer.first_line)}`);

    box.append(head, body);
    if (viewer.image !== null) {
      // Los bytes NO vienen en la foto: se piden aparte y se pintan cuando
      // llegan. Hasta entonces se ve la vista cruda, que es lo honesto —el
      // fichero es ese— en vez de un hueco vacío.
      this.pintarImagen(viewer, box, body);
    }
    this.viewerRoot.replaceChildren(box);
    // Cuántas líneas caben lo sabe QUIEN PINTA. El host lo estimaba con
    // celdas de disposición menos un cromo adivinado, así que mandaba más
    // líneas de las que se ven —se recortaban sin decirlo— y avanzaba una
    // página por un número distinto: cada página saltaba lo recortado.
    const filas = Math.max(1, Math.floor(body.clientHeight / this.cell().h));
    if (filas !== this.viewerRows) {
      this.viewerRows = filas;
      this.send({ action: "set_viewer_rows", rows: filas });
    }
  }

  /** Revoca el `blob:` vivo, si lo hay. Idempotente. */
  private soltarImagen(): void {
    if (this.imagenUrl !== null) {
      URL.revokeObjectURL(this.imagenUrl);
      this.imagenUrl = null;
    }
    this.imagenDe = null;
  }

  /**
   * Pide los bytes de la imagen y la pinta cuando llegan.
   *
   * Una vez por fichero: la clave es la ruta MÁS lo que la cabecera declara,
   * así que reabrir el mismo fichero tras cambiarlo vuelve a pedirlo pero un
   * repintado cualquiera no.
   *
   * Los bytes ya vienen validados por el host —formato por bytes mágicos,
   * dimensiones declaradas contra el presupuesto, tamaño— así que aquí no se
   * decide nada: se envuelve y se pinta (ADR 0069).
   */
  private pintarImagen(viewer: ViewerView, box: HTMLElement, body: HTMLElement): void {
    const img = viewer.image;
    if (img === null) {
      return;
    }
    const clave = `${viewer.path_display}|${img.format}|${String(img.width)}x${String(img.height)}`;
    if (this.imagenDe === clave && this.imagenUrl !== null) {
      // Ya está pedida —o pintada— y es la misma: no se vuelve a pedir.
      box.replaceChildren(box.firstChild ?? body, this.nodoImagen(this.imagenUrl, img));
      return;
    }
    this.soltarImagen();
    this.imagenDe = clave;
    void this.fetchImage()
      .then((bytes) => {
        // Mientras volaba, el visor pudo cambiar o cerrarse. Pintar la foto
        // anterior sobre el fichero de ahora es la misma clase de error que
        // abrir un visor que nadie pidió.
        if (this.imagenDe !== clave || bytes.byteLength === 0) {
          return;
        }
        const url = URL.createObjectURL(new Blob([bytes]));
        this.imagenUrl = url;
        body.replaceWith(this.nodoImagen(url, img));
      })
      .catch(() => {
        // Sin imagen se queda la vista cruda, que es el fichero de verdad.
        this.imagenDe = null;
      });
  }

  /** El `<img>` con su tamaño declarado, para que no salte al cargar. */
  private nodoImagen(
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
  private buildHandles(view: ViewSnapshot, cell: { w: number; h: number }): void {
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
          const cells = vertical
            ? Math.round(e.clientX / cell.w)
            : Math.round(e.clientY / cell.h);
          this.send({ action: "resize_slot", slot_id: slot, cells });
        });
        this.root.append(el);
      }
    }
  }

  private rebuild(view: ViewSnapshot, cell: { w: number; h: number }): void {
    this.root.replaceChildren();
    this.slots.clear();
    this.buildHandles(view, cell);
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
      el.append(tabs, title, header, scroller);
      this.root.append(el);
      const dom: SlotDom = {
        root: el,
        tabs,
        title,
        header,
        scroller,
        canvas,
        rows: new Map(),
        lastRange: null,
        generation: 0,
      };
      this.slots.set(p.slot_id, dom);
      this.wire(p.slot_id, dom);
    }
  }

  private wire(slotId: number, dom: SlotDom): void {
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

  private cursorOf(slotId: number): number | null {
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
  private scheduleRange(slotId: number, dom: SlotDom): void {
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

  /**
   * La barra de PESTAÑAS de un hueco, si está en un grupo.
   *
   * Se pinta aunque solo se vea el contenido de una: lo que hay detrás sigue
   * abierto, y una ventana que no lo dice esconde trabajo. El rótulo llega ya
   * enmascarado del host —un directorio hostil dentro de una pestaña es tan
   * hostil como dentro de un listado— con su bandera al lado.
   */
  private paintTabs(dom: SlotDom, grupo: TabGroupView | undefined): void {
    if (grupo === undefined) {
      if (dom.tabs.dataset["open"] === "true") {
        dom.tabs.replaceChildren();
        dom.tabs.dataset["open"] = "false";
      }
      return;
    }
    dom.tabs.dataset["open"] = "true";
    const lista = document.createElement("ul");
    lista.className = "tabs";
    lista.setAttribute("role", "tablist");
    for (const [i, t] of grupo.tabs.entries()) {
      const li = document.createElement("li");
      li.className = "tab";
      li.setAttribute("role", "tab");
      li.setAttribute("aria-selected", String(i === grupo.active));
      li.dataset["active"] = String(i === grupo.active);
      li.dataset["hostile"] = String(t.title_hostile);
      li.textContent = t.title;
      if (t.title_hostile) {
        li.append(badge(this.t("hostile-name")));
      }
      li.addEventListener("click", () => {
        // Por SLOT y no por posición: la lista puede haberse movido entre el
        // pintado y el clic, y el host rehúsa un hueco que ya no está en
        // ningún grupo en vez de acertar por casualidad.
        this.send({ action: "select_tab", slot_id: t.slot_id });
      });
      lista.append(li);
    }
    dom.tabs.replaceChildren(lista);
  }

  private paintSlot(
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

  /**
   * El árbol de directorios.
   *
   * Dos gestos distintos sobre la misma fila: el TRIÁNGULO pliega y despliega,
   * y el nombre NAVEGA. Un solo gesto obligaría a elegir cuál de las dos cosas
   * significa un click, y las dos hacen falta — mirar dentro de una rama sin
   * mover el listado es la mitad de para qué sirve un árbol.
   *
   * El árbol no se mueve al navegar: es lo que hace útil tenerlo abierto.
   */
  private paintTree(dom: SlotDom, slot: TreeSlotView): void {
    dom.root.setAttribute("aria-label", this.t("tree-title"));
    dom.title.textContent = this.t("tree-title");
    dom.scroller.className = "tree";
    const lista = document.createElement("ul");
    lista.className = "tree-rows";
    lista.setAttribute("role", "tree");
    for (const [i, r] of slot.rows.entries()) {
      const fila = document.createElement("li");
      fila.className = "tree-row";
      fila.id = `tree-row-${String(i)}`;
      fila.setAttribute("role", "treeitem");
      fila.setAttribute("aria-level", String(r.depth + 1));
      fila.setAttribute("aria-selected", String(slot.cursor === i));
      // La sangría, por variable: el CSS no puede multiplicar una profundidad
      // que solo existe en los datos.
      fila.style.setProperty("--depth", String(r.depth));
      const marca = document.createElement("span");
      marca.className = "tree-twisty";
      if (r.children === false) {
        // Una hoja no lleva triángulo, pero SÍ su hueco: sin él los nombres
        // de un mismo nivel no se alinean y el árbol deja de leerse como tal.
        marca.textContent = " ";
      } else {
        // `null` —todavía no se ha mirado— se pinta como plegada y no como
        // hoja: pintar «no tiene nada dentro» a algo que nadie ha leído es
        // una respuesta inventada.
        marca.textContent = r.expanded ? "▾" : "▸";
        fila.setAttribute("aria-expanded", String(r.expanded));
        marca.addEventListener("click", (ev) => {
          // Que no llegue al nombre: plegar no navega.
          ev.stopPropagation();
          this.send({
            action: "tree_toggle_row",
            row: i,
            generation: slot.generation,
          });
        });
      }
      const nombre = document.createElement("span");
      nombre.className = "tree-name";
      nombre.dataset["hostile"] = String(r.hostile);
      nombre.textContent = r.label;
      if (r.hostile) {
        nombre.append(badge(this.t("hostile-name")));
      }
      fila.addEventListener("click", () => {
        // La generación de ESTA pintada: los hijos de una rama llegan solos y
        // se insertan EN MEDIO, así que sin ella un click podía navegar a una
        // carpeta que nadie pulsó.
        this.send({
          action: "tree_activate_row",
          row: i,
          generation: slot.generation,
        });
      });
      fila.append(marca, nombre);
      lista.append(fila);
    }
    lista.setAttribute("aria-activedescendant", `tree-row-${String(slot.cursor)}`);
    dom.scroller.replaceChildren(lista);
    revelar(lista.querySelector(`#tree-row-${String(slot.cursor)}`) ?? undefined);
  }

  /**
   * La barra lateral de sitios: volúmenes y favoritos.
   *
   * Un click ELIGE Y ACTIVA, al contrario que las otras listas: una barra
   * lateral existe para ir a sitios, y un click que solo mueve un cursor
   * obliga a rematar con el teclado. Una cabecera pliega en vez de navegar,
   * que es lo que el host hace con ella.
   */
  private paintPlaces(dom: SlotDom, slot: PlacesSlotView): void {
    dom.root.setAttribute("aria-label", this.t("places-title"));
    dom.title.textContent = this.t("places-title");
    dom.scroller.className = "places";
    const lista = document.createElement("ul");
    lista.className = "places-rows";
    lista.setAttribute("role", "listbox");
    for (const [i, r] of slot.rows.entries()) {
      const fila = document.createElement("li");
      fila.className = "places-row";
      fila.id = `place-row-${String(i)}`;
      fila.dataset["row"] = r.row;
      fila.setAttribute("role", "option");
      fila.setAttribute("aria-selected", String(slot.cursor === i));
      fila.addEventListener("click", () => {
        // La generación de ESTA pintada. Los volúmenes llegan solos y se
        // insertan antes que los favoritos, así que sin ella un click podía
        // navegar a un sitio que nadie pulsó.
        this.send({
          action: "place_activate_row",
          row: i,
          generation: slot.generation,
        });
      });
      if (r.row === "header") {
        fila.setAttribute("aria-expanded", String(!r.folded));
        const marca = document.createElement("span");
        marca.className = "places-fold";
        marca.textContent = r.folded ? "▸" : "▾";
        const texto = document.createElement("span");
        texto.className = "places-header";
        texto.textContent = r.label;
        fila.append(marca, texto);
      } else if (r.row === "drive") {
        const nombre = document.createElement("span");
        nombre.className = "places-name";
        nombre.dataset["hostile"] = String(r.hostile);
        nombre.textContent = r.label;
        if (r.hostile) {
          nombre.append(badge(this.t("hostile-name")));
        }
        const detalle = document.createElement("span");
        detalle.className = "places-detail";
        detalle.textContent = r.detail;
        fila.append(nombre, detalle);
      } else {
        const nombre = document.createElement("span");
        nombre.className = "places-name";
        nombre.textContent = r.name;
        fila.append(nombre);
        if (r.broken === "") {
          const destino = document.createElement("span");
          destino.className = "places-detail";
          destino.dataset["hostile"] = String(r.hostile);
          destino.textContent = r.target;
          if (r.hostile) {
            destino.append(badge(this.t("hostile-name")));
          }
          fila.append(destino);
        } else {
          // Un favorito roto se PINTA con su motivo: uno que desaparece en
          // silencio es un fallo de configuración que nadie puede ver.
          const roto = document.createElement("span");
          roto.className = "places-broken";
          roto.textContent = r.broken;
          fila.append(roto);
        }
      }
      lista.append(fila);
    }
    lista.setAttribute("aria-activedescendant", `place-row-${String(slot.cursor)}`);
    dom.scroller.replaceChildren(lista);
    revelar(lista.querySelector(`#place-row-${String(slot.cursor)}`) ?? undefined);
  }

  /**
   * La hoja de atributos: etiqueta y valor, y nada más.
   *
   * Todo llega formateado y saneado del host — el tamaño con su forma humana
   * y su número exacto, la fecha en ISO, cada atributo por la misma puerta
   * que su columna. Aquí no se formatea nada.
   */
  private paintMetadata(dom: SlotDom, slot: MetadataSlotView): void {
    dom.root.setAttribute("aria-label", this.t("metadata-title"));
    dom.title.textContent = this.t("metadata-title");
    dom.scroller.className = "metadata";
    if (slot.note !== "") {
      dom.scroller.replaceChildren(nota(slot.note));
      return;
    }
    const lista = document.createElement("dl");
    lista.className = "metadata-fields";
    for (const f of slot.fields) {
      const dt = document.createElement("dt");
      dt.textContent = f.label;
      const dd = document.createElement("dd");
      dd.dataset["hostile"] = String(f.hostile);
      dd.textContent = f.value;
      if (f.hostile) {
        dd.append(badge(this.t("hostile-name")));
      }
      lista.append(dt, dd);
    }
    dom.scroller.replaceChildren(lista);
  }

  /**
   * El visor acoplado (#291): el mismo cuerpo que el visor a pantalla
   * completa —es el mismo visor en otro sitio, como en la TUI— con la ruta
   * de título y, si no hay fichero, la nota que dice por qué.
   *
   * Viene la VENTANA de líneas que cabe en el hueco, desde donde el host
   * tiene desplazado el visor: la rueda y las teclas del visor lo mueven por
   * él, igual que el grande.
   */
  private paintPreview(dom: SlotDom, slot: PreviewSlotView): void {
    const titulo =
      slot.viewer === null ? this.t("panelbar-viewer") : slot.viewer.path_display;
    dom.root.setAttribute("aria-label", titulo);
    dom.title.textContent = titulo;
    if (slot.viewer?.path_hostile === true) {
      dom.title.append(badge(this.t("hostile-name")));
    }
    dom.scroller.className = "preview";
    if (slot.viewer === null) {
      dom.scroller.onwheel = null;
      dom.scroller.replaceChildren(nota(slot.note));
      return;
    }
    // La rueda desplaza por el HOST, como el registro: la ventana visible
    // la decide él, y las teclas del visor —con el hueco enfocado— mueven el
    // mismo desplazamiento.
    dom.scroller.onwheel = (e) => {
      e.preventDefault();
      this.send({
        action: "preview_scroll",
        slot_id: slot.slot_id,
        delta: e.deltaY > 0 ? 3 : -3,
      });
    };
    const via = slot.viewer.preview_by;
    const cabecera: HTMLElement[] = [];
    if (via !== "") {
      // Lo que se enseña lo produjo un PLUGIN: se dice, como en el grande.
      const marca = document.createElement("span");
      marca.className = "viewer-via";
      marca.textContent = via;
      cabecera.push(marca);
    }
    dom.scroller.replaceChildren(...cabecera, viewerBody(slot.viewer));
  }

  /**
   * El panel de procesos: las MISMAS tareas de la franja, con su cursor.
   *
   * No hay una segunda lista: dos listas de tareas se separan, y la que se ve
   * deja de ser la que se cancela.
   */
  private paintProcesses(
    dom: SlotDom,
    slot: ProcessesSlotView,
    view: ViewSnapshot,
  ): void {
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

  /**
   * El panel de registro (#326): lo que este proceso está registrando.
   *
   * La cabecera lleva tres cosas que el panel no puede callar. El NIVEL y el
   * FILTRO, porque un panel que se ve vacío con un filtro puesto se lee como
   * un panel roto. Si está pegado al final, porque «no pasa nada» y «te has
   * despegado y esto es historia» son indistinguibles sin decirlo. Y de qué
   * PROCESO son las líneas: la ventana arranca su propio daemon, así que aquí
   * NO está lo del daemon —los providers, el journal, la política—, y quien lo
   * abra buscando el motivo de una conexión fallida no lo va a encontrar.
   *
   * Las líneas tiradas por el anillo también se dicen: un registro con un
   * agujero silencioso miente sobre lo que pasó, porque la ausencia de una
   * línea es indistinguible de que el evento no ocurriera.
   */
  private paintLog(dom: SlotDom, slot: LogSlotView): void {
    dom.root.setAttribute("aria-label", this.t("log-title"));
    dom.scroller.className = "log";
    dom.title.replaceChildren(
      document.createTextNode(this.t("log-title")),
      chip(`${this.t("log-level")}: ${slot.level}`),
      ...(slot.filter === "" ? [] : [chip(`/${slot.filter}`)]),
      ...(slot.following ? [] : [chip(this.t("log-detached"))]),
      // Se está guardando MÁS de lo que se ve: quien mira tiene derecho a
      // saberlo, sobre todo antes de hacer una captura de pantalla.
      ...(slot.capturing === "" ? [] : [chip(slot.capturing)]),
      ...(slot.dropped_note === "" ? [] : [chip(slot.dropped_note)]),
      // La fuente. Con un daemon que sirve su registro es un SELECTOR —una
      // pulsación recorre ventana, daemon y los dos—; sin él es una etiqueta,
      // porque un mando entre tres vistas de un mismo anillo promete algo que
      // no existe. El host ya colapsa `both` a `window` en ese caso, así que
      // aquí solo hay que decidir si se puede pulsar.
      slot.sources_available ? this.selectorDeFuente(slot) : chip(slot.source),
      // Y lo que haya que decir de ella: que el daemon no sirve su registro,
      // o de quién es el nivel que se está enseñando.
      ...(slot.source_note === "" ? [] : [chip(slot.source_note)]),
    );
    // El bloque de mandos se REUSA mientras siga siendo el mismo hueco. Se
    // creaba en cada repintado, y como cada tecla del filtro provoca una foto
    // —o sea un repintado—, el campo se destruía con el primer carácter y se
    // perdían el foco y el caret. Es el mismo fallo que el campo de un diálogo
    // ya tuvo, y la misma cura: conservar el nodo.
    let mandos = this.logControles;
    if (mandos === null || this.logPintado !== slot.slot_id) {
      mandos = this.crearControlesDeRegistro();
      this.logControles = mandos;
      this.logPintado = slot.slot_id;
    }
    for (const b of mandos.querySelectorAll("button[data-level]")) {
      const el = b as HTMLElement;
      el.dataset["on"] = String(el.dataset["level"] === slot.level);
    }
    const filtro = mandos.querySelector(".log-filter");
    // Solo si NO se está escribiendo en él: resembrarlo mientras tiene el foco
    // devolvería la proyección del host encima de lo que el lector teclea.
    if (filtro instanceof HTMLInputElement && document.activeElement !== filtro) {
      filtro.value = slot.filter;
    }
    const seguir = mandos.querySelector(".log-follow");
    if (seguir instanceof HTMLButtonElement) {
      seguir.disabled = slot.following;
    }

    const lista = document.createElement("ul");
    lista.className = "log-lines";
    lista.setAttribute("role", "log");
    for (const l of slot.lines) {
      const fila = document.createElement("li");
      fila.className = "log-line";
      fila.dataset["level"] = l.level;
      // De qué proceso salió. En la lista mezclada es lo que separa «el
      // provider falló» de «la ventana no pudo pintarlo», que se leen igual y
      // son dos averías distintas.
      fila.dataset["source"] = l.source;
      const hora = document.createElement("span");
      hora.className = "log-time";
      hora.textContent = l.time;
      const nivel = document.createElement("span");
      nivel.className = "log-level";
      nivel.textContent = l.level;
      const target = document.createElement("span");
      target.className = "log-target";
      target.textContent = l.target;
      const msg = document.createElement("span");
      msg.className = "log-message";
      msg.textContent = l.message;
      fila.append(hora, nivel, target, msg);
      if (l.hostile) {
        fila.append(badge(this.t("hostile-name")));
      }
      lista.append(fila);
    }
    // La rueda desplaza el registro por el HOST, no por el DOM: la ventana
    // visible la decide él, y dejar que el navegador desplace un trozo que
    // solo tiene las líneas visibles no llegaría a ninguna parte.
    dom.scroller.onwheel = (e) => {
      e.preventDefault();
      this.send({ action: "log_scroll", delta: e.deltaY > 0 ? 3 : -3 });
    };
    // Un panel vacío lo DICE. Sin esto, «no hay nada», «el filtro se lo come
    // todo» y «este proceso no tiene anillo» se pintan los tres igual: una
    // caja en blanco, que se lee como un panel roto.
    const cuerpo: HTMLElement =
      slot.lines.length === 0 ? nota(this.t("log-empty")) : lista;
    dom.scroller.replaceChildren(mandos, cuerpo);
    this.scheduleLogRows(dom);
  }

  /**
   * El selector de fuente del registro (#328).
   *
   * Se crea en cada pintado y no se reusa como el bloque de mandos: no tiene
   * estado del DOM que perder —ni foco ni caret— y su etiqueta cambia con la
   * fuente, que es justo lo que hay que repintar.
   *
   * `data-source` lleva el identificador de WIRE y no la etiqueta traducida:
   * es lo que permite comprobar cuál está puesta sin atar la prueba al idioma,
   * la misma regla que los botones de nivel.
   */
  private selectorDeFuente(slot: LogSlotView): HTMLElement {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "chip log-source";
    b.dataset["source"] = slot.source_mode;
    b.textContent = slot.source;
    b.addEventListener("click", () => {
      this.send({ action: "log_cycle_source" });
    });
    return b;
  }

  /**
   * Los mandos del registro, UNA vez por hueco.
   *
   * Aparte del pintado porque llevan estado del DOM que no se puede tirar en
   * cada foto: el foco y el caret del filtro.
   */
  private crearControlesDeRegistro(): HTMLElement {
    const mandos = document.createElement("div");
    mandos.className = "log-controls";
    // Un botón por valor del vocabulario CERRADO. Se comparan por el
    // identificador de wire y no por su etiqueta traducida: comparar frases
    // traducidas ataría el nivel al idioma.
    for (const nivel of ["error", "warn", "info", "debug", "trace"]) {
      const b = document.createElement("button");
      b.type = "button";
      b.dataset["level"] = nivel;
      b.textContent = this.t(`log-level-${nivel}`);
      b.addEventListener("click", () => {
        this.send({ action: "log_set_level", level: nivel });
      });
      mandos.append(b);
    }
    const filtro = document.createElement("input");
    filtro.type = "text";
    filtro.className = "log-filter";
    filtro.placeholder = this.t("log-filter");
    filtro.setAttribute("aria-label", this.t("log-filter"));
    filtro.addEventListener("input", () => {
      this.send({ action: "log_set_filter", filter: filtro.value });
    });
    mandos.append(filtro);
    const seguir = document.createElement("button");
    seguir.type = "button";
    seguir.className = "log-follow";
    seguir.textContent = this.t("log-follow");
    seguir.addEventListener("click", () => {
      this.send({ action: "log_follow" });
    });
    mandos.append(seguir);
    return mandos;
  }

  /**
   * Cuántas líneas caben, medidas del DOM y mandadas al host.
   *
   * El host no puede adivinarlo, y mientras nadie se lo dijo se quedó con su
   * valor de arranque —UNA fila— así que el panel enseñaba una línea recortada
   * dentro de una caja de doce, y la rueda se saltaba dos por muesca. Es la
   * misma medida que hace el listado y por el mismo motivo: la ventana visible
   * la decide quien la pinta.
   */
  private scheduleLogRows(dom: SlotDom): void {
    if (this.pendingLogRows !== null) {
      return;
    }
    this.pendingLogRows = requestAnimationFrame(() => {
      this.pendingLogRows = null;
      const { h } = this.cell();
      const cuerpo = dom.scroller.querySelector(".log-lines, .slot-note");
      const alto =
        cuerpo instanceof HTMLElement ? cuerpo.clientHeight : dom.scroller.clientHeight;
      const rows = Math.max(1, Math.floor(alto / h));
      if (this.logFilas === rows) {
        return;
      }
      this.logFilas = rows;
      this.send({ action: "log_set_visible_range", rows });
    });
  }

  private paintAux(
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
        ...statusNodes(view.status, view.connection.state, (k) => this.t(k)),
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

  private paintBrowser(
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
    if (slot.skipped_note !== "") {
      // Lo que el provider se SALTÓ, ya dicho en Rust. Va en la CABECERA y no
      // al final de la lista: lo que falta no está, así que no hay ninguna
      // fila donde el lector pueda tropezarse con ello.
      const aviso = document.createElement("span");
      aviso.className = "slot-skipped";
      aviso.setAttribute("role", "status");
      aviso.textContent = slot.skipped_note;
      dom.title.append(aviso);
    }
    if (slot.hidden_note !== "") {
      // Lo que la OCULTACIÓN aparta, por la misma razón y en el mismo sitio:
      // no hay ninguna fila donde tropezarse con lo que no se pinta.
      const ocultas = document.createElement("span");
      ocultas.className = "slot-hidden";
      ocultas.setAttribute("role", "status");
      ocultas.textContent = slot.hidden_note;
      dom.title.append(ocultas);
    }
    dom.root.setAttribute("aria-label", slot.path_display);
    dom.generation = slot.generation;

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
    for (const [i, row] of slot.rows.entries()) {
      const index = slot.first_visible + i;
      wanted.add(row.key);
      const el = dom.rows.get(row.key) ?? newRow(dom, slot.slot_id, row.key);
      updateRow(el, row, index, cell.h);
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

  /** La cabecera: etiquetas y marca de orden, ambas resueltas en Rust. */
  private paintHeader(dom: SlotDom, slot: BrowserSlotView): void {
    const nodes = slot.columns.map((c) => {
      const el = document.createElement("span");
      el.className = c.id === "name" ? "col col-name" : "col";
      el.setAttribute("role", "columnheader");
      el.dataset["column"] = c.id;
      // `aria-sort` va en la cabecera que ordena y en ninguna otra.
      el.setAttribute("aria-sort", c.sort === null ? "none" : `${c.sort}ending`);
      el.textContent = c.label;
      if (c.sort !== null) {
        const marca = document.createElement("span");
        marca.className = "sort-mark";
        marca.textContent = c.sort === "asc" ? "▲" : "▼";
        el.append(marca);
      }
      if (c.sortable) {
        el.dataset["sortable"] = "true";
        el.setAttribute("tabindex", "-1");
      }
      return el;
    });
    dom.header.replaceChildren(...nodes);
  }

  /**
   * El plan de renombrado en revisión.
   *
   * Los dos nombres de cada pareja van en ELEMENTOS distintos, jamás
   * concatenados con una flecha: un nombre puede contener la flecha, y la
   * fila se leería como otra pareja. El separador lo pone el CSS, que un
   * nombre no puede escribir.
   */
  private paintAiRename(plan: AiRenameView | null): void {
    if (plan === null) {
      this.aiRenameRoot.replaceChildren();
      this.aiRenameRoot.dataset["open"] = "false";
      return;
    }
    this.aiRenameRoot.dataset["open"] = "true";
    const caja = document.createElement("section");
    caja.className = "ai-rename";
    caja.setAttribute("role", "dialog");
    caja.setAttribute("aria-modal", "true");
    const h = document.createElement("h2");
    h.id = "ai-rename-title";
    h.textContent = this.t("modal-ai-rename-plan");
    caja.setAttribute("aria-labelledby", h.id);
    caja.append(h);

    const donde = document.createElement("p");
    donde.className = "ai-rename-dir";
    donde.textContent = plan.dir.text;
    donde.dataset["hostile"] = String(plan.dir.hostile);
    if (plan.dir.hostile) {
      donde.classList.add("hostile");
      donde.append(badge(this.t("hostile-name")));
    }
    caja.append(donde);

    // El VEREDICTO va arriba, junto al directorio: de todo el cuerpo es la
    // línea que no se puede perder si la pantalla se queda corta.
    const estado = document.createElement("p");
    estado.className = "ai-rename-status";
    estado.dataset["confirmable"] = String(plan.confirmable);
    estado.setAttribute("role", "status");
    estado.textContent = plan.status;
    caja.append(estado);

    const lista = document.createElement("ol");
    lista.className = "ai-rename-pairs";
    lista.setAttribute("start", String(plan.first_visible + 1));
    for (const par of plan.pairs) {
      const fila = document.createElement("li");
      fila.className = "ai-rename-pair";
      // Los dos nombres en LÍNEAS distintas, y la segunda con su propio
      // color. Ponerlos en la misma línea separados por una flecha los
      // separaba con un glifo que un nombre puede contener: `cap 2 → final`
      // se leía como una pareja distinta de la que es. La numeración la pinta
      // el `<ol>`, que un nombre tampoco puede falsificar.
      for (const [clase, linea] of [
        ["ai-rename-from", par.from],
        ["ai-rename-to", par.to],
      ] as const) {
        const el = document.createElement("div");
        el.className = clase;
        el.textContent = linea.text;
        el.dataset["hostile"] = String(linea.hostile);
        if (linea.hostile) {
          el.classList.add("hostile");
          el.append(badge(this.t("hostile-name")));
        }
        fila.append(el);
      }
      lista.append(fila);
    }
    caja.append(lista);

    if (plan.more_note !== "") {
      // Ya traducido y ya sustituido POR EL HOST. Sustituirlo aquí no
      // funcionaba: el catálogo lleva las cadenas ya formateadas y sin
      // argumentos, y Fluent escribe una variable ausente como `{$shown}` —
      // sin espacios—, así que el `.replace` no casaba nunca y la línea que
      // dice cuánto del plan se está viendo pintaba dos identificadores.
      const mas = document.createElement("p");
      mas.className = "ai-rename-more";
      mas.textContent = plan.more_note;
      caja.append(mas);
    }
    if (plan.hidden_hostile) {
      // Lo que se enmascara se dice TAMBIÉN cuando no cabe en la ventana: la
      // marca de una línea solo existe para esa línea, y la pareja alterada
      // puede estar en la posición doce.
      const aviso = document.createElement("p");
      aviso.className = "ai-rename-hidden-hostile hostile";
      aviso.setAttribute("role", "alert");
      aviso.textContent = this.t("modal-ai-rename-hidden-hostile");
      caja.append(aviso);
    }

    for (const linea of plan.detail) {
      const p = document.createElement("p");
      p.className = "ai-rename-detail";
      p.textContent = linea.text;
      p.dataset["hostile"] = String(linea.hostile);
      if (linea.hostile) {
        p.classList.add("hostile");
        p.append(badge(this.t("hostile-name")));
      }
      caja.append(p);
    }

    if (plan.real_steps_note !== "") {
      // Cuántos renombra DE VERDAD: el planificador tira las parejas nulas, y
      // enseñar solo las pedidas promete de más.
      const reales = document.createElement("p");
      reales.className = "ai-rename-real";
      reales.textContent = plan.real_steps_note;
      caja.append(reales);
    }

    // Botones, y no solo teclas. Un clic es un gesto DIRIGIDO a esta
    // pantalla, así que no necesita el reconocimiento que sí necesita una
    // tecla; y sin ellos un lector con el ratón no podía ni quitarse de
    // encima una pantalla que se abrió sola.
    const botones = document.createElement("div");
    botones.className = "choices";
    // Las dos claves, LITERALES: una `t(variable)` es una clave que el
    // barrido del catálogo no puede seguir, y una clave que no se sigue se
    // pinta como su propio identificador el día que falte.
    const aplicar = document.createElement("button");
    aplicar.type = "button";
    aplicar.textContent = this.t("modal-ai-rename-apply");
    aplicar.disabled = !plan.confirmable;
    aplicar.addEventListener("click", () => {
      this.send({ action: "ai_rename_decide", approve: true });
    });
    const descartar = document.createElement("button");
    descartar.type = "button";
    descartar.textContent = this.t("modal-ai-rename-discard");
    descartar.addEventListener("click", () => {
      this.send({ action: "ai_rename_decide", approve: false });
    });
    botones.append(aplicar, descartar);
    caja.append(botones);

    const pie = document.createElement("p");
    pie.className = "ai-rename-hint";
    pie.textContent = this.t("gui-modal-ai-rename-plan-hint");
    caja.append(pie);
    this.aiRenameRoot.replaceChildren(caja);
  }

  /** Un campo etiquetado de un diálogo: la etiqueta fuera de banda y el valor
   *  con su marca si lo pintado difiere de lo que hay. */
  private campoDeDialogo(etiquetaTexto: string, linea: DialogLine): HTMLElement {
    const p = document.createElement("p");
    p.className = "dialog-field";
    const etiqueta = document.createElement("span");
    etiqueta.className = "dialog-field-label";
    etiqueta.textContent = etiquetaTexto;
    const valor = document.createElement("span");
    valor.textContent = linea.text;
    valor.dataset["hostile"] = String(linea.hostile);
    p.append(etiqueta, valor);
    if (linea.hostile) {
      valor.classList.add("hostile");
      p.append(badge(this.t("hostile-name")));
    }
    return p;
  }

  private paintDialogs(dialogs: DialogView[]): void {
    if (dialogs.length === 0) {
      this.dialogsRoot.replaceChildren();
      this.dialogoPintado = null;
      this.dialogoInput = null;
      return;
    }
    const top = dialogs[dialogs.length - 1];
    if (top === undefined) {
      return;
    }
    const box = document.createElement("div");
    box.className = "dialog";
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-modal", "true");
    const h = document.createElement("h2");
    h.id = `dialog-title-${String(top.id)}`;
    h.textContent = this.t(top.title_key);
    box.setAttribute("aria-labelledby", h.id);
    box.append(h);
    if (top.destination !== null) {
      // El destino, en su propio elemento y con su etiqueta traducida. NO
      // como una línea del cuerpo con una flecha delante: un directorio puede
      // llamarse `docs → /casa/BORRAR`, esa flecha es legítima y no se
      // enmascara, así que la línea se leería como dos rutas y quien confirma
      // creería estar mandando sus ficheros a la segunda.
      const dest = document.createElement("p");
      dest.className = "dialog-destination";
      const etiqueta = document.createElement("span");
      etiqueta.className = "dialog-destination-label";
      etiqueta.textContent = this.t("dialog-destination");
      const valor = document.createElement("span");
      valor.textContent = top.destination.text;
      valor.dataset["hostile"] = String(top.destination.hostile);
      dest.append(etiqueta, valor);
      if (top.destination.hostile) {
        valor.classList.add("hostile");
        dest.append(badge(this.t("hostile-name")));
      }
      box.append(dest);
    }
    // Qué se pide y quién lo pide, cada uno etiquetado y FUERA de la lista de
    // rutas: entre líneas de rutas, un nombre de fichero que dijera lo mismo
    // sería indistinguible.
    if (top.subject !== null) {
      box.append(this.campoDeDialogo(this.t("dialog-subject"), top.subject));
    }
    if (top.asker !== null) {
      box.append(this.campoDeDialogo(this.t("dialog-asker"), top.asker));
    }
    if (top.body.length > 0) {
      // Numeradas por POSICIÓN, con una lista ordenada: la etiqueta es
      // estructural y ningún nombre de fichero puede escribirla.
      const lista = document.createElement("ol");
      lista.className = "dialog-body";
      for (const line of top.body) {
        const li = document.createElement("li");
        li.textContent = line.text;
        li.dataset["hostile"] = String(line.hostile);
        if (line.hostile) {
          // Esta es la pantalla donde se aprueba borrar, copiar o mover un
          // nombre. Un nombre que se pinta distinto de lo que es y no lo dice
          // se lee como fiel, y la aprobación es de otra cosa.
          li.classList.add("hostile");
          li.append(badge(this.t("hostile-name")));
        }
        lista.append(li);
      }
      box.append(lista);
    }
    if (top.deadline !== null) {
      // El plazo, en su propio elemento: con `ttl_ms == 0` no hay línea de
      // plazo que pintar, y entonces un fichero llamado «caduca en 3600 s»
      // sería la única que lo pareciera.
      const plazo = document.createElement("p");
      plazo.className = "dialog-deadline";
      plazo.setAttribute("role", "status");
      plazo.textContent = top.deadline;
      // Y si el host dijo CUÁNDO vence, se cuenta de verdad (#279). La frase
      // del host se calcula al abrir y se congelaba: un modal que llevaba
      // cuatro minutos delante seguía diciendo «caduca en 300 s».
      //
      // El texto lo sigue componiendo el host —aquí solo se sustituye el
      // número dentro de él— porque la frase es suya y está traducida: el
      // renderer no sabe decir «caduca en» en el idioma de esta ventana.
      const vence = top.deadline_at_ms;
      if (vence !== undefined && vence !== null) {
        const plantilla = top.deadline;
        const pintar = (): boolean => {
          const quedan = Math.max(0, Math.ceil((vence - Date.now()) / 1000));
          plazo.textContent = plantilla.replace(/\d+/, String(quedan));
          return quedan > 0;
        };
        pintar();
        const tick = window.setInterval(() => {
          // Cuando llega a cero se para solo: el diálogo lo cierra el host al
          // vencer, y seguir contando en negativo sobre algo que ya no está
          // es ruido.
          if (!pintar() || !plazo.isConnected) {
            window.clearInterval(tick);
          }
        }, 1000);
      }
      box.append(plazo);
    }
    if (top.overflow_note !== "") {
      // La lista está recortada, y decirlo es lo único que impide confirmar
      // una operación sobre doscientos ficheros creyendo que son dieciséis.
      const nota = document.createElement("p");
      nota.className = "dialog-overflow";
      nota.setAttribute("role", "alert");
      nota.textContent = top.overflow_note;
      box.append(nota);
    }
    if (top.input_hostile) {
      // Es la ÚNICA superficie donde se aprueba un nombre: si lo que se pinta
      // difiere de lo que se creará, se dice aquí.
      const aviso = document.createElement("p");
      aviso.className = "hostile";
      aviso.setAttribute("role", "alert");
      aviso.textContent = this.t("hostile-name");
      box.append(aviso);
    }
    if (top.input === null) {
      this.dialogoInput = null;
    } else {
      // El campo se REUSA mientras sea el mismo diálogo. Antes se creaba uno
      // nuevo en cada repintado y se le dejaba el valor sin poner —para no
      // devolverle la proyección del host, enmascarada y acotada, que el
      // siguiente evento habría mandado de vuelta como si fuera lo tecleado—,
      // así que el campo salía VACÍO. Y como cada tecla provoca un parche,
      // cada tecla lo vaciaba: lo que llegaba a `fs.mkdir` era el último
      // carácter. Reusar el nodo conserva de paso el cursor y la selección.
      const previo = this.dialogoPintado === top.id ? this.dialogoInput : null;
      let input = previo;
      if (input === null) {
        input = document.createElement("input");
        // #327: una contraseña se pinta como contraseña. Lo que llega en
        // `top.input` son PUNTOS —el host no manda nunca el texto—, así que
        // sembrar el campo con eso escribiría puntos literales dentro: se
        // siembra vacío, que es lo que el diálogo acaba de abrir.
        input.type = top.input_secret ? "password" : "text";
        input.value = top.input_secret ? "" : top.input;
        if (top.input_secret) {
          // `new-password` y no `off`: Chromium y WebView2 IGNORAN `off` en un
          // campo de contraseña a propósito, y este es el valor que sí
          // respetan. Esto no se guarda en ninguna parte, que es justo lo que
          // el cuerpo del diálogo promete.
          input.autocomplete = "new-password";
          input.setAttribute("autocorrect", "off");
          input.spellcheck = false;
        }
        const vivo = input;
        vivo.addEventListener("input", () => {
          // Una CONTRASEÑA no se manda al teclear (#327): el host no guarda lo
          // que se escribe, el campo lo enmascara el propio navegador, y por
          // aquí cruzarían `h`, `hu`, `hun`… — un prefijo por pulsación, cada
          // uno en un trozo de heap que nadie pisa. Cruza una vez, al
          // confirmar.
          if (top.input_secret) {
            return;
          }
          this.send({ action: "dialog_input", id: top.id, text: vivo.value });
        });
        if (top.input_secret) {
          // Enter DENTRO del campo confirma, y lleva el valor. Sin esto, la
          // tecla sale al host como un acorde `dialog.confirm` — que sobre un
          // diálogo de contraseña no lleva nada y por tanto es inerte—, así
          // que la forma más natural de contestar no habría hecho nada.
          vivo.addEventListener("keydown", (e) => {
            if (e.key !== "Enter") {
              return;
            }
            e.preventDefault();
            e.stopPropagation();
            this.send({
              action: "dialog",
              id: top.id,
              choice: "confirm",
              secret: vivo.value,
            });
          });
        }
        queueMicrotask(() => {
          vivo.focus();
        });
      }
      input.setAttribute("aria-labelledby", h.id);
      this.dialogoInput = input;
      box.append(input);
    }
    const choices = document.createElement("div");
    choices.className = "choices";
    for (const c of top.choices) {
      const b = document.createElement("button");
      b.type = "button";
      b.textContent = this.t(c.label_key);
      b.dataset["destructive"] = String(c.destructive);
      b.addEventListener("click", () => {
        // La contraseña viaja CON la respuesta afirmativa, y solo con ella
        // (#327): cancelar no entrega nada. Se lee del campo vivo en este
        // instante, que es lo que el lector está viendo — el host no guarda
        // ninguna copia con la que pudiera discrepar.
        if (top.input_secret && c.id === "confirm") {
          this.send({
            action: "dialog",
            id: top.id,
            choice: c.id,
            secret: this.dialogoInput?.value ?? "",
          });
          return;
        }
        this.send({ action: "dialog", id: top.id, choice: c.id });
      });
      choices.append(b);
    }
    box.append(choices);
    this.dialogsRoot.replaceChildren(box);
    this.dialogoPintado = top.id;
  }
}

function place(el: HTMLElement, p: SlotPlacement, cell: { w: number; h: number }): void {
  el.style.setProperty("left", `${p.x * cell.w}px`);
  el.style.setProperty("top", `${p.y * cell.h}px`);
  el.style.setProperty("width", `${p.width * cell.w}px`);
  el.style.setProperty("height", `${p.height * cell.h}px`);
}

function newRow(dom: SlotDom, slotId: number, key: number): HTMLElement {
  const el = document.createElement("div");
  el.className = "row";
  // Id ESTABLE: `aria-activedescendant` apunta a él, y un id que cambia al
  // repintar deja al lector de pantalla señalando a un nodo que ya no está.
  el.id = `row-${String(slotId)}-${String(key)}`;
  el.setAttribute("role", "row");
  el.dataset["key"] = String(key);
  dom.rows.set(key, el);
  dom.canvas.append(el);
  return el;
}

function updateRow(el: HTMLElement, row: RowView, index: number, rowH: number): void {
  el.style.setProperty("top", `${index * rowH}px`);
  el.setAttribute("aria-rowindex", String(index + 1));
  el.setAttribute("aria-selected", String(row.selected));
  el.dataset["marked"] = String(row.marked);
  el.className = `row kind-${row.kind}`;
  const name = document.createElement("span");
  name.className = row.hostile ? "cell-name hostile" : "cell-name";
  name.setAttribute("role", "gridcell");
  name.textContent = row.display_name;
  // El nombre y lo que lo decora, juntos y a la IZQUIERDA; las celdas de las
  // columnas siguen a la derecha. El bloque es quien crece, así que el nombre
  // se puede recortar con elipsis SIN llevarse por delante la insignia: el
  // TUI, que no puede hacer eso, tiene que tirar la decoración entera cuando
  // el nombre no cabe.
  const bloque = document.createElement("span");
  bloque.className = "name-block";
  bloque.append(name);
  const nodes: Node[] = [bloque];
  if (row.hostile) {
    // Un nombre que se pinta distinto del real se DICE. Nunca se esconde.
    name.append(badge("△"));
  }
  if (row.badge !== "") {
    // Lo que un PLUGIN dice de esta fila, DENTRO del bloque del nombre y
    // justo detrás, como en el TUI. Suelta entre el nombre y la primera
    // celda flotaba a la derecha —`.cell-name` es `flex: 1`— y se leía como
    // parte de la columna de tamaño: la misma insignia decía dos cosas
    // distintas según quién pintara.
    //
    // En su propio NODO, no en el mismo texto: son dos datos de dos orígenes
    // y `unicode-bidi: isolate` no separa dos cosas concatenadas.
    //
    // El color sale del ROL, vocabulario cerrado del tema: un plugin no
    // elige el suyo.
    const marca = document.createElement("span");
    marca.className = "cell-badge";
    marca.dataset["role"] = row.badge_role;
    marca.dataset["hostile"] = String(row.badge_hostile);
    marca.textContent = row.badge;
    if (row.badge_hostile) {
      marca.append(badge("△"));
    }
    bloque.append(marca);
  }
  for (const c of row.cells) {
    const cell = document.createElement("span");
    cell.className = "cell";
    cell.setAttribute("role", "gridcell");
    cell.textContent = c.text ?? "";
    nodes.push(cell);
  }
  el.replaceChildren(...nodes);
}

/** Una etiqueta pequena de cabecera (nivel, filtro, origen del registro). */
function chip(text: string): HTMLElement {
  const c = document.createElement("span");
  c.className = "chip";
  c.textContent = text;
  return c;
}

function badge(text: string): HTMLElement {
  const b = document.createElement("span");
  b.className = "hostile-badge";
  b.textContent = text;
  return b;
}

function emptyNode(text: string): HTMLElement {
  const d = document.createElement("div");
  d.className = "empty";
  d.textContent = text;
  return d;
}

function errorNode(text: string, detail: string | null): HTMLElement {
  const d = document.createElement("div");
  d.className = "error";
  d.setAttribute("role", "alert");
  d.textContent = detail === null ? text : `${text}: ${detail}`;
  return d;
}

/** Los dos glifos del medio de una fila comparada, ya traducidos por el
 *  host: el veredicto y cuánto vale. */
function veredicto(r: CompareRowView, tr: (k: string) => string): HTMLElement {
  const el = document.createElement("span");
  el.className = "compare-verdict";
  el.textContent = r.verdict;
  const conf = document.createElement("span");
  conf.className = "compare-confidence";
  conf.textContent = r.confidence;
  el.append(conf);
  if (r.reason !== null) {
    const por = document.createElement("span");
    por.className = "compare-reason";
    por.textContent = r.reason;
    el.append(por);
  }
  el.title = tr("compare-title");
  return el;
}

function statusNodes(
  status: StatusView,
  connection: string,
  tr: (k: string) => string,
): Node[] {
  const nodes: Node[] = [];
  for (const b of status.banners) {
    const el = document.createElement("span");
    el.className = "banner";
    el.textContent = b.text;
    if (b.subject !== null) {
      // La conexión, en su propio elemento y etiquetada. NUNCA como
      // `scheme://host` dentro de la frase: un host puede llamarse
      // `banco.example@malo.example` sin llevar ni un carácter que se
      // enmascare, y ahí se leería como userinfo de un host legítimo.
      const sujeto = document.createElement("span");
      sujeto.className = "banner-subject";
      sujeto.dataset["hostile"] = String(b.subject.hostile);
      const esquema = document.createElement("span");
      esquema.className = "banner-scheme";
      esquema.textContent = b.subject.scheme;
      const host = document.createElement("span");
      host.className = "banner-host";
      host.textContent = b.subject.host;
      sujeto.append(esquema, host);
      // El MOTIVO, en su propio elemento y por lo mismo que la conexión: es
      // texto ya traducido por el host, y no se interpola en la frase. Sin
      // él, un motivo que el host no conoce se leía igual que «FTP en
      // claro» — un aviso de seguridad afirmando una causa que nadie dijo.
      const motivo = document.createElement("span");
      motivo.className = "banner-reason";
      motivo.textContent = b.subject.reason;
      sujeto.append(motivo);
      // El detalle solo viene con un motivo desconocido, y ya llega
      // enmascarado y acotado: es texto del otro extremo.
      if (b.subject.detail !== undefined && b.subject.detail !== "") {
        const detalle = document.createElement("span");
        detalle.className = "banner-detail";
        detalle.textContent = b.subject.detail;
        sujeto.append(detalle);
      }
      if (b.subject.hostile) {
        sujeto.classList.add("hostile");
        sujeto.append(badge(tr("hostile-name")));
      }
      el.append(sujeto);
    }
    nodes.push(el);
  }
  if (connection !== "connected") {
    const el = document.createElement("span");
    el.className = "banner";
    el.textContent = connection;
    nodes.push(el);
  }
  const msg = document.createElement("span");
  msg.textContent = status.message ?? "";
  nodes.push(msg);
  if (status.pending !== null) {
    const p = document.createElement("span");
    p.className = "pending";
    const count = status.pending.count;
    p.textContent =
      count === null
        ? status.pending.chords
        : `${String(count)} ${status.pending.chords}`;
    nodes.push(p);
  }
  return nodes;
}

function taskNode(t: TaskView, tr: (k: string) => string): HTMLElement {
  const el = document.createElement("div");
  el.className = "task";
  el.setAttribute("role", "listitem");
  const kind = document.createElement("span");
  // Con su prefijo `gui-`, que es como se llaman en el catálogo: sin él
  // TODAS caían al `?? key` y cada task del tablero se leía `task-kind-copy`.
  kind.textContent = tr(`gui-task-kind-${t.kind}`);
  const state = document.createElement("span");
  state.setAttribute("role", "progressbar");
  state.setAttribute("aria-valuemin", "0");
  state.setAttribute("aria-valuemax", "100");
  if (t.percent !== null) {
    state.setAttribute("aria-valuenow", String(t.percent));
  }
  state.textContent = t.percent === null ? t.state : `${t.state} ${String(t.percent)}%`;
  const detail = document.createElement("span");
  detail.textContent = t.detail ?? "";
  detail.dataset["hostile"] = String(t.detail_hostile);
  el.append(kind, state, detail);
  if (t.detail_hostile) {
    // El fichero en curso se pinta distinto de lo que es: se dice, igual que
    // en una fila del listado. Sin insignia, un nombre enmascarado se lee
    // como el nombre de verdad.
    detail.classList.add("hostile");
    el.append(badge(tr("hostile-name")));
  }
  if (t.foreign) {
    el.append(badge(tr("gui-task-foreign")));
  }
  return el;
}
