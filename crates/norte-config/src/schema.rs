//! The strict, canonical schema of `norte.toml` (ADR 0035): every section,
//! `deny_unknown_fields`, compact hostile-safe diagnostics (#73).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

/// Default keymap preset (decision from 2026-07-10).
pub const DEFAULT_PRESET: &str = "orthodox";

/// General configuration from `norte.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NorteToml {
    /// Keymap settings.
    #[serde(default)]
    pub keymap: KeymapSection,
    /// User-interface settings.
    #[serde(default)]
    pub ui: UiSection,
    /// Daemon settings.
    #[serde(default)]
    pub daemon: DaemonSection,
    /// Archive-provider limits (`[archive]`, #95.2).
    #[serde(default)]
    pub archive: ArchiveSection,
    /// Local diagnostic log (`[log]`, roadmap item 9).
    #[serde(default)]
    pub log: LogSection,
    /// Favourite directories shown by `Ctrl+D`.
    ///
    /// Entries accumulate across layers instead of replacing lower-layer
    /// values. The project layer is excluded while [`crate::load::load`]
    /// merges the list. An absent value contributes no favourites from that
    /// layer.
    #[serde(default)]
    pub hotlist: Vec<HotlistEntry>,
    /// AI subsystem settings (`[ai]`, ADR 0031/0035). Honored from
    /// System+User layers only — never Project (fail-closed, same carve-out
    /// as `[archive]`).
    #[serde(default)]
    pub ai: AiSection,
    /// Profile settings (`[profile]`, spec 2026-08-26). Meaningful ONLY in
    /// `profiles/<name>/norte.toml`; any other layer ignores it with a
    /// warning.
    #[serde(default)]
    pub profile: ProfileSection,
}

/// `[profile]` — only meaningful in `profiles/<name>/norte.toml` (spec
/// 2026-08-26, D3). In any other layer it is ignored with a warning, so nobody
/// writes it into their own `norte.toml` and waits for something to happen.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct ProfileSection {
    /// Name to display. The profile's IDENTITY is its directory, not this:
    /// two profiles may share a title and still be two profiles.
    pub title: Option<String>,
    /// Where each slot opens when this profile has no saved state yet.
    ///
    /// Keys are slot ids of the profile's own layout, as text — TOML has no
    /// numeric keys. Values are `VPath`s in WIRE form (`file:///home/u/src`,
    /// `sftp://host/srv`), which is what [`crate::save_profile`] writes: a
    /// slot of a profile may sit on sftp or inside a container, and a native
    /// path cannot say so. It also removes the question of what a `~` or a
    /// relative path would resolve against — a profile is used across machines
    /// and across days, and "wherever you launched it from" is not an answer.
    ///
    /// Anything that does not parse — a key that is not a slot id, a value
    /// that is not a `VPath` — is dropped with a warning rather than refusing
    /// to start: the file is the reader's, but a typo does not earn a refusal
    /// to run. Those warnings reach the screen
    /// ([`crate::CommonConfig::profile_warnings`]); that is what keeps the
    /// strictness from being a trap.
    ///
    /// The SESSION wins over this. `[profile.start]` says where a slot opens
    /// the first time, not every time: a profile is a workspace, not a
    /// bookmark that drags you back to the start whenever you enter it.
    pub start: std::collections::BTreeMap<String, String>,
}

/// One `[[hotlist]]` entry as stored in `norte.toml`.
///
/// `path` contains an unvalidated wire value such as `scheme://...`, including
/// valid remote schemes. [`crate::load::load`] validates it as a `VPath`
/// while merging layers. One invalid entry is handled independently and does
/// not prevent the rest of `norte.toml` from loading.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HotlistEntry {
    /// Name displayed in the `Ctrl+D` popup.
    pub name: String,
    /// Path in wire form, not yet validated.
    pub path: String,
}

/// The `[archive]` section of `norte.toml` (#95.2): local anti-bomb limits
/// for browsing zip/tar/tar.gz containers. Absent values keep the compiled
/// defaults. Applied at startup on the embedded engine only — a container
/// that exceeds them fails with `LimitExceeded`, never silently truncates.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ArchiveSection {
    /// Maximum indexed entries per container (default 500000).
    #[serde(default)]
    pub max_entries: Option<u64>,
    /// Decompression budget in bytes for indexing a `tar.gz` (default 64 GiB).
    #[serde(default)]
    pub max_decompressed_bytes: Option<u64>,
    /// `[archive] max_nesting` (#56): tope de capas de archivo anidadas.
    pub max_nesting: Option<usize>,
    /// `[archive] rar_delegate` (roadmap ítem 11): ruta ABSOLUTA del programa
    /// externo que lee RAR (`7z`, `7zz` o `unrar`). Ausente = se sondea
    /// `PATH`. **Nunca se honra desde la capa Project**: una clave que nombra
    /// un ejecutable, leída de un `.norte.toml` dentro de un repositorio,
    /// sería ejecución de código arbitrario al entrar en el directorio.
    pub rar_delegate: Option<String>,
}

/// The `[daemon]` section of `norte.toml` (ADR 0011).
///
/// Transport mode is selected at startup and is not hot reloaded. Changing it
/// requires restarting the frontend.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DaemonSection {
    /// `embedded` for immediate startup (the default), or `daemon`.
    #[serde(default)]
    pub mode: Option<DaemonMode>,
    /// Daemon socket path. When absent, use the operating-system default.
    #[serde(default)]
    pub socket: Option<PathBuf>,
}

/// `[log]`: the local diagnostic log (roadmap item 9).
///
/// Never honoured from the project layer, same as [`DaemonSection`] and for the
/// same reason: deciding where a process writes is not presentation, and a
/// `norte.toml` arriving with somebody else's repository must not redirect it.
///
/// There is no level key on purpose. `RUST_LOG` already selects levels, and a
/// second mechanism for one setting is how the two drift apart.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LogSection {
    /// Where the rotated files go. Absent = `<state_dir>/logs`.
    #[serde(default)]
    pub dir: Option<PathBuf>,
    /// How many rotated files survive. Absent = the appender's own default.
    #[serde(default)]
    pub retain: Option<usize>,
    /// How the FILE is written (ADR 0127). Absent = `text`. stderr stays text
    /// whatever this says: it is read by a person in a terminal.
    #[serde(default)]
    pub format: Option<LogFormat>,
}

/// `[log] format`: one line per event, for a person or for a program.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    /// `tracing`'s readable line (the default).
    #[default]
    Text,
    /// One JSON object per line, with the event's fields and the spans it
    /// happened in (`spans`, outermost first), so a request and its tasks can
    /// be followed with `jq`.
    Json,
}

/// Core transport. This changes transport only, not behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DaemonMode {
    /// Core in-process (default).
    Embedded,
    /// Connect to the Unix-domain-socket daemon (Unix only; ADR 0011).
    Daemon,
}

/// The `[ui]` section of `norte.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UiSection {
    /// Language (`es` or `en`). When absent, negotiate from the environment.
    #[serde(default)]
    pub lang: Option<String>,
    /// Theme preset name or a path to a custom TOML theme (ADR 0020). When
    /// absent, use `default`.
    ///
    /// The bundled presets are `default`, `vscode-dark`, `vscode-light`,
    /// `catppuccin-mocha`, `catppuccin-latte`, `gruvbox-dark`,
    /// `gruvbox-light`, `nord`, `retro-crt` and `retro-crt-amber`.
    /// `norte_theme::preset_names()` is the source of truth; this list is a
    /// copy for readers of the schema, and it had gone stale before (it named
    /// six of the eight that existed).
    #[serde(default)]
    pub theme: Option<String>,
    /// The theme the WINDOW paints when the desktop prefers a light colour
    /// scheme (`prefers-color-scheme: light`): a preset name or a path, like
    /// `theme`. Absent = `theme` in both schemes. The terminal ignores it.
    #[serde(default)]
    pub theme_light: Option<String>,
    /// The theme the window paints when the desktop prefers a dark scheme;
    /// see `theme_light`.
    #[serde(default)]
    pub theme_dark: Option<String>,
    /// Quick-search mode for `/`: `"filter"` narrows the listing (the default),
    /// while `"jump"` moves the cursor without changing the listing.
    ///
    /// [`crate::load::load`] rejects other values so its diagnostic can
    /// include the source configuration path. Invalid values never silently
    /// fall back.
    #[serde(default)]
    pub quick_search: Option<String>,
    /// UI font family for GUI chrome (GP: `docs/superpowers/specs/2026-07-23-gui-visual-plugins-design.md`).
    /// When absent, use the platform default (bundled fallback).
    #[serde(default)]
    pub font: Option<String>,
    /// Monospace font family for listings/viewer (GP spec, same design doc).
    /// When absent, use the bundled monospace font.
    #[serde(default)]
    pub mono_font: Option<String>,
    /// Base UI font size in pixels (GP spec, same design doc). Valid range is
    /// `[8.0, 32.0]`; [`crate::load::load`] rejects values outside that range
    /// so its diagnostic can include the source configuration path — it is
    /// NOT clamped in silence.
    #[serde(default)]
    pub font_size: Option<f32>,
    /// Accessibility motion override (spec §17 a11y; GUI phase G2). `true`
    /// forces every animated GUI effect off — CRT flicker, cursor blink, and
    /// any future `with_animation` use — regardless of what a theme's
    /// `[effects]` section declares. `None`/absent leaves motion as the
    /// theme requests it; GPUI exposes no platform-level "prefers reduced
    /// motion" hint at this revision, so the effective default when absent
    /// is `false` (motion allowed), not an OS query. `Option` for the same
    /// absent-vs-explicit reason as `[ai] enabled`.
    #[serde(default)]
    pub reduce_motion: Option<bool>,
    /// Whether `app.quit` confirms before closing (S2): `"auto"` (default)
    /// confirms only with pending work (active tasks in the TUI's board,
    /// tasks/marks in the GUI — the behavior before this setting existed,
    /// preserved as the default rather than a fixed point in time);
    /// `"always"` always confirms, even with nothing pending; `"never"`
    /// closes immediately.
    ///
    /// [`crate::load::load`] rejects other values (same pattern as
    /// `quick_search`) so its diagnostic can include the source path.
    #[serde(default)]
    pub confirm_quit: Option<String>,
    /// Whether panes show hidden entries at startup (#107): unix dot-entries,
    /// decided on the raw bytes of the last path segment. Absent = `true`
    /// (show everything — the conservative default: nothing the provider
    /// lists silently disappears until the user asks). `pane.toggle-hidden`
    /// (Ctrl+H) flips it per pane at runtime; this key only seeds the
    /// initial state.
    #[serde(default)]
    pub show_hidden: Option<bool>,
    /// Qué disposición de paneles arranca. Ausente = `orthodox`, la de
    /// siempre: dos paneles al 50 %, la franja de tareas y la barra de estado.
    ///
    /// Cualquier otro nombre se busca en `layouts/<nombre>.toml` dentro del
    /// directorio de configuración. Un layout que no carga NO deja a norte sin
    /// pantalla: se avisa y se arranca con `orthodox`.
    #[serde(default)]
    pub layout: Option<String>,
    /// Whether the TUI captures the mouse (click, wheel, drag). Absent =
    /// `true` (captured).
    ///
    /// Capture is not free: while it is on, the TERMINAL stops seeing the
    /// button presses it uses for its own text selection, so
    /// select-and-middle-click-paste needs Shift held down in most
    /// terminals. `false` gives the terminal its mouse back and leaves norte
    /// keyboard-only. The GUI ignores this key — it has no terminal to
    /// share the pointer with.
    #[serde(default)]
    pub mouse: Option<bool>,
    /// Whether pressing and releasing Alt on its own opens the menu bar in
    /// the TUI. Absent = `false`.
    ///
    /// Off by default because the terminal can only report a lone modifier
    /// under the kitty keyboard protocol's "report all keys" mode, and in
    /// that mode the terminal sends keys, not text: a character composed with
    /// a dead key (`é`) or typed with `AltGr` (`@`, `#` on a Spanish layout)
    /// arrives as its base key, so `a@b` can become `a2b`. Only terminals
    /// that implement the protocol honour it (kitty, foot, `WezTerm`, Ghostty);
    /// tmux, xterm and VTE-based terminals cannot, and there it does
    /// nothing. The GUI ignores this key: a window always has the gesture.
    #[serde(default)]
    pub alt_menu: Option<bool>,
    /// Whether the menu bar is pinned to the top row. Absent = `true`.
    ///
    /// On by default because the menu was the only way to reach several
    /// commands and there was nothing on screen saying it existed: a reader
    /// who does not already know `Alt+M` cannot find what they cannot see.
    /// It costs one row, and `menu_bar = false` gives it back — the menu still
    /// opens with its key, drawn over the top row as it always was.
    ///
    /// **Both frontends honour it.** This used to say the GUI ignored the key;
    /// it reads it (`MenuView.bar`), and a comment that lies is what the next
    /// audit believes.
    #[serde(default)]
    pub menu_bar: Option<bool>,
    /// Whether the panel bar is pinned under the menu bar. Absent = `true`.
    ///
    /// On for the same reason the menu bar is: the side panels — places, tree,
    /// jobs, details, the log — were reachable by shortcut, by menu and by the
    /// palette, and all three require knowing the panel exists. Nothing on
    /// screen said so, which also means a panel contributed by a plugin was
    /// invisible to anyone who did not go looking.
    ///
    /// It costs one row, and `panel_bar = false` gives it back — every panel
    /// still opens by its own key and from the menu.
    ///
    /// **Both frontends honour it.** This used to say the GUI ignored the key;
    /// it reads it (`PanelBarView.bar`).
    #[serde(default)]
    pub panel_bar: Option<bool>,
    /// Whether every listing carries a `..` row at the top. Absent = `true`.
    ///
    /// The row an orthodox reader expects: the cursor lands on it and Enter
    /// goes up, which is muscle memory from every manager in the family.
    /// `parent_entry = false` gives the row back to the listing — going up
    /// is still Backspace, and its key never went anywhere.
    ///
    /// It is never an OPERAND: with the cursor on it nothing is selected, so
    /// a copy or a delete has nothing to act on rather than acting on the
    /// parent directory. That is enforced in the shared pane model, not in
    /// each frontend.
    #[serde(default)]
    pub parent_entry: Option<bool>,
    /// The editor `pane.edit` launches, as an argv TEMPLATE with the same
    /// field codes as `openers.toml` (`%f` the file, `%d` the pane's
    /// directory): `editor = ["zed", "%f"]`. Absent = `$VISUAL`, then
    /// `$EDITOR`, then the POSIX fallback, which is what norte did before
    /// this key existed.
    ///
    /// **Never honoured from the PROJECT layer**, and that is not a detail:
    /// this key names a program to execute, so a cloned repository could
    /// otherwise choose what runs when you press F4. Same fail-closed rule as
    /// `openers.toml` and `[daemon]`.
    #[serde(default)]
    pub editor: Option<Vec<String>>,
    /// Whether that editor opens a WINDOW of its own rather than taking over
    /// the terminal. Absent = `false`.
    ///
    /// A terminal editor needs norte to step aside and wait for it; a windowed
    /// one (Zed, VS Code without `--wait`) hands control straight back, and
    /// suspending for it leaves the reader staring at a blank terminal until
    /// they close a window somewhere else. Same fail-closed layering as
    /// [`Self::editor`].
    #[serde(default)]
    pub editor_detached: Option<bool>,
    /// `[ui] diff` (#312): the program that compares TWO files, as an argv
    /// template with the same field codes as `openers.toml` — `%F` expands to
    /// both paths, `%d` to the pane's directory: `diff = ["meld", "%F"]`.
    /// Absent = `diff -u`, which POSIX guarantees is there and whose output
    /// norte holds on screen until a key is pressed.
    ///
    /// **Never honoured from the PROJECT layer**, same fail-closed rule as
    /// [`Self::editor`] and for the same reason: it names a program to run.
    #[serde(default)]
    pub diff: Option<Vec<String>>,
    /// Whether that comparison tool opens a WINDOW of its own. Absent =
    /// `false`. Same meaning and same layering as [`Self::editor_detached`] —
    /// a graphical differ (Meld, Kompare) hands control straight back.
    #[serde(default)]
    pub diff_detached: Option<bool>,
    /// Whether the row of function keys (F1–F10 with what each one does on
    /// the current screen) stays pinned at the bottom. Absent = `true`.
    ///
    /// Derived from the effective keymap, never drawn by hand: rebinding F5
    /// changes the label, and a screen that binds nothing to F7 shows an
    /// empty cell there. Clicking a cell runs the command.
    #[serde(default)]
    pub key_bar: Option<bool>,
    /// How the panel bar names its buttons: `"names"` (default) paints the
    /// localized panel name with its access letter underlined; `"letters"`
    /// paints only the letter, the row's original form. Below 60 usable
    /// cells `names` falls back to letters on its own.
    ///
    /// [`crate::load::load`] rejects other values (same pattern as
    /// `quick_search`) so its diagnostic can include the source path.
    #[serde(default)]
    pub panel_bar_style: Option<String>,
    /// Where the panel bar sits: `"top"`, a row under the menu bar;
    /// `"left"`, an activity rail on the left edge; `"auto"` (default), each
    /// frontend's own answer — top in the terminal, left in the window.
    ///
    /// [`crate::load::load`] rejects other values.
    #[serde(default)]
    pub panel_bar_position: Option<String>,
    /// The window's title bar: `"native"` (default), the desktop's own;
    /// `"custom"`, none from the desktop, and the menu bar doubles as the
    /// title bar with its own minimize, maximize and close buttons, as in
    /// VS Code. Read at start-up. The terminal has no title bar and ignores
    /// it.
    ///
    /// [`crate::load::load`] rejects other values.
    #[serde(default)]
    pub titlebar: Option<String>,
    /// The items on the right half of the status bar, in screen order:
    /// any of `"position"`, `"marks"`, `"sort"`, `"encoding"`, `"tasks"`,
    /// `"notices"`, each at most once. Absent = all six in that order; an
    /// empty list leaves the right half empty. The left half — messages and
    /// warnings — is not configurable.
    ///
    /// [`crate::load::load`] rejects unknown and repeated ids.
    #[serde(default)]
    pub status_items: Option<Vec<String>>,
    /// Whether every listing carries a footer with its counts (directories,
    /// files, bytes), what is marked, and the free space of the volume the
    /// directory lives on. Absent = `true`.
    #[serde(default)]
    pub pane_footer: Option<bool>,
    /// Whether a listing paints its odd rows on a band of their own — the
    /// «pyjama» that makes a wide row easy to follow from its name to its
    /// date. Absent = `false`.
    ///
    /// It is off by default because the band is a READING aid whose worth
    /// depends on the pane being wide, and because the colour is the theme's:
    /// a theme that does not define the `stripe` role paints nothing, and a
    /// setting that looks broken on half the themes is worse than one the
    /// reader turns on. Every bundled preset defines it.
    ///
    /// The band never covers what MEANS something: the cursor, a marked row
    /// and the pointer are painted over it.
    #[serde(default)]
    pub row_stripes: Option<bool>,
    /// Default format of the `mtime` column when `[ui.columns]` does not fix
    /// one: `"smart"` (default — the time today, day and time this year,
    /// the date before that), `"relative"` (`11h ago`) or `"iso"`
    /// (`2026-09-10 14:02`). All three print LOCAL time.
    ///
    /// [`crate::load::load`] rejects other values (same pattern as
    /// `quick_search`) so its diagnostic can include the source path.
    #[serde(default)]
    pub date_format: Option<String>,
    /// Seconds a transient notice stays on the status line before it moves
    /// to the notice ring and only a badge remains. Absent = `8`; `0` keeps
    /// the notice until the next key, the behaviour before this key
    /// existed. Persistent banners (a degraded connection, a journal that
    /// cannot open) are state, not notices, and never expire.
    ///
    /// [`crate::load::load`] rejects values above 600.
    #[serde(default)]
    pub notice_seconds: Option<u32>,
    /// Whether a modal's key line is painted as buttons (`[ Enter  Confirm ]
    /// [ Esc  Cancel ]`, each clickable) instead of the plain
    /// `[enter] confirm · [esc] cancel` line. Absent = `true`.
    #[serde(default)]
    pub dialog_buttons: Option<bool>,
    /// How many directories each panel's navigation history keeps: the list
    /// `pane.history` shows and the trail `nav.back` walks. Absent = `30`.
    ///
    /// [`crate::load::load`] rejects values outside `5..=64`: 64 is what the
    /// saved session keeps per panel, so a larger number would be lost on
    /// restart without a word.
    #[serde(default)]
    pub history_size: Option<u32>,
    /// What the startup screen does: `"brief"` (default — a cover any key
    /// takes away, with the build and where you were), `"off"` (none) or
    /// `"home"` (a start screen that stays until a key, with recent and
    /// popular directories, bookmarks and profiles by number).
    ///
    /// [`crate::load::load`] rejects other values. The first-run wizard wins
    /// over it, and `--no-splash` or `NORTE_NO_SPLASH` turn it off for one run.
    #[serde(default)]
    pub splash: Option<String>,
    /// How long `splash = "brief"` covers the first frame, in milliseconds.
    /// Absent = `4000`.
    ///
    /// [`crate::load::load`] rejects values outside `200..=60_000`: below that
    /// the cover is a flash nobody can read, and above it you have `"home"`,
    /// which stays until a key instead of pretending to leave.
    #[serde(default)]
    pub splash_ms: Option<u32>,
    /// Whether the processes panel opens by itself: `"auto"` (default — it
    /// opens when a task starts and closes when the last one is gone) or
    /// `"manual"` (only the command and the panel bar move it). Opening or
    /// closing it by hand while a task runs wins until that task ends.
    ///
    /// [`crate::load::load`] rejects other values.
    #[serde(default)]
    pub processes_panel: Option<String>,
    /// `[ui] images`: how the TUI shows an image file in the viewer. Absent =
    /// `auto`.
    ///
    /// `auto` uses the terminal's graphics protocol when it has one and falls
    /// back to an approved `previewer` plugin otherwise; `kitty` and `blocks`
    /// force one of the two; `off` leaves the viewer on hexview. The GUI
    /// ignores this key: a window paints images by itself.
    #[serde(default)]
    pub images: Option<String>,
    /// The `/` in front of a directory row: `"auto"` (default — only when the
    /// icon column is closed, since an icon already says what the row is),
    /// `"slash"` (always) or `"none"`.
    ///
    /// [`crate::load::load`] rejects other values.
    #[serde(default)]
    pub dir_indicator: Option<String>,
    /// `[ui.columns]` (#108 block 4): column selection and sort order.
    #[serde(default)]
    pub columns: Option<UiColumnsSection>,
}

/// The `[ui.columns]` section (#108, columns design Layer 4). Column IDS are
/// raw strings here (an open set — `attr:`/`plugin:` forms exist before
/// their renderer does): the frontend parses them and `norte doctor`
/// reports the unusable ones. The SORT vocabulary is closed and validated
/// at load (same pattern as `quick_search`).
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UiColumnsSection {
    /// Column ids in paint order (`"name"`, `"size"`, `"mtime"`, `"kind"`,
    /// `"attr:<id>"`, `"plugin:<plugin>/<column>"`). Absent = the built-in
    /// default (`name`, `size`, `mtime`).
    #[serde(default)]
    pub default: Option<Vec<String>>,
    /// Global sort order. Absent = name/asc/dirs-first.
    #[serde(default)]
    pub sort: Option<SortSection>,
    /// Per-scheme overrides, keyed by scheme (`sftp`, `s3`…). An override
    /// REPLACES the column list — it never merges (design decision: merging
    /// makes "why is this column here?" unanswerable).
    #[serde(default)]
    pub scheme: Option<std::collections::BTreeMap<String, SchemeColumnsSection>>,
    /// `[[ui.columns.spec]]` entries (#108 block 7b): per-column
    /// presentation, keyed by `id`.
    #[serde(default)]
    pub spec: Option<Vec<ColumnSpecSection>>,
}

/// A sort choice inside `[ui.columns]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SortSection {
    /// `"name"` | `"size"` | `"mtime"` — closed, validated at load.
    #[serde(default)]
    pub column: Option<String>,
    /// `"asc"` | `"desc"` — closed, validated at load.
    #[serde(default)]
    pub dir: Option<String>,
    /// Directories first (default `true`).
    #[serde(default)]
    pub dirs_first: Option<bool>,
}

/// One scheme's override inside `[ui.columns.scheme.<scheme>]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SchemeColumnsSection {
    /// Column ids for panes on this scheme (replaces the default list).
    #[serde(default)]
    pub columns: Option<Vec<String>>,
    /// Sort for panes on this scheme.
    #[serde(default)]
    pub sort: Option<SortSection>,
    /// `[[ui.columns.scheme.<scheme>.spec]]` entries: per-column
    /// presentation for panes on this scheme; they win over the global
    /// `spec` entries at resolve.
    #[serde(default)]
    pub spec: Option<Vec<ColumnSpecSection>>,
}

/// One `[[ui.columns.spec]]` entry (#108 block 7b): per-column
/// presentation. Keyed by `id`; a scheme block may carry its own `spec`
/// entries that win for panes on that scheme.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ColumnSpecSection {
    /// Column id this entry styles (required).
    pub id: String,
    /// `"auto"` | `{ fixed = n }` | `{ min = n, weight = m }` — cells,
    /// validated to `[1, 64]` at load.
    #[serde(default)]
    pub width: Option<WidthSection>,
    /// `"left"` | `"right"` — closed, validated at load.
    #[serde(default)]
    pub align: Option<String>,
    /// `"exact"` | `"iec"` | `"si"` | `"relative"` | `"iso"` | `"smart"` |
    /// `"octal"` | `"rwx"` — closed, validated at load; whether it FITS the column is
    /// the frontend's call (doctor reports mismatches).
    #[serde(default)]
    pub format: Option<String>,
    /// Custom header label (free text; the frontend sanitizes and caps).
    #[serde(default)]
    pub header: Option<String>,
}

/// The `width` of a spec entry. NOTE: this enum is `untagged`, and serde
/// ignores `deny_unknown_fields` inside untagged struct-syntax variants —
/// an unknown key next to `fixed`/`min` is silently ignored, never an
/// error (pinned by `width_fixed_con_campo_extra_comportamiento_serde` in
/// `load.rs`). The vocabularies and ranges themselves ARE validated at
/// load.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum WidthSection {
    /// `"auto"` (any other string is a load error).
    Keyword(String),
    /// `{ fixed = n }`.
    Fixed {
        /// Cells.
        fixed: u16,
    },
    /// `{ min = n, weight = m }`.
    Flex {
        /// Floor in cells.
        min: u16,
        /// Share weight (0 = never grows).
        #[serde(default)]
        weight: u16,
    },
}

/// The `[keymap]` section of `norte.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct KeymapSection {
    /// Base preset (`orthodox`, `vim`, or `cua`). Inherit when absent.
    #[serde(default)]
    pub preset: Option<String>,
}

/// The `[ai]` section of `norte.toml` (ADR 0031). All off by default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AiSection {
    /// AI enabled. `None`/`false` = the gate rejects every operation.
    /// `Option` (not a plain `bool`) so layer merge distinguishes "absent"
    /// (inherit the lower layer's value) from "explicitly false".
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Local-only mode: reject remote providers (spec §9). `Option` for the
    /// same absent-vs-false reason as `enabled` above.
    #[serde(default)]
    pub local_only: Option<bool>,
    /// Prefixes whose content/names never leave the process. Wire strings;
    /// validated to `VPath` during merge ([`crate::load::load`]). Merge is
    /// a UNION across layers, not last-wins: a deny never disappears by
    /// adding a layer (ADR 0035).
    #[serde(default)]
    pub denied_prefixes: Vec<String>,
    /// Provider name used for AI rename. By-name, later-layer-wins merge
    /// (same as other `Option` scalars in this schema).
    #[serde(default)]
    pub rename_provider: Option<String>,
    /// Provider name used for embeddings (`index.embed` /
    /// `index.search_semantic`). Absent in every layer = no embeddings (the
    /// methods degrade to `Unsupported`). Later-layer-wins merge (same as
    /// other `Option` scalars in this schema).
    #[serde(default)]
    pub embed_provider: Option<String>,
    /// Declared providers (`[ai.providers.<name>]`). By-name, later-layer-wins
    /// merge: a provider redeclared in a higher layer replaces the lower
    /// layer's entry for that name, other names are untouched.
    #[serde(default)]
    pub providers: BTreeMap<String, AiProviderEntry>,
}

/// One `[ai.providers.<name>]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AiProviderEntry {
    /// `anthropic` | `ollama` | `openai-compat`. An invalid value is not
    /// rejected at parse time — same pattern as `[ui] quick_search` — the AI
    /// gate rejects it at use, where the diagnostic can name the provider.
    pub kind: String,
    /// Model id as the provider expects it.
    pub model: String,
    /// Base URL (required for `openai-compat`).
    #[serde(default)]
    pub base_url: Option<String>,
}

/// Error de carga de config. Siempre con el ARCHIVO en el diagnóstico.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// No se pudo leer un archivo que existe.
    #[error("no se pudo leer {}: {source}", path.display())]
    Io {
        /// El archivo.
        path: PathBuf,
        /// La causa.
        source: std::io::Error,
    },
    /// TOML inválido o con claves desconocidas.
    #[error("{}: {message}", path.display())]
    Toml {
        /// El archivo.
        path: PathBuf,
        /// Diagnóstico del parser (incluye campo y posición).
        message: String,
    },
}

/// Diagnóstico COMPACTO de un error de `toml`: posición + mensaje semántico.
/// El `Display` multilínea del crate cita ENTERA la línea del fichero —
/// contenido potencialmente hostil/kilométrico que además desplazaría lo
/// accionable («unknown field …», que va al final) fuera del tope de la
/// barra (#73).
///
/// Wired into [`crate::load::load`]'s error path.
pub(crate) fn toml_diag(raw: &str, e: &toml::de::Error) -> String {
    match e.span() {
        Some(s) => {
            let line = 1 + raw[..s.start.min(raw.len())].matches('\n').count();
            format!("line {line}: {}", e.message())
        }
        None => e.message().to_owned(),
    }
}

/// Lee un archivo si existe; `None` si no está (una capa ausente no es
/// error), `Err` si existe pero no se puede leer.
#[doc(hidden)]
pub fn read_optional(path: &std::path::Path) -> Result<Option<String>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The latent bug this task fixes: the strict struct must accept `[ai]`
    /// (ADR 0031 documents it in norte.toml; the old TUI struct rejected it).
    #[test]
    fn norte_toml_estricto_acepta_seccion_ai() {
        let doc = r#"
[ui]
theme = "nord"

[ai]
enabled = true
local_only = true
denied_prefixes = ["file:///secret"]
rename_provider = "local"

[ai.providers.local]
kind = "ollama"
model = "llama3"
"#;
        let parsed: NorteToml = toml::from_str(doc).expect("[ai] es sección canónica");
        assert_eq!(parsed.ai.enabled, Some(true));
        assert_eq!(parsed.ai.local_only, Some(true));
        assert_eq!(parsed.ai.denied_prefixes, vec!["file:///secret".to_owned()]);
        assert_eq!(parsed.ai.rename_provider, Some("local".to_owned()));
        assert_eq!(parsed.ai.providers.len(), 1);
        let provider = parsed.ai.providers.get("local").expect("declarado");
        assert_eq!(provider.kind, "ollama");
        assert_eq!(provider.model, "llama3");
    }

    /// `deny_unknown_fields` pinned on the NEW `[ai]` struct too: a typo in
    /// its top-level fields is a hard error, not silently ignored.
    #[test]
    fn ai_section_campo_desconocido_es_error() {
        assert!(toml::from_str::<NorteToml>("[ai]\nenabld = true\n").is_err());
    }

    /// `deny_unknown_fields` pinned on `[ai.providers.<name>]` too.
    #[test]
    fn ai_provider_entry_campo_desconocido_es_error() {
        let doc = r#"
[ai.providers.x]
kind = "ollama"
model = "m"
modle = "typo"
"#;
        assert!(toml::from_str::<NorteToml>(doc).is_err());
    }

    /// Strictness is uniform: a typo anywhere is a hard error.
    #[test]
    fn campo_desconocido_sigue_siendo_error() {
        assert!(toml::from_str::<NorteToml>("[ui]\ntheem = \"nord\"\n").is_err());
    }
}

#[cfg(test)]
mod toml_diag_tests {
    use super::*;

    /// #73: el diagnóstico compacto conserva posición + mensaje semántico y
    /// NO cita la línea del fichero — un TOML hostil puede meter valores
    /// kilométricos/bidi que desplazarían lo accionable fuera del tope de la
    /// barra (hallazgo MEDIA-1 del encoding-auditor).
    #[test]
    fn toml_diag_compacto_sin_citar_el_contenido() {
        let hostil = format!("v = \"{}\u{202E}\"\nbad", "x".repeat(300));
        let e = toml::from_str::<NorteToml>(&hostil).expect_err("no parsea");
        let d = toml_diag(&hostil, &e);
        assert!(!d.contains("xxx"), "no cita el contenido: {d}");
        assert!(!d.contains('\u{202E}'), "sin bidi: {d}");
        assert!(d.len() < 200, "compacto ({} bytes): {d}", d.len());
        assert!(d.contains("line "), "la posición sobrevive: {d}");
    }

    /// El span puede faltar (errores semánticos sin posición): mensaje solo.
    #[test]
    fn toml_diag_sin_span_no_panica() {
        let e = toml::from_str::<NorteToml>("keymap = 3").expect_err("no valida");
        let _ = toml_diag("keymap = 3", &e);
    }
}
