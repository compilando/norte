// The typography: the bundled one is the fallback, and `[ui]` still wins.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

import {
  FILA_POR_TAMANO,
  applyAppearance,
  applyTheme,
  showFatal,
  themeFor,
} from "../src/main";
import type { HostCatalog } from "../src/types";

const CSS = readFileSync(resolve(__dirname, "../src/style.css"), "utf8");

describe("bundled typography and configuration", () => {
  it("the stylesheet backs the bundled fonts and lets [ui] win", () => {
    // Both stacks name the bundled font BEHIND the variable the
    // configuration writes; a sheet that hardcoded "JetBrains Mono" on
    // `body` would leave `[ui] mono_font` without effect.
    expect(CSS).toMatch(/--font-mono:\s*var\(\s*--mono,\s*"JetBrains Mono"/);
    expect(CSS).toMatch(/--font-ui:\s*var\(\s*--ui-font,\s*"Inter"/);
    // And the body reads the stack, not the font.
    expect(CSS).toMatch(/body\s*\{[^}]*font-family:\s*var\(--font-mono\)/);
    // Tabular numerals and no ligatures in the listing.
    expect(CSS).toMatch(/body\s*\{[^}]*font-variant-numeric:\s*tabular-nums/);
    expect(CSS).toMatch(/body\s*\{[^}]*font-variant-ligatures:\s*none/);
  });

  it("[ui] font, mono_font and font_size reach the root, and the size drives the grid", () => {
    const root = document.documentElement;
    applyAppearance(document, {
      font: "Fira Sans",
      mono_font: "Fira Code",
      font_size: 16,
      reduce_motion: null,
    });
    expect(root.style.getPropertyValue("--ui-font")).toBe("Fira Sans");
    expect(root.style.getPropertyValue("--mono")).toBe("Fira Code");
    expect(root.style.getPropertyValue("--ui-font-size")).toBe("16px");
    expect(root.style.getPropertyValue("--cell-h")).toBe(
      `${String(Math.round(16 * FILA_POR_TAMANO))}px`,
    );
  });

  it("[ui] titlebar = custom marks the root, and the native one clears it (ADR 0136)", () => {
    const root = document.documentElement;
    const base = { font: null, mono_font: null, font_size: null, reduce_motion: null };
    applyAppearance(document, { ...base, custom_titlebar: true });
    expect(root.dataset["titlebar"]).toBe("custom");
    applyAppearance(document, base);
    expect(root.dataset["titlebar"]).toBeUndefined();
  });

  it("the fatal error carries its own title bar if the window has no desktop one", () => {
    const root = document.documentElement;
    const fatal = document.createElement("div");
    const requests: string[] = [];
    const win = { t: (k: string) => k, pedir: (v: string) => requests.push(v) };
    root.dataset["titlebar"] = "custom";
    showFatal(fatal, "the daemon went away", win);
    const close = fatal.querySelector('[data-verb="close"]') as HTMLButtonElement;
    expect(close).not.toBeNull();
    close.click();
    expect(requests).toEqual(["close"]);
    expect(fatal.textContent).toContain("the daemon went away");
    // With the native one, the desktop already closes it: nothing to add.
    delete root.dataset["titlebar"];
    showFatal(fatal, "again", win);
    expect(fatal.querySelector(".window-controls")).toBeNull();
    expect(fatal.textContent).toBe("again");
  });

  it("a null field doesn't touch what was already there", () => {
    const root = document.documentElement;
    root.style.setProperty("--mono", "Fira Code");
    root.style.setProperty("--cell-h", "22px");
    applyAppearance(document, {
      font: null,
      mono_font: null,
      font_size: null,
      reduce_motion: null,
    });
    expect(root.style.getPropertyValue("--mono")).toBe("Fira Code");
    expect(root.style.getPropertyValue("--cell-h")).toBe("22px");
  });

  it("switching theme CLEARS what the previous one left", () => {
    // The host sends only what the theme SAYS: since the chrome roles (spec
    // 2026-09-11, F2) a theme that doesn't define `hover` doesn't send
    // `--hover`, and the sheet derives it with
    // `var(--hover, var(--panel-focus-bg))` — a derivation that ONLY acts
    // while the variable is unset.
    //
    // Without the clearing, going from a theme that does define the chrome to
    // one that doesn't left half the window with the previous palette: the
    // command palette and menus dark over a light theme, with no way to fix
    // it short of restarting. This couldn't happen before because every
    // projected name was a role every preset defines.
    const root = document.documentElement;
    applyTheme(document, { bg: "#1f1f1f", hover: "#2a2d2e", "widget-bg": "#202020" });
    expect(root.style.getPropertyValue("--hover")).toBe("#2a2d2e");

    // A theme that stays silent on the chrome.
    applyTheme(document, { bg: "#fbf1c7" });
    expect(root.style.getPropertyValue("--bg")).toBe("#fbf1c7");
    expect(root.style.getPropertyValue("--hover")).toBe("");
    expect(root.style.getPropertyValue("--widget-bg")).toBe("");
  });

  it("the theme by scheme: the variant if there is one, and `theme` if not", () => {
    const base: HostCatalog = {
      bridge_version: 0,
      instance_id: "i",
      locale: "es",
      strings: {},
      theme: { fg: "#111111" },
      measure: false,
      theme_dark: { fg: "#eeeeee" },
      theme_light: null,
    };
    expect(themeFor(base, true)).toEqual({ fg: "#eeeeee" });
    // With no light variant, `theme`.
    expect(themeFor(base, false)).toEqual({ fg: "#111111" });
    const baseOnly: HostCatalog = {
      bridge_version: 0,
      instance_id: "i",
      locale: "es",
      strings: {},
      theme: { fg: "#111111" },
      measure: false,
    };
    expect(themeFor(baseOnly, true)).toEqual({ fg: "#111111" });
  });

  it("the default row is 22 px for 14 px of text", () => {
    expect(Math.round(14 * FILA_POR_TAMANO)).toBe(22);
    expect(CSS).toMatch(/--cell-h:\s*22px/);
    expect(CSS).toMatch(/font-size:\s*var\(--ui-font-size,\s*14px\)/);
  });
});
