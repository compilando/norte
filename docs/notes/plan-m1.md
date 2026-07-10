# Propuesta de plan M1 — "TUI usable"

Criterio de salida (spec §15): **"yo lo uso a diario en vez de Yazi/mc"**.
Alcance spec: ratatui dual-pane, keymap engine + presets, config en capas,
viewer con detección de encoding, trash.

Método idéntico a M0: una fase = un commit convencional con `just ci` verde,
test-first en lo que toque paths/encoding/keymaps, encoding-auditor y
rust-reviewer sobre los diffs, ADR para toda decisión de protocolo/estructura.

## Fases propuestas

| # | Fase | Contenido | Notas |
|---|------|-----------|-------|
| 1 | Deuda dura de M0 | Issues #2–#5, #9, #10: no-replace renames (TOCTOU), staging NAME_MAX, sondeo lazy, case-rename Windows, move walk único, test fd-leak | Endurece el engine ANTES de sentarle un TUI encima |
| 2 | Engine: políticas de colisión | `CollisionPolicy { Ask, Skip, Overwrite, RenameAuto, Newer }` en copy/move; el Conflict de M0 se convierte en pregunta al frontend; symlinks: API `symlink()` + follow/preserve/skip (issues #6, #8) | Cambio de proto (params de fs.copy) → golden + bump + ADR |
| 3 | TUI esqueleto | ratatui+crossterm, dual-pane, listado desde `Engine::list`, nombres lossy con badge "no-UTF8", sort, navegación, cd | Solo depende de proto + core embebido (regla 7) |
| 4 | Keymap engine | Resolución de secuencias + presets (mc/tc/vim-like), `keymap.toml`; proptest "ninguna secuencia ambigua" (spec §12) | ADR: semántica de resolución |
| 5 | Tasks en el TUI | Panel de tasks vivo (watch), copy/move/delete desde panes, diálogos de colisión (usa fase 2), Ctrl-C/Esc cancelación | Reusa TaskHandle tal cual |
| 6 | Config en capas | defaults → `/etc/norte` → `~/.config/norte` → `.norte/` → flags; TOML dividido, hot-reload con watcher (degradación a polling), JSON Schema publicado | ADR: precedencia y hot-reload |
| 7 | Viewer + encoding | Detección BOM → chardetng → heurística NUL; "recargar como…" (encoding_rs); hexview fallback; corpus de contenidos del testkit como fixtures (por fin ejercitado); EOL en status bar | deps nuevas: chardetng, encoding_rs (justificar) |
| 8 | Trash | freedesktop / Recycle Bin / macOS; capability TRASH; degradación explícita a borrado permanente con aviso | dep candidata: crate `trash` (evaluar vs propio) |
| 9 | i18n | Fluent es/en para TUI y CLI (issue #1); `t!()` en todo string de UI | |
| 10 | Calidad M1 | Snapshot tests del TUI (insta), benchmarks criterion (cold start <50 ms, 100k listado <200 ms hasta primer render), fuzzing de config TOML | Presupuestos de la spec §12 |

## Decisiones a tomar al arrancar (preguntas para el usuario)

1. **¿Daemon en M1 o M2?** El TUI puede ir embebido (como el CLI M0) y el
   daemon JSON-RPC llegar con los remotos de M2 (que lo necesitan de verdad).
   Recomendación: embebido en M1 — menos superficie, mismo protocolo de tipos.
2. **Preset de keymap por defecto** (mc clásico vs híbrido moderno).
3. **Dependencias UI**: ratatui+crossterm asumidas; ¿widget de tabla propio o
   `ratatui-widgets`? (evaluar al llegar a fase 3).

## Riesgos principales

- Perf del listado 100k (presupuesto 200 ms): puede exigir paginación del
  protocolo antes de lo previsto (el ADR 0004 ya deja la cláusula de
  compatibilidad lista).
- chardetng/encoding_rs: superficie de deps grande — justificar y aislar en
  un crate `norte-encoding` con la misma dualidad MIT/Apache.
- Hot-reload de config con watcher: límites de inotify (trampa conocida) —
  degradar a polling con aviso, jamás fallar.
