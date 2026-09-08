// Pintores de `Screen` para sync (ola W10): funciones con `this: Screen`,
// enganchadas como propiedades en `render.ts`. El estado sigue en la clase.

import type { Screen } from "../render";
import type { CompareFaceView, CompareView, SyncStepView, SyncView } from "../types";
import { badge, veredicto } from "./dom";

/** El panel de sincronización: el PLAN. Se pinta en el mismo hueco que el
 *  de diferencias — son dos pantallas enteras y no coinciden. */
export function paintSync(this: Screen, sync: SyncView | null): void {
  if (sync === null) {
    if (this.syncRoot.dataset["open"] === "true") {
      this.syncRoot.replaceChildren();
      this.syncRoot.dataset["open"] = "false";
    }
    return;
  }
  this.syncRoot.dataset["open"] = "true";
  const caja = document.createElement("section");
  caja.className = "sync";
  caja.setAttribute("role", "dialog");
  caja.setAttribute("aria-modal", "true");
  caja.setAttribute("aria-label", this.t("sync-title"));

  // El MODO, arriba y en su propio elemento: un espejo borra en el destino
  // y una actualización no, y quien aprueba tiene que verlo antes.
  const modo = document.createElement("p");
  modo.className = "sync-mode";
  modo.dataset["mode"] = sync.mode;
  // Claves LITERALES: una interpolada no la ve el barrido que comprueba
  // que toda clave existe, y una clave que falta se pinta como su propio
  // identificador.
  modo.textContent =
    sync.mode === "mirror"
      ? this.t("gui-sync-mode-mirror")
      : this.t("gui-sync-mode-update");
  caja.append(modo);

  const raices = document.createElement("div");
  raices.className = "sync-roots";
  for (const raiz of [sync.source, sync.dest]) {
    const r = document.createElement("span");
    r.className = "sync-root";
    r.dataset["hostile"] = String(raiz.hostile);
    r.textContent = raiz.text;
    if (raiz.hostile) {
      r.append(badge(this.t("hostile-name")));
    }
    raices.append(r);
  }
  caja.append(raices);

  if (sync.summary.length > 0) {
    // El RESUMEN, arriba: cuántos pasos no se pueden deshacer, cuántos
    // bytes, qué no se pudo leer. Es lo que se lee antes de aprobar, y
    // debajo de la lista no lo lee nadie.
    const resumen = document.createElement("ul");
    resumen.className = "sync-summary";
    for (const linea of sync.summary) {
      const li = document.createElement("li");
      li.textContent = linea;
      resumen.append(li);
    }
    caja.append(resumen);
  }
  if (sync.blockers.length > 0) {
    // Lo que IMPIDE aplicar va como ALERTA y arriba: un plan que no se
    // puede ejecutar tiene que decir por qué antes que enseñar sus pasos.
    const lista = document.createElement("ul");
    lista.className = "sync-blockers";
    lista.setAttribute("role", "alert");
    for (const b of sync.blockers) {
      const li = document.createElement("li");
      const que = document.createElement("span");
      que.className = "sync-blocker-label";
      que.textContent = b.label;
      // La ruta en su propio elemento: «el destino es de solo lectura» sin
      // decir CUÁL manda a buscar el problema a ciegas.
      const donde = document.createElement("span");
      donde.className = "sync-blocker-path";
      donde.dataset["hostile"] = String(b.path_hostile);
      donde.textContent = b.path;
      li.append(que, donde);
      if (b.path_hostile) {
        li.append(badge(this.t("hostile-name")));
      }
      lista.append(li);
    }
    if (sync.blockers_total > sync.blockers.length) {
      // El wire recorta la lista: que hay cuarenta mil y se enseñan
      // doscientos cincuenta y seis tiene que decirse.
      const mas = document.createElement("li");
      mas.className = "sync-blockers-more";
      mas.textContent = `${String(sync.blockers.length)} / ${String(sync.blockers_total)}`;
      lista.append(mas);
    }
    caja.append(lista);
  }

  const pasos = document.createElement("ol");
  pasos.className = "sync-steps";
  pasos.setAttribute("role", "list");
  // La numeración arranca donde arranca la VENTANA: la lista no es el plan
  // entero, y pintarla desde uno la haría pasar por él.
  pasos.setAttribute("start", String(sync.first_visible + 1));
  if (sync.total > sync.steps.length) {
    pasos.dataset["window"] = `${String(sync.first_visible + 1)}-${String(
      sync.first_visible + sync.steps.length,
    )}/${String(sync.total)}`;
  }
  for (const p of sync.steps) {
    pasos.append(this.syncStep(p));
  }
  caja.append(pasos);

  if (sync.failures.length > 0) {
    // Lo que FALLÓ, uno a uno: el recuento va en el estado, y «3 fallaron»
    // sin decir cuáles no se puede arreglar.
    const fallos = document.createElement("ul");
    fallos.className = "sync-failures";
    fallos.setAttribute("role", "alert");
    for (const f of sync.failures) {
      const li = document.createElement("li");
      li.dataset["anchor"] = f.anchor;
      const causa = document.createElement("span");
      causa.className = "sync-failure-cause";
      causa.textContent = f.cause;
      const ruta = document.createElement("span");
      ruta.className = "sync-failure-path";
      ruta.dataset["hostile"] = String(f.path_hostile);
      ruta.textContent = f.path;
      li.append(causa, ruta);
      if (f.anchor_label !== "") {
        const ancla = document.createElement("span");
        ancla.className = "sync-failure-anchor";
        ancla.textContent = f.anchor_label;
        li.append(ancla);
      }
      if (f.path_hostile) {
        li.append(badge(this.t("hostile-name")));
      }
      fallos.append(li);
    }
    caja.append(fallos);
  }
  if (sync.confirming !== null) {
    // La SEGUNDA pregunta, como alerta y con su propio elemento: es la
    // última pantalla donde todavía se puede decir que no.
    const pregunta = document.createElement("p");
    pregunta.className = "sync-confirm";
    pregunta.setAttribute("role", "alertdialog");
    pregunta.textContent = sync.confirming;
    caja.append(pregunta);
  }
  const estado = document.createElement("p");
  estado.className = "sync-status";
  estado.setAttribute("role", "status");
  estado.setAttribute("aria-live", "polite");
  estado.dataset["running"] = String(sync.running);
  estado.dataset["approvable"] = String(sync.can_approve);
  estado.dataset["cancelRequested"] = String(sync.cancel_requested);
  estado.textContent = sync.status;
  caja.append(estado);

  const pie = document.createElement("p");
  pie.className = "sync-hint";
  pie.textContent = sync.hint;
  caja.append(pie);
  this.syncRoot.replaceChildren(caja);
}

/** Un paso del plan: qué hace, sobre qué, y si el deshacer lo devuelve. */
export function syncStep(this: Screen, p: SyncStepView): HTMLElement {
  const li = document.createElement("li");
  li.className = "sync-step";
  li.id = `sync-step-${String(p.id)}`;
  li.dataset["anchor"] = p.anchor;
  const kind = document.createElement("span");
  kind.className = "sync-step-kind";
  kind.textContent = p.kind;
  const ruta = document.createElement("span");
  ruta.className = "sync-step-path";
  ruta.dataset["hostile"] = String(p.path_hostile);
  ruta.textContent = p.path;
  li.append(kind, ruta);
  if (p.anchor_label !== "") {
    // El ancla se DICE, no se deduce de un `data-anchor` que nadie lee.
    const ancla = document.createElement("span");
    ancla.className = "sync-step-anchor";
    ancla.textContent = p.anchor_label;
    li.append(ancla);
  }
  if (p.path_hostile) {
    li.append(badge(this.t("hostile-name")));
  }
  if (p.dest_path !== null) {
    // La ortografía del DESTINO en su propio elemento: la escritura cae
    // sobre ESTA, y juntarlas en una celda deja que un nombre imite a otro.
    const dest = document.createElement("span");
    dest.className = "sync-step-dest";
    dest.dataset["hostile"] = String(p.dest_path_hostile);
    dest.textContent = p.dest_path;
    li.append(dest);
    if (p.dest_path_hostile) {
      li.append(badge(this.t("hostile-name")));
    }
    if (p.twins) {
      // Las dos se rinden IGUAL: sin decirlo, el panel parece repetirse.
      const gemelas = document.createElement("span");
      gemelas.className = "sync-step-twins";
      gemelas.textContent = this.t("sync-dest-twin");
      li.append(gemelas);
    }
  }
  const undo = document.createElement("span");
  undo.className = "sync-step-undo";
  undo.textContent = p.undo;
  li.append(undo);
  if (p.reason !== "") {
    const por = document.createElement("span");
    por.className = "sync-step-reason";
    por.textContent = p.reason;
    li.append(por);
  }
  return li;
}

/** El panel de diferencias. Comparte hueco con la búsqueda: los dos son
 *  pantallas enteras y no se pintan a la vez. */
export function paintCompare(this: Screen, compare: CompareView | null): void {
  if (compare === null) {
    if (this.compareRoot.dataset["open"] === "true") {
      this.compareRoot.replaceChildren();
      this.compareRoot.dataset["open"] = "false";
    }
    return;
  }
  this.compareRoot.dataset["open"] = "true";
  const caja = document.createElement("section");
  caja.className = "compare";
  caja.setAttribute("role", "dialog");
  caja.setAttribute("aria-modal", "true");
  caja.setAttribute("aria-label", this.t("compare-title"));

  const cabecera = document.createElement("div");
  cabecera.className = "compare-roots";
  for (const [texto, hostil] of [
    [compare.left, compare.left_hostile],
    [compare.right, compare.right_hostile],
  ] as [string, boolean][]) {
    const raiz = document.createElement("span");
    raiz.className = "compare-root";
    raiz.dataset["hostile"] = String(hostil);
    raiz.textContent = texto;
    if (hostil) {
      raiz.append(badge(this.t("hostile-name")));
    }
    cabecera.append(raiz);
  }
  caja.append(cabecera);

  const filtros = document.createElement("div");
  filtros.className = "compare-filters";
  for (const f of compare.filters) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "compare-filter";
    b.dataset["hidden"] = String(f.hidden);
    b.setAttribute("aria-pressed", String(!f.hidden));
    b.textContent = `${f.label} (${String(f.count)})`;
    b.addEventListener("click", () => {
      this.send({ action: "compare_toggle_filter", category: f.id });
    });
    filtros.append(b);
  }
  caja.append(filtros);

  const lista = document.createElement("ul");
  lista.className = "compare-rows";
  lista.setAttribute("role", "listbox");
  for (const r of compare.rows) {
    const fila = document.createElement("li");
    fila.className = "compare-row";
    // El id, no la posición: es la identidad de la fila y lo que el host
    // espera de vuelta.
    fila.id = `compare-row-${String(r.id)}`;
    fila.dataset["category"] = r.category;
    fila.setAttribute("role", "option");
    fila.setAttribute("aria-selected", String(compare.selected === r.id));
    fila.addEventListener("click", () => {
      this.send({ action: "compare_select_row", id: r.id });
    });
    fila.addEventListener("dblclick", () => {
      this.send({ action: "compare_activate_row", id: r.id });
    });
    fila.append(
      this.compareFace(r.left),
      veredicto(r, (k) => this.t(k)),
      this.compareFace(r.right),
    );
    if (r.paired_under !== null) {
      // Frase en su propia línea, JAMÁS pegada al nombre: lo que se pega a
      // un nombre lo puede falsificar un nombre.
      const nota = document.createElement("p");
      nota.className = "compare-paired-under";
      nota.textContent = r.paired_under;
      fila.append(nota);
    }
    lista.append(fila);
  }
  if (compare.selected !== null) {
    lista.setAttribute(
      "aria-activedescendant",
      `compare-row-${String(compare.selected)}`,
    );
  }
  caja.append(lista);

  const estado = document.createElement("p");
  estado.className = "compare-status";
  estado.setAttribute("role", "status");
  estado.setAttribute("aria-live", "polite");
  estado.dataset["running"] = String(compare.running);
  estado.textContent = compare.status;
  caja.append(estado);
  this.compareRoot.replaceChildren(caja);
}

/** Una cara de una fila comparada, o el hueco de un huérfano. */
export function compareFace(this: Screen, face: CompareFaceView | null): HTMLElement {
  const el = document.createElement("span");
  el.className = "compare-face";
  if (face === null) {
    // Vacío y DICHO: un huérfano no tiene nada de este lado, y una celda
    // en blanco sin más se lee como un fichero sin nombre.
    el.dataset["absent"] = "true";
    el.textContent = "—";
    return el;
  }
  el.dataset["dir"] = String(face.is_dir);
  el.dataset["hostile"] = String(face.hostile);
  const nombre = document.createElement("span");
  nombre.className = "compare-name";
  nombre.textContent = face.name;
  el.append(nombre);
  if (face.hostile) {
    el.append(badge(this.t("hostile-name")));
  }
  // Tamaño y fecha solo cuando se saben: vacío es AUSENCIA, no cero.
  for (const [clase, texto] of [
    ["compare-size", face.size],
    ["compare-mtime", face.mtime],
  ] as [string, string][]) {
    if (texto === "") {
      continue;
    }
    const celda = document.createElement("span");
    celda.className = clase;
    celda.textContent = texto;
    el.append(celda);
  }
  return el;
}
