// Pintor de `Screen` para el panel de un PLUGIN (fase 3): función con
// `this: Screen`, enganchada como propiedad en `render.ts`. El estado sigue en
// la clase.

import type { Screen } from "../render";
import type { PanelSlotView, SpanView } from "../types";
import type { SlotDom } from "./dom";

/** Un tramo con estilo, como lo pinta el visor: el ROL gana al color.
 *
 *  El rol llega ya validado desde Rust contra lo que un plugin PUEDE pedir, y
 *  el texto ya viene enmascarado: aquí no se valida nada, se pinta. Por
 *  `textContent` y jamás por `innerHTML` — el texto es de un tercero. */
export function tramo(s: SpanView): HTMLElement {
  const el = document.createElement("span");
  el.textContent = s.text;
  if (s.role !== null && s.role !== undefined) {
    el.dataset["role"] = s.role;
  } else if (s.fg !== null && s.fg !== undefined) {
    el.style.color = s.fg;
  }
  if (s.bg !== null && s.bg !== undefined) {
    el.style.backgroundColor = s.bg;
  }
  return el;
}

/**
 * El panel que pinta un plugin: su marco, dentro de un borde de la ventana.
 *
 * El guest no dibuja, DESCRIBE: líneas con estilo y zonas pulsables. El borde,
 * el título y el foco los pone esta casa, que es lo que impide que un plugin
 * se haga pasar por otro panel.
 *
 * Una ZONA no ejecuta nada por su cuenta: manda la CELDA que se pulsó
 * (`panel_click`) y el host resuelve qué zona era y qué comando le toca, con
 * el mismo filtro que aplica el terminal. Por eso `HitView` no trae comando —
 * si lo trajera, el comando lo elegiría quien hable con el renderer.
 *
 * Sin marco todavía —la primera petición en vuelo, o el plugin falló— se pinta
 * el borde con su título y nada dentro: se sabe que el panel está y de quién
 * es. Lo que nunca hace es parpadear, porque el host conserva el último marco
 * mientras pide el siguiente.
 */
export function paintPanel(this: Screen, dom: SlotDom, slot: PanelSlotView): void {
  dom.root.setAttribute("aria-label", slot.title);
  dom.scroller.className = "panel-plugin";
  // El nodo se REUSA mientras la geometría del hueco no cambie, así que un
  // hueco que era un registro o un visor y pasa a ser este panel llega con el
  // manejador de rueda del anterior puesto: rodar aquí seguía mandando
  // `log_scroll` para este `slot_id`. El visor lo limpia por lo mismo.
  dom.scroller.onwheel = null;
  dom.title.replaceChildren(document.createTextNode(slot.title));

  // Las zonas, agrupadas por fila en UNA pasada: el marco puede traer 256
  // líneas y 128 zonas, y filtrar la lista entera por cada línea era recorrer
  // la lista 256 veces para pintar lo mismo.
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
    // Las zonas de ESTA fila se pintan encima, como botones sin cromo: el
    // marco es texto, y una zona es una región de ese texto que responde.
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
