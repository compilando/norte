# norte

[![CI](https://github.com/compilando/norte/actions/workflows/ci.yml/badge.svg)](https://github.com/compilando/norte/actions/workflows/ci.yml)

norte is a next-generation orthodox file manager built around a headless Rust
core. The core provides a stable protocol and a provider-independent virtual
filesystem. Its TUI and CLI are interchangeable clients, while AI agents
operate through the same governed interface with policy checks, journaling, and
auditing.

> The name is provisional (see Appendix A of the specification). The current
> release is **v0.3.0-alpha**. Local filesystems, SFTP, S3, ZIP/TAR archives,
> resumable copies, trash, and themes are available. The interface and
> configuration may still change during the alpha period.

## Install

The release ships two binaries as portable archives, each with its own
installer: `ntc`, the file manager, and `norte`, the command line tool that
runs the daemon, connections, policy, undo, the index and `doctor`. Install
both:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/compilando/norte/releases/latest/download/norte-tui-installer.sh | sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/compilando/norte/releases/latest/download/norte-cli-installer.sh | sh
```

> **The alpha carries x86_64 Linux only.** The release is built on a developer
> machine and uploaded by hand, because CI is off. The macOS and Windows
> targets are configured and will appear the day a release runs in CI; until
> then, build from source on those platforms.

To install from source, run `make setup`, followed by:

```sh
cargo install --path crates/norte-cli --locked   # the `norte` command
cargo install --path crates/norte-tui --locked   # the `ntc` file manager
```

There **is** a graphical interface: `norte-gui`, a Tauri window over the same
core, shipped as a `.deb`, `.rpm` or AppImage that carries the `norte` and
`ntc` binaries with it so a clean install has a daemon to talk to. It is a
supported frontend as of 2026-09-01 (ADR 0087): it has its own gate,
`just gui-ci`, which the repository's `pre-push` hook runs (`just hooks`) on
any change to the window or to the crates it is built on, and `just gui-smoke`
installs the built package in a clean container and starts it there. The GPUI
attempt it replaced was retired on 2026-08-20 (ADR 0065).

The gates run **locally**: `just ci` before merging, and the `pre-push` hook as
the floor. There are workflows under `.github/workflows/`, but GitHub Actions
is disabled on this repository, so nothing runs there — install the hook.

Supported does not mean finished. It is not yet exercised against screen
readers, IME input or fractional scaling (#261), and the packages are built on
a current glibc/WebKitGTK, so an older distribution needs a build from source.
The TUI remains the frontend with the most surface.

```sh
cargo install --path crates/norte-gui-tauri --locked   # the `norte-gui` window
```

Building it needs the system's WebKitGTK, GTK3 and libsoup3 development
packages; `make setup` does not install them, and neither does the portable
test gate — that is deliberate, so a machine without them can still build and
test everything else.

## Run

```sh
norte tui              # terminal interface, in the current directory
norte tui ~/code       # ...in another directory
norte tui --preset vim # ...with a keymap preset (orthodox|vim|cua)
```

`norte tui` hands the process over to `ntc`, which can also be launched
directly — it takes `[DIR]` and `--socket` (`--help` lists the rest). The TUI
runs the core embedded unless `--daemon` says otherwise.

The `norte` command itself is the non-interactive side: `ls`, `cp`, `mv`,
`rm`, `mkdir`, `connect`, `daemon`, `mcp`, `policy`, `undo`, `index`, `ai`,
`audit`, `doctor`. Run `norte --help` for the full list.

## Plugins

norte loads WASM plugins from `~/.config/norte/plugins/<id>/`. One ships with
the source tree — a syntax highlighter for the viewer:

```sh
just plugin-syntect
```

Installing is not consenting. The plugin arrives discovered and **unapproved**;
you approve its capabilities and enable it in the extensions manager. To install
any other plugin from a local directory holding a `plugin.toml` and a
`plugin.wasm`:

```sh
norte plugin install <dir>          # refuses to replace an installed id
norte plugin install <dir> --force  # replaces it, and withdraws its consent
```

`--force` withdraws consent on purpose: the approval digest covers the manifest,
not the `.wasm`, so replacing the binary under an identical manifest would
otherwise keep running new code under a permission granted to old code.

```sh
norte plugin list                   # id, category, approved, enabled, capabilities
norte plugin uninstall <id>         # removes it, and withdraws its consent
```

A `provider` plugin serves the URL scheme it declares (ADR 0093): once one
declaring `webdav` is installed, approved and enabled, `webdav://host:8443`
opens through it. `file`, `sftp`, `ftp` and `s3` stay with the core and cannot
be claimed. A plugin that declares `net` reaches exactly the connection's
`ip:port` — the URL's port, or the `default-port` its manifest declares.

Signing and a registry are deliberately absent — installing from a local path
needs neither, and both need decisions this project has not made yet.

## Documentation

- [Project documentation](docs/README.md)
- [Architecture overview](ARCHITECTURE.md)
- [Project specification](docs/spec/norte-spec.md)
- [Architecture decisions](docs/adr/README.md)
- [Theme configuration](docs/theming.md)
- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)

## Development

On a new machine, including one without Rust or `just`, set up the development
environment with:

```sh
make setup
```

The most common commands are:

```sh
just ci      # Run the same formatting, lint, test, and documentation checks as CI
just test    # Run the test suite with cargo-nextest
just cov     # Enforce the local 85% coverage threshold for proto, VFS, and core
make dev     # Run a debug build of the TUI (use `make run` for a release build)
```

`make` is a small wrapper around `just`; both use the same commands as CI. The
Rust version is pinned in `rust-toolchain.toml`. The minimum supported Rust
version is stable minus two releases and is checked in CI.

## Licensing

norte uses the same split-license model as Zed. The protocol, VFS providers,
test kit, and future plugin SDK are available under either MIT or Apache-2.0,
so third parties can build clients, providers, and plugins without adopting the
application license. The core and official frontends are AGPL-3.0-only. Each
crate contains its applicable license files.

## Telemetry

norte collects no telemetry, including opt-in telemetry. Diagnostics remain on
the local machine.
