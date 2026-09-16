// El arranque del renderer: engancha el bridge, pinta lo que llega y manda lo
// que el usuario hace. Nada más vive aquí.

// Las dos fuentes EMPAQUETADAS (spec 2026-09-11, V1). Solo los subconjuntos
// latinos y los pesos que se usan: cada fichero son ~24 KB de woff2, y Vite
// los mete en `dist/assets`, que Tauri incrusta en el binario. La `@font-face`
// de cada uno lleva `font-display: swap`, así que la pila del sistema pinta
// mientras cargan y nada se queda en blanco.
import "@fontsource/jetbrains-mono/latin-400.css";
import "@fontsource/jetbrains-mono/latin-ext-400.css";
import "@fontsource/jetbrains-mono/latin-700.css";
import "@fontsource/inter/latin-400.css";
import "@fontsource/inter/latin-ext-400.css";
import "@fontsource/inter/latin-600.css";

import { invokeMetrics, tauriPort } from "./bridge";
import type { HostPort } from "./bridge";
import { AltSolo, esParaElCampo, keyAction, keyInputOf } from "./keys";
import { Screen } from "./render";
import { Session } from "./session";
import { BRIDGE_VERSION } from "./types";
import type { Appearance, HostCatalog, UiAction } from "./types";

/** Medidas del spike: latencia tecla→pintado. Las lee el arnés de la 3.6. */
export interface Metrics {
  keyToPaint: number[];
  scrollToPaint: number[];
  updates: number;
  resyncs: number;
}

export async function boot(port: HostPort, doc: Document): Promise<Metrics> {
  const screenEl = doc.getElementById("screen");
  const menuEl = doc.getElementById("menu");
  const panelBarEl = doc.getElementById("panelbar");
  const paletteEl = doc.getElementById("palette");
  const whichKeyEl = doc.getElementById("whichkey");
  const helpEl = doc.getElementById("help");
  const settingsEl = doc.getElementById("settings");
  const extensionsEl = doc.getElementById("extensions");
  const themeEl = doc.getElementById("theme");
  const pickerEl = doc.getElementById("picker");
  const profilesEl = doc.getElementById("profiles");
  const layoutsEl = doc.getElementById("layouts");
  const columnsEl = doc.getElementById("columns");
  const searchEl = doc.getElementById("search");
  const compareEl = doc.getElementById("compare");
  const syncEl = doc.getElementById("sync");
  const agentsEl = doc.getElementById("agents");
  const pluginOutputEl = doc.getElementById("plugin-output");
  const programOutputEl = doc.getElementById("program-output");
  const viewerEl = doc.getElementById("viewer");
  const dialogsEl = doc.getElementById("dialogs");
  const aiRenameEl = doc.getElementById("ai-rename");
  const splashEl = doc.getElementById("splash");
  const fatalEl = doc.getElementById("fatal");
  if (
    screenEl === null ||
    menuEl === null ||
    panelBarEl === null ||
    paletteEl === null ||
    whichKeyEl === null ||
    helpEl === null ||
    settingsEl === null ||
    extensionsEl === null ||
    themeEl === null ||
    pickerEl === null ||
    profilesEl === null ||
    layoutsEl === null ||
    columnsEl === null ||
    searchEl === null ||
    compareEl === null ||
    syncEl === null ||
    agentsEl === null ||
    pluginOutputEl === null ||
    programOutputEl === null ||
    viewerEl === null ||
    dialogsEl === null ||
    aiRenameEl === null ||
    splashEl === null ||
    fatalEl === null
  ) {
    throw new Error("el documento no tiene los anclajes del renderer");
  }
  const metrics: Metrics = { keyToPaint: [], scrollToPaint: [], updates: 0, resyncs: 0 };
  const session = new Session();
  const catalog = await port.catalog();
  let catalogoActual = catalog;
  applyThemeFor(doc, catalog);
  applyAppearance(doc, catalog.appearance);
  // (El seguimiento del esquema del escritorio se engancha más abajo, en
  // cuanto existe `send`: desde el puente 66 no basta con enchufar variables.)
  // El umbral de «te estoy haciendo esperar» viene del host, que lo saca del
  // crate compartido con el terminal. Aquí solo se enchufa como variable CSS:
  // escribirlo en la hoja de estilos sería un tercer sitio donde vive el
  // mismo número, y el primero donde olvidarse de cambiarlo.
  doc.documentElement.style.setProperty(
    "--busy-delay",
    `${String(catalog.busy_threshold_ms ?? 250)}ms`,
  );

  // Lo que se está esperando pintar, y desde cuándo. La medida es del gesto
  // al frame que lo enseña: cualquier cosa más corta mide otra cosa.
  let pending: { at: number; what: "key" | "scroll" } | null = null;
  // El host promete que las acciones se aplican EN ORDEN, y esa promesa vale
  // para su buzón — no para el camino que lo alimenta. Dos `invoke` sueltos
  // son dos tareas que compiten por entrar: `focus_slot` y `select_row` de un
  // mismo click podían invertirse y el click no seleccionaba nada, de vez en
  // cuando. Una sola cadena de promesas: como mucho un `dispatch` en vuelo, y
  // la cola se drena en orden.
  let cola: Promise<void> = Promise.resolve();
  let muerto = false;
  const send = (action: UiAction): void => {
    if (muerto) {
      // Contrato incompatible o fallo fatal: no se manda NADA más. Pintar un
      // cartel y seguir despachando teclas es lo peor de los dos mundos.
      return;
    }
    cola = cola
      .then(() => port.dispatch(action))
      .then((ack) => {
        // Aceptada (aunque sea «no disponible»): el host la entendió, y el
        // aviso de una rechazada antes ya no cuenta nada.
        screen.accepted();
        if (ack.status === "unavailable") {
          // Un comando atenuado no es un error: el host ya dijo por qué.
          console.info("no disponible:", screen.t(ack.reason_key));
        }
      })
      .catch((e: unknown) => {
        // No deserializa o el contrato está roto: el host nunca la vio, así
        // que solo el renderer puede decirlo — en la barra, no en la consola
        // que nadie mira.
        screen.rejected(action, e);
      });
  };
  const screen = new Screen(
    screenEl,
    menuEl,
    panelBarEl,
    paletteEl,
    whichKeyEl,
    helpEl,
    settingsEl,
    extensionsEl,
    themeEl,
    pickerEl,
    profilesEl,
    layoutsEl,
    columnsEl,
    searchEl,
    compareEl,
    syncEl,
    agentsEl,
    pluginOutputEl,
    programOutputEl,
    viewerEl,
    dialogsEl,
    aiRenameEl,
    splashEl,
    catalog,
    send,
    () => port.imageBytes(),
  );

  const repaint = (): void => {
    const view = session.view();
    if (view === null) {
      return;
    }
    screen.paint(view);
    // La barra de menús acaba de reservar (o soltar) su fila: el host reparte
    // sobre el alto que este renderer declara, y sin volver a decirlo la
    // primera fila del listado se quedaría debajo de la barra.
    if (screen.takeViewportDirty()) {
      sendViewport(send, screen, doc);
    }
    if (pending !== null) {
      const { at, what } = pending;
      pending = null;
      requestAnimationFrame(() => {
        const dt = performance.now() - at;
        if (what === "key") {
          metrics.keyToPaint.push(dt);
        } else {
          metrics.scrollToPaint.push(dt);
        }
      });
    }
  };

  const resync = (): void => {
    metrics.resyncs += 1;
    void port.requestSnapshot();
  };

  await port.onUpdate((env) => {
    metrics.updates += 1;
    const out = session.receive(env);
    switch (out.kind) {
      case "applied":
        repaint();
        return;
      case "gap":
        // Jamás se deduce lo que faltó: se pide la foto entera.
        resync();
        return;
      case "incompatible":
        showFatal(
          fatalEl,
          // La del RENDERER, que es contra la que se comparó. `catalog`
          // trae la del host, así que decía `bridge 17 ≠ 17`.
          `bridge ${String(out.version)} ≠ ${String(BRIDGE_VERSION)}`,
        );
        return;
      case "notice":
        if (out.notice.notice === "fatal") {
          muerto = true;
          showFatal(fatalEl, screen.t(out.notice.key));
        }
        repaint();
        return;
      case "ignored":
        return;
    }
  });
  await port.onLagged(resync);
  // El catálogo cambió: hoy solo lo mueve el TEMA, y lo que hay que rehacer
  // son sus variables CSS. Los textos no se re-aplican porque no se mueven —
  // el idioma se fija una vez por proceso— y `Screen` se quedó con los suyos.
  await port.onCatalog(() => {
    void port
      .catalog()
      .then((cat) => {
        catalogoActual = cat;
        applyThemeFor(doc, cat);
        applyAppearance(doc, cat.appearance);
      })
      .catch((e: unknown) => {
        console.error("no se pudo releer el catálogo:", e);
      });
  });

  // El snapshot inicial viene en el MISMO sobre que el resto: una sola forma
  // en el cable es una sola forma que mantener.
  const first = await port.initialSnapshot();
  // El desenlace de la PRIMERA foto no se tira: es el único mensaje que un
  // renderer desfasado ve seguro, y descartarlo dejaba una ventana en blanco
  // en vez de la pantalla de incompatibilidad.
  const inicial = session.receive(first);
  if (inicial.kind === "incompatible") {
    muerto = true;
    showFatal(fatalEl, `bridge ${String(inicial.version)} ≠ ${String(BRIDGE_VERSION)}`);
    return metrics;
  }
  repaint();
  sendViewport(send, screen, doc);
  // El escritorio cambia de claro a oscuro y la ventana le sigue (V6).
  //
  // Dos cosas, no una. Las VARIABLES CSS las enchufa este lado, en el acto:
  // el catálogo ya trae las dos variantes resueltas y pasar por el host
  // costaría un parpadeo con la paleta equivocada. Pero desde el puente 66 el
  // color de una entrada va COCIDO en su fila y lo resuelve el host, que no
  // tiene escritorio al que preguntar: hay que decírselo, o pinta el cromo
  // con una variante y los NOMBRES con la otra.
  //
  // Se manda también AHORA, no solo al cambiar: el host arranca suponiendo
  // claro porque no puede saberlo, y este primer mensaje es lo que lo corrige
  // antes de que nadie mire.
  const w = doc.defaultView;
  if (w !== null && typeof w.matchMedia === "function") {
    const consulta = w.matchMedia("(prefers-color-scheme: dark)");
    send({ action: "set_color_scheme", dark: consulta.matches });
    consulta.addEventListener("change", (e) => {
      applyTheme(doc, themeFor(catalogoActual, e.matches));
      send({ action: "set_color_scheme", dark: e.matches });
    });
  }
  // Sin `norte.toml` de usuario, el asistente de primer arranque (spec
  // 2026-09-10): lo abre el host, que es quien lo escribe; aquí solo se le
  // dice que este arranque es el primero.
  if (catalog.first_run === true) {
    send({ action: "wizard_open" });
  }
  // Y la pantalla de arranque (ADR 0115), por la misma puerta: si la quiere
  // —`[ui] splash`— lo sabe el host, que también es quien le cede el sitio al
  // asistente. Aquí solo se avisa de que este es el arranque.
  //
  // Salvo que este arranque la haya apagado (`--no-splash`,
  // `NORTE_NO_SPLASH`): eso lo ve el proceso de la ventana, no el host, y sin
  // esta puerta cada captura automática saldría con la pantalla encima.
  if (catalog.no_splash !== true) {
    send({ action: "splash_open" });
  }

  // Alt solo va a la barra de menús (puente 68). Se mira ANTES del filtro de
  // modificadores: para `keyInputOf` un Alt solo no es una tecla, y no lo es
  // para el keymap; es un gesto aparte.
  const altSolo = new AltSolo();
  doc.addEventListener("keyup", (e) => {
    if (altSolo.arriba(e)) {
      // Sin esto WebKitGTK puede llevarse el foco a su propia barra.
      e.preventDefault();
      send({ action: "menu_toggle" });
    }
  });
  doc.addEventListener("mousedown", () => altSolo.soltar(), true);
  doc.addEventListener("wheel", () => altSolo.soltar(), { capture: true, passive: true });
  (doc.defaultView ?? window).addEventListener("blur", () => altSolo.soltar());

  doc.addEventListener("keydown", (e) => {
    altSolo.abajo(e);
    const k = keyInputOf(e);
    if (k === null) {
      return;
    }
    // Un campo de texto abierto es dueño de sus teclas: las que escriben y
    // las que EDITAN. La regla vive en `keys.ts`, donde se puede probar.
    if (esParaElCampo(k, e.target instanceof HTMLInputElement)) {
      return;
    }
    // Con la ayuda leyendo su CUERPO, las teclas de página son del scroll y
    // no del host: el cuerpo de una página cruza entero y quien lo desplaza
    // es el DOM. Sin esta salida, `preventDefault` mataba el scroll nativo y
    // una página más alta que la caja solo se podía leer con la rueda.
    if (screen.helpBodyScrolls() && (k.key === "PageDown" || k.key === "PageUp")) {
      return;
    }
    e.preventDefault();
    pending ??= { at: performance.now(), what: "key" };
    send(keyAction(k));
  });

  let resizeHandle: number | null = null;
  (doc.defaultView ?? window).addEventListener("resize", () => {
    if (resizeHandle !== null) {
      return;
    }
    resizeHandle = requestAnimationFrame(() => {
      resizeHandle = null;
      sendViewport(send, screen, doc);
    });
  });

  if (catalog.measure) {
    // La pasada de la 3.6: se mide sola, con eventos sintéticos y por el
    // camino COMPLETO del renderer (evento → invoke → host → evento →
    // DOM → siguiente frame). Fuera quedan la entrega del evento por el
    // sistema y el compositor, y eso se dice en el informe.
    await medir(doc, metrics, port, () => {
      pending ??= { at: performance.now(), what: "scroll" };
    });
  }
  return metrics;
}

/** Latencias de una pasada guionizada: teclas primero, scroll después. */
async function medir(
  doc: Document,
  metrics: Metrics,
  port: HostPort,
  marcarScroll: () => void,
): Promise<void> {
  const frame = (): Promise<number> =>
    new Promise((r) => {
      requestAnimationFrame(() => {
        r(performance.now());
      });
    });
  // Estado ESTABLE primero: un listado de cien mil entradas sigue llegando
  // por detrás durante segundos, y medir encima de ese relleno mide el
  // relleno. Se espera a que el host se calle (dos segundos sin
  // actualizaciones), con tope de un minuto.
  {
    let visto = metrics.updates;
    let quieto = 0;
    for (let i = 0; i < 600 && quieto < 20; i += 1) {
      await new Promise((r) => setTimeout(r, 100));
      quieto = metrics.updates === visto ? quieto + 1 : 0;
      visto = metrics.updates;
    }
  }

  // El SUELO de la máquina: qué tarda un frame sin hacer nada. Sin esta
  // referencia, «33 ms por frame» no distingue un renderer lento de una
  // pantalla que presenta a 30 Hz.
  const vacio: number[] = [];
  {
    let anterior = await frame();
    for (let i = 0; i < 200; i += 1) {
      const ahora = await frame();
      vacio.push(ahora - anterior);
      anterior = ahora;
    }
  }

  // El scroll LOCAL: lo que cuesta un frame mientras se arrastra, sin
  // esperar a Rust. Es lo que el presupuesto de la 3.6 llama «trabajo de las
  // filas visibles», y es el camino que NO cruza el bridge.
  const local: number[] = [];
  const scrollerLocal = doc.querySelector(".scroller");
  if (scrollerLocal instanceof HTMLElement) {
    let anterior = await frame();
    for (let i = 0; i < 200; i += 1) {
      scrollerLocal.scrollTop = i * 40;
      const ahora = await frame();
      local.push(ahora - anterior);
      anterior = ahora;
    }
  }

  // Las muestras las apunta `repaint`: del gesto al frame que lo enseña.
  for (let i = 0; i < 200; i += 1) {
    doc.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    await frame();
    await frame();
  }
  const scroller = doc.querySelector(".scroller");
  for (let i = 0; i < 200 && scroller instanceof HTMLElement; i += 1) {
    scroller.scrollTop = i * 40;
    marcarScroll();
    scroller.dispatchEvent(new Event("scroll"));
    await frame();
    await frame();
  }
  const teclas = metrics.keyToPaint;
  const scroll = metrics.scrollToPaint;
  const enviar = async (what: string, samples: number[]): Promise<void> => {
    try {
      await invokeMetrics(what, samples);
    } catch {
      // Sin la feature `metrics` el comando no existe: no es un fallo, es un
      // binario de producción. Las muestras se quedan en `__norteMetrics`.
    }
  };
  await enviar("key-to-paint", teclas);
  await enviar("scroll-to-rows", scroll);
  await enviar("scroll-frame-local", local);
  await enviar("idle-frame", vacio);
  await port.dispatch({ action: "resync" });
}

function sendViewport(send: (a: UiAction) => void, screen: Screen, doc: Document): void {
  const cell = screen.cell();
  const view = doc.defaultView ?? window;
  // Del elemento que de verdad lleva el reparto, no de la ventana: la barra
  // de menús le come una fila, y medir la ventana le declararía al host un
  // alto que no tiene dónde pintarse.
  const alto = doc.getElementById("screen")?.clientHeight ?? view.innerHeight;
  send({
    action: "set_viewport",
    width: Math.max(1, Math.floor(view.innerWidth / cell.w)),
    height: Math.max(1, Math.floor(alto / cell.h)),
  });
}

/**
 * Las variables que el tema ANTERIOR dejó puestas en el raíz.
 *
 * Hace falta porque el host manda solo lo que el tema DICE: desde los roles
 * de cromo (spec 2026-09-11, F2), un tema que no define `hover` no manda
 * `--hover`, y la hoja lo deriva con `var(--hover, var(--panel-focus-bg))` —
 * derivación que solo actúa mientras la variable esté SIN PONER. Antes ese
 * mecanismo no existía y todo nombre proyectado era un rol que cualquier
 * preset define, así que cada cambio pisaba el juego entero y bastaba con
 * asignar.
 */
let variablesPuestas: string[] = [];

/**
 * Los colores salen del tema resuelto EN RUST; aquí solo se enchufan.
 *
 * Se BORRA lo del tema anterior antes de poner lo del nuevo. Sin eso, pasar
 * de `vscode-dark` a `gruvbox-light` dejaba la paleta clara en todo lo que
 * sale de un rol CORE y la oscura en todo lo que sale de uno de cromo: la
 * paleta de comandos y los menús en #202020, el hover de fila en #2a2d2e y
 * las migas en #9d9d9d, sobre fondo claro y sin forma de arreglarlo salvo
 * reiniciar. `removeProperty` devuelve la variable a lo que diga `:root`,
 * que para las de cromo es «sin poner», que es justo lo que la derivación
 * necesita.
 */
export function applyTheme(doc: Document, theme: Record<string, string>): void {
  const raiz = doc.documentElement;
  for (const name of variablesPuestas) {
    raiz.style.removeProperty(`--${name}`);
  }
  for (const [name, value] of Object.entries(theme)) {
    raiz.style.setProperty(`--${name}`, value);
  }
  variablesPuestas = Object.keys(theme);
}

/**
 * El tema que toca según el escritorio (spec 2026-09-11, V6): la variante
 * clara u oscura de `[ui] theme_light` / `theme_dark` si la hay para ese
 * lado, y `[ui] theme` si no. Las tres llegan ya resueltas a variables: aquí
 * no se sabe qué es un tema, solo cuál de los tres juegos enchufar.
 */
export function themeFor(cat: HostCatalog, dark: boolean): Record<string, string> {
  const variante = dark ? cat.theme_dark : cat.theme_light;
  return variante ?? cat.theme;
}

/** Si el escritorio pide oscuro. Sin `matchMedia` (un test), no. */
function escritorioOscuro(doc: Document): boolean {
  const w = doc.defaultView;
  if (w === null || typeof w.matchMedia !== "function") {
    return false;
  }
  return w.matchMedia("(prefers-color-scheme: dark)").matches;
}

function applyThemeFor(doc: Document, cat: HostCatalog): void {
  applyTheme(doc, themeFor(cat, escritorioOscuro(doc)));
}

/** La proporción alto/tamaño de una fila. 22px de fila para 14px de letra es
 *  lo que la hoja de estilos dice desde la tipografía empaquetada (V1); se
 *  conserva al escalar para que una fila siga teniendo el mismo aire con
 *  cualquier tamaño. */
export const FILA_POR_TAMANO = 22 / 14;

/**
 * Fuentes y movimiento (`[ui] font`, `mono_font`, `font_size`,
 * `reduce_motion`).
 *
 * Las cuatro se cargaban, se validaban y se ofrecían en la pantalla de ajustes
 * sin que las leyera nadie. Un campo `null` NO se escribe: entonces manda lo
 * que ya hay, que para el movimiento es lo que diga el escritorio.
 *
 * El tamaño mueve también `--cell-h`, y eso no es un extra: esta ventana se
 * reparte en CELDAS, así que una letra más grande dentro de una fila del mismo
 * alto se sale de su fila. `--cell-w` se MIDE con la fuente puesta, porque el
 * ancho de una monoespaciada no es una proporción del tamaño: depende de la
 * familia, y una columna calculada con el ancho equivocado desalinea el
 * listado entero.
 */
export function applyAppearance(doc: Document, ap: Appearance | undefined): void {
  if (ap === undefined) {
    return;
  }
  const raiz = doc.documentElement;
  if (ap.font !== null) {
    raiz.style.setProperty("--ui-font", ap.font);
  }
  if (ap.mono_font !== null) {
    raiz.style.setProperty("--mono", ap.mono_font);
  }
  if (ap.font_size !== null) {
    raiz.style.setProperty("--ui-font-size", `${String(ap.font_size)}px`);
    raiz.style.setProperty(
      "--cell-h",
      `${String(Math.round(ap.font_size * FILA_POR_TAMANO))}px`,
    );
  }
  // Solo se AÑADE la petición: la configuración no puede contradecir a quien
  // ya pidió menos movimiento en su escritorio, así que `false` no apaga el
  // `prefers-reduced-motion` del sistema — deja de forzarlo y nada más.
  if (ap.reduce_motion === true) {
    raiz.dataset["reduceMotion"] = "true";
  } else {
    delete raiz.dataset["reduceMotion"];
  }
  medirCelda(doc);
}

/**
 * Mide el ancho de una celda con la fuente que hay AHORA puesta.
 *
 * Un carácter de una monoespaciada no ocupa una fracción fija de su tamaño:
 * cada familia tiene su avance. Con el ancho equivocado, cada columna del
 * listado cae un poco más lejos de donde el host la repartió, y al final de la
 * fila el error es de varios caracteres.
 *
 * Se miden VARIOS caracteres y se divide: uno solo redondea al pixel y el
 * error se multiplica por el número de columnas.
 */
function medirCelda(doc: Document): void {
  const regla = doc.createElement("span");
  regla.textContent = "M".repeat(50);
  regla.style.cssText =
    // La MISMA pila que el cuerpo (`--font-mono`), no `--mono` a secas: si
    // la configuración no dice fuente, el cuerpo pinta con la empaquetada y
    // medir con `monospace` daría el avance de otra familia.
    "position:absolute;visibility:hidden;white-space:pre;font-family:var(--font-mono, monospace);font-size:var(--ui-font-size, 14px)";
  doc.body.append(regla);
  const ancho = regla.getBoundingClientRect().width / 50;
  regla.remove();
  if (ancho > 0) {
    doc.documentElement.style.setProperty("--cell-w", `${String(ancho)}px`);
  }
}

function showFatal(el: HTMLElement, text: string): void {
  el.textContent = text;
  el.dataset["shown"] = "true";
}

if (typeof document !== "undefined" && document.getElementById("screen") !== null) {
  void boot(tauriPort, document)
    .then((m) => {
      // El arnés de medida de la tarea 3.6 lee esto; no hay nada sensible.
      (window as unknown as { __norteMetrics: Metrics }).__norteMetrics = m;
    })
    .catch((e: unknown) => {
      // Sin daemon, `catalog()` rechaza y `boot` lanza. El proceso monta la
      // ventana a propósito para poder DECIR qué pasó (un binario que muere
      // en el terminal no le cuenta nada a quien lo abrió desde un lanzador),
      // y sin este `catch` el usuario veía una ventana vacía.
      const fatal = document.getElementById("fatal");
      if (fatal !== null) {
        fatal.textContent = e instanceof Error ? e.message : String(e);
        fatal.dataset["shown"] = "true";
      }
    });
}
