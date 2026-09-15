// Pintores de `Screen` para extensions (ola W10): funciones con `this: Screen`,
// enganchadas como propiedades en `render.ts`. El estado sigue en la clase.

import type { Screen } from "../render";
import type {
  ExtensionsView,
  ExtensionRowView,
  ExtensionErrorView,
  AgentsView,
  ExtensionCommandView,
  ExtensionOutputView,
  ProgramOutputView,
} from "../types";
import { revelar, badge } from "./dom";

/**
 * El gestor de extensiones (F12): la lista a la izquierda y, a la derecha,
 * la ficha de la elegida con sus botones (puente 61).
 *
 * Las capabilities van en la FILA y no escondidas tras un gesto: son la
 * decisión que un humano aprueba. Los botones no toman esa decisión por su
 * cuenta: mandan la MISMA acción que la tecla, y es el host quien pregunta
 * —conceder enumera las capabilities, desinstalar dice qué se pierde— antes
 * de tocar nada. Aprobar y desinstalar se pintan como lo que son, no como
 * el «Aceptar» de un aviso.
 */
export function paintExtensions(this: Screen, ext: ExtensionsView | null): void {
  if (ext === null) {
    this.extensionsRoot.replaceChildren();
    this.extensionsRoot.dataset["open"] = "false";
    return;
  }
  this.extensionsRoot.dataset["open"] = "true";
  const caja = document.createElement("section");
  caja.className = "extensions";
  caja.setAttribute("role", "dialog");
  caja.setAttribute("aria-modal", "true");
  caja.setAttribute("aria-label", this.t("ext-title"));

  const cabecera = document.createElement("header");
  cabecera.className = "extensions-head";
  const titulo = document.createElement("h1");
  titulo.textContent = this.t("ext-title");
  cabecera.append(titulo);
  const resumen = document.createElement("span");
  resumen.className = "extensions-summary";
  if (ext.loading) {
    // «Cargando» y «ninguna» no son lo mismo, y una lista vacía sin este
    // aviso se lee como lo segundo.
    resumen.classList.add("extensions-note");
    resumen.setAttribute("role", "status");
    resumen.textContent = this.t("ext-loading");
  } else {
    // Dos cuentas y no una: «7 instaladas» con «6 encendidas» es la
    // pregunta que trae a alguien a esta pantalla.
    const encendidas = ext.rows.filter((r) => r.approved && r.enabled).length;
    resumen.textContent = `${String(ext.rows.length)} ${this.t("ext-installed")} · ${String(
      encendidas,
    )} ${this.t("ext-enabled")}`;
  }
  cabecera.append(resumen);
  const cerrar = document.createElement("button");
  cerrar.type = "button";
  cerrar.className = "extensions-close";
  cerrar.setAttribute("aria-label", this.t("ext-close"));
  cerrar.textContent = "×";
  cerrar.addEventListener("click", () => {
    // La misma tecla que cierra: el host decide si el primer `esc` cierra
    // una ficha o el gestor, y un botón que lo decidiera aparte divergiría.
    this.send({
      action: "key",
      key: "Escape",
      ctrl: false,
      alt: false,
      shift: false,
      meta: false,
    });
  });
  cabecera.append(cerrar);
  caja.append(cabecera);

  const cuerpo = document.createElement("div");
  cuerpo.className = "extensions-body";

  const lista = document.createElement("ul");
  lista.className = "extensions-rows";
  lista.setAttribute("role", "listbox");
  if (!ext.loading && ext.rows.length === 0) {
    const vacio = document.createElement("p");
    vacio.className = "extensions-note";
    vacio.textContent = this.t("ext-empty");
    lista.append(vacio);
  }
  for (const [i, r] of ext.rows.entries()) {
    const fila = document.createElement("li");
    fila.className = "extensions-row";
    fila.id = `extension-row-${String(i)}`;
    fila.setAttribute("role", "option");
    fila.setAttribute("aria-selected", String(ext.cursor === i));
    fila.addEventListener("click", () => {
      this.send({ action: "extension_select_row", row: i });
    });

    const principal = document.createElement("div");
    principal.className = "extensions-row-main";
    const nombre = document.createElement("span");
    nombre.className = "extensions-name";
    nombre.textContent = r.name;
    const version = document.createElement("span");
    version.className = "extensions-version";
    version.textContent = r.version;
    principal.append(nombre, version, estadoDe(r, this.t.bind(this)));
    fila.append(principal);

    const meta = document.createElement("span");
    meta.className = "extensions-meta";
    const trozos = [r.category];
    if (r.publisher !== "") {
      trozos.push(r.publisher);
    }
    meta.textContent = trozos.join(" · ");
    fila.append(meta);

    if (r.description !== "") {
      const desc = document.createElement("span");
      desc.className = "extensions-desc";
      desc.textContent = r.description;
      fila.append(desc);
    }

    if (r.capabilities.length > 0) {
      fila.append(capsDe(r.capabilities));
    }
    lista.append(fila);
  }
  if (ext.cursor < ext.rows.length) {
    lista.setAttribute("aria-activedescendant", `extension-row-${String(ext.cursor)}`);
  }
  cuerpo.append(lista);

  const panel = document.createElement("article");
  panel.className = "extensions-pane";
  const elegida = ext.rows[ext.cursor];
  // Las que no cargaron van DETRÁS de las cargadas en la cuenta del cursor
  // (puente 69): la fila `rows.length + j` es `errors[j]`.
  const rota = ext.errors[ext.cursor - ext.rows.length];
  if (elegida !== undefined) {
    panel.append(this.extensionPaneHead(elegida, ext.cursor));
    if (ext.detail !== null && ext.detail.id === elegida.id) {
      panel.append(this.extensionDetail(ext.detail, elegida.name));
    } else {
      const pista = document.createElement("p");
      pista.className = "extensions-note extensions-detail-hint";
      pista.textContent = this.t("ext-detail-hint");
      panel.append(pista);
    }
  } else if (rota !== undefined) {
    panel.append(fichaDeRota(this, rota, ext.cursor));
  }
  cuerpo.append(panel);
  caja.append(cuerpo);

  if (ext.errors.length > 0) {
    const titulo = document.createElement("h2");
    titulo.className = "extensions-errors-title";
    titulo.textContent = this.t("ext-errors-title");
    caja.append(titulo);
    const errores = document.createElement("ul");
    errores.className = "extensions-errors";
    for (const [j, e] of ext.errors.entries()) {
      // Una fila más: se señala con un clic, y su ficha tiene el único
      // verbo que le queda. Sin esto, una extensión rota solo se quitaba a
      // mano, borrando su directorio.
      const row = ext.rows.length + j;
      const li = document.createElement("li");
      li.className = "extensions-error";
      li.id = `extension-row-${String(row)}`;
      li.setAttribute("role", "option");
      li.setAttribute("aria-selected", String(ext.cursor === row));
      li.addEventListener("click", () => {
        this.send({ action: "extension_select_row", row });
      });
      const dir = document.createElement("span");
      dir.className = "extensions-error-dir";
      dir.dataset["hostile"] = String(e.hostile);
      dir.textContent = e.dir;
      if (e.hostile) {
        dir.append(badge(this.t("hostile-name")));
      }
      const motivo = document.createElement("span");
      motivo.className = "extensions-error-reason";
      motivo.dataset["hostile"] = String(e.reason_hostile);
      motivo.textContent = e.reason;
      if (e.reason_hostile) {
        // El motivo lo escribe el core, pero CITA el manifiesto del plugin
        // y a veces un `Path::display()`.
        motivo.append(badge(this.t("hostile-name")));
      }
      li.append(dir, motivo);
      errores.append(li);
    }
    caja.append(errores);
  }
  this.extensionsRoot.replaceChildren(caja);
  revelar(caja.querySelector(`#extension-row-${String(ext.cursor)}`) ?? undefined);
}

/**
 * La ficha de una extensión que NO cargó: dónde, por qué, y el único verbo
 * que le queda. Sin id no hay botón —el directorio no se llama como un id y
 * no hay nada que mandar a borrar—, y se dice con la frase del host.
 */
function fichaDeRota(s: Screen, e: ExtensionErrorView, row: number): HTMLElement {
  const cabecera = document.createElement("header");
  cabecera.className = "extensions-pane-head";
  const nombre = document.createElement("h2");
  nombre.className = "extensions-pane-name";
  nombre.dataset["hostile"] = String(e.hostile);
  nombre.textContent = e.dir;
  if (e.hostile) {
    nombre.append(badge(s.t("hostile-name")));
  }
  const motivo = document.createElement("p");
  motivo.className = "extensions-error-reason";
  motivo.dataset["hostile"] = String(e.reason_hostile);
  motivo.textContent = e.reason;
  if (e.reason_hostile) {
    motivo.append(badge(s.t("hostile-name")));
  }
  cabecera.append(nombre, motivo);

  const acciones = document.createElement("div");
  acciones.className = "extensions-actions";
  acciones.setAttribute("role", "group");
  acciones.setAttribute("aria-label", s.t("ext-actions"));
  const id = e.id;
  if (id !== null) {
    const desinstalar = document.createElement("button");
    desinstalar.type = "button";
    desinstalar.className = "extensions-action extensions-action-uninstall";
    desinstalar.textContent = s.t("ext-uninstall");
    desinstalar.dataset["destructive"] = "true";
    desinstalar.addEventListener("click", (ev) => {
      ev.stopPropagation();
      // La misma acción que la tecla: el host pregunta antes de borrar.
      s.send({ action: "extension_govern", row, id, change: "uninstall" });
    });
    acciones.append(desinstalar);
  } else {
    const nota = document.createElement("p");
    nota.className = "extensions-note";
    nota.textContent = s.t("ext-broken-not-id");
    acciones.append(nota);
  }
  cabecera.append(acciones);
  return cabecera;
}

/** La píldora de estado: DOS hechos independientes, y se dicen los dos. */
function estadoDe(r: ExtensionRowView, t: (key: string) => string): HTMLElement {
  const estado = document.createElement("span");
  estado.className = "extensions-state";
  estado.dataset["approved"] = String(r.approved);
  estado.dataset["enabled"] = String(r.enabled);
  estado.textContent = r.approved
    ? t(r.enabled ? "ext-state-on" : "ext-state-off")
    : t("ext-unapproved");
  return estado;
}

/** Las capabilities como fichas, una por nodo: texto de tercero. */
function capsDe(capabilities: string[]): HTMLElement {
  const caps = document.createElement("ul");
  caps.className = "extensions-caps";
  for (const c of capabilities) {
    const cap = document.createElement("li");
    cap.className = "extensions-cap";
    cap.textContent = c;
    caps.append(cap);
  }
  return caps;
}

/**
 * La cabecera de la ficha (puente 61): quién es, cómo está, y los botones.
 *
 * Cada botón dice lo que VA a hacer, resuelto desde el estado —«Revocar»
 * sobre una aprobada, «Aprobar» sobre una que no—, y manda la misma acción
 * que la tecla; el host resuelve igual y pregunta lo que haya que preguntar.
 * Encender una sin aprobar no se ofrece: el botón está deshabilitado y su
 * `title` dice por qué, con la frase que el host contestaría.
 */
export function extensionPaneHead(
  this: Screen,
  r: ExtensionRowView,
  row: number,
): HTMLElement {
  const cabecera = document.createElement("header");
  cabecera.className = "extensions-pane-head";

  const nombre = document.createElement("h2");
  nombre.className = "extensions-pane-name";
  nombre.textContent = r.name;
  cabecera.append(nombre);

  const meta = document.createElement("div");
  meta.className = "extensions-pane-meta";
  const version = document.createElement("span");
  version.className = "extensions-version";
  version.textContent = r.version;
  meta.append(version);
  if (r.publisher !== "") {
    const quien = document.createElement("span");
    quien.className = "extensions-publisher";
    quien.textContent = r.publisher;
    meta.append(quien);
  }
  const categoria = document.createElement("span");
  categoria.className = "extensions-category";
  categoria.textContent = r.category;
  meta.append(categoria, estadoDe(r, this.t.bind(this)));
  cabecera.append(meta);

  if (r.description !== "") {
    const desc = document.createElement("p");
    desc.className = "extensions-pane-desc";
    desc.textContent = r.description;
    cabecera.append(desc);
  }

  const acciones = document.createElement("div");
  acciones.className = "extensions-actions";
  acciones.setAttribute("role", "group");
  acciones.setAttribute("aria-label", this.t("ext-actions"));
  const boton = (clase: string, texto: string, manda: () => void): HTMLButtonElement => {
    const b = document.createElement("button");
    b.type = "button";
    b.className = `extensions-action ${clase}`;
    b.textContent = texto;
    b.addEventListener("click", (ev) => {
      // El clic no sube a la fila: la acción ya la señala, y una segunda
      // orden de selección pisaría la ficha que la primera pide.
      ev.stopPropagation();
      manda();
    });
    return b;
  };
  const aprobar = boton(
    "extensions-action-approval",
    this.t(r.approved ? "ext-revoke" : "ext-approve"),
    () => {
      this.send({ action: "extension_govern", row, id: r.id, change: "approval" });
    },
  );
  // Conceder no borra nada, pero es LA decisión de seguridad: se marca
  // para que no se pinte como el «Aceptar» de un aviso.
  aprobar.dataset["primary"] = String(!r.approved);
  acciones.append(aprobar);

  const encender = boton(
    "extensions-action-enabled",
    this.t(r.enabled ? "ext-disable" : "ext-enable"),
    () => {
      this.send({ action: "extension_govern", row, id: r.id, change: "enabled" });
    },
  );
  if (!r.approved && !r.enabled) {
    encender.disabled = true;
    encender.title = this.t("host-extension-not-approved");
  }
  acciones.append(encender);

  if (r.has_help) {
    acciones.append(
      boton("extensions-action-help", this.t("ext-help"), () => {
        this.send({ action: "extension_help", row, id: r.id });
      }),
    );
  }

  const desinstalar = boton(
    "extensions-action-uninstall",
    this.t("ext-uninstall"),
    () => {
      this.send({ action: "extension_govern", row, id: r.id, change: "uninstall" });
    },
  );
  desinstalar.dataset["destructive"] = "true";
  acciones.append(desinstalar);
  cabecera.append(acciones);

  if (r.capabilities.length > 0) {
    cabecera.append(capsDe(r.capabilities));
  }

  // Cuántas cosas aporta: los números que la fila no tiene sitio para decir.
  const cuentas = document.createElement("p");
  cuentas.className = "extensions-counts";
  const partes: string[] = [];
  if (r.commands > 0) {
    partes.push(`${String(r.commands)} ${this.t("ext-counts-commands")}`);
  }
  if (r.columns > 0) {
    partes.push(`${String(r.columns)} ${this.t("ext-counts-columns")}`);
  }
  if (partes.length > 0) {
    cuentas.textContent = partes.join(" · ");
    cabecera.append(cuentas);
  }
  return cabecera;
}

/** La ficha de una extensión: sus claves `[config]` con su valor. */
export function extensionDetail(
  this: Screen,
  d: ExtensionsView["detail"],
  nombre: string,
): HTMLElement {
  const ficha = document.createElement("article");
  ficha.className = "extensions-detail";
  if (d === null) {
    return ficha;
  }
  const titulo = document.createElement("h2");
  titulo.textContent = this.t("ext-config-title");
  if (nombre !== "") {
    const suya = document.createElement("span");
    suya.className = "extensions-detail-of";
    suya.textContent = nombre;
    titulo.append(" · ", suya);
  }
  ficha.append(titulo);
  if (d.config.length === 0) {
    const nada = document.createElement("p");
    nada.className = "extensions-note";
    nada.textContent = this.t("ext-config-none");
    ficha.append(nada);
    return ficha;
  }
  const tabla = document.createElement("table");
  tabla.className = "extensions-config";
  const tbody = document.createElement("tbody");
  for (const [i, k] of d.config.entries()) {
    const tr = document.createElement("tr");
    tr.id = `extension-key-${String(i)}`;
    // Cuál está elegida y cuál se puede editar: sin lo segundo, la
    // pantalla ofrece `Enter` sobre una clave de un tipo que este build no
    // conoce y el lector concluye que la escritura falló.
    tr.dataset["current"] = String(i === d.cursor);
    tr.dataset["editable"] = String(k.editable);
    // Un valor que NO es el del esquema se marca: es lo único que
    // distingue «así viene» de «así lo dejaste».
    tr.dataset["changed"] = String(k.value !== k.default);
    const clave = document.createElement("th");
    clave.setAttribute("scope", "row");
    clave.className = "extensions-key";
    clave.textContent = k.key;
    const valor = document.createElement("td");
    valor.className = "extensions-key-value";
    valor.dataset["hostile"] = String(k.hostile);
    if (i === d.cursor && d.editing !== null) {
      // Lo que se está TECLEANDO, en su propio nodo y marcado: sustituye
      // al valor porque es lo que se va a escribir, no lo que hay.
      const buf = document.createElement("span");
      buf.className = "extensions-key-editing";
      buf.dataset["hostile"] = String(d.editing_hostile);
      buf.textContent = d.editing;
      valor.append(buf);
      if (d.editing_hostile) {
        valor.append(badge(this.t("hostile-name")));
      }
    } else {
      valor.textContent = k.value;
    }
    if (k.hostile && d.editing === null) {
      // Lo que se pinta difiere de lo que es, y lo escribe el plugin: se
      // dice, igual que en un nombre de fichero.
      valor.append(badge(this.t("hostile-name")));
    }
    const tipo = document.createElement("td");
    tipo.className = "extensions-key-kind";
    // El tipo y el dominio, cada uno en su nodo: unirlos en uno solo deja
    // que un valor de `enum` con letras RTL reordene el par entero, y el
    // `unicode-bidi: isolate` del contenedor solo separa HERMANOS.
    const kindSpan = document.createElement("span");
    kindSpan.className = "extensions-key-kind-name";
    kindSpan.textContent = k.kind;
    tipo.append(kindSpan);
    if (k.domain !== "") {
      const sep = document.createElement("span");
      sep.className = "sep";
      sep.textContent = " · ";
      const dom = document.createElement("span");
      dom.className = "extensions-key-domain";
      dom.textContent = k.domain;
      tipo.append(sep, dom);
    }
    const desc = document.createElement("td");
    desc.className = "extensions-key-desc";
    desc.textContent = k.description;
    tr.append(clave, valor, tipo, desc);
    tbody.append(tr);
  }
  tabla.append(tbody);
  ficha.append(tabla);
  ficha.append(this.extensionCommands(d.commands));
  return ficha;
}

/**
 * Los comandos que aporta una extensión.
 *
 * Se LISTAN y no se lanzan desde aquí: la paleta es la puerta —la misma
 * que en el TUI—, y tener dos deja dos respuestas a qué significa que uno
 * falle. El `id` no se pinta: el manifiesto no le valida charset.
 */
export function extensionCommands(
  this: Screen,
  cmds: ExtensionCommandView[],
): HTMLElement {
  const caja = document.createElement("div");
  caja.className = "extensions-commands";
  if (cmds.length === 0) {
    return caja;
  }
  const titulo = document.createElement("h3");
  titulo.textContent = this.t("ext-commands-title");
  const lista = document.createElement("ul");
  for (const c of cmds) {
    const li = document.createElement("li");
    li.className = "extensions-command";
    li.dataset["hostile"] = String(c.hostile);
    li.textContent = c.title;
    if (c.hostile) {
      li.append(badge(this.t("hostile-name")));
    }
    lista.append(li);
  }
  caja.append(titulo, lista);
  return caja;
}

/**
 * Las sesiones de agente que esta ventana ha visto pedir permiso.
 *
 * La NOTA va dentro del panel y no en la documentación: esta lista no es
 * el censo de agentes del sistema —no hay método que lo dé—, y una lista
 * vacía sin esa frase se lee como «ningún agente ha tocado nada».
 */
export function paintAgents(this: Screen, agents: AgentsView | null): void {
  if (agents === null) {
    if (this.agentsRoot.dataset["open"] === "true") {
      this.agentsRoot.replaceChildren();
      this.agentsRoot.dataset["open"] = "false";
    }
    return;
  }
  this.agentsRoot.dataset["open"] = "true";
  const caja = document.createElement("section");
  caja.className = "agents";
  caja.setAttribute("role", "dialog");
  caja.setAttribute("aria-modal", "true");
  caja.setAttribute("aria-label", this.t("agents-title"));
  const titulo = document.createElement("h2");
  titulo.textContent = this.t("agents-title");
  const nota = document.createElement("p");
  nota.className = "agents-note";
  nota.textContent = agents.note;
  caja.append(titulo, nota);
  if (agents.forgotten > 0) {
    // Lo OLVIDADO se dice: el id de sesión lo elige el agente, así que
    // inundar la lista para empujar fuera a una concreta está a su
    // alcance, y una lista recortada que se presenta como completa es lo
    // que convierte eso en «esa sesión no existe».
    const podadas = document.createElement("p");
    podadas.className = "agents-forgotten";
    podadas.setAttribute("role", "status");
    podadas.textContent = String(agents.forgotten);
    podadas.dataset["forgotten"] = String(agents.forgotten);
    caja.append(podadas);
  }
  if (agents.rows.length === 0) {
    // La frase la compone el HOST: una lista vacía significa cosas
    // distintas según si esta ventana escucha las peticiones.
    const vacio = document.createElement("p");
    vacio.className = "agents-empty";
    vacio.textContent = agents.empty;
    caja.append(vacio);
    this.agentsRoot.replaceChildren(caja);
    return;
  }
  const lista = document.createElement("ul");
  lista.className = "agents-rows";
  lista.setAttribute("role", "listbox");
  for (const [i, r] of agents.rows.entries()) {
    const li = document.createElement("li");
    li.className = "agents-row";
    li.id = `agent-row-${String(i)}`;
    li.setAttribute("role", "option");
    li.setAttribute("aria-selected", String(agents.cursor === i));
    li.addEventListener("click", () => {
      // La generación viaja con el clic: la lista se reordena sola, y un
      // clic contra la de antes elige otra fila — aquí «esta fila» es de
      // quién se deshace el trabajo.
      this.send({
        action: "agent_select_row",
        row: i,
        generation: agents.generation,
      });
    });
    // El id y el último op, cada uno aislado y con su bandera: el id es
    // una clave opaca del daemon y puede traer letras RTL que reordenarían
    // la fila entera.
    const id = document.createElement("span");
    id.className = "agents-session";
    id.dataset["hostile"] = String(r.session_hostile);
    id.textContent = r.session;
    li.append(id);
    if (r.session_hostile) {
      li.append(badge(this.t("hostile-name")));
    }
    const op = document.createElement("span");
    op.className = "agents-op";
    op.dataset["hostile"] = String(r.last_op_hostile);
    op.textContent = r.last_op;
    li.append(op);
    if (r.last_op_hostile) {
      li.append(badge(this.t("hostile-name")));
    }
    // Pidió N y se le aprobaron M: no son lo mismo cuando contestó otra
    // ventana, cuando se denegó, o cuando caducó.
    const cuentas = document.createElement("span");
    cuentas.className = "agents-counts";
    cuentas.textContent = r.counts;
    li.append(cuentas);
    li.dataset["undoing"] = String(r.undoing);
    lista.append(li);
  }
  lista.setAttribute("aria-activedescendant", `agent-row-${String(agents.cursor)}`);
  caja.append(lista);
  this.agentsRoot.replaceChildren(caja);
  revelar(lista.querySelector(`#agent-row-${String(agents.cursor)}`) ?? undefined);
}

/**
 * Lo que imprimió un comando de extensión.
 *
 * Todo aquí lo escribe un tercero, y las tres cosas se dicen: quién
 * imprimió, qué comando, y si la salida se cortó — que el receptor no
 * puede deducir, porque el texto le llega ya corto.
 */
export function paintPluginOutput(
  this: Screen,
  output: ExtensionOutputView | null,
): void {
  if (output === null) {
    if (this.pluginOutputRoot.dataset["open"] === "true") {
      this.pluginOutputRoot.replaceChildren();
      this.pluginOutputRoot.dataset["open"] = "false";
    }
    return;
  }
  this.pluginOutputRoot.dataset["open"] = "true";
  const caja = document.createElement("section");
  caja.className = "plugin-output";
  caja.setAttribute("role", "dialog");
  caja.setAttribute("aria-modal", "true");
  caja.setAttribute("aria-label", this.t("plugin-output-title"));
  const titulo = document.createElement("h2");
  titulo.textContent = this.t("plugin-output-title");
  const quien = document.createElement("p");
  quien.className = "plugin-output-who";
  // Quién y qué, cada uno en su nodo y con SU bandera: unirlos en una
  // frase deja que un título de tercero con letras RTL reordene el par
  // entero, y una bandera para los dos acaba describiendo al otro.
  const plugin = document.createElement("span");
  plugin.className = "plugin-output-plugin";
  plugin.dataset["hostile"] = String(output.plugin.hostile);
  plugin.textContent = output.plugin.text;
  quien.append(plugin);
  if (output.plugin.hostile) {
    quien.append(badge(this.t("hostile-name")));
  }
  // El id reverse-DNS, que el core SÍ valida: dos extensiones pueden
  // llamarse igual y el nombre lo escribe el manifiesto.
  const ident = document.createElement("span");
  ident.className = "plugin-output-id";
  ident.textContent = output.plugin_id;
  quien.append(ident);
  if (output.command.text !== "") {
    const cmd = document.createElement("span");
    cmd.className = "plugin-output-command";
    cmd.dataset["hostile"] = String(output.command.hostile);
    cmd.textContent = output.command.text;
    quien.append(cmd);
    if (output.command.hostile) {
      quien.append(badge(this.t("hostile-name")));
    }
  }
  const cuerpo = document.createElement("pre");
  cuerpo.className = "plugin-output-text";
  cuerpo.dataset["hostile"] = String(output.text_hostile);
  // Vacío se DICE: un panel en blanco se lee como que no llegó a correr.
  cuerpo.textContent =
    output.lines.length === 0 ? this.t("plugin-output-empty") : output.lines.join("\n");
  caja.append(titulo, quien, cuerpo);
  if (output.text_hostile) {
    caja.append(badge(this.t("hostile-name")));
  }
  if (output.truncated) {
    const corte = document.createElement("p");
    corte.className = "plugin-output-truncated";
    corte.setAttribute("role", "status");
    corte.textContent = this.t("plugin-output-truncated");
    caja.append(corte);
  }
  this.pluginOutputRoot.replaceChildren(caja);
}

/**
 * La salida de un programa que quien hospeda corrió esperándolo (#312):
 * el comparador de dos ficheros. La misma caja que la salida de una
 * extensión —es la misma clase de texto, de otro programa— con el
 * comando que corrió y, si no arrancó, dicho.
 */
export function paintProgramOutput(this: Screen, output: ProgramOutputView | null): void {
  if (output === null) {
    if (this.programOutputRoot.dataset["open"] === "true") {
      this.programOutputRoot.replaceChildren();
      this.programOutputRoot.dataset["open"] = "false";
    }
    return;
  }
  this.programOutputRoot.dataset["open"] = "true";
  const caja = document.createElement("section");
  caja.className = "plugin-output program-output";
  caja.setAttribute("role", "dialog");
  caja.setAttribute("aria-modal", "true");
  caja.setAttribute("aria-label", this.t(output.title_key));
  const titulo = document.createElement("h2");
  titulo.textContent = this.t(output.title_key);
  const quien = document.createElement("p");
  quien.className = "plugin-output-who";
  const cmd = document.createElement("span");
  cmd.className = "plugin-output-command program-output-command";
  cmd.dataset["hostile"] = String(output.command.hostile);
  cmd.textContent = output.command.text;
  quien.append(cmd);
  if (output.command.hostile) {
    quien.append(badge(this.t("hostile-name")));
  }
  caja.append(titulo, quien);
  if (output.failed) {
    const no = document.createElement("p");
    no.className = "program-output-failed";
    no.setAttribute("role", "alert");
    no.textContent = this.t("program-output-failed");
    caja.append(no);
  }
  const cuerpo = document.createElement("pre");
  cuerpo.className = "plugin-output-text";
  cuerpo.dataset["hostile"] = String(output.text_hostile);
  cuerpo.textContent =
    output.lines.length === 0 ? this.t("plugin-output-empty") : output.lines.join("\n");
  caja.append(cuerpo);
  if (output.text_hostile) {
    caja.append(badge(this.t("hostile-name")));
  }
  if (output.truncated) {
    const corte = document.createElement("p");
    corte.className = "plugin-output-truncated";
    corte.setAttribute("role", "status");
    corte.textContent = this.t("plugin-output-truncated");
    caja.append(corte);
  }
  this.programOutputRoot.replaceChildren(caja);
}
