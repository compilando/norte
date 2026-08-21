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
  PaletteView,
  ExtensionsView,
  LayoutPickerView,
  MetadataSlotView,
  SearchView,
  PlacesSlotView,
  PickerView,
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

/** Filas de más que se piden por arriba y por abajo del hueco visible. */
const OVERSCAN = 8;

type Send = (action: UiAction) => void;

interface SlotDom {
  root: HTMLElement;
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
  /** Las líneas de visor que ya se declararon. */
  private viewerRows = 0;
  /** La ayuda está abierta con el CUERPO enfocado. */
  private helpBodyFocused = false;
  private pendingRange = new Map<number, number>();

  constructor(
    private readonly root: HTMLElement,
    private readonly paletteRoot: HTMLElement,
    private readonly whichKeyRoot: HTMLElement,
    private readonly helpRoot: HTMLElement,
    private readonly settingsRoot: HTMLElement,
    private readonly extensionsRoot: HTMLElement,
    private readonly themeRoot: HTMLElement,
    private readonly pickerRoot: HTMLElement,
    private readonly layoutsRoot: HTMLElement,
    private readonly searchRoot: HTMLElement,
    private readonly viewerRoot: HTMLElement,
    private readonly dialogsRoot: HTMLElement,
    private readonly catalog: HostCatalog,
    private readonly send: Send,
  ) {}

  /**
   * La ayuda está abierta y su cuerpo tiene el foco, así que las teclas de
   * página son del scroll del DOM y no del host.
   */
  helpBodyScrolls(): boolean {
    return this.helpBodyFocused;
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
      this.paintSlot(dom, slot, view, cell);
    }
    this.paintPalette(view.palette);
    this.paintWhichKey(view.whichkey);
    this.paintHelp(view.help);
    this.paintSettings(view.settings);
    this.paintExtensions(view.extensions);
    this.paintTheme(view.theme);
    this.paintPicker(view.picker);
    this.paintLayouts(view.layouts);
    this.paintSearch(view.search);
    this.paintViewer(view.viewer);
    this.paintDialogs(view.dialogs);
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
      return;
    }
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
          valor.textContent = r.value;
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
        motivo.textContent = e.reason;
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
    for (const k of d.config) {
      const tr = document.createElement("tr");
      // Un valor que NO es el del esquema se marca: es lo único que
      // distingue «así viene» de «así lo dejaste».
      tr.dataset["changed"] = String(k.value !== k.default);
      const clave = document.createElement("th");
      clave.setAttribute("scope", "row");
      clave.className = "extensions-key";
      clave.textContent = k.key;
      const valor = document.createElement("td");
      valor.className = "extensions-key-value";
      valor.textContent = k.value;
      const tipo = document.createElement("td");
      tipo.className = "extensions-key-kind";
      tipo.textContent = k.domain === "" ? k.kind : `${k.kind} · ${k.domain}`;
      const desc = document.createElement("td");
      desc.className = "extensions-key-desc";
      desc.textContent = k.description;
      tr.append(clave, valor, tipo, desc);
      tbody.append(tr);
    }
    tabla.append(tbody);
    ficha.append(tabla);
    return ficha;
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

    if (theme.unsupported_effects.length > 0) {
      // Se NOMBRAN. Un tema retro que se ve idéntico a los demás se lee como
      // roto, y el usuario va a buscar el bug donde no está.
      const aviso = document.createElement("p");
      aviso.className = "theme-effects";
      aviso.setAttribute("role", "note");
      aviso.textContent = `${this.t("theme-effects-unsupported")} ${theme.unsupported_effects.join(" · ")}`;
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
    caja.setAttribute("aria-label", this.t("search-title"));

    const titulo = document.createElement("h1");
    titulo.textContent = `${this.t("search-title")} · ${search.query}`;
    caja.append(titulo);

    const donde = document.createElement("p");
    donde.className = "search-root";
    donde.dataset["hostile"] = String(search.root_hostile);
    donde.textContent = search.root;
    if (search.root_hostile) {
      donde.append(badge(this.t("hostile-name")));
    }
    caja.append(donde);

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
      return;
    }
    this.viewerRoot.dataset["open"] = "true";
    const box = document.createElement("section");
    box.className = "viewer";
    box.setAttribute("role", "document");
    box.setAttribute("aria-label", viewer.path_display);

    const head = document.createElement("header");
    head.className = "viewer-head";
    head.append(document.createTextNode(viewer.path_display));
    if (viewer.path_hostile) {
      head.append(badge(this.t("hostile-name")));
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

    const body = document.createElement("pre");
    body.className = viewer.hex ? "viewer-body hexview" : "viewer-body";
    body.setAttribute("tabindex", "-1");
    body.setAttribute("aria-describedby", `viewer-meta-${String(viewer.first_line)}`);
    body.textContent = viewer.lines.join("\n");

    box.append(head, body);
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

  private rebuild(view: ViewSnapshot, cell: { w: number; h: number }): void {
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
      el.append(title, header, scroller);
      this.root.append(el);
      const dom: SlotDom = {
        root: el,
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
      if (dom.root.dataset["role"] !== "active") {
        this.send({ action: "focus_slot", slot_id: slotId });
      }
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
    if (slot.kind === "metadata") {
      this.paintMetadata(dom, slot);
      return;
    }
    if (slot.kind === "processes") {
      this.paintProcesses(dom, slot, view);
      return;
    }
    if (slot.kind === "unsupported") {
      this.paintAux(dom, slot.kind_name, view);
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

  private paintAux(dom: SlotDom, kindName: string, view: ViewSnapshot): void {
    dom.root.setAttribute("aria-label", kindName);
    dom.title.textContent = "";
    if (kindName === "status") {
      dom.scroller.className = "statusbar";
      dom.scroller.setAttribute("role", "status");
      dom.scroller.setAttribute("aria-live", "polite");
      dom.scroller.replaceChildren(...statusNodes(view.status, view.connection.state));
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
    dom.title.replaceChildren(document.createTextNode(slot.path_display));
    if (slot.path_hostile) {
      dom.title.append(badge(this.t("hostile-name")));
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
      dom.canvas.replaceChildren(
        errorNode(this.t(slot.state.reason_key), slot.state.detail),
      );
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
    for (const line of top.body) {
      const p = document.createElement("p");
      p.textContent = line;
      box.append(p);
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
        input.type = "text";
        input.value = top.input;
        const vivo = input;
        vivo.addEventListener("input", () => {
          this.send({ action: "dialog_input", id: top.id, text: vivo.value });
        });
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
  const nodes: Node[] = [name];
  if (row.hostile) {
    // Un nombre que se pinta distinto del real se DICE. Nunca se esconde.
    name.append(badge("△"));
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

function statusNodes(status: StatusView, connection: string): Node[] {
  const nodes: Node[] = [];
  for (const b of status.banners) {
    const el = document.createElement("span");
    el.className = "banner";
    el.textContent = b;
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
  kind.textContent = tr(`task-kind-${t.kind}`);
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
  el.append(kind, state, detail);
  if (t.foreign) {
    el.append(badge(tr("task-foreign")));
  }
  return el;
}
