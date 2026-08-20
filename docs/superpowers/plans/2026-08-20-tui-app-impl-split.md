# Repartir `norte-tui/src/app.rs` — lo hecho

> **Estado:** COMPLETO. `app.rs`: **7.377 → 991 líneas** (904 de producción,
> 87 de test). Trece módulos nuevos bajo `app/`, ninguno por encima de 1.017
> líneas de producción. 15 commits en `refactor/tui-app-impl`, sin pushear.
> `just ci-fast` verde: **5.069 tests**, y los 892 de `norte-tui` son los
> mismos de antes y después, uno a uno.
>
> **Fecha:** 2026-08-20.

## Por qué existe esto

La ronda anterior (`2026-08-19-tui-main-extraction.md`) dejó dicho por qué NO
tocaba `app.rs`: `App` tiene 63 campos y su `impl` eran 3.577 líneas con 187
métodos, **todos pequeños** (el mayor, 71 líneas). El problema es la ANCHURA,
no la profundidad, y una anchura se reparte; no se rediseña. La rama
`refactor/tui-names` ya se había llevado los siete tipos autónomos
(`Modal`, `Pane`, `Trail`, `HelpView`, `NavPopup`, la paleta, los plugins);
lo que quedaba era el `impl App` entero y un `mod tests` de 2.553 líneas.

## Lo hecho

Un `impl App` puede vivir en un módulo hijo: la privacidad de Rust alcanza a
los descendientes, así que `app/layout.rs` ve los campos privados de `App` sin
abrir nada. Doce rebanadas temáticas, una por commit, verificadas por
compilación (el movimiento no puede cambiar comportamiento si compila y los
tests no se tocan):

| módulo | prod | qué |
| --- | --- | --- |
| `prompts.rs` | 1.017 | las nueve familias `open_*`/`*_push`/`*_pop`/`cancel_*`/`*_confirm`/`*_submitted`/`*_set_error` |
| `layout.rs` | 698 | `set_layout`, los `toggle_*` de places/preview/tree/procesos/metadatos, resize y foco de hueco |
| `dialogs.rs` | 388 | los quince `ALLOW_*`, `dialog_action`, `help_action`, `trust_lua_key` |
| `ops.rs` | 298 | colisiones y aprobaciones en cola, copiar/mover/borrar, orden, propiedades |
| `compare.rs` | 293 | `request_compare`, `request_sync` y la sonda de tamaño (#157) |
| `caps.rs` | 230 | catálogos de atributos, caché de `Capabilities`, solo-lectura, hechos de la ayuda |
| `session.rs` | 222 | la sesión de UI (L2, ADR 0059) |
| `nav.rs` | 198 | historial, hotlist y volúmenes |
| `pickers.rs` | 194 | tema, disposición, conexiones y columnas |
| `focus.rs` | 193 | foco, intercambio, ventana de `stat` y pestañas |
| `banners.rs` | 180 | degradación, journal y sesión |
| `errors.rs` | 137 | la presentación de error de la barra (#73) |
| `testutil.rs` | 131 | los trece constructores que comparten los tests |

Y los tests se fueron con su código: 82 de los 91 del `mod tests` de `app.rs`
están ahora en el módulo que prueban. En `app.rs` quedan los cinco del diálogo
de búsqueda, que es lo único de comportamiento que sigue viviendo allí.

## Lo que hizo falta y no era mover líneas

- **`app/testutil.rs`.** Trece helpers (`root`, `file`, `pane_con`,
  `app_dos_panes`, `app_en`…) los usan los tests de siete módulos hermanos. La
  alternativa era copiarlos trece veces; el módulo va `#[cfg(test)]` y
  `pub(crate)`.
- **Tres métodos privados pasan a `pub(super)`** (`open_transfer_name_with`,
  `mint_slot`, `browsers_in_tree`): su llamante se quedó al otro lado del
  corte. Es la única frontera que el reparto movió de verdad.
- **`cargo doc` cazó lo que el bucle no ve.** Cuatro `[`Tipo`]` resolvían en
  `app.rs` y no en el hijo, y el doc de `SyncRoots` se quedó huérfano al
  mudarse su `use`. Ni `just t` (nextest, sin doctests) ni `just c` (clippy,
  sin enlaces intra-doc) dicen nada: exactamente la familia de puntos ciegos
  que CLAUDE.md ya tenía escrita.

## Lo que este reparto NO hace

- **No unifica las nueve familias de prompts.** Ahora están juntas en un
  fichero, que es la condición previa para ver si comparten forma suficiente
  para un genérico o una macro. Eso es rediseño y quiere su propia ronda.
- **No parte `run`** (`event_loop.rs`, 2.735 líneas). Sigue siendo lo que decía
  la ronda anterior: convertir la cadena `else if` de 19 ramas en una tabla de
  precedencia de overlays cambia el orden de los `await` dentro de un
  `tokio::select!` de 23 brazos.
- **No toca `norte-gui`**, marcado para borrado en la Fase 8.3 del plan Tauri.
