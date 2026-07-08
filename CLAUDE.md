# CLAUDE.md — norte

File manager ortodoxo headless-core en Rust. Core daemon + protocolo JSON-RPC + frontends (TUI/GUI/CLI) + agentes vía MCP. Spec completa: `docs/spec/norte-spec.md`. Mapa del repo: `ARCHITECTURE.md`.

## Comandos

```bash
cargo build --workspace                  # build completo
cargo nextest run --workspace           # tests (usamos nextest, no `cargo test`)
cargo nextest run -p norte-vfs          # tests de un crate
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo llvm-cov nextest --workspace      # coverage local (gate CI: 85% en core/vfs/proto)
cargo deny check                        # licencias + advisories
just ci                                 # todo lo anterior, como lo corre CI
```

## Estructura (workspace Cargo)

- `crates/norte-proto` — tipos del protocolo. Apache/MIT. CUALQUIER cambio aquí es cambio de wire format: exige golden test actualizado + bump de versión de protocolo + revisión doble.
- `crates/norte-vfs` — trait `Provider` + tipos (`VPath`, `Entry`, `Capabilities`). Apache/MIT.
- `crates/norte-vfs-{local,sftp,object,archive}` — providers. Los providers NO se conocen entre sí.
- `crates/norte-core` — daemon: scheduler, policy engine, journal, sesiones. AGPL.
- `crates/norte-{index,ai,mcp,plugin-host}` — subsistemas del core.
- `crates/norte-{tui,cli}` — frontends. Solo dependen de `norte-proto` (+ core en modo embebido).
- `crates/norte-testkit` — `MemProvider`, fixtures hostiles, estrategias proptest. Apache/MIT.
- `docs/adr/` — decisiones de arquitectura (MADR). `docs/spec/` — la spec.

## Reglas duras (no negociables)

1. **Nombres de archivo = bytes.** Nunca asumas UTF-8 en paths. Usa `VPath` (bytes) / `OsString`; `String` solo para display con conversión lossy explícita. Si escribes `path.to_str().unwrap()` la PR se rechaza.
2. **Nada de I/O bloqueante en contexto async.** FS local va por `spawn_blocking` o las utilidades de `norte-vfs-local`. Nada de `std::fs` directo fuera de ese crate.
3. **Toda operación larga es una Task** con `CancellationToken` chequeado en el inner loop. Toda Task nueva lleva test de cancelación limpia.
4. **Toda mutación pasa por el journal.** No hay writes "por fuera"; si añades una operación mutante, añades su entrada de journal y su undo (o la marcas `Irreversible` con justificación).
5. **`unsafe` prohibido** (`#![forbid(unsafe_code)]`) salvo en `norte-vfs-local`, con `// SAFETY:` + test.
6. **Errores tipados:** `thiserror` en libs, `anyhow` solo en binarios. Nunca `unwrap()`/`expect()` fuera de tests salvo invariante comentada.
7. **Frontends sin lógica de negocio.** Si una feature necesita lógica, va al core y se expone por protocolo.
8. **Sin dependencias nuevas sin justificación** en la descripción de la PR (qué aporta, tamaño, mantenimiento, alternativa evaluada).
9. **Los agentes/plugins nunca tocan el FS directo**: todo pasa por core → policy engine. No introduzcas atajos "temporales".
10. **Secretos jamás en config ni en logs.** Keyring + referencias.

## Convenciones

- Conventional Commits (`feat(vfs): …`, `fix(tui): …`). PRs < 400 líneas netas; una PR = un propósito.
- Test-first en bugs: primero el test rojo (si es de encoding/paths, la fixture entra al corpus de `norte-testkit`), luego el fix.
- Docs: rustdoc con doctest en todo item público de `proto`/`vfs`/SDK. `#![warn(missing_docs)]` activo.
- Los strings de UI van por Fluent (`i18n/`), nunca hardcodeados: `t!("pane.copy.confirm")`.
- Tracing: toda función de core con efectos lleva `#[instrument]` con campos relevantes (task_id, vpath en forma redactada).
- ADR nuevo para cualquier decisión que afecte a protocolo, licencias, seguridad o dependencias estructurales. Usa `/adr` (slash command).

## Definition of Done (toda PR)

Código + tests (unit; integration si toca borde OS/provider) + rustdoc + changelog (release-plz lo deriva del commit) + `just ci` verde local + sin `TODO` sin issue vinculada.

## Trampas conocidas del dominio (léelas antes de tocar vfs/core)

- macOS normaliza nombres a NFD; compara siempre en NFC, preserva bytes originales.
- Windows: paths >260 requieren prefijo `\\?\`; nombres reservados (`CON`, `NUL`, `AUX`…); trailing dots/spaces se pierden silenciosamente si no usas el prefijo.
- ZIP: el encoding del nombre depende del bit 11 (UTF-8) o es cp437/encoding local; nunca decodifiques a ciegas.
- Case-insensitive FS: la colisión se evalúa contra el FS **destino**, no el origen.
- inotify tiene límites de watches; el watcher debe degradar a polling con aviso, no fallar.
- Cancelar una copia debe dejar destino limpio o `.norte-partial`, nunca un archivo a medias sin marcar.
