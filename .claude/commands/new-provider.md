---
description: Scaffold a VFS provider and connect it to the provider conformance suite
argument-hint: <scheme, for example sftp>
---
Create the VFS provider for the `$ARGUMENTS` scheme:

1. Create `crates/norte-vfs-$ARGUMENTS` with `/new-crate`; use the
   `Apache-2.0 OR MIT` license.
2. Implement the `Provider` trait with a documented `todo!()` for each method.
3. Declare an accurate, conservative initial `Capabilities` value.
4. Connect the conformance suite with
   `norte_vfs::provider_contract! { name: $ARGUMENTS, setup: ... }`.
5. Add a crate-level rustdoc checklist for symlinks, case sensitivity, atomic
   rename, trash, maximum paths, and filename encoding. Resolve it before the
   first release.
6. Do not add dependencies on other providers. Providers are independent.
