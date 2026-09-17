// Pintor de `Screen` para el mapa de disco (puente 71, fase 4): función con
// `this: Screen`, enganchada como propiedad en `render.ts`.
//
// El treemap ya viene REPARTIDO por el host: aquí no se calcula nada. Es
// deliberado — un reparto hecho dos veces son dos repartos distintos en cuanto
// alguien toque un redondeo, y entonces el rectángulo que se ve y el que
// resuelve un clic dejan de ser el mismo.

import type { Screen } from "../render";
import type { DiskMapSlotView } from "../types";
import type { SlotDom } from "./dom";
// El MISMO tramo que pinta un panel de plugin, no una copia: es la conversión
// de un tramo estilado a DOM, y dos copias se separan en cuanto una aprenda
// algo que la otra no —un rol nuevo, otro enmascarado—.
import { tramo } from "./panel";

/**
 * Pinta el mapa de un hueco: sus líneas y un botón por rectángulo.
 *
 * El clic manda la CELDA (`panel_click`), no el hijo: quién es cada rectángulo
 * lo resuelve el host contra el marco que él mismo repartió. Si el nombre
 * viajara, habría que elegir entre la forma que se pinta —enmascarada, que no
 * identifica ningún fichero— y la reversible, y encima sería un nombre que
 * puede mandar cualquiera que hable con este renderer.
 */
export function paintDiskMap(this: Screen, dom: SlotDom, slot: DiskMapSlotView): void {
  const titulo = slot.measuring
    ? `${slot.title} — ${this.t("disk-map-measuring")}`
    : slot.title;
  dom.root.setAttribute("aria-label", titulo);
  dom.scroller.className = "disk-map";
  // El nodo se REUSA mientras la geometría no cambie, así que un hueco que era
  // un registro o un visor llega con el manejador de rueda del anterior
  // puesto. Mismo cuidado que el panel de plugin.
  dom.scroller.onwheel = null;
  dom.title.replaceChildren(document.createTextNode(titulo));

  // Las zonas, agrupadas por fila en UNA pasada: un mapa puede traer 256
  // líneas y 128 zonas, y filtrar la lista entera por cada línea sería
  // recorrerla 256 veces para pintar lo mismo.
  const porFila = new Map<number, typeof slot.hits>();
  for (const h of slot.hits) {
    const fila = porFila.get(h.row) ?? [];
    fila.push(h);
    porFila.set(h.row, fila);
  }

  const cuerpo = document.createElement("div");
  cuerpo.className = "panel-lines";
  for (const [fila, linea] of slot.lines.entries()) {
    const li = document.createElement("div");
    li.className = "panel-line";
    li.replaceChildren(...linea.map(tramo));
    for (const hit of porFila.get(fila) ?? []) {
      const boton = document.createElement("button");
      boton.type = "button";
      boton.className = "panel-hit";
      boton.style.setProperty("--hit-col", String(hit.col));
      boton.style.setProperty("--hit-width", String(hit.width));
      boton.addEventListener("click", () => {
        this.send({
          action: "panel_click",
          slot_id: slot.slot_id,
          row: hit.row,
          col: hit.col,
        });
      });
      li.append(boton);
    }
    cuerpo.append(li);
  }
  dom.scroller.replaceChildren(cuerpo);
}
