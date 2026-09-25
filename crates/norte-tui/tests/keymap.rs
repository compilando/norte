//! TUI keymap integration tests (ADR 0006): parsing, Yazi merging,
//! prefix-free on load, and deterministic trie resolution. They exercise
//! [`norte_frontend::keymap`]'s SHARED engine THROUGH `norte_tui::keymap`'s
//! re-export (GUI-c T2) — not a local copy: if the extraction broke
//! something, these tests would catch it just the same.

use norte_tui::keymap::{COMMANDS, KeyCode};
use norte_tui::keymap::{
    Chord, Count, Effective, KeymapError, Mods, Resolution, Resolver, Screen, parse_chord,
    parse_keymap,
};

fn eff(preset: &str, user: Option<&str>) -> Result<Effective, KeymapError> {
    let preset = parse_keymap(preset)?;
    let user = user.map(parse_keymap).transpose()?;
    // The TUI's REAL set, which carries `LUA_HOST` (ADR 0110): with plain
    // `COMMANDS` a `lua:` would be said to be unavailable here.
    Effective::build(
        &preset,
        user.as_ref(),
        &norte_tui::shortcuts_editor::known_commands(Screen::Browse),
    )
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

/// The canonical chord ([`Chord::new`], re-exported from
/// `norte_frontend::keymap`) discards `shift` on `Char` keys — the
/// character ALREADY encodes it (`'G'` vs `'g'`) — but keeps it on the rest
/// (e.g. `shift+f5`). This used to be `Chord::from_event`'s (crossterm)
/// behavior before GUI-c T2; now it lives in `Chord::new` itself, neutral,
/// and the `chord_from_crossterm` adapter inherits it for free.
#[test]
fn chord_new_normalizes_shift_in_chars_but_not_in_other_keys() {
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
    assert_eq!(
        r.push(parse_chord("g").unwrap()),
        Resolution::Run {
            command: "cursor.top".into(),
            count: Count::None
        }
    );
    // After running, the state is left clean.
    assert_eq!(
        r.push(parse_chord("G").unwrap()),
        Resolution::Run {
            command: "cursor.bottom".into(),
            count: Count::None
        }
    );
    // Key with no binding: silent reset.
    assert_eq!(r.push(parse_chord("z").unwrap()), Resolution::Reset);
    // Pending prefix + a key that does not continue it: reset (runs nothing).
    r.push(parse_chord("g").unwrap());
    assert_eq!(r.push(parse_chord("q").unwrap()), Resolution::Reset);
    // q on its own (global context) does run.
    assert_eq!(
        r.push(parse_chord("q").unwrap()),
        Resolution::Run {
            command: "app.quit".into(),
            count: Count::None
        }
    );
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
    assert_eq!(
        r.push(parse_chord("esc").unwrap()),
        Resolution::Run {
            command: "app.quit".into(),
            count: Count::None
        }
    );
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
fn shift_with_char_is_a_diagnosable_error() {
    // A "shift+g" binding would never match (the canonical event discards
    // shift on chars): a rejection on parse, not a dead binding.
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
    let preset_malo = r#"
        [pane]
        prepend_keymap = [{ on = ["j"], run = "cursor.down" }]
    "#;
    match eff(preset_malo, None) {
        Err(KeymapError::WrongLayerKey {
            layer: "preset", ..
        }) => {}
        other => panic!("expected WrongLayerKey preset, got {other:?}"),
    }
}

#[test]
fn context_specificity_prevails_over_the_layer() {
    // ADR 0006 (disambiguated in phase 4): layers merge PER context; among
    // contexts the specific one wins. A user append in [pane] overrides the
    // preset's [global] keymap…
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
        Resolution::Run {
            command: "cursor.up".into(),
            count: Count::None
        },
        "pane.append wins over global.keymap (specificity > layer)"
    );
    // …and a user prepend in [global] does NOT override [pane]'s keymap.
    assert_eq!(
        r.push(parse_chord("j").unwrap()),
        Resolution::Run {
            command: "cursor.down".into(),
            count: Count::None
        },
        "global.prepend does not override pane.keymap"
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

/// M4 Lua (T8): a binding to `lua:<name>` passes validation even if the
/// name is not in COMMANDS — the Lua registry is dynamic (runtime); an
/// unregistered lua command is NOT a keymap error (on invocation, the bar
/// warns with `err-lua-unknown`). The NAME is validated with the same
/// charset as `norte.command` (`[a-z0-9._-]{1,64}`): a binding to a name
/// that could never register is diagnosable broken config, not a silently
/// dead binding.
#[test]
fn prefixed_lua_passes_command_validation() {
    let preset = r#"
        [pane]
        keymap = [{ on = ["x"], run = "lua:mi-comando.v2" }]
    "#;
    let mut r = Resolver::new(eff(preset, None).expect("lua: with a valid name passes"));
    assert_eq!(
        r.push(parse_chord("x").unwrap()),
        Resolution::Run {
            command: "lua:mi-comando.v2".into(),
            count: Count::None
        },
        "the binding resolves to the full lua: command"
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
        Resolution::Run {
            command: "cursor.top".into(),
            count: Count::None
        },
        "prepend OVERRIDES the preset"
    );
    assert_eq!(
        r.push(parse_chord("k").unwrap()),
        Resolution::Run {
            command: "cursor.up".into(),
            count: Count::None
        },
        "append does NOT override an existing sequence"
    );
    assert_eq!(
        r.push(parse_chord("x").unwrap()),
        Resolution::Run {
            command: "app.quit".into(),
            count: Count::None
        },
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
    assert_eq!(
        r.push(parse_chord("q").unwrap()),
        Resolution::Run {
            command: "cursor.up".into(),
            count: Count::None
        }
    );
    assert_eq!(
        r.push(parse_chord("tab").unwrap()),
        Resolution::Run {
            command: "pane.switch".into(),
            count: Count::None
        }
    );
}

#[test]
fn the_three_factory_presets_load_and_cover_the_basics() {
    for (name, preset) in norte_tui::keymap::presets() {
        let eff = Effective::build(&preset, None, COMMANDS)
            .unwrap_or_else(|e| panic!("preset {name}: {e:?}"));
        let mut r = Resolver::new(eff.clone());
        // Every preset must be able to quit and switch pane. `alt+f4` is
        // Total Commander's real quit (K2b): its F10 means "activate/leave
        // the menu," not quit, so it does NOT share it with q/f10/ctrl+q.
        let quit_possible = ["q", "f10", "ctrl+q", "alt+f4"].iter().any(|k| {
            let res = r.push(parse_chord(k).unwrap());
            res == Resolution::Run {
                command: "app.quit".into(),
                count: Count::None,
            }
        });
        assert!(quit_possible, "preset {name}: no way to quit");
        assert_eq!(
            r.push(parse_chord("tab").unwrap()),
            Resolution::Run {
                command: "pane.switch".into(),
                count: Count::None
            },
            "preset {name}: Tab is sacred (spec)"
        );
    }
}

#[test]
fn presets_bind_file_operations() {
    for (name, preset) in norte_tui::keymap::presets() {
        let eff = Effective::build(&preset, None, COMMANDS)
            .unwrap_or_else(|e| panic!("preset {name}: {e:?}"));
        let mut r = Resolver::new(eff.clone());
        for (key, cmd) in [
            ("f5", "pane.copy"),
            ("f6", "pane.move"),
            ("f8", "pane.delete"),
        ] {
            assert_eq!(
                r.push(parse_chord(key).unwrap()),
                Resolution::Run {
                    command: cmd.into(),
                    count: Count::None
                },
                "preset {name}: {key}"
            );
        }
    }
    // `ctrl+k` → `task.cancel` is norte's OWN convention (orthodox/vim/cua):
    // neither Total Commander nor Krusader document a cancel-task key in
    // their sources (K2b rule 1), so one is not invented there — this
    // second loop stays within the three native presets.
    for (name, preset) in norte_tui::keymap::presets()
        .into_iter()
        .filter(|(n, _)| matches!(*n, "orthodox" | "vim" | "cua"))
    {
        let eff = Effective::build(&preset, None, COMMANDS)
            .unwrap_or_else(|e| panic!("preset {name}: {e:?}"));
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(parse_chord("ctrl+k").unwrap()),
            Resolution::Run {
                command: "task.cancel".into(),
                count: Count::None
            },
            "preset {name}: ctrl+k"
        );
    }
}

#[test]
fn multiple_layers_fold_by_precedence() {
    // ADR 0007: higher layers' prepends first; appends the same.
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
    // Capas en precedencia ASCENDENTE: sistema, usuario.
    let eff = Effective::build_layered(&preset, &[system, user], COMMANDS).unwrap();
    let mut r = Resolver::new(eff);
    assert_eq!(
        r.push(parse_chord("j").unwrap()),
        Resolution::Run {
            command: "cursor.top".into(),
            count: Count::None
        },
        "the HIGHEST layer's prepend wins"
    );
    assert_eq!(
        r.push(parse_chord("x").unwrap()),
        Resolution::Run {
            command: "cursor.bottom".into(),
            count: Count::None
        },
        "among appends, the highest layer also wins"
    );
}

#[test]
fn the_viewer_context_merges_for_its_screen() {
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
    // In Browse, the global q rules and enter exists.
    let browse = Effective::build_for(&preset, &[], COMMANDS, Screen::Browse).unwrap();
    let mut r = Resolver::new(browse);
    assert_eq!(
        r.push(parse_chord("q").unwrap()),
        Resolution::Run {
            command: "app.quit".into(),
            count: Count::None
        }
    );
    assert_eq!(
        r.push(parse_chord("enter").unwrap()),
        Resolution::Run {
            command: "nav.enter".into(),
            count: Count::None
        }
    );
    // In Viewer, its specific q OVERRIDES global and enter does NOT exist.
    let viewer = Effective::build_for(&preset, &[], COMMANDS, Screen::Viewer).unwrap();
    let mut r = Resolver::new(viewer);
    assert_eq!(
        r.push(parse_chord("q").unwrap()),
        Resolution::Run {
            command: "cursor.top".into(),
            count: Count::None
        }
    );
    assert_eq!(r.push(parse_chord("enter").unwrap()), Resolution::Reset);
}

/// Help extensibility: EVERY command has a description in BOTH locales — a
/// new command with no help-cmd-* entry fails here. The screen's decoration
/// (title, sections) too.
///
/// `help-hint` is GONE: the overlay's footer is GENERATED from the
/// effective keymap (`hints::DialogHints::help`) since H3b, and the static
/// string that was left announced a key (`q`) keymap routing no longer
/// accepts.
#[test]
fn every_command_has_translated_help() {
    use norte_tui::keymap::help_id;
    let ids_decoration = [
        "help-title".to_owned(),
        // H3b: the sidebar's synthetic `keys` entry's label. It is resolved
        // in `HelpView::new` and travels to the model as a row TITLE: with
        // no Fluent entry, the sidebar would paint `help-topic-keys` literally.
        "help-topic-keys".to_owned(),
        "help-section-browse".to_owned(),
        "help-section-viewer".to_owned(),
    ];
    let ids = COMMANDS.iter().map(|cmd| help_id(cmd));
    for id in ids.chain(ids_decoration) {
        for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
            let text = norte_i18n::t_in(lang, &id);
            assert_ne!(text, id, "{id}: no translation in {lang:?}");
        }
    }
}

/// H3b: EVERY group header of the help sidebar has a Fluent entry in BOTH
/// locales.
///
/// `draw_help` paints `t(&format!("help-group-{tag}"))` with the tag coming
/// from the corpus's FRONT MATTER, and `norte_i18n::t` answers a lookup
/// failure with the id: a topic filed under a new tag would paint a literal
/// `help-group-advanced` strip in the sidebar. It is the same defect this
/// same phase fixed one file over (`help.rs`'s `label_id`, pinned by
/// `the_cheatsheet_never_paints_a_fluent_id`), and the twin lookup had no
/// guard.
///
/// The i18n parity suite does NOT cover it: it only asserts EN and ES have
/// the SAME set of ids, so a tag absent from both slips through.
///
/// The tags are pulled from the MODEL (`HelpState::rows`), not from a
/// hand-written list: they are exactly the `Group` rows the painter walks.
/// H3h writes the whole corpus and this sweep grows with it untouched.
///
/// The ones the painter does NOT paint are left out, and it says who is who
/// itself (`ui::help_group_is_painted`), not a copy of the rule: the
/// synthetic `keys` group's header is suppressed — it is a group of one
/// whose header would be named the same as its only row — so requiring a
/// Fluent entry for it would demand a string nobody looks up. If a corpus
/// topic is ever filed under that tag, its header DOES paint and this sweep
/// asks for it again.
#[test]
fn every_help_group_header_has_a_translated_label() {
    use norte_frontend::help::{HelpState, SidebarRow};

    let mut seen = 0usize;
    for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
        // `keys`'s label is not being tested here (`ids_decoration` covers
        // it); it does not matter what it is as long as it is not empty.
        let state = HelpState::new(lang, "Teclado".to_owned());
        for (i, row) in state.rows().iter().enumerate() {
            let SidebarRow::Group { tag } = row else {
                continue;
            };
            if !norte_tui::ui::help_group_is_painted(state.rows(), i) {
                continue;
            }
            seen += 1;
            let id = format!("help-group-{tag}");
            assert_ne!(
                norte_i18n::t_in(lang, &id),
                id,
                "{id}: no translation in {lang:?} — the sidebar would paint \
                 the Fluent id as if it were the group's name. Add the entry \
                 in i18n/{}.ftl",
                match lang {
                    norte_i18n::Lang::Es => "es",
                    norte_i18n::Lang::En => "en",
                }
            );
        }
    }
    // Anti-emptiness: with no `Group` rows the loop asserts nothing. Today
    // there are 3 PAINTED groups per locale (`basics`, `doing`, `remote`;
    // the synthetic `keys`'s header does not paint).
    assert!(
        seen >= 6,
        "the sweep did not see enough group headers ({seen}): the model \
         stopped grouping and this test would pass vacuously"
    );
}

/// H1 T3 (#24): EVERY command in [`DIALOG_COMMANDS`] has a SHORT label in
/// BOTH locales — the source of the overlays' generated hints
/// (`dialog_hints`/`DialogHints`). Same mangling as `help_id` (dots→dashes)
/// but for the suffix after `dialog.`, via `dialog_hint_id`: a new command
/// in `DIALOG_COMMANDS` with no `dialog-cmd-*` fails here, never silently
/// in the footer.
#[test]
fn every_dialog_command_has_a_translated_label() {
    use norte_tui::keymap::{DIALOG_COMMANDS, dialog_hint_id};
    for cmd in DIALOG_COMMANDS {
        let id = dialog_hint_id(cmd);
        for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
            let text = norte_i18n::t_in(lang, &id);
            assert_ne!(text, id, "{id}: no translation in {lang:?}");
        }
    }
}

/// Help is built from the EFFECTIVE keymap: the exposed bindings reflect
/// preset + layers IN PRECEDENCE ORDER, and a shadowed binding appears ONCE
/// with the command that wins (what the key really does, not what the
/// preset says).
#[test]
fn exposed_bindings_reflect_the_layers() {
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
    // Shadowed: "j" just ONCE and the user's prepend wins.
    let jotas: Vec<_> = b.iter().filter(|(seq, _)| seq == "j").collect();
    assert_eq!(jotas.len(), 1, "duplicated shadowed binding: {b:?}");
    assert_eq!(jotas[0].1, "cursor.top", "the user layer must win");
    // Precedence order: the user's prepend before the preset.
    let pos = |wanted: &str| b.iter().position(|(seq, _)| seq == wanted).unwrap();
    assert!(pos("j") < pos("k"), "prepend before preset: {b:?}");
    assert!(
        b.iter()
            .any(|(seq, cmd)| seq == "g g" && *cmd == "cursor.top"),
        "the user's append shows up in help: {b:?}"
    );
}

/// HIGH (security review M4 Lua): `./.norte/keymap.toml` loads WITHOUT
/// trust, so a hostile repo could rebind a common key (`j`, `enter`) to a
/// `lua:` command from the USER's init.lua (no sandbox, no confirmation,
/// with cwd = the hostile repo). `lua:` bindings originating in the
/// PROJECT layer are DISCARDED (counted for the bar's warning); project
/// rebinds to builtins keep working; the same binding in a user layer DOES
/// resolve.
#[test]
fn project_keymap_lua_is_discarded_with_a_warning() {
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

    // PROJECT layer: the lua: binding is discarded — the key falls to the
    // preset's builtin — and it is counted for the warning.
    let mut project = parse_keymap(layer).unwrap();
    project.mark_project();
    let known = norte_tui::shortcuts_editor::known_commands(Screen::Browse);
    let eff = Effective::build_layered(&preset, std::slice::from_ref(&project), &known)
        .expect("discarding is not a load error");
    assert_eq!(eff.discarded_lua_bindings(), 1, "counted for the warning");
    let mut r = Resolver::new(eff);
    assert_eq!(
        r.push(parse_chord("j").unwrap()),
        Resolution::Run {
            command: "cursor.down".into(),
            count: Count::None
        },
        "the key falls to the builtin, never to the project's lua:"
    );

    // The SAME binding in a USER layer (unmarked): resolves normally.
    let user = parse_keymap(layer).unwrap();
    let eff = Effective::build_layered(&preset, std::slice::from_ref(&user), &known).unwrap();
    assert_eq!(eff.discarded_lua_bindings(), 0);
    let mut r = Resolver::new(eff);
    assert_eq!(
        r.push(parse_chord("j").unwrap()),
        Resolution::Run {
            command: "lua:pwn".into(),
            count: Count::None
        },
        "in a user layer the lua: binding is legitimate"
    );

    // Project rebind to a BUILTIN: still works (the discard is ONLY for
    // `lua:` — innocuous project config is not broken).
    let mut project = parse_keymap(
        r#"
        [pane]
        prepend_keymap = [{ on = ["x"], run = "cursor.up" }]
    "#,
    )
    .unwrap();
    project.mark_project();
    let eff = Effective::build_layered(&preset, std::slice::from_ref(&project), COMMANDS).unwrap();
    assert_eq!(eff.discarded_lua_bindings(), 0);
    let mut r = Resolver::new(eff);
    assert_eq!(
        r.push(parse_chord("x").unwrap()),
        Resolution::Run {
            command: "cursor.up".into(),
            count: Count::None
        }
    );
}
