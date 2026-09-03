# D1 — `org.norte.file-icons`: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the first demo plugin: a badge per row that says what kind of file
it is, from the name alone, in both frontends, with a `[config]` switch
between emoji and ASCII glyphs.

**Architecture:** a `decorator` guest (`norte-decorator` world). The host hands
it the basenames of the visible page (never paths, never kinds — a decorator
knows nothing about directories) and it answers one `Decoration` per entry,
positionally. The mapping is a data table in `icons.rs`, classified by
special name first (`Makefile`, `Cargo.toml`, `.gitignore`…) and extension
second (last dot, bytes, ASCII-lowercased; a dotfile with no other dot has
none — the same rule as `mark.extension` and the rename template). No
capabilities. `[config.style] enum emoji|ascii`, read through `host-config`.

**Tech Stack:** wit-bindgen 0.46 guest on `wasm32-wasip2`, std; host-side unit
tests on the table; an end-to-end test in `norte-core` over the real guest.

**Spec:** `docs/superpowers/specs/2026-09-03-plugin-kit-and-demo-plugins-design.md`, section D1.

## Global Constraints

- Names are bytes (rule 1): classify on bytes, never `String`; `from_utf8_lossy`
  only for logging.
- A badge is ≤ 8 characters after masking (`norte_frontend::decoration::BADGE_MAX_CHARS`);
  every glyph in the table is 1–2 characters.
- The guest is outside the workspace (`[workspace]`, own lockfile, `wit` symlink).
- `just t norte-core` in the loop; ONE `just ci-fast`; `just ci` at the close.

---

### Task 1: the table and the guest

**Files:** `plugins/file-icons/{Cargo.toml, plugin.toml, help.md, .gitignore, wit -> ../../crates/norte-plugin-host/wit, src/lib.rs, src/icons.rs}`

**Produces (icons.rs, pure, host-testable with `crate-type = ["cdylib","rlib"]`):**
```rust
pub enum Style { Emoji, Ascii }
pub fn badge_for(name: &[u8], style: Style) -> Option<&'static str>;
```
Classes and glyphs (emoji / ascii): code `💻`/`{}` (rs py js ts go c h cpp
java rb lua), rust `🦀`/`{}` (`.rs`, `Cargo.toml`, `Cargo.lock`), script
`⚡`/`$` (sh bash fish zsh ps1), doc `📄`/`''` (md txt rst pdf doc docx odt),
readme `📖`/`''` (`README`, `README.*`), image `🖼️`→ use `🖼`/`%` (png jpg
jpeg gif webp svg bmp), audio `🎵`/`~` (mp3 wav flac ogg m4a), video `🎬`/`>`
(mp4 mkv webm mov avi), archive `📦`/`[]` (zip tar gz tgz xz bz2 7z rar),
config `⚙`/`#` (toml yaml yml json ini conf cfg env), build `🔧`/`#`
(`Makefile`, `justfile`, `CMakeLists.txt`), git `🐙`/`#` (`.gitignore`,
`.gitattributes`, `.gitmodules`), container `🐳`/`#` (`Dockerfile`,
`docker-compose.yml`), licence `📜`/`''` (`LICENSE`, `LICENSE.*`, `COPYING`).
Anything else: `None`. No `role` in v1.

- [ ] **Step 1:** unit tests in `icons.rs` (host side): `badge_for(b"main.rs", Emoji) == Some("🦀")`,
  `b"Cargo.toml"` → `🦀` (special name wins over `.toml`), `b".bashrc"` → `None`
  (dotfile: no extension), `b"archive.tar.gz"` → `📦` (last dot), `b"PHOTO.JPG"` → `🖼`
  (case-insensitive), `b"caf\xff.md"` → `📄` (bytes, not text), `b"README"` → `📖`,
  `b"x"` → `None`; ASCII style yields `{}` for `main.rs`; every glyph in both
  tables has ≤ 2 chars (a loop over the table).
- [ ] **Step 2:** `cargo test --manifest-path plugins/file-icons/Cargo.toml` — RED.
- [ ] **Step 3:** implement `icons.rs`; `lib.rs` reads `host_config::get("style")`
  once per `decorate` (`"ascii"` → `Style::Ascii`, anything else → `Emoji`) and maps
  the batch.
- [ ] **Step 4:** GREEN on the host tests; `cargo build --release --target wasm32-wasip2` in the plugin dir.
- [ ] **Step 5:** commit `feat(plugins): file-icons, a badge per row from the name alone`.

### Task 2: the end-to-end test, the recipe, the docs

**Files:** `crates/norte-core/tests/decorator_icons_e2e.rs` (create), `justfile`
(`plugin-file-icons` + add to `plugins`), `plugins/README.md` (row), `CHANGELOG.md`.

- [ ] **Step 1: failing E2E** (pattern: `plugin_template_e2e.rs` for the build and
  `install`; `Backend::plugin_decorate`'s embedded arm for the call — instantiate
  through `PluginRuntime::instantiate_decorator`, `set_settings`, `decorate`):
  install, approve, enable; `decorate` over `[b"main.rs", b"README.md", b"song.mp3", b".bashrc", b"x"]`
  yields `[🦀, 📄, 🎵, None, None]` positionally; over every name of
  `norte_testkit::corpus::hostile_names()` every badge is `None` or ≤ 8 chars
  with no control character, and the vector is 1:1; with `config.toml`
  `style = "ascii"` written and the registry rediscovered, `main.rs` yields `{}`.
- [ ] **Step 2:** RED (the guest is not found until Task 1 lands — order the tasks; if
  Task 1 is done this is GREEN at once, which is fine: the table tests were RED first).
- [ ] **Step 3:** recipe, README row, CHANGELOG.
- [ ] **Step 4:** `just plugin-file-icons` on this machine; approve in the TUI; a
  listing shows the badges; `just gui-ci` not needed (no renderer change).
- [ ] **Step 5:** commit `feat(plugins): file-icons is installed by just and proven by the gate`.

### Task 3: close

- [ ] `just ci-fast` (ONE). `just ci` (ONE). Memory; merge with K2.
