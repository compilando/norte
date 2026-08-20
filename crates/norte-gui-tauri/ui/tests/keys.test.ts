import { describe, expect, it } from "vitest";

import { keyInputOf } from "../src/keys";

function ev(init: KeyboardEventInit): KeyboardEvent {
  return new KeyboardEvent("keydown", init);
}

describe("las teclas", () => {
  it("viajan con el nombre del navegador y sus modificadores", () => {
    expect(keyInputOf(ev({ key: "ArrowDown" }))).toEqual({
      key: "ArrowDown",
      ctrl: false,
      alt: false,
      shift: false,
      meta: false,
    });
    expect(keyInputOf(ev({ key: "F5", ctrlKey: true, shiftKey: true }))?.ctrl).toBe(true);
  });

  it("un modificador SOLO no es una tecla", () => {
    for (const key of ["Shift", "Control", "Alt", "Meta"]) {
      expect(keyInputOf(ev({ key }))).toBeNull();
    }
  });

  it("a media composición de IME no se manda nada", () => {
    expect(keyInputOf(ev({ key: "a", isComposing: true }))).toBeNull();
  });

  it("no resuelve NADA: manda la tecla, no el comando", () => {
    // Si esto dejara de ser cierto habría dos keymaps (decisión D14).
    const k = keyInputOf(ev({ key: "q", ctrlKey: true }));
    expect(k).toEqual({ key: "q", ctrl: true, alt: false, shift: false, meta: false });
  });
});
