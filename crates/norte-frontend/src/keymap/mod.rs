//! PURE keymap engine shared by the frontends (ADR 0006/0007/0043/0044): a
//! `(context, sequence) -> command` map, Yazi-style layers, prefix-free
//! validated at load time — resolution is a deterministic linear scan over
//! the effective map (at most a few hundred bindings), no timeouts. NEUTRAL
//! key (no crossterm/gpui): each frontend converts its native event to
//! [`Chord`] with [`Chord::new`].
//!
//! ADR 0044 adds the numeric prefix (`5j`) and the two load rules that come
//! with it: the count travels WITH the command ([`Count`]) and it is the
//! frontend that repeats the dispatch, so no command signature changes; a
//! digit 1-9 cannot be a binding while counts are enabled, and `Tab` is
//! reserved for `pane.switch` in Browse (the rule looks at the FIRST chord
//! of the sequence).

pub mod catalogue;
mod chord;
mod effective;
mod layer;
pub mod presets;
mod rebind;
mod resolve;

pub use catalogue::{CATALOGUE, CommandDef, Effect, Status};
pub use chord::{
    Chord, KeyCode, ModKey, Mods, mod_key, paint_chord, parse_chord, set_chord_lang, set_mod_key,
    unpaint_chord,
};
pub use effective::{Availability, Continuation, Effective, LUA_HOST, valid_lua_name};
// Not public API: the spelling a sequence has IN THE FILE, which the keyboard
// sheet paints (after `paint_chord`) and the shortcut editor hands to the
// `keymap.toml` writer. Two copies of it is how the writer and the loader
// would eventually disagree about what `g g` is called.
pub(crate) use effective::render_seq;
pub use layer::{KeymapFile, Screen, parse_keymap, parse_keymap_layer};
pub use rebind::{
    Rebind, RebindError, RebindSources, RebindSplit, RebindWrite, UnbindOutcome, UnbindWrite,
    rebind_check, rebind_dry_run, unbind_dry_run,
};
pub use resolve::{Count, Resolution, Resolver};

use layer::RawSection;

/// A keymap load or parse error. ALWAYS an actionable diagnostic: broken
/// config is a clear error, never odd behavior.
#[derive(Debug, thiserror::Error)]
pub enum KeymapError {
    /// The TOML does not parse or has unknown keys.
    #[error("invalid keymap.toml: {0}")]
    Toml(String),
    /// A key is not understood (`"megatecla"`, `"ctrl+"`, `"f99"`).
    #[error("invalid key: {chord:?}")]
    BadChord {
        /// The text that did not parse.
        chord: String,
    },
    /// A binding with an empty sequence.
    #[error("binding with an empty sequence (run = {run:?})")]
    EmptySequence {
        /// The empty binding's command.
        run: String,
    },
    /// The command does not exist (typo or old version).
    #[error("unknown command: {run:?}")]
    UnknownCommand {
        /// The unrecognized name.
        run: String,
    },
    /// `shift+<char>` would never match (the char ALREADY encodes shift):
    /// rejected with a diagnostic instead of being a dead binding.
    #[error(
        "{chord:?}: shift does not combine with characters — write the key already \"shifted\" (\"G\", \"plus\")"
    )]
    ShiftWithChar {
        /// The offending text.
        chord: String,
    },
    /// The layer carries the wrong list: a preset defines `keymap`; a user
    /// layer defines `prepend_keymap`/`append_keymap` (Yazi model). Silently
    /// ignoring it would be broken config with no error.
    #[error("the {layer} layer does not accept {key} (preset: keymap; usuario: prepend/append)")]
    WrongLayerKey {
        /// `"preset"` or `"user"`.
        layer: &'static str,
        /// The extra key.
        key: &'static str,
    },
    /// `dialog_from` and an own `[dialog]` section at once (K2b): two
    /// answers to the same question, and silently picking one would leave
    /// the user with an overlay context they did not write.
    #[error("dialog_from = {name:?} together with its own [dialog] {list}: pick one of the two")]
    DialogFromAndDialog {
        /// The preset it meant to inherit from.
        name: String,
        /// Which of the section's three lists triggered it — without this
        /// the message sends the reader looking for a `keymap` that may not
        /// exist.
        list: &'static str,
    },
    /// `dialog_from` names something that is not a factory preset (typo, or
    /// a preset from another version).
    #[error("dialog_from = {name:?}: no factory preset has that name ({known})")]
    UnknownDialogFrom {
        /// The name that does not resolve.
        name: String,
        /// The ones that do, comma-separated (comes from `presets::NAMES`).
        known: String,
    },
    /// The inherited preset itself inherits: inheritance is ONE level, no
    /// chains — otherwise the effective `[dialog]` would depend on a hop
    /// invisible from reading the file.
    #[error(
        "dialog_from = {name:?}, but that preset in turn inherits from {then:?}: [dialog] inheritance is one level only"
    )]
    DialogFromChain {
        /// The preset named by the file being loaded.
        name: String,
        /// Who THAT one inherits from, which is what closes the chain.
        then: String,
    },
    /// `esc` inside a multi-key sequence: unreachable, because `Esc` ALWAYS
    /// cancels a pending one (only valid as a lone binding).
    #[error("esc can only be bound as a lone key, not inside {sequence:?}")]
    EscInSequence {
        /// The offending sequence.
        sequence: String,
    },
    /// A sequence is a strict prefix of another: forbidden (ADR 0006 — with
    /// no timeouts, resolution must be deterministic).
    #[error("ambiguous sequences: {shorter:?} is a prefix of {longer:?}")]
    AmbiguousPrefix {
        /// The short sequence (the one that would always fire).
        shorter: String,
        /// The long sequence (the unreachable one).
        longer: String,
    },
    /// A digit opens a binding's sequence in a context whose preset enables
    /// numeric counts (K2a). The two things cannot both be true, and
    /// silently picking one for the user is how a keymap becomes
    /// unpredictable. `0` is exempt: a count never starts with zero, so the
    /// two never compete for the key.
    #[error(
        "{chord:?} cannot be a key and a count at once: it is bound to {run:?} and the preset enables counts (0 is bindable)"
    )]
    DigitBoundWithCounts {
        /// The offending chord, as written.
        chord: String,
        /// What it is bound to.
        run: String,
    },
    /// A binding takes a key the specification reserves (§12: `Tab` switches
    /// panes). A preset imitating another program DOCUMENTS the difference;
    /// it does not keep the key.
    #[error("{chord:?} is reserved for {reserved_for} (spec §12) and cannot be bound to {run:?}")]
    SacredKey {
        /// The reserved chord, as written.
        chord: String,
        /// The command it is reserved for.
        reserved_for: &'static str,
        /// What the offending binding tried to run instead.
        run: String,
    },
}

/// One finding from [`Effective::build_diagnostics`]: reported WITHOUT
/// stopping the walk, unlike [`Effective::build_for`], which fails on the
/// first defect. `norte doctor` (#102) maps each to a report row in a
/// SINGLE pass — no per-typo rebuild, no retry cap, and no non-convergent
/// `lua:`-charset case (a broken `lua:` name can never be fixed by extending
/// `known_commands`, so the retry-with-known trick never terminates for it;
/// the one-pass walk classifies it directly instead).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeymapDiagnostic {
    /// A plain (non-`lua:`) `run` name absent from `known_commands` — a typo
    /// or a binding for a newer version's command. Recoverable: the rest of
    /// the keymap is unaffected. `run` is UNTRUSTED config text.
    UnknownCommand {
        /// The unrecognized command name (untrusted config text).
        run: String,
    },
    /// A defect that would make [`Effective::build_for`] fail outright: a bad
    /// chord, an empty or `esc`-bearing sequence, a wrong layer key, an
    /// ambiguous prefix, or a `lua:` name that fails the charset. Carries the
    /// rendered [`KeymapError`] message (may embed UNTRUSTED config text).
    Structural {
        /// Human-readable description (from the underlying [`KeymapError`]).
        message: String,
    },
}

/// The union of `run` names bound (in ANY of the three lists — `keymap`,
/// `prepend_keymap`, `append_keymap`, though only `keymap` is actually used
/// by a bundled preset today) by ANY bundled preset (`orthodox`/`vim`/`cua`)
/// for `screen` — its screen-specific context (`pane`/`viewer`/`dialog`)
/// merged with `global`. A preset that fails to parse is skipped silently
/// (the three bundled presets are compile-time embedded and pinned by this
/// module's own test suite, so this only matters if that invariant ever
/// breaks).
///
/// This is an HONEST APPROXIMATION, not a frontend's actual command
/// catalog: it only sees command names bound by a keymap, not the full set
/// a frontend implements (a command with no default binding in any preset
/// is invisible here). `norte doctor` (H2) uses it to flag a config layer's
/// `run` name that no bundled preset recognizes for that screen — worth a
/// warning, not proof the command doesn't exist (see its rustdoc/report
/// footer for the caveat).
#[must_use]
pub fn preset_commands(screen: Screen) -> Vec<String> {
    let specific = screen.specific();
    let mut out: Vec<String> = Vec::new();
    let push_all = |section: &RawSection, out: &mut Vec<String>| {
        for list in [
            &section.keymap,
            &section.prepend_keymap,
            &section.append_keymap,
        ] {
            for b in list {
                if !out.contains(&b.run) {
                    out.push(b.run.clone());
                }
            }
        }
    };
    for name in presets::NAMES {
        let Some(src) = presets::source(name) else {
            continue;
        };
        let Ok(kf) = parse_keymap(src) else {
            continue;
        };
        push_all(specific(&kf), &mut out);
        push_all(&kf.global, &mut out);
    }
    out
}

/// The user-facing sentence for an unavailable key. Lives here rather than in
/// each frontend so the TUI and the GUI cannot word it differently.
///
/// [`Availability::Here`] has nothing to say — the key runs — so it renders
/// empty; a caller only ever builds this from a
/// [`Resolution::Unavailable`], which never carries it.
///
/// ```
/// use norte_frontend::keymap::{Availability, unavailable_message};
///
/// let m = unavailable_message(
///     "pane.pack",
///     Availability::NotBuilt { reason: "keymap-reason-archive-write", issue: 132 },
/// );
/// assert!(m.contains("pane.pack"), "{m}");
/// assert!(m.contains("132"), "{m}");
/// // The catalogue holds a Fluent ID: it must be TRANSLATED, not pasted.
/// assert!(!m.contains("keymap-reason-"), "{m}");
///
/// assert!(unavailable_message("pane.hotlist", Availability::NotHere).contains("pane.hotlist"));
/// assert!(unavailable_message("pane.copy", Availability::Here).is_empty());
/// ```
#[must_use]
pub fn unavailable_message(command: &str, why: Availability) -> String {
    unavailable_message_in(command, why, norte_i18n::active())
}

/// [`unavailable_message`] in a GIVEN language.
///
/// The window's status bar used to change language depending on which
/// message it got: its own come out in the host's, and this one came out
/// in the process's.
#[must_use]
pub fn unavailable_message_in(command: &str, why: Availability, lang: norte_i18n::Lang) -> String {
    match why {
        Availability::Here => String::new(),
        // `reason` is a Fluent ID, not prose (see the catalogue): translate it
        // first, then interpolate. Interpolating the id would print English
        // inside a Spanish sentence.
        Availability::NotBuilt { reason, issue } => norte_i18n::ta_in(
            lang,
            "keymap-unavailable-not-built",
            &[
                ("command", command),
                ("reason", &norte_i18n::t_in(lang, reason)),
                ("issue", &issue.to_string()),
            ],
        ),
        Availability::NotHere => {
            norte_i18n::ta_in(lang, "keymap-unavailable-not-here", &[("command", command)])
        }
    }
}

/// The SHORT form of the same fact, for a surface that already names the
/// command in the row it is decorating: the which-key panel (K3a) and the
/// reference sheet (K3b). No command in it, and the language is a parameter
/// rather than the process-wide one, because both callers render a whole page
/// in one language chosen by the caller.
///
/// It still says "not built yet" rather than only the reason, because the
/// third surface that prints it — `norte help keys` — writes to a pipe and
/// cannot dim anything: on its own, `writing archives (#132)` reads like a
/// description of what the key DOES. The wording has to carry the meaning that
/// styling carries elsewhere.
///
/// One function and not one per surface, next to [`unavailable_message`], so
/// the long form and the short form cannot drift into disagreeing about the
/// same `Availability`.
///
/// ```
/// use norte_frontend::keymap::{Availability, short_unavailable_message};
/// use norte_i18n::Lang;
///
/// let m = short_unavailable_message(
///     Availability::NotBuilt { reason: "keymap-reason-archive-write", issue: 132 },
///     Lang::En,
/// );
/// assert!(m.contains("132"), "{m}");
/// // It SAYS "not built": the bare reason would read as a description of
/// // what the key does on the one surface that cannot dim a row.
/// assert!(m.contains("not built"), "{m}");
/// // The catalogue holds a Fluent ID: it must be TRANSLATED, not pasted.
/// assert!(!m.contains("keymap-reason-"), "{m}");
/// // The command is the row's job, not this sentence's.
/// assert!(!m.contains("pane.pack"), "{m}");
/// assert!(short_unavailable_message(Availability::Here, Lang::En).is_empty());
/// ```
#[must_use]
pub fn short_unavailable_message(why: Availability, lang: norte_i18n::Lang) -> String {
    match why {
        Availability::Here => String::new(),
        // Same rule as the long form: `reason` is a Fluent id, so translate it
        // before interpolating, or a Spanish page carries an English clause.
        Availability::NotBuilt { reason, issue } => norte_i18n::ta_in(
            lang,
            "keymap-short-not-built",
            &[
                ("reason", &norte_i18n::t_in(lang, reason)),
                ("issue", &issue.to_string()),
            ],
        ),
        Availability::NotHere => norte_i18n::t_in(lang, "keymap-short-not-here"),
    }
}

/// The user-facing sentence for a count that landed on a command that takes
/// none. Lives here, like [`unavailable_message`], so the TUI and the GUI
/// cannot word it differently.
///
/// The count is never swallowed: the command runs ONCE and this says the
/// number went nowhere. Silence would leave the user believing `3q` did
/// something three times.
///
/// ```
/// use norte_frontend::keymap::count_ignored_message;
///
/// let m = count_ignored_message("app.quit", 3);
/// assert!(m.contains("app.quit"), "{m}");
/// assert!(m.contains('3'), "{m}");
/// ```
#[must_use]
pub fn count_ignored_message(command: &str, count: u32) -> String {
    norte_i18n::ta(
        "keymap-count-ignored",
        &[("command", command), ("count", &count.to_string())],
    )
}

#[cfg(test)]
mod preset_commands_tests {
    use super::{Screen, preset_commands};

    /// Orthodox binds `app.quit` in `[global]` (ADR 0006: global merges
    /// into every screen), so `Browse` must see it.
    #[test]
    fn orthodox_browse_contains_app_quit() {
        let v = preset_commands(Screen::Browse);
        assert!(v.contains(&"app.quit".to_owned()), "{v:?}");
    }

    /// Every bundled preset binds `y` to `dialog.approve` in `[dialog]`.
    #[test]
    fn dialog_contains_dialog_approve() {
        let v = preset_commands(Screen::Dialog);
        assert!(v.contains(&"dialog.approve".to_owned()), "{v:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eff(preset: &str, user: Option<&str>) -> Result<Effective, KeymapError> {
        const COMMANDS: &[&str] = &[
            "app.quit",
            "pane.switch",
            "cursor.up",
            "cursor.down",
            "cursor.top",
            "cursor.bottom",
            "nav.enter",
        ];
        let preset = parse_keymap(preset)?;
        let user = user.map(parse_keymap).transpose()?;
        Effective::build(&preset, user.as_ref(), COMMANDS)
    }

    /// A `Resolution::Run` with NO count — what every assertion written before
    /// K2a means, and what a preset without `counts = true` can ever produce.
    fn run(command: &str) -> Resolution {
        Resolution::Run {
            command: command.to_owned(),
            count: Count::None,
        }
    }

    #[test]
    fn parse_de_chords() {
        assert_eq!(
            parse_chord("f5").unwrap(),
            Chord::new(Mods::default(), KeyCode::F(5))
        );
        assert_eq!(
            parse_chord("ctrl+c").unwrap(),
            Chord::new(
                Mods {
                    ctrl: true,
                    ..Default::default()
                },
                KeyCode::Char('c')
            )
        );
        assert_eq!(
            parse_chord("alt+enter").unwrap(),
            Chord::new(
                Mods {
                    alt: true,
                    ..Default::default()
                },
                KeyCode::Enter
            )
        );
        // Uppercase: the char ALREADY encodes shift.
        assert_eq!(
            parse_chord("G").unwrap(),
            Chord::new(Mods::default(), KeyCode::Char('G'))
        );
        assert_eq!(
            parse_chord("shift+f5").unwrap(),
            Chord::new(
                Mods {
                    shift: true,
                    ..Default::default()
                },
                KeyCode::F(5)
            )
        );
        for s in ["", "ctrl+", "megatecla", "ctrl+ctrl+c", "f99"] {
            assert!(parse_chord(s).is_err(), "{s:?} must fail");
        }
    }

    /// `+` is the modifier separator, so a bare "+" is unparseable and `plus`
    /// is the only spelling. Pinned because a future refactor that "simplifies"
    /// the token table would silently make the mark.pattern-add chord
    /// unreachable (#103).
    #[test]
    fn plus_token_is_the_only_spelling_of_the_plus_key() {
        assert_eq!(
            parse_chord("plus").unwrap(),
            Chord::new(Mods::default(), KeyCode::Char('+'))
        );
        assert!(matches!(
            parse_chord("+"),
            Err(KeymapError::BadChord { .. })
        ));
        assert_eq!(
            parse_chord("ctrl+plus").unwrap(),
            Chord::new(
                Mods {
                    ctrl: true,
                    ..Default::default()
                },
                KeyCode::Char('+')
            )
        );
        // …and under the new modifiers too: `plus` is still the only
        // spelling, the separator does not change meaning.
        assert_eq!(
            parse_chord("cmd+plus").unwrap(),
            Chord::new(
                Mods {
                    cmd: true,
                    ..Default::default()
                },
                KeyCode::Char('+')
            )
        );
        assert_eq!(
            parse_chord("mod+plus").unwrap(),
            parse_chord("ctrl+plus").unwrap(),
            "the default policy is Ctrl"
        );
        assert!(matches!(
            parse_chord("cmd++"),
            Err(KeymapError::BadChord { .. })
        ));
    }

    /// `mod+` is the one per-OS mechanism: a preset stays a single file. The
    /// process picks which physical modifier it means, once, at startup.
    #[test]
    fn mod_is_ctrl_by_default() {
        let c = parse_chord("mod+c").unwrap();
        assert_eq!(c, parse_chord("ctrl+c").unwrap());
    }

    /// `cmd+` is literal, for a preset that means Cmd and nothing else.
    #[test]
    fn cmd_is_its_own_modifier_and_is_not_ctrl() {
        let cmd = parse_chord("cmd+c").unwrap();
        let ctrl = parse_chord("ctrl+c").unwrap();
        assert_ne!(cmd, ctrl);
    }

    /// The resolution is a pure function of the policy, so it is testable
    /// without a macOS machine — which matters, because CI is off and nobody
    /// here has one. NOTE: no test may call `set_mod_key`; the policy is
    /// process-wide and a test that sets it would poison every later test in
    /// the same binary. `apply` is the pure half, and it is the half worth
    /// pinning.
    #[test]
    fn the_policy_decides_what_mod_translates_to() {
        assert_eq!(
            ModKey::Ctrl.apply(Mods::default()),
            Mods {
                ctrl: true,
                ..Mods::default()
            }
        );
        assert_eq!(
            ModKey::Cmd.apply(Mods::default()),
            Mods {
                cmd: true,
                ..Mods::default()
            }
        );
    }

    /// The repeated-modifier check sees THROUGH the alias: with the default
    /// policy `mod` IS `ctrl`, so `ctrl+mod+x` names the same key twice and
    /// dies as `BadChord`, exactly like `ctrl+ctrl+x`. `cmd+mod+x` is the
    /// same story on the other side, and stays legal here only because the
    /// default policy resolves `mod` to Ctrl.
    #[test]
    fn the_alias_counts_as_its_modifier_for_the_repeat() {
        assert!(matches!(
            parse_chord("ctrl+mod+x"),
            Err(KeymapError::BadChord { .. })
        ));
        assert!(matches!(
            parse_chord("mod+ctrl+x"),
            Err(KeymapError::BadChord { .. })
        ));
        assert!(matches!(
            parse_chord("cmd+cmd+x"),
            Err(KeymapError::BadChord { .. })
        ));
        // rust-reviewer MINOR-10: `cmd`+`mod` is ALWAYS rejected, not only
        // under the Cmd policy (where it would be the same key twice). This
        // test used to pin the opposite — "legal under the default policy"
        // — and that made a chord's validity depend on the operating
        // system: it loaded on Linux and blew up on macOS, which is the
        // asymmetry ADR 0043's decision 8 says to avoid.
        assert!(matches!(
            parse_chord("cmd+mod+x"),
            Err(KeymapError::BadChord { .. })
        ));
        // And the order does not matter: it is the combination that is
        // rejected.
        assert!(matches!(
            parse_chord("mod+cmd+x"),
            Err(KeymapError::BadChord { .. })
        ));
    }

    /// `cmd` sorts BEFORE `ctrl` in `Display`, so a chord carrying both has
    /// exactly ONE spelling and the round trip is closed.
    #[test]
    fn cmd_precede_a_ctrl_en_la_grafia_canonica() {
        let c = Chord::new(
            Mods {
                cmd: true,
                ctrl: true,
                ..Mods::default()
            },
            KeyCode::Char('x'),
        );
        assert_eq!(c.to_string(), "cmd+ctrl+x");
        assert_eq!(parse_chord(&c.to_string()).unwrap(), c);
    }

    #[test]
    fn plus_chord_round_trips_through_display() {
        let c = Chord::new(Mods::default(), KeyCode::Char('+'));
        assert_eq!(c.to_string(), "plus");
        assert_eq!(parse_chord(&c.to_string()).unwrap(), c);
    }

    /// Table-wide `parse(display(c)) == c` property, scoped to the domain
    /// where `Display` and `parse_chord` actually agree: every non-`Char`
    /// key, `F(1..=12)`, and a char sample (space, `+`, an uppercase ASCII
    /// letter, and a non-ASCII char).
    ///
    /// `F(n)` OUTSIDE `1..=12` is deliberately EXCLUDED — `F(0)` as much as
    /// `F(13)`: `Display` renders ANY `F(n)` as
    /// `"f{n}"`, but `parse_chord` accepts only `1..=12`, so the two domains
    /// disagree there. Since #109 both frontend adapters clamp to `1..=12`
    /// (classic xterm reports Shift+F1 as F13 — the TUI adapter used to
    /// forward it unclamped), so no runtime path constructs one; the type
    /// still allows it, and this property scopes itself to the shared
    /// domain rather than pretending `Display` is total.
    #[test]
    fn display_and_parse_chord_round_trip_over_the_token_table() {
        let non_char = [
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::Esc,
            KeyCode::Backspace,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Insert,
            KeyCode::Delete,
        ];
        // encoding-auditor MINOR 1+2: this is the CROSS PRODUCT, not two
        // sweeps that never meet. The previous shape ran all 16 modifier
        // combinations against `F(5)` only, and every key against
        // `Mods::default()` only — so no modifier was ever applied to
        // `space`. That gap was load-bearing: `Char(' ')` and `Char('+')`
        // spell as WORDS precisely because the raw characters collide with
        // `render_seq`'s space join and `paint_chord`'s `split`, and a
        // refactor that "simplified" `Display` back to the raw char would
        // have kept the old test green while quietly turning a two-key
        // sequence into a three-key one in the help.
        //
        // `shift` is fed in unconditionally rather than masked off for
        // `Char`: `Chord::new` DROPS it there (the character already encodes
        // it) and `parse_chord` rejects the spelling, so running the bit
        // exercises that normalisation inside the round trip instead of
        // assuming it.
        let codes = non_char
            .into_iter()
            .chain((1..=12u8).map(KeyCode::F))
            .chain([' ', '+', 'G', 'ñ'].map(KeyCode::Char));
        for code in codes {
            for bits in 0..16u8 {
                let mods = Mods {
                    cmd: bits & 1 != 0,
                    ctrl: bits & 2 != 0,
                    alt: bits & 4 != 0,
                    shift: bits & 8 != 0,
                };
                // `Display` orders them `cmd+ctrl+alt+shift+`, the one
                // canonical spelling `parse_chord` reads back whatever order
                // it was written in.
                let c = Chord::new(mods, code);
                assert_eq!(parse_chord(&c.to_string()).unwrap(), c, "{mods:?} {code:?}");
            }
        }
    }

    #[test]
    fn chord_new_normalizes_shift_in_chars_but_not_in_other_keys() {
        // A native event with Char('G')+shift: the canonical chord drops
        // shift (the char already encodes it) — parity with the TUI's old
        // `Chord::from_event` (now `Chord::new`'s default behavior).
        let c = Chord::new(
            Mods {
                shift: true,
                ..Default::default()
            },
            KeyCode::Char('G'),
        );
        assert_eq!(c, Chord::new(Mods::default(), KeyCode::Char('G')));
        // On non-char keys, shift IS information.
        let f = Chord::new(
            Mods {
                shift: true,
                ..Default::default()
            },
            KeyCode::F(5),
        );
        assert_eq!(
            f,
            Chord::new(
                Mods {
                    shift: true,
                    ..Default::default()
                },
                KeyCode::F(5)
            )
        );
    }

    #[test]
    fn parse_chord_rejects_multi_codepoint_tokens_without_splitting() {
        // A token that is NOT exactly one char (decomposed é = e+U+0301, or
        // a ZWJ emoji) is cleanly rejected, never truncated halfway.
        assert!(matches!(
            parse_chord("e\u{0301}"),
            Err(KeymapError::BadChord { .. })
        ));
        assert!(matches!(
            parse_chord("👨\u{200d}👩\u{200d}👧"),
            Err(KeymapError::BadChord { .. })
        ));
    }

    #[test]
    fn resolves_multi_key_sequences() {
        let preset = r#"
            [pane]
            keymap = [
                { on = ["g", "g"], run = "cursor.top" },
                { on = ["G"], run = "cursor.bottom" },
            ]
            [global]
            keymap = [{ on = ["q"], run = "app.quit" }]
        "#;
        let eff = eff(preset, None).unwrap();
        let mut r = Resolver::new(eff.clone());
        assert_eq!(
            r.push(parse_chord("g").unwrap()),
            Resolution::Pending(1),
            "valid prefix: waits"
        );
        assert_eq!(r.push(parse_chord("g").unwrap()), run("cursor.top"));
        // After running, the state is clean.
        assert_eq!(r.push(parse_chord("G").unwrap()), run("cursor.bottom"));
        // Key with no binding: silent reset.
        assert_eq!(r.push(parse_chord("z").unwrap()), Resolution::Reset);
        // Pending prefix + a key that does not continue it: reset (runs nothing).
        r.push(parse_chord("g").unwrap());
        assert_eq!(r.push(parse_chord("q").unwrap()), Resolution::Reset);
        // A lone q (global context) does run.
        assert_eq!(r.push(parse_chord("q").unwrap()), run("app.quit"));
    }

    #[test]
    fn esc_cancels_the_pending_sequence() {
        let preset = r#"
            [pane]
            keymap = [
                { on = ["g", "g"], run = "cursor.top" },
                { on = ["esc"], run = "app.quit" },
            ]
        "#;
        let eff = eff(preset, None).unwrap();
        let mut r = Resolver::new(eff.clone());
        r.push(parse_chord("g").unwrap());
        // With a pending sequence, Esc ALWAYS cancels (never runs a binding).
        assert_eq!(r.push(parse_chord("esc").unwrap()), Resolution::Reset);
        // With nothing pending, Esc is just another key.
        assert_eq!(r.push(parse_chord("esc").unwrap()), run("app.quit"));
    }

    #[test]
    fn ambiguous_prefix_is_a_load_error() {
        let preset = r#"
            [pane]
            keymap = [
                { on = ["g"], run = "cursor.top" },
                { on = ["g", "g"], run = "cursor.bottom" },
            ]
        "#;
        match eff(preset, None) {
            Err(KeymapError::AmbiguousPrefix { .. }) => {}
            other => panic!("expected AmbiguousPrefix, got {other:?}"),
        }
    }

    #[test]
    fn shift_con_char_es_error_diagnosticable() {
        // A "shift+g" binding would never match (the canonical chord drops
        // shift on chars): rejected at parse time, not a dead binding.
        match parse_chord("shift+g") {
            Err(KeymapError::ShiftWithChar { .. }) => {}
            other => panic!("expected ShiftWithChar, got {other:?}"),
        }
        assert!(parse_chord("ctrl+shift+c").is_err());
        // On non-char keys, shift is legitimate.
        assert!(parse_chord("shift+f5").is_ok());
    }

    #[test]
    fn wrong_list_in_a_layer_is_an_error() {
        // User with `keymap` (instead of prepend/append): an error, not silence.
        let preset = r#"
            [pane]
            keymap = [{ on = ["j"], run = "cursor.down" }]
        "#;
        let user = r#"
            [pane]
            keymap = [{ on = ["j"], run = "cursor.up" }]
        "#;
        match eff(preset, Some(user)) {
            Err(KeymapError::WrongLayerKey {
                layer: "usuario", ..
            }) => {}
            other => panic!("expected WrongLayerKey usuario, got {other:?}"),
        }
        // Preset with prepend: same treatment.
        let bad_preset = r#"
            [pane]
            prepend_keymap = [{ on = ["j"], run = "cursor.down" }]
        "#;
        match eff(bad_preset, None) {
            Err(KeymapError::WrongLayerKey {
                layer: "preset", ..
            }) => {}
            other => panic!("expected WrongLayerKey preset, got {other:?}"),
        }
    }

    #[test]
    fn context_specificity_prevails_over_the_layer() {
        // ADR 0006 (disambiguated in phase 4): layers merge PER context;
        // between contexts the specific one wins. A user append in [pane]
        // beats the preset's [global] keymap…
        let preset = r#"
            [global]
            keymap = [{ on = ["q"], run = "app.quit" }]
            [pane]
            keymap = [{ on = ["j"], run = "cursor.down" }]
        "#;
        let user = r#"
            [pane]
            append_keymap = [{ on = ["q"], run = "cursor.up" }]
            [global]
            prepend_keymap = [{ on = ["j"], run = "app.quit" }]
        "#;
        let eff = eff(preset, Some(user)).unwrap();
        let mut r = Resolver::new(eff.clone());
        assert_eq!(
            r.push(parse_chord("q").unwrap()),
            run("cursor.up"),
            "pane.append beats global.keymap (specificity > layer)"
        );
        // …and a user prepend in [global] does NOT beat the [pane] keymap.
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            run("cursor.down"),
            "global.prepend does not beat pane.keymap"
        );
    }

    #[test]
    fn esc_inside_sequence_is_a_load_error() {
        let preset = r#"
            [pane]
            keymap = [{ on = ["a", "esc"], run = "cursor.up" }]
        "#;
        match eff(preset, None) {
            Err(KeymapError::EscInSequence { .. }) => {}
            other => panic!("expected EscInSequence, got {other:?}"),
        }
    }

    #[test]
    fn unknown_command_is_a_load_error() {
        let preset = r#"
            [pane]
            keymap = [{ on = ["x"], run = "comando.inventado" }]
        "#;
        match eff(preset, None) {
            Err(KeymapError::UnknownCommand { .. }) => {}
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    /// M4 Lua (T8, mirrored): a binding to `lua:<name>` passes validation
    /// even when the name is not in `known_commands` — the Lua registry is
    /// dynamic (runtime); an unregistered lua command is NOT a keymap
    /// error. The NAME itself IS validated with the same charset as
    /// `norte.command` (`[a-z0-9._-]{1,64}`): a binding to a name that
    /// could never register is diagnosable broken config, not a silently
    /// dead binding.
    #[test]
    fn prefixed_lua_passes_command_validation() {
        let preset = r#"
            [pane]
            keymap = [{ on = ["x"], run = "lua:mi-comando.v2" }]
        "#;
        let kf = parse_keymap(preset).unwrap();
        let with_host =
            Effective::build(&kf, None, &[LUA_HOST]).expect("lua: with a valid name passes");
        let mut r = Resolver::new(with_host);
        assert_eq!(
            r.push(parse_chord("x").unwrap()),
            run("lua:mi-comando.v2"),
            "the binding resolves to the whole lua: command"
        );

        // ADR 0110: the same binding in a frontend WITHOUT a Lua host
        // validates the same way, but is not announced as runnable.
        let without_host = eff(preset, None).expect("without a host it still loads");
        let mut r = Resolver::new(without_host);
        assert_eq!(
            r.push(parse_chord("x").unwrap()),
            Resolution::Unavailable {
                command: "lua:mi-comando.v2".to_owned(),
                why: Availability::NotHere,
            },
            "with no LUA_HOST the key says it is not here"
        );

        // Names outside the [a-z0-9._-]{1,64} charset: a LOAD error.
        let long = format!("lua:{}", "a".repeat(65));
        for bad in ["lua:", "lua:Mayuscula", "lua:con espacio", long.as_str()] {
            let preset = format!(
                r#"
                [pane]
                keymap = [{{ on = ["x"], run = "{bad}" }}]
                "#
            );
            match eff(&preset, None) {
                Err(KeymapError::UnknownCommand { .. }) => {}
                other => panic!("expected UnknownCommand for {bad:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn yazi_layers_prepend_overrides_and_append_only_adds() {
        let preset = r#"
            [pane]
            keymap = [
                { on = ["j"], run = "cursor.down" },
                { on = ["k"], run = "cursor.up" },
            ]
        "#;
        let user = r#"
            [pane]
            prepend_keymap = [{ on = ["j"], run = "cursor.top" }]
            append_keymap = [
                { on = ["k"], run = "cursor.bottom" },
                { on = ["x"], run = "app.quit" },
            ]
        "#;
        let eff = eff(preset, Some(user)).unwrap();
        let mut r = Resolver::new(eff.clone());
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            run("cursor.top"),
            "prepend BEATS the preset"
        );
        assert_eq!(
            r.push(parse_chord("k").unwrap()),
            run("cursor.up"),
            "append does NOT beat an existing sequence"
        );
        assert_eq!(
            r.push(parse_chord("x").unwrap()),
            run("app.quit"),
            "append adds the new one"
        );
    }

    #[test]
    fn specific_context_overrides_global_by_exact_sequence() {
        let preset = r#"
            [global]
            keymap = [
                { on = ["q"], run = "app.quit" },
                { on = ["tab"], run = "pane.switch" },
            ]
            [pane]
            keymap = [{ on = ["q"], run = "cursor.up" }]
        "#;
        let eff = eff(preset, None).unwrap();
        let mut r = Resolver::new(eff.clone());
        assert_eq!(r.push(parse_chord("q").unwrap()), run("cursor.up"));
        assert_eq!(r.push(parse_chord("tab").unwrap()), run("pane.switch"));
    }

    #[test]
    fn multiple_layers_fold_by_precedence() {
        // Layers in ASCENDING precedence: system, user.
        const COMMANDS: &[&str] = &[
            "app.quit",
            "cursor.up",
            "cursor.down",
            "cursor.top",
            "cursor.bottom",
        ];
        // ADR 0007: higher layers' prepends first; same for appends.
        let preset = parse_keymap(
            r#"
            [pane]
            keymap = [{ on = ["j"], run = "cursor.down" }]
        "#,
        )
        .unwrap();
        let system = parse_keymap(
            r#"
            [pane]
            prepend_keymap = [{ on = ["j"], run = "cursor.up" }]
            append_keymap = [{ on = ["x"], run = "app.quit" }]
        "#,
        )
        .unwrap();
        let user = parse_keymap(
            r#"
            [pane]
            prepend_keymap = [{ on = ["j"], run = "cursor.top" }]
            append_keymap = [{ on = ["x"], run = "cursor.bottom" }]
        "#,
        )
        .unwrap();
        let eff = Effective::build_layered(&preset, &[system, user], COMMANDS).unwrap();
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            run("cursor.top"),
            "the HIGHEST layer's prepend wins"
        );
        assert_eq!(
            r.push(parse_chord("x").unwrap()),
            run("cursor.bottom"),
            "among appends the highest layer wins too"
        );
    }

    #[test]
    fn the_viewer_context_merges_for_its_screen() {
        const COMMANDS: &[&str] = &["app.quit", "nav.enter", "cursor.top"];
        let preset = parse_keymap(
            r#"
            [global]
            keymap = [{ on = ["q"], run = "app.quit" }]
            [pane]
            keymap = [{ on = ["enter"], run = "nav.enter" }]
            [viewer]
            keymap = [{ on = ["q"], run = "cursor.top" }]
        "#,
        )
        .unwrap();
        // In Browse, global's q rules and enter exists.
        let browse = Effective::build_for(&preset, &[], COMMANDS, Screen::Browse).unwrap();
        let mut r = Resolver::new(browse);
        assert_eq!(r.push(parse_chord("q").unwrap()), run("app.quit"));
        assert_eq!(r.push(parse_chord("enter").unwrap()), run("nav.enter"));
        // In Viewer, its specific q BEATS global's and enter does NOT exist.
        let viewer = Effective::build_for(&preset, &[], COMMANDS, Screen::Viewer).unwrap();
        let mut r = Resolver::new(viewer);
        assert_eq!(r.push(parse_chord("q").unwrap()), run("cursor.top"));
        assert_eq!(r.push(parse_chord("enter").unwrap()), Resolution::Reset);
    }

    /// Help is built from the EFFECTIVE keymap: the exposed bindings
    /// reflect preset + layers IN PRECEDENCE ORDER, and a shadowed binding
    /// appears ONCE with the command that wins (what the key really does,
    /// not what the preset says).
    #[test]
    fn exposed_bindings_reflect_the_layers() {
        const COMMANDS: &[&str] = &["cursor.down", "cursor.up", "cursor.top"];
        let preset = parse_keymap(
            r#"
            [pane]
            keymap = [
                { on = ["j"], run = "cursor.down" },
                { on = ["k"], run = "cursor.up" },
            ]
        "#,
        )
        .unwrap();
        let user = parse_keymap(
            r#"
            [pane]
            prepend_keymap = [{ on = ["j"], run = "cursor.top" }]
            append_keymap = [{ on = ["g", "g"], run = "cursor.top" }]
        "#,
        )
        .unwrap();
        let eff = Effective::build_layered(&preset, std::slice::from_ref(&user), COMMANDS).unwrap();
        let b = eff.bindings();
        // Shadowed: "j" appears ONCE and the user's prepend wins.
        let js: Vec<_> = b.iter().filter(|(seq, _)| seq == "j").collect();
        assert_eq!(js.len(), 1, "duplicated shadowed binding: {b:?}");
        assert_eq!(js[0].1, "cursor.top", "the user layer must win");
        // Precedence order: the user's prepend before the preset.
        let pos = |wanted: &str| b.iter().position(|(seq, _)| seq == wanted).unwrap();
        assert!(pos("j") < pos("k"), "prepend before preset: {b:?}");
        assert!(
            b.iter()
                .any(|(seq, cmd)| seq == "g g" && *cmd == "cursor.top"),
            "the user's append appears in the help: {b:?}"
        );
    }

    /// HIGH (security review M4 Lua): `./.norte/keymap.toml` loads WITHOUT
    /// trust, so a hostile repo could rebind a common key (`j`, `enter`) to
    /// a `lua:` command from the USER's init.lua (no sandbox, no
    /// confirmation, with cwd = the hostile repo). `lua:` bindings
    /// originating in the PROJECT layer are DISCARDED (counted for the bar
    /// warning); project rebinds to builtins keep working; the same
    /// binding in a user layer DOES resolve.
    #[test]
    fn project_keymap_lua_is_discarded_with_a_warning() {
        // A frontend that HOSTS Lua (ADR 0110): with no `LUA_HOST`, the
        // user binding would say it is not available and this test would
        // not prove the discard.
        const COMMANDS: &[&str] = &["cursor.down", "cursor.up", LUA_HOST];
        let preset = parse_keymap(
            r#"
            [pane]
            keymap = [{ on = ["j"], run = "cursor.down" }]
        "#,
        )
        .unwrap();
        let layer = r#"
            [pane]
            prepend_keymap = [{ on = ["j"], run = "lua:pwn" }]
        "#;

        // PROJECT layer: the lua: binding is discarded — the key falls
        // back to the preset's builtin — and is counted for the warning.
        let mut project = parse_keymap(layer).unwrap();
        project.mark_project();
        let eff = Effective::build_layered(&preset, std::slice::from_ref(&project), COMMANDS)
            .expect("discarding is not a load error");
        assert_eq!(eff.discarded_lua_bindings(), 1, "counted for the warning");
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            run("cursor.down"),
            "the key falls back to the builtin, never to the project's lua:"
        );

        // The SAME binding in a USER layer (unmarked): resolves normally.
        let user = parse_keymap(layer).unwrap();
        let eff = Effective::build_layered(&preset, std::slice::from_ref(&user), COMMANDS).unwrap();
        assert_eq!(eff.discarded_lua_bindings(), 0);
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            run("lua:pwn"),
            "in a user layer the lua: binding is legitimate"
        );

        // Project rebind to a BUILTIN: keeps working (the discard is ONLY
        // for `lua:` — harmless project config is not broken).
        let mut project = parse_keymap(
            r#"
            [pane]
            prepend_keymap = [{ on = ["x"], run = "cursor.up" }]
        "#,
        )
        .unwrap();
        project.mark_project();
        let eff =
            Effective::build_layered(&preset, std::slice::from_ref(&project), COMMANDS).unwrap();
        assert_eq!(eff.discarded_lua_bindings(), 0);
        let mut r = Resolver::new(eff);
        assert_eq!(r.push(parse_chord("x").unwrap()), run("cursor.up"));
    }

    /// New (GUI-c T1): the engine does NOT know concrete commands — it
    /// validates against the `known_commands` list the CALLER passes it
    /// (each frontend has its own catalogue). With "foo.bar" in the list:
    /// OK; without it, `UnknownCommand`.
    #[test]
    fn build_validates_against_the_given_known_commands() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["x"], run = "foo.bar" }]"#,
        )
        .unwrap();
        // With "foo.bar" known: OK.
        assert!(Effective::build(&preset, None, &["foo.bar"]).is_ok());
        // Without it: UnknownCommand (the engine does NOT know concrete commands).
        assert!(matches!(
            Effective::build(&preset, None, &["otro.cmd"]),
            Err(KeymapError::UnknownCommand { .. })
        ));
    }

    /// GUI-c T2 review regression: a key the FRONTEND does not model (e.g.
    /// crossterm `BackTab`/`Media`, adapted to `None`) must break any
    /// multi-key sequence in progress — the old `from_event` ALWAYS pushed
    /// into the resolver (even with an exotic chord that never matched),
    /// which produced a `Miss` and cleared the pending state. An adapter
    /// that returns `Option`, with a caller that simply discards the
    /// `None`, leaves the INTERNAL pending state intact — `reset()` is the
    /// explicit equivalent of the `Miss` the adapter can no longer produce
    /// on its own.
    #[test]
    fn reset_breaks_the_pending_sequence() {
        let kf = parse_keymap(
            r#"[pane]
keymap = [{ on = ["g", "g"], run = "cursor.top" }]"#,
        )
        .unwrap();
        let eff = Effective::build_for(&kf, &[], &["cursor.top"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        assert!(matches!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('g'))),
            Resolution::Pending(_)
        ));
        r.reset();
        // After reset, a single 'g' is pending again (the sequence broke:
        // if it had NOT broken, this second 'g' would fire
        // Run("cursor.top") instead of Pending(1)).
        assert!(matches!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('g'))),
            Resolution::Pending(_)
        ));
    }

    /// USED TO pin `build_for_subset`: a PRESET binding to a command this
    /// frontend does not implement was skipped in silence, and a LAYER binding
    /// to the same name was an error — provenance decided the verdict. Since
    /// K1 the CATALOGUE decides: `app.help` is `Live`, so the binding SURVIVES
    /// as `NotHere` from either source, and only a name the vocabulary has
    /// never heard of is still fatal. The two halves of the old assertion are
    /// still here, both inverted.
    #[test]
    fn a_foreign_command_survives_whether_it_comes_from_the_preset_or_a_layer() {
        let preset = parse_keymap(
            "[pane]\nkeymap = [\n { on = [\"q\"], run = \"app.quit\" },\n { on = [\"f1\"], run = \"app.help\" },\n]\n",
        )
        .unwrap();
        let known = ["app.quit"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Browse)
            .expect("preset con extras construye");
        let all = eff.bindings_all();
        assert_eq!(
            all.iter()
                .find(|(seq, _, _)| seq == "f1")
                .map(|(_, run, avail)| (*run, *avail)),
            Some(("app.help", Availability::NotHere)),
            "the binding is no longer filtered out: it survives marked — {all:?}"
        );
        // …and it still does NOT RUN: `bindings()` only lists what is runnable.
        assert!(
            !eff.bindings().iter().any(|(seq, _)| seq == "f1"),
            "{all:?}"
        );
        // A LAYER that binds the same foreign command no longer fails either.
        let layer =
            parse_keymap("[pane]\nprepend_keymap = [{ on = [\"z\"], run = \"app.help\" }]\n")
                .unwrap();
        assert!(
            Effective::build_for(&preset, &[layer], &known, Screen::Browse).is_ok(),
            "a catalogue command is not a typo, wherever it comes from"
        );
        // What still dies: a name the catalogue does not know.
        let typo =
            parse_keymap("[pane]\nprepend_keymap = [{ on = [\"z\"], run = \"app.hlep\" }]\n")
                .unwrap();
        assert!(matches!(
            Effective::build_for(&preset, &[typo], &known, Screen::Browse),
            Err(KeymapError::UnknownCommand { .. })
        ));
    }

    /// The `lua:` name is validated by CHARSET before anything else: a
    /// `lua:` with an invalid name (outside `[a-z0-9._-]{1,64}`) has no way
    /// to sneak in through the catalogue's door — a `lua:` is never in it.
    #[test]
    fn invalid_lua_is_still_an_error() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["x"], run = "lua:Bad Name" }]"#,
        )
        .unwrap();
        match Effective::build_for(&preset, &[], &[], Screen::Browse) {
            Err(KeymapError::UnknownCommand { .. }) => {}
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }

    /// `build_diagnostics` (#102): reports EVERY unknown-command finding in
    /// ONE walk — no per-typo rebuild, no retry cap. Three distinct made-up
    /// `run` names in a layer must all come back as `UnknownCommand`
    /// diagnostics from a single call.
    #[test]
    fn build_diagnostics_reports_all_unknowns_in_one_pass() {
        let preset =
            parse_keymap("[pane]\nkeymap = [{ on = [\"q\"], run = \"app.quit\" }]\n").unwrap();
        let layer = parse_keymap(
            "[pane]\nappend_keymap = [\n { on = [\"x\"], run = \"typo.one\" },\n { on = [\"y\"], run = \"typo.two\" },\n { on = [\"z\"], run = \"typo.three\" },\n]\n",
        )
        .unwrap();
        let known = ["app.quit"];
        let diags = Effective::build_diagnostics(&preset, &[layer], &known, Screen::Browse);
        let unknowns: Vec<&str> = diags
            .iter()
            .filter_map(|d| match d {
                KeymapDiagnostic::UnknownCommand { run } => Some(run.as_str()),
                KeymapDiagnostic::Structural { .. } => None,
            })
            .collect();
        assert_eq!(
            unknowns,
            ["typo.one", "typo.two", "typo.three"],
            "{diags:?}"
        );
    }

    /// A `lua:<name>` binding whose name fails the charset is a `Structural`
    /// diagnostic (never fixable by adding it to `known`), NOT a recoverable
    /// `UnknownCommand` — this is exactly the non-convergent case #102's
    /// one-pass builder resolves by construction.
    #[test]
    fn build_diagnostics_lua_charset_invalid_es_structural() {
        let preset =
            parse_keymap("[pane]\nkeymap = [{ on = [\"x\"], run = \"lua:bad name!\" }]\n").unwrap();
        let diags = Effective::build_diagnostics(&preset, &[], &[], Screen::Browse);
        assert_eq!(diags.len(), 1, "{diags:?}");
        match &diags[0] {
            KeymapDiagnostic::Structural { message } => {
                assert!(message.contains("lua:bad name!"), "{message}");
            }
            d @ KeymapDiagnostic::UnknownCommand { .. } => {
                panic!("expected Structural, got {d:?}")
            }
        }
    }

    /// A well-formed keymap yields NO diagnostics (the caller reports
    /// `keymap-ok`).
    #[test]
    fn build_diagnostics_valid_keymap_without_findings() {
        let preset =
            parse_keymap("[pane]\nkeymap = [{ on = [\"q\"], run = \"app.quit\" }]\n").unwrap();
        let diags = Effective::build_diagnostics(&preset, &[], &["app.quit"], Screen::Browse);
        assert!(diags.is_empty(), "{diags:?}");
    }

    /// INVERTED (K1 T3). It used to be `subset_prefijo_filtrado_no_bloquea_
    /// secuencia`, and it pinned the opposite: a filtered PRESET binding
    /// vanished before `check_prefix_free`, so `"g"` (preset, unimplemented)
    /// left `"g g"` (layer) resolving cleanly. That is a load-time property
    /// being decided by what this build happens to run, and the direction is
    /// now the other one — the shape of the map is fixed at load (ADR 0006),
    /// so an unavailable `"g"` still blocks `"g g"`. Do not "fix" it back:
    /// a `"g"` that swallows the first key of `"g g"` in one frontend and not
    /// in the other is exactly the drift K1 exists to kill.
    #[test]
    fn an_unavailable_binding_still_blocks_the_prefix() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["g"], run = "pane.pack" }]"#,
        )
        .unwrap();
        let layer = parse_keymap(
            r#"[pane]
append_keymap = [{ on = ["g", "g"], run = "cursor.top" }]"#,
        )
        .unwrap();
        let known = ["cursor.top"];
        match Effective::build_for(&preset, &[layer], &known, Screen::Browse) {
            Err(KeymapError::AmbiguousPrefix { .. }) => {}
            other => panic!("expected AmbiguousPrefix, got {other:?}"),
        }
    }

    /// INVERTED (K1 T3). It used to be `subset_dedup_desenmascara_binding_
    /// global`, and it pinned a divergence that was deliberate at the time:
    /// the dedup (`seen.insert`, first-wins) only saw the bindings that
    /// SURVIVED the filter, so a `pane` binding this frontend could not run
    /// was dropped BEFORE the dedup and the `global` one underneath became
    /// active — the same key doing two different things depending on which
    /// frontend read the preset. Since K1 nothing is dropped: the specific
    /// context still wins, and the key that survives is the unavailable one,
    /// which will say so instead of quietly doing something else. Do not
    /// "fix" it back: falling through is how a Total Commander user gets a
    /// surprise instead of an answer.
    #[test]
    fn an_unavailable_binding_still_shadows() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["x"], run = "pane.pack" }]
[global]
keymap = [{ on = ["x"], run = "app.quit" }]"#,
        )
        .unwrap();
        let known = ["app.quit"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Browse)
            .expect("pane.x unavailable, global.x known");
        let all = eff.bindings_all();
        let hits: Vec<_> = all.iter().filter(|(seq, _, _)| seq == "x").collect();
        assert_eq!(hits.len(), 1, "dedup leaves ONE per sequence: {all:?}");
        assert_eq!(hits[0].1, "pane.pack", "the specific context wins");
        // Unavailable, and WHY does not matter: what is being tested is that
        // the shadow drops it just the same. (It used to be `NotBuilt` until
        // #132 left the table with no `Planned` commands; today it is
        // `NotHere`.)
        assert!(!matches!(hits[0].2, Availability::Here), "{:?}", hits[0].2);
        // `[global]`'s `app.quit` stays SHADOWED: it does not surface.
        assert!(
            !eff.bindings().iter().any(|(seq, _)| seq == "x"),
            "the key runs nothing — it will say why: {all:?}"
        );
    }

    /// An unreadable chord (`"megatecla"`) in a binding whose command is
    /// ALSO unknown: the sequence parse runs BEFORE consulting the
    /// catalogue (`raw.on.iter().map(parse_chord)`), so `BadChord` wins —
    /// structurally broken config is never declared "unavailable".
    #[test]
    fn bad_chord_wins_over_unknown_command() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["megatecla"], run = "gui.unknown" }]"#,
        )
        .unwrap();
        match Effective::build_for(&preset, &[], &[], Screen::Browse) {
            Err(KeymapError::BadChord { .. }) => {}
            other => panic!("expected BadChord, got {other:?}"),
        }
    }

    /// H1 (#24): the `dialog` context exists — a preset with [dialog]
    /// builds and resolves for `Screen::Dialog`.
    #[test]
    fn dialog_context_is_parsed_and_built() {
        let preset =
            parse_keymap("[dialog]\nkeymap = [{ on = [\"y\"], run = \"dialog.approve\" }]\n")
                .unwrap();
        let eff = Effective::build_for(&preset, &[], &["dialog.approve"], Screen::Dialog)
            .expect("construye");
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('y'))),
            run("dialog.approve")
        );
    }

    /// A user layer extends [dialog] with prepend and WINS.
    #[test]
    fn layer_can_extend_dialog() {
        let preset =
            parse_keymap("[dialog]\nkeymap = [{ on = [\"y\"], run = \"dialog.approve\" }]\n")
                .unwrap();
        let layer =
            parse_keymap("[dialog]\nprepend_keymap = [{ on = [\"y\"], run = \"dialog.deny\" }]\n")
                .unwrap();
        let eff = Effective::build_for(
            &preset,
            &[layer],
            &["dialog.approve", "dialog.deny"],
            Screen::Dialog,
        )
        .expect("construye");
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('y'))),
            run("dialog.deny")
        );
    }

    /// A layer with a full `keymap` in [dialog]: an error, as in the rest.
    #[test]
    fn has_full_keymap_ve_dialog() {
        let layer =
            parse_keymap("[dialog]\nkeymap = [{ on = [\"y\"], run = \"dialog.approve\" }]\n")
                .unwrap();
        assert!(layer.has_full_keymap());
    }

    /// The table [`paint_chord`] documents, walked over what `Display`
    /// actually writes — the round trip is the point: every spelling here is
    /// produced by formatting a real [`Chord`], never hand-written, so the
    /// table cannot drift from the `Display` it is the counterpart of.
    #[test]
    fn paint_chord_spells_a_key_the_way_the_docs_do() {
        for (raw, painted) in [
            ("f1", "F1"),
            ("f12", "F12"),
            ("shift+f8", "Shift+F8"),
            ("ctrl+alt+f5", "Ctrl+Alt+F5"),
            // `cmd` joined the modifier table; `Display` writes it FIRST, so
            // this fixture is also the canonical-order pin.
            ("cmd+f5", "Cmd+F5"),
            ("cmd+ctrl+alt+shift+f5", "Cmd+Ctrl+Alt+Shift+F5"),
            ("enter", "Enter"),
            ("tab", "Tab"),
            ("esc", "Esc"),
            ("backspace", "Backspace"),
            ("space", "Space"),
            ("plus", "Plus"),
            ("up", "Up"),
            ("down", "Down"),
            ("left", "Left"),
            ("right", "Right"),
            ("home", "Home"),
            ("end", "End"),
            ("pgup", "PgUp"),
            ("pgdn", "PgDn"),
            ("insert", "Insert"),
            ("delete", "Delete"),
        ] {
            assert_eq!(
                parse_chord(raw)
                    .expect("chord from the catalogue")
                    .to_string(),
                raw,
                "the fixture has to be what `Display` really writes"
            );
            assert_eq!(paint_chord(raw), painted, "{raw}");
        }
    }

    /// THE case that matters: a single printable character is painted
    /// EXACTLY as it is bound. Telling a reader to press `Y` is telling them
    /// to press Shift — and it is not only cosmetic, because `Char('Y')` is a
    /// different binding that `parse_chord` would resolve to a different key.
    /// Holds under a modifier too (`Ctrl+k`, never `Ctrl+K`).
    #[test]
    fn paint_chord_never_shifts_a_printable_key() {
        for chord in [
            "y",
            "n",
            "k",
            "G",
            "ctrl+k",
            "alt+p",
            "ctrl+alt+k",
            "ñ",
            "漢",
        ] {
            let painted = paint_chord(chord);
            let key = painted.rsplit('+').next().expect("there is always a key");
            let bound = chord.rsplit('+').next().expect("there is always a key");
            assert_eq!(key, bound, "{chord} -> {painted}: the KEY is not touched");
        }
        assert_eq!(paint_chord("ctrl+k"), "Ctrl+k");
        assert_eq!(
            paint_chord("G"),
            "G",
            "…and a bound uppercase letter stays uppercase"
        );
    }

    /// A multi-key SEQUENCE keeps its space join, and every key of it is
    /// spelled.
    #[test]
    fn paint_chord_spells_every_key_of_a_sequence() {
        assert_eq!(paint_chord("g g"), "g g");
        assert_eq!(paint_chord("g home"), "g Home");
        assert_eq!(paint_chord("ctrl+x f5"), "Ctrl+x F5");
    }

    /// Nothing unrecognised is invented: an `f` that is not a function key,
    /// a modifier spelled wrong, an empty string — all pass through.
    #[test]
    fn paint_chord_invents_nothing() {
        for raw in ["", "fx", "f", "megakey", "CTRL+k", "f5x"] {
            assert_eq!(
                paint_chord(raw),
                raw,
                "{raw:?} is not recognized: passes through as is"
            );
        }
    }

    /// A command the catalogue calls Live but THIS frontend does not implement
    /// is kept as `NotHere` instead of being filtered away in silence — the
    /// H3f bug.
    ///
    /// **And it is the only live coverage of "a binding that cannot run
    /// SURVIVES marked".** There used to be a twin test with a `Planned`
    /// command, which was the case of a preset faithful to Total Commander
    /// when a third of it named things not yet built. #132 built the last
    /// one, the table ran out of `Planned` entries, and a test over data
    /// that no longer exists proves nothing: it was retired. When a
    /// promised capability exists again, its twin comes back with it.
    #[test]
    fn a_live_command_this_frontend_does_not_implement_is_not_here() {
        let preset = parse_keymap(
            r#"
[global]
keymap = [ { on = ["f1"], run = "app.help" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["pane.copy"], Screen::Browse).unwrap();
        let all = eff.bindings_all();
        let (_, _, avail) = all
            .iter()
            .find(|(_, run, _)| *run == "app.help")
            .expect("cannot disappear");
        assert_eq!(*avail, Availability::NotHere);
    }

    /// `bindings()` keeps its old meaning — only what actually runs — so the
    /// help and the hints render exactly as before this change.
    #[test]
    fn bindings_only_returns_what_is_executable() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [
    { on = ["f5"], run = "pane.copy" },
    { on = ["alt+f1"], run = "pane.pack" },
]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["pane.copy"], Screen::Browse).unwrap();
        let runs: Vec<&str> = eff.bindings().into_iter().map(|(_, run)| run).collect();
        assert_eq!(runs, vec!["pane.copy"]);
    }

    /// A name absent from the CATALOGUE is a typo and still dies loudly, in a
    /// preset and in a user layer alike. "Not built yet" and "you misspelled
    /// it" stop being the same event; they must not become the same event
    /// again.
    #[test]
    fn a_name_outside_the_catalogue_is_still_an_error() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [ { on = ["f5"], run = "pane.copyy" } ]
"#,
        )
        .unwrap();
        let e = Effective::build_for(&preset, &[], &["pane.copy"], Screen::Browse).unwrap_err();
        assert!(matches!(e, KeymapError::UnknownCommand { .. }), "{e:?}");
    }

    /// An unavailable binding SHADOWS a lower-precedence available one. If a
    /// preset puts `alt+f1` in `[pane]`, the key must say "not built yet"
    /// rather than quietly falling through to whatever `[global]` had —
    /// falling through is how a Total Commander user gets a surprise instead
    /// of an answer.
    #[test]
    fn an_unavailable_binding_shadows_the_global_one() {
        let preset = parse_keymap(
            r#"
[global]
keymap = [ { on = ["alt+f1"], run = "pane.refresh" } ]

[pane]
keymap = [ { on = ["alt+f1"], run = "pane.pack" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["pane.refresh"], Screen::Browse).unwrap();
        let all = eff.bindings_all();
        let hits: Vec<_> = all.iter().filter(|(seq, _, _)| seq == "alt+f1").collect();
        assert_eq!(hits.len(), 1, "dedup leaves ONE per sequence: {all:?}");
        assert_eq!(hits[0].1, "pane.pack", "the specific context wins");
        // Unavailable; why does not matter here (`NotBuilt` until #132
        // emptied the `Planned` list, `NotHere` now).
        assert!(!matches!(hits[0].2, Availability::Here), "{:?}", hits[0].2);
    }

    /// Unavailable bindings take part in the prefix-free check: the shape of
    /// the map is a load-time property (ADR 0006), independent of what runs.
    #[test]
    fn an_unavailable_binding_still_counts_toward_prefix_free() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [
    { on = ["g"], run = "pane.pack" },
    { on = ["g", "g"], run = "cursor.top" },
]
"#,
        )
        .unwrap();
        let e = Effective::build_for(&preset, &[], &["cursor.top"], Screen::Browse).unwrap_err();
        assert!(matches!(e, KeymapError::AmbiguousPrefix { .. }), "{e:?}");
    }

    /// Pressing a key bound to something norte has not built returns a third
    /// outcome. `Reset` would be indistinguishable from an unbound key, which is
    /// precisely the silence this work exists to remove.
    #[test]
    fn an_unavailable_key_resolves_to_unavailable() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [ { on = ["alt+f1"], run = "pane.pack" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["pane.copy"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        let chord = parse_chord("alt+f1").unwrap();
        match r.push(chord) {
            Resolution::Unavailable { command, why } => {
                assert_eq!(command, "pane.pack");
                // `NotHere` since #132: the command exists and this
                // frontend does not implement it. What is being tested is
                // that the key resolves to "unavailable" and runs NOTHING.
                assert!(matches!(why, Availability::NotHere), "{why:?}");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
        assert!(r.pending().is_empty(), "the sequence must come out clean");
    }

    /// The message must NAME the command and, when the reason exists, carry it —
    /// a "not available" with no subject is the silence with extra steps.
    #[test]
    fn the_not_available_message_names_the_command_and_the_reason() {
        let m = unavailable_message(
            "pane.pack",
            Availability::NotBuilt {
                reason: "keymap-reason-archive-write",
                issue: 132,
            },
        );
        assert!(m.contains("pane.pack"), "{m}");
        assert!(m.contains("132"), "{m}");
        // The ID must have been TRANSLATED, not interpolated raw.
        assert!(!m.contains("keymap-reason-"), "{m}");

        let m = unavailable_message("pane.hotlist", Availability::NotHere);
        assert!(m.contains("pane.hotlist"), "{m}");
    }

    /// encoding-auditor MINOR 6: the assertion above runs in the AMBIENT
    /// locale (`Lang::from_env`), so exactly one of the two is pinned and
    /// which one depends on the developer's `LANG`. A locale that dropped
    /// `{ $issue }` would silently lose the issue number — undoing, at the
    /// last step, precisely what `every_planned_one_has_a_reason_and_an_issue` exists
    /// to guarantee. Pin both explicitly.
    #[test]
    fn the_not_available_message_carries_the_three_arguments_in_both_locales() {
        for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
            let m = norte_i18n::ta_in(
                lang,
                "keymap-unavailable-not-built",
                &[
                    ("command", "pane.pack"),
                    ("reason", "MOTIVO"),
                    ("issue", "132"),
                ],
            );
            assert!(m.contains("pane.pack"), "{lang:?}: {m}");
            assert!(m.contains("MOTIVO"), "{lang:?}: {m}");
            assert!(m.contains("132"), "{lang:?}: {m}");

            let m = norte_i18n::ta_in(
                lang,
                "keymap-unavailable-not-here",
                &[("command", "pane.hotlist")],
            );
            assert!(m.contains("pane.hotlist"), "{lang:?}: {m}");
        }
    }

    /// The ignored-count sentence must NAME both halves: which command refused
    /// the count and which number went nowhere. "A count was ignored" tells
    /// the user nothing they can act on.
    #[test]
    fn the_ignored_counter_message_names_command_and_number() {
        let m = count_ignored_message("app.quit", 3);
        assert!(m.contains("app.quit"), "{m}");
        assert!(m.contains('3'), "{m}");
    }

    /// Same reasoning as `..._en_ambos_locales` above: the assertion over the
    /// ambient locale pins exactly one of the two, and which one depends on
    /// the developer's `LANG`. A locale that dropped `{ $count }` would lose
    /// the number in half the world.
    #[test]
    fn the_ignored_counter_message_carries_both_arguments_in_both_locales() {
        for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
            let m = norte_i18n::ta_in(
                lang,
                "keymap-count-ignored",
                &[("command", "app.quit"), ("count", "3")],
            );
            assert!(m.contains("app.quit"), "{lang:?}: {m}");
            assert!(m.contains('3'), "{lang:?}: {m}");
        }
    }

    /// encoding-auditor MINOR 5: `unavailable_message` interpolates its
    /// `command` into a terminal sentence WITHOUT masking, and the argument
    /// that this is safe is that the string is always byte-equal to a
    /// `CommandDef.name` — `catalogue::lookup` is byte-exact `&str` equality,
    /// so a hostile `run` can only ever miss and become a load error. That
    /// argument is real, and until now nothing held it in place. Pin both
    /// halves: the catalogue is ASCII, and a hostile `run` classifies as
    /// `UnknownCommand` rather than surfacing as `Unavailable`.
    #[test]
    fn a_hostile_run_is_a_load_error_never_an_unavailability() {
        for d in CATALOGUE {
            assert!(
                d.name.bytes().all(|b| b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || matches!(b, b'.' | b'-')),
                "{} is not ASCII-safe — the unavailability message \
                 interpolates it WITHOUT masking and that is the only reason \
                 it can",
                d.name
            );
        }
        for h in norte_testkit::corpus::hostile_runs() {
            // Through `parse_keymap`, with the hazards written as `\uXXXX` —
            // a TOML basic string is exactly how such a name would ship.
            let escaped: String = h
                .run
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '.' {
                        c.to_string()
                    } else {
                        format!("\\u{:04X}", c as u32)
                    }
                })
                .collect();
            let kf = parse_keymap(&format!(
                "[pane]\nkeymap = [{{ on = [\"alt+f1\"], run = \"{escaped}\" }}]\n"
            ))
            .unwrap_or_else(|e| panic!("{}: the TOML must parse: {e}", h.id));
            let err = Effective::build_for(&kf, &[], &["pane.copy"], Screen::Browse)
                .expect_err(&format!("{}: loading must fail — {}", h.id, h.why));
            assert!(
                matches!(err, KeymapError::UnknownCommand { .. }),
                "{}: {err:?}",
                h.id
            );
        }
        // K2a widened the argument. `count_ignored_message` interpolates a
        // command name unmasked too, and its reachable domain is STRICTLY
        // larger than `unavailable_message`'s: a `lua:` command never becomes
        // `Unavailable` (it is always `Availability::Here`) but a count over
        // one is ALWAYS `Ignored`, so a Lua name reaches the status bar
        // through this sentence and no other. It is safe because
        // `valid_lua_name` is ASCII by construction — pin that leg too, so
        // the day someone widens the Lua charset this fails instead of the
        // status line.
        for hostile in ["lua:aa\u{202E}bb", "lua:x\u{0007}y", "lua:ñ"] {
            let name = hostile.strip_prefix("lua:").expect("lua: prefix");
            assert!(
                !valid_lua_name(name),
                "{hostile}: a lua name with hazards must be rejected by the charset"
            );
        }
        let m = count_ignored_message("lua:mi-script.v2", 3);
        assert!(
            !m.chars().any(|c| c.is_control()
                || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}')),
            "the ignored-count message cannot carry hazards: {m:?}"
        );
    }

    /// The tripwire for the obvious next change. `unavailable_message` names
    /// the COMMAND but not the KEY, and "Alt+F1: not built yet" is the
    /// natural improvement someone will make in K3. The chord is exactly
    /// where the hostile bytes live — `parse_chord` accepts any lone
    /// codepoint as `Char`, `Chord`'s `Display` is raw on purpose, and
    /// `norte_i18n` runs with `set_use_isolating(false)` (a terminal wants no
    /// FSI/PDI cells), so there is no second line of defence: one RLO
    /// reorders the whole status line. Trivially true today; it fails the day
    /// the chord arrives unmasked, which is the entire point.
    #[test]
    fn the_not_available_message_never_carries_a_terminal_hazard() {
        for d in CATALOGUE {
            for why in [
                Availability::NotHere,
                Availability::NotBuilt {
                    reason: "keymap-reason-archive-write",
                    issue: 132,
                },
            ] {
                let m = unavailable_message(d.name, why);
                assert!(
                    !m.chars().any(norte_encoding::is_terminal_hazard),
                    "{}: {:?}",
                    d.name,
                    m.escape_debug()
                );
            }
        }
    }

    /// The same tripwire for the SHORT form (K3b), which is now the one that
    /// reaches a pipe (`norte help keys`) and a JSON field, in both locales.
    #[test]
    fn the_short_message_also_does_not_carry_a_terminal_hazard() {
        for d in CATALOGUE {
            let why = match d.status {
                Status::Live => Availability::NotHere,
                Status::Planned { reason, issue } => Availability::NotBuilt { reason, issue },
            };
            for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
                let m = short_unavailable_message(why, lang);
                assert!(
                    !m.chars().any(norte_encoding::is_terminal_hazard),
                    "{} [{lang:?}]: {:?}",
                    d.name,
                    m.escape_debug()
                );
            }
        }
    }

    /// The short form SAYS the key is not built, in both locales. The wording
    /// is not decoration: `norte help keys` writes to a pipe and cannot dim a
    /// row, so `writing archives (#132)` on its own reads as a description of
    /// what the key does. A later edit back to the bare reason would restore
    /// exactly the confusion K3b removed, and would otherwise be green.
    #[test]
    fn the_short_message_says_it_is_not_built_in_both_locales() {
        let why = Availability::NotBuilt {
            reason: "keymap-reason-archive-write",
            issue: 132,
        };
        for (lang, marker) in [
            (norte_i18n::Lang::En, "not built"),
            (norte_i18n::Lang::Es, "aún no construido"),
        ] {
            let m = short_unavailable_message(why, lang);
            assert!(m.contains(marker), "{lang:?}: {m}");
            assert!(m.contains("132"), "{lang:?}: {m}");
            assert!(
                !m.contains("keymap-reason-"),
                "the reason is a Fluent id and must be TRANSLATED: {lang:?}: {m}"
            );
        }
    }

    /// Masking runs FIRST and the cosmetics cannot undo it (encoding audit
    /// H1): every hostile chord of the canonical corpus — bindable from an
    /// untrusted project `./.norte/keymap.toml` — comes out as `U+FFFD`, with
    /// no hazard surviving, whether it is the whole chord or one key of a
    /// sequence under a modifier.
    #[test]
    fn paint_chord_masks_before_it_prettifies() {
        for hazard in norte_testkit::corpus::hostile_chords() {
            for raw in [
                hazard.token.to_string(),
                format!("ctrl+{}", hazard.token),
                format!("f5 {}", hazard.token),
            ] {
                let painted = paint_chord(&raw);
                assert!(
                    !painted.chars().any(norte_encoding::is_terminal_hazard),
                    "[{}] raw hazard after painting {raw:?}: {painted:?}",
                    hazard.id
                );
                assert!(
                    painted.contains('\u{FFFD}'),
                    "[{}] the hazard must come out as U+FFFD: {painted:?}",
                    hazard.id
                );
            }
        }
    }

    // --- K2a: the numeric count -----------------------------------------

    /// `5j` runs the command five times. The count rides WITH the command; the
    /// frontend is what repeats, so no command signature changes.
    #[test]
    fn a_counter_arrives_with_the_command() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["j"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        assert_eq!(r.push(parse_chord("5").unwrap()), Resolution::Counting(5));
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::Repeat(5),
            }
        );
    }

    /// Digits accumulate left to right, and the count survives a multi-key
    /// sequence: `12gj` is twelve, not one then two.
    ///
    /// The sequence deliberately runs `cursor.down` and not `cursor.top`.
    /// K2a's plan wrote this fixture as vim's `12gg`, but the catalogue marks
    /// `cursor.top` as taking NO count — and it is right: the count REPEATS
    /// the dispatch, so twelve "go to the top" is still the top. `12gg`
    /// meaning "go to line 12" would need the count to reach the command,
    /// which is exactly the design this task rejected. A count over
    /// `cursor.top` is therefore [`Count::Ignored`], pinned by
    /// `a_counter_on_a_command_without_a_counter_is_not_swallowed`.
    #[test]
    fn digits_accumulate_and_survive_a_sequence() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["g", "j"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        assert_eq!(r.push(parse_chord("1").unwrap()), Resolution::Counting(1));
        assert_eq!(r.push(parse_chord("2").unwrap()), Resolution::Counting(12));
        assert_eq!(r.push(parse_chord("g").unwrap()), Resolution::Pending(1));
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::Repeat(12),
            }
        );
    }

    /// A digit typed MID-sequence is a key, not a count: with `g` pending,
    /// `5` must reach the lookup. Otherwise a preset could never bind a
    /// sequence whose second chord is a digit, and the count would silently
    /// eat it.
    #[test]
    fn a_digit_mid_sequence_is_a_key() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["g", "5"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        assert_eq!(r.push(parse_chord("2").unwrap()), Resolution::Counting(2));
        assert_eq!(r.push(parse_chord("g").unwrap()), Resolution::Pending(1));
        assert_eq!(
            r.push(parse_chord("5").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::Repeat(2),
            }
        );
    }

    /// A count over a command the catalogue says takes none is NOT swallowed:
    /// the command runs once and the frontend is told the count was ignored.
    #[test]
    fn a_counter_on_a_command_without_a_counter_is_not_swallowed() {
        let preset = parse_keymap(
            r#"
counts = true

[global]
keymap = [ { on = ["q"], run = "app.quit" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["app.quit"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        r.push(parse_chord("3").unwrap());
        assert_eq!(
            r.push(parse_chord("q").unwrap()),
            Resolution::Run {
                command: "app.quit".to_owned(),
                count: Count::Ignored(3),
            }
        );
    }

    /// Zero never STARTS a count — `0` stays a bindable key, which is what
    /// vim's "go to the first column" and mc's mask keys rely on. It does
    /// accumulate once a count is open: `10` is ten.
    #[test]
    fn zero_does_not_open_a_counter_but_it_does_accumulate() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [
    { on = ["0"], run = "cursor.top" },
    { on = ["j"], run = "cursor.down" },
]
"#,
        )
        .unwrap();
        let known = ["cursor.top", "cursor.down"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        // A bare 0 is the binding, not a count.
        assert_eq!(
            r.push(parse_chord("0").unwrap()),
            Resolution::Run {
                command: "cursor.top".to_owned(),
                count: Count::None
            }
        );
        // But 1 then 0 is ten.
        assert_eq!(r.push(parse_chord("1").unwrap()), Resolution::Counting(1));
        assert_eq!(r.push(parse_chord("0").unwrap()), Resolution::Counting(10));
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::Repeat(10)
            }
        );
    }

    /// Four digits is the ceiling. A fifth is dropped rather than wrapping the
    /// accumulator — 99999 must not silently become something else.
    #[test]
    fn the_counter_caps_at_four_digits() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["j"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        for _ in 0..5 {
            r.push(parse_chord("9").unwrap());
        }
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::Repeat(9999)
            }
        );
    }

    /// Esc clears the count as well as the pending sequence. A count left
    /// stuck to the next keystroke is the worst failure this feature can have.
    #[test]
    fn esc_clears_the_counter() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["j"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        r.push(parse_chord("7").unwrap());
        assert_eq!(r.push(parse_chord("esc").unwrap()), Resolution::Reset);
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::None
            }
        );
    }

    /// An unbound key clears the count too — otherwise a typo leaves a number
    /// glued to whatever you press next.
    #[test]
    fn a_key_without_a_binding_clears_the_counter() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["j"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        r.push(parse_chord("4").unwrap());
        assert_eq!(r.push(parse_chord("z").unwrap()), Resolution::Reset);
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::None
            }
        );
    }

    /// A key the FRONTEND does not model (`Resolver::reset`, the explicit
    /// equivalent of a miss) clears the count for the same reason an unbound
    /// key does: a number must never outlive the keystroke that ended it.
    #[test]
    fn reset_clears_the_counter() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["j"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        r.push(parse_chord("6").unwrap());
        assert_eq!(r.count(), Some(6));
        r.reset();
        assert_eq!(r.count(), None);
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::None
            }
        );
    }

    /// Without the preset flag a digit is just a key: `orthodox` and `cua`
    /// must not grow counts behind their users' backs.
    #[test]
    fn without_the_presets_flag_a_digit_is_a_key() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [ { on = ["5"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(parse_chord("5").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::None
            }
        );
    }

    /// A digit with a modifier was never a count: `ctrl+5` is an ordinary
    /// chord, even with counts on.
    #[test]
    fn a_digit_with_a_modifier_is_not_a_counter() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["ctrl+5"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(parse_chord("ctrl+5").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::None
            }
        );
    }

    /// The count POLICY belongs to the preset: a user layer that could flip it
    /// on would silently change what every digit key means.
    #[test]
    fn a_user_layer_cannot_turn_on_the_counters() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [ { on = ["j"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let layer = parse_keymap(
            r#"
counts = true

[pane]
prepend_keymap = [ { on = ["k"], run = "cursor.up" } ]
"#,
        )
        .unwrap();
        let known = ["cursor.down", "cursor.up"];
        let e = Effective::build_for(&preset, &[layer], &known, Screen::Browse).unwrap_err();
        assert!(
            matches!(e, KeymapError::WrongLayerKey { key: "counts", .. }),
            "{e:?}"
        );
    }

    /// With counts on, a digit key bound in the same context is a LOAD error,
    /// not silent precedence. Same spirit as prefix-free: the conflict
    /// surfaces when the file loads, not when a finger slips.
    #[test]
    fn a_digit_bound_with_counters_is_a_load_error() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["5"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let e = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap_err();
        assert!(
            matches!(e, KeymapError::DigitBoundWithCounts { .. }),
            "{e:?}"
        );
    }

    /// `0` is exempt, because a count never starts with zero — binding it
    /// stays legal even with counts on.
    #[test]
    fn zero_is_still_bindable_with_counters() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["0"], run = "cursor.top" } ]
"#,
        )
        .unwrap();
        Effective::build_for(&preset, &[], &["cursor.top"], Screen::Browse)
            .expect("0 with counts is legal");
    }

    /// "Digit" means ASCII 0-9 and nothing else, in BOTH paths — they share
    /// `digit_of`, and `char::to_digit(10)` is ASCII-only. So U+0665 ARABIC-
    /// INDIC DIGIT FIVE and U+FF15 FULLWIDTH FIVE are ordinary bindable keys
    /// that load fine AND never open a count. Pinned in both directions
    /// because the two answers must stay the same one: an "improvement" to
    /// `is_numeric()` in either path would make a key that loads as a binding
    /// and then resolves as a count, or the reverse.
    #[test]
    fn a_non_ascii_digit_is_a_normal_key_on_both_paths() {
        for exotic in ['\u{0665}', '\u{FF15}'] {
            let preset = parse_keymap(&format!(
                "counts = true\n\n[pane]\nkeymap = [ {{ on = [\"{exotic}\"], run = \"cursor.down\" }} ]\n"
            ))
            .unwrap();
            let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse)
                .unwrap_or_else(|e| panic!("{exotic:?} must be bindable: {e:?}"));
            let mut r = Resolver::new(eff);
            assert_eq!(
                r.push(parse_chord(&exotic.to_string()).unwrap()),
                Resolution::Run {
                    command: "cursor.down".to_owned(),
                    count: Count::None,
                },
                "{exotic:?} does not open a count: it is a key"
            );
        }
    }

    /// A digit MID-sequence is an ordinary key: the rule looks at the first
    /// chord only, exactly like the accumulator, which never opens a count
    /// with a sequence in flight.
    #[test]
    fn a_digit_mid_sequence_is_still_bindable_with_counters() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["g", "5"], run = "cursor.top" } ]
"#,
        )
        .unwrap();
        Effective::build_for(&preset, &[], &["cursor.top"], Screen::Browse)
            .expect("a non-initial digit is just another key");
    }

    /// Specification §12: Tab switches panes and a preset may not take it.
    /// K2b imports four foreign keymaps, which is when this stops being
    /// theoretical.
    #[test]
    fn a_preset_cannot_rebind_tab() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [ { on = ["tab"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let e = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap_err();
        assert!(matches!(e, KeymapError::SacredKey { .. }), "{e:?}");
    }

    /// Tab may not OPEN a sequence either. A preset that binds `["tab","j"]`
    /// and no bare `tab` would leave Tab sitting pending, which loses pane
    /// switching just as completely as rebinding it (specification §12).
    #[test]
    fn tab_cannot_open_a_sequence_either() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [ { on = ["tab", "j"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let e = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap_err();
        assert!(matches!(e, KeymapError::SacredKey { .. }), "{e:?}");
    }

    /// The prohibition is on the BROWSE screen only. Every bundled preset
    /// binds `tab` to `dialog.pane` inside `[dialog]`, and that is not pane
    /// switching — blanket-banning the key would break the dialogs we ship.
    #[test]
    fn tab_is_still_free_in_the_dialog_context() {
        let preset = parse_keymap(
            r#"
[dialog]
keymap = [ { on = ["tab"], run = "dialog.pane" } ]
"#,
        )
        .unwrap();
        Effective::build_for(&preset, &[], &["dialog.pane"], Screen::Dialog)
            .expect("tab in dialog is legal");
    }

    /// A user layer cannot take Tab either — the rule is about the effective
    /// map, not about who wrote the line.
    #[test]
    fn a_user_layer_cannot_take_tab_either() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [ { on = ["tab"], run = "pane.switch" } ]
"#,
        )
        .unwrap();
        let layer = parse_keymap(
            r#"
[pane]
prepend_keymap = [ { on = ["tab"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let known = ["pane.switch", "cursor.down"];
        let e = Effective::build_for(&preset, &[layer], &known, Screen::Browse).unwrap_err();
        assert!(matches!(e, KeymapError::SacredKey { .. }), "{e:?}");
    }

    /// `norte doctor` must not be the one path that stays quiet: both new
    /// rules are reported by the diagnostic walk too, which is the contract
    /// [`check_binding`]'s rustdoc states for every load-time rule.
    #[test]
    fn the_diagnostic_also_reports_the_two_new_rules() {
        let digito = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["5"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let d = Effective::build_diagnostics(&digito, &[], &["cursor.down"], Screen::Browse);
        assert!(
            d.iter().any(|f| matches!(
                f,
                KeymapDiagnostic::Structural { message } if message.contains('5')
            )),
            "{d:?}"
        );

        let tab = parse_keymap(
            r#"
[pane]
keymap = [ { on = ["tab"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let d = Effective::build_diagnostics(&tab, &[], &["cursor.down"], Screen::Browse);
        assert!(
            d.iter().any(|f| matches!(
                f,
                KeymapDiagnostic::Structural { message } if message.contains("tab")
            )),
            "{d:?}"
        );
    }

    /// The two loaders must denounce the SAME defect, which is the contract
    /// [`check_binding`]'s rustdoc states for every load-time rule: they share
    /// the checks precisely so they cannot drift.
    ///
    /// `the_diagnostic_also_reports_the_two_new_rules` proves less than
    /// it looks. It pins two keymaps and asks only whether SOMETHING
    /// `Structural` came back mentioning `5` or `tab` — a `build_diagnostics`
    /// that had lost `check_sacred` but kept `check_prefix_free` would still
    /// pass it if any other rule fired, and nothing at all pins the rules the
    /// two new ones were bolted next to. This walks every rule, one keymap per
    /// rule, and compares the RENDERED message against the error
    /// [`Effective::build_for`] returns for the same input: a rule wired into
    /// one path and not the other fails here, and so does a rule wired into
    /// both with different arguments (`check_digits_free(.., false)` in the
    /// diagnostic walk, say).
    #[test]
    fn both_loaders_report_the_same_defect() {
        const KNOWN: &[&str] = &["app.quit", "cursor.down", "cursor.top", "pane.switch"];
        // (name, preset, user layer)
        let cases: &[(&str, &str, Option<&str>)] = &[
            (
                "invalid key",
                "[pane]\nkeymap = [{ on = [\"megatecla\"], run = \"cursor.down\" }]\n",
                None,
            ),
            (
                "empty sequence",
                "[pane]\nkeymap = [{ on = [], run = \"cursor.down\" }]\n",
                None,
            ),
            (
                "esc inside a sequence",
                "[pane]\nkeymap = [{ on = [\"esc\", \"a\"], run = \"cursor.down\" }]\n",
                None,
            ),
            (
                "unknown command",
                "[pane]\nkeymap = [{ on = [\"x\"], run = \"typo.no-existe\" }]\n",
                None,
            ),
            (
                "lua name outside the charset",
                "[pane]\nkeymap = [{ on = [\"x\"], run = \"lua:Nombre Malo\" }]\n",
                None,
            ),
            (
                "ambiguous prefix",
                "[pane]\nkeymap = [\n { on = [\"z\"], run = \"cursor.down\" },\n { on = [\"z\", \"z\"], run = \"cursor.top\" },\n]\n",
                None,
            ),
            (
                "key in the wrong layer",
                "[pane]\nkeymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n",
                Some("[pane]\nkeymap = [{ on = [\"k\"], run = \"cursor.top\" }]\n"),
            ),
            (
                "the layer enables counts",
                "[pane]\nkeymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n",
                Some(
                    "counts = true\n[pane]\nprepend_keymap = [{ on = [\"k\"], run = \"cursor.top\" }]\n",
                ),
            ),
            (
                "digit bound with counts",
                "counts = true\n[pane]\nkeymap = [{ on = [\"5\"], run = \"cursor.down\" }]\n",
                None,
            ),
            (
                "tab rebound",
                "[pane]\nkeymap = [{ on = [\"tab\"], run = \"cursor.down\" }]\n",
                None,
            ),
            (
                "tab opening a sequence",
                "[pane]\nkeymap = [{ on = [\"tab\", \"j\"], run = \"cursor.down\" }]\n",
                None,
            ),
        ];
        for (name, preset_src, layer_src) in cases {
            let preset = parse_keymap(preset_src).unwrap_or_else(|e| panic!("{name}: {e}"));
            let layers: Vec<KeymapFile> = layer_src
                .iter()
                .map(|s| parse_keymap(s).unwrap_or_else(|e| panic!("{name}: layer: {e}")))
                .collect();
            let e = Effective::build_for(&preset, &layers, KNOWN, Screen::Browse)
                .err()
                .unwrap_or_else(|| panic!("{name}: build_for accepted it"));
            let d = Effective::build_diagnostics(&preset, &layers, KNOWN, Screen::Browse);
            let same = d.iter().any(|f| match f {
                KeymapDiagnostic::Structural { message } => *message == e.to_string(),
                // The only finding that is NOT rendered from the error: a
                // plain unknown name is recoverable, so it travels typed.
                // The `run` is compared, which is what identifies it.
                KeymapDiagnostic::UnknownCommand { run } => {
                    matches!(&e, KeymapError::UnknownCommand { run: r } if r == run)
                }
            });
            assert!(
                same,
                "{name}: build_for said {e:?}, the diagnostic said {d:?}"
            );
        }

        // And the other side of the contract: what `build_for` accepts
        // cannot leave findings. K2a's two rules each have a LEGAL shape
        // (`0` bound with counts on, `tab` on its reserved command), and a
        // false positive here would fill `norte doctor` with noise about a
        // healthy keymap.
        let clean = parse_keymap(
            r#"
counts = true

[pane]
keymap = [
    { on = ["tab"], run = "pane.switch" },
    { on = ["0"], run = "cursor.top" },
    { on = ["g", "5"], run = "cursor.down" },
]
"#,
        )
        .expect("the clean keymap parses");
        Effective::build_for(&clean, &[], KNOWN, Screen::Browse).expect("build_for accepts it");
        let d = Effective::build_diagnostics(&clean, &[], KNOWN, Screen::Browse);
        assert!(d.is_empty(), "diagnostic false positive: {d:?}");
    }

    /// The three bundled presets must survive both rules unchanged.
    #[test]
    fn the_factory_presets_pass_both_new_rules() {
        for name in presets::NAMES {
            let src = presets::source(name).expect("NAMES resolves");
            let kf = parse_keymap(src).expect("preset parses");
            for screen in [Screen::Browse, Screen::Viewer, Screen::Dialog] {
                let known = preset_commands(screen);
                let known: Vec<&str> = known.iter().map(String::as_str).collect();
                Effective::build_for(&kf, &[], &known, screen)
                    .unwrap_or_else(|e| panic!("{name} in {screen:?}: {e:?}"));
            }
        }
    }

    /// **norte's OWN surfaces have a key in ALL presets.**
    ///
    /// A preset is a transcription of the original manager, and those
    /// managers had no settings screen, no extension manager and no command
    /// palette: there was nothing to transcribe, so four of the seven
    /// (`krusader`, `far`, `norton`, `total-commander`) came out with NONE
    /// of the three. The effect for whoever uses them is that norte's own
    /// configuration cannot be reached from the keyboard — not even through
    /// the palette, which is the path to a command with no key.
    ///
    /// Fidelity is transcribing what the original HAD, not silencing what
    /// norte has on top. This test is the line that stops that in the next
    /// preset that comes in.
    #[test]
    fn every_preset_reaches_nortes_own_surfaces() {
        // `app.help` is in the list on purpose even though all seven have
        // it today: it is the one most missed when it is missing, and the
        // test has to say so before the user does.
        const IMPRESCINDIBLES: &[&str] =
            &["app.help", "app.settings", "app.extensions", "app.palette"];
        let known = preset_commands(Screen::Browse);
        let known: Vec<&str> = known.iter().map(String::as_str).collect();
        for name in presets::NAMES {
            let src = presets::source(name).expect("NAMES resolves");
            let kf = parse_keymap(src).expect("preset parses");
            let eff = Effective::build_for(&kf, &[], &known, Screen::Browse)
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
            for cmd in IMPRESCINDIBLES {
                assert!(
                    eff.bindings().iter().any(|(_, c)| c == cmd),
                    "preset {name}: `{cmd}` has no key, so that screen cannot \
                     be reached from the keyboard"
                );
            }
        }
    }

    /// **Marking while moving is in all SEVEN, or in none.**
    ///
    /// The whole family (`shift`+arrows, `shift`+page, `shift`+ends) was
    /// added all at once, and that is exactly the moment a preset falls
    /// behind with nothing saying so: the catalogue announces the command,
    /// the reference sheet prints it, the palette offers it, and the
    /// keyboard of whoever uses that preset does nothing. It has happened
    /// three times (#228, #250, and the nine `ctrl+<UPPERCASE>` six presets
    /// carried that no terminal delivers).
    ///
    /// Kept apart from `every_preset_reaches_nortes_own_surfaces`
    /// because the argument is different: that one is about screens no
    /// original had; this is a family the originals DO have — Krusader
    /// documents it whole — and that none of the seven can afford to have
    /// half of.
    #[test]
    fn all_seven_presets_mark_while_moving() {
        const FAMILY: &[&str] = &[
            "mark.toggle-up",
            "mark.toggle-page-down",
            "mark.toggle-page-up",
            "mark.to-top",
            "mark.to-bottom",
        ];
        let known = preset_commands(Screen::Browse);
        let known: Vec<&str> = known.iter().map(String::as_str).collect();
        for name in presets::NAMES {
            let src = presets::source(name).expect("NAMES resolves");
            let kf = parse_keymap(src).expect("preset parses");
            let eff = Effective::build_for(&kf, &[], &known, Screen::Browse)
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
            for cmd in FAMILY {
                assert!(
                    eff.bindings().iter().any(|(_, c)| c == cmd),
                    "preset {name}: `{cmd}` has no key — the marking-while-moving \
                     family goes in all seven or in none"
                );
            }
        }
    }

    /// **A core command with no key in a preset is a DECISION or an
    /// oversight, and here the two are told apart** (#228).
    ///
    /// The test next to this one —
    /// [`every_preset_reaches_nortes_own_surfaces`] — always
    /// demands a key, because no original had a screen of norte's own and
    /// silencing it means losing the screen. These six are different: they
    /// are commands the originals COULD have had, so inventing a chord for
    /// a preset whose entire point is fidelity is worse than leaving the
    /// gap — the reference sheet prints `—` and the user finds out.
    ///
    /// What cannot happen is the gap being an oversight. Every exception
    /// goes in the table with its reason; the preset tells it at length in
    /// its header. Adding a preset, or losing a key, turns RED here.
    #[test]
    fn every_core_slot_in_a_preset_is_decided() {
        /// The six from #228's inventory: the ones the catalogue calls core
        /// and that the four imported presets could have brought.
        const CORE: &[&str] = &[
            "pane.compare-dirs",
            "pane.sync-dirs",
            "pane.rename",
            "pane.search",
            "pane.mirror",
            "pane.pull",
        ];
        /// `(preset, command, why it is NOT bound)`. The reason is here so
        /// that whoever deletes a row has to read it first.
        const ACEPTADOS: &[(&str, &str, &str)] = &[
            (
                "far",
                "pane.compare-dirs",
                "Far's \"Compare folders\" lives in the F9 dropdown with no chord of its own, \
                 and Shift+F2 — the one the rest take from Total Commander — is already \
                 \"Unpack files\"",
            ),
            (
                "far",
                "pane.sync-dirs",
                "Far ships no folder syncer in the product: Advanced Compare is a plugin, \
                 so there is no key to transcribe",
            ),
            (
                "far",
                "pane.mirror",
                "a gesture of norte's own: neither transcribed source names \
                 \"send the other pane to this path\"",
            ),
            ("far", "pane.pull", "the same as `pane.mirror`, reversed"),
            (
                "norton",
                "pane.compare-dirs",
                "\"Compare directories\" was a Commands menu entry, and no first-hand source \
                 survives saying whether it had a chord",
            ),
            (
                "norton",
                "pane.sync-dirs",
                "NC had no syncer: there is not even a menu entry to hang a guess on",
            ),
            (
                "norton",
                "pane.rename",
                "NC's F6 (\"RenMov\") is rename-and-move in ONE key, and it is bound to \
                 `pane.move`, whose dialog carries an editable destination name",
            ),
            (
                "norton",
                "pane.mirror",
                "a gesture of norte's own, and this preset is the one with the least source",
            ),
            ("norton", "pane.pull", "the same as `pane.mirror`"),
            (
                "total-commander",
                "pane.mirror",
                "TC's attested family sends the other pane the directory of the entry \
                 UNDER THE CURSOR, which is the direction already bound to `pane.pull`",
            ),
        ];
        let known = preset_commands(Screen::Browse);
        let known: Vec<&str> = known.iter().map(String::as_str).collect();
        let mut extra: Vec<(&str, &str)> = Vec::new();
        for name in presets::NAMES {
            let src = presets::source(name).expect("NAMES resolves");
            let kf = parse_keymap(src).expect("preset parses");
            let eff = Effective::build_for(&kf, &[], &known, Screen::Browse)
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
            for cmd in CORE {
                let bound = eff.bindings().iter().any(|(_, c)| c == cmd);
                let accepted = ACEPTADOS.iter().any(|(p, c, _)| p == name && c == cmd);
                assert!(
                    bound || accepted,
                    "preset {name}: `{cmd}` has no key and is not in the table of decided \
                     gaps. Either bind it, or note it there with the reason — a command only \
                     reachable through the palette is a command nobody reaches"
                );
                if bound && accepted {
                    extra.push((name, cmd));
                }
            }
        }
        assert!(
            extra.is_empty(),
            "these table rows no longer describe anything — the preset DOES bind the \
             command — and an exception that excludes nothing is the one that survives \
             someone reading it: {extra:?}"
        );
    }

    /// WHICH presets count is a decision, not an implementation detail: `vim`
    /// does because vim does, `orthodox` and `cua` do not because their
    /// originals do not and turning it on would take `1`..`9` away from them.
    ///
    /// Nothing else in the suite notices if `counts = true` leaves
    /// `vim.toml` — no bundled preset binds a bare digit, so the load rule
    /// stays quiet either way and the only symptom would be that `5j` silently
    /// stops counting. K2b lands four more imported keymaps; this is the line
    /// that says which of them may flip the flag.
    #[test]
    fn only_vim_ships_with_counters_turned_on() {
        let known = preset_commands(Screen::Browse);
        let known: Vec<&str> = known.iter().map(String::as_str).collect();
        for name in presets::NAMES {
            let src = presets::source(name).expect("NAMES resolves");
            let kf = parse_keymap(src).expect("preset parses");
            let eff = Effective::build_for(&kf, &[], &known, Screen::Browse)
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(eff.counts(), *name == "vim", "preset {name}");
        }
    }
}
