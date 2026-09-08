// Pintores de `Screen` para extensions (ola W10): funciones con `this: Screen`,
// enganchadas como propiedades en `render.ts`. El estado sigue en la clase.

import type { Screen } from "../render";
import type {
  ExtensionsView,
  AgentsView,
  ExtensionCommandView,
  ExtensionOutputView,
  ProgramOutputView,
} from "../types";
import { revelar, badge } from "./dom";

/**
 * El gestor de extensiones (F12), en solo lectura.
 *
 * Las capabilities van en la FILA y no escondidas tras un gesto: son la
 * decisión que un humano aprueba, y esta ventana la enseña sin poder
 * tomarla. No hay ni un control para aprobar o encender: lo que no está no
 * se pulsa por accidente.
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

  const titulo = document.createElement("h1");
  titulo.textContent = this.t("ext-title");
  caja.append(titulo);

  if (ext.loading) {
    // «Cargando» y «ninguna» no son lo mismo, y una lista vacía sin este
    // aviso se lee como lo segundo.
    const cargando = document.createElement("p");
    cargando.className = "extensions-note";
    cargando.setAttribute("role", "status");
    cargando.textContent = this.t("ext-loading");
    caja.append(cargando);
  } else if (ext.rows.length === 0) {
    const vacio = document.createElement("p");
    vacio.className = "extensions-note";
    vacio.textContent = this.t("ext-empty");
    caja.append(vacio);
  }

  const lista = document.createElement("ul");
  lista.className = "extensions-rows";
  lista.setAttribute("role", "listbox");
  for (const [i, r] of ext.rows.entries()) {
    const fila = document.createElement("li");
    fila.className = "extensions-row";
    fila.id = `extension-row-${String(i)}`;
    fila.setAttribute("role", "option");
    fila.setAttribute("aria-selected", String(ext.cursor === i));
    fila.addEventListener("click", () => {
      this.send({ action: "extension_select_row", row: i });
    });

    const nombre = document.createElement("span");
    nombre.className = "extensions-name";
    nombre.textContent = r.name;
    const version = document.createElement("span");
    version.className = "extensions-version";
    version.textContent = r.version;
    fila.append(nombre, version);

    const estado = document.createElement("span");
    estado.className = "extensions-state";
    // DOS hechos independientes, y se dicen los dos: una extensión
    // aprobada pero apagada no es lo mismo que una sin aprobar.
    estado.dataset["approved"] = String(r.approved);
    estado.dataset["enabled"] = String(r.enabled);
    estado.textContent = r.approved
      ? this.t(r.enabled ? "ext-state-on" : "ext-state-off")
      : this.t("ext-unapproved");
    fila.append(estado);

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

    const caps = document.createElement("ul");
    caps.className = "extensions-caps";
    for (const c of r.capabilities) {
      const cap = document.createElement("li");
      cap.className = "extensions-cap";
      cap.textContent = c;
      caps.append(cap);
    }
    if (r.capabilities.length > 0) {
      fila.append(caps);
    }
    lista.append(fila);
  }
  if (ext.rows.length > 0) {
    lista.setAttribute("aria-activedescendant", `extension-row-${String(ext.cursor)}`);
  }
  caja.append(lista);

  if (ext.detail !== null) {
    // La ficha se titula con el NOMBRE de su extensión, no con «sus
    // ajustes» a secas: con la lista desplazada, la fila elegida puede no
    // estar a la vista y la ficha se quedaba sin dueño visible.
    const suya = ext.rows.find((r) => r.id === ext.detail?.id);
    caja.append(this.extensionDetail(ext.detail, suya?.name ?? ""));
  }
  if (ext.errors.length > 0) {
    const errores = document.createElement("ul");
    errores.className = "extensions-errors";
    for (const e of ext.errors) {
      const li = document.createElement("li");
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
  revelar(lista.querySelector(`#extension-row-${String(ext.cursor)}`) ?? undefined);
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
