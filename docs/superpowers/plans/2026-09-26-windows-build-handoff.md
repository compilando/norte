# Windows build continuation handoff

Written on 2026-09-26 after commit `a2c21d25` established the reproducible
Windows build lab and reached the first `norte-core` compile gate.

## Continuation prompt

Continue the Windows portability and packaging work for Norte in
`/home/oscar/work/wot/projects/high/norte`.

Read `CLAUDE.md` first. Then inspect commit `a2c21d25` and these documents:

- `docs/adr/0157-native-builders-share-one-release-contract.md`
- `infra/vm/windows/README.md`
- `scripts/platform/common/artifact-contract.md`

Do not rebuild or redesign the working VM infrastructure. The immediate goal
is to make the native Windows gate compile `norte-client`, `norte-core`,
`norte-cli`, and `norte-tui`, then proceed to the Tauri GUI. Do not add GitHub
Actions or NSIS until that compile gate is green.

### Existing VM

- libvirt domain: `norte-win11-build`
- libvirt connection: `qemu:///system`
- observed address: `192.168.122.58`; resolve it again because DHCP may change
- Windows user: `norte`
- password file:
  `/home/oscar/work/wot/projects/high/norte-lab/windows/secrets/windows-password`
- QCOW2 snapshots: `provisioned` and `windows-gate`
- guest checkout: `C:\src\norte`

Never print the password. A non-interactive connection can use:

```sh
sshpass -f /home/oscar/work/wot/projects/high/norte-lab/windows/secrets/windows-password \
  ssh norte@WINDOWS_IP '<command>'
```

The guest has Git 2.55, Node 22.23, npm 10.9, Rust 1.96.1 MSVC, WebView2,
OpenSSH, and Visual Studio 2022 Build Tools. The VM is normally left running.
Do not delete it, its disks, or its snapshots.

Run the gate inside the guest with:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass `
  -File C:/src/norte/scripts/platform/windows/check.ps1 `
  -Repository C:/src/norte
```

### Verified progress

`norte-client` now compiles on Windows through a platform transport seam.
`crates/norte-client/src/transport/unsupported.rs` deliberately returns
`Unsupported` on non-Unix platforms. This is only a compile-audit adapter; it
must eventually be replaced by a per-user authenticated Windows named pipe.
ADR 0157 forbids shipping an embedded-only GUI as a shortcut.

Two invalid self-references in `crates/norte-vfs/src/native.rs` were fixed
from `norte_vfs::wtf8` to `crate::wtf8` and now compile on Windows.

On Linux, all 31 `norte-client` unit tests pass. ShellCheck, xmllint,
`just platform-selftest`, formatting, and `git diff --check` also pass.

### Current red gate

`norte-core` currently reports 28 Windows errors, grouped into four design
problems rather than 28 unrelated fixes:

1. `norte-vfs-local` only exports `Bounds`, `ConfinedRoot`, `LocationKind`,
   `LocationMeta`, and their location implementation under `cfg(unix)`.
   Consumers are primarily `crates/norte-core/src/plugins.rs` and
   `crates/norte-core/src/hooks.rs`.
2. `plugins.rs` directly uses Unix `MetadataExt::{dev, ino}` near line 3933
   and `OsStrExt::as_bytes` near line 4093.
3. `crates/norte-core/src/embedded.rs` directly uses `UnixStream` and the
   Unix-only `crate::daemon` near line 342.
4. `TaskCanceller::Remote` and `TaskPauser::Remote` have inconsistent `cfg`
   conditions in `crates/norte-core/src/backend.rs`.

Secondary Windows warnings exist for `AgentIdentity` in
`crates/norte-connect/src/ssh.rs` and `OsStr` in
`crates/norte-vfs-rar/src/delegate.rs`.

### Required approach

Start by checking Git state and reproducing the current Windows gate. Group
changes by abstraction and keep each group testable. Correct the backend and
embedded `cfg` boundaries first, then design the Windows implementation of
the confined local-provider abstraction, and finally adapt plugins and hooks.

Do not merely hide essential capabilities behind `cfg` to make compilation
green. In particular, do not invent Unix inode semantics on Windows: define a
platform-appropriate file identity behind a shared abstraction. Preserve the
security boundary of `ConfinedRoot` and plugin locations. Keep
`#![forbid(unsafe_code)]`; if safe Windows APIs are insufficient, stop and
explain the smallest isolation boundary before changing that policy.

After each group:

1. run focused Linux checks and tests;
2. synchronize only the changed source into `C:\src\norte`;
3. rerun `check.ps1`, using the existing Cargo cache;
4. record the next genuine Windows blocker.

Do not touch `infra/vm/windows/config.env`, `../norte-lab`, ISOs, QCOW2 files,
or credentials. Do not commit automatically. Leave a focused diff and report
the exact tests and Windows gate reached.

Once the four crates compile, implement the authenticated named-pipe client
and server required by ADR 0157. Only then run the gate with `-WithUi` and
start the portable ZIP/NSIS work.
