// Pintores de `Screen` para viewer (ola W10): funciones con `this: Screen`,
// enganchadas como propiedades en `render.ts`. El estado sigue en la clase.

import type { Screen } from "../render";
import type { MetadataSlotView, PreviewSlotView, ViewerView } from "../types";
import { nota, viewerBar, viewerBody, badge } from "./dom";
import type { SlotDom } from "./dom";

/** El visor tapa la pantalla mientras está abierto. */
export function paintViewer(this: Screen, viewer: ViewerView | null): void {
  if (viewer === null) {
    this.viewerRoot.replaceChildren();
    this.viewerRoot.dataset["open"] = "false";
    this.viewerRows = 0;
    this.visorPintado = null;
    // El visor se cerró: se SUELTA el búfer. Un object URL sin revocar
    // retiene sus bytes mientras viva el documento.
    this.soltarImagen();
    return;
  }
  // El mismo visor en una ventana del mismo tamaño y con la misma celda ya
  // está pintado: nada de lo que se construye abajo cambiaría. Leer el
  // tamaño de la ventana y las variables de la raíz no fuerza un reflujo;
  // medir el cuerpo, que es lo que hace el final de esta función, sí.
  const celda = this.cell();
  const firma = `${String(window.innerWidth)}x${String(window.innerHeight)}|${String(celda.w)}x${String(celda.h)}`;
  const previo = this.visorPintado;
  if (previo?.viewer === viewer && previo.firma === firma) {
    return;
  }
  this.visorPintado = { viewer, firma };
  if (previo !== null && previo.firma === firma && desplazado(previo.viewer, viewer)) {
    // Solo se ha DESPLAZADO: el mismo fichero, la misma cabecera, el mismo
    // tamaño. Se cambian el cuerpo, las marcas y las barras en su sitio.
    // Rehacer la caja entera en cada paso de la rueda, y medir el cuerpo
    // nuevo —un reflujo—, era lo que hacía pesado desplazarse.
    const box = this.viewerRoot.querySelector<HTMLElement>(".viewer");
    const viejo = box?.querySelector<HTMLElement>(".viewer-body");
    const meta = box?.querySelector<HTMLElement>(".viewer-meta");
    if (box && viejo && meta) {
      meta.textContent = marcasDelVisor(this, viewer);
      box.querySelectorAll(".viewer-bar").forEach((b) => {
        b.remove();
      });
      const body = cuerpoDelVisor(viewer);
      viejo.replaceWith(body);
      ponerBarras(this, viewer, box, body);
      return;
    }
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
  meta.textContent = marcasDelVisor(this, viewer);
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

  const body = cuerpoDelVisor(viewer);

  // El cuerpo y la barra VERTICAL van en una fila; la HORIZONTAL debajo de las
  // dos. Las barras son propias porque el host manda solo la ventana visible:
  // el `pre` mide lo que se ve y `overflow` no tiene qué mover.
  const lienzo = document.createElement("div");
  lienzo.className = "viewer-canvas";
  lienzo.append(body);
  box.append(head, lienzo);
  ponerBarras(this, viewer, box, body);
  // La rueda desplaza por el HOST, como el visor acoplado y el registro: la
  // ventana visible la decide él. Con `shift`, de lado — es el gesto normal
  // para un eje horizontal, y el visor no envuelve.
  box.onwheel = (e) => {
    e.preventDefault();
    const c = this.cell();
    const pasos = (delta: number, celda: number): number => {
      if (delta === 0) {
        return 0;
      }
      // Al menos UNO: un trackpad manda deltas de pocos píxeles, y redondear
      // a cero convertía el gesto en nada.
      const n = Math.round(Math.abs(delta) / celda);
      return Math.sign(delta) * Math.max(1, n);
    };
    const lines = e.shiftKey ? 0 : pasos(e.deltaY, c.h);
    const cols = e.shiftKey ? pasos(e.deltaY, c.w) : pasos(e.deltaX, c.w);
    if (lines !== 0 || cols !== 0) {
      this.send({ action: "viewer_scroll", lines, cols });
    }
  };
  if (viewer.image !== null) {
    // Los bytes NO vienen en la foto: se piden aparte y se pintan cuando
    // llegan. Hasta entonces se ve la vista cruda, que es lo honesto —el
    // fichero es ese— en vez de un hueco vacío.
    this.pintarImagen(viewer, body);
  }
  this.viewerRoot.replaceChildren(box);
  // Cuántas líneas caben lo sabe QUIEN PINTA. El host lo estimaba con
  // celdas de disposición menos un cromo adivinado, así que mandaba más
  // líneas de las que se ven —se recortaban sin decirlo— y avanzaba una
  // página por un número distinto: cada página saltaba lo recortado.
  const filas = Math.max(1, Math.floor(body.clientHeight / celda.h));
  if (filas !== this.viewerRows) {
    this.viewerRows = filas;
    this.send({ action: "set_viewer_rows", rows: filas });
  }
  // Y cuántas COLUMNAS, por lo mismo: es lo que el host le dice al
  // previewer (proto 0.66.0) la próxima vez, y el viewport entero cuenta
  // el cromo — una imagen encogida a él se salía por la derecha.
  const columnas = Math.max(1, Math.floor(body.clientWidth / celda.w));
  if (columnas !== this.viewerCols) {
    this.viewerCols = columnas;
    this.send({ action: "set_viewer_cols", cols: columnas });
  }
}

/** Revoca el `blob:` vivo, si lo hay. Idempotente. */
export function soltarImagen(this: Screen): void {
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
 *
 * Solo necesita el CUERPO, no el marco: la foto sustituye al cuerpo en su
 * sitio, sea quien sea su padre. Recibir el marco era lo que permitía a una
 * de las dos ramas reconstruirlo y llevarse por delante lo que hubiera al
 * lado del cuerpo.
 */
export function pintarImagen(this: Screen, viewer: ViewerView, body: HTMLElement): void {
  const img = viewer.image;
  if (img === null) {
    return;
  }
  const clave = `${viewer.path_display}|${img.format}|${String(img.width)}x${String(img.height)}`;
  if (this.imagenDe === clave && this.imagenUrl !== null) {
    // Ya está pedida —o pintada— y es la misma: no se vuelve a pedir.
    //
    // Se cambia el CUERPO por la foto, en su sitio, igual que hace la rama de
    // abajo cuando llegan los bytes. Antes esto reconstruía la caja entera
    // (`box.replaceChildren(cabecera, foto)`), y eso se llevaba por delante
    // todo lo que la caja tuviera además del cuerpo: con el lienzo de las
    // barras dentro, la foto acabó siendo hija directa del marco y el hueco
    // se quedó sin alto. Una rama que reconstruye lo que otra solo sustituye
    // es la misma divergencia de siempre, dentro de una función.
    body.replaceWith(this.nodoImagen(this.imagenUrl, img, viewer.image_zoom));
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
      body.replaceWith(this.nodoImagen(url, img, viewer.image_zoom));
    })
    .catch(() => {
      // Sin imagen se queda la vista cruda, que es el fichero de verdad.
      this.imagenDe = null;
    });
}

/**
 * La hoja de atributos: etiqueta y valor, y nada más.
 *
 * Todo llega formateado y saneado del host — el tamaño con su forma humana
 * y su número exacto, la fecha en ISO, cada atributo por la misma puerta
 * que su columna. Aquí no se formatea nada.
 */
export function paintMetadata(this: Screen, dom: SlotDom, slot: MetadataSlotView): void {
  // El título DICE a qué listado sigue. «Detalles» a secas no dice de qué
  // son los detalles, y con dos listados abiertos la única forma de
  // averiguarlo era mover el cursor y mirar si la hoja se movía.
  const titulo =
    slot.follows_display === ""
      ? this.t("metadata-title")
      : `${this.t("metadata-title")} · ${slot.follows_display}`;
  dom.root.setAttribute("aria-label", titulo);
  if (slot.follows_display === "") {
    dom.title.replaceChildren(document.createTextNode(titulo));
    return this.paintMetadataBody(dom, slot);
  }
  // La ruta en su propio nodo y con la clase que la recorta CON puntos
  // suspensivos, igual que la cabecera de un listado: como texto suelto de
  // la cabecera, `.slot-title` la corta sin decirlo —`overflow: hidden` y
  // nada más— y una ruta cortada en seco nombra otro directorio que además
  // existe.
  // Espacio DURO detrás del separador: `.slot-title` es un flex, y el
  // espacio normal al final de un nodo de texto se colapsa contra el span
  // de al lado — «Detalles ·⟨file⟩/…» pegado.
  const etiqueta = document.createTextNode(`${this.t("metadata-title")} ·\u00a0`);
  const ruta = document.createElement("span");
  ruta.className = "title-path";
  ruta.textContent = slot.follows_display;
  dom.title.replaceChildren(etiqueta, ruta);
  if (slot.follows_hostile) {
    ruta.append(badge(this.t("hostile-name")));
  }
  return this.paintMetadataBody(dom, slot);
}

/** El cuerpo de la hoja: los campos, o la nota que dice por qué no hay. */
export function paintMetadataBody(
  this: Screen,
  dom: SlotDom,
  slot: MetadataSlotView,
): void {
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
export function paintPreview(this: Screen, dom: SlotDom, slot: PreviewSlotView): void {
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
 * Las marcas de la cabecera del visor, en una línea.
 *
 * Aparte porque el desplazamiento las cambia (la fila y la columna) sin
 * cambiar nada más de la cabecera, y `paintViewer` las reescribe solas.
 */
function marcasDelVisor(screen: Screen, viewer: ViewerView): string {
  // Cada marca es un DATO que el host resolvió: encoding, fin de línea, si
  // lo forzó el usuario, si la decodificación tuvo errores, si el fichero
  // seguía. Ninguna se calcula aquí.
  const marcas = [viewer.encoding, viewer.eol];
  if (viewer.hex) {
    marcas.push("hex");
  }
  if (viewer.forced) {
    marcas.push(screen.t("viewer-forced"));
  }
  if (viewer.had_errors) {
    // `viewer-lossy`, que es como se llama esta marca en el catálogo desde
    // que existe el visor del TUI: inventar `viewer-errors` fue pedir una
    // clave que no está, y `t` contesta con la clave misma.
    marcas.push(screen.t("viewer-lossy"));
  }
  if (viewer.truncated) {
    marcas.push(screen.t("viewer-truncated"));
  }
  // La fila y la COLUMNA, en palabras: es lo que lee quien no ve las barras,
  // que son indicadores visuales y van `aria-hidden`.
  marcas.push(
    `${String(viewer.first_line + 1)}/${String(Math.max(1, viewer.total_rows))}`,
  );
  if (viewer.first_col > 0) {
    marcas.push(
      `${String(viewer.first_col + 1)}/${String(Math.max(1, viewer.total_cols))}`,
    );
  }
  return marcas.join(" · ");
}

/** El cuerpo del visor, con lo que lo hace enfocable y anunciable. */
function cuerpoDelVisor(viewer: ViewerView): HTMLElement {
  const body = viewerBody(viewer);
  body.setAttribute("tabindex", "-1");
  body.setAttribute("aria-describedby", `viewer-meta-${String(viewer.first_line)}`);
  return body;
}

/**
 * Las barras del visor: la vertical junto al cuerpo, en su lienzo; la
 * horizontal al pie de la caja.
 */
function ponerBarras(
  screen: Screen,
  viewer: ViewerView,
  box: HTMLElement,
  body: HTMLElement,
): void {
  // Ninguna barra sobre una IMAGEN. Un fichero de imagen se lee en
  // hexadecimal por debajo, así que las cuentas que llegan describen ese
  // volcado — y dibujarlas encima de la foto sería una barra hablando de un
  // contenido que no está en pantalla. La imagen se escala al hueco: no hay
  // nada que desplazar.
  if (viewer.image !== null) {
    return;
  }
  const vertical = viewerBar(
    true,
    viewer.total_rows,
    viewer.first_line,
    screen.viewerRows,
  );
  if (vertical !== null) {
    body.parentElement?.append(vertical);
  }
  const horizontal = viewerBar(
    false,
    viewer.total_cols,
    viewer.first_col,
    screen.viewerCols,
  );
  if (horizontal !== null) {
    box.append(horizontal);
  }
}

/**
 * `ahora` es `antes` desplazado: el mismo fichero leído igual, con la misma
 * cabecera salvo las marcas. Una imagen nunca, porque su cuerpo lo sustituye
 * la foto cuando llega.
 */
function desplazado(antes: ViewerView, ahora: ViewerView): boolean {
  return (
    antes.image === null &&
    ahora.image === null &&
    antes.path_display === ahora.path_display &&
    antes.path_hostile === ahora.path_hostile &&
    antes.preview_by === ahora.preview_by &&
    antes.preview_lossy === ahora.preview_lossy &&
    antes.image_refused === ahora.image_refused
  );
}
