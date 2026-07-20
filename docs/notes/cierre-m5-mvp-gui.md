# Cierre M5 hito 2 — MVP de la GUI (GPUI)

- Fecha: 2026-07-20
- Estado: **MVP GUI COMPLETO** (GUI-a..e). Rama `m5-cierre` (junto al cierre de deuda).

## Criterio de salida M5

«GUI y TUI sobre la misma sesión simultáneamente» — demostrado desde el spike
(hito 1, ADR 0027) y consolidado: la GUI es un cliente más del daemon
(`RemoteBackend`, regla 7/9), coexiste con la TUI sobre la misma sesión.

## Sub-proyectos (todos IMPLEMENTADOS + mergeados a main salvo `m5-cierre` pendiente)

- **GUI-a** — navegación dual-pane read-only + crate compartido `norte-frontend`
  (display/sort/QuickSearch/PaneState).
- **GUI-b** — mutaciones: copy/move/delete con marcas, backend de sesión
  persistente, modales, franja de tasks, cancelación, conflicto (cola simple).
- **GUI-c** — keymap configurable: motor extraído a `norte-frontend::keymap` con
  tecla NEUTRA (sin crossterm); adaptadores por frontend; preset + capas.
- **GUI-d** — viewer F3: `Viewer` core extraído a `norte-frontend::viewer` (sin
  i18n); texto/hexview/encoding; previewer de plugin; contexto Viewer del keymap.
- **GUI-e** — i18n (strings → Fluent, unifica `viewer_status` con la TUI) +
  AccessKit foundational (roles/labels saneados en el árbol de a11y).

## Cierre de deuda (rama `m5-cierre`)

- **#86** fmt drift → `rustfmt.toml` (style_edition=2024) determinista. Bump a
  edición 2024 completo diferido (#90, migración RPIT de #87).
- **#88** charset lua duplicado → `valid_lua_name` pub en `norte-frontend`, la
  TUI delega.
- **#83/#84/#85** deuda GUI → `task.dismiss` (ctrl+l), coalesce de relist
  (correcto, re-relista al aterrizar), `affected_dirs`/`first_cancelable` puros
  con test. Nav de franja diferida (#91).
- **#87** lag (cerrado antes): `uniform_list` virtualiza (O(visible)).

## Seguridad/encoding a lo largo del MVP

- Nombres = bytes, display SIEMPRE saneado (`display_name`/`path_display`) en
  TODA superficie: filas, modales, viewer (incl. Trojan Source CVE-2021-42574
  cerrado en `render_line`), task strip, y AHORA los labels a11y (un lector de
  pantalla jamás lee bidi/controles crudos).
- La GUI conecta como humano (`User`, allow-all); todo por el daemon (regla 9).

## Deuda viva

- **#89** AccessKit: verificación AT-SPI/lector real (Linux) pendiente.
- **#90** edición 2024 completa (migración RPIT).
- **#91** navegación de la franja de tasks.
- Viewer: preview de imágenes/media (el previewer es texto), búsqueda en el visor.
- Hot-reload del keymap; `Resolution::Pending` sin indicador visual; unificar
  `viewer_status` GUI↔TUI del todo cuando la GUI tenga selección de locale por
  config (hoy solo env).
- #82 (unificar el `Pane` de la TUI sobre `PaneState`).

## Siguiente

M5 hito 2 cerrado. Verificación GUI interactiva end-to-end (incl. AT-SPI)
pendiente de oscar. Luego: lo que decida el roadmap (M5 hito 3 si lo hay, o
volver a M3-5 audit export / otros milestones del core).
