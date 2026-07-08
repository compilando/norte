---
description: Scaffolding de un crate nuevo del workspace con lints y licencia correctos
argument-hint: <nombre del crate, p.ej. norte-vfs-sftp>
---
Crea el crate `$ARGUMENTS` en `crates/`:

1. `Cargo.toml` con `lints.workspace = true`, versión 0.0.0, y licencia según
   la tabla de la spec §16.2: Apache-2.0 OR MIT para proto/vfs*/testkit/SDK;
   AGPL-3.0-only para core/frontends. Copia los ficheros `LICENSE-*` que toquen.
2. `src/lib.rs` con `#![forbid(unsafe_code)]` (salvo norte-vfs-local),
   `#![warn(missing_docs)]`, doc de crate de una línea.
3. Módulo de tests vacío y, si es provider, hueco para `provider_contract!`.
4. Alta en `[workspace.members]` y entrada nueva en `ARCHITECTURE.md`.
5. Verifica: `cargo build -p $ARGUMENTS && cargo clippy -p $ARGUMENTS`.
