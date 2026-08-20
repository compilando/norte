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
