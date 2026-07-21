# 0003 - Workspace structure, lint policy, and crate licenses

- Status: accepted
- Date: 2026-07-08
- Decision makers: Oscar González

## Context

The planned monorepo contains roughly fifteen crates, two license families, and
strict requirements for linting, MSRV support, and coverage. The project needs a
single enforcement mechanism before the first production crate is added.

## Options considered

1. **One Cargo workspace with centralized `[workspace.lints]` and
   `[workspace.dependencies]`**
   - Keeps lint policy and dependency versions in one place.
   - Lets each crate inherit the policy with `lints.workspace = true` while
     making exceptions visible in the crate diff.
   - Requires a modern Cargo resolver and Rust 2024 edition, both covered by the
     supported toolchain.
2. **Per-crate lint attributes and independent dependency versions**
   - Inevitably drifts and makes maintainers review the same policy in every
     crate.
3. **Separate repositories for each license family**
   - Breaks atomic protocol/core/frontend changes and duplicates CI.

## Decision

Use a single monorepo and Cargo workspace under `crates/*`.

- **Lints:** enable `clippy::pedantic` and `missing_docs` as warnings; CI
  promotes warnings to errors. Allow `module_name_repetitions` globally. Place
  other justified exceptions on the affected item.
- **Unsafe code:** each crate uses `#![forbid(unsafe_code)]` except
  `norte-vfs-local`, which uses `#![deny(unsafe_code)]` and permits individual
  items only with a `// SAFETY:` explanation and tests.
- **Toolchain:** pin stable Rust in `rust-toolchain.toml`. Set the MSRV to stable
  minus two releases in `workspace.package.rust-version` and test it in CI. Use
  Rust 2024 edition.
- **Licensing:** protocol, VFS, testkit, and SDK crates use
  `MIT OR Apache-2.0`; the core and official frontends use
  `AGPL-3.0-only`. Copy the applicable license files into each crate and verify
  the complete dependency graph with `cargo-deny`.
- **Dependencies:** declare versions in `[workspace.dependencies]`. A crate opts
  into a dependency only when its pull request justifies that dependency.

## Consequences

- A new crate inherits project policy with minimal configuration; the
  `/new-crate` command automates the setup.
- License enforcement is executable configuration rather than an informal
  convention.
- Pedantic lints occasionally require a documented local exception.
- The pinned stable toolchain requires a deliberate periodic update.
