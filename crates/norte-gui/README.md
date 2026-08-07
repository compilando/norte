# norte-gui: M5 feasibility spike

This GPUI binary renders a real directory supplied by the norte daemon and uses
colours from `norte-theme`. It was built as a feasibility spike for the M5 GUI,
not as a production frontend. ADR 0027 records the resulting decision.

The frontend follows the project's no-business-logic rule. It communicates only
through `norte-proto`, using `RemoteBackend` from `norte-core`. This version is
read-only (`fs.list`): it does not mutate files, open its own socket, or provide
a keymap.

## Workspace status

The root `Cargo.toml` lists this crate under `[workspace].exclude`, alongside
`examples-wasm`, rather than under `members`. As a result:

- `cargo build --workspace`, workspace Clippy and nextest runs, and `just ci`
  do not build it.
- The crate has its own `Cargo.lock`.
- Build and run it either from the repository root with `-p`, or from this
  directory:

  ```sh
  # From the repository root
  cargo build -p norte-gui
  cargo run -p norte-gui

  # Equivalent commands from this directory
  cd crates/norte-gui
  cargo build
  cargo run
  ```

## Toolchain and platform requirements

- The spike uses stable Rust 1.96.1, matching the root `rust-toolchain.toml`.
  The pinned GPUI revision does not require nightly Rust.
- GPUI is pinned to an exact Zed Git revision because no stable `gpui` crate is
  published on crates.io:

  ```toml
  gpui          = { git = "https://github.com/zed-industries/zed", rev = "f14fea9bf3c93797d5161f7440ed418655bc6c57" }
  gpui_platform = { git = "https://github.com/zed-industries/zed", rev = "f14fea9bf3c93797d5161f7440ed418655bc6c57", features = ["wayland", "x11"] }
  ```

  Revision `f14fea9bf3c93797d5161f7440ed418655bc6c57` is from the Zed `main`
  branch on 2026-07-19. GPUI is Apache-2.0 licensed. The `wayland` and `x11`
  features enable its native Linux backend. Header comments in `src/main.rs`
  and the crate's `Cargo.toml` contain the API-discovery notes for this pinned
  revision.
- Opening a window requires an X11 or Wayland display. The spike does not run
  on a headless Linux host without a compositor. It was verified under KDE
  Plasma with KWin/Wayland.

## Environment variables

`session::LoadConfig::from_env` in `src/session.rs` reads the daemon and
directory settings below.

| Variable | Purpose | Default |
| --- | --- | --- |
| `NORTE_SOCKET` | Path to the daemon's Unix-domain socket. | `$XDG_RUNTIME_DIR/norte/daemon.sock`, matching `norte daemon run` and the TUI. |
| `NORTE_DIR` | Directory to list in wire form, for example `file:///absolute/path`, including `%XX` escapes for non-ASCII bytes. | The process working directory, converted to a `file://` `VPath` in the same way as the TUI. |
| `NORTE_GUI_DEBUG` | When present, print the loaded entry count and each entry's name, kind, file kind, and foreground colour to stderr. | No debug output. |

The spike does not start the daemon automatically: it calls
`RemoteBackend::connect` with `spawn_cmd = None`. If no daemon is listening at
`NORTE_SOCKET`, the window still opens and displays the error instead of
panicking.

## Run the daemon, TUI, and GUI together

Use three terminals to reproduce the simultaneous-client check:

```sh
# 1. Build once, from the repository root
cargo build -p norte-cli -p norte-tui
(cd crates/norte-gui && cargo build)

# 2. Start the daemon and note the socket path
./target/debug/norte daemon run --socket /tmp/norte-demo/daemon.sock

# 3a. In a real terminal, start the TUI against that socket
cd /tmp/norte-demo
./target/debug/ntc --daemon --socket /tmp/norte-demo/daemon.sock

# A CLI client can be used when no TTY is available
./target/debug/norte --daemon --socket /tmp/norte-demo/daemon.sock ls /tmp/norte-demo

# 3b. In another terminal, start the GUI against the same socket and directory
NORTE_SOCKET=/tmp/norte-demo/daemon.sock \
NORTE_DIR=file:///tmp/norte-demo \
NORTE_GUI_DEBUG=1 \
cargo run -p norte-gui
```

The daemon supports multiple connections. The TUI or CLI and GUI can remain
connected to the same socket and view the same directory at the same time.

## Verification record

The M5 criterion 2 check was run under KDE Plasma/KWin on Wayland, using the
custom socket `/tmp/norte-t6-demo/daemon.sock` and a directory containing a
regular file, a symlink, a subdirectory, and a Rust source file.

1. The daemon created a mode-0600 Unix socket and accepted the first client.
2. A real TUI instance was run in a PTY supplied by tmux. Without a PTY, raw
   terminal initialization failed as expected. The TUI loaded and rendered the
   entries in both panes.
3. While the TUI connection remained open, the GUI connected to the same socket
   and loaded the same directory. Debug output confirmed node- and
   extension-specific colours:

   ```text
   [norte-gui] listado recibido del daemon: 8 entradas de file:///tmp/norte-t6-demo
   [norte-gui] 'main.rs' kind=File filekind=Regular fg=Some(Color { r: 215, g: 135, b: 95 })
   [norte-gui] 'enlace.lnk' kind=Symlink filekind=Symlink fg=Some(Color { r: 95, g: 175, b: 175 })
   [norte-gui] 'subdir' kind=Dir filekind=Dir fg=Some(Color { r: 95, g: 175, b: 215 })
   ```

   The debug strings above come from the application and are preserved as
   captured output. The eighth entry was the redirected GUI log, created after
   the original seven-entry fixture was counted.
4. A screenshot confirmed the GPUI window and the same colours reported in the
   log.
5. The daemon, TUI, and GUI remained alive, and the TUI connection stayed open
   until all three processes were stopped manually.

The result satisfies the simultaneous-client criterion: the TUI and GUI used
the same daemon and directory without interfering with each other.

## Scope

The spike covers real-directory listing, simultaneous clients, and file-type
theming. The measurement and go/no-go decision are recorded in
[ADR 0027](../../docs/adr/0027-gpui-feasibility.md).

Dual-pane navigation, mutations, configurable keymaps, theme hot reload, and
Windows/macOS support belonged to the following M5 milestone.
