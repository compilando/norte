// Pintor de `Screen` para la revisión de ORGANIZAR (fase 8): una función con
// `this: Screen`, enganchada como propiedad en `render.ts`. El estado sigue en
// la clase.

import type { Screen } from "../render";
import type { OrganizeView } from "../types";
import { badge } from "./dom";

/** La clase CSS de cada clase de línea del árbol. */
const CLASE = {
  new_dir: "organize-new-dir",
  existing_dir: "organize-existing-dir",
  moved: "organize-moved",
} as const;

/**
 * El plan de ORGANIZAR en revisión.
 *
 * Se pinta como un ÁRBOL y no como una lista de parejas porque lo que cambia
 * es la FORMA del directorio: cuántas carpetas aparecen, cuáles, y qué acaba
 * dentro de cada una. Eso es lo que se está aprobando, y una lista de cuarenta
 * `a.pdf → facturas/2026/a.pdf` no lo deja ver.
 *
 * La clase de cada línea llega como DATO (`kind`) y se pinta con una clase CSS
 * Y con un marcador de texto. Las dos cosas: el color dice «nueva» de un
 * vistazo, y el marcador sobrevive a un tema monocromo o a un lector de
 * pantalla. El marcador va en su propio elemento, jamás concatenado al nombre
 * — un fichero llamado `+ facturas` no puede disfrazarse de carpeta nueva.
 */
export function paintOrganize(this: Screen, plan: OrganizeView | null): void {
  if (plan === null) {
    this.organizeRoot.replaceChildren();
    this.organizeRoot.dataset["open"] = "false";
    return;
  }
  this.organizeRoot.dataset["open"] = "true";
  const caja = document.createElement("section");
  caja.className = "organize";
  caja.setAttribute("role", "dialog");
  caja.setAttribute("aria-modal", "true");
  const h = document.createElement("h2");
  h.id = "organize-title";
  h.textContent = this.t("modal-organize-plan");
  caja.setAttribute("aria-labelledby", h.id);
  caja.append(h);

  const donde = document.createElement("p");
  donde.className = "organize-dir";
  donde.textContent = plan.dir.text;
  donde.dataset["hostile"] = String(plan.dir.hostile);
  if (plan.dir.hostile) {
    donde.classList.add("hostile");
    donde.append(badge(this.t("hostile-name")));
  }
  caja.append(donde);

  // El RECUENTO va arriba, junto al directorio: es lo que se lee para decidir
  // sin contar líneas, y de todo el cuerpo es lo que no se puede perder si la
  // pantalla se queda corta.
  const resumen = document.createElement("p");
  resumen.className = "organize-summary";
  resumen.setAttribute("role", "status");
  resumen.textContent = plan.summary;
  caja.append(resumen);

  const arbol = document.createElement("ul");
  arbol.className = "organize-tree";
  for (const linea of plan.lines) {
    const fila = document.createElement("li");
    fila.className = `organize-line ${CLASE[linea.kind]}`;
    // El sangrado es un DATO del estilo, no espacios en el texto: un nombre
    // que empiece por espacios no puede fingir estar más adentro.
    fila.style.setProperty("--depth", String(linea.depth));
    const marca = document.createElement("span");
    marca.className = "organize-mark";
    marca.setAttribute("aria-hidden", "true");
    marca.textContent =
      linea.kind === "new_dir" ? "+" : linea.kind === "existing_dir" ? "·" : "→";
    const nombre = document.createElement("span");
    nombre.className = "organize-name";
    nombre.textContent = linea.text.text;
    nombre.dataset["hostile"] = String(linea.text.hostile);
    if (linea.text.hostile) {
      nombre.classList.add("hostile");
      nombre.append(badge(this.t("hostile-name")));
    }
    fila.append(marca, nombre);
    arbol.append(fila);
  }
  caja.append(arbol);

  if (plan.more_note !== "") {
    // Ya traducido y ya sustituido POR EL HOST, por lo mismo que en la
    // revisión de renombrar: el catálogo lleva las cadenas ya formateadas.
    const mas = document.createElement("p");
    mas.className = "organize-more";
    mas.textContent = plan.more_note;
    caja.append(mas);
  }
  if (plan.hidden_hostile) {
    const aviso = document.createElement("p");
    aviso.className = "organize-hidden-hostile hostile";
    aviso.setAttribute("role", "alert");
    aviso.textContent = this.t("modal-ai-rename-hidden-hostile");
    caja.append(aviso);
  }

  // Recorrer con el ratón. Aprobar exige haber llegado al final, así que sin
  // esto la pantalla era una que un lector sin teclado no podía aprobar nunca.
  if (plan.total > plan.lines.length) {
    arbol.addEventListener("wheel", (e) => {
      e.preventDefault();
      this.send({ action: "organize_scroll", down: e.deltaY > 0 });
    });
  }

  const botones = document.createElement("div");
  botones.className = "choices";
  // Las dos claves, LITERALES: una `t(variable)` es una clave que el barrido
  // del catálogo no puede seguir.
  const aplicar = document.createElement("button");
  aplicar.type = "button";
  aplicar.textContent = this.t("modal-ai-rename-apply");
  // Deshabilitado hasta haberlo leído entero, que es la única condición aquí:
  // el token vino CON el plan, así que no hay veredicto que esperar.
  aplicar.disabled = !plan.seen_all;
  aplicar.addEventListener("click", () => {
    this.send({ action: "organize_decide", approve: true });
  });
  const descartar = document.createElement("button");
  descartar.type = "button";
  descartar.textContent = this.t("modal-ai-rename-discard");
  descartar.addEventListener("click", () => {
    this.send({ action: "organize_decide", approve: false });
  });
  botones.append(aplicar, descartar);
  caja.append(botones);

  const pie = document.createElement("p");
  pie.className = "organize-hint";
  pie.textContent = this.t("gui-modal-ai-rename-plan-hint");
  caja.append(pie);
  this.organizeRoot.replaceChildren(caja);
}
