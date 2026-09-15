# 0112 — Release artefacts are built in one pinned image, and smoked per distribution

- Status: accepted
- Date: 2026-09-15
- Decision makers: Oscar González
- Related: ADR 0021 (cargo-dist releases), ADR 0087 (the window is a
  supported frontend), ADR 0111 (the window package on Ubuntu 22.04, whose
  recipes this replaces), plan `2026-09-15-baseline-release-system.md`

## Context and problem statement

The v0.3.0-alpha.4 release check came back GO, and it was wrong. Six things
made it possible:

1. `just dist` built the `norte` and `ntc` tarballs and `installer.sh` on the
   reference machine, whose glibc is 2.44. Its `norte` needed **GLIBC_2.39**:
   it does not start on Ubuntu 22.04 (2.35) or Debian 12 (2.36).
2. `just dist-smoke` ran those tarballs on the machine that built them, so it
   could not see (1).
3. Builds took HEAD, not a ref. After tagging `v0.3.0-alpha.4` on the release
   commit and merging, the binaries reported `v0.3.0-alpha.4-1-gaecbc039`.
4. `gui-baseline` reinstalled apt packages, rustup and "the latest Node 22" on
   every run: slow, and two runs were not the same build.
5. ADR 0111's glibc floor was a number written to `glibc.txt` and enforced by
   nothing.
6. Nobody smoke-tested the AppImage.

The window packages were already built on Ubuntu 22.04 (ADR 0111); the
tarballs and installers published next to them were not, and the checks
could not tell the difference.

## Considered options

### A — One pinned builder image for every artefact, and a container smoke matrix

A Dockerfile on Ubuntu 22.04 pinned by digest, with Node pinned and checked,
the toolchain from `rust-toolchain.toml` and cargo-dist from
`dist-workspace.toml`. A build clones the ref inside it, runs `dist` and the
Tauri bundler, and refuses the result if a binary is above the floor or
reports another revision. A matrix file, pinned by digest, smoke-tests each
artefact on each distribution.

- Good: one floor for everything published; the tarballs start on 22.04; the
  revision is the ref's; the image is the same build after build.
- Bad: releasing needs Docker, a builder image and cache volumes on disk, and
  a full matrix is 18 container runs.

### B — Keep `dist` on the host, smoke the tarballs in containers

- Good: small change.
- Bad: detects (1), fixes nothing; every release would fail its smoke until
  someone moved the build anyway.

### C — `cargo-zigbuild` targeting an old glibc from the host

- Good: no containers for the build.
- Bad: a new build tool, and ADR 0021 already records cross-building aws-lc-rs
  as fragile; the window still needs WebKitGTK 4.1 headers of a matching age.

### D — CI runners on `ubuntu-22.04`

- Bad: GitHub Actions has not run on this repository since 2026-07-13, and
  publishing is local (`gh release upload`). A build in one place and a smoke
  in another is how the published and the tested artefact drift apart.

## Decision

**Option A**, in `scripts/baseline/`:

- `Dockerfile` + `image.sh`: the builder image, tagged `norte-builder:<hash>`
  of the Dockerfile, `rust-toolchain.toml` and `dist-workspace.toml`, so a
  stale image is never reused. The Docker context is those three files.
- `build.sh [ref]`: clones the ref from the read-only `.git` (so `git
  describe` and `dist`'s source archive work), runs `dist build` for the
  local and global artefacts, bundles the window with the **same** `norte`
  and `ntc` binaries as sidecars, and writes `target/baseline/<revision>/`
  with `dist/`, `gui/`, `MANIFEST` and `SHA256SUMS`. The MANIFEST records the
  highest glibc each shipped binary needs and the revision it reports; a
  binary above **glibc 2.35** or with another revision fails the build.
- `matrix.txt` + `smoke.sh` + `smoke-inside.sh`: tarball and installer on
  Ubuntu 22.04/24.04, Debian 12/13 and Fedora 41; deb on the four Debian-family
  images; rpm on Fedora 41; AppImage on Ubuntu 22.04, Debian 13 and Fedora 41 —
  every image pinned by digest. The installer is pointed at the local files
  through `INSTALLER_DOWNLOAD_URL=file:///…`; the AppImage is extracted,
  because a container has no FUSE. Each run checks the reported revision,
  that a listing arrives, and that the window survives start-up under Xvfb.
- `verify.sh`: sums, floor, revisions and a passed smoke for every matrix line.
- `just baseline [ref]` runs all three. `baseline-publish <tag> <dir>` uploads
  only a verified build whose revision is exactly `<tag>-0-g…`.
- `lib.sh` holds the logic that can be wrong silently, tested without Docker
  by `just baseline-selftest`.
- Removed: `dist`, `dist-smoke`, `dist-publish`, `gui-baseline`, `gui-publish`,
  `scripts/dist-smoke.sh`, `scripts/gui-baseline.sh`. `gui-smoke` stays for
  the local `gui-package` loop, through the same in-container smoke body.

## Consequences

- Positive: what is published was built on the oldest supported base, started
  on every distribution of the matrix, and says which commit it is.
- Positive: the floor, the revision and the matrix are checked by code that
  fails, not recorded in a file somebody may read.
- Negative: releasing needs Docker; the builder image and the
  `norte-baseline-registry` / `norte-baseline-target` volumes live on disk
  until `just baseline-prune`.
- Negative: Ubuntu 22.04 leaves standard support in 2027; the base and the
  floor move then, and the image digest is the line that changes.
- Negative: packages are still unsigned, and `cargo-semver-checks` still skips
  every check on a prerelease bump. Neither is addressed here.
