// Pintores de `Screen` para dialogs (ola W10): funciones con `this: Screen`,
// enganchadas como propiedades en `render.ts`. El estado sigue en la clase.

import type { Screen } from "../render";
import type { DialogLine, DialogView } from "../types";
import { badge } from "./dom";

/** Un campo etiquetado de un diálogo: la etiqueta fuera de banda y el valor
 *  con su marca si lo pintado difiere de lo que hay. */
export function campoDeDialogo(
  this: Screen,
  etiquetaTexto: string,
  linea: DialogLine,
): HTMLElement {
  const p = document.createElement("p");
  p.className = "dialog-field";
  const etiqueta = document.createElement("span");
  etiqueta.className = "dialog-field-label";
  etiqueta.textContent = etiquetaTexto;
  const valor = document.createElement("span");
  valor.textContent = linea.text;
  valor.dataset["hostile"] = String(linea.hostile);
  p.append(etiqueta, valor);
  if (linea.hostile) {
    valor.classList.add("hostile");
    p.append(badge(this.t("hostile-name")));
  }
  return p;
}

export function paintDialogs(this: Screen, dialogs: DialogView[]): void {
  if (dialogs.length === 0) {
    this.dialogsRoot.replaceChildren();
    this.dialogoPintado = null;
    this.dialogoInput = null;
    this.dialogoCampos.clear();
    return;
  }
  const top = dialogs[dialogs.length - 1];
  if (top === undefined) {
    return;
  }
  // El campo al que devolver el foco cuando la caja nueva esté montada.
  let refocar: HTMLInputElement | null = null;
  // Y, en un FORMULARIO, cuál de sus campos lo tenía y por dónde iba el caret
  // (puente 91): la caja se rehace entera en cada parche —y cada tecla
  // produce uno—, así que sin esto se escribe una letra y el foco se cae.
  const activo = document.activeElement;
  let campoFoco: string | null = null;
  let caret = 0;
  if (activo instanceof HTMLInputElement && activo.dataset["campo"] !== undefined) {
    campoFoco = activo.dataset["campo"];
    caret = activo.selectionStart ?? activo.value.length;
  }
  const box = document.createElement("div");
  box.className = "dialog";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  const h = document.createElement("h2");
  h.id = `dialog-title-${String(top.id)}`;
  h.textContent = this.t(top.title_key);
  box.setAttribute("aria-labelledby", h.id);
  box.append(h);
  if (top.destination !== null) {
    // El destino, en su propio elemento y con su etiqueta traducida. NO
    // como una línea del cuerpo con una flecha delante: un directorio puede
    // llamarse `docs → /casa/BORRAR`, esa flecha es legítima y no se
    // enmascara, así que la línea se leería como dos rutas y quien confirma
    // creería estar mandando sus ficheros a la segunda.
    const dest = document.createElement("p");
    dest.className = "dialog-destination";
    const etiqueta = document.createElement("span");
    etiqueta.className = "dialog-destination-label";
    etiqueta.textContent = this.t("dialog-destination");
    const valor = document.createElement("span");
    valor.textContent = top.destination.text;
    valor.dataset["hostile"] = String(top.destination.hostile);
    dest.append(etiqueta, valor);
    if (top.destination.hostile) {
      valor.classList.add("hostile");
      dest.append(badge(this.t("hostile-name")));
    }
    box.append(dest);
  }
  // Qué se pide y quién lo pide, cada uno etiquetado y FUERA de la lista de
  // rutas: entre líneas de rutas, un nombre de fichero que dijera lo mismo
  // sería indistinguible.
  if (top.subject !== null) {
    box.append(this.campoDeDialogo(this.t("dialog-subject"), top.subject));
  }
  if (top.asker !== null) {
    box.append(this.campoDeDialogo(this.t("dialog-asker"), top.asker));
  }
  if (top.body.length > 0) {
    // Numeradas por POSICIÓN, con una lista ordenada: la etiqueta es
    // estructural y ningún nombre de fichero puede escribirla.
    const lista = document.createElement("ol");
    lista.className = "dialog-body";
    for (const line of top.body) {
      const li = document.createElement("li");
      li.textContent = line.text;
      li.dataset["hostile"] = String(line.hostile);
      if (line.hostile) {
        // Esta es la pantalla donde se aprueba borrar, copiar o mover un
        // nombre. Un nombre que se pinta distinto de lo que es y no lo dice
        // se lee como fiel, y la aprobación es de otra cosa.
        li.classList.add("hostile");
        li.append(badge(this.t("hostile-name")));
      }
      lista.append(li);
    }
    box.append(lista);
  }
  if (top.deadline !== null) {
    // El plazo, en su propio elemento: con `ttl_ms == 0` no hay línea de
    // plazo que pintar, y entonces un fichero llamado «caduca en 3600 s»
    // sería la única que lo pareciera.
    const plazo = document.createElement("p");
    plazo.className = "dialog-deadline";
    plazo.setAttribute("role", "status");
    plazo.textContent = top.deadline;
    // Y si el host dijo CUÁNDO vence, se cuenta de verdad (#279). La frase
    // del host se calcula al abrir y se congelaba: un modal que llevaba
    // cuatro minutos delante seguía diciendo «caduca en 300 s».
    //
    // El texto lo sigue componiendo el host —aquí solo se sustituye el
    // número dentro de él— porque la frase es suya y está traducida: el
    // renderer no sabe decir «caduca en» en el idioma de esta ventana.
    const vence = top.deadline_at_ms;
    if (vence !== undefined && vence !== null) {
      const plantilla = top.deadline;
      const pintar = (): boolean => {
        const quedan = Math.max(0, Math.ceil((vence - Date.now()) / 1000));
        plazo.textContent = plantilla.replace(/\d+/, String(quedan));
        return quedan > 0;
      };
      pintar();
      const tick = window.setInterval(() => {
        // Cuando llega a cero se para solo: el diálogo lo cierra el host al
        // vencer, y seguir contando en negativo sobre algo que ya no está
        // es ruido.
        if (!pintar() || !plazo.isConnected) {
          window.clearInterval(tick);
        }
      }, 1000);
    }
    box.append(plazo);
  }
  if (top.overflow_note !== "") {
    // La lista está recortada, y decirlo es lo único que impide confirmar
    // una operación sobre doscientos ficheros creyendo que son dieciséis.
    const nota = document.createElement("p");
    nota.className = "dialog-overflow";
    nota.setAttribute("role", "alert");
    nota.textContent = top.overflow_note;
    // Y si algo de lo que NO se enseña se pintaría alterado. El badge no
    // puede hablar de una ruta concreta —esa no está delante— pero sí decir
    // que ahí fuera hay algo así, que es lo que decide si merece la pena
    // ampliar antes de aprobar. El terminal lo decía y esta ventana no.
    if (top.overflow_hostile === true) {
      nota.append(" ", badge(this.t("hostile-name")));
    }
    box.append(nota);
  }
  const chequeo = top.dest_check ?? { state: "not_asked" };
  if (chequeo.state === "checking") {
    // Se DICE que se está preguntando, y el sitio queda reservado: un aviso
    // que aterriza de golpe encima de los botones los mueve bajo el
    // puntero de quien ya iba a pulsar. Y sobre todo, mientras esto se lea
    // «comprobando», la ausencia de la línea de #164 no se puede leer como
    // «este destino confina».
    const espera = document.createElement("p");
    espera.className = "dialog-checking";
    espera.textContent = this.t("dialog-checking-destination");
    box.append(espera);
  }
  if (chequeo.state === "done") {
    for (const aviso of chequeo.warnings) {
      // Del DESTINO: que no cabe, que no sabe confinar. Ya traducidos y sin
      // una sola cadena que controle un tercero, así que van en su propio
      // bloque y no entre las líneas del cuerpo — donde un nombre de
      // fichero los podría suplantar.
      const linea = document.createElement("p");
      linea.className = "dialog-warning";
      linea.setAttribute("role", "alert");
      linea.textContent = aviso;
      box.append(linea);
    }
  }
  if (top.input_hostile) {
    // Es la ÚNICA superficie donde se aprueba un nombre: si lo que se pinta
    // difiere de lo que se creará, se dice aquí.
    const aviso = document.createElement("p");
    aviso.className = "hostile";
    aviso.setAttribute("role", "alert");
    aviso.textContent = this.t("hostile-name");
    box.append(aviso);
  }
  if (top.input === null) {
    this.dialogoInput = null;
  } else {
    // El campo se REUSA mientras sea el mismo diálogo. Antes se creaba uno
    // nuevo en cada repintado y se le dejaba el valor sin poner —para no
    // devolverle la proyección del host, enmascarada y acotada, que el
    // siguiente evento habría mandado de vuelta como si fuera lo tecleado—,
    // así que el campo salía VACÍO. Y como cada tecla provoca un parche,
    // cada tecla lo vaciaba: lo que llegaba a `fs.mkdir` era el último
    // carácter. Reusar el nodo conserva de paso el cursor y la selección.
    const previo = this.dialogoPintado === top.id ? this.dialogoInput : null;
    // Reusar el nodo no basta: la caja del diálogo se rehace en cada
    // repintado y el campo se MUEVE a la nueva, y mover un nodo lo saca del
    // documento un instante, que es lo que le quita el foco. Cada tecla
    // provoca un parche, así que cada tecla dejaba el campo sin foco y la
    // siguiente se iba al host como un acorde. Se apunta si lo tenía y se
    // le devuelve al final, con la caja ya montada.
    const teniaFoco = previo !== null && document.activeElement === previo;
    if (teniaFoco) {
      refocar = previo;
    }
    let input = previo;
    if (input === null) {
      input = document.createElement("input");
      // #327: una contraseña se pinta como contraseña. Lo que llega en
      // `top.input` son PUNTOS —el host no manda nunca el texto—, así que
      // sembrar el campo con eso escribiría puntos literales dentro: se
      // siembra vacío, que es lo que el diálogo acaba de abrir.
      input.type = top.input_secret ? "password" : "text";
      input.value = top.input_secret ? "" : top.input;
      if (top.input_secret) {
        // `new-password` y no `off`: Chromium y WebView2 IGNORAN `off` en un
        // campo de contraseña a propósito, y este es el valor que sí
        // respetan. Esto no se guarda en ninguna parte, que es justo lo que
        // el cuerpo del diálogo promete.
        input.autocomplete = "new-password";
        input.setAttribute("autocorrect", "off");
        input.spellcheck = false;
      }
      const vivo = input;
      vivo.addEventListener("input", () => {
        // Una CONTRASEÑA no se manda al teclear (#327): el host no guarda lo
        // que se escribe, el campo lo enmascara el propio navegador, y por
        // aquí cruzarían `h`, `hu`, `hun`… — un prefijo por pulsación, cada
        // uno en un trozo de heap que nadie pisa. Cruza una vez, al
        // confirmar.
        if (top.input_secret) {
          return;
        }
        this.send({ action: "dialog_input", id: top.id, text: vivo.value });
      });
      if (top.input_secret) {
        // Enter DENTRO del campo confirma, y lleva el valor. Sin esto, la
        // tecla sale al host como un acorde `dialog.confirm` — que sobre un
        // diálogo de contraseña no lleva nada y por tanto es inerte—, así
        // que la forma más natural de contestar no habría hecho nada.
        vivo.addEventListener("keydown", (e) => {
          if (e.key !== "Enter") {
            return;
          }
          e.preventDefault();
          e.stopPropagation();
          this.send({
            action: "dialog",
            id: top.id,
            choice: "confirm",
            secret: vivo.value,
          });
        });
      }
      queueMicrotask(() => {
        vivo.focus();
      });
    }
    input.setAttribute("aria-labelledby", h.id);
    this.dialogoInput = input;
    box.append(input);
  }
  // Los CAMPOS de un formulario (puente 91), en el orden en que llegan: el
  // host los ordena, y reordenarlos aquí sería contar otra historia.
  const campos = top.fields ?? [];
  if (campos.length > 0) {
    // Un diálogo distinto empieza de cero; el MISMO reutiliza sus nodos.
    if (this.dialogoPintado !== top.id) {
      this.dialogoCampos.clear();
    }
    const nacieron = this.dialogoCampos.size === 0;
    const caja = document.createElement("div");
    caja.className = "dialog-fields";
    let primero: HTMLInputElement | null = null;
    for (const f of campos) {
      const fila = document.createElement("p");
      fila.className = "dialog-field";
      const etiqueta = document.createElement("label");
      etiqueta.className = "dialog-field-label";
      etiqueta.textContent = this.t(f.label_key);
      etiqueta.htmlFor = `dialog-field-${f.id}`;
      fila.append(etiqueta);
      const previo = this.dialogoCampos.get(f.id) ?? null;
      if (f.kind.kind === "text") {
        // **El nodo se REUTILIZA y su valor NO se vuelve a sembrar.** Lo que
        // manda el host es su PROYECCIÓN —enmascarada y acotada—, así que
        // re-sembrarla haría que la siguiente tecla la devolviera como si
        // fuera lo tecleado: un `U+FFFD` de pantalla acabaría siendo el
        // patrón que se busca. Es la misma disciplina que el campo único de
        // arriba, y el host le pone el otro cinturón rechazando `U+FFFD`.
        let texto = previo instanceof HTMLInputElement ? previo : null;
        if (texto === null) {
          texto = document.createElement("input");
          texto.type = "text";
          texto.id = `dialog-field-${f.id}`;
          texto.value = f.value;
          texto.dataset["campo"] = f.id;
          const vivo = texto;
          vivo.addEventListener("input", () => {
            this.send({
              action: "dialog_field",
              id: top.id,
              field: f.id,
              value: { set: "text", text: vivo.value },
            });
          });
          this.dialogoCampos.set(f.id, vivo);
        }
        texto.dataset["hostile"] = String(f.hostile);
        texto.classList.toggle("hostile", f.hostile);
        primero ??= texto;
        fila.append(texto);
        if (f.hostile) {
          fila.append(badge(this.t("hostile-name")));
        }
      } else if (f.kind.kind === "toggle") {
        // Un interruptor sí se re-siembra: su estado es del HOST y no hay
        // nada tecleado que pisar.
        let casilla = previo instanceof HTMLInputElement ? previo : null;
        if (casilla === null) {
          casilla = document.createElement("input");
          casilla.type = "checkbox";
          casilla.id = `dialog-field-${f.id}`;
          casilla.dataset["campo"] = f.id;
          casilla.addEventListener("change", () => {
            // Sin valor: se dice que se TOCÓ, y a qué estado va lo decide el
            // host. Mandar el destino dejaría que dos pulsaciones rápidas se
            // pisaran, la segunda nacida de una foto anterior.
            this.send({
              action: "dialog_field",
              id: top.id,
              field: f.id,
              value: { set: "toggled" },
            });
          });
          this.dialogoCampos.set(f.id, casilla);
        }
        casilla.checked = f.kind.on;
        fila.append(casilla);
      } else {
        let boton = previo instanceof HTMLButtonElement ? previo : null;
        if (boton === null) {
          boton = document.createElement("button");
          boton.type = "button";
          boton.id = `dialog-field-${f.id}`;
          boton.dataset["campo"] = f.id;
          boton.addEventListener("click", () => {
            this.send({
              action: "dialog_field",
              id: top.id,
              field: f.id,
              value: { set: "cycled" },
            });
          });
          this.dialogoCampos.set(f.id, boton);
        }
        boton.textContent = this.t(f.kind.value_key);
        fila.append(boton);
      }
      caja.append(fila);
    }
    box.append(caja);
    // Un formulario recién abierto se lleva el foco a su primer campo, como
    // el diálogo de un solo campo: sin esto se abre y teclear no hace nada
    // hasta que alguien pulsa dentro.
    if (nacieron && campoFoco === null && primero !== null) {
      const destino = primero;
      queueMicrotask(() => {
        destino.focus();
      });
    }
  }
  const choices = document.createElement("div");
  choices.className = "choices";
  for (const c of top.choices) {
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = this.t(c.label_key);
    b.dataset["destructive"] = String(c.destructive);
    b.addEventListener("click", () => {
      // La contraseña viaja CON la respuesta afirmativa, y solo con ella
      // (#327): cancelar no entrega nada. Se lee del campo vivo en este
      // instante, que es lo que el lector está viendo — el host no guarda
      // ninguna copia con la que pudiera discrepar.
      if (top.input_secret && c.id === "confirm") {
        this.send({
          action: "dialog",
          id: top.id,
          choice: c.id,
          secret: this.dialogoInput?.value ?? "",
        });
        return;
      }
      this.send({ action: "dialog", id: top.id, choice: c.id });
    });
    choices.append(b);
  }
  box.append(choices);
  this.dialogsRoot.replaceChildren(box);
  this.dialogoPintado = top.id;
  if (refocar !== null) {
    refocar.focus();
  }
  // Y el campo del formulario que lo tenía, con su caret donde estaba. Se
  // busca por `data-campo` y no con un selector compuesto: el id lo pone el
  // host, pero componer un selector con texto ajeno es una costumbre que se
  // acaba usando con texto que no lo es.
  if (campoFoco !== null) {
    const destino = Array.from(box.querySelectorAll("input, button")).find(
      (n) => n instanceof HTMLElement && n.dataset["campo"] === campoFoco,
    );
    if (destino instanceof HTMLElement) {
      destino.focus();
      if (destino instanceof HTMLInputElement && destino.type === "text") {
        const donde = Math.min(caret, destino.value.length);
        destino.setSelectionRange(donde, donde);
      }
    }
  }
}
