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
  ViewerView,
  WhichKeyView,
} from "./types";

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
  /** Las líneas de visor que ya se declararon. */
  private viewerRows = 0;
  private pendingRange = new Map<number, number>();

  constructor(
    private readonly root: HTMLElement,
    private readonly paletteRoot: HTMLElement,
    private readonly whichKeyRoot: HTMLElement,
    private readonly helpRoot: HTMLElement,
    private readonly viewerRoot: HTMLElement,
    private readonly dialogsRoot: HTMLElement,
    private readonly catalog: HostCatalog,
    private readonly send: Send,
  ) {}

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
      return;
    }
    this.helpRoot.dataset["open"] = "true";
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
    return nav;
  }

  /** El cuerpo: la prosa de la página y lo que se puede ejecutar en ella. */
  private helpBody(help: HelpView): HTMLElement {
    const cuerpo = document.createElement("article");
    cuerpo.className = "help-body";
    cuerpo.dataset["focused"] = String(help.focus === "body");

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
        label.textContent = a.opens_topic ? `${a.label} →` : a.label;
        fila.append(chord, label);
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
          // Atenuar sin decir por qué deja al lector adivinando si la
          // ventana está rota; el motivo ya viene traducido del host.
          label.textContent =
            r.enabled || r.reason === "" ? r.label : `${r.label} — ${r.reason}`;
          tr.append(chord, label);
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
        el.dataset["topic"] = s.topic;
        el.textContent = s.text;
        return el;
      }
    }
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
      marcas.push(this.t("viewer-errors"));
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
    if (slot.kind === "unsupported") {
      this.paintAux(dom, slot.kind_name, view);
      return;
    }
    this.paintBrowser(dom, slot, cell);
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
    if (top.input !== null) {
      const input = document.createElement("input");
      input.type = "text";
      // El valor se pone UNA vez, al crear el campo. Reescribirlo en cada
      // repintado devolvía al campo la proyección del host —enmascarada y
      // acotada— y el siguiente evento la mandaba de vuelta como si fuera lo
      // tecleado: el nombre se convertía en su propia sombra.
      if (this.dialogoPintado !== top.id) {
        input.value = top.input;
      }
      input.setAttribute("aria-labelledby", h.id);
      input.addEventListener("input", () => {
        this.send({ action: "dialog_input", id: top.id, text: input.value });
      });
      box.append(input);
      queueMicrotask(() => {
        input.focus();
      });
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
