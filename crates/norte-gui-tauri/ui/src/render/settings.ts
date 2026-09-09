// Pintores de `Screen` para settings (ola W10): funciones con `this: Screen`,
// enganchadas como propiedades en `render.ts`. El estado sigue en la clase.

import type { Screen } from "../render";
import type {
  ColumnsPickerView,
  LayoutPickerView,
  ProfilePickerView,
  PickerView,
  SettingsView,
  ThemeView,
} from "../types";
import { revelar, badge } from "./dom";

/**
 * Los ajustes (F11).
 *
 * Dos clases de sección y ninguna decisión aquí: el host manda el registro
 * con su valor ya resuelto y las ubicaciones ya saneadas. Lo único que este
 * método sabe es que una fila de ruta que falta se dice, que la lista es
 * un `listbox` con un cursor que el host lleva, y que un doble clic sobre
 * una fila la activa — girarla o pedir su valor lo decide el host.
 */
export function paintSettings(this: Screen, settings: SettingsView | null): void {
  if (settings === null) {
    this.settingsRoot.replaceChildren();
    this.settingsRoot.dataset["open"] = "false";
    return;
  }
  this.settingsRoot.dataset["open"] = "true";
  const caja = document.createElement("section");
  caja.className = "settings";
  caja.setAttribute("role", "dialog");
  caja.setAttribute("aria-modal", "true");
  caja.setAttribute("aria-label", this.t("settings-title"));

  const titulo = document.createElement("h1");
  titulo.textContent = this.t("settings-title");
  caja.append(titulo);

  const lista = document.createElement("ul");
  lista.className = "settings-rows";
  lista.setAttribute("role", "listbox");
  // El cursor cuenta filas ELEGIBLES: las cabeceras no entran, así que el
  // índice se lleva aparte del recorrido de las secciones.
  let i = 0;
  for (const sec of settings.sections) {
    const cabecera = document.createElement("li");
    cabecera.className = "settings-group";
    cabecera.setAttribute("role", "presentation");
    cabecera.textContent = sec.title;
    // Si TODAS las filas de la sección piden reiniciar, se dice UNA vez en
    // su cabecera. Cinco insignias idénticas no informan de nada: hacen
    // ruido justo encima de lo que sí varía, que es el valor.
    const todas =
      sec.section === "settings" &&
      sec.rows.length > 0 &&
      sec.rows.every((r) => r.restart_required);
    if (todas) {
      const marca = document.createElement("span");
      marca.className = "settings-badge";
      marca.textContent = this.t("settings-restart-badge");
      cabecera.append(" ", marca);
    }
    lista.append(cabecera);
    // El `switch` va FUERA del bucle de filas: dentro, TypeScript no
    // puede estrechar el tipo de la fila a partir de la sección, y una
    // fila de ruta y una de ajuste no comparten ni un campo.
    if (sec.section === "settings") {
      for (const r of sec.rows) {
        const fila = this.settingsRow(i, settings.cursor);
        const nombre = document.createElement("span");
        nombre.className = "settings-name";
        nombre.textContent = r.name;
        const valor = document.createElement("span");
        valor.className = "settings-value";
        valor.dataset["hostile"] = String(r.hostile);
        valor.textContent = r.value;
        if (r.hostile) {
          // La fila de RUTA de esta misma lista siempre lo dijo; la de
          // ajuste no, y las dos pintan en la misma columna.
          valor.append(badge(this.t("hostile-name")));
        }
        fila.append(nombre, valor);
        if (r.restart_required && !todas) {
          const marca = document.createElement("span");
          marca.className = "settings-badge";
          marca.textContent = this.t("settings-restart-badge");
          fila.append(marca);
        }
        const desc = document.createElement("span");
        desc.className = "settings-desc";
        desc.textContent = r.desc;
        fila.append(desc);
        lista.append(fila);
        i += 1;
      }
    } else {
      for (const r of sec.rows) {
        const fila = this.settingsRow(i, settings.cursor);
        const nombre = document.createElement("span");
        nombre.className = "settings-name";
        nombre.textContent = r.label;
        const valor = document.createElement("span");
        valor.className = "settings-value";
        valor.dataset["hostile"] = String(r.hostile);
        valor.textContent = r.display;
        fila.append(nombre, valor);
        if (r.hostile) {
          valor.append(badge(this.t("hostile-name")));
        }
        if (r.missing) {
          // Que un sitio no exista es un HECHO del diagnóstico y no un
          // error: una capa que nadie ha creado es lo normal.
          const falta = document.createElement("span");
          falta.className = "settings-missing";
          falta.textContent = this.t("settings-path-missing");
          fila.append(falta);
        }
        lista.append(fila);
        i += 1;
      }
    }
  }
  lista.setAttribute("aria-activedescendant", `settings-row-${String(settings.cursor)}`);
  caja.append(lista);
  this.settingsRoot.replaceChildren(caja);
  revelar(lista.querySelector(`#settings-row-${String(settings.cursor)}`) ?? undefined);
}

/**
 * El tema por dentro (F9).
 *
 * Cada rol con su color como MUESTRA, no como texto: un `#2d4f8a` no le
 * dice nada a nadie hasta que se ve al lado del cuadrado que pinta.
 */
export function paintTheme(this: Screen, theme: ThemeView | null): void {
  if (theme === null) {
    this.themeRoot.replaceChildren();
    this.themeRoot.dataset["open"] = "false";
    return;
  }
  this.themeRoot.dataset["open"] = "true";
  const caja = document.createElement("section");
  caja.className = "theme";
  caja.setAttribute("role", "dialog");
  caja.setAttribute("aria-modal", "true");
  caja.setAttribute("aria-label", this.t("theme-title"));

  const titulo = document.createElement("h1");
  titulo.textContent = `${this.t("theme-title")} · ${theme.name}`;
  caja.append(titulo);

  // La lista de temas, con el cursor. Moverse por ella previsualiza EN
  // VIVO: los colores de la ventana entera ya han cambiado cuando esto se
  // pinta, así que lo que hay debajo es el tema señalado.
  if (theme.choices.length > 0) {
    const elegir = document.createElement("ul");
    elegir.className = "theme-choices";
    elegir.setAttribute("role", "listbox");
    for (const [i, nombre] of theme.choices.entries()) {
      const fila = document.createElement("li");
      fila.className = "theme-choice";
      fila.id = `theme-choice-${String(i)}`;
      fila.setAttribute("role", "option");
      fila.setAttribute("aria-selected", String(theme.cursor === i));
      fila.textContent = nombre;
      elegir.append(fila);
    }
    elegir.setAttribute("aria-activedescendant", `theme-choice-${String(theme.cursor)}`);
    caja.append(elegir);
  }

  if (theme.unsupported_effects.length > 0) {
    // Se NOMBRAN. Un tema retro que se ve idéntico a los demás se lee como
    // roto, y el usuario va a buscar el bug donde no está.
    const aviso = document.createElement("p");
    aviso.className = "theme-effects";
    aviso.setAttribute("role", "note");
    const hostil = theme.unsupported_effects.some((e) => e.hostile);
    aviso.textContent = `${this.t("theme-effects-unsupported")} ${theme.unsupported_effects
      .map((e) => e.key)
      .join(" · ")}`;
    aviso.dataset["hostile"] = String(hostil);
    if (hostil) {
      // Las claves salen del fichero de tema: si se enmascararon, se dice.
      aviso.classList.add("hostile");
      aviso.append(badge(this.t("hostile-name")));
    }
    caja.append(aviso);
  }

  const sub = document.createElement("h2");
  sub.textContent = this.t("theme-roles");
  caja.append(sub);

  const lista = document.createElement("ul");
  lista.className = "theme-roles";
  for (const r of theme.roles) {
    const fila = document.createElement("li");
    fila.className = "theme-role";
    const muestra = document.createElement("span");
    muestra.className = "theme-swatch";
    // Por CSSOM y no por atributo `style`: la CSP lo bloquea.
    muestra.style.setProperty("background-color", r.color);
    const nombre = document.createElement("span");
    nombre.className = "theme-role-name";
    nombre.textContent = r.role;
    const hex = document.createElement("span");
    hex.className = "theme-role-hex";
    hex.textContent = r.color;
    fila.append(muestra, nombre, hex);
    lista.append(fila);
  }
  caja.append(lista);
  this.themeRoot.replaceChildren(caja);
}

/**
 * El selector de disposiciones, con la FORMA de la elegida al lado.
 *
 * La miniatura llega como líneas de texto pintadas por el mismo motor que
 * reparte la pantalla de verdad, así que no puede mentir sobre lo que va a
 * salir. Aquí solo se pone en un `<pre>`.
 */
/**
 * El selector de COLUMNAS: qué se pinta, en qué orden y con qué formato.
 *
 * Dice en su título el ALCANCE —un esquema o todos— y en su pie que lo
 * elegido vale para ESTA ventana y no se guarda: esta fase no escribe
 * configuración, y callarlo dejaría al usuario creyendo que acaba de
 * configurar norte.
 */
export function paintColumns(this: Screen, columns: ColumnsPickerView | null): void {
  if (columns === null) {
    this.columnsRoot.replaceChildren();
    this.columnsRoot.dataset["open"] = "false";
    return;
  }
  this.columnsRoot.dataset["open"] = "true";
  const caja = document.createElement("section");
  caja.className = "columns-picker";
  caja.setAttribute("role", "dialog");
  caja.setAttribute("aria-modal", "true");
  caja.setAttribute("aria-label", columns.title);

  const titulo = document.createElement("h1");
  titulo.textContent = columns.title;
  caja.append(titulo);

  const lista = document.createElement("ul");
  lista.className = "columns-rows";
  lista.setAttribute("role", "listbox");
  for (const [i, r] of columns.rows.entries()) {
    const fila = document.createElement("li");
    fila.className = "columns-row";
    fila.id = `columns-row-${String(i)}`;
    fila.setAttribute("role", "option");
    fila.setAttribute("aria-selected", String(columns.cursor === i));
    // Encendida o no, y si se puede tocar: las dos cosas al lector de
    // pantalla, no solo al que mira.
    fila.setAttribute("aria-checked", String(r.enabled));
    fila.dataset["enabled"] = String(r.enabled);
    fila.dataset["fixed"] = String(r.fixed);

    const marca = document.createElement("span");
    marca.className = "columns-check";
    marca.textContent = r.enabled ? "☑" : "☐";
    const nombre = document.createElement("span");
    nombre.className = "columns-label";
    nombre.dataset["hostile"] = String(r.hostile);
    nombre.textContent = r.label;
    if (r.hostile) {
      nombre.append(badge(this.t("hostile-name")));
    }
    fila.append(marca, nombre);
    if (r.format !== "") {
      // El formato vigente. Bloqueado = lo fija un ajuste del esquema y
      // aquí no se cicla; se pinta apagado en vez de desaparecer, porque
      // una tecla que no hace nada y no dice por qué es peor.
      const fmt = document.createElement("span");
      fmt.className = "columns-format";
      fmt.dataset["locked"] = String(r.format_locked);
      fmt.textContent = r.format;
      fila.append(fmt);
    }
    lista.append(fila);
  }
  if (columns.cursor < columns.rows.length) {
    lista.setAttribute("aria-activedescendant", `columns-row-${String(columns.cursor)}`);
  }
  caja.append(lista);

  const nota = document.createElement("p");
  nota.className = "columns-note";
  nota.setAttribute("role", "note");
  nota.textContent = columns.note;
  caja.append(nota);

  const pie = document.createElement("footer");
  pie.className = "columns-hint";
  // Del HOST (#287): los verbos `dialog.*` se pueden reatar, y una cadena
  // de aquí que nombre teclas concretas deja de ser cierta en cuanto
  // alguien lo hace.
  pie.textContent = columns.hint;
  caja.append(pie);
  this.columnsRoot.replaceChildren(caja);
}

/**
 * El selector de PERFILES (ADR 0079).
 *
 * Una fila que no se puede cargar se ENSEÑA con su motivo en vez de
 * desaparecer: esconder un directorio que el lector creó es peor que
 * enseñarlo roto. Y los dos avisos que la spec pide por su nombre —qué
 * otra cosa se llama igual, y qué perfil no puede guardar estado— van en
 * la fila, no en una nota al pie que nadie asocia.
 */
export function paintProfiles(this: Screen, profiles: ProfilePickerView | null): void {
  if (profiles === null) {
    this.profilesRoot.replaceChildren();
    this.profilesRoot.dataset["open"] = "false";
    return;
  }
  this.profilesRoot.dataset["open"] = "true";
  const caja = document.createElement("section");
  caja.className = "profiles";
  caja.setAttribute("role", "dialog");
  caja.setAttribute("aria-modal", "true");
  caja.setAttribute("aria-label", this.t("profile-picker-title"));

  const titulo = document.createElement("h1");
  titulo.textContent = this.t("profile-picker-title");
  caja.append(titulo);

  const lista = document.createElement("ul");
  lista.className = "profiles-rows";
  lista.setAttribute("role", "listbox");
  for (const [i, r] of profiles.rows.entries()) {
    const fila = document.createElement("li");
    fila.className = "profiles-row";
    fila.id = `profile-row-${String(i)}`;
    fila.setAttribute("role", "option");
    fila.setAttribute("aria-selected", String(profiles.cursor === i));
    fila.dataset["active"] = String(r.active);
    fila.dataset["broken"] = String(r.problem !== "");
    fila.addEventListener("click", () => {
      this.send({
        action: "profile_activate_row",
        row: i,
        generation: profiles.generation,
      });
    });
    const nombre = document.createElement("span");
    nombre.className = "profiles-name";
    nombre.textContent = r.name;
    if (r.name_hostile) {
      // El nombre son bytes de un directorio: si se enmascaró, se dice.
      nombre.append(badge(this.t("hostile-name")));
    }
    fila.append(nombre);
    if (r.title !== null) {
      const t = document.createElement("span");
      t.className = "profiles-title";
      t.textContent = r.title;
      fila.append(t);
    }
    // El orden de las notas es el del terminal: primero por qué NO carga,
    // luego lo que no podrá guardar, y al final el choque de nombre. De
    // más grave a menos.
    const nota =
      r.problem !== ""
        ? r.problem
        : r.no_state
          ? this.t("profile-picker-no-state")
          : r.clash;
    if (nota !== "") {
      const n = document.createElement("span");
      n.className = "profiles-note";
      n.textContent = nota;
      fila.append(n);
    }
    lista.append(fila);
  }
  lista.setAttribute("aria-activedescendant", `profile-row-${String(profiles.cursor)}`);
  if (profiles.rows.length === 0) {
    const vacio = document.createElement("li");
    vacio.className = "empty";
    vacio.textContent = this.t("profile-picker-empty");
    lista.append(vacio);
  }
  caja.append(lista);
  this.profilesRoot.replaceChildren(caja);
}

export function paintLayouts(this: Screen, layouts: LayoutPickerView | null): void {
  if (layouts === null) {
    this.layoutsRoot.replaceChildren();
    this.layoutsRoot.dataset["open"] = "false";
    return;
  }
  this.layoutsRoot.dataset["open"] = "true";
  const caja = document.createElement("section");
  caja.className = "layouts";
  caja.setAttribute("role", "dialog");
  caja.setAttribute("aria-modal", "true");
  caja.setAttribute("aria-label", layouts.title);

  const titulo = document.createElement("h1");
  titulo.textContent = layouts.title;
  caja.append(titulo);

  const cuerpo = document.createElement("div");
  cuerpo.className = "layouts-body";
  const lista = document.createElement("ul");
  lista.className = "layouts-rows";
  lista.setAttribute("role", "listbox");
  for (const [i, r] of layouts.rows.entries()) {
    const fila = document.createElement("li");
    fila.className = "layouts-row";
    fila.id = `layout-row-${String(i)}`;
    fila.setAttribute("role", "option");
    fila.setAttribute("aria-selected", String(layouts.cursor === i));
    fila.dataset["broken"] = String(r.broken);
    fila.addEventListener("click", () => {
      this.send({ action: "layout_activate_row", row: i });
    });
    const nombre = document.createElement("span");
    nombre.className = "layouts-name";
    nombre.dataset["hostile"] = String(r.hostile);
    nombre.textContent = r.name;
    if (r.hostile) {
      nombre.append(badge(this.t("hostile-name")));
    }
    fila.append(nombre);
    if (r.factory) {
      const marca = document.createElement("span");
      marca.className = "layouts-tag";
      marca.textContent = this.t("layout-picker-factory");
      fila.append(marca);
    }
    if (r.shares_keymap_name) {
      // Se AVISA: elegir esta disposición no cambia ni una tecla, y sin la
      // línea la coincidencia de nombre es una trampa.
      const aviso = document.createElement("span");
      aviso.className = "layouts-warn";
      aviso.textContent = this.t("layout-picker-shares-keymap");
      fila.append(aviso);
    }
    lista.append(fila);
  }
  lista.setAttribute("aria-activedescendant", `layout-row-${String(layouts.cursor)}`);
  cuerpo.append(lista);

  if (layouts.problem === "") {
    const vista = document.createElement("pre");
    vista.className = "layouts-preview";
    vista.setAttribute("aria-hidden", "true");
    vista.textContent = layouts.preview.join("\n");
    cuerpo.append(vista);
  } else {
    const roto = document.createElement("p");
    roto.className = "layouts-problem";
    roto.textContent = layouts.problem;
    roto.dataset["hostile"] = String(layouts.problem_hostile);
    if (layouts.problem_hostile) {
      roto.classList.add("hostile");
      roto.append(badge(this.t("hostile-name")));
    }
    cuerpo.append(roto);
  }
  caja.append(cuerpo);
  this.layoutsRoot.replaceChildren(caja);
  revelar(lista.querySelector(`#layout-row-${String(layouts.cursor)}`) ?? undefined);
}

/** El selector de volúmenes. */
export function paintPicker(this: Screen, picker: PickerView | null): void {
  if (picker === null) {
    this.pickerRoot.replaceChildren();
    this.pickerRoot.dataset["open"] = "false";
    return;
  }
  this.pickerRoot.dataset["open"] = "true";
  const caja = document.createElement("section");
  caja.className = "picker";
  caja.setAttribute("role", "dialog");
  caja.setAttribute("aria-modal", "true");
  caja.setAttribute("aria-label", picker.title);

  const titulo = document.createElement("h1");
  titulo.textContent = picker.title;
  caja.append(titulo);

  if (picker.empty !== "") {
    // La frase la escribe el host: distingue «todavía preguntando» de «no
    // hay ninguno», que es la distinción que una lista vacía se come.
    const vacio = document.createElement("p");
    vacio.className = "picker-empty";
    vacio.setAttribute("role", "status");
    vacio.textContent = picker.empty;
    caja.append(vacio);
  }

  const lista = document.createElement("ul");
  lista.className = "picker-rows";
  lista.setAttribute("role", "listbox");
  for (const [i, r] of picker.rows.entries()) {
    const fila = document.createElement("li");
    fila.className = "picker-row";
    fila.id = `picker-row-${String(i)}`;
    fila.setAttribute("role", "option");
    fila.setAttribute("aria-selected", String(picker.cursor === i));
    fila.addEventListener("click", () => {
      // La generación de ESTA pintada: si la lista cambió entre el
      // pintado y el click, el host lo rechaza en vez de elegir otra fila.
      this.send({
        action: "picker_select_row",
        row: i,
        generation: picker.generation,
      });
    });
    const label = document.createElement("span");
    label.className = "picker-label";
    label.dataset["hostile"] = String(r.hostile);
    label.textContent = r.label;
    if (r.hostile) {
      label.append(badge(this.t("hostile-name")));
    }
    const detalle = document.createElement("span");
    detalle.className = "picker-detail";
    detalle.textContent = r.detail;
    fila.append(label, detalle);
    lista.append(fila);
  }
  if (picker.cursor !== null) {
    lista.setAttribute("aria-activedescendant", `picker-row-${String(picker.cursor)}`);
    revelar(lista.querySelector(`#picker-row-${String(picker.cursor)}`) ?? undefined);
  }
  caja.append(lista);
  this.pickerRoot.replaceChildren(caja);
}

/**
 * El `<li>` de una fila de ajustes, con su cursor, su click y su doble click.
 *
 * El click SEÑALA y el doble click ACTIVA, como en un listado: el primero
 * mueve el cursor y el segundo hace lo que `enter`. Los dos viajan —un
 * doble click es también un click— y el host los ordena.
 */
export function settingsRow(this: Screen, i: number, cursor: number): HTMLElement {
  const fila = document.createElement("li");
  fila.className = "settings-row";
  fila.id = `settings-row-${String(i)}`;
  fila.setAttribute("role", "option");
  fila.setAttribute("aria-selected", String(cursor === i));
  fila.addEventListener("click", () => {
    this.send({ action: "settings_select_row", row: i });
  });
  fila.addEventListener("dblclick", () => {
    this.send({ action: "settings_activate", row: i });
  });
  return fila;
}
