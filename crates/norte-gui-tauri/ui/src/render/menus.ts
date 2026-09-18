// Pintores de `Screen` para menus (ola W10): funciones con `this: Screen`,
// enganchadas como propiedades en `render.ts`. El estado sigue en la clase.

import type { Screen } from "../render";
import type {
  BrowserSlotView,
  ColumnHeader,
  MenuView,
  KeyBarView,
  PanelBarView,
  WizardView,
  PaletteView,
  TabGroupView,
  WhichKeyView,
} from "../types";
import { badge, colVar, sinCambios } from "./dom";
import type { SlotDom } from "./dom";

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
export function paintPanelBar(this: Screen, bar: PanelBarView): void {
  const alto = bar.bar ? "var(--cell-h)" : "0px";
  if (this.panelBarHeight !== alto) {
    document.documentElement.style.setProperty("--panelbar-h", alto);
    this.panelBarHeight = alto;
    this.viewportSucio = true;
  }
  if (sinCambios(this.panelBarRoot, JSON.stringify(bar))) {
    return;
  }
  if (!bar.bar) {
    this.panelBarRoot.replaceChildren();
    return;
  }
  const fila = document.createElement("nav");
  fila.className = "panelbar";
  fila.setAttribute("role", "toolbar");
  fila.setAttribute("aria-label", this.t("panelbar-label"));
  // `[ui] panel_bar_style`: con nombres o solo con la letra. El nombre
  // sigue en el título del botón en los dos casos.
  fila.dataset["names"] = String(bar.names !== false);
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
      this.send({ action: "panel_bar_activate", button: i });
    });
    fila.append(boton);
  }
  this.panelBarRoot.replaceChildren(fila);
}

/**
 * La barra de teclas de función (puente 63): diez celdas con lo que cada
 * `F` hace en la pantalla que tiene el teclado. El host la deriva del
 * keymap; aquí solo se pinta, y un click devuelve la TECLA, que el host
 * sintetiza — no hay un segundo despacho que pueda divergir.
 *
 * Su raíz se busca por id y, si el documento no la trae (un test, una
 * página anterior), se crea al final del cuerpo: es una fila `fixed` abajo,
 * y el orden del documento no le importa.
 */
export function paintKeyBar(this: Screen, bar: KeyBarView | null): void {
  const doc = this.root.ownerDocument;
  let raiz = doc.getElementById("keybar");
  if (raiz === null) {
    raiz = doc.createElement("div");
    raiz.id = "keybar";
    doc.body.append(raiz);
  }
  const visible = bar !== null && bar.bar;
  const alto = visible ? "var(--cell-h)" : "0px";
  if (this.keyBarHeight !== alto) {
    doc.documentElement.style.setProperty("--keybar-h", alto);
    this.keyBarHeight = alto;
    this.viewportSucio = true;
  }
  if (sinCambios(raiz, JSON.stringify(bar))) {
    return;
  }
  if (!visible) {
    raiz.replaceChildren();
    return;
  }
  const fila = doc.createElement("nav");
  fila.className = "keybar";
  fila.setAttribute("role", "toolbar");
  fila.setAttribute("aria-label", this.t("keybar-label"));
  for (const c of bar.cells) {
    const boton = doc.createElement("button");
    boton.type = "button";
    boton.className = "keybar-cell";
    boton.dataset["bound"] = String(c.command !== null);
    boton.disabled = c.command === null;
    if (c.command !== null) {
      boton.title = `F${String(c.key)} · ${c.command}`;
    }
    const num = doc.createElement("span");
    num.className = "keybar-num";
    num.textContent = String(c.key);
    const etiqueta = doc.createElement("span");
    etiqueta.className = "keybar-label";
    etiqueta.textContent = c.label;
    boton.append(num, etiqueta);
    boton.addEventListener("click", () => {
      this.send({ action: "key_bar_activate", key: c.key });
    });
    fila.append(boton);
  }
  raiz.replaceChildren(fila);
}

/**
 * El asistente de primer arranque (puente 63): el título del paso, la
 * pregunta, las filas con el cursor y la línea de teclas. Todo llega ya
 * traducido; un click en una fila la elige y la confirma. Su raíz se busca
 * por id y, si el documento no la trae, se crea al final del cuerpo: es un
 * velo a pantalla completa, y el orden del documento no le importa.
 */
export function paintWizard(this: Screen, wizard: WizardView | null): void {
  const doc = this.root.ownerDocument;
  let raiz = doc.getElementById("wizard");
  if (raiz === null) {
    raiz = doc.createElement("div");
    raiz.id = "wizard";
    doc.body.append(raiz);
  }
  if (wizard === null) {
    raiz.replaceChildren();
    raiz.dataset["open"] = "false";
    return;
  }
  raiz.dataset["open"] = "true";
  const caja = doc.createElement("section");
  caja.className = "wizard";
  caja.setAttribute("role", "dialog");
  caja.setAttribute("aria-modal", "true");
  caja.setAttribute("aria-label", wizard.title);
  const titulo = doc.createElement("h2");
  titulo.className = "wizard-title";
  titulo.textContent = wizard.title;
  const pregunta = doc.createElement("p");
  pregunta.className = "wizard-question";
  pregunta.textContent = wizard.question;
  const lista = doc.createElement("ul");
  lista.className = "wizard-rows";
  lista.setAttribute("role", "listbox");
  for (const [i, texto] of wizard.rows.entries()) {
    const fila = doc.createElement("li");
    fila.className = "wizard-row";
    fila.id = `wizard-row-${String(i)}`;
    fila.setAttribute("role", "option");
    fila.setAttribute("aria-selected", String(wizard.cursor === i));
    fila.textContent = texto;
    fila.addEventListener("click", () => {
      this.send({ action: "wizard_activate_row", row: i });
    });
    lista.append(fila);
  }
  lista.setAttribute("aria-activedescendant", `wizard-row-${String(wizard.cursor)}`);
  const pista = doc.createElement("p");
  pista.className = "wizard-hint";
  pista.textContent = wizard.hint;
  caja.append(titulo, pregunta, lista, pista);
  raiz.replaceChildren(caja);
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
export function paintMenu(this: Screen, menu: MenuView): void {
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
  if (sinCambios(this.menuRoot, JSON.stringify(menu))) {
    return;
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
      // Una sección empieza AQUÍ (puente 74): una raya, con su rótulo si lo
      // tiene. Es un `separator` y no una entrada, así que el cursor, que
      // cuenta entradas, no la ve.
      if (item.section !== null) {
        const raya = document.createElement("li");
        raya.className = "menu-section";
        raya.setAttribute("role", "separator");
        if (item.section !== "") {
          raya.textContent = item.section;
          raya.dataset["titled"] = "true";
        }
        lista.append(raya);
      }
      const fila = document.createElement("li");
      fila.className = "menu-item";
      fila.id = `menu-item-${String(i)}`;
      fila.setAttribute("role", "menuitem");
      fila.setAttribute("aria-disabled", String(!item.enabled));
      fila.dataset["enabled"] = String(item.enabled);
      fila.dataset["current"] = String(menu.cursor === i);
      fila.dataset["role"] = item.role;
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
  if (menu.open !== null) {
    // El desplegable cuelga del título PINTADO, medido una vez montado: los
    // títulos se pintan con relleno en píxeles y no miden lo mismo, así que
    // una cuenta en celdas se desviaba más cuanto más a la derecha estaba el
    // menú. Se mide después de `replaceChildren` porque antes no hay
    // geometría; forzar un reparto aquí es barato, un menú se abre a mano.
    const titulo = this.menuRoot.querySelector(`#menu-title-${String(menu.open)}`);
    const lista = this.menuRoot.querySelector(".menu-items");
    if (titulo instanceof HTMLElement && lista instanceof HTMLElement) {
      const x = titulo.getBoundingClientRect().left;
      lista.style.setProperty("--menu-left", `${String(Math.max(0, x))}px`);
    }
  }
}

/** La paleta de comandos. */
export function paintPalette(this: Screen, palette: PaletteView | null): void {
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
    fila.dataset["recent"] = String(r.recent === true);
    // La etiqueta humana primero y entera, el id atenuado, el chord a la
    // derecha: es lo que se lee, en ese orden. El id sigue en el DOM porque
    // es lo que un lector que ya lo sabe teclea.
    const desc = document.createElement("span");
    desc.className = "palette-desc";
    desc.textContent = r.desc;
    const texto = document.createElement("span");
    texto.className = "palette-text";
    texto.textContent = r.text;
    const chord = document.createElement("span");
    chord.className = "palette-chord";
    chord.textContent = r.chord;
    fila.append(desc, texto, chord);
    if (r.hostile) {
      // Solo una fila de PLUGIN puede serlo, y esta es la pantalla donde
      // se elige qué código de tercero correr: un texto enmascarado que
      // viaja sin decirlo se lee como fiel.
      fila.append(badge(this.t("hostile-name")));
    }
    lista.append(fila);
  }
  if (palette.cursor !== null) {
    lista.setAttribute("aria-activedescendant", `palette-row-${String(palette.cursor)}`);
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
export function paintWhichKey(this: Screen, panel: WhichKeyView | null): void {
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
 * La barra de PESTAÑAS de un hueco, si está en un grupo.
 *
 * Se pinta aunque solo se vea el contenido de una: lo que hay detrás sigue
 * abierto, y una ventana que no lo dice esconde trabajo. El rótulo llega ya
 * enmascarado del host —un directorio hostil dentro de una pestaña es tan
 * hostil como dentro de un listado— con su bandera al lado.
 */
export function paintTabs(
  this: Screen,
  dom: SlotDom,
  grupo: TabGroupView | undefined,
): void {
  if (sinCambios(dom.tabs, JSON.stringify(grupo ?? null))) {
    return;
  }
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

/**
 * La cabecera: etiquetas y marca de orden, ambas resueltas en Rust.
 *
 * El ancho fijo y la alineación de cada columna (puente 64) se escriben como
 * variables en la RAÍZ del hueco, no en cada celda: las filas ya pintadas
 * las leen sin repintarse, y arrastrar el tirador solo cambia una variable.
 * La del nombre nunca: es la que crece.
 */
export function paintHeader(this: Screen, dom: SlotDom, slot: BrowserSlotView): void {
  if (sinCambios(dom.header, JSON.stringify(slot.columns))) {
    // Las mismas columnas: los nodos y las variables de ancho ya están. Lo
    // que NO se puede saltar es el descarte, que depende del ancho del hueco
    // y no de las columnas — un hueco que se ensanchó recupera la columna que
    // descartó de estrecho, y por eso se vuelve a decidir desde cero.
    for (const c of slot.columns) {
      if (c.id !== "name") {
        dom.root.style.removeProperty(`${colVar(c.id)}-show`);
      }
    }
    descartarLasQueNoCaben(dom, slot, this.cell().w);
    return;
  }
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
    const v = colVar(c.id);
    if (c.id === "name") {
      dom.root.style.removeProperty(v);
      dom.root.style.removeProperty(`${v}-align`);
      return el;
    }
    if (c.width === null) {
      dom.root.style.removeProperty(v);
    } else {
      dom.root.style.setProperty(v, `calc(var(--cell-w) * ${String(c.width)})`);
    }
    dom.root.style.setProperty(`${v}-align`, c.align === "right" ? "right" : "left");
    el.style.width = `var(${v}, auto)`;
    el.style.textAlign = `var(${v}-align, left)`;
    el.style.display = `var(${v}-show, block)`;
    // Se vuelve a decidir en cada pintado: un hueco que se ensanchó recupera
    // la columna que descartó cuando era estrecho.
    dom.root.style.removeProperty(`${v}-show`);
    const grip = document.createElement("span");
    grip.className = "col-grip";
    grip.dataset["grip"] = c.id;
    el.append(grip);
    return el;
  });
  dom.header.replaceChildren(...nodes);
  descartarLasQueNoCaben(dom, slot, this.cell().w);
}

/**
 * La regla 2 del reparto compartido (`columns::layout`): si las columnas
 * no dejan al nombre su suelo, se descartan desde la MÁS A LA DERECHA hasta
 * que quepan. Se hace aquí y no en el host porque el ancho útil del hueco
 * en píxeles —bordes, relleno, tiradores— solo lo sabe quien pinta.
 *
 * El suelo del nombre viene del HOST en la propia cabecera (`width` de la
 * columna `name` es `NAME_MIN`, no un ancho): un número que viviera aquí
 * también se separaría del de Rust sin que nadie lo viera. Y cuenta TODAS
 * las columnas, no solo las fijas: una `auto` o `flex` pesa lo que mide su
 * cabecera ya pintada. Sin medida (un documento sin layout, como el de los
 * tests) no se descarta nada: mejor una columna de más que un listado sin
 * columnas.
 */
function descartarLasQueNoCaben(
  dom: SlotDom,
  slot: BrowserSlotView,
  cellW: number,
): void {
  const total = dom.root.clientWidth;
  if (total <= 0 || cellW <= 0) {
    return;
  }
  const suelo = slot.columns.find((c) => c.id === "name")?.width ?? 10;
  const cabeceras = [...dom.header.querySelectorAll<HTMLElement>(".col")];
  const anchoDe = (c: ColumnHeader, i: number): number =>
    c.width === null
      ? (cabeceras[i]?.getBoundingClientRect().width ?? 0)
      : c.width * cellW;
  // Relleno de la fila (6 px a cada lado), el borde del hueco, la casilla
  // de marca (1,1 em ≈ una celda y media) y una celda de separación por
  // columna.
  let libre = total - 14 - cellW * 1.5 - cellW * slot.columns.length;
  const otras = slot.columns.map((c, i) => ({ c, i })).filter(({ c }) => c.id !== "name");
  for (const { c, i } of otras) {
    libre -= anchoDe(c, i);
  }
  const minimo = suelo * cellW;
  for (let k = otras.length - 1; k >= 0 && libre < minimo; k -= 1) {
    const entrada = otras[k];
    if (entrada === undefined) {
      break;
    }
    dom.root.style.setProperty(`${colVar(entrada.c.id)}-show`, "none");
    libre += anchoDe(entrada.c, entrada.i) + cellW;
  }
}
