# 0006 — Semántica de resolución del keymap engine

- Estado: accepted
- Fecha: 2026-07-11
- Decisores: oscar (dirección — preset default mc/orthodox, decisión
  2026-07-10), Claude (propuesta técnica)
- Relacionado: spec §12 (líneas "Keybindings"), plan M1 fase 4.

## Contexto y problema

La spec fija el modelo: mapa `(contexto, secuencia) → comando nombrado`,
secuencias multi-tecla estilo vim (`g g`), contextos jerárquicos
(`global > pane > dialog > …`), presets de fábrica (orthodox/vim/cua) y
`keymap.toml` con `prepend_keymap`/`append_keymap` (modelo Yazi). Exige
además resolución "determinista" y un proptest "ninguna secuencia ambigua"
(spec §12). Queda por decidir la SEMÁNTICA exacta: qué pasa con prefijos
compartidos, cuándo se resuelve una secuencia pendiente, y dónde vive el
engine.

## Opciones consideradas

### A. Ambigüedad: timeout vs prohibición

- **A1 — timeout estilo vim** (`g` suelto ejecuta tras N ms si también
  existe `g g`): permite keymaps densos, pero introduce no-determinismo
  temporal (la misma pulsación hace cosas distintas según latencia del
  usuario/terminal) y hace imposible el proptest de la spec tal cual.
- **A2 — prefix-free obligatorio**: dentro del keymap EFECTIVO, ninguna
  secuencia ligada puede ser prefijo estricto de otra. El conflicto se
  detecta AL CARGAR (error con diagnóstico, no warning silencioso) y la
  resolución queda 100% determinista sin relojes: cada tecla o extiende un
  prefijo válido, o ejecuta un comando, o resetea.

### B. Fusión de contextos: en caliente vs al activar

- **B1 — buscar en cada contexto al pulsar** (walk específico→general por
  tecla): flexible, pero la ambigüedad cruzada entre contextos solo aflora
  en runtime — imposible validarla al cargar.
- **B2 — keymap EFECTIVO precomputado por stack de contextos**: al activar
  un stack (p. ej. `[pane, global]`) se fusionan los mapas — el contexto
  más específico PISA por secuencia exacta; después de fusionar se valida
  prefix-free (A2). Capas de usuario: `prepend_keymap` pisa al preset,
  `append_keymap` solo añade si la secuencia no existe (modelo Yazi
  literal).

### C. Ubicación del engine

- **C1 — crate propio `norte-keymap`**: reutilizable por el GUI (M4+),
  pero hoy solo hay un consumidor y cada crate nuevo cuesta mantenimiento.
- **C2 — módulo `keymap` dentro de `norte-tui`**: keybindings son lógica
  de PRESENTACIÓN (regla 7 no aplica: no es negocio); se extrae a crate
  cuando exista el segundo consumidor.

## Decisión

- **Desambiguación capa×contexto** (revisión fase 4): las capas se
  fusionan POR CONTEXTO y la especificidad de contexto prevalece sobre la
  capa — un `append_keymap` de usuario en `[pane]` gana al `keymap`
  `[global]` del preset, y un `prepend_keymap` en `[global]` NO pisa un
  binding `[pane]`. Cada capa admite solo sus listas: `keymap` en la capa
  de usuario (o `prepend/append` en un preset) es error de carga.
- **Sintaxis reservada**: el char `+` no es bindeable con la sintaxis
  actual (colisiona con el separador); cuando haga falta (selección de mc,
  fase 5) se añadirá el alias `"plus"` — retrocompatible.
- **A2 + B2**: keymap efectivo precomputado, prefix-free validado al
  cargar, resolución por trie sin timeouts. `Esc` resetea una secuencia
  pendiente; una tecla sin continuación válida resetea (y se descarta).
  Corolario (lo cazó el proptest): `esc` DENTRO de una secuencia
  multi-tecla sería inalcanzable — solo se admite como binding suelto
  (error de carga `EscInSequence`).
  La secuencia pendiente se muestra en la status bar (el overlay
  which-key de la spec llega con la infraestructura de overlays, fase 5).
- **C2**: módulo `norte_tui::keymap`; extracción a crate cuando el GUI lo
  necesite.
- **Comandos nombrados** estilo protocolo (spec: "acciones = comandos"):
  `app.quit`, `pane.switch`, `cursor.up|down|page-up|page-down|top|bottom`,
  `nav.enter`, `nav.parent`. Nombres estables: son los que verán
  `keymap.toml`, la palette (fase 5) y el wire cuando los comandos se
  expongan por protocolo.
- **Sintaxis de tecla** en TOML: `"f5"`, `"ctrl+c"`, `"alt+enter"`,
  `"g"`, `"G"` (mayúscula = el char ya codifica shift; `shift+` solo
  para teclas no-char), `"tab"`, `"esc"`, `"backspace"`, `"enter"`,
  `"space"`, `"up"`/`"down"`/…, `"pgup"`/`"pgdn"`, `"home"`/`"end"`.
- **Presets embebidos** (`orthodox.toml`, `vim.toml`, `cua.toml` via
  `include_str!`), parseados y validados por los MISMOS caminos que el
  keymap de usuario. Default: **orthodox** (decisión cerrada 2026-07-10).
- **Contextos en M1**: `global` y `pane` (el resto de la jerarquía de la
  spec se añade cuando exista la UI que los necesite: dialog en fase 5,
  viewer en fase 7).

## Consecuencias

Positivas:

- Resolución 100% determinista y testeable: el proptest de la spec es
  directamente "ningún par (secuencia, secuencia-prefijo) en el efectivo".
- Los conflictos de capas de usuario dan error DIAGNOSTICABLE al cargar
  (base para `norte doctor keymap`, spec §13).
- Sin relojes: cero flakiness en tests, cero sorpresas por latencia.

Negativas / deuda asumida:

- Prefix-free prohíbe patrones vim legítimos (`d` y `d d`): el preset vim
  se diseña sin ellos en M1 (los modos visual/normal de la spec exigirán
  contextos nuevos, no timeouts).
- El keymap efectivo se recomputa por stack de contextos; con los stacks
  de M1 (2 contextos) es trivial — si la jerarquía crece, cachear por
  stack (los stacks posibles son pocos y estáticos).
- `append_keymap` que colisione por prefijo con el preset es error de
  carga (no silencioso): más estricto que Yazi, coherente con "config
  rota = error claro, jamás comportamiento raro".
