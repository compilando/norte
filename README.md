# norte

[![CI](https://github.com/compilando/norte/actions/workflows/ci.yml/badge.svg)](https://github.com/compilando/norte/actions/workflows/ci.yml)

norte is a next-generation orthodox file manager built around a headless Rust
core. The core provides a stable protocol and a provider-independent virtual
filesystem. Its TUI, GUI, and CLI are interchangeable clients, while AI agents
operate through the same governed interface with policy checks, journaling, and
auditing.

> The name is provisional (see Appendix A of the specification). The current
> release is **v0.3.0-alpha**. Local filesystems, SFTP, S3, ZIP/TAR archives,
> resumable copies, trash, and themes are available. The interface and
> configuration may still change during the alpha period.

## Install

On Linux and macOS, install the prebuilt binary from the latest release:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/compilando/norte/releases/latest/download/ntc-installer.sh | sh
```

On Windows, run this command in PowerShell:

```powershell
irm https://github.com/compilando/norte/releases/latest/download/ntc-installer.ps1 | iex
```

To install from source instead, run `make setup`, followed by:

```sh
cargo install --path crates/norte-cli --locked   # the `norte` command
cargo install --path crates/norte-tui --locked   # the `ntc` file manager
cargo install --path crates/norte-gui --locked   # graphical interface (optional, GPU)
```

## Run

```sh
norte tui              # terminal interface, in the current directory
norte tui ~/code       # ...in another directory
norte tui --preset vim # ...with a keymap preset (orthodox|vim|cua)
norte gui              # graphical interface
norte gui ~/code       # ...in another directory
```

`norte tui` and `norte gui` hand the process over to `ntc` and
`norte-gui`, which can also be launched directly — both take `[DIR]` and
`--socket` (`--help` lists the rest). They read the same configuration. The
TUI runs the core embedded unless `--daemon` says otherwise; the GUI always
talks to the daemon.

The `norte` command itself is the non-interactive side: `ls`, `cp`, `mv`,
`rm`, `mkdir`, `connect`, `daemon`, `mcp`, `policy`, `undo`, `index`, `ai`,
`audit`, `doctor`. Run `norte --help` for the full list.

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
