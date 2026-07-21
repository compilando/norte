---
description: Scaffold a workspace crate with the required lints and license
argument-hint: <crate name, for example norte-vfs-sftp>
---
Create `$ARGUMENTS` under `crates/`:

1. Add a `Cargo.toml` with `lints.workspace = true`, version `0.0.0`, and the
   license from specification section 16.2: `Apache-2.0 OR MIT` for protocol,
   VFS, testkit, and SDK crates; `AGPL-3.0-only` for the core and frontends. Copy
   the applicable `LICENSE-*` files.
2. Add `src/lib.rs` with `#![forbid(unsafe_code)]` (except for
   `norte-vfs-local`), `#![warn(missing_docs)]`, and a one-line crate doc.
3. Add an empty test module and, for a provider, a placeholder for
   `provider_contract!`.
4. Add the crate to `[workspace.members]` and to `ARCHITECTURE.md`.
5. Run `cargo build -p $ARGUMENTS` and `cargo clippy -p $ARGUMENTS`.
