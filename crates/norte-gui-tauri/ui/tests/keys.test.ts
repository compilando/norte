import { describe, expect, it } from "vitest";

import { AltSolo, isForTheField, keyInputOf } from "../src/keys";

function ev(init: KeyboardEventInit): KeyboardEvent {
  return new KeyboardEvent("keydown", init);
}

describe("keys", () => {
  it("travel with the browser's name and its modifiers", () => {
    expect(keyInputOf(ev({ key: "ArrowDown" }))).toEqual({
      key: "ArrowDown",
      ctrl: false,
      alt: false,
      shift: false,
      meta: false,
    });
    expect(keyInputOf(ev({ key: "F5", ctrlKey: true, shiftKey: true }))?.ctrl).toBe(true);
  });

  it("a modifier ALONE is not a key", () => {
    for (const key of ["Shift", "Control", "Alt", "Meta"]) {
      expect(keyInputOf(ev({ key }))).toBeNull();
    }
  });

  it("mid-IME-composition sends nothing", () => {
    expect(keyInputOf(ev({ key: "a", isComposing: true }))).toBeNull();
  });

  it("resolves NOTHING: sends the key, not the command", () => {
    // If this stopped being true there would be two keymaps (decision D14).
    const k = keyInputOf(ev({ key: "q", ctrlKey: true }));
    expect(k).toEqual({ key: "q", ctrl: true, alt: false, shift: false, meta: false });
  });
});

describe("a TEXT key", () => {
  it("is measured in code points, not UTF-16 units", () => {
    // An emoji is two UTF-16 units and ONE code point: measured with
    // `length`, the text field wouldn't receive it and it couldn't be typed
    // into a name.
    const emoji = keyInputOf(ev({ key: "😀" }));
    expect(emoji).not.toBeNull();
    expect([...(emoji?.key ?? "")].length).toBe(1);
    expect((emoji?.key ?? "").length).toBe(2);
  });

  it("an é in NFD is TWO code points: it's not a text key", () => {
    const nfd = keyInputOf(ev({ key: "é" }));
    expect(nfd).not.toBeNull();
    expect([...(nfd?.key ?? "")].length).toBe(2);
  });
});

describe("an open text field", () => {
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

  it("keeps the keys that TYPE", () => {
    expect(isForTheField(k("a"), true)).toBe(true);
    expect(isForTheField(k("\u{1F600}"), true)).toBe(true);
  });

  it("and the ones that EDIT, which is the missing half", () => {
    // Without this, `preventDefault` cancelled the field's own deletion and
    // the host swallowed it: a half-typed name couldn't be corrected, and in
    // a password field (#327) — forty characters, without seeing them — the
    // only way out of a typo was abandoning navigation.
    for (const key of ["Backspace", "Delete", "ArrowLeft", "ArrowRight", "Home", "End"]) {
      expect(isForTheField(k(key), true)).toBe(true);
    }
  });

  it("and paste, which is how a password dialog gets answered", () => {
    expect(isForTheField(k("v", { ctrl: true }), true)).toBe(true);
    expect(isForTheField(k("c", { ctrl: true }), true)).toBe(true);
    expect(isForTheField(k("z", { ctrl: true }), true)).toBe(true);
  });

  it("but NOT the host's chords", () => {
    // `ctrl+q` is not editing: if the field kept it, there would be no way
    // to leave the window with a dialog in front.
    expect(isForTheField(k("q", { ctrl: true }), true)).toBe(false);
    expect(isForTheField(k("Enter"), true)).toBe(false);
    expect(isForTheField(k("Escape"), true)).toBe(false);
    expect(isForTheField(k("F5"), true)).toBe(false);
    expect(isForTheField(k("Tab"), true)).toBe(false);
  });

  it("and with no field open, nothing is kept", () => {
    expect(isForTheField(k("a"), false)).toBe(false);
    expect(isForTheField(k("Backspace"), false)).toBe(false);
  });
});

describe("Alt ALONE (bridge 68)", () => {
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

  it("pressing and releasing Alt with nothing in between is the gesture", () => {
    const a = new AltSolo();
    a.down(t("Alt"));
    expect(a.up(t("Alt"))).toBe(true);
  });

  it("Alt held and repeating is still the gesture", () => {
    const a = new AltSolo();
    a.down(t("Alt"));
    a.down(t("Alt"));
    expect(a.up(t("Alt"))).toBe(true);
  });

  it("Alt+another key is NOT it, even if Alt is released last", () => {
    const a = new AltSolo();
    a.down(t("Alt"));
    a.down(t("F4"));
    expect(a.up(t("F4"))).toBe(false);
    expect(a.up(t("Alt"))).toBe(false);
  });

  it("AltGraph is not Alt: it types @ and # on a Spanish keyboard", () => {
    const a = new AltSolo();
    a.down(t("AltGraph"));
    expect(a.up(t("AltGraph"))).toBe(false);
  });

  it("with another modifier held down it doesn't arm", () => {
    const a = new AltSolo();
    a.down(t("Alt", { ctrlKey: true }));
    expect(a.up(t("Alt"))).toBe(false);
  });

  it("a click or losing focus in between disarms it", () => {
    const a = new AltSolo();
    a.down(t("Alt"));
    a.release();
    expect(a.up(t("Alt"))).toBe(false);
  });

  it("a release with no prior press fires nothing", () => {
    expect(new AltSolo().up(t("Alt"))).toBe(false);
  });
});
