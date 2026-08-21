// El corpus golden del host, leído TAL CUAL desde el árbol de Rust.
//
// Es el mismo fichero que clava el lado Rust (`crates/norte-ui-host/tests/
// golden/*.json`). Que las dos partes lean el MISMO corpus es lo único que
// impide que los tipos de `src/types.ts` se separen del contrato sin que nada
// se ponga rojo: una copia aquí sería una copia que se queda vieja.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const GOLDEN = resolve(process.cwd(), "../../norte-ui-host/tests/golden");

export function golden(name: string): Record<string, unknown> {
  return JSON.parse(readFileSync(resolve(GOLDEN, name), "utf8")) as Record<
    string,
    unknown
  >;
}

const I18N = resolve(process.cwd(), "../../norte-i18n/i18n");

/**
 * El catálogo de verdad, leído del mismo `.ftl` que usa el host.
 *
 * Los tests montaban un catálogo de DOS claves inventadas, así que ninguno
 * podía notar que faltara una: `t` contesta la clave ausente con la clave, y
 * con un fixture inventado eso es indistinguible de lo normal. Ocho
 * superficies pintaron `hostile-name` literal por esto.
 *
 * El parser es de una línea: `clave = valor`. Fluent tiene más gramática
 * —atributos, selectores, continuaciones— y aquí no hace falta ninguna: lo
 * que se necesita es saber qué claves EXISTEN y con qué texto simple.
 */
export function catalogoReal(lang: "es" | "en" = "es"): Record<string, string> {
  const ftl = readFileSync(resolve(I18N, `${lang}.ftl`), "utf8");
  const out: Record<string, string> = {};
  for (const linea of ftl.split("\n")) {
    const m = /^([a-z][a-z0-9-]*) = (.*)$/.exec(linea);
    if (m?.[1] !== undefined && m[2] !== undefined) {
      out[m[1]] = m[2];
    }
  }
  return out;
}
