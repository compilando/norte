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

describe("una tecla de TEXTO", () => {
  it("se mide en puntos de código, no en unidades UTF-16", () => {
    // Un emoji son dos unidades UTF-16 y UN punto de código: si se mide con
    // `length`, el campo de texto no lo recibe y no se puede escribir en un
    // nombre.
    const emoji = keyInputOf(ev({ key: "😀" }));
    expect(emoji).not.toBeNull();
    expect([...(emoji?.key ?? "")].length).toBe(1);
    expect((emoji?.key ?? "").length).toBe(2);
  });

  it("una é en NFD son DOS puntos de código: no es una tecla de texto", () => {
    const nfd = keyInputOf(ev({ key: "e\u0301" }));
    expect(nfd).not.toBeNull();
    expect([...(nfd?.key ?? "")].length).toBe(2);
  });
});
