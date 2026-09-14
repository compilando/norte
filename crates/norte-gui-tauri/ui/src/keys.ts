// Del evento del navegador al vocabulario del keymap. Y nada más.
//
// Quién resuelve la tecla —un prefijo a medias, un contador, qué comando lleva
// ligado `ctrl+shift+f5`— es Rust, con el mismo resolver y los mismos presets
// que el TUI (decisión D14). Aquí solo se traduce la FORMA del evento.

import type { KeyInput, UiAction } from "./types";

/** Teclas que por sí solas no son una tecla: solo modifican a la siguiente. */
const SOLO_MODIFICADOR = new Set(["Shift", "Control", "Alt", "Meta", "AltGraph", "Dead"]);

/** `null` si el evento no es una tecla que el host deba ver. */
export function keyInputOf(e: KeyboardEvent): KeyInput | null {
  if (e.isComposing) {
    // Mitad de una composición de IME: el texto todavía no existe.
    return null;
  }
  if (SOLO_MODIFICADOR.has(e.key)) {
    return null;
  }
  return {
    key: e.key,
    ctrl: e.ctrlKey,
    alt: e.altKey,
    shift: e.shiftKey,
    meta: e.metaKey,
  };
}

/** Teclas que EDITAN un campo de texto sin escribir un carácter. */
const EDICION = new Set([
  "Backspace",
  "Delete",
  "ArrowLeft",
  "ArrowRight",
  "Home",
  "End",
]);

/**
 * ¿Esta tecla es del CAMPO de texto que tiene el foco, y no del host?
 *
 * Un campo abierto es dueño de las teclas de texto y de las de edición. De las
 * de texto lo era ya; de las de edición no, y esa mitad que faltaba hacía que
 * `preventDefault` cancelara el borrado y el pegado del propio campo — y el
 * host se los tragaba sin hacer nada. O sea que un nombre a medio escribir no
 * se podía corregir y no se podía pegar nada dentro.
 *
 * Se notaba poco con un `mkdir` —se reescribe y ya— y deja de ser una molestia
 * en el campo de una CONTRASEÑA (#327): cuarenta caracteres aleatorios, sin ver
 * lo que se teclea, y la única salida de una errata era Escape, que abandona la
 * navegación. La TUI tiene las dos cosas desde siempre, y su comentario dice
 * por qué el pegado importa aquí más que en ningún sitio: pegar desde un gestor
 * de contraseñas es como la mayoría de la gente contesta ese diálogo.
 *
 * Vive aquí, exportada, y no dentro del manejador de `main.ts`, porque ahí no
 * había forma de probarla — y no estaba probada.
 */
export function esParaElCampo(k: KeyInput, hayCampo: boolean): boolean {
  if (!hayCampo) {
    return false;
  }
  // «Una tecla de texto» se mide en puntos de código, no en unidades UTF-16:
  // `length === 1` deja fuera un emoji (dos unidades) y una `é` en NFD (macOS),
  // así que `preventDefault` se los llevaba y no se podían escribir.
  const esTexto = !k.ctrl && !k.alt && !k.meta && [...k.key].length === 1;
  // El portapapeles y el deshacer del campo. `meta` fuera a propósito: en este
  // escritorio no es un modificador de edición, y dejarlo pasar abriría un
  // hueco por el que se colarían acordes del host.
  const esPortapapeles = k.ctrl && !k.alt && !k.meta && "vacxz".includes(k.key);
  return esTexto || EDICION.has(k.key) || esPortapapeles;
}

export function keyAction(k: KeyInput): UiAction {
  return { action: "key", ...k };
}

/** La forma mínima de un evento de teclado que mira [`AltSolo`]. */
export interface TeclaCruda {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

/**
 * Alt pulsado y soltado SOLO: el gesto de escritorio para ir a la barra de
 * menús (puente 68).
 *
 * Se arma al bajar Alt sin otro modificador y se desarma con CUALQUIER otra
 * cosa en medio —otra tecla, un clic, perder el foco—, así que `Alt+F4`,
 * `Alt+Tab` o arrastrar con Alt no abren el menú al soltar. `AltGraph` no es
 * Alt: con un teclado español escribe `@` y `#`, y abrir el menú ahí
 * rompería la mitad de lo que se teclea.
 *
 * Solo detecta. Qué pasa después —plegar, abrir, nada si hay un diálogo— lo
 * decide el host.
 */
export class AltSolo {
  private armado = false;

  /** Un `keydown`. La repetición de Alt mantenido no desarma. */
  abajo(e: TeclaCruda): void {
    this.armado = e.key === "Alt" && !e.ctrlKey && !e.shiftKey && !e.metaKey;
  }

  /** Un `keyup`: `true` si cierra un Alt solo. */
  arriba(e: TeclaCruda): boolean {
    const fue = this.armado && e.key === "Alt";
    this.armado = false;
    return fue;
  }

  /** Algo que no es teclado se metió en medio (clic, rueda, foco). */
  soltar(): void {
    this.armado = false;
  }
}
