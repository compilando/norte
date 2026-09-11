// La tipografía: la empaquetada es el respaldo y `[ui]` sigue mandando.
//
// Con las fuentes dentro del bundle (V1) la tentación es que la hoja de
// estilos las nombre a pelo. Lo que este fichero comprueba es que la
// configuración sigue por encima: `font`, `mono_font` y `font_size` acaban
// en las variables que `style.css` lee, y el tamaño arrastra la rejilla.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

import { FILA_POR_TAMANO, applyAppearance, themeFor } from "../src/main";
import type { HostCatalog } from "../src/types";

const CSS = readFileSync(resolve(__dirname, "../src/style.css"), "utf8");

describe("la tipografía empaquetada y la configuración", () => {
  it("la hoja de estilos respalda con las fuentes empaquetadas y deja mandar a [ui]", () => {
    // Las dos pilas nombran la empaquetada DETRÁS de la variable que la
    // configuración escribe; una hoja que pusiera "JetBrains Mono" a pelo en
    // `body` dejaría `[ui] mono_font` sin efecto.
    expect(CSS).toMatch(/--font-mono:\s*var\(\s*--mono,\s*"JetBrains Mono"/);
    expect(CSS).toMatch(/--font-ui:\s*var\(\s*--ui-font,\s*"Inter"/);
    // Y el cuerpo lee la pila, no la fuente.
    expect(CSS).toMatch(/body\s*\{[^}]*font-family:\s*var\(--font-mono\)/);
    // Numerales tabulares y sin ligaduras en el listado.
    expect(CSS).toMatch(/body\s*\{[^}]*font-variant-numeric:\s*tabular-nums/);
    expect(CSS).toMatch(/body\s*\{[^}]*font-variant-ligatures:\s*none/);
  });

  it("[ui] font, mono_font y font_size llegan al raíz, y el tamaño mueve la rejilla", () => {
    const raiz = document.documentElement;
    applyAppearance(document, {
      font: "Fira Sans",
      mono_font: "Fira Code",
      font_size: 16,
      reduce_motion: null,
    });
    expect(raiz.style.getPropertyValue("--ui-font")).toBe("Fira Sans");
    expect(raiz.style.getPropertyValue("--mono")).toBe("Fira Code");
    expect(raiz.style.getPropertyValue("--ui-font-size")).toBe("16px");
    expect(raiz.style.getPropertyValue("--cell-h")).toBe(
      `${String(Math.round(16 * FILA_POR_TAMANO))}px`,
    );
  });

  it("un campo null no toca lo que había", () => {
    const raiz = document.documentElement;
    raiz.style.setProperty("--mono", "Fira Code");
    raiz.style.setProperty("--cell-h", "22px");
    applyAppearance(document, {
      font: null,
      mono_font: null,
      font_size: null,
      reduce_motion: null,
    });
    expect(raiz.style.getPropertyValue("--mono")).toBe("Fira Code");
    expect(raiz.style.getPropertyValue("--cell-h")).toBe("22px");
  });

  it("el tema por esquema: la variante si la hay, y `theme` si no", () => {
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
    // Sin variante clara, `theme`.
    expect(themeFor(base, false)).toEqual({ fg: "#111111" });
    const soloBase: HostCatalog = {
      bridge_version: 0,
      instance_id: "i",
      locale: "es",
      strings: {},
      theme: { fg: "#111111" },
      measure: false,
    };
    expect(themeFor(soloBase, true)).toEqual({ fg: "#111111" });
  });

  it("la fila por defecto es 22 px para 14 px de letra", () => {
    expect(Math.round(14 * FILA_POR_TAMANO)).toBe(22);
    expect(CSS).toMatch(/--cell-h:\s*22px/);
    expect(CSS).toMatch(/font-size:\s*var\(--ui-font-size,\s*14px\)/);
  });
});
