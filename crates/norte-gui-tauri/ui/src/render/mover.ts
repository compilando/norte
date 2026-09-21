// Mover un panel arrastrándolo por su título o su pestaña (ADR 0138), como
// en VS Code: al pasar por encima de otro panel se ve dónde caería —una de
// sus mitades, o el panel entero para unirse como pestaña— y al soltar se
// manda `move_slot`. Qué le pasa al árbol lo decide el host.

import type { Screen } from "../render";
import type { DropZone } from "../types";

/** Píxeles que hay que mover antes de que un clic pase a ser un arrastre:
 *  el título lleva migas que se pulsan, y la pestaña se elige con un clic. */
const UMBRAL = 6;

/** Qué parte del panel, desde cada borde, es «ese lado». El resto es el
 *  centro. */
const BORDE = 0.25;

/** Los huecos de cromo: ni se arrastran ni reciben. */
const CROMO = new Set(["status", "tasks"]);

/**
 * La zona de `rect` bajo el punto: el lado más cercano si está a menos de
 * un cuarto de él, y si no el centro.
 */
export function zonaDe(
  x: number,
  y: number,
  rect: { left: number; top: number; width: number; height: number },
): DropZone {
  const fx = rect.width > 0 ? (x - rect.left) / rect.width : 0.5;
  const fy = rect.height > 0 ? (y - rect.top) / rect.height : 0.5;
  const lados: [DropZone, number][] = [
    ["left", fx],
    ["right", 1 - fx],
    ["top", fy],
    ["bottom", 1 - fy],
  ];
  let mejor: [DropZone, number] = ["center", Number.POSITIVE_INFINITY];
  for (const lado of lados) {
    if (lado[1] < mejor[1]) {
      mejor = lado;
    }
  }
  return mejor[1] < BORDE ? mejor[0] : "center";
}

/** El panel bajo el punto y la zona, o `null` sobre el propio, el cromo o
 *  nada. */
function destinoEn(
  screen: Screen,
  x: number,
  y: number,
  origen: number,
): { slot: number; zone: DropZone; rect: DOMRect } | null {
  for (const [id, dom] of screen.slots) {
    if (CROMO.has(dom.root.dataset["kind"] ?? "")) {
      continue;
    }
    const r = dom.root.getBoundingClientRect();
    if (x < r.left || x >= r.right || y < r.top || y >= r.bottom) {
      continue;
    }
    return id === origen ? null : { slot: id, zone: zonaDe(x, y, r), rect: r };
  }
  return null;
}

/** El rectángulo de la zona, relativo al tablero. */
function pintarVelo(
  velo: HTMLElement,
  tablero: DOMRect,
  d: { zone: DropZone; rect: DOMRect } | null,
): void {
  if (d === null) {
    velo.hidden = true;
    return;
  }
  velo.hidden = false;
  velo.dataset["zone"] = d.zone;
  const r = d.rect;
  let [left, top, width, height] = [
    r.left - tablero.left,
    r.top - tablero.top,
    r.width,
    r.height,
  ];
  if (d.zone === "left" || d.zone === "right") {
    width = r.width / 2;
    if (d.zone === "right") {
      left += r.width / 2;
    }
  } else if (d.zone === "top" || d.zone === "bottom") {
    height = r.height / 2;
    if (d.zone === "bottom") {
      top += r.height / 2;
    }
  }
  velo.style.setProperty("left", `${String(left)}px`);
  velo.style.setProperty("top", `${String(top)}px`);
  velo.style.setProperty("width", `${String(width)}px`);
  velo.style.setProperty("height", `${String(height)}px`);
}

/**
 * Hace de `asa` —el título de un panel, o una pestaña— el sitio por donde
 * se arrastra el hueco `slotId`.
 *
 * Por `window` y no por captura del puntero: el arrastre sale del asa en el
 * primer píxel, y lo que importa es dónde se suelta. `Esc` lo cancela sin
 * que la tecla llegue al host, y el clic que el navegador dispara al soltar
 * sobre el asa se traga: un arrastre no es elegir la pestaña.
 */
export function hacerArrastrable(screen: Screen, asa: HTMLElement, slotId: number): void {
  asa.addEventListener("pointerdown", (e: PointerEvent) => {
    if (e.button !== 0) {
      return;
    }
    const doc = asa.ownerDocument;
    const ventana = doc.defaultView;
    if (ventana === null) {
      return;
    }
    const [x0, y0] = [e.clientX, e.clientY];
    let arrastrando = false;
    let destino: { slot: number; zone: DropZone; rect: DOMRect } | null = null;
    const velo = doc.createElement("div");
    velo.className = "drop-target";
    velo.hidden = true;

    const limpiar = (): void => {
      ventana.removeEventListener("pointermove", mover);
      ventana.removeEventListener("pointerup", soltar);
      ventana.removeEventListener("pointercancel", cancelar);
      ventana.removeEventListener("keydown", tecla, true);
      velo.remove();
      delete doc.documentElement.dataset["dragging"];
    };
    const tragarClic = (): void => {
      const trago = (ev: Event): void => {
        ev.stopPropagation();
        ev.preventDefault();
      };
      ventana.addEventListener("click", trago, { capture: true, once: true });
      // El clic del soltar llega JUSTO detrás del `pointerup`, antes que
      // cualquier temporizador. Si no llega —se soltó sobre otro panel, y
      // el navegador solo hace clic si se baja y se sube en el mismo sitio—
      // el trago se quita: si no, se comería el siguiente clic de verdad.
      ventana.setTimeout(() => {
        ventana.removeEventListener("click", trago, { capture: true });
      }, 0);
    };
    const mover = (ev: PointerEvent): void => {
      if (!arrastrando) {
        if (Math.hypot(ev.clientX - x0, ev.clientY - y0) < UMBRAL) {
          return;
        }
        arrastrando = true;
        doc.documentElement.dataset["dragging"] = "slot";
        screen.root.append(velo);
      }
      destino = destinoEn(screen, ev.clientX, ev.clientY, slotId);
      pintarVelo(velo, screen.root.getBoundingClientRect(), destino);
    };
    const soltar = (): void => {
      limpiar();
      if (!arrastrando) {
        return;
      }
      tragarClic();
      if (destino !== null) {
        screen.send({
          action: "move_slot",
          slot_id: slotId,
          target: destino.slot,
          zone: destino.zone,
        });
      }
    };
    const tecla = (ev: KeyboardEvent): void => {
      if (ev.key !== "Escape") {
        return;
      }
      ev.stopImmediatePropagation();
      ev.preventDefault();
      destino = null;
      arrastrando = false;
      limpiar();
    };
    // Un puntero que el sistema cancela (la ventana pierde el foco) no
    // suelta nada: sin esto los oyentes seguían puestos y el siguiente
    // `pointerup`, en cualquier sitio, mandaba un movimiento rancio.
    const cancelar = (): void => {
      destino = null;
      arrastrando = false;
      limpiar();
    };
    ventana.addEventListener("pointermove", mover);
    ventana.addEventListener("pointerup", soltar);
    ventana.addEventListener("pointercancel", cancelar);
    ventana.addEventListener("keydown", tecla, true);
  });
}
