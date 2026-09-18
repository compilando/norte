# Revisión de arquitectura de 2026-09-18 — plan

Tres trabajos, tres ramas, en este orden. Cada uno cierra con `just ci-fast`;
el último, con `just ci`.

La revisión pedía tres cosas genéricas: el patrón Command («una clase por
acción»), una estructura de carpetas «por dominio» y logging estructurado. Lo
que se hace aquí es lo que de eso le falta de verdad a norte. Lo que ya tiene
(comandos con nombre, ADR 0006; el journal como Command con undo; capas
hexagonales por crate con tests de frontera) no se toca.

## 1. `refactor/command-effects` — el catálogo dice qué hace un comando (ADR 0126)

**El problema.** Lo que un comando le hace al mundo estaba en dos listas que
no eran el catálogo: `norte-ui-host::commands::MUTAN` (qué no corre una
ventana de solo lectura) y `norte-frontend::menu::role` (qué se pinta como
destructivo o como IA). Olvidar `MUTAN` al añadir un comando que escribe es un
fallo ABIERTO y callado.

**Lo que se hace.**

1. `norte-frontend/src/keymap/catalogue.rs`: `enum Effect { Inert,
   ReadsContent, Launches, Writes, Destroys, SendsOut }` y
   `CommandDef.effect`. `live(name, counts, effect)` y `planned(..., effect)`:
   **sin valor por defecto**. `pub fn effect(name) -> Option<Effect>`.
2. Clasificación (idéntica en conjunto a `MUTAN`, que eran 24):
   - `Writes`: copy, move, mkdir, edit-new, rename, rename-batch, chmod,
     organize, sync-dirs, pack, unpack, split-file, combine-files.
   - `Destroys`: delete, delete-permanent.
   - `SendsOut`: ai-rename, semantic-search.
   - `ReadsContent`: checksum, checksum-verify.
   - `Launches`: open, edit, compare-files, app.terminal, app.handoff, y los
     dos que sólo tiene la TUI: pane.command-line, app.toggle-panels.
   - Todo lo demás, `Inert`.
3. Test en el catálogo: el conjunto de no-`Inert` es EXACTAMENTE ese (como
   `el_conjunto_con_contador_es_exactamente_este`).
4. `norte-ui-host/src/commands.rs`: fuera `MUTAN`; `implementados(SoloLectura)`
   filtra por `!effect.is_inert()`. Los tests que lo usaban se reescriben
   contra el efecto (misma aritmética: solo lectura quita exactamente lo que
   no es inerte).
5. `norte-frontend/src/menu.rs`: `role(id)` se deriva: `Destroys` →
   `Destructive`, `SendsOut` → `Ai`. Sus tests no cambian: pinan el mismo
   resultado.
6. `.claude/skills/new-command/SKILL.md`: la lista de sitios que toca un
   comando nuevo (proto → core → catálogo con efecto → los dos despachos →
   siete presets → i18n → ayuda → golden), que hoy es prosa en CLAUDE.md.

Revisor: `rust-reviewer`. Sin proto, sin puente (el `role` del puente sale
igual).

## 2. `feat/structured-logs` — logs en JSON y spans correlacionables (ADR 0127)

**Lo que hay.** `tracing` + `EnvFilter` + fichero rotado diario + anillo del
panel + cap de `suppaftp`. Formato sólo texto. El `dispatch` del daemon y el
scheduler tienen spans, pero la tarea corre en un `tokio::spawn` sin padre:
lo que registra no se puede atar a la petición que la pidió.

**Lo que se hace.**

1. `[log] format = "text" | "json"` (`LogSection.format`, `LogFormat`, por
   defecto `text`). `LogConfig.format`. Sólo cambia la capa de FICHERO: el
   stderr es para una persona y el anillo ya es estructurado (`LogLine`).
   JSON con `with_current_span` y `with_span_list`, para que cada línea lleve
   su cadena de spans.
2. Dependencia: feature `json` de `tracing-subscriber`. Trae `tracing-serde`
   (mismo repo tokio-rs/tracing, MIT) y `serde_json` (ya en el árbol). Se
   justifica en el ADR (regla 8).
3. Jerarquía de spans, fija:
   - `conn{conn_id}` — `serve_connection` en `daemon/server.rs`.
   - `rpc{method, req_id}` — alrededor de `dispatch`. `req_id` es el `id`
     JSON-RPC, que ya viaja: **no hace falta cambiar el protocolo** para
     correlacionar, porque `conn_id` + `req_id` es único y el cliente conoce
     su `id`.
   - `task{task_id, kind, provider}` — creado en `Scheduler::submit` (hijo del
     `rpc` que lo pidió) y **guardado en el `QueuedJob`**. El runner
     instrumenta el futuro con el span DEL JOB QUE SACA del heap, que no es
     necesariamente el que empujó; `tokio::spawn` a pelo perdía el contexto.
4. Test: un subscriber de captura comprueba que un evento emitido dentro de
   una tarea lleva `task_id` y el `method` del `rpc` padre; y que `format =
   "json"` produce líneas JSON válidas con `spans`.
5. Niveles, por escrito en el ADR y en el rustdoc de `logging.rs`:
   `error` = falló algo del usuario y no se recupera; `warn` = se degradó y
   siguió; `info` = ciclo de vida; `debug` = decisiones; `trace` = por
   entrada/bloque. No hay `fatal`: es un `error!` en el `main` de un binario
   seguido de la salida vía `anyhow`; una librería nunca mata el proceso.

Revisor: `rust-reviewer` + `security-reviewer` (el log puede llevar rutas: los
campos nuevos son ids y nombres de método, nunca rutas ni parámetros).

## 3. `refactor/frontend-folders` — `norte-frontend` por carpetas

**Lo que hay.** ~70 ficheros sueltos en `norte-frontend/src/`, junto a cinco
carpetas que ya agrupan (`keymap/`, `pane/`, `sync/`, `layout/`).

**Lo que se hace.** Mover, sin tocar lógica, y **re-exportar cada módulo en
su ruta antigua** (`pub use ops::chmod;`) para que ningún crate de fuera
cambie una línea. Grupos:

- `ops/`: chmod, checksums, organize, rename_pattern, compare, diffpair.
- `nav/`: goto, history, places, tree, watch (y `nav.rs` pasa a `nav/mod.rs`).
- `chrome/`: footer, keybar, menu, panelbar, splash, banners, frame.
- `overlays/`: palette, palette_state, modal, whichkey, wizard y los
  `*_picker`.
- `view/`: viewer, treemap, diskmap, columns, display, format.

Una sola rama y un commit por carpeta. Si un módulo usa `super::` para llegar
a un hermano de la raíz, el compilador lo dice; se corrige a `crate::`.

Sin revisor: son movimientos. El gate es la prueba.

## Cierre

ADR 0126 y 0127, memoria (`revision-arquitectura.md`), CHANGELOG, `just ci`.
