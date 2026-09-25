//! The embedded factory presets: sources, the name catalogue, and the lookup
//! each frontend uses for its "known preset" list.
//!
//! Bundled keymap presets (ADR 0006), shared by every frontend. Each
//! frontend declares ITS OWN command set to
//! [`Effective::build_for`](super::Effective::build_for); a preset binding to
//! a command that frontend lacks is KEPT and tagged
//! [`Availability::NotHere`](super::Availability), never dropped, so the key
//! can say why it does nothing (K1).

/// The default orthodox preset.
pub const ORTHODOX: &str = include_str!("../../presets/keymap/orthodox.toml");
/// Vim-style preset.
pub const VIM: &str = include_str!("../../presets/keymap/vim.toml");
/// CUA preset.
pub const CUA: &str = include_str!("../../presets/keymap/cua.toml");
/// Total Commander-style preset (K2b).
pub const TOTAL_COMMANDER: &str = include_str!("../../presets/keymap/total-commander.toml");
/// Krusader-style preset (K2b).
pub const KRUSADER: &str = include_str!("../../presets/keymap/krusader.toml");
/// Far Manager-style preset (K2b). No numeric prefix in the original — Far
/// spends `Ctrl+1..Ctrl+0` on panel view modes — so, like every preset in
/// this module, it never sets `counts`.
pub const FAR: &str = include_str!("../../presets/keymap/far.toml");
/// Norton Commander-style preset (K2b). No first-hand source (`NC.HLP` is
/// internally compressed); the file transcribes the uncontroversial core —
/// see this file's own header comment.
pub const NORTON: &str = include_str!("../../presets/keymap/norton.toml");

/// Names of every embedded preset (final review MINOR 4: this catalog
/// used to be mirrored as a hardcoded `&[&str]` in each frontend that
/// needs a "known preset" list or an "available presets" banner message
/// — TUI, GUI — with no single source of truth. `source()` and `NAMES`
/// are now tested against each other below, so a preset added to one and
/// not the other fails CI instead of drifting silently.
pub const NAMES: &[&str] = &[
    "orthodox",
    "vim",
    "cua",
    "total-commander",
    "krusader",
    "far",
    "norton",
];

/// Preset source by name; `None` if unknown (caller falls back +
/// reports, same contract the TUI had).
#[must_use]
pub fn source(name: &str) -> Option<&'static str> {
    match name {
        "orthodox" => Some(ORTHODOX),
        "vim" => Some(VIM),
        "cua" => Some(CUA),
        "total-commander" => Some(TOTAL_COMMANDER),
        "krusader" => Some(KRUSADER),
        "far" => Some(FAR),
        "norton" => Some(NORTON),
        _ => None,
    }
}

#[cfg(test)]
mod presets_catalog_tests {
    use super::{NAMES, source};

    /// `NAMES` is the catalogue — every entry must resolve a real source
    /// (final review MINOR 4: without this, `NAMES` could fall out of sync
    /// with `source()` with no test noticing).
    #[test]
    fn every_name_in_names_resolves_a_source() {
        for name in NAMES {
            assert!(
                source(name).is_some(),
                "NAMES declares {name:?} but source({name:?}) is None"
            );
        }
    }

    /// Pins the catalogue's size: a new preset must touch this test on
    /// purpose (and with it, the rest of the frontends that consume
    /// `NAMES`). Renamed in K2b Task 2 (was
    /// `names_tiene_los_tres_presets_de_fabrica`): it is no longer three,
    /// and the old name would lie about the real size.
    #[test]
    fn names_has_the_factory_presets() {
        assert_eq!(NAMES.len(), 7);
    }

    /// An embedded preset that does not parse is not a loud failure: its
    /// consumers silently ignore it. `preset_commands` does
    /// `let Ok(kf) = parse_keymap(src) else { continue }`, so a typo in —
    /// say — a K2b preset's `dialog_from` would shrink the vocabulary
    /// `norte doctor` and `ntc keys` treat as known, and the symptom would
    /// be "unknown command" warnings somewhere else. Let it fail here,
    /// with the preset's name and the diagnostic.
    #[test]
    fn every_factory_preset_parses() {
        for name in NAMES {
            let src = source(name).expect("NAMES resolves");
            crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {name}: {e}"));
        }
    }
}

/// K2b Task 4 — the gate that keeps every bundled preset honest, driven by
/// [`NAMES`] so a future eighth preset inherits every check for free.
///
/// The plan lists seven checks; two of them do NOT live here:
///
/// - **Check 1** (builds for `Screen::{Browse, Viewer, Dialog}` against a
///   FRONTEND's command set) needs a frontend's actual `COMMANDS` list, which
///   this crate cannot see — `norte-frontend` is upstream of every frontend,
///   not the other way round. Its TUI half is
///   `norte_tui::keymap::tests::todos_los_presets_construyen_las_tres_pantallas_del_tui`;
///   its graphical half died with the GPUI frontend (ADR 0065) and is owed by
///   whatever replaces it.
/// - **Check 6** ("only `vim` sets `counts`") already has a pinned test that
///   iterates `NAMES`/`source` exactly this way:
///   [`super::tests::only_vim_ships_with_counters_turned_on`] in this
///   module's parent (`keymap/mod.rs`), predating this task. Duplicating it
///   here would just be two tests that can drift from each other.
///
/// The other five (2–5, 7) are independent of any frontend's vocabulary —
/// they ask about the preset's OWN data (its bindings, its `[dialog]`
/// section, its header) — so they belong next to [`NAMES`], not downstream.
#[cfg(test)]
mod k2b_gate_tests {
    use super::{NAMES, source};
    use crate::keymap::{CATALOGUE, Effective, Screen, Status, parse_chord};

    /// Every `Live` command in the shared catalogue — an "omniscient
    /// frontend" that implements everything norte has actually built, so a
    /// `Live` binding always resolves and only `Planned` stays
    /// `NotBuilt`. Checks 2 and 3 need bindings to actually RUN (not just
    /// survive marked `NotHere`), and neither check is about a specific
    /// frontend's subset — that is check 1's job.
    fn live_commands() -> Vec<&'static str> {
        CATALOGUE
            .iter()
            .filter(|d| matches!(d.status, Status::Live))
            .map(|d| d.name)
            .collect()
    }

    /// Parses `name` and builds its effective keymap for `screen` against
    /// [`live_commands`]. Panics name the preset AND the screen — a bare
    /// `.unwrap()` on a `for` loop over seven presets would only say
    /// `SacredKey { .. }` and leave the reader grepping seven files.
    fn build(name: &str, screen: Screen, known: &[&str]) -> Effective {
        let src = source(name).expect("NAMES resolves");
        let kf = crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {name}: {e}"));
        Effective::build_for(&kf, &[], known, screen)
            .unwrap_or_else(|e| panic!("preset {name} in {screen:?}: {e}"))
    }

    /// Check 2: `tab` resolves to `pane.switch` in every bundled preset.
    ///
    /// "…and to nothing else" is NOT re-asserted here: [`Effective::build_for`]
    /// already enforces it structurally, at load, via `check_sacred` (ADR
    /// 0044) — a preset that bound `tab` to anything but `pane.switch` alone
    /// would have failed inside [`build`] above with `SacredKey`, before this
    /// test's assertion ever runs. What `check_sacred` does NOT guarantee is
    /// the positive half: it forbids the wrong binding, but nothing requires
    /// the right one to exist at all, and four newly-imported foreign layouts
    /// are the first real chance to get that half wrong (plan rule 2).
    #[test]
    fn tab_resolves_to_pane_switch_in_every_preset() {
        let known = live_commands();
        let tab = parse_chord("tab").expect("tab parses");
        for name in NAMES {
            let eff = build(name, Screen::Browse, &known);
            assert!(
                eff.single_chord_runs(tab, "pane.switch"),
                "preset {name}: tab does not resolve to pane.switch"
            );
        }
    }

    /// A chord cannot name TWO commands on the same screen.
    ///
    /// The file allows it — they are two lines of a list — and the
    /// effective one keeps one of them: the other loses its key with
    /// nothing saying so, and reappears as `—` in the palette, in the menu
    /// and in the shortcut sheet. `orthodox` had `alt+n` in `[pane]` bound
    /// to both `pane.disconnect` and `pane.tab-next`, so "next tab" came
    /// out with no key in the menu and the key did the other thing.
    ///
    /// Checked against the EFFECTIVE one and not the file because that is
    /// what runs: it also covers what `[global]` mixes into each screen,
    /// which is where a collision is easiest to write without seeing it.
    #[test]
    fn no_preset_binds_a_chord_to_two_commands_on_the_same_screen() {
        for name in NAMES {
            let kf = crate::keymap::parse_keymap(source(name).expect("NAMES resolves"))
                .unwrap_or_else(|e| panic!("preset {name}: {e}"));
            // Section by section, and over the FILE: by the time the
            // effective one is built the second has already overwritten
            // the first, so the collision is invisible there — which is
            // exactly what makes it hard to see.
            for (section, raw) in [
                ("global", &kf.global),
                ("pane", &kf.pane),
                ("viewer", &kf.viewer),
                ("dialog", &kf.dialog),
            ] {
                let mut by_chord: std::collections::BTreeMap<
                    String,
                    std::collections::BTreeSet<&str>,
                > = std::collections::BTreeMap::new();
                for b in raw
                    .keymap
                    .iter()
                    .chain(&raw.prepend_keymap)
                    .chain(&raw.append_keymap)
                {
                    by_chord
                        .entry(b.on.join(" "))
                        .or_default()
                        .insert(b.run.as_str());
                }
                for (seq, cmds) in by_chord {
                    assert!(
                        cmds.len() < 2,
                        "preset {name}, section [{section}]: `{seq}` binds {} commands ({}), \
                         so all but one are left with no key",
                        cmds.len(),
                        cmds.into_iter().collect::<Vec<_>>().join(", ")
                    );
                }
            }
        }
    }

    /// Check 3: `viewer.close` and the six movers (rule 7 — "F3 is never a
    /// room with no door") are bound and RUNNABLE in `[viewer]`, for every
    /// bundled preset.
    const VIEWER_MOVERS: [&str; 6] = [
        "viewer.up",
        "viewer.down",
        "viewer.page-up",
        "viewer.page-down",
        "viewer.top",
        "viewer.bottom",
    ];

    #[test]
    fn viewer_close_and_the_six_movers_are_bound_in_every_preset() {
        let known = live_commands();
        for name in NAMES {
            let eff = build(name, Screen::Viewer, &known);
            let bound: Vec<&str> = eff.bindings().iter().map(|(_, cmd)| *cmd).collect();
            assert!(
                bound.contains(&"viewer.close"),
                "preset {name}: no viewer.close in [viewer]"
            );
            for mover in VIEWER_MOVERS {
                assert!(
                    bound.contains(&mover),
                    "preset {name}: no {mover} in [viewer]"
                );
            }
        }
    }

    /// An image's ZOOM is bound in all SEVEN presets (spec 2026-09-20).
    ///
    /// It is a surface of norte's own: no reference manager attests a zoom
    /// in its viewer, so we chose the three keys ourselves, and the only
    /// way a reader has them is if they are in all of them. Otherwise, the
    /// catalogue announces them, the reference sheet prints them, the
    /// palette offers them, and their keyboard does nothing — which is
    /// exactly the breakage CLAUDE.md says has already landed three times.
    #[test]
    fn the_viewers_zoom_is_bound_in_all_seven_presets() {
        let known = live_commands();
        let mut missing: Vec<String> = Vec::new();
        for name in NAMES {
            let eff = build(name, Screen::Viewer, &known);
            let bound: Vec<&str> = eff.bindings().iter().map(|(_, cmd)| *cmd).collect();
            for cmd in ["viewer.zoom-in", "viewer.zoom-out", "viewer.zoom-fit"] {
                if !bound.contains(&cmd) {
                    missing.push(format!("{name}: {cmd}"));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "zoom with no key in some preset:\n  {}",
            missing.join("\n  ")
        );
    }

    /// Check 4: after `dialog_from` resolution (Task 1), every preset's
    /// `[dialog]` section is non-empty — the inheritance actually landed,
    /// not just parsed without error.
    ///
    /// Reads the parsed [`crate::keymap::KeymapFile`] directly rather than
    /// building an [`Effective`]: `Screen::Dialog` merges `[dialog]` with
    /// `[global]`, so a non-empty EFFECTIVE dialog screen would pass even if
    /// `[dialog]` itself came back empty (an `orthodox`-inherited `y` bound
    /// under `[global]` would never happen, but the merge makes that
    /// coincidence, not a property this test would actually be checking).
    /// `.keymap` alone (not `prepend_keymap`/`append_keymap`) is enough: a
    /// preset — the only kind of file that ever reaches `dialog_from` — is
    /// refused those two lists by `check_layer_keys`.
    #[test]
    fn dialog_is_not_empty_after_resolving_dialog_from_in_every_preset() {
        for name in NAMES {
            let src = source(name).expect("NAMES resolves");
            let kf =
                crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {name}: {e}"));
            assert!(
                !kf.dialog.keymap.is_empty(),
                "preset {name}: [dialog] empty after resolving dialog_from"
            );
        }
    }

    /// The `run` names of every binding a preset's OWN `keymap` lists declare
    /// (`[global]`/`[pane]`/`[viewer]`/`[dialog]` — presets never use
    /// `prepend_keymap`/`append_keymap`, `check_layer_keys` refuses that).
    /// Field access only: `RawSection`/`RawBinding` are `pub(super)` to
    /// `layer.rs`'s parent (`keymap`), and this module is inside that same
    /// tree, so the fields are visible without naming either type.
    fn preset_runs(kf: &crate::keymap::KeymapFile) -> Vec<&str> {
        [&kf.global, &kf.pane, &kf.viewer, &kf.dialog]
            .into_iter()
            .flat_map(|section| section.keymap.iter())
            .map(|b| b.run.as_str())
            .collect()
    }

    /// `pane.sync-dirs` is bound in EVERY preset that binds its twin
    /// `pane.compare-dirs`, and to `ctrl+y` in the five that bind it — four
    /// taking it from Krusader's chord, which is the only reference manager
    /// that gives it one.
    ///
    /// The list of the ones that do NOT bind it is written BY HAND, and
    /// that is the decision: `far` and `norton` leave both unbound by the
    /// fidelity rule their own files state — Far has the syncer in a plugin
    /// and NC did not have one — and a test that demanded "every preset"
    /// would undo those two comments without discussing them. Written here,
    /// dropping a fidelity choice costs editing this test.
    #[test]
    fn sync_is_bound_wherever_compare_is() {
        const UNBOUND: [&str; 2] = ["far", "norton"];
        for name in NAMES {
            let src = source(name).expect("NAMES resolves");
            let kf =
                crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {name}: {e}"));
            let runs = preset_runs(&kf);
            let compares = runs.contains(&"pane.compare-dirs");
            let syncs = runs.contains(&"pane.sync-dirs");
            if UNBOUND.contains(name) {
                assert!(
                    !compares && !syncs,
                    "preset {name}: is no longer one of the ones that leave the family unbound — remove the name from UNBOUND"
                );
                continue;
            }
            assert!(compares, "preset {name}: no pane.compare-dirs");
            assert!(
                syncs,
                "preset {name}: binds compare and NOT sync — the writing half was left with no key"
            );
        }
    }

    /// `pane.sync-dirs`'s default key is not a function key with a
    /// modifier.
    ///
    /// #159: under tmux NONE arrive — not `Shift+F2` nor `Alt+F7` — so a key
    /// like that would be a documented, dead shortcut, and this repo has
    /// already shipped one. It asserts on the CONCRETE chord rather than on
    /// a property of `Chord`, because what has to be prevented is someone
    /// moving it to a function key "because it is symmetric with Shift+F2".
    #[test]
    fn syncs_key_is_not_a_function_key_with_a_modifier() {
        let known = live_commands();
        let ctrl_y = parse_chord("ctrl+y").expect("ctrl+y parses");
        for name in ["orthodox", "cua", "vim", "total-commander", "krusader"] {
            let eff = build(name, Screen::Browse, &known);
            assert!(
                eff.single_chord_runs(ctrl_y, "pane.sync-dirs"),
                "preset {name}: ctrl+y does not resolve to pane.sync-dirs"
            );
        }
    }

    /// Check 5: every `run` name a preset binds is in the shared catalogue
    /// (Live or Planned) — the same rule [`Effective::build_for`]'s
    /// `UnknownCommand` already enforces, asserted directly against the raw
    /// data instead of through a build, so a failure names the preset AND
    /// the offending command instead of surfacing as one of several possible
    /// `KeymapError` variants from an unrelated screen/known-list
    /// combination.
    #[test]
    fn every_run_in_every_preset_is_in_the_shared_catalogue() {
        for name in NAMES {
            let src = source(name).expect("NAMES resolves");
            let kf =
                crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {name}: {e}"));
            for run in preset_runs(&kf) {
                let known = run
                    .strip_prefix("lua:")
                    .is_some_and(crate::keymap::valid_lua_name)
                    || crate::keymap::catalogue::lookup(run).is_some();
                assert!(known, "preset {name}: unknown command {run:?}");
            }
        }
    }

    /// norte's OWN surfaces are bound in all SEVEN presets.
    ///
    /// They are the ones no reference manager had, so there is nothing to
    /// transcribe and a key has to be picked by hand — and that is why they
    /// get forgotten. `app.theme` was left unbound in the four imported
    /// presets: the ONLY one of the family that fell through, and in all
    /// four at once. The theme could only be reached through the menu or
    /// the palette.
    ///
    /// It is #228's shape — presets that leave core commands with no key —
    /// applied to the whole family instead of to a single command.
    #[test]
    fn nortes_own_surfaces_are_bound_in_all_seven_presets() {
        let own = [
            "app.theme",
            "app.settings",
            "app.extensions",
            "app.palette",
            "app.menu",
        ];
        let mut missing: Vec<String> = Vec::new();
        for name in NAMES {
            let src = source(name).expect("NAMES resolves");
            let kf =
                crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {name}: {e}"));
            let bound = preset_runs(&kf);
            for cmd in own {
                if !bound.contains(&cmd) {
                    missing.push(format!("{name}: {cmd}"));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "norte's own surfaces with no key — reachable only through the \
             menu or the palette:\n  {}",
            missing.join("\n  ")
        );
    }

    /// Every panel that can keep the KEYBOARD opens with a key, and the
    /// screen is browsed whole with another.
    ///
    /// The same shape as the test above, over the other family no
    /// reference manager had: the side panels. `layout.processes` was
    /// unbound in all seven — the only place that says what norte is
    /// copying could only be reached through the menu — and so was
    /// `layout.focus-next`, so with the sidebar and the viewer in front you
    /// had to remember each panel's own key to move between them: `tab`
    /// only toggles the two listings.
    ///
    /// `layout.focus-prev` is NOT in the list: in krusader its chord is
    /// "Sync panels"'s and it is left unbound on purpose (see the file).
    /// The ring wraps around, so forward still gets there.
    #[test]
    fn keyboard_panels_open_and_are_browsed_in_all_seven_presets() {
        let panels = [
            "layout.places",
            "layout.preview",
            "layout.processes",
            "layout.focus-next",
            // The log (#323): a panel that takes the keyboard, so it enters
            // this list. `alt+l` binds it in all seven, and the
            // repository's rule is that a key change is not done until all
            // seven have it — this is what enforces that by machine.
            "layout.log",
            // The disk map (phase 4): another panel that takes the
            // keyboard, and therefore another that cannot be left with no
            // key in any of them. `alt+z` in all seven — the only letter
            // still free in all of them.
            "layout.disk-map",
            // The terminal (#362), and it is the EXTREME case of this
            // list: the other panels consume catalogue commands, and this
            // one consumes bytes, i.e. it also takes the chords that would
            // be norte's. With no key you cannot enter and, what matters,
            // cannot leave. `ctrl+alt+s` in all seven.
            "layout.terminal",
        ];
        let mut missing: Vec<String> = Vec::new();
        for name in NAMES {
            let src = source(name).expect("NAMES resolves");
            let kf =
                crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {name}: {e}"));
            let bound = preset_runs(&kf);
            for cmd in panels {
                if !bound.contains(&cmd) {
                    missing.push(format!("{name}: {cmd}"));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "panels with no key — either they do not open, or there is no \
             way out of them without a mouse:\n  {}",
            missing.join("\n  ")
        );
    }

    /// No preset binds a chord a terminal CANNOT deliver.
    ///
    /// `ctrl+<uppercase letter>` is that shape. [`crate::keymap::parse_chord`]
    /// stores it as `Char('P')`, and a terminal sends the SAME byte (0x10)
    /// for Ctrl+P and for Ctrl+Shift+P, which arrives as lower-case
    /// `Char('p')`. Telling them apart requires Kitty's keyboard protocol,
    /// and the TUI's adapter (`norte_tui::keymap::chord_from_crossterm`)
    /// documents that norte does NOT enable it — the same reason `mod+` is
    /// Ctrl on every platform. So the binding exists, the catalogue
    /// announces it, help prints it, and the key does NOTHING.
    ///
    /// It came out of `krusader`'s palette: `ctrl+P` to open it, with a
    /// comment explaining that the uppercase IS the shift. It is, in the
    /// chord grammar; it is not, on the wire. And since lower-case `ctrl+p`
    /// IS bound there to `pane.split-file`, whoever looked for the palette
    /// found a split-file dialog.
    ///
    /// The test lives HERE and not in the TUI because the presets belong to
    /// this crate, and the rule is about what a preset is allowed to
    /// promise.
    #[test]
    fn no_preset_binds_a_chord_the_terminal_cannot_deliver() {
        let mut dead: Vec<String> = Vec::new();
        for name in NAMES {
            let src = source(name).expect("NAMES resolves");
            let kf =
                crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {name}: {e}"));
            for b in [&kf.global, &kf.pane, &kf.viewer, &kf.dialog]
                .into_iter()
                .flat_map(|s| s.keymap.iter())
            {
                for txt in &b.on {
                    if crate::keymap::parse_chord(txt).is_err() {
                        continue; // what does not parse is already caught by another check
                    }
                    // On the binding's TEXT and not on the `Chord`: that is
                    // where the rule lives — what a preset wrote — and the
                    // type does not expose its fields outside its module.
                    let mut parts: Vec<&str> = txt.split('+').collect();
                    let Some(key) = parts.pop() else { continue };
                    let with_ctrl = parts
                        .iter()
                        .any(|m| m.eq_ignore_ascii_case("ctrl") || m.eq_ignore_ascii_case("mod"));
                    let bare_letter = key.chars().count() == 1
                        && key.chars().next().is_some_and(|c| c.is_ascii_uppercase());
                    if with_ctrl && bare_letter {
                        dead.push(format!("{name}: \u{ab}{txt}\u{bb} -> {}", b.run));
                    }
                }
            }
        }
        assert!(
            dead.is_empty(),
            "chords no terminal delivers without Kitty's protocol, which \
             norte does not enable:\n  {}",
            dead.join("\n  ")
        );
    }

    /// The four profile commands exist in the shared catalogue and NO
    /// preset binds them.
    ///
    /// #228 was the opposite gap — presets that left core commands with no
    /// key at all — and the lesson from that is not "bind everything":
    /// binding four new keys in seven presets with nobody asking for it
    /// decides for the reader which key is a profile, on top of keys that
    /// in their lifelong manager mean something else. They are reached
    /// through the palette and the menu, and whoever wants a chord binds it
    /// themselves.
    #[test]
    fn profile_commands_exist_and_no_preset_binds_them() {
        let profile = [
            "profile.pick",
            "profile.next",
            "profile.prev",
            "profile.save-as",
        ];
        for cmd in profile {
            assert!(
                crate::keymap::catalogue::lookup(cmd).is_some(),
                "{cmd} is not in the shared catalogue"
            );
        }
        for name in NAMES {
            let src = source(name).expect("NAMES resolves");
            let kf =
                crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {name}: {e}"));
            for run in preset_runs(&kf) {
                assert!(
                    !profile.contains(&run),
                    "preset {name} binds {run:?}: profiles are reached with no chord"
                );
            }
        }
    }

    /// Check 7, scoped to the FOUR imported presets (spec "K2b — the four
    /// presets": "Each file records the program, its version, the source,
    /// and the date it was transcribed, so that when it ages the staleness
    /// is dated rather than unknown" — plan rule 8's mandatory header
    /// block). `orthodox`/`vim`/`cua` predate this convention and are not
    /// transcriptions of one external document with a version to record —
    /// there is no `<Program> <version>` to name for "classic mc" or
    /// "vim-like" the way there is for "Total Commander 11.58" — so the same
    /// header shape does not fit them, and asserting it there would either
    /// fail on files this task did not touch or force a fabricated
    /// "transcribed from" that names no real source. Same rescoping the plan
    /// already did the other direction (K2b Task 2's `pane_gesture_chords_*`
    /// and friends, scoped to the three NATIVE presets and explaining why in
    /// their own doc comments).
    const TRANSCRIBED_IMPORTS: [&str; 4] = ["total-commander", "krusader", "far", "norton"];

    /// `true` if `s` contains a substring shaped like `YYYY-MM-DD`
    /// (ASCII digits and hyphens only — no calendar validation, this is a
    /// staleness DATE STAMP, not a parser). Manual scan instead of a `regex`
    /// dependency for one ten-byte pattern checked over four short strings.
    fn has_iso_date(s: &str) -> bool {
        let b = s.as_bytes();
        b.len() >= 10
            && (0..=b.len() - 10).any(|i| {
                let w = &b[i..i + 10];
                w[..4].iter().all(u8::is_ascii_digit)
                    && w[4] == b'-'
                    && w[5..7].iter().all(u8::is_ascii_digit)
                    && w[7] == b'-'
                    && w[8..10].iter().all(u8::is_ascii_digit)
            })
    }

    #[test]
    fn imported_presets_date_their_transcription_on_the_first_line() {
        for name in TRANSCRIBED_IMPORTS {
            let src = source(name).expect("NAMES resolves");
            let first = src.lines().next().unwrap_or_default();
            assert!(
                first.starts_with('#'),
                "preset {name}: the first line is not a comment: {first:?}"
            );
            assert!(
                first.contains("transcribed"),
                "preset {name}: the first line does not name a transcription: {first:?}"
            );
            assert!(
                has_iso_date(first),
                "preset {name}: the first line carries no ISO date: {first:?}"
            );
        }
    }
}
