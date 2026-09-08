// Pintores de `Screen` para log (ola W10): funciones con `this: Screen`,
// enganchadas como propiedades en `render.ts`. El estado sigue en la clase.

import type { Screen } from "../render";
import type { LogSlotView } from "../types";
import { nota, chip, badge } from "./dom";
import type { SlotDom } from "./dom";

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
export function paintLog(this: Screen, dom: SlotDom, slot: LogSlotView): void {
  dom.root.setAttribute("aria-label", this.t("log-title"));
  dom.scroller.className = "log";
  dom.title.replaceChildren(
    document.createTextNode(this.t("log-title")),
    // La ETIQUETA, no el id de cable: el chip decía `trace` mientras los
    // botones de al lado decían «traza» y cada línea decía `trace` otra
    // vez. `TRACE` es lo que pinta el terminal, lo que se escribe en
    // `RUST_LOG` y lo que alguien busca con la vista en una lista larga.
    chip(`${this.t("log-level")}: ${slot.level_label ?? slot.level}`),
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
    nivel.textContent = l.level_label ?? l.level;
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
  const cuerpo: HTMLElement = slot.lines.length === 0 ? nota(this.t("log-empty")) : lista;
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
export function selectorDeFuente(this: Screen, slot: LogSlotView): HTMLElement {
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
export function crearControlesDeRegistro(this: Screen): HTMLElement {
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
export function scheduleLogRows(this: Screen, dom: SlotDom): void {
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
