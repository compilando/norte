// Pintores de `Screen` para places (ola W10): funciones con `this: Screen`,
// enganchadas como propiedades en `render.ts`. El estado sigue en la clase.

import type { Screen } from "../render";
import type { PlacesSlotView, TreeSlotView } from "../types";
import { revelar, badge } from "./dom";
import { icono } from "./iconos";
import type { SlotDom } from "./dom";

/**
 * El árbol de directorios.
 *
 * Dos gestos distintos sobre la misma fila: el TRIÁNGULO pliega y despliega,
 * y el nombre NAVEGA. Un solo gesto obligaría a elegir cuál de las dos cosas
 * significa un click, y las dos hacen falta — mirar dentro de una rama sin
 * mover el listado es la mitad de para qué sirve un árbol.
 *
 * El árbol no se mueve al navegar: es lo que hace útil tenerlo abierto.
 */
export function paintTree(this: Screen, dom: SlotDom, slot: TreeSlotView): void {
  dom.root.setAttribute("aria-label", this.t("tree-title"));
  dom.title.textContent = this.t("tree-title");
  dom.scroller.className = "tree";
  const lista = document.createElement("ul");
  lista.className = "tree-rows";
  lista.setAttribute("role", "tree");
  for (const [i, r] of slot.rows.entries()) {
    const fila = document.createElement("li");
    fila.className = "tree-row";
    fila.id = `tree-row-${String(i)}`;
    fila.setAttribute("role", "treeitem");
    fila.setAttribute("aria-level", String(r.depth + 1));
    fila.setAttribute("aria-selected", String(slot.cursor === i));
    // La sangría, por variable: el CSS no puede multiplicar una profundidad
    // que solo existe en los datos.
    fila.style.setProperty("--depth", String(r.depth));
    const marca = document.createElement("span");
    marca.className = "tree-twisty";
    if (r.children === false) {
      // Una hoja no lleva triángulo, pero SÍ su hueco: sin él los nombres
      // de un mismo nivel no se alinean y el árbol deja de leerse como tal.
      marca.textContent = " ";
    } else {
      // `null` —todavía no se ha mirado— se pinta como plegada y no como
      // hoja: pintar «no tiene nada dentro» a algo que nadie ha leído es
      // una respuesta inventada.
      // Un chevrón que GIRA al desplegar, como en VS Code: la misma
      // marca en dos posiciones se lee como un interruptor.
      marca.textContent = "›";
      marca.dataset["expanded"] = String(r.expanded);
      fila.setAttribute("aria-expanded", String(r.expanded));
      marca.addEventListener("click", (ev) => {
        // Que no llegue al nombre: plegar no navega.
        ev.stopPropagation();
        this.send({
          action: "tree_toggle_row",
          row: i,
          generation: slot.generation,
        });
      });
    }
    // La carpeta, abierta o cerrada según la rama: es lo que hace que la
    // columna se lea como un árbol de un vistazo, como en VS Code.
    const carpeta = icono(document, r.expanded ? "fs:folder-open" : "fs:folder");
    const nombre = document.createElement("span");
    nombre.className = "tree-name";
    nombre.dataset["hostile"] = String(r.hostile);
    nombre.textContent = r.label;
    if (r.hostile) {
      nombre.append(badge(this.t("hostile-name")));
    }
    fila.addEventListener("click", () => {
      // La generación de ESTA pintada: los hijos de una rama llegan solos y
      // se insertan EN MEDIO, así que sin ella un click podía navegar a una
      // carpeta que nadie pulsó.
      this.send({
        action: "tree_activate_row",
        row: i,
        generation: slot.generation,
      });
    });
    if (carpeta !== null) {
      carpeta.classList.add("tree-icon");
    }
    fila.append(marca, carpeta ?? document.createElement("span"), nombre);
    lista.append(fila);
  }
  lista.setAttribute("aria-activedescendant", `tree-row-${String(slot.cursor)}`);
  dom.scroller.replaceChildren(lista);
  revelar(lista.querySelector(`#tree-row-${String(slot.cursor)}`) ?? undefined);
}

/**
 * La barra lateral de sitios: volúmenes y favoritos.
 *
 * Un click ELIGE Y ACTIVA, al contrario que las otras listas: una barra
 * lateral existe para ir a sitios, y un click que solo mueve un cursor
 * obliga a rematar con el teclado. Una cabecera pliega en vez de navegar,
 * que es lo que el host hace con ella.
 */
export function paintPlaces(this: Screen, dom: SlotDom, slot: PlacesSlotView): void {
  dom.root.setAttribute("aria-label", this.t("places-title"));
  dom.title.textContent = this.t("places-title");
  dom.scroller.className = "places";
  const lista = document.createElement("ul");
  lista.className = "places-rows";
  lista.setAttribute("role", "listbox");
  for (const [i, r] of slot.rows.entries()) {
    const fila = document.createElement("li");
    fila.className = "places-row";
    fila.id = `place-row-${String(i)}`;
    fila.dataset["row"] = r.row;
    fila.setAttribute("role", "option");
    fila.setAttribute("aria-selected", String(slot.cursor === i));
    fila.addEventListener("click", () => {
      // La generación de ESTA pintada. Los volúmenes llegan solos y se
      // insertan antes que los favoritos, así que sin ella un click podía
      // navegar a un sitio que nadie pulsó.
      this.send({
        action: "place_activate_row",
        row: i,
        generation: slot.generation,
      });
    });
    if (r.row === "header") {
      fila.setAttribute("aria-expanded", String(!r.folded));
      const marca = document.createElement("span");
      marca.className = "places-fold";
      marca.textContent = r.folded ? "▸" : "▾";
      const texto = document.createElement("span");
      texto.className = "places-header";
      texto.textContent = r.label;
      fila.append(marca, texto);
    } else if (r.row === "drive") {
      // UNA línea (captura del 2026-09-21): icono por clase de unidad, el
      // nombre CORTO y el libre corto a la derecha, como en la TUI. El
      // montaje entero y la frase del espacio van en el título.
      fila.dataset["kind"] = r.kind ?? "unknown";
      if (r.free !== undefined) {
        fila.dataset["line"] = "one";
      }
      fila.title = [r.mount ?? "", r.detail].filter((s) => s !== "").join("\n");
      const dibujo = icono(
        document,
        r.kind === "removable"
          ? "fs:removable"
          : r.kind === "network"
            ? "fs:network"
            : "fs:drive",
      );
      if (dibujo !== null) {
        dibujo.classList.add("places-icon");
        fila.append(dibujo);
      }
      const nombre = document.createElement("span");
      nombre.className = "places-name";
      nombre.dataset["hostile"] = String(r.hostile);
      nombre.textContent = r.label;
      if (r.hostile) {
        nombre.append(badge(this.t("hostile-name")));
      }
      const detalle = document.createElement("span");
      detalle.className = "places-detail";
      detalle.textContent = r.free ?? r.detail;
      fila.append(nombre, detalle);
    } else {
      // Un favorito, en UNA línea: estrella y nombre; el destino va en el
      // título. Uno roto sigue diciendo por qué, debajo y en rojo.
      const estrella = icono(document, "fs:favorite");
      if (estrella !== null) {
        estrella.classList.add("places-icon");
        fila.append(estrella);
      }
      const nombre = document.createElement("span");
      nombre.className = "places-name";
      nombre.textContent = r.name;
      fila.append(nombre);
      if (r.broken === "") {
        fila.dataset["line"] = "one";
        fila.title = r.target;
        if (r.hostile) {
          // El destino no se ve en la fila, así que la marca va al nombre:
          // un favorito que apunta a un nombre enmascarado lo DICE.
          nombre.dataset["hostile"] = "true";
          nombre.append(badge(this.t("hostile-name")));
        }
      } else {
        // Un favorito roto se PINTA con su motivo: uno que desaparece en
        // silencio es un fallo de configuración que nadie puede ver.
        const roto = document.createElement("span");
        roto.className = "places-broken";
        roto.textContent = r.broken;
        fila.append(roto);
      }
    }
    lista.append(fila);
  }
  lista.setAttribute("aria-activedescendant", `place-row-${String(slot.cursor)}`);
  dom.scroller.replaceChildren(lista);
  revelar(lista.querySelector(`#place-row-${String(slot.cursor)}`) ?? undefined);
}
