// Pintor de `Screen` para la línea de tiempo del journal (puente 78, #359):
// función con `this: Screen`, enganchada como propiedad en `render.ts`.
//
// Las filas vienen YA pintables: el host agrupó los lotes, formateó la hora y
// tradujo la cola. Lo único que se decide aquí es cómo se ve un punto, que es
// lo único que de verdad cambia entre un terminal y una ventana.

import type { Screen } from "../render";
import type { TimelineSlotView } from "../types";
import type { SlotDom } from "./dom";
import { badge, revelar } from "./dom";

/**
 * Pinta la línea de tiempo de un hueco: una fila por mutación —o por lote—,
 * de la más nueva a la más vieja, con el cursor sobre el punto al que se
 * volvería, y al pie lo que se llevaría un `Enter` ahí.
 */
export function paintTimeline(this: Screen, dom: SlotDom, slot: TimelineSlotView): void {
  dom.root.setAttribute("aria-label", slot.title);
  dom.scroller.className = "timeline";
  // El nodo se REUSA mientras la geometría no cambie: un hueco que era otra
  // cosa llega con el manejador de rueda del anterior puesto.
  dom.scroller.onwheel = null;
  dom.title.replaceChildren(document.createTextNode(slot.title));

  if (slot.rows.length === 0) {
    const vacio = document.createElement("div");
    vacio.className = "empty";
    vacio.textContent = slot.empty;
    dom.scroller.replaceChildren(vacio);
    return;
  }

  const lista = document.createElement("ul");
  lista.className = "timeline-rows";
  lista.setAttribute("role", "listbox");
  for (const [i, r] of slot.rows.entries()) {
    const fila = document.createElement("li");
    fila.className = "timeline-row";
    fila.id = `timeline-${String(slot.slot_id)}-${String(i)}`;
    fila.setAttribute("role", "option");
    fila.setAttribute("aria-selected", String(slot.cursor === i));
    const hora = document.createElement("span");
    hora.className = "timeline-time";
    hora.textContent = r.time;
    // El punto lleva el COLOR del actor: «yo» contra «algo en mi nombre». Lo
    // tuyo se deshace desde aquí; lo de un agente, por otra puerta.
    const punto = document.createElement("span");
    punto.className = "timeline-dot";
    punto.dataset["actor"] =
      r.actor === "user" || r.actor === "agent" ? r.actor : "other";
    punto.setAttribute("aria-hidden", "true");
    punto.textContent = "●";
    fila.append(hora, punto);
    if (r.hostile) {
      // DELANTE, como en toda superficie donde se decide algo: el servidor ya
      // enmascaró el nombre, y esto es lo que impide leerlo como fiel.
      fila.append(badge(this.t("hostile-name")));
    }
    const verbo = document.createElement("span");
    verbo.className = "timeline-op";
    verbo.textContent = r.op;
    const ruta = document.createElement("span");
    ruta.className = "timeline-path";
    ruta.textContent = r.path;
    fila.append(verbo, ruta);
    if (r.tail !== "") {
      const cola = document.createElement("span");
      cola.className = "timeline-tail";
      cola.textContent = r.tail;
      fila.append(cola);
    }
    lista.append(fila);
  }
  if (slot.cursor !== null) {
    lista.setAttribute(
      "aria-activedescendant",
      `timeline-${String(slot.slot_id)}-${String(slot.cursor)}`,
    );
  }
  const pie = document.createElement("div");
  pie.className = "timeline-footer";
  pie.textContent = slot.footer;
  dom.scroller.replaceChildren(lista, pie);
  // La fila del cursor, a la vista: la siguiente página se pide al llegar a
  // la última cargada, y un cursor que baja sin verse no sabe dónde está.
  revelar(lista.querySelector('[aria-selected="true"]') ?? undefined);
}
