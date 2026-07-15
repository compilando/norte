# ARCHITECTURE — norte

Mapa mental del repo en una página (estilo matklad). La spec completa vive en
`docs/spec/norte-spec.md`; las decisiones, en `docs/adr/`.

## Qué es

File manager ortodoxo *headless-core*: un core en Rust que expone un protocolo
JSON-RPC y un VFS universal; encima, frontends intercambiables (TUI/GUI/CLI) y
agentes IA gobernados (MCP + policy + journal). Ningún frontend tiene lógica de
negocio: si una operación no se puede hacer vía protocolo, no existe.

## Crates (estado M0)

| Crate | Qué es | Licencia |
|---|---|---|
| `norte-proto` | Tipos del wire format (serde). Sin lógica. Cambiar esto = cambiar el protocolo: golden test + bump + revisión doble | MIT OR Apache-2.0 |
| `norte-vfs` | El contrato central: trait `Provider`, `VPath` (re-export), streams, capabilities, suite contractual | MIT OR Apache-2.0 |
| `norte-vfs-local` | Provider del FS local por OS. **Único crate con `unsafe` permitido** (`// SAFETY:` + test) | MIT OR Apache-2.0 |
| `norte-testkit` | `MemProvider` determinista con fallos inyectables, corpus de fixtures hostiles, estrategias proptest | MIT OR Apache-2.0 |
| `norte-core` | Scheduler de tasks (cancelación, progreso), copy engine. En M0: lib embebida, sin daemon | AGPL-3.0-only |
| `norte-plugin-host` | Host de plugins WASM (M4): manifiesto, capabilities, catálogo. Runtime wasmtime en M4-P2 | AGPL-3.0-only |
| `norte-cli` | `norte ls/cp`: banco de pruebas manual del core. No es un producto | AGPL-3.0-only |
| `norte-tui` | Frontend TUI dual-pane (ratatui). Sin lógica de negocio: proto + core embebido + libs de presentación (norte-encoding) | AGPL-3.0-only |
| `norte-encoding` | Detección/decodificación de encodings de texto (aísla chardetng/encoding_rs) | MIT OR Apache-2.0 |
| `norte-i18n` | Strings de UI por Fluent (es/en) para los frontends | MIT OR Apache-2.0 |
| `norte-theme` | Modelo de theming compartido (roles semánticos, Color truecolor con degradación 256/16, presets). Sin backend de render: lo consumen TUI y GUI (M5) | MIT OR Apache-2.0 |

Hitos posteriores añaden: `norte-vfs-{sftp,object,archive}`, `norte-index`,
`norte-ai`, `norte-mcp`, `norte-plugin-host`, `norte-gui` (spec §3).

## Reglas de dependencia (enforcement: cargo-deny + revisión)

```
proto  ←  vfs  ←  { vfs-local, testkit, core }  ←  cli
```

- Los frontends solo dependen de `norte-proto` (+ `norte-core` en modo embebido).
- Los providers VFS no se conocen entre sí.
- `norte-testkit` es dev-dependency de quien lo necesite; nunca dependencia normal.

## Invariantes que no se negocian

1. Nombres de archivo = **bytes** (`VPath`); UTF-8 solo para display, lossy y marcado.
2. Nada de I/O bloqueante en contexto async: FS local vía `spawn_blocking` (ADR 0002).
3. Toda operación larga es una Task con `CancellationToken` chequeado en el inner loop.
4. Cancelar deja el destino limpio o `.norte-partial`; jamás un archivo a medias sin marcar.
5. Errores tipados por taxonomía (spec §17.7); los frontends renderizan por categoría.

## Dónde está cada cosa

- Comandos de desarrollo: `justfile` (CI corre exactamente `just ci`).
- Decisiones de arquitectura: `docs/adr/` (MADR; se crean con `/adr`).
- Corpus de casos hostiles: `crates/norte-testkit/fixtures/`.
- Ecosistema Claude Code (agentes, hooks, commands): `.claude/` + `CLAUDE.md`.
