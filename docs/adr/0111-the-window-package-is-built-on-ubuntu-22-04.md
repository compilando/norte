# 0111 — The window package is built on Ubuntu 22.04, and that sets its floor

- Status: accepted; its recipes (`gui-baseline`, `gui-publish`) superseded by ADR 0112
- Date: 2026-09-15
- Decision makers: Oscar González
- Related: ADR 0021 (cargo-dist releases), ADR 0087 (the window is a
  supported frontend), issue #256 (packaging), plan
  `2026-08-19-multi-frontend-tauri-transition.md` task 7.1

## Context and problem statement

`just gui-package` builds the window's `.deb`, `.rpm` and AppImage on the
reference machine, an Arch box kept current. A binary needs the glibc it was
linked against or newer, so those packages demand a glibc newer than most
installed Linux systems have. Task 7.1 asks to "build on the oldest supported
glibc/WebKitGTK baseline", and nothing did.

The window cannot go below WebKitGTK 4.1: Tauri 2 links `webkit2gtk-4.1`. So the
question is which is the oldest distribution that ships it, and how to build
there without a second machine.

Measured on 2026-09-15:

| base | glibc | WebKitGTK 4.1 |
| --- | --- | --- |
| Ubuntu 22.04 | 2.35 | 2.50.4 (jammy-updates) |
| Debian 12 | 2.36 | yes |
| Fedora 41 | 2.40 | yes |

## Considered options

### A — Build in an Ubuntu 22.04 container, locally

`scripts/gui-baseline.sh` feeds `git archive HEAD` into `ubuntu:22.04`, installs
the same system libraries `gui.yml` does, and runs the `gui-package` steps,
keeping rustup, the cargo registry, Node and `target/`
in named Docker volumes.

- Good: the oldest base with WebKitGTK 4.1; one machine; the build is the
  COMMIT, so it does not touch the host's `target/` or `node_modules` and is
  what a tag would hold.
- Good: fits the existing publishing model, which is local
  (`just dist-publish`).
- Bad: a cold build is long (rustup, Node, a full release build), and the
  named volumes are another tree on disk.

### B — Build in CI on `ubuntu-22.04` runners

- Good: reproducible by anyone; no Docker on the reference machine.
- Bad: publishing is local today, and `gui-smoke` deliberately is not CI
  ("a clean-install failure should not depend on who pressed the button").
  Splitting build and smoke across two places is how the published package
  and the tested package drift.

### C — A manylinux-style image with an older glibc

- Bad: no WebKitGTK 4.1 there; the window would have to vendor its GUI stack.

## Decision

**Option A.** The published window package is the one `just gui-baseline`
builds on Ubuntu 22.04, and `just gui-publish` uploads only after the sums
check and `gui-smoke` passes on that `.deb`.

- `target/baseline/<image>/` holds the packages, one `.sha256` per file in the
  format `dist` uses (`<hash> *<file>`), and `glibc.txt`.
- `glibc.txt` records the highest `GLIBC_` symbol each binary needs. Today it
  is **2.34** for `norte-gui`, `norte` and `ntc`: that, together with
  WebKitGTK 4.1, is the documented floor.
- `NORTE_REVISION` is passed in as `git describe --tags --always --long`,
  because the archive has no `.git` and the binaries would otherwise report
  `unknown`.
- `gui-smoke` checks that the package manager owns each `/usr/bin` binary,
  and has an rpm mode for Fedora-family images. The smoke matrix is
  `ubuntu:22.04` and `debian:bookworm` for the `.deb`, `fedora:41` for the
  `.rpm`.

## Consequences

- Positive: the window installs and starts on Ubuntu 22.04, Debian 12 and
  Fedora 41 from one build, and that is verified, not assumed.
- Positive: the floor is a number in a file, so a dependency that raises it
  shows up in the next build's `glibc.txt`.
- Negative: packages are not signed; Fedora installs them with "skipped
  OpenPGP checks". Signing is still undecided (key custody), and this ADR does
  not decide it.
- Negative: Ubuntu 22.04 leaves standard support in 2027; the base moves then,
  and with it the floor.
- Positive: `norte` and `ntc` are built with their default features, one
  package per invocation, as `dist` builds them. Before this, `gui-package`
  used the gate's feature set, and `norte-core/testing` put a policy-free
  minting path (`run_column_values_for_test`) into shipped binaries; the
  `schema` features were in them too, against ADR 0007.
- Neutral: the `release.yml` workflow (cargo-dist) still publishes `norte` and
  `ntc` only; the window is added to a release by hand with `gui-publish`.
