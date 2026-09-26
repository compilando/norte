# 0157 — Native builders share one release contract, and VM state stays outside the tree

- Status: accepted
- Date: 2026-09-26
- Decision makers: Oscar González
- Related: ADR 0021 (cargo-dist releases), ADR 0066 (daemon-only GUI), ADR
  0112 (build, smoke and verify the same release artefacts)

## Context

The release configuration names Linux, macOS and Windows targets, but the
only complete product build is the Linux baseline. `cargo-dist` can plan
Windows archives for `norte` and `ntc`, while the Tauri configuration only
bundles Linux packages. More importantly, `norte-client` and the daemon still
use Unix sockets directly, so a Windows target does not compile.

Adding a Windows VM beside the existing Linux scripts could make one local
machine produce an installer, but it would create a second release system:
one set of commands in the VM, another in Actions, and the Linux baseline's
build-from-ref and verify-before-publish guarantees in neither. A directory
containing an opaque, hand-maintained VM would be especially hard to review
or recreate.

The eventual macOS builder has the same shape as Windows: it must build on its
native OS, package the Tauri window with the exact CLI/TUI sidecars, smoke the
result and return evidence to the release host. The orchestration must name
that common shape without pretending the three operating systems have the
same package tools or smoke environment.

## Decision

### One release contract, native adapters

Every supported platform implements six operations: `check`, `build`,
`package`, `smoke`, `verify` and `publish`. A platform adapter may use Docker,
a VM, physical hardware or a hosted runner, but:

1. a publishable build starts from an exact Git commit or tag, never from an
   ambient working directory;
2. `norte`, `ntc` and `norte-gui` in one graphical package come from the same
   build;
3. the smoke test consumes the files that will be published, not another
   build of the same source;
4. verification records revision, target, tool versions, checksums and smoke
   results; and
5. publication accepts only that verified output directory.

ADR 0112's Linux baseline remains the Linux adapter. It is not rewritten as a
condition of adding Windows. Windows first implements the contract in
`scripts/platform/windows`; macOS can implement the same boundary later.

### Local VM and hosted CI run the same guest scripts

The Windows guest owns PowerShell scripts for checking, building, packaging
and smoking. The local libvirt host only creates the machine, transfers an
exact source input, invokes those scripts and retrieves their output. GitHub
Actions invokes the same PowerShell entry points directly on a Windows
runner. There is no CI-only build recipe.

The first local provider is libvirt/KVM. Provider-specific host operations
live under `infra/vm/windows`; they never enter the guest scripts. Vagrant or
another provider may be added later without changing the release contract,
but no third-party Windows base box is trusted as an input. The VM starts
from a user-supplied Microsoft ISO whose SHA-256 is checked.

### Definitions in the tree, state outside it

Reviewable definitions, unattended-install templates and provisioning scripts
are committed. The default state root is the sibling `../norte-lab`,
overridable with `NORTE_LAB_HOME`. ISOs, generated answer media, disks,
snapshots, caches, credentials and artefacts live there and are never added to
the source tree.

The unattended install receives its local administrator password through the
environment and writes it only into generated media under the external state
root. Release signing credentials do not enter the VM at all; hosted CI will
use a protected release environment and an identity-based signing service.

### Windows milestones

Windows x86_64 is the first target. The sequence is: reproducible VM, reliable
red compile gate, named-pipe transport, portable ZIP, NSIS, hosted CI, MSI,
then signing. NSIS precedes MSI because it gives the normal desktop installer
without making WiX and VBSCRIPT part of the first portability loop.

The reference GUI stays daemon-only, as ADR 0066 requires. Windows therefore
gets a named-pipe client and server with a per-user security boundary; it does
not get an embedded-only GUI that behaves differently from Linux and macOS.

## Consequences

- A local VM is a debugging tool and an independent clean-machine smoke
  environment; it is not a private alternative release authority.
- GitHub Actions can become the Windows release builder without changing the
  commands that were made green locally.
- VM disks are disposable. Recreating one may take time, but no undocumented
  state is required to do it.
- Windows support cannot be declared merely because `dist plan` lists a ZIP:
  the transport, package and clean-machine smoke all have gates.
- The infrastructure has a small platform abstraction, not a universal shell
  script. OS-specific package and security details remain explicit.
- Windows 11 x86_64 is the first tested desktop. Supporting Windows 10 or
  ARM64 requires its own matrix entry and evidence rather than an inference.
