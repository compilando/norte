//! The TUI's keymap: re-exports the shared engine from
//! [`norte_frontend::keymap`] and contributes the crossterm adapter, the
//! TUI's command list, and its factory presets. The engine itself (neutral
//! types, parsing, layer merging, resolution) lives in `norte-frontend`
//! (GUI-c T1/T2) — this module is a thin TUI-specific layer.
/// `ModKey`/`set_mod_key` are DELIBERATELY absent from this list (ADR 0043
/// decision 9): the TUI cannot observe ⌘ — crossterm does not deliver super
/// without `PushKeyboardEnhancementFlags`, which norte does not enable — so it
/// cannot honor any policy other than Ctrl, and therefore must not be able to
/// name it. An API that accepts a setting it is going to ignore is worse than
/// one that does not offer it.
pub use norte_frontend::keymap::{
    Availability, Chord, Count, Effective, KeyCode, KeymapError, KeymapFile, Mods, Rebind,
    RebindError, RebindSources, RebindWrite, Resolution, Resolver, Screen, UnbindOutcome,
    UnbindWrite, count_ignored_message, paint_chord, parse_chord, parse_keymap, rebind_check,
    rebind_dry_run, unavailable_message, unbind_dry_run,
};

use crossterm::event::{KeyCode as CtCode, KeyModifiers as CtMods};

/// Adapter: a crossterm event → a neutral [`Chord`]. On `Char` the character
/// already encodes shift (`Chord::new` discards it); everything else keeps
/// its mods. Returns `None` for keys the keymap does not model (e.g. `Media`,
/// `BackTab`, `CapsLock`): the caller must treat it as if the key bound
/// nothing (equivalent to [`Resolution::Reset`] — never a panic, never a key
/// "lost" silently, differently from before).
#[must_use]
pub fn chord_from_crossterm(mods: CtMods, code: CtCode) -> Option<Chord> {
    let neutral = match code {
        CtCode::Char(c) => KeyCode::Char(c),
        // Clamp to f1..=f12, like the GUI's adapter (#109): classic xterm
        // reports Shift+F1 as F13, and an F(n>12) matches no binding
        // (`parse_chord` rejects it) nor does its `Display` ("f13") re-parse
        // — better an unmodeled key than an unrepresentable chord.
        CtCode::F(n @ 1..=12) => KeyCode::F(n),
        CtCode::Enter => KeyCode::Enter,
        CtCode::Tab => KeyCode::Tab,
        CtCode::Esc => KeyCode::Esc,
        CtCode::Backspace => KeyCode::Backspace,
        CtCode::Up => KeyCode::Up,
        CtCode::Down => KeyCode::Down,
        CtCode::Left => KeyCode::Left,
        CtCode::Right => KeyCode::Right,
        CtCode::Home => KeyCode::Home,
        CtCode::End => KeyCode::End,
        CtCode::PageUp => KeyCode::PageUp,
        CtCode::PageDown => KeyCode::PageDown,
        CtCode::Insert => KeyCode::Insert,
        CtCode::Delete => KeyCode::Delete,
        _ => return None, // keys the keymap does not model
    };
    // Only ctrl/alt/shift: crossterm does not report super/meta without
    // PushKeyboardEnhancementFlags (not enabled).
    let m = Mods {
        ctrl: mods.contains(CtMods::CONTROL),
        alt: mods.contains(CtMods::ALT),
        shift: mods.contains(CtMods::SHIFT),
        // Can NEVER be anything else in the TUI. `CtMods::SUPER` exists in the
        // type, but the terminal only delivers it under
        // `PushKeyboardEnhancementFlags` (Kitty's keyboard protocol), which
        // norte does not enable: reading it here would always return `false`
        // and fake a capability that is not there. That is why `mod+` is
        // Ctrl in the TUI on ALL platforms, macOS included, and we say so
        // instead of promising a key the terminal is never going to deliver.
        cmd: false,
    };
    Some(Chord::new(m, neutral))
}

/// The inverse of [`chord_from_crossterm`]: a neutral [`Chord`] → the
/// crossterm event that would produce it. For SYNTHESIZING a key from a
/// button (spec 2026-09-10): a click on `[Enter] Confirm` is pressing Enter,
/// and it goes through `on_key` as if the terminal had delivered it. `None`
/// for what the TUI cannot deliver (`cmd`, which never arrives here).
#[must_use]
pub fn crossterm_from_chord(chord: Chord) -> Option<(CtMods, CtCode)> {
    let (m, code) = chord.parts();
    if m.cmd {
        return None;
    }
    let ct = match code {
        KeyCode::Char(c) => CtCode::Char(c),
        KeyCode::F(n) => CtCode::F(n),
        KeyCode::Enter => CtCode::Enter,
        KeyCode::Tab => CtCode::Tab,
        KeyCode::Esc => CtCode::Esc,
        KeyCode::Backspace => CtCode::Backspace,
        KeyCode::Up => CtCode::Up,
        KeyCode::Down => CtCode::Down,
        KeyCode::Left => CtCode::Left,
        KeyCode::Right => CtCode::Right,
        KeyCode::Home => CtCode::Home,
        KeyCode::End => CtCode::End,
        KeyCode::PageUp => CtCode::PageUp,
        KeyCode::PageDown => CtCode::PageDown,
        KeyCode::Insert => CtCode::Insert,
        KeyCode::Delete => CtCode::Delete,
    };
    let mut mods = CtMods::NONE;
    mods.set(CtMods::CONTROL, m.ctrl);
    mods.set(CtMods::ALT, m.alt);
    mods.set(CtMods::SHIFT, m.shift);
    Some((mods, ct))
}

/// The commands the TUI knows how to run — the ONE source every keymap is
/// validated against (the same names the palette and the wire will see, ADR
/// 0006).
/// A single source for the command vocabulary (#112): the macro emits
/// `COMMANDS` (the usual validation list, the same public surface) AND the
/// [`Command`] enum with one variant per name. `dispatch` (main.rs) matches
/// the enum with NO wildcard: a new command with no arm, or an arm with no
/// variant, is a COMPILE ERROR — the class of bug that motivated this
/// (`mark.pattern-*` in COMMANDS with no arm: a panic in debug, a silent
/// no-op in release) no longer exists at runtime.
macro_rules! commands {
    ($($name:literal => $variant:ident,)+) => {
        /// The commands the TUI knows how to run — the ONE source every
        /// keymap is validated against (the same names the palette and the
        /// wire will see, ADR 0006).
        pub const COMMANDS: &[&str] = &[$($name),+];

        /// `dispatch`'s vocabulary, typed (#112). Parsed ONCE at the
        /// boundary (resolver/palette -> [`Command::parse`]); from there on
        /// the compiler requires one arm per variant.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Command {
            $(
                #[doc = concat!("`", $name, "`")]
                $variant,
            )+
        }

        impl Command {
            /// Name -> variant. `None` = outside the vocabulary (the keymap
            /// validates it on load; `lua:`/`plugin:` are routed BEFORE
            /// this).
            #[must_use]
            pub fn parse(s: &str) -> Option<Self> {
                match s {
                    $($name => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

commands! {
    "app.quit" => AppQuit,
    "pane.switch" => PaneSwitch,
    "pane.tab-new" => TabNew,
    "pane.tab-close" => TabClose,
    "pane.tab-next" => TabNext,
    "pane.tab-prev" => TabPrev,
    "pane.tab-move-left" => TabMoveLeft,
    "pane.tab-move-right" => TabMoveRight,
    "pane.tab-goto-1" => TabGoto1,
    "pane.tab-goto-2" => TabGoto2,
    "pane.tab-goto-3" => TabGoto3,
    "pane.tab-goto-4" => TabGoto4,
    "pane.tab-goto-5" => TabGoto5,
    "pane.tab-goto-6" => TabGoto6,
    "pane.tab-goto-7" => TabGoto7,
    "pane.tab-goto-8" => TabGoto8,
    "pane.tab-goto-9" => TabGoto9,
    "layout.split-h" => LayoutSplitH,
    "layout.split-v" => LayoutSplitV,
    "layout.focus-next" => LayoutFocusNext,
    "layout.focus-prev" => LayoutFocusPrev,
    "layout.close-slot" => LayoutCloseSlot,
    "layout.grow" => LayoutGrow,
    "layout.shrink" => LayoutShrink,
    "layout.equalize" => LayoutEqualize,
    "layout.flip" => LayoutFlip,
    "layout.set-target" => LayoutSetTarget,
    "layout.places" => LayoutPlaces,
    "layout.preview" => LayoutPreview,
    "layout.processes" => LayoutProcesses,
    "layout.metadata" => LayoutMetadata,
    "layout.log" => LayoutLog,
    "layout.disk-map" => LayoutDiskMap,
    "layout.pick" => LayoutPick,
    "profile.pick" => ProfilePick,
    "profile.save-as" => ProfileSaveAs,
    "profile.next" => ProfileNext,
    "profile.prev" => ProfilePrev,
    "pane.mirror" => PaneMirror,
    "pane.sync-nav" => PaneSyncNav,
    "pane.mirror-target" => PaneMirrorTarget,
    "pane.pull" => PanePull,
    "pane.swap" => PaneSwap,
    "cursor.up" => CursorUp,
    "cursor.down" => CursorDown,
    "cursor.page-up" => CursorPageUp,
    "cursor.page-down" => CursorPageDown,
    "cursor.top" => CursorTop,
    "cursor.bottom" => CursorBottom,
    "nav.enter" => NavEnter,
    "nav.parent" => NavParent,
    "nav.back" => NavBack,
    "nav.forward" => NavForward,
    "nav.jump-back" => NavJumpBack,
    "nav.set-jump-point" => NavSetJumpPoint,
    "app.help" => AppHelp,
    "app.theme" => AppTheme,
    "app.extensions" => AppExtensions,
    "app.palette" => AppPalette,
    "app.goto" => AppGoto,
    "layout.timeline" => LayoutTimeline,
    "layout.terminal" => LayoutTerminal,
    "app.menu" => AppMenu,
    "app.settings" => AppSettings,
    "app.pick-accept" => AppPickAccept,
    "app.terminal" => AppTerminal,
    "app.handoff" => AppHandoff,
    "app.toggle-panels" => AppTogglePanels,
    "pane.command-line" => PaneCommandLine,
    "pane.ai-rename" => PaneAiRename,
    "pane.organize" => PaneOrganize,
    "pane.semantic-search" => PaneSemanticSearch,
    "pane.copy" => PaneCopy,
    "pane.move" => PaneMove,
    "pane.delete" => PaneDelete,
    "pane.delete-permanent" => PaneDeletePermanent,
    "pane.view" => PaneView,
    "pane.open" => PaneOpen,
    "task.cancel" => TaskCancel,
    "task.pause" => TaskPause,
    "task.retry" => TaskRetry,
    "task.queue" => TaskQueue,
    "task.up" => TaskUp,
    "task.down" => TaskDown,
    "viewer.close" => ViewerClose,
    "viewer.up" => ViewerUp,
    "viewer.down" => ViewerDown,
    "viewer.page-up" => ViewerPageUp,
    "viewer.page-down" => ViewerPageDown,
    "viewer.top" => ViewerTop,
    "viewer.bottom" => ViewerBottom,
    "viewer.left" => ViewerLeft,
    "viewer.right" => ViewerRight,
    "viewer.encoding" => ViewerEncoding,
    "viewer.encoding-auto" => ViewerEncodingAuto,
    "viewer.hex" => ViewerHex,
    "viewer.zoom-in" => ViewerZoomIn,
    "viewer.zoom-out" => ViewerZoomOut,
    "viewer.zoom-fit" => ViewerZoomFit,
    "viewer.next" => ViewerNext,
    "viewer.prev" => ViewerPrev,
    "pane.quick-search" => PaneQuickSearch,
    "pane.history" => PaneHistory,
    "pane.hotlist" => PaneHotlist,
    "pane.popular" => PanePopular,
    "pane.history-left" => PaneHistoryLeft,
    "pane.history-right" => PaneHistoryRight,
    "pane.select-drive" => PaneSelectDrive,
    "pane.select-drive-left" => PaneSelectDriveLeft,
    "pane.select-drive-right" => PaneSelectDriveRight,
    "pane.compare-dirs" => PaneCompareDirs,
    "pane.compare-files" => PaneCompareFiles,
    "pane.sync-dirs" => PaneSyncDirs,
    "pane.search" => PaneSearch,
    "pane.names-encoding" => PaneNamesEncoding,
    "pane.toggle-hidden" => PaneToggleHidden,
    "pane.columns" => PaneColumns,
    "pane.sort-name" => PaneSortName,
    "pane.sort-ext" => PaneSortExt,
    "pane.sort-size" => PaneSortSize,
    "pane.sort-time" => PaneSortTime,
    "pane.sort-menu" => PaneSortMenu,
    "pane.properties" => PaneProperties,
    "pane.chmod" => PaneChmod,
    "pane.edit" => PaneEdit,
    "pane.tree" => PaneTree,
    "pane.connect" => PaneConnect,
    "pane.disconnect" => PaneDisconnect,
    "pane.edit-new" => PaneEditNew,
    "pane.dir-size" => PaneDirSize,
    "pane.checksum" => PaneChecksum,
    "pane.checksum-verify" => PaneChecksumVerify,
    "pane.pack" => PanePack,
    "pane.unpack" => PaneUnpack,
    "pane.test-archive" => PaneTestArchive,
    "pane.split-file" => PaneSplitFile,
    "pane.combine-files" => PaneCombineFiles,
    "pane.mkdir" => PaneMkdir,
    "pane.rename" => PaneRename,
    "pane.rename-batch" => PaneRenameBatch,
    "pane.refresh" => PaneRefresh,
    "pane.copy-path" => PaneCopyPath,
    "mark.toggle" => MarkToggle,
    "mark.all" => MarkAll,
    "mark.invert" => MarkInvert,
    "mark.clear" => MarkClear,
    "mark.pattern-add" => MarkPatternAdd,
    "mark.pattern-remove" => MarkPatternRemove,
    "mark.extension-add" => MarkExtensionAdd,
    "mark.extension-remove" => MarkExtensionRemove,
    "mark.files" => MarkFiles,
    "mark.dirs" => MarkDirs,
    "mark.restore" => MarkRestore,
    "mark.toggle-up" => MarkToggleUp,
    "mark.toggle-page-down" => MarkTogglePageDown,
    "mark.toggle-page-up" => MarkTogglePageUp,
    "mark.to-top" => MarkToTop,
    "mark.to-bottom" => MarkToBottom,
}

/// The `dialog` context's commands (H1, issue #24) — the CLOSED list the
/// TUI passes to [`Effective::build_for`] for `Screen::Dialog`. Each overlay
/// (modal, theme picker, extensions, nav popup) declares in code its own
/// ALLOWLIST of which ones it supports (`app::dialog_action` and the ad hoc
/// resolutions in `main.rs`); the security semantics live there, never here.
/// Matches 1:1 the `[dialog]` sections of the three shared presets
/// (`orthodox`/`vim`/`cua`) — a new command in the preset with no entry here
/// fails to build with `UnknownCommand`.
pub const DIALOG_COMMANDS: &[&str] = &[
    "dialog.confirm",
    "dialog.cancel",
    "dialog.approve",
    "dialog.deny",
    "dialog.overwrite",
    "dialog.skip",
    "dialog.rename",
    "dialog.newer",
    "dialog.up",
    "dialog.down",
    "dialog.page-up",
    "dialog.page-down",
    "dialog.top",
    "dialog.bottom",
    "dialog.section-prev",
    "dialog.section-next",
    "dialog.add",
    "dialog.toggle-enabled",
    "dialog.remove",
    "dialog.move-up",
    "dialog.move-down",
    "dialog.sort",
    "dialog.cycle-format",
    // H3b — verbs the help overlay adds. They are `dialog.*` and not
    // `app.*` because they only mean anything inside an overlay: "the other
    // pane of this overlay", "the page I came from", "start filtering this
    // list". Documented in the `help` topic, so the documentation gate is
    // paid in the same change that introduces them.
    "dialog.pane",
    "dialog.back",
    "dialog.filter",
    // The history lists (spec 2026-09-15 D2).
    "dialog.confirm-other",
    "dialog.clear",
];

/// Fluent id with a command's description (`app.quit` → `help-cmd-app-quit`).
/// The suite REQUIRES it to exist in both locales for EVERY command in
/// [`COMMANDS`]: a new command with no description breaks tests — help must
/// not fall behind.
#[must_use]
pub fn help_id(command: &str) -> String {
    format!("help-cmd-{}", command.replace('.', "-"))
}

/// Fluent id with a `dialog.*` command's SHORT label (`dialog.page-up` →
/// `dialog-cmd-page-up`), used by the generated footer hints (H1 T3, #24).
/// Same mangling as [`help_id`] (dots→dashes) applied to the SUFFIX after
/// `dialog.` — the prefix is not repeated in the id (avoids
/// `dialog-cmd-dialog-page-up`). The suite REQUIRES it to exist in both
/// locales for EVERY command in [`DIALOG_COMMANDS`].
#[must_use]
pub fn dialog_hint_id(command: &str) -> String {
    let suffix = command.strip_prefix("dialog.").unwrap_or(command);
    format!("dialog-cmd-{}", suffix.replace('.', "-"))
}

/// The factory presets, parsed (validated by the tests and when building the
/// effective keymap). The product's default: `orthodox` (decision
/// 2026-07-10).
///
/// # Panics
/// Never, with the embedded TOMLs (the suite validates them).
#[must_use]
pub fn presets() -> Vec<(&'static str, KeymapFile)> {
    use norte_frontend::keymap::presets as shared;

    shared::NAMES
        .iter()
        .map(|&name| {
            let src = shared::source(name).expect("NAMES resolves in source()");
            (
                name,
                parse_keymap(src).unwrap_or_else(|e| panic!("invalid embedded preset {name}: {e}")),
            )
        })
        .collect()
}

/// The status bar's "pending" segment (`[… ]` in `draw_status`): the count
/// typed so far (K2a) followed by the chords already pressed. The two travel
/// TOGETHER because they coexist in `12gg` — the `12` stays alive while the
/// `g g` sequence is typed, and painting only one of the two lies about what
/// is going to happen when the next key is released. A count that is not
/// shown is a count that cannot be cancelled.
///
/// Empty when there is neither a count nor a sequence: the bar stays silent.
///
/// K3a: the composition lives in `norte_frontend::whichkey::pending_title`,
/// which is also the which-key panel's TITLE. The two surfaces paint the SAME
/// state one line apart, so two different strings would be two spellings of
/// one thing (`f5 g` below, `F5 g` above) — and only one of them went through
/// `paint_chord`, which is what MASKS: a project `keymap.toml` carries no
/// trust and can bind an RLO or a BEL, and this string gets painted on a
/// terminal.
#[must_use]
pub fn pending_display(resolver: &Resolver) -> String {
    norte_frontend::whichkey::pending_title(resolver.pending(), resolver.count())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// K2a: the bar paints the count WHILE it is typed, and keeps painting it
    /// with a half-done sequence on top (`12` + `g`). With no live count the
    /// segment is the usual one; with nothing, it stays silent.
    #[test]
    fn the_pending_segment_paints_count_and_sequence() {
        let preset = parse_keymap(
            r"
counts = true

[pane]
keymap = [ { on = ['g', 'g'], run = 'cursor.top' } ]
",
        )
        .expect("test preset");
        let eff = Effective::build_for(&preset, &[], &["cursor.top"], Screen::Browse)
            .expect("test effective");
        let mut r = Resolver::new(eff);
        assert_eq!(pending_display(&r), "", "with nothing, it stays silent");
        assert!(matches!(
            r.push(parse_chord("1").expect("chord")),
            Resolution::Counting(1)
        ));
        assert!(matches!(
            r.push(parse_chord("2").expect("chord")),
            Resolution::Counting(12)
        ));
        assert_eq!(
            pending_display(&r),
            "12",
            "the count is visible as it is typed"
        );
        assert!(matches!(
            r.push(parse_chord("g").expect("chord")),
            Resolution::Pending(1)
        ));
        assert_eq!(
            pending_display(&r),
            "12 g",
            "the count SURVIVES the half-done sequence"
        );
    }

    /// With no counts in the preset the segment is exactly what it was before
    /// K2a: the chords joined by a space, with no invented numeric prefix.
    #[test]
    fn with_no_counts_the_segment_is_just_the_sequence() {
        let preset = parse_keymap(
            r"
[pane]
keymap = [ { on = ['g', 'g'], run = 'cursor.top' } ]
",
        )
        .expect("test preset");
        let eff = Effective::build_for(&preset, &[], &["cursor.top"], Screen::Browse)
            .expect("test effective");
        let mut r = Resolver::new(eff);
        assert!(matches!(
            r.push(parse_chord("g").expect("chord")),
            Resolution::Pending(1)
        ));
        assert_eq!(pending_display(&r), "g");
    }

    /// #112: `COMMANDS` and `Command` are born from the SAME macro — every
    /// name parses to its variant. Trivial by construction; pins against a
    /// future hand-edit of the list outside the macro.
    #[test]
    fn every_commands_name_parses_to_a_variant() {
        for name in COMMANDS {
            assert!(Command::parse(name).is_some(), "{name}");
        }
        assert!(Command::parse("does.not.exist").is_none());
        assert!(
            Command::parse("plugin:x:y").is_none(),
            "plugin: is routed separately"
        );
    }

    /// The TUI's `COMMANDS`/`DIALOG_COMMANDS` are now a SUBSET declaration, not a
    /// vocabulary. A name the shared catalogue has never heard of means the two
    /// have drifted — which is the whole class of bug this catalogue removes.
    #[test]
    fn every_tui_command_is_in_the_shared_catalogue() {
        use norte_frontend::keymap::catalogue::{Status, lookup};
        for name in COMMANDS.iter().chain(DIALOG_COMMANDS.iter()) {
            let def = lookup(name)
                .unwrap_or_else(|| panic!("{name} is implemented by the TUI and not in CATALOGUE"));
            assert_eq!(
                def.status,
                Status::Live,
                "{name} is implemented by the TUI but the catalogue declares it Planned"
            );
        }
    }

    /// The tmux harness saw NOTHING on pressing `Shift+F2` over the default
    /// preset (nor on pressing `Shift+F6`, which has been bound to
    /// `pane.rename` for a long time), while the SAME command on a bare `f2`
    /// responded instantly. This pins the two halves that ARE on this side,
    /// so the next time someone looks at it they do not have to rule them out
    /// again:
    ///
    /// 1. the adapter KEEPS shift on a function key (it only discards it on
    ///    `Char`, where the character already encodes it), and
    /// 2. the chord it produces is byte for byte what `"shift+f2"` parses to,
    ///    which is what the preset binds.
    ///
    /// What is left out — whether the terminal sends the sequence and whether
    /// crossterm decodes it as `SHIFT + F(2)` — cannot be asserted without a
    /// terminal, and that is where the evidence points.
    #[test]
    fn a_shifted_function_key_keeps_its_shift() {
        let from_terminal = chord_from_crossterm(CtMods::SHIFT, CtCode::F(2));
        assert_eq!(
            from_terminal,
            parse_chord("shift+f2").ok(),
            "the adapter and the preset's parser must agree"
        );
        let (mods, code) = from_terminal.expect("chord").parts();
        assert!(mods.shift, "shift must not be lost on a function key");
        assert_eq!(code, KeyCode::F(2));
    }

    #[test]
    fn chord_from_crossterm_translates_known_keys() {
        assert_eq!(
            chord_from_crossterm(CtMods::CONTROL, CtCode::Char('k')),
            Some(Chord::new(
                Mods {
                    ctrl: true,
                    ..Default::default()
                },
                KeyCode::Char('k')
            ))
        );
        assert_eq!(
            chord_from_crossterm(CtMods::NONE, CtCode::F(5)),
            Some(Chord::new(Mods::default(), KeyCode::F(5)))
        );
    }

    #[test]
    fn chord_from_crossterm_returns_none_for_unmodeled_keys() {
        assert_eq!(chord_from_crossterm(CtMods::NONE, CtCode::BackTab), None);
        assert_eq!(chord_from_crossterm(CtMods::NONE, CtCode::CapsLock), None);
    }

    /// #109: classic xterm reports Shift+F1 as `F13`, so an `F(13)` is
    /// constructible AT RUNTIME from this adapter — but `parse_chord` only
    /// accepts `f1..=f12`, so the chord cannot match any binding and its
    /// `Display` (`"f13"`) does not re-parse. Same clamp as the GUI's
    /// adapter: out of range = unmodeled key, `None`.
    #[test]
    fn chord_from_crossterm_clamps_f13_and_above_as_unmodeled() {
        assert_eq!(chord_from_crossterm(CtMods::NONE, CtCode::F(13)), None);
        assert_eq!(chord_from_crossterm(CtMods::NONE, CtCode::F(0)), None);
        assert_eq!(chord_from_crossterm(CtMods::NONE, CtCode::F(255)), None);
        assert_eq!(
            chord_from_crossterm(CtMods::NONE, CtCode::F(12)),
            Some(Chord::new(Mods::default(), KeyCode::F(12)))
        );
    }

    #[test]
    fn the_three_factory_presets_parse() {
        for (name, _preset) in presets() {
            assert!(!name.is_empty());
        }
    }

    /// K2b Task 4, check 1 — HALF A of two. Every bundled preset
    /// (`presets()`, driven by the shared `presets::NAMES`, not a hardcoded
    /// three) builds for all three `Screen`s against the TUI's OWN
    /// vocabulary: `COMMANDS` for Browse/Viewer, `COMMANDS` ∪
    /// `DIALOG_COMMANDS` for Dialog — the same union `main.rs` passes at
    /// startup (`Effective::build_for(preset, &cfg.keymap_layers,
    /// &dialog_known, Screen::Dialog)`).
    ///
    /// Split across two crates on purpose: `norte-frontend` (where
    /// `presets::NAMES` and the shared catalogue live) cannot see EITHER
    /// frontend's `COMMANDS` list — it is upstream of both — so only a
    /// frontend that owns a list can check a preset against it. This is
    /// HALF A. Half B belonged to the GPUI frontend, retired on 2026-08-20
    /// (ADR 0065): the graphical frontend that replaces it owes this repo the
    /// same test against ITS command list, and until it exists no preset is
    /// checked against a graphical surface at all.
    #[test]
    fn every_preset_builds_the_tuis_three_screens() {
        let dialog_known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect();
        for (name, preset) in presets() {
            for screen in [Screen::Browse, Screen::Viewer, Screen::Dialog] {
                let known: &[&str] = if screen == Screen::Dialog {
                    &dialog_known
                } else {
                    COMMANDS
                };
                Effective::build_for(&preset, &[], known, screen)
                    .unwrap_or_else(|e| panic!("preset {name} on {screen:?}: {e}"));
            }
        }
    }

    /// Decision #23 pinned: in the `cua` preset, Ctrl+C QUITS (universal
    /// emergency) — never copy. `pane.copy` stays on F5. Binding Ctrl+C to
    /// copy would diverge from the hardcoded Ctrl-C that aborts cd/refresh
    /// (transient loops that do not consult the keymap). This test fixes the
    /// decision: whoever tries to rebind Ctrl+C to copy breaks here and sees
    /// why.
    #[test]
    fn cua_ctrl_c_quits_not_copies() {
        let cua = presets()
            .into_iter()
            .find(|(n, _)| *n == "cua")
            .expect("cua preset")
            .1;
        let eff = Effective::build_for(&cua, &[], COMMANDS, Screen::Browse).expect("cua effective");
        let mut r = Resolver::new(eff);
        let ctrl_c = Chord::new(
            Mods {
                ctrl: true,
                ..Default::default()
            },
            KeyCode::Char('c'),
        );
        assert_eq!(
            r.push(ctrl_c),
            Resolution::Run {
                command: "app.quit".to_owned(),
                count: Count::None
            }
        );
        // Copy lives on F5, not on a Ctrl chord.
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::F(5))),
            Resolution::Run {
                command: "pane.copy".to_owned(),
                count: Count::None
            }
        );
    }

    /// H1 T2: the three presets build `Screen::Dialog` with the UNION
    /// `known_commands` (`COMMANDS` ∪ `DIALOG_COMMANDS` — `build_for_impl`
    /// validates the WHOLE merged effective, including `[global]`, against
    /// the list the caller passes it; T1 confirmed it). `y` resolves
    /// `dialog.approve` in all three (identical preset).
    #[test]
    fn dialog_commands_resolve_in_the_three_presets() {
        let known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect();
        for (name, preset) in presets() {
            let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog)
                .unwrap_or_else(|e| panic!("preset {name}: {e}"));
            let mut r = Resolver::new(eff);
            assert_eq!(
                r.push(Chord::new(Mods::default(), KeyCode::Char('y'))),
                Resolution::Run {
                    command: "dialog.approve".to_owned(),
                    count: Count::None
                },
                "preset {name}"
            );
        }
    }

    #[test]
    fn help_id_replaces_dots_with_dashes() {
        assert_eq!(help_id("app.quit"), "help-cmd-app-quit");
        assert_eq!(
            help_id("pane.delete-permanent"),
            "help-cmd-pane-delete-permanent"
        );
    }

    #[test]
    fn dialog_hint_id_strips_the_dialog_dot_prefix() {
        assert_eq!(dialog_hint_id("dialog.confirm"), "dialog-cmd-confirm");
        assert_eq!(dialog_hint_id("dialog.page-up"), "dialog-cmd-page-up");
        assert_eq!(
            dialog_hint_id("dialog.toggle-enabled"),
            "dialog-cmd-toggle-enabled"
        );
    }

    /// #108 7a: the columns picker's keys resolve in the THREE presets
    /// THROUGH the real crossterm adapter — pins the chord decisions verified
    /// in the plan:
    /// - `shift+up`/`shift+down` → move-up/move-down: `parse_chord` KEEPS
    ///   shift on non-Char keys and `chord_from_crossterm` does too, so the
    ///   preset's chord matches the SHIFT+arrow event.
    /// - `K`/`J` → move-up/move-down: crossterm delivers `Char('J')`+SHIFT
    ///   and `Chord::new` DISCARDS shift on Char — matches the `"J"` binding.
    ///   (`shift+j` as text does NOT parse: `ShiftWithChar`.)
    /// - `ctrl+s` → dialog.sort: a bare `s` is already `dialog.skip`
    ///   (collision modal clash) — the plan's fallback.
    #[test]
    fn columns_picker_chords_resolve_via_the_crossterm_adapter() {
        let expected = [
            ((CtMods::SHIFT, CtCode::Up), "dialog.move-up"),
            ((CtMods::SHIFT, CtCode::Down), "dialog.move-down"),
            ((CtMods::SHIFT, CtCode::Char('K')), "dialog.move-up"),
            ((CtMods::SHIFT, CtCode::Char('J')), "dialog.move-down"),
            ((CtMods::CONTROL, CtCode::Char('s')), "dialog.sort"),
            // #108 7b: `f` cycles the format (free in all three [dialog]s).
            ((CtMods::NONE, CtCode::Char('f')), "dialog.cycle-format"),
        ];
        let known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect();
        // K2b: `presets()` now also brings `total-commander`/`krusader`,
        // which do NOT share `alt+c` → `pane.columns` (it is not in their
        // sources, and rule 1 forbids inventing it) — this test pins norte's
        // OWN convention, so it stays scoped to the three native presets.
        for (name, preset) in presets()
            .into_iter()
            .filter(|(n, _)| matches!(*n, "orthodox" | "vim" | "cua"))
        {
            let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog)
                .unwrap_or_else(|e| panic!("preset {name}: {e}"));
            for ((mods, code), command) in &expected {
                let mut r = Resolver::new(eff.clone());
                let chord = chord_from_crossterm(*mods, *code)
                    .unwrap_or_else(|| panic!("preset {name}: unmodeled chord {code:?}"));
                assert_eq!(
                    r.push(chord),
                    Resolution::Run {
                        command: (*command).to_owned(),
                        count: Count::None
                    },
                    "preset {name}: {command}"
                );
            }
            // And `alt+c` opens the picker from the pane.
            let browse = Effective::build_for(&preset, &[], COMMANDS, Screen::Browse)
                .unwrap_or_else(|e| panic!("preset {name}: {e}"));
            let mut r = Resolver::new(browse);
            let alt_c = chord_from_crossterm(CtMods::ALT, CtCode::Char('c')).expect("alt+c");
            assert_eq!(
                r.push(alt_c),
                Resolution::Run {
                    command: "pane.columns".to_owned(),
                    count: Count::None
                },
                "preset {name}: pane.columns"
            );
        }
    }

    /// The five pane/navigation gestures resolve in the three factory
    /// presets, THROUGH the real crossterm adapter. A preset that loses one
    /// leaves the gesture unreachable by keyboard while every other test
    /// stays green — the same regression class the mark commands are pinned
    /// against.
    ///
    /// `pane.swap` is deliberately NOT the same chord everywhere: `ctrl+u` is
    /// already `cursor.page-up` in `vim`, and the vim idiom outranks the
    /// borrowed one in its own preset, so there it is `alt+s`. This test
    /// spells out both so a future edit that "unifies" them has to argue with
    /// the reason.
    ///
    /// K2b: scoped to the three NATIVE presets on purpose. `total-commander`
    /// and `krusader` transcribe a foreign program (plan rule 1): TC has no
    /// "mirror" key at all, and Krusader's own "adopt the other panel's
    /// path" is `ctrl+=`, not `alt+u` — inventing `alt+i`/`alt+u` there to
    /// pass this pin would be exactly the approximation rule 1 forbids.
    #[test]
    fn pane_gesture_chords_resolve_in_the_three_presets() {
        let common = [
            ((CtMods::ALT, CtCode::Char('i')), "pane.mirror"),
            ((CtMods::ALT, CtCode::Char('u')), "pane.pull"),
            ((CtMods::ALT, CtCode::Left), "nav.back"),
            ((CtMods::ALT, CtCode::Right), "nav.forward"),
        ];
        for (name, preset) in presets()
            .into_iter()
            .filter(|(n, _)| matches!(*n, "orthodox" | "vim" | "cua"))
        {
            let eff = Effective::build_for(&preset, &[], COMMANDS, Screen::Browse)
                .unwrap_or_else(|e| panic!("preset {name}: {e}"));
            let swap = if name == "vim" {
                (CtMods::ALT, CtCode::Char('s'))
            } else {
                (CtMods::CONTROL, CtCode::Char('u'))
            };
            for ((mods, code), command) in common.iter().chain(&[(swap, "pane.swap")]) {
                let mut r = Resolver::new(eff.clone());
                let chord = chord_from_crossterm(*mods, *code)
                    .unwrap_or_else(|| panic!("preset {name}: unmodeled chord {code:?}"));
                assert_eq!(
                    r.push(chord),
                    Resolution::Run {
                        command: (*command).to_owned(),
                        count: Count::None
                    },
                    "preset {name}: {command}"
                );
            }
        }
    }

    /// In `vim`, `ctrl+u` is STILL `cursor.page-up`: the `pane.swap` chord
    /// from the other two presets did not override it. (Mutation control:
    /// binding `pane.swap` there breaks this test.)
    #[test]
    fn vim_ctrl_u_is_still_page_up() {
        let vim = presets()
            .into_iter()
            .find(|(n, _)| *n == "vim")
            .expect("vim preset")
            .1;
        let eff = Effective::build_for(&vim, &[], COMMANDS, Screen::Browse).expect("vim effective");
        let mut r = Resolver::new(eff);
        let ctrl_u = chord_from_crossterm(CtMods::CONTROL, CtCode::Char('u')).expect("ctrl+u");
        assert_eq!(
            r.push(ctrl_u),
            Resolution::Run {
                command: "cursor.page-up".to_owned(),
                count: Count::None
            }
        );
    }

    /// The six mark commands resolve in the three factory presets (#103). A
    /// preset that loses one leaves the selection unreachable by keyboard,
    /// which is the regression class the GUI already hit once.
    ///
    /// K2b: scoped to the three NATIVE presets. `total-commander` and
    /// `krusader` bind the same SIX commands (rule 1 fidelity check, not
    /// this pin's job), but on different chords — `ctrl+A`/`ctrl+shift+a`
    /// for `mark.clear` is norte's own convention, not Krusader's (which is
    /// `alt+-`) or TC's (`ctrl+-`, TC's own `CTRL+NUM-`).
    #[test]
    fn mark_commands_resolve_in_the_three_presets() {
        let expected = [
            (Chord::new(Mods::default(), KeyCode::Insert), "mark.toggle"),
            (
                Chord::new(
                    Mods {
                        ctrl: true,
                        ..Default::default()
                    },
                    KeyCode::Char('a'),
                ),
                "mark.all",
            ),
            (
                Chord::new(Mods::default(), KeyCode::Char('*')),
                "mark.invert",
            ),
            (
                // `alt+a`, not `ctrl+A`. This pair — `ctrl+a` to mark
                // everything and its uppercase to unmark — read well and did
                // not work: the terminal sends the SAME byte for Ctrl+A and
                // Ctrl+Shift+A, so `mark.clear` was advertised and dead in
                // all three native presets. Caught by
                // `no_preset_binds_a_chord_the_terminal_cannot_deliver`.
                Chord::new(
                    Mods {
                        alt: true,
                        ..Default::default()
                    },
                    KeyCode::Char('a'),
                ),
                "mark.clear",
            ),
            (
                Chord::new(Mods::default(), KeyCode::Char('+')),
                "mark.pattern-add",
            ),
            (
                Chord::new(Mods::default(), KeyCode::Char('-')),
                "mark.pattern-remove",
            ),
        ];
        for (name, preset) in presets()
            .into_iter()
            .filter(|(n, _)| matches!(*n, "orthodox" | "vim" | "cua"))
        {
            let eff = Effective::build_for(&preset, &[], COMMANDS, Screen::Browse)
                .unwrap_or_else(|e| panic!("preset {name}: {e}"));
            for (chord, command) in &expected {
                let mut r = Resolver::new(eff.clone());
                assert_eq!(
                    r.push(*chord),
                    Resolution::Run {
                        command: (*command).to_owned(),
                        count: Count::None
                    },
                    "preset {name}: {command}"
                );
            }
        }
    }
}

/// Parses a palette plugin row's `key` back into `(plugin_id, command_id)`.
///
/// Lives in `norte-frontend`, ALONGSIDE `palette::plugin_rows`, which is what
/// COMPOSES that key: whoever writes it and whoever reads it cannot be in two
/// crates with two different answers about where `command_id` starts — which
/// is exactly the half with no validated charset. Re-exported under its usual
/// name so no call site in this crate has to move.
pub use norte_frontend::palette::parse_plugin_key;

#[cfg(test)]
mod parse_plugin_key_tests {
    use super::parse_plugin_key;

    #[test]
    fn splits_plugin_id_and_command_id() {
        assert_eq!(
            parse_plugin_key("plugin:org.norte.demo:greet"),
            Some(("org.norte.demo", "greet"))
        );
    }

    /// `command_id` has NO validated charset (unlike `plugin_id`): it can
    /// carry `:` or newlines, and the split keeps EVERYTHING that follows the
    /// first one, without splitting again.
    #[test]
    fn a_hostile_command_id_is_taken_whole_without_splitting() {
        assert_eq!(
            parse_plugin_key("plugin:org.norte.demo:a:b\nc"),
            Some(("org.norte.demo", "a:b\nc"))
        );
    }

    #[test]
    fn with_no_plugin_prefix_it_is_none() {
        assert_eq!(parse_plugin_key("app.quit"), None);
        assert_eq!(parse_plugin_key(""), None);
    }

    /// With no second `:` (minimal format `plugin:x` with no `command_id`):
    /// `None` — a partial dispatch must never run `plugin_run_command` with an
    /// empty or guessed id.
    #[test]
    fn with_no_second_separator_it_is_none() {
        assert_eq!(parse_plugin_key("plugin:org.norte.demo"), None);
    }

    /// An empty `plugin_id` (`"plugin::greet"`) is `None` — never reachable
    /// from a real row (`PluginInfo.id` is always non-empty, validated by the
    /// core), but the parser must not hand an empty id to
    /// `plugin_run_command` if it ever were one.
    #[test]
    fn an_empty_plugin_id_is_none() {
        assert_eq!(parse_plugin_key("plugin::greet"), None);
    }
}
