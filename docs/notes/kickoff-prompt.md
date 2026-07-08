# Prompt de arranque — Sesión 1 (M0: esqueleto)

> Uso: repo vacío con `docs/spec/norte-spec.md`, `CLAUDE.md` y `.claude/` ya colocados (los tres artefactos generados). Abrir Claude Code en la raíz, entrar en plan mode y pegar esto.

---

Vamos a arrancar **norte**, el file manager headless-core descrito en `docs/spec/norte-spec.md`. Lee la spec completa y `CLAUDE.md` antes de proponer nada. Esta sesión implementa el **hito M0** (spec §15) y nada más: resiste la tentación de adelantar features de M1+.

## Objetivo de M0

Workspace Cargo funcional con el núcleo mínimo verificable: tipos del protocolo, trait VFS con dos providers (memoria y local), scheduler de tasks con cancelación y progreso, y una CLI de humo. Criterio de salida literal de la spec: *"copy/move/delete local con progreso y cancelación, testeado en los 3 OS"*.

## Alcance exacto

1. **Workspace y tooling** — `Cargo.toml` de workspace con lints compartidos (`[workspace.lints]`: clippy pedantic, `missing_docs` en crates de API), `rust-toolchain.toml` (stable pineada), `justfile` con `ci`, `cov`, `fmt`; `deny.toml`; licencias por crate según spec §16.2 (Apache/MIT en proto, vfs, testkit; AGPL en core y cli); `ARCHITECTURE.md` inicial de una página.
2. **`norte-proto` (v0)** — Tipos serde para: `VPath` (representación en bytes con serialización segura — decide y documenta: base64 para segmentos no-UTF8 o WTF-8; escribe ADR), `Entry`, `Capabilities` (bitflags), `Task*` (estados, progreso, prioridad), taxonomía de errores del spec §17.7, y los métodos mínimos `fs.list/stat/copy/move/delete` + notificación `task.progress`. Golden tests de serialización desde el primer tipo.
3. **`norte-vfs`** — Trait `Provider` (spec §5) + tipos. Suite contractual exportada como macro `provider_contract!` para que todo provider presente y futuro pase los mismos tests.
4. **`norte-testkit`** — `MemProvider` determinista con inyección de fallos (latencia, error en byte N, desconexión); primeras 15 fixtures hostiles del corpus (spec §6.2: nombres con bytes inválidos, NFD, path >260, nombres reservados Windows, contenido UTF-16/Latin-1/Shift-JIS con y sin BOM); estrategias proptest (`arb_hostile_filename`, `arb_vpath`).
5. **`norte-vfs-local`** — Provider FS local para los 3 OS: list/stat/read/write/mkdir/remove/rename, capabilities detectadas (case-sensitivity por sondeo, max path), paths largos Windows con `\\?\` transparente, todo I/O vía `spawn_blocking`. Pasa `provider_contract!`. Único crate con `unsafe` permitido si hace falta (con `// SAFETY:`).
6. **`norte-core` (mínimo)** — Scheduler de tasks (colas por prioridad, `CancellationToken`, progreso coalescido) y **copy engine v0**: copy/move/delete sobre el trait Provider, streaming con buffer acotado, colisión → error `Conflict` (las políticas ask/overwrite llegan en M1), cancelación limpia (destino eliminado o `.norte-partial`). Sin daemon todavía: core como lib en modo embebido.
7. **`norte-cli` (humo)** — `norte ls <path> --json`, `norte cp <src> <dst>` con barra de progreso y Ctrl-C = cancelación limpia. Es el banco de pruebas manual del core, no un producto.
8. **CI** — GitHub Actions: matrix {ubuntu, macos, windows} × {stable, MSRV}; jobs fmt, clippy -D warnings, nextest, llvm-cov con gate 85 % en `proto`/`vfs`/`core`, cargo-deny, docs. Badge en README.

## Fuera de alcance en M0 (recházame si te lo pido)

TUI, remotos, archivos comprimidos, plugins, IA, MCP, journal/undo, config en capas, keybindings. Todo eso tiene hito propio.

## Método de trabajo

- Empieza en **plan mode**: propón el orden de implementación y los ADRs necesarios (mínimo: 0001 representación de VPath, 0002 runtime y modelo de blocking I/O, 0003 estructura del workspace y política de lints). No escribas código hasta que apruebe el plan.
- Test-first en `VPath` y en el copy engine: la matriz de tests antes que la implementación. Usa el subagente `test-engineer` para la matriz y `encoding-auditor` sobre todo lo que toque paths.
- PRs conceptuales pequeñas: un commit convencional por unidad coherente (workspace → proto → vfs+testkit → local → core → cli → ci). Tras cada unidad: `just ci` verde antes de seguir.
- Ante cualquier ambigüedad de la spec, pregunta o propón ADR; no inventes en silencio.
- Al terminar: resumen de estado contra el criterio de salida de M0, deuda registrada como issues (`TODO` sin issue = no pasa), y propuesta de plan para M1.

Confirma que has leído spec y CLAUDE.md resumiéndome en 10 líneas las decisiones ya tomadas que condicionan M0 (licencias, VPath, async, testing) y presenta tu plan.
