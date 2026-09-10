// El orden del documento ES el orden de pintado: esta ventana no usa
// `z-index` en ninguna parte, así que quien va después en `index.html` tapa
// al anterior. Estos tests fijan el orden que importa para el teclado: lo
// que se queda las teclas tiene que pintarse encima de lo que las pierde.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

// Por la raíz de vitest (el directorio `ui/`) y no por `import.meta.url`: bajo
// jsdom esa URL es `http://` y no se puede leer como fichero.
const html = readFileSync(resolve(process.cwd(), "index.html"), "utf8");

function posicion(id: string): number {
  const i = html.indexOf(`id="${id}"`);
  expect(i, `#${id} existe en index.html`).toBeGreaterThanOrEqual(0);
  return i;
}

describe("el orden del documento", () => {
  it("los diálogos van encima de los ajustes y de todos los selectores", () => {
    // Un diálogo se queda el teclado por encima de lo que hubiera. Declarado
    // antes que `#settings`, el prompt que pide el valor de un ajuste abría
    // DEBAJO de los ajustes: se veía el velo y ningún campo.
    for (const debajo of [
      "settings",
      "agents",
      "extensions",
      "theme",
      "picker",
      "profiles",
      "layouts",
      "columns",
      "search",
      "compare",
      "sync",
      "ai-rename",
    ]) {
      expect(posicion("dialogs"), `#dialogs después de #${debajo}`).toBeGreaterThan(
        posicion(debajo),
      );
    }
  });

  it("solo la ayuda y el aviso fatal van encima de los diálogos", () => {
    expect(posicion("help")).toBeGreaterThan(posicion("dialogs"));
    expect(posicion("fatal")).toBeGreaterThan(posicion("help"));
  });

  it("la salida de un plugin sigue debajo de los diálogos", () => {
    // Una confirmación que sigue teniendo el teclado no puede quedar tapada
    // por la salida de un comando cuyo momento elige el plugin.
    expect(posicion("plugin-output")).toBeLessThan(posicion("dialogs"));
    expect(posicion("program-output")).toBeLessThan(posicion("dialogs"));
  });
});
