// From the browser's event to the keymap's vocabulary. And nothing else.
//
// Who resolves the key — a half-finished prefix, a counter, which command
// `ctrl+shift+f5` is bound to — is Rust, with the same resolver and the same
// presets as the TUI (decision D14). Only the event's SHAPE is translated
// here.

import type { KeyInput, UiAction } from "./types";

/** Keys that are not a key on their own: they only modify the next one. */
const MODIFIER_ONLY = new Set(["Shift", "Control", "Alt", "Meta", "AltGraph", "Dead"]);

/** `null` if the event is not a key the host should see. */
export function keyInputOf(e: KeyboardEvent): KeyInput | null {
  if (e.isComposing) {
    // Mid-IME composition: the text does not exist yet.
    return null;
  }
  if (MODIFIER_ONLY.has(e.key)) {
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

/** Keys that EDIT a text field without typing a character. */
const EDITING = new Set([
  "Backspace",
  "Delete",
  "ArrowLeft",
  "ArrowRight",
  "Home",
  "End",
]);

/**
 * Is this key the focused text FIELD's, and not the host's?
 *
 * An open field owns the text keys and the editing keys. It already owned
 * the text ones; it did not own the editing ones, and that missing half made
 * `preventDefault` cancel the field's own delete and paste — and the host
 * swallowed them without doing anything. So a half-typed name could not be
 * corrected and nothing could be pasted into it.
 *
 * It barely showed with an `mkdir` — you just retype it — and stops being a
 * minor annoyance in a PASSWORD field (#327): forty random characters,
 * without seeing what is typed, and the only way out of a typo was Escape,
 * which abandons navigation. The TUI has always had both, and its comment
 * says why pasting matters here more than anywhere: pasting from a password
 * manager is how most people answer that dialog.
 *
 * Lives here, exported, and not inside `main.ts`'s handler, because there
 * was no way to test it there — and it was not tested.
 */
export function esParaElCampo(k: KeyInput, hayCampo: boolean): boolean {
  if (!hayCampo) {
    return false;
  }
  // "A text key" is measured in code points, not UTF-16 units: `length === 1`
  // leaves out an emoji (two units) and an NFD `é` (macOS), so
  // `preventDefault` used to take them and they could not be typed.
  const isText = !k.ctrl && !k.alt && !k.meta && [...k.key].length === 1;
  // The field's clipboard and undo. `meta` left out on purpose: on this
  // desktop it is not an editing modifier, and letting it through would open
  // a gap host chords could slip through.
  const isClipboard = k.ctrl && !k.alt && !k.meta && "vacxz".includes(k.key);
  return isText || EDITING.has(k.key) || isClipboard;
}

export function keyAction(k: KeyInput): UiAction {
  return { action: "key", ...k };
}

/** The minimal shape of a keyboard event [`AltSolo`] looks at. */
export interface TeclaCruda {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

/**
 * Alt pressed and released ALONE: the desktop gesture for going to the menu
 * bar (bridge 68).
 *
 * It arms when Alt goes down with no other modifier and disarms with
 * ANYTHING else in between — another key, a click, losing focus — so
 * `Alt+F4`, `Alt+Tab` or dragging with Alt do not open the menu on release.
 * `AltGraph` is not Alt: on a Spanish keyboard it types `@` and `#`, and
 * opening the menu there would break half of what gets typed.
 *
 * It only detects. What happens next — collapse, open, nothing if there is a
 * dialog — is decided by the host.
 */
export class AltSolo {
  private armado = false;

  /** A `keydown`. Alt's held-down repeat does not disarm it. */
  abajo(e: TeclaCruda): void {
    this.armado = e.key === "Alt" && !e.ctrlKey && !e.shiftKey && !e.metaKey;
  }

  /** A `keyup`: `true` if it closes an Alt-alone. */
  arriba(e: TeclaCruda): boolean {
    const fue = this.armado && e.key === "Alt";
    this.armado = false;
    return fue;
  }

  /** Something that is not the keyboard got in the way (click, wheel, focus). */
  soltar(): void {
    this.armado = false;
  }
}
