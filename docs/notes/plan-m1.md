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
| 2 | Engine: colisiones + trait ancho | `CollisionPolicy { Ask, Skip, Overwrite, RenameAuto, Newer }` en copy/move; symlinks: `symlink()` + follow/preserve/skip (issues #6, #8); `read` con rango y capabilities de remotos — ensanchar el trait ANTES de que M2 lo implemente | Cambio de proto (params de fs.copy) → golden + bump + ADR |
| 3 | TUI esqueleto | ratatui+crossterm, dual-pane, listado desde `Engine::list`, nombres lossy con badge "no-UTF8", sort, navegación, cd | Solo depende de proto + core embebido (regla 7) |
| 4 | Keymap engine | Resolución de secuencias + presets (mc/tc/vim-like), `keymap.toml`; proptest "ninguna secuencia ambigua" (spec §12) | ADR: semántica de resolución |
| 5 | Tasks en el TUI | Panel de tasks vivo (watch), copy/move/delete desde panes, diálogos de colisión (usa fase 2), Ctrl-C/Esc cancelación | Reusa TaskHandle tal cual |
| 6 | Config en capas | defaults → `/etc/norte` → `~/.config/norte` → `.norte/` → flags; TOML dividido, hot-reload con watcher (degradación a polling), JSON Schema publicado | ADR: precedencia y hot-reload |
| 7 | Viewer + encoding | Detección BOM → chardetng → heurística NUL; "recargar como…" (encoding_rs); hexview fallback; corpus de contenidos del testkit como fixtures (por fin ejercitado); EOL en status bar | deps nuevas: chardetng, encoding_rs (justificar) |
| 8 | Trash | freedesktop / Recycle Bin / macOS; capability TRASH; degradación explícita a borrado permanente con aviso | dep candidata: crate `trash` (evaluar vs propio) |
| 9 | i18n | Fluent es/en para TUI y CLI (issue #1); `t!()` en todo string de UI | |
| 10 | Calidad M1 | Snapshot tests del TUI (insta), benchmarks criterion (cold start <50 ms, 100k listado <200 ms hasta primer render), fuzzing de config TOML | Presupuestos de la spec §12 |

## Decisiones tomadas (usuario, 2026-07-10)

1. **Daemon → M2.** El TUI de M1 va embebido (como el CLI M0); el daemon
   JSON-RPC llega con los remotos, que lo necesitan de verdad.
2. **Preset por defecto: mc clásico** (F5 copiar, F6 mover, F7 mkdir,
   F8 borrar, Tab entre panes…). Presets alternativos después.
3. **Widgets: librería, no a mano.** Elegida **ratatui** (MIT — compatible
   con nuestro frontend AGPL): estándar de facto, mantenimiento activo, y
   trae de serie Table/List/Tabs/Gauge/Scrollbar/Paragraph; ecosistema para
   lo que falte (`tui-textarea`/`tui-input`, MIT, para inputs y el viewer).
   Backend `crossterm` (MIT). Evaluar add-ons concretos al llegar a la fase 3.

## Preparación para remotos y plugins (decisión 4)

sftp/s3 NO son plugins: son providers de primera parte en M2
(`norte-vfs-sftp`, `norte-vfs-object` — MIT/Apache, como todo provider).
FTP plano: candidato a provider extra en M2+ (no está en la spec; decidir
allí). Providers de terceros (WebDAV, Drive, ERP…) llegan como plugins WASM
en M4 vía la interfaz WIT `provider` (spec §7.1), sandboxed y sin fricción
de licencia (el SDK es MIT/Apache).

Para que M2 no duela, la fase 2 de M1 ENSANCHA el trait `Provider` de una
vez (cada método nuevo rompe a todo implementador — mejor antes de que
existan providers remotos):

- `symlink()` + política follow/preserve/skip (issue #6),
- `read` con rango (`Option<Range>`) — lo exige el resume de M2
  (`.norte-partial` + offset, spec §5),
- revisar capabilities faltantes para remotos (APPEND, RANDOM_WRITE) y la
  semántica de reintentos (`ProviderUnavailable{retryable}` + backoff en el
  engine).

Lo que ya está listo para remotos: `Authority` validada en VPath
(`sftp://host:22/...`), colisiones contra el provider DESTINO, copy engine
cross-provider genérico, `copy_native` con contrato anti-sobrescritura
(la mina S3 CopyObject ya está desactivada en la suite contractual),
MemProvider con desconexión/latencia inyectables. Secretos: keyring +
referencias en `connections.toml` (M2, regla dura 10). Tests de M2 contra
servicios reales via testcontainers (openssh, MinIO — spec §12).

## Riesgos principales

- Perf del listado 100k (presupuesto 200 ms): puede exigir paginación del
  protocolo antes de lo previsto (el ADR 0004 ya deja la cláusula de
  compatibilidad lista).
- chardetng/encoding_rs: superficie de deps grande — justificar y aislar en
  un crate `norte-encoding` con la misma dualidad MIT/Apache.
- Hot-reload de config con watcher: límites de inotify (trampa conocida) —
  degradar a polling con aviso, jamás fallar.
