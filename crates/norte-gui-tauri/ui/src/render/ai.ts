// Pintores de `Screen` para ai (ola W10): funciones con `this: Screen`,
// enganchadas como propiedades en `render.ts`. El estado sigue en la clase.

import type { Screen } from "../render";
import type { AiRenameView } from "../types";
import { badge } from "./dom";

/**
 * El plan de renombrado en revisión.
 *
 * Los dos nombres de cada pareja van en ELEMENTOS distintos, jamás
 * concatenados con una flecha: un nombre puede contener la flecha, y la
 * fila se leería como otra pareja. El separador lo pone el CSS, que un
 * nombre no puede escribir.
 */
export function paintAiRename(this: Screen, plan: AiRenameView | null): void {
  if (plan === null) {
    this.aiRenameRoot.replaceChildren();
    this.aiRenameRoot.dataset["open"] = "false";
    return;
  }
  this.aiRenameRoot.dataset["open"] = "true";
  const caja = document.createElement("section");
  caja.className = "ai-rename";
  caja.setAttribute("role", "dialog");
  caja.setAttribute("aria-modal", "true");
  const h = document.createElement("h2");
  h.id = "ai-rename-title";
  h.textContent = this.t("modal-ai-rename-plan");
  caja.setAttribute("aria-labelledby", h.id);
  caja.append(h);

  const donde = document.createElement("p");
  donde.className = "ai-rename-dir";
  donde.textContent = plan.dir.text;
  donde.dataset["hostile"] = String(plan.dir.hostile);
  if (plan.dir.hostile) {
    donde.classList.add("hostile");
    donde.append(badge(this.t("hostile-name")));
  }
  caja.append(donde);

  // El VEREDICTO va arriba, junto al directorio: de todo el cuerpo es la
  // línea que no se puede perder si la pantalla se queda corta.
  const estado = document.createElement("p");
  estado.className = "ai-rename-status";
  estado.dataset["confirmable"] = String(plan.confirmable);
  estado.setAttribute("role", "status");
  estado.textContent = plan.status;
  caja.append(estado);

  const lista = document.createElement("ol");
  lista.className = "ai-rename-pairs";
  lista.setAttribute("start", String(plan.first_visible + 1));
  for (const par of plan.pairs) {
    const fila = document.createElement("li");
    fila.className = "ai-rename-pair";
    // Los dos nombres en LÍNEAS distintas, y la segunda con su propio
    // color. Ponerlos en la misma línea separados por una flecha los
    // separaba con un glifo que un nombre puede contener: `cap 2 → final`
    // se leía como una pareja distinta de la que es. La numeración la pinta
    // el `<ol>`, que un nombre tampoco puede falsificar.
    for (const [clase, linea] of [
      ["ai-rename-from", par.from],
      ["ai-rename-to", par.to],
    ] as const) {
      const el = document.createElement("div");
      el.className = clase;
      el.textContent = linea.text;
      el.dataset["hostile"] = String(linea.hostile);
      if (linea.hostile) {
        el.classList.add("hostile");
        el.append(badge(this.t("hostile-name")));
      }
      fila.append(el);
    }
    lista.append(fila);
  }
  caja.append(lista);

  if (plan.more_note !== "") {
    // Ya traducido y ya sustituido POR EL HOST. Sustituirlo aquí no
    // funcionaba: el catálogo lleva las cadenas ya formateadas y sin
    // argumentos, y Fluent escribe una variable ausente como `{$shown}` —
    // sin espacios—, así que el `.replace` no casaba nunca y la línea que
    // dice cuánto del plan se está viendo pintaba dos identificadores.
    const mas = document.createElement("p");
    mas.className = "ai-rename-more";
    mas.textContent = plan.more_note;
    caja.append(mas);
  }
  if (plan.hidden_hostile) {
    // Lo que se enmascara se dice TAMBIÉN cuando no cabe en la ventana: la
    // marca de una línea solo existe para esa línea, y la pareja alterada
    // puede estar en la posición doce.
    const aviso = document.createElement("p");
    aviso.className = "ai-rename-hidden-hostile hostile";
    aviso.setAttribute("role", "alert");
    aviso.textContent = this.t("modal-ai-rename-hidden-hostile");
    caja.append(aviso);
  }

  for (const linea of plan.detail) {
    const p = document.createElement("p");
    p.className = "ai-rename-detail";
    p.textContent = linea.text;
    p.dataset["hostile"] = String(linea.hostile);
    if (linea.hostile) {
      p.classList.add("hostile");
      p.append(badge(this.t("hostile-name")));
    }
    caja.append(p);
  }

  if (plan.real_steps_note !== "") {
    // Cuántos renombra DE VERDAD: el planificador tira las parejas nulas, y
    // enseñar solo las pedidas promete de más.
    const reales = document.createElement("p");
    reales.className = "ai-rename-real";
    reales.textContent = plan.real_steps_note;
    caja.append(reales);
  }

  // Botones, y no solo teclas. Un clic es un gesto DIRIGIDO a esta
  // pantalla, así que no necesita el reconocimiento que sí necesita una
  // tecla; y sin ellos un lector con el ratón no podía ni quitarse de
  // encima una pantalla que se abrió sola.
  const botones = document.createElement("div");
  botones.className = "choices";
  // Las dos claves, LITERALES: una `t(variable)` es una clave que el
  // barrido del catálogo no puede seguir, y una clave que no se sigue se
  // pinta como su propio identificador el día que falte.
  const aplicar = document.createElement("button");
  aplicar.type = "button";
  aplicar.textContent = this.t("modal-ai-rename-apply");
  aplicar.disabled = !plan.confirmable;
  aplicar.addEventListener("click", () => {
    this.send({ action: "ai_rename_decide", approve: true });
  });
  const descartar = document.createElement("button");
  descartar.type = "button";
  descartar.textContent = this.t("modal-ai-rename-discard");
  descartar.addEventListener("click", () => {
    this.send({ action: "ai_rename_decide", approve: false });
  });
  botones.append(aplicar, descartar);
  caja.append(botones);

  const pie = document.createElement("p");
  pie.className = "ai-rename-hint";
  pie.textContent = this.t("gui-modal-ai-rename-plan-hint");
  caja.append(pie);
  this.aiRenameRoot.replaceChildren(caja);
}
