**Downloads — Linux**

- `norte_{VERSION}_amd64.deb`, `norte-{VERSION}-1.x86_64.rpm`, `norte_{VERSION}_amd64.AppImage` — the window (`norte-gui`) together with the daemon (`norte`) and the terminal client (`ntc`), in one package.
- `norte-cli-x86_64-unknown-linux-gnu.tar.xz` (`norte`), `norte-tui-x86_64-unknown-linux-gnu.tar.xz` (`ntc`) and their `-installer.sh` — without the window.

**Downloads — Windows (preview)**

- `norte_{VERSION}_x64-setup.exe` — installer for the current user (no administrator): the window with `norte` and `ntc` beside it.
- `norte-{VERSION}-x86_64-pc-windows-msvc.zip` — the same three binaries, portable.

The Windows build is still a preview and **not signed**, so SmartScreen will ask before running it. It was built from this tag on a clean GitHub Windows runner.

**Requirements.** The Linux builds were made on Ubuntu 22.04 and need glibc 2.34 or newer; the window also needs WebKitGTK 4.1. Each Linux artefact was installed and started in clean containers of the distributions in `scripts/baseline/matrix.txt` (ADR 0112). Windows needs Windows 10/11 x64 with WebView2 (present on Windows 11).

**Checksums.** `SHA256SUMS` covers the Linux files and `SHA256SUMS-windows` the Windows ones; each Linux archive also has its own `.sha256`. Nothing is signed yet.
