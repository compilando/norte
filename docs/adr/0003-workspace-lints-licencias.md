# 0003 — Estructura del workspace, política de lints y licencias por crate

- Estado: accepted
- Fecha: 2026-07-08
- Decisores: Oscar González, Claude (sesión M0)

## Contexto y problema

Monorepo con ~15 crates previstos (spec §3), dos licencias distintas (§16.2) y
un estándar de calidad alto (§18: pedantic, MSRV, coverage gate). Hay que fijar
la estructura y el mecanismo de enforcement antes del primer crate real.

## Opciones consideradas

1. **Workspace Cargo con `[workspace.lints]` + `[workspace.dependencies]` centralizados**
   - ✓ Un solo sitio para lints y versiones; cada crate hereda con `lints.workspace = true`.
   - ✓ Divergencias imposibles por accidente; excepciones visibles en el diff del crate.
   - ✗ Requiere resolver Cargo moderno (resolver 3, edition 2024) — MSRV lo cubre.
2. **Lints por crate (attrs en cada lib.rs) y versiones sueltas**
   - ✗ Deriva inevitable entre crates; revisar 15 cabeceras a mano.
3. **Repos separados por licencia**
   - ✗ Rompe la atomicidad de cambios cross-crate (proto+core+frontend) y duplica CI.

## Decisión

**Opción 1.** Monorepo, workspace único, `crates/*`:

- **Lints:** `clippy::pedantic = warn` (CI los promociona con `-D warnings`),
  `missing_docs = warn` global. Única excepción global:
  `module_name_repetitions = allow`. El resto de excepciones, `#[allow]` en línea
  justificado (spec §18).
- **`unsafe`:** `#![forbid(unsafe_code)]` en cada crate salvo `norte-vfs-local`,
  que usa `#![deny(unsafe_code)]` + `#[allow]` por ítem con `// SAFETY:` y test.
- **Toolchain:** stable pineada en `rust-toolchain.toml`; **MSRV = stable − 2**
  (hoy 1.94) declarada en `workspace.package.rust-version` y testeada en la
  matrix de CI. Edition 2024.
- **Licencias (modelo Zed, §16.2):** `proto`/`vfs*`/`testkit`/SDK = `MIT OR
  Apache-2.0`; `core` y frontends = `AGPL-3.0-only`. Ficheros `LICENSE-*`
  copiados en cada crate desde el commit 1. `cargo-deny` valida el grafo entero.
- **Dependencias:** versiones en `[workspace.dependencies]`; un crate solo opta
  por las que justifica su PR (regla 8 de CLAUDE.md).

## Consecuencias

- ＋ Un crate nuevo hereda toda la política con 2 líneas (`/new-crate` lo automatiza).
- ＋ El gate de licencias es mecánico (deny.toml), no una wiki que nadie lee.
- － pedantic global genera fricción puntual; se paga con `#[allow]` explicados.
- － Pin de stable exige bump periódico consciente (rutina de release).
