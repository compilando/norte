// Pintor de `Screen` para el panel de terminal (#362, puente 95): función con
// `this: Screen`, enganchada como propiedad en `render.ts`. El estado sigue en
// la clase.

import type { Screen } from "../render";
import type { TerminalColorView, TerminalSlotView, TerminalSpanView } from "../types";
import { nota } from "./dom";
import type { SlotDom } from "./dom";

/**
 * El panel de terminal: la rejilla que el host ya emuló.
 *
 * Lo que llega son FILAS YA PINTADAS, no los bytes del pty. La emulación la
 * hace `norte-term` del lado del host —el mismo crate que usa la terminal—,
 * así que los dos frontends enseñan lo mismo por construcción y no porque
 * alguien compare dos emuladores.
 *
 * **Esto es contenido AJENO.** No lleva ni un rol del tema, y no debe: lo que
 * un programa pinta dentro es suyo, y teñirlo con el tema sería mentir sobre
 * lo que ese programa dijo. Lo nuestro es el marco, que lo pone el hueco.
 *
 * Tampoco hay que sanear nada aquí, y no es un descuido: lo que sale de la
 * rejilla no puede llevar un byte de control, porque el parser se come los
 * escapes y tira los C0 que no mueven el cursor. Se pinta con
 * `textContent`, así que tampoco hay HTML que pueda colarse.
 */
export function paintTerminal(this: Screen, dom: SlotDom, slot: TerminalSlotView): void {
  dom.root.setAttribute("aria-label", this.t("panelbar-terminal"));
  dom.scroller.className = "terminal";
  dom.title.replaceChildren(document.createTextNode(this.t("panelbar-terminal")));
  if (slot.no_shell) {
    // Un panel en blanco y un panel sin shell se ven igual y no son lo mismo.
    dom.scroller.replaceChildren(nota(this.t("terminal-none")));
    return;
  }
  const filas = slot.rows.map((fila, y) => pintaFila(fila, y, slot.cursor));
  dom.scroller.replaceChildren(...filas);
}

/** Una fila: sus fragmentos, más el cursor si cae en ella. */
function pintaFila(
  fila: TerminalSpanView[],
  y: number,
  cursor: [number, number] | null,
): HTMLElement {
  const linea = document.createElement("div");
  linea.className = "terminal-row";
  // El cursor se pinta partiendo el fragmento donde cae, y no con una capa
  // encima: una capa posicionada por columnas supone que todas las celdas
  // miden lo mismo, y con un carácter ancho deja de ser verdad.
  const col = cursor !== null && cursor[0] === y ? cursor[1] : null;
  let x = 0;
  for (const span of fila) {
    // `Array.from` y no `split("")`: partir por unidades UTF-16 rompe un
    // emoji por la mitad y deja dos mitades que no son caracteres.
    const chars = Array.from(span.text);
    if (col === null || col < x || col >= x + chars.length) {
      linea.append(pintaSpan(span, span.text, false));
      x += chars.length;
      continue;
    }
    const corte = col - x;
    if (corte > 0) {
      linea.append(pintaSpan(span, chars.slice(0, corte).join(""), false));
    }
    linea.append(pintaSpan(span, chars[corte] ?? " ", true));
    if (corte + 1 < chars.length) {
      linea.append(pintaSpan(span, chars.slice(corte + 1).join(""), false));
    }
    x += chars.length;
  }
  // El cursor detrás del último fragmento —o en una fila vacía— sigue siendo
  // un sitio donde va: sin esto no se ve en un prompt recién pintado.
  if (col !== null && col >= x) {
    const hueco = document.createElement("span");
    hueco.className = "terminal-cursor";
    hueco.textContent = " ";
    linea.append(hueco);
  }
  return linea;
}

function pintaSpan(
  span: TerminalSpanView,
  texto: string,
  esCursor: boolean,
): HTMLElement {
  const el = document.createElement("span");
  // `textContent` y nunca `innerHTML`: esto lo escribió otro programa.
  el.textContent = texto;
  if (esCursor) {
    el.classList.add("terminal-cursor");
  }
  // `reverse` se resuelve AQUÍ, intercambiando los dos colores: el host lo
  // manda como bandera justamente para no perder cuál era cuál.
  //
  // Y el intercambio tiene que valer también cuando uno de los dos NO está.
  // Un `ls` que invierte para marcar algo no manda colores: manda `SGR 7` a
  // secas, y lo que espera es el papel al revés. Sin los dos colores por
  // defecto explícitos, eso se quedaba en nada visible.
  const fg = span.reverse ? span.bg : span.fg;
  const bg = span.reverse ? span.fg : span.bg;
  if (fg !== undefined) {
    el.style.color = css(fg);
  } else if (span.reverse) {
    el.style.color = "var(--term-bg)";
  }
  if (bg !== undefined) {
    el.style.background = css(bg);
  } else if (span.reverse) {
    el.style.background = "var(--term-fg)";
  }
  if (span.bold) el.style.fontWeight = "bold";
  if (span.dim) el.style.opacity = "0.65";
  if (span.italic) el.style.fontStyle = "italic";
  if (span.underline) el.style.textDecoration = "underline";
  if (span.strike) {
    el.style.textDecoration = span.underline ? "underline line-through" : "line-through";
  }
  return el;
}

/**
 * El color de un fragmento, como CSS.
 *
 * Un índice sale como `var(--term-N)`: la paleta la define el TEMA, que es
 * quien tiene que decidir qué azul es el «color 4». Por eso el host lo manda
 * sin resolver — si lo hubiera resuelto él, esta línea no existiría y el panel
 * no obedecería al tema.
 *
 * Un `#rrggbb` lo eligió el programa y va tal cual: ahí no hay nada que
 * decidir.
 */
function css(color: TerminalColorView): string {
  return color.kind === "indexed" ? `var(--term-${color.index})` : color.hex;
}
