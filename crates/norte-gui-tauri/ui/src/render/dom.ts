// Los ayudantes de DOM del renderer, compartidos por `render.ts` y por los
// pintores de `render/*` (ola W10: el fichero único de 4.400 líneas).

import type {
  CompareRowView,
  RowView,
  SlotPlacement,
  StatusView,
  TaskView,
  UiAction,
  ViewerView,
} from "../types";

/**
 * Desplaza lo justo para que `el` se vea, si el entorno sabe hacerlo.
 *
 * `scrollIntoView` no existe en jsdom, donde corren los tests del renderer:
 * sin la guarda, comprobar el pintado de una lista tumbaba el test en una
 * llamada que no es del pintado.
 */
export function revelar(el: Element | undefined): void {
  if (el instanceof HTMLElement && typeof el.scrollIntoView === "function") {
    el.scrollIntoView({ block: "nearest" });
  }
}

/** Un párrafo con una frase que el host ya escribió. */
export function nota(texto: string): HTMLElement {
  const p = document.createElement("p");
  p.className = "slot-note";
  p.textContent = texto;
  return p;
}

/**
 * Una barra de scroll del visor, o `null` si cabe todo.
 *
 * PROPIA y no la del navegador: el host manda solo la ventana visible, así que
 * el `pre` mide exactamente lo que se ve y `overflow` no tiene nada que
 * desplazar. Sin barra, el visor decía «hay más» solo en la cuenta de la
 * cabecera, y a lo ancho no lo decía nada — y el visor no envuelve, así que un
 * fichero cortado por la derecha se lee como un fichero corto.
 *
 * No se arrastra: es un INDICADOR. Arrastrarla pediría traducir píxeles a
 * líneas del lado del renderer, que es justo lo que el host hace ya para la
 * rueda. Por eso va `aria-hidden` y NO `role="scrollbar"`: ese rol promete un
 * control que no existe y exige un `aria-controls` que no hay. Quien no la ve
 * lee la posición en las marcas de la cabecera, que la dicen con palabras.
 *
 * `visible === 0` es la medida de ANTES de pintar —el cuerpo aún no tiene
 * altura— y entonces no se dibuja nada: con `Math.max(1, 0)` salía un pulgar
 * de un píxel durante un frame.
 */
export function viewerBar(
  vertical: boolean,
  total: number,
  first: number,
  visible: number,
): HTMLElement | null {
  if (visible <= 0 || total <= visible) {
    return null;
  }
  const bar = document.createElement("div");
  bar.className = vertical ? "viewer-bar viewer-bar-v" : "viewer-bar viewer-bar-h";
  bar.setAttribute("aria-hidden", "true");
  const thumb = document.createElement("div");
  thumb.className = "viewer-thumb";
  const largo = visible / total;
  const donde = Math.min(1, Math.max(0, first / (total - visible)));
  const pct = (x: number): string => `${(x * 100).toFixed(2)}%`;
  if (vertical) {
    thumb.style.height = pct(largo);
    thumb.style.top = pct((1 - largo) * donde);
  } else {
    thumb.style.width = pct(largo);
    thumb.style.left = pct((1 - largo) * donde);
  }
  bar.append(thumb);
  return bar;
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
export function viewerBody(viewer: ViewerView): HTMLElement {
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
export const OVERSCAN = 8;

export type Send = (action: UiAction) => void;

export interface SlotDom {
  root: HTMLElement;
  /** La barra de pestañas, vacía cuando el hueco no está en un grupo. */
  tabs: HTMLElement;
  title: HTMLElement;
  header: HTMLElement;
  scroller: HTMLElement;
  canvas: HTMLElement;
  /** El pie del listado (cuentas, marcado, espacio libre). Vacío = oculto. */
  footer: HTMLElement;
  rows: Map<number, HTMLElement>;
  /**
   * El aviso de «esperando», ESTABLE. No se crea en cada pintada porque su
   * umbral es un `animation-delay`, y una animación que empieza de cero cada
   * vez que su nodo nace nunca llega a los 250 ms: `paint()` repinta todos
   * los huecos en cada actualización, así que el aviso no habría aparecido
   * jamás en los casos lentos, que son para los que existe.
   */
  busy: HTMLElement;
  lastRange: { first: number; count: number } | null;
  /**
   * La generación que se PINTÓ. Toda acción de fila la lleva: sin ella la
   * clave es un índice, y un índice de la pantalla anterior nombra otro
   * fichero. El host la compara y responde `stale` si no coincide.
   */
  generation: number;
}

export function place(
  el: HTMLElement,
  p: SlotPlacement,
  cell: { w: number; h: number },
): void {
  el.style.setProperty("left", `${p.x * cell.w}px`);
  el.style.setProperty("top", `${p.y * cell.h}px`);
  el.style.setProperty("width", `${p.width * cell.w}px`);
  el.style.setProperty("height", `${p.height * cell.h}px`);
}

export function newRow(dom: SlotDom, slotId: number, key: number): HTMLElement {
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

export function updateRow(
  el: HTMLElement,
  row: RowView,
  index: number,
  rowH: number,
  // La columna de iconos está abierta en este hueco (puente 62): ALGUNA
  // fila tiene icono, así que todas llevan la celda, vacía o no, para que
  // los nombres sigan alineados. Lo decide quien pinta el hueco, no la fila.
  iconColumn = false,
): void {
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
  if (iconColumn) {
    // El icono, ANTES del nombre y en su propio nodo de ancho fijo: es
    // texto de un plugin, y la celda existe aunque esta fila no tenga
    // icono, que es lo que mantiene la columna.
    const icono = document.createElement("span");
    icono.className = "cell-icon";
    icono.dataset["hostile"] = String(row.icon_hostile);
    icono.textContent = row.icon;
    if (row.icon_hostile) {
      icono.append(badge("△"));
    }
    bloque.append(icono);
  }
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
export function chip(text: string): HTMLElement {
  const c = document.createElement("span");
  c.className = "chip";
  c.textContent = text;
  return c;
}

export function badge(text: string): HTMLElement {
  const b = document.createElement("span");
  b.className = "hostile-badge";
  b.textContent = text;
  return b;
}

export function emptyNode(text: string): HTMLElement {
  const d = document.createElement("div");
  d.className = "empty";
  d.textContent = text;
  return d;
}

export function errorNode(text: string, detail: string | null): HTMLElement {
  const d = document.createElement("div");
  d.className = "error";
  d.setAttribute("role", "alert");
  d.textContent = detail === null ? text : `${text}: ${detail}`;
  return d;
}

/** Los dos glifos del medio de una fila comparada, ya traducidos por el
 *  host: el veredicto y cuánto vale. */
export function veredicto(r: CompareRowView, tr: (k: string) => string): HTMLElement {
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

export function statusNodes(
  status: StatusView,
  connection: string,
  tr: (k: string) => string,
  rechazo: string | null = null,
  onNotices: (() => void) | null = null,
): Node[] {
  const nodes: Node[] = [];
  if (rechazo !== null) {
    // Delante de todo: es lo único de esta barra que el host no sabe.
    const el = document.createElement("span");
    el.className = "banner rejected";
    el.textContent = rechazo;
    nodes.push(el);
  }
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
  // Los avisos caducados sin leer (puente 63): una insignia `!n` que abre
  // el registro, por el botón de la barra de paneles — el mismo despacho
  // que su tecla. Sin sitio donde abrirlo (sin botón), la insignia solo
  // cuenta.
  const sinLeer = status.notices_unread ?? 0;
  if (sinLeer > 0 && status.message === null) {
    const insignia = document.createElement("button");
    insignia.type = "button";
    insignia.className = "notices";
    insignia.textContent = `!${String(sinLeer)}`;
    insignia.title = tr("status-notices");
    insignia.setAttribute("aria-label", tr("status-notices"));
    if (onNotices !== null) {
      insignia.addEventListener("click", onNotices);
    }
    nodes.push(insignia);
  }
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

export function taskNode(t: TaskView, tr: (k: string) => string): HTMLElement {
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
