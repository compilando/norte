# 0021 - Prebuilt releases and cargo-dist installers

- Status: accepted
- Date: 2026-07-15
- Decision makers: Oscar González
- Related: specification sections 15, 16, and 18; ADR 0011

## Context

Before this decision, installing norte required a source build of hundreds of
dependencies. Alpha releases need optimized prebuilt binaries and a one-line
installer on the main desktop operating systems.

## Decision

### Release tooling

Use cargo-dist 0.32. `dist init` maintains the release workflow and
`dist-workspace.toml`. A `v*` tag builds each target, generates shell and
PowerShell installers plus checksums, creates the GitHub release from the
changelog, and uploads the artifacts.

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/compilando/norte/releases/latest/download/norte-tui-installer.sh | sh
```

A hand-written workflow was rejected because it would duplicate cargo-dist's
checksum, PATH, platform, and signing work indefinitely.

### Targets

Build `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-gnu`, and
`x86_64-pc-windows-msvc`. Defer musl until the `aws-lc-rs` cryptography stack
can be cross-compiled reliably in CI. Add Windows Arm and other targets when
demand justifies them.

### Distributed binary

Distribute only `norte-tui` for now. `norte-cli` remains a source-installed
engine test bed and sets `[package.metadata.dist].dist = false`. Whether to
rename the command or bundle the CLI is a later product decision.

### Release profile

Use thin LTO, one codegen unit, and symbol stripping. This reduced the binary
from roughly 26 MiB to 18 MiB while keeping `opt-level = 3`. Retain unwind
panics because the core supervises task panics with `catch_unwind`; aborting
would break that contract. cargo-dist's `dist` profile inherits from release.

## Consequences

Users receive checked, cross-platform installers from a version tag without
project-specific release scripts, and the binary is roughly one third smaller.
The workflow can only be exercised fully in GitHub Actions after a new tag.
musl remains deferred, and the embedded remote-provider stack still accounts
for most of the binary. cargo-dist is a development/CI dependency only.

## Amendment 2026-08-07: the binary is `ntc`, and the release is built by hand

Two facts about this ADR are no longer true, and they are recorded here rather
than edited into the text above — the decision stands, its consequences moved.

**The installer URLs name `ntc`, not `norte-tui`.** The terminal binary was
renamed (`crates/norte-tui/Cargo.toml`; the crate is unchanged), and dist
derives the installer name from the binary. The URLs in the sections above are
the ones this ADR shipped with; the current ones are in `README.md`.

**"The workflow can only be exercised fully in GitHub Actions" is now the
problem, not a note.** Actions is off for billing, so the workflow this ADR
committed has never run: two tags produced no downloadable artefact. Releases
are built locally (`just dist`) and uploaded by hand (`just dist-publish`),
which means x86_64 Linux only. `dist-workspace.toml` keeps declaring all five
targets — it describes the release CI would produce, and the release notes say
which platforms are actually in the release. See
`docs/superpowers/specs/2026-08-07-packaging-design.md`.
