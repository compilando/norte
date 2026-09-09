// Pintores de `Screen` para search (ola W10): funciones con `this: Screen`,
// enganchadas como propiedades en `render.ts`. El estado sigue en la clase.

import type { Screen } from "../render";
import type { SearchView } from "../types";
import { revelar, badge } from "./dom";

export function paintSearch(this: Screen, search: SearchView | null): void {
  if (search === null) {
    this.searchRoot.replaceChildren();
    this.searchRoot.dataset["open"] = "false";
    return;
  }
  this.searchRoot.dataset["open"] = "true";
  const caja = document.createElement("section");
  caja.className = "search";
  caja.setAttribute("role", "dialog");
  caja.setAttribute("aria-modal", "true");
  // Una búsqueda por significado no recorre un subárbol: su alcance es el
  // índice entero, y titularla como la otra prometería lo que no hay.
  const rotulo = search.semantic
    ? this.t("search-title-semantic")
    : this.t("search-title");
  caja.setAttribute("aria-label", rotulo);

  const titulo = document.createElement("h1");
  titulo.textContent = `${rotulo} · ${search.query}`;
  caja.append(titulo);

  if (search.semantic) {
    const alcance = document.createElement("p");
    alcance.className = "search-root";
    alcance.textContent = this.t("modal-semantic-scope");
    caja.append(alcance);
  } else {
    const donde = document.createElement("p");
    donde.className = "search-root";
    donde.dataset["hostile"] = String(search.root_hostile);
    donde.textContent = search.root;
    if (search.root_hostile) {
      donde.append(badge(this.t("hostile-name")));
    }
    caja.append(donde);
  }

  const estado = document.createElement("p");
  estado.className = "search-status";
  estado.dataset["running"] = String(search.running);
  // `status` mientras corre: un lector de pantalla anuncia el avance sin
  // robarle el foco a lo que el usuario esté haciendo.
  estado.setAttribute("role", "status");
  estado.setAttribute("aria-live", "polite");
  estado.textContent = search.status;
  caja.append(estado);

  const lista = document.createElement("ul");
  lista.className = "search-rows";
  lista.setAttribute("role", "listbox");
  for (const [i, r] of search.rows.entries()) {
    const fila = document.createElement("li");
    fila.className = "search-row";
    fila.id = `search-row-${String(i)}`;
    fila.setAttribute("role", "option");
    fila.setAttribute("aria-selected", String(search.cursor === i));
    fila.dataset["dir"] = String(r.is_dir);
    fila.addEventListener("click", () => {
      this.send({ action: "search_activate_row", row: i });
    });
    const nombre = document.createElement("span");
    nombre.className = "search-name";
    nombre.dataset["hostile"] = String(r.hostile);
    nombre.textContent = r.name;
    if (r.hostile) {
      nombre.append(badge(this.t("hostile-name")));
    }
    const padre = document.createElement("span");
    padre.className = "search-parent";
    padre.dataset["hostile"] = String(r.parent_hostile);
    padre.textContent = r.parent;
    fila.append(nombre, padre);
    if (r.score !== null) {
      // El parecido, en su propia celda: sin él, un 0,91 y un 0,42 se leen
      // igual de buenos y el orden parece arbitrario. Dos decimales, que es
      // lo que distingue sin fingir precisión.
      const parecido = document.createElement("span");
      parecido.className = "search-score";
      parecido.textContent = r.score.toFixed(2);
      fila.append(parecido);
    }
    lista.append(fila);
  }
  if (search.cursor !== null) {
    lista.setAttribute("aria-activedescendant", `search-row-${String(search.cursor)}`);
  }
  caja.append(lista);
  this.searchRoot.replaceChildren(caja);
  if (search.cursor !== null) {
    revelar(lista.querySelector(`#search-row-${String(search.cursor)}`) ?? undefined);
  }
}
