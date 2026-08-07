# Packaging and distribution — design

**Date:** 2026-08-07
**Issue:** #13 (cargo-semver-checks + protocol JSON Schema)
**Status:** approved

## The problem

`dist-workspace.toml` has configured cargo-dist since before the first tag: five
targets, shell and PowerShell installers, `.github/workflows/release.yml`
generated and committed. None of it has ever run. GitHub Actions is off for
billing, and dist's model is CI-driven — the workflow is what turns a tag into
artifacts. Two tags exist (`v0.3.0-alpha.1`, `v0.3.0-alpha.2`) and neither
produced a binary anyone can download.

So the gap is not "we need packaging". It is that the packaging we have is a
description of a release nobody can perform.

A second gap sits next to it: nobody is going to type `norte-tui` twice a day.
Norton Commander was `nc`; this one is `ntc`.

## Decisions

### 1. Artefacts are built locally, uploaded by hand

`dist build` on this machine, `gh release upload` to the tag. No CI.

That means **x86_64 Linux only**. macOS and Windows need those machines or a
cross toolchain, and ADR 0021 already recorded why cross-compiling `aws-lc-rs`
(russh/rustls) is not a road worth taking for a side effect of packaging.

`dist-workspace.toml` **keeps all five targets**. The config describes the
release the project should produce once CI exists; what we build today is a
subset of it. The release notes say which platforms are in the release rather
than the configuration quietly pretending the other three were never intended.

### 2. The terminal binary becomes `ntc`

| Binary | What it is |
| --- | --- |
| `ntc` | the file manager (today `norte-tui`) |
| `norte` | the command-line tool |
| `norte-gui` | the window, experimental |

The CRATE stays `norte-tui`. Renaming it would touch the manifests of five
dependents and the crates.io identity for no gain: nobody types a crate name.

`ntc` was checked before choosing it: nothing on this machine's `PATH`, no
Debian stable package shipping a file by that name, no `ntc` crate on
crates.io. What that check does NOT cover: AUR, Homebrew, npm, and whatever
aliases a user has in their own shell. It is evidence, not a guarantee.

Renaming now is cheap precisely because no binary has ever been published: the
only installation in the world is the author's.

The rename surface is found by `grep`, never from memory. Known so far:

- `crates/norte-tui/Cargo.toml`: the `[[bin]]` name.
- `crates/norte-cli/src/main.rs:493`: `exec_frontend("norte-tui", args)` — the
  CLI launches the frontend BY BINARY NAME, so `norte tui` breaks the moment
  the name changes and nothing in the type system says so.
- `justfile`: the `install`, `uninstall` and `tui` recipes.
- `README.md`: the installer URLs, which dist derives from the binary name.
- Documentation and the help corpus: the corpus does not name `norte-tui`
  today, but it does name `norte` in examples, and both must stay true.

### 3. `norte-gui` joins the workspace, outside `default-members`

`members` gains it, so cargo and dist see one workspace with one lockfile.
`default-members` excludes it, so `cargo build`, `cargo test` and `just ci`
behave exactly as they do today — which is what made the exclusion worth having.
`just gui-ci` stays as the GUI's own gate.

One consequence is not free and is not hidden: `cargo deny` walks `members`, so
the GPUI dependency tree enters the licence and advisory audit. The attempt is
to audit it honestly. If Zed's tree carries something this project does not
govern, the exception is written down with its reason and an expiry to revisit
— never a silent `skip`.

The GUI binary ships as experimental with its requirements stated (a Wayland or
X session with working Vulkan). A GPUI binary links against the graphics stack
of the machine that built it, and that is the honest limit of shipping one at
all.

### 4. What #13 adds to the release

- `cargo semver-checks` runs in `just release-check` for real. The recipe has
  asked for it since it was written; the tool is not installed, so the step has
  been reporting its own absence and continuing. Baseline: the latest tag.
- The protocol JSON Schemas (`docs/schema/proto.schema.json`, plus the config
  and keymap schemas beside it) are attached to the release. They are in the
  repository and the gate already compares them; publishing them is what lets a
  third party write a client without cloning.

## Out of scope, deliberately

Artefact signing, package managers (AUR, Homebrew), update notification, and
publishing to crates.io. All four are in spec §17 and none belongs here: the
first three need infrastructure decisions (where the key lives, who maintains
the formula) and the fourth is its own project.

The release carries the SHA-256 checksums dist already generates. A checksum is
not a signature and the release notes will not call it one.

## Testing

Packaging is mostly not unit-testable; what IS testable is pinned:

- **The rename cannot half-land.** A test asserts the CLI launches the frontend
  under the name the manifest actually builds, rather than a string that
  compiles fine and fails at `exec` time. This is the one defect in the rename
  with real user impact: `norte tui` silently stops working.
- **`just release-check` is the gate**, extended with semver-checks, and run
  before the tag.
- **The artefact is smoke-tested**: unpack the built archive in a temporary
  directory and run `ntc --version` and `norte --version` from it, so a release
  that cannot start is caught here rather than by the first person who
  downloads it.

## Order

1. `ntc` rename, with the CLI-launch test. Independent of everything else.
2. `norte-gui` into `members` (not `default-members`), with the `cargo deny`
   outcome recorded either way.
3. `just dist` / `just dist-publish` and the artefact smoke test.
4. `cargo semver-checks` in `release-check`; schemas attached to the release.
