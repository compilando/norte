// El arranque del renderer: engancha el bridge, pinta lo que llega y manda lo
// que el usuario hace. Nada más vive aquí.

import { invokeMetrics, tauriPort } from "./bridge";
import type { HostPort } from "./bridge";
import { keyAction, keyInputOf } from "./keys";
import { Screen } from "./render";
import { Session } from "./session";
import { BRIDGE_VERSION } from "./types";
import type { UiAction } from "./types";

/** Medidas del spike: latencia tecla→pintado. Las lee el arnés de la 3.6. */
export interface Metrics {
  keyToPaint: number[];
  scrollToPaint: number[];
  updates: number;
  resyncs: number;
}

export async function boot(port: HostPort, doc: Document): Promise<Metrics> {
  const screenEl = doc.getElementById("screen");
  const paletteEl = doc.getElementById("palette");
  const whichKeyEl = doc.getElementById("whichkey");
  const helpEl = doc.getElementById("help");
  const settingsEl = doc.getElementById("settings");
  const extensionsEl = doc.getElementById("extensions");
  const themeEl = doc.getElementById("theme");
  const pickerEl = doc.getElementById("picker");
  const layoutsEl = doc.getElementById("layouts");
  const columnsEl = doc.getElementById("columns");
  const searchEl = doc.getElementById("search");
  const compareEl = doc.getElementById("compare");
  const syncEl = doc.getElementById("sync");
  const viewerEl = doc.getElementById("viewer");
  const dialogsEl = doc.getElementById("dialogs");
  const aiRenameEl = doc.getElementById("ai-rename");
  const fatalEl = doc.getElementById("fatal");
  if (
    screenEl === null ||
    paletteEl === null ||
    whichKeyEl === null ||
    helpEl === null ||
    settingsEl === null ||
    extensionsEl === null ||
    themeEl === null ||
    pickerEl === null ||
    layoutsEl === null ||
    columnsEl === null ||
    searchEl === null ||
    compareEl === null ||
    syncEl === null ||
    viewerEl === null ||
    dialogsEl === null ||
    aiRenameEl === null ||
    fatalEl === null
  ) {
    throw new Error("el documento no tiene los anclajes del renderer");
  }
  const metrics: Metrics = { keyToPaint: [], scrollToPaint: [], updates: 0, resyncs: 0 };
  const session = new Session();
  const catalog = await port.catalog();
  applyTheme(doc, catalog.theme);

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
        if (ack.status === "unavailable") {
          // Un comando atenuado no es un error: el host ya dijo por qué.
          console.info("no disponible:", screen.t(ack.reason_key));
        }
      })
      .catch((e: unknown) => {
        console.error("el host no aceptó la acción:", e);
      });
  };
  const screen = new Screen(
    screenEl,
    paletteEl,
    whichKeyEl,
    helpEl,
    settingsEl,
    extensionsEl,
    themeEl,
    pickerEl,
    layoutsEl,
    columnsEl,
    searchEl,
    compareEl,
    syncEl,
    viewerEl,
    dialogsEl,
    aiRenameEl,
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

  doc.addEventListener("keydown", (e) => {
    const k = keyInputOf(e);
    if (k === null) {
      return;
    }
    const target = e.target;
    // Un campo de texto abierto es dueño de las teclas de TEXTO. «Una
    // tecla de texto» se mide en puntos de código, no en unidades UTF-16:
    // `length === 1` deja fuera un emoji (dos unidades) y una `é` en NFD
    // (macOS), así que `preventDefault` se los llevaba y no se podían
    // escribir en un nombre.
    const esTexto = !k.ctrl && !k.alt && !k.meta && [...k.key].length === 1;
    if (target instanceof HTMLInputElement && esTexto) {
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
  send({
    action: "set_viewport",
    width: Math.max(1, Math.floor(view.innerWidth / cell.w)),
    height: Math.max(1, Math.floor(view.innerHeight / cell.h)),
  });
}

/** Los colores salen del tema resuelto EN RUST; aquí solo se enchufan. */
function applyTheme(doc: Document, theme: Record<string, string>): void {
  for (const [name, value] of Object.entries(theme)) {
    doc.documentElement.style.setProperty(`--${name}`, value);
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
