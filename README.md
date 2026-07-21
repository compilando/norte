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
  https://github.com/compilando/norte/releases/latest/download/norte-tui-installer.sh | sh
```

On Windows, run this command in PowerShell:

```powershell
irm https://github.com/compilando/norte/releases/latest/download/norte-tui-installer.ps1 | iex
```

Start the terminal interface with `norte-tui`.

To install from source instead, run `make setup`, followed by:

```sh
cargo install --path crates/norte-tui --locked
```

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
