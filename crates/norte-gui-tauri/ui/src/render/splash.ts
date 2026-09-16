// Pintor de `Screen` para la pantalla de arranque (puente 69, ADR 0115):
// función con `this: Screen`, enganchada como propiedad en `render.ts`. El
// estado sigue en la clase.
//
// Lo que se enseña lo decide el host (`norte_frontend::splash`), así que esta
// pantalla dice lo mismo que la del terminal; aquí solo se elige dónde cae
// cada cosa y quién se lleva el clic.

import type { Screen } from "../render";
import type { SplashView } from "../types";

/**
 * El plazo del modo `brief` lo CUMPLE el renderer: no hay bucle de eventos
 * que despierte al host, y el número viene en la propia pantalla
 * (`close_after_ms`). Vive en `Screen` —no en este módulo— y se arma una vez
 * por aparición; ver `Screen.splashPlazo`.
 */
export function paintSplash(this: Screen, splash: SplashView | null): void {
  if (splash === null) {
    // Se fue (una tecla, un clic, el propio plazo): desarmar lo que quedara,
    // o un temporizador vivo mandaría el cierre de una pantalla que ya no
    // está y quemaría un número de secuencia.
    if (this.splashPlazo !== null) {
      clearTimeout(this.splashPlazo);
      this.splashPlazo = null;
    }
    this.splashPuesto = false;
    this.splashRoot.replaceChildren();
    this.splashRoot.dataset["open"] = "false";
    return;
  }
  this.splashRoot.dataset["open"] = "true";

  const caja = document.createElement("section");
  caja.className = "splash";
  caja.setAttribute("role", "dialog");
  caja.setAttribute("aria-modal", "true");
  caja.setAttribute("aria-label", this.t("splash-title"));

  // El arte, en su propio bloque y ESCONDIDO para quien lee con los oídos:
  // una brújula dibujada con barras y guiones se deletrea como ruido.
  const arte = document.createElement("pre");
  arte.className = "splash-art";
  arte.setAttribute("aria-hidden", "true");
  arte.textContent = splash.art.join("\n");
  caja.append(arte);

  const version = document.createElement("p");
  version.className = "splash-version";
  version.textContent = `${splash.version} ${splash.revision}`.trim();
  caja.append(version);

  const daemon = document.createElement("p");
  daemon.className = "splash-daemon";
  daemon.textContent = splash.daemon;
  caja.append(daemon);

  for (const seccion of splash.sections) {
    const titulo = document.createElement("h2");
    titulo.className = "splash-section";
    titulo.textContent = seccion.title;
    caja.append(titulo);

    const lista = document.createElement("ul");
    lista.className = "splash-rows";
    for (const fila of seccion.rows) {
      const li = document.createElement("li");
      li.className = "splash-row";
      // El número que la fila enseña, o un hueco: más allá de nueve la fila
      // se lee y no se promete, igual que en el terminal.
      const num = document.createElement("span");
      num.className = "splash-number";
      num.textContent = fila.number === 0 ? "" : String(fila.number);
      const label = document.createElement("span");
      label.className = "splash-label";
      label.textContent = fila.label;
      const detalle = document.createElement("span");
      detalle.className = "splash-detail";
      detalle.textContent = fila.detail;
      li.append(num, label, detalle);
      if (fila.number !== 0) {
        // Un clic en una fila numerada la abre. Las demás no son botones y
        // no se anuncian como tales: un clic ahí solo quita la pantalla, que
        // es lo que un clic en cualquier otro sitio hace.
        li.dataset["actionable"] = "true";
        li.addEventListener("click", (e) => {
          e.stopPropagation();
          this.send({ action: "splash_activate_row", number: fila.number });
        });
      }
      lista.append(li);
    }
    caja.append(lista);
  }

  const pie = document.createElement("p");
  pie.className = "splash-hint";
  pie.textContent = splash.hint;
  caja.append(pie);

  // Un clic EN CUALQUIER SITIO la quita. Va en la raíz y no en la caja para
  // que el aire de alrededor cuente también: quitarla es lo que se intenta.
  this.splashRoot.onclick = (): void => {
    this.send({ action: "splash_close" });
  };
  this.splashRoot.replaceChildren(caja);

  // El plazo se arma en la PRIMERA pintada de esta aparición y no se toca
  // más: el host manda la vista entera en cada parche, así que rearmarlo aquí
  // convertía «1,2 segundos» en «1,2 segundos después del último parche» — y
  // durante el arranque los parches no paran de llegar, que es exactamente
  // cuando esta pantalla está puesta.
  const queda = splash.close_after_ms;
  if (!this.splashPuesto && queda !== null && queda !== undefined) {
    this.splashPlazo = setTimeout(() => {
      this.splashPlazo = null;
      this.send({ action: "splash_close" });
    }, queda);
  }
  this.splashPuesto = true;
}
