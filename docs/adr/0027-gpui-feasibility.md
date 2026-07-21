# 0027 - GPUI feasibility decision for the GUI

- Status: accepted
- Date: 2026-07-19
- Decision makers: Oscar González
- Related: specification section 18.3; ADR 0020; M5 feasibility spike

## Context

The specification preferred GPUI for a native GPU-backed Rust interface but
required measured validation before building a complete dual-pane frontend.
The isolated `norte-gui` spike had four criteria: list a real daemon directory,
run alongside the TUI on the same socket, apply `norte-theme` file styles, and
measure toolchain/build/runtime costs. The crate remained outside the main
workspace so unstable GPUI APIs could not break core CI.

## Options considered

### GPUI

GPUI is Zed's retained-mode Rust toolkit, pinned to a Zed Git revision because
there is no stable crates.io release. The tested revision uses wgpu 29.

- Shared Rust protocol and theme types cross directly into the frontend.
- Mapping shared true-colour values to `gpui::Rgba` is small and deterministic.
- The API changes frequently and the GPU/text/vector dependency tree is large.
- It requires a real graphical display.

### Tauri

Tauri offers a stable native-WebView shell and smaller binaries, but introduces
HTML/CSS/JavaScript, serialized process boundaries, CSS theme export, and
operating-system-dependent WebView performance. It remains the documented
fallback if GPUI maintenance becomes unsustainable.

## Measurements

Environment: Arch Linux kernel 7.1.3, KDE Plasma/KWin on Wayland, stable Rust
1.96.1, GPUI 0.2.2 at Zed revision
`f14fea9bf3c93797d5161f7440ed418655bc6c57` (2026-07-19).

| Metric | Result |
| --- | --- |
| Toolchain | Stable 1.96.1; no nightly toolchain required. |
| Clean debug build | 136 seconds. |
| Clean release build | About 376 seconds. |
| Release binary | 40,928,408 bytes (about 40 MiB). |
| Debug binary | About 803 MiB with full debug information. |
| Transitive crates | 757 unique crates. |
| Main heavy dependencies | wgpu/naga, cosmic-text, tiny-skia, usvg, and zed-font-kit. |
| Cold-cache startup | About 2.2 to 2.9 seconds. |
| Warm startup to first real-listing frame | About 0.35 seconds. |
| Linux system libraries | XCB, xkbcommon, Wayland/X11; a display is required. |

GPUI itself is Apache-2.0. Because the spike is excluded from the workspace,
its complete dependency license graph must enter cargo-deny when the GUI is
integrated.

## Criteria

1. **Real daemon listing: passed.** `RemoteBackend` connected without spawning
   the daemon and rendered real entries. Connection and path errors appeared in
   the window rather than panicking.
2. **Simultaneous TUI and GUI: passed.** A real TUI in a tmux PTY and the GPUI
   window remained connected to the same daemon socket and directory without
   interference.
3. **Shared themes: passed.** The GUI rendered the exact default-theme RGB
   values for directories, symlinks, and Rust source files through a tested
   colour conversion.
4. **Feasibility measurements: passed.** Stable Rust worked and none of the
   build, size, or startup results blocked development.

## Decision

Proceed with GPUI for the M5 graphical frontend.

Warm startup is comfortably below one second, a 40 MiB native GPU binary is
reasonable, and the long clean build remains isolated from core development.
The real protocol-to-GPU path, shared theme mapping, and simultaneous-client
model all worked. The Git pin and large dependency graph are accepted risks,
not release blockers.

## Consequences

- Begin the dual-pane GUI with navigation, protocol mutations, keymaps, viewer,
  Fluent localization, and AccessKit.
- Reuse the tested colour mapping and the separate Tokio-runtime-to-GPUI event
  pattern.
- Pin GPUI by SHA and revalidate API usage when updating it. Business logic and
  wire types remain isolated from toolkit churn.
- Keep the GUI outside the workspace until the MVP stabilizes. Integrating it
  requires cargo-deny review of the GPU dependency graph.
- Isolate state and mapping tests from rendering because graphical tests need a
  display.
- Retain Tauri as a reasoned fallback rather than an unexamined alternative.
