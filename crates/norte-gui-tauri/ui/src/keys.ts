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

export function keyAction(k: KeyInput): UiAction {
  return { action: "key", ...k };
}
