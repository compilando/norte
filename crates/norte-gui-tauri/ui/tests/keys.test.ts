import { describe, expect, it } from "vitest";

import { AltSolo, esParaElCampo, keyInputOf } from "../src/keys";

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

describe("un campo de texto abierto", () => {
  const k = (
    key: string,
    mods: Partial<Record<"ctrl" | "alt" | "meta" | "shift", boolean>> = {},
  ) => ({
    key,
    ctrl: mods.ctrl ?? false,
    alt: mods.alt ?? false,
    shift: mods.shift ?? false,
    meta: mods.meta ?? false,
  });

  it("se queda las teclas que ESCRIBEN", () => {
    expect(esParaElCampo(k("a"), true)).toBe(true);
    expect(esParaElCampo(k("\u{1F600}"), true)).toBe(true);
  });

  it("y las que EDITAN, que es la mitad que faltaba", () => {
    // Sin esto, `preventDefault` cancelaba el borrado del propio campo y el
    // host se lo tragaba: un nombre a medio escribir no se pod\u00eda corregir, y
    // en el campo de una contrase\u00f1a (#327) \u2014cuarenta caracteres, sin verlos\u2014
    // la \u00fanica salida de una errata era abandonar la navegaci\u00f3n.
    for (const tecla of [
      "Backspace",
      "Delete",
      "ArrowLeft",
      "ArrowRight",
      "Home",
      "End",
    ]) {
      expect(esParaElCampo(k(tecla), true)).toBe(true);
    }
  });

  it("y el pegado, que es como se contesta un di\u00e1logo de contrase\u00f1a", () => {
    expect(esParaElCampo(k("v", { ctrl: true }), true)).toBe(true);
    expect(esParaElCampo(k("c", { ctrl: true }), true)).toBe(true);
    expect(esParaElCampo(k("z", { ctrl: true }), true)).toBe(true);
  });

  it("pero NO los acordes del host", () => {
    // `ctrl+q` no es edici\u00f3n: si el campo se lo quedara, no habr\u00eda forma de
    // salir de la ventana con un di\u00e1logo delante.
    expect(esParaElCampo(k("q", { ctrl: true }), true)).toBe(false);
    expect(esParaElCampo(k("Enter"), true)).toBe(false);
    expect(esParaElCampo(k("Escape"), true)).toBe(false);
    expect(esParaElCampo(k("F5"), true)).toBe(false);
    expect(esParaElCampo(k("Tab"), true)).toBe(false);
  });

  it("y sin campo abierto no se queda nada", () => {
    expect(esParaElCampo(k("a"), false)).toBe(false);
    expect(esParaElCampo(k("Backspace"), false)).toBe(false);
  });
});

describe("Alt SOLO (puente 68)", () => {
  const t = (
    key: string,
    mods: Partial<{ ctrlKey: boolean; shiftKey: boolean; metaKey: boolean }> = {},
  ) => ({
    key,
    ctrlKey: false,
    shiftKey: false,
    metaKey: false,
    ...mods,
  });

  it("bajar y soltar Alt sin nada en medio es el gesto", () => {
    const a = new AltSolo();
    a.abajo(t("Alt"));
    expect(a.arriba(t("Alt"))).toBe(true);
  });

  it("Alt mantenido que se repite sigue siendo el gesto", () => {
    const a = new AltSolo();
    a.abajo(t("Alt"));
    a.abajo(t("Alt"));
    expect(a.arriba(t("Alt"))).toBe(true);
  });

  it("Alt+otra tecla NO lo es, aunque Alt se suelte el último", () => {
    const a = new AltSolo();
    a.abajo(t("Alt"));
    a.abajo(t("F4"));
    expect(a.arriba(t("F4"))).toBe(false);
    expect(a.arriba(t("Alt"))).toBe(false);
  });

  it("AltGraph no es Alt: escribe @ y # en un teclado español", () => {
    const a = new AltSolo();
    a.abajo(t("AltGraph"));
    expect(a.arriba(t("AltGraph"))).toBe(false);
  });

  it("con otro modificador bajado no se arma", () => {
    const a = new AltSolo();
    a.abajo(t("Alt", { ctrlKey: true }));
    expect(a.arriba(t("Alt"))).toBe(false);
  });

  it("un clic o perder el foco en medio lo desarma", () => {
    const a = new AltSolo();
    a.abajo(t("Alt"));
    a.soltar();
    expect(a.arriba(t("Alt"))).toBe(false);
  });

  it("un soltar suelto no dispara nada", () => {
    expect(new AltSolo().arriba(t("Alt"))).toBe(false);
  });
});
