# 0007 — Config en capas: precedencia y hot-reload

- Estado: accepted
- Fecha: 2026-07-11
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: spec §13 (config), plan M1 fase 6, ADR 0006 (keymap).

## Contexto y problema

La spec fija: config TOML dividida (`norte.toml`, `keymap.toml`, …), en
capas (defaults → sistema → usuario → proyecto → flags), con hot-reload.
Queda por decidir la semántica exacta de fusión, qué pasa con config rota
(al arrancar Y en caliente), cómo degrada el watcher (trampa conocida:
límites de inotify) y dónde vive el código.

## Opciones consideradas

### A. Fusión de escalares

- **A1 — merge profundo por clave**: máxima flexibilidad, semántica opaca
  (¿qué gana si dos capas tocan la misma tabla?).
- **A2 — último-gana por CAMPO simple, con capas ordenadas**: cada campo
  presente en una capa superior pisa al de la inferior; las ausencias caen
  a la capa anterior y al final al default compilado. Trivial de razonar y
  de diagnosticar (`norte doctor config` podrá decir "viene de X").

### B. Config rota

- **B1 — ignorar la capa rota con warning**: arranca siempre, pero el
  usuario opera con una config que no es la que escribió.
- **B2 — al ARRANCAR: error duro con archivo y diagnóstico; en CALIENTE
  (hot-reload): conservar la config vigente + aviso en la barra**: romper
  un TUI en marcha por un TOML a medio guardar sería absurdo; arrancar
  con config ignorada, también.

### C. Watcher

- **C1 — solo notify (inotify/FSEvents/ReadDirectoryChanges)**: falla en
  FS de red y con límites de inotify agotados.
- **C2 — notify con degradación a POLLING con aviso** (trampa documentada
  en CLAUDE.md): si el watcher no arranca, un poll de mtimes cada 2 s;
  el usuario ve "config: vigilancia degradada a polling".

## Decisión

- **A2**. Capas en orden de precedencia ascendente: defaults compilados →
  `/etc/norte/` (`%ProgramData%\norte` en Windows) → `$XDG_CONFIG_HOME/norte`
  (`~/.config/norte`; `%APPDATA%\norte`) → `./.norte/` (el cwd, sin
  búsqueda hacia arriba en M1) → flags de CLI. Escalares: último-gana por
  campo.
- **`keymap.toml` NO es escalar**: cada capa aporta
  `prepend_keymap`/`append_keymap` (ADR 0006) y se PLIEGAN por precedencia:
  los `prepend` de capas superiores ganan (van primero), luego el preset,
  luego los `append` (los de capa superior antes). El preset se elige en
  `norte.toml` (`[keymap] preset = "orthodox"`) o con el flag.
- **B2**. Claves desconocidas en `norte.toml`/`keymap.toml` = error con
  archivo y campo (deny_unknown_fields — coherente con "config rota =
  error claro, jamás comportamiento raro", ADR 0006). Reload en caliente
  que falla: se conserva TODO lo vigente + aviso por la barra.
- **C2**, con debounce (los editores escriben en ráfagas); el reload
  relee TODAS las capas (barato y sin estados a medias).
- **Ubicación**: módulo `norte_tui::config` (mismo criterio C2 del ADR
  0006); cuando el daemon M2 necesite config se extrae a crate.
- **Exención acotada de la regla 2**: la config del PROPIO frontend se lee
  con `std::fs` (leerla vía providers sería circular); la condición es que
  desde contexto async SIEMPRE se pase por `load_async`/`spawn_blocking` —
  el módulo lo ofrece y el binario lo cumple.
- **Vigilancia**: watcher nativo + poll de RESPALDO siempre activo (lento
  con nativo, rápido sin él): cubre dirs de capa creados en caliente y
  eventos perdidos por colas de inotify; los `Err` del watcher también
  disparan reload (releer todo es la respuesta correcta a "quizá perdiste
  eventos"). Soltar el `Watch` cancela ambos mecanismos (regla 3).
- **JSON Schema publicado** (spec §13): `docs/schema/norte.schema.json` y
  `keymap.schema.json` generados con `schemars` desde los MISMOS structs
  serde que parsean — un test golden falla si el schema commiteado
  diverge del código.

## Consecuencias

Positivas:

- Precedencia explicable en una línea; diagnósticos con archivo y campo.
- Editar `keymap.toml` o `norte.toml` se aplica EN CALIENTE sin reiniciar
  y sin romper la sesión ante un guardado a medias.
- Los schemas dan autocompletado/validación en editores (taplo/even
  better TOML) gratis.

Negativas / deuda asumida:

- `.norte/` solo mira el cwd (sin walk hacia arriba estilo git): M1;
  ampliar cuando haya una noción de "proyecto".
- Deps nuevas: `notify` (watcher multiplataforma — exactamente lo que la
  spec exige) y `schemars` (schemas desde los structs; se evalúa moverla
  tras un feature-flag si pesa en el binario).
- El debounce introduce una latencia pequeña (≈300 ms) entre guardar y
  ver el cambio: aceptable.
- `theme.toml`/`openers.toml`/… llegan con las features que los consuman.
