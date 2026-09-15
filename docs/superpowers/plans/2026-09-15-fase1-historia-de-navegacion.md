# Fase 1 — Historia de navegación completa (plan)

Spec: `docs/superpowers/specs/2026-09-15-historia-y-wow-design.md`, sección
«Fase 1». Rama `feat/historia-de-navegacion`. Sin cambio de wire (la sesión
es cuerpo de frontend; campos aditivos con `#[serde(default)]`). Puente de la
ventana SÍ sube (fila del selector con marca).

Gate: `just t <crate>` en el bucle; `just ci-fast` tras T4 y tras T6; `just ci`
una vez al cerrar (recetas una a una en primer plano; + `gui-ci` por el
puente).

## T1 — `norte-frontend::nav`: cota, punto de salto, populares, filas

- `History` lleva `cap: usize` (defecto `HISTORY_DEFAULT = 30`, techo
  `session::HISTORY_CAP = 64`). `History::with_capacity(cap)` y
  `set_capacity(cap)`: acota `cap` a `5..=64`, recorta MRU y `back` por el
  extremo viejo y, si `back+fwd > cap`, `fwd` por el extremo LEJANO (índice 0
  es lo más lejano del lector: `fwd` es «más viejo primero»… verificar el
  orden con `step_forward` = `pop` ⇒ el último es el más cercano; se recorta
  desde el principio). El invariante pasa a `back.len()+fwd.len() <= cap`.
- `History.jump: Option<VPath>` + `set_jump(VPath)`, `jump() -> Option<&VPath>`.
  `remove(path)` también limpia `jump` si coincide.
- `Popular { entries: Vec<PopularEntry{path, visits: u32, last: u64}> }`,
  `POPULAR_CAP = 50`. `visit(path, now_seq)`: suma o inserta; lleno → expulsa
  el de menos visitas y, en empate, el `last` más bajo. `remove`, `clear`,
  `ranked()` (visitas desc, `last` desc). `last` es un contador monótono de la
  propia estructura, no un reloj (determinista en tests).
- `HistoryMark { Current, Back, Forward }`; `history_rows(&History, current:
  &VPath, filter: &str) -> Vec<HistoryRow{path, mark}>`: primera fila el
  actual (`Current`), luego el MRU; una fila cuya ruta está en `forward_trail`
  lleva `Forward`. Filtro por subsecuencia sin mayúsculas sobre el display de
  la ruta (reusar la función de `palette_state`, hacerla `pub(crate)` si hace
  falta). El actual NO se filtra fuera (es la ancla visual) salvo que el filtro
  no lo case — se filtra igual que las demás, para no mentir.
- `popular_rows(&Popular, filter) -> Vec<HistoryRow{path, mark: Back}>`.
- `record_visit(history, popular, prev, dir, trail)` en `nav`: la ÚNICA
  decisión compartida de «esto cuenta» (`prev != dir && trail == Record`);
  la TUI (`navigate::record_step`) y el host (`navegar_hueco`) pasan a
  llamarla. Populares suman `dir` (a donde se llega), el rastro guarda `prev`.
- Tests: property test (proptest ya en dev-deps de norte-frontend: verificar)
  sobre secuencias `record/step_back/step_forward/remove/set_capacity` que
  afirma el invariante; `set_capacity` a la baja conserva lo más cercano;
  jump; expulsión de populares; `record_visit` con `Replay`/`Seed` no suma
  (verificación por mutación: borrar la guarda y ver rojo); `history_rows`
  marca `Current`/`Forward`; filtro.

## T2 — Sesión y configuración

- `SlotState.jump: Option<VPath>` (`#[serde(default, skip_serializing_if =
  "Option::is_none")]`), `SessionBody.popular: Vec<PopularEntry>` (default).
  `prune`: `popular` a `POPULAR_CAP`; `fit_to_envelope` tira `popular` ANTES
  que el historial de huecos visibles (menos doloroso). Test: redondeo, cuerpo
  viejo sin campos carga, recorte por bytes.
- `[ui] history_size`: `schema.rs` (`Option<u32>`, doc), `UiChrome.history_size`
  + `history_size()` (defecto 30, rechazo fuera de `5..=64` en `load` con ruta
  en el diagnóstico, patrón de `notice_seconds`), merge. `settings.rs`:
  `ui.history-size` `Int{5,64}`, `applies_live: true`, lectura en el
  `match` de valores (línea ~426). Fluent `settings-ui-history-size-name/-desc`
  en/es. Golden del schema (`NORTE_UPDATE_SCHEMA=1`, lo último).

## T3 — Vocabulario

Siete comandos nuevos, todos `live(.., false)`:
`nav.jump-back`, `nav.set-jump-point`, `pane.popular`, `pane.history-left`,
`pane.history-right`, `dialog.confirm-other`, `dialog.clear`.

Por cada uno (checklist de `krusader-ctrl-flechas`): catálogo; TUI
`keymap.rs` (enum + nombre + brazo); ui-host `commands.rs` (lista + `Efecto`
+ brazo); `help-cmd-*` en/es; `menu-item-*` en/es y entrada en el menú **Ir**
(`menu.rs`) para los cinco de navegación; `DOCUMENTED` en
`norte-help/tests/corpus.rs`. Mensajes: `msg-nav-no-jump-point`,
`msg-nav-jump-point-set`, `popular-title`, `popular-empty`,
`history-title-left/-right`, `history-mark-current/-forward`,
`history-clear-confirm`. Los dos `dialog.*` entran en `ALLOW_NAV_POPUP` (TUI)
y en la allowlist del selector (host).

## T4 — Presets (los siete) y cabeceras

Según la tabla D1 de la spec. Cabeceras: krusader (corregir la frase de
«per-panel bookmark menus», atar alt+left/right, ctrl+alt+left/right, ctrl+j,
ctrl+z; `Ctrl+Shift+J` no entregable), total-commander (`Alt+Shift+Down` =
misma lista, jump/popular sin fuente), far (jump/popular sin fuente; las
teclas de su lista son las que norte usa en el diálogo), norton (sin fuente
primaria de historia), orthodox (`alt+y`, `alt+H`; `alt+u` sigue siendo
`pane.pull`), vim (`H`/`L`, ranger/yazi), cua (sin nuevos; motivos). Diálogo
en orthodox: `ctrl+enter` → `dialog.confirm-other`, `delete` →
`dialog.remove`, `shift+delete` → `dialog.clear` (verificar que no chocan en
`[dialog]`). Tests de preset + hoja de referencia; golden `norte-cli`
(`NORTE_UPDATE_GOLDEN=1`).

→ `just ci-fast`.

## T5 — TUI

- `Histories` guarda la cota y la aplica en `for_slot_mut`; `reload_config`
  la re-aplica.
- `NavPopupKind::{History{side: Option<Side>}, Popular}`; `open_nav_popup`
  construye con `history_rows`/`popular_rows`; filas con marca pintada
  (`●` actual, `→` adelante). Filtro con `/` (ya en el diálogo).
- `side_nav.rs`: `dialog.confirm-other` (navega el OTRO panel con
  `Trail::Record`, foco quieto), `dialog.remove` (historia: `History::remove`;
  populares: `Popular::remove`), `dialog.clear` (modal de confirmación →
  vaciar), `dialog.add` en historia/populares → abrir el input de favorito con
  la ruta.
- `dispatch.rs`: `nav.jump-back`, `nav.set-jump-point`, `pane.popular`,
  `pane.history-left/-right` (lado resuelto al abrir, como volúmenes).
- `record_step` → `nav::record_visit`; `App.popular: Popular`.
- Sesión: captura/aplica `jump` y `popular`.
- Tests en `app/nav.rs` y `screens/side_nav.rs` con el backend falso.

## T6 — Ventana (ui-host + renderer)

- `Hueco.historial` con la cota de config; `navegar_hueco` → `record_visit`;
  `Controller.popular`.
- `Selector::historial(slot, rows)` y `Selector::populares(slot, rows)` desde
  las filas compartidas; `PickerRowView.mark: Option<String>` (`current` |
  `forward`) → **sube `BRIDGE_VERSION`** y el hash de forma del corpus
  (bendecir en dos pasadas, ver memoria `columna-de-iconos-0105`).
- Verbos del selector: `dialog.confirm-other`, `dialog.remove`/`clear`/`add`
  cuando el selector es de historia o populares (`es_hotlist` pasa a un
  `SelectorKind` pequeño: `Hotlist | Historia | Populares | Otro`).
- `Efecto` para los cinco comandos de navegación nuevos.
- Sesión: `jump`, `popular`.
- Renderer: `mouseup` con `button === 3/4` sobre la ventana → acción
  `History{back}` del hueco activo; pinta `mark`. Test vitest.

→ `just ci-fast` + `just gui-ci`.

## T7 — Paridad, ayuda, ADR, cierre

- Test de paridad: mismo estado → mismas filas y mismo destino en TUI y
  ventana para confirm / confirm-other / remove / jump.
- Topic `history` en/es (front-matter `commands` con los siete nuevos + los
  cuatro viejos), `panes.md` enlaza; golden CLI.
- ADR 0114 «La historia de un panel se recorre, se lista, se marca y se
  cuenta» (Krusader fuente vs docs, por qué no pins, por qué populares es
  global, techo 64).
- CHANGELOG `[Unreleased]`, memoria (`gestos-de-panel` + nueva), `just link`.
- Revisores: `rust-reviewer` (cota e invariante, `record_visit`),
  `encoding-auditor` (filas de ruta en listas, filtro sobre bytes).
- `just ci` (lint, test, docs, cov) + `gui-ci`.
