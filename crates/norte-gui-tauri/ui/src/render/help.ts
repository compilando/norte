// Pintores de `Screen` para help (ola W10): funciones con `this: Screen`,
// enganchadas como propiedades en `render.ts`. El estado sigue en la clase.

import type { Screen } from "../render";
import type { HelpBlockView, HelpSpanView, HelpView } from "../types";
import { revelar } from "./dom";

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
export function paintHelp(this: Screen, help: HelpView | null): void {
  if (help === null) {
    this.helpRoot.replaceChildren();
    this.helpRoot.dataset["open"] = "false";
    this.helpBodyFocused = false;
    this.helpPintada = null;
    return;
  }
  // Dónde iba leyendo, para devolvérselo. El cuerpo se reconstruye entero
  // en CADA parche —y mover el cursor de la lateral es un parche—, así que
  // sin esto leer media página y pulsar `↓` devolvía el scroll a cero.
  // Solo dentro de la MISMA página: cambiar de página empieza arriba, que
  // es lo que hace cualquier lector.
  const scroll =
    this.helpPintada === help.topic_id
      ? (this.helpRoot.querySelector(".help-body")?.scrollTop ?? 0)
      : 0;
  this.helpPintada = help.topic_id;
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
  if (scroll > 0) {
    const cuerpo = this.helpRoot.querySelector(".help-body");
    if (cuerpo instanceof HTMLElement) {
      cuerpo.scrollTop = scroll;
    }
  }
}

/**
 * Desplaza el cuerpo de la ayuda con una tecla, si es de las que lo mueven.
 * Devuelve si la consumió.
 *
 * Lo hace el renderer y no el DOM «por su cuenta»: el scroll nativo de
 * teclado necesita que la caja tenga el foco del documento, y el cuerpo se
 * reconstruye en cada parche —nadie le devolvía el foco—, así que `PgDn`
 * solo desplazaba después de un clic dentro. Tampoco el host: el cuerpo
 * cruza entero y quien sabe lo que mide es esta caja (#267).
 *
 * Las flechas solo cuando la página no tiene nada ejecutable (la hoja de
 * teclado, la más larga): con acciones, las flechas eligen una y son del
 * host, que la revela.
 */
export function desplazarAyuda(this: Screen, key: string): boolean {
  const cuerpo = this.helpRoot.querySelector(".help-body");
  if (!(cuerpo instanceof HTMLElement)) {
    return false;
  }
  // Una línea de prosa, y una página con dos líneas de solape para no
  // perder el sitio en el salto.
  const linea = parseFloat(getComputedStyle(cuerpo).lineHeight) || 16;
  const pagina = Math.max(linea, cuerpo.clientHeight - 2 * linea);
  const sinAcciones = cuerpo.querySelector(".help-actions") === null;
  switch (key) {
    case "PageDown":
      cuerpo.scrollTop += pagina;
      return true;
    case "PageUp":
      cuerpo.scrollTop -= pagina;
      return true;
    case "Home":
      cuerpo.scrollTop = 0;
      return true;
    case "End":
      cuerpo.scrollTop = cuerpo.scrollHeight;
      return true;
    case "ArrowDown":
      if (sinAcciones) {
        cuerpo.scrollTop += linea;
        return true;
      }
      return false;
    case "ArrowUp":
      if (sinAcciones) {
        cuerpo.scrollTop -= linea;
        return true;
      }
      return false;
    default:
      return false;
  }
}

/** La lateral: cabeceras de grupo y páginas. */
export function helpSidebar(this: Screen, help: HelpView): HTMLElement {
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
      // La lateral corta con elipsis los títulos largos; el completo, al
      // pasar por encima. `title` es texto: no se interpreta como marcado.
      fila.title = r.title;
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
export function helpBody(this: Screen, help: HelpView): HTMLElement {
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
export function helpBlock(this: Screen, b: HelpBlockView): HTMLElement {
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
export function helpSpan(this: Screen, s: HelpSpanView): HTMLElement {
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
