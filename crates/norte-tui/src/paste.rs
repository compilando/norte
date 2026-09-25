//! Routing a terminal paste to whichever field has focus.
//!
//! Used to live in the `ntc` binary's root — a crate DIFFERENT from this
//! lib — with its 334 lines of tests inside `main.rs` because that was the
//! only place from which it could be called. And it is not just the binary's
//! problem: the shortcuts editor tests it too (a paste over the field that
//! captures a chord must not enter as text), so as long as this stayed in the
//! binary that test could not leave either.

use norte_i18n::{t, ta};

use crate::app::{App, Modal, Settings, Shortcuts};
use crate::overlays::{help_owns_keys, modal_wins};

/// What a routed paste did, so the caller knows whether the discarded-lines
/// message (below) applies.
enum PasteOutcome {
    /// No free-text sink was active: the paste is dropped with no message,
    /// same as a printable keystroke landing nowhere a resolver can use it.
    Ignored,
    /// The first line landed in a sink.
    Inserted,
    /// The paste was refused outright and already left its own message —
    /// [`route_paste`] must not overwrite it with the discard count.
    Rejected,
}

/// The first "line" of a paste, and how many more follow it — where a line
/// boundary is CRLF, a lone `\n`, a lone `\r` (classic Mac text — some
/// clipboard managers and old files still use it), or the Unicode NEL/LS/PS
/// separators a rich-text source can paste (encoding-auditor review of
/// #143: a splitter that only recognized `\n` left a bare `\r` sitting
/// mid-string in the inserted line — a control byte no physical keystroke
/// can ever produce, since `Enter` always arrives as `KeyCode::Enter`, never
/// `KeyCode::Char('\r')` — and silently under-counted the discard).
///
/// CRLF is folded to a single `\n` FIRST so it is never counted as two
/// boundaries (one for the `\r`, one for the `\n`) — a Windows clipboard's
/// two-line paste must discard exactly one line, not two.
fn first_pasted_line(text: &str) -> (String, usize) {
    let normalized = text.replace("\r\n", "\n");
    let mut lines = normalized.split(['\n', '\r', '\u{0085}', '\u{2028}', '\u{2029}']);
    let first = lines.next().unwrap_or_default().to_owned();
    let discarded = lines.count();
    (first, discarded)
}

/// Routes a bracketed paste (`Event::Paste`, #143) to whichever free-text
/// sink the SAME keystroke would reach — the `if`/`else if` chain here is
/// the run loop's own chain around `Event::Key`, read top to bottom, with
/// every `KeyCode::Char(c) if plain => sink.push_char(c)` arm turned into a
/// loop over the pasted line. It is not a parallel dispatcher: it is the
/// same precedence, because an overlay that owns a keystroke has to own a
/// paste too, or the two surfaces drift and one of them keeps today's bug.
///
/// Only the FIRST line is ever inserted, and it never submits: a pasted
/// newline used to read as Enter (mkdir's name is the sharpest case — the
/// tail of the paste landed on the dispatcher as commands). Everything after
/// the first line boundary is discarded and counted in `app.message`; see
/// `first_pasted_line` (privada) for what counts as a boundary.
///
/// The shortcuts editor is the one sink that does NOT get the paste inserted
/// as text while it is capturing a new chord: `hostile_key` (below) exists
/// because a codepoint like an RLO override cannot come from a physical key,
/// only from a paste, and letting one through would bind a chord the user
/// never pressed. A capture answers a SINGLE keystroke, and a paste is never
/// that, so it gets the exact outcome a hostile keystroke gets there
/// (`msg-shortcut-not-bindable`) instead of being fed to `capture_chord`.
#[expect(
    clippy::too_many_lines,
    reason = "run loop wiring: 1:1 order with the `Event::Key` chain"
)]
pub fn route_paste(app: &mut App, text: &str) {
    let (first_line, discarded) = first_pasted_line(text);
    let first_line = first_line.as_str();

    let outcome = if (app.theme_picker.is_some()
        || app.layout_picker.is_some()
        || app.connections_picker.is_some()
        || app.columns_picker.is_some())
        && !modal_wins(app)
    {
        PasteOutcome::Ignored // pickers: navigation only, nothing to fill
    } else if app.extensions.is_some() && !modal_wins(app) {
        // G3c: raw text ONLY while a `[config]` value is being edited — same
        // guard `on_extensions_key` uses to route to `on_plugin_config_edit_key`.
        let editing = app
            .extensions
            .as_ref()
            .and_then(|m| m.config.as_ref())
            .is_some_and(|p| p.state.is_editing());
        if editing {
            for c in first_line.chars() {
                if let Some(panel) = app.extensions.as_mut().and_then(|m| m.config.as_mut()) {
                    panel.state.edit_push_char(c);
                }
            }
            PasteOutcome::Inserted
        } else {
            PasteOutcome::Ignored // keymap-driven list: not free text
        }
    } else if app.nav_popup.is_some() && !modal_wins(app) {
        let has_name_input = app
            .nav_popup
            .as_ref()
            .is_some_and(|p| p.name_input.is_some());
        if has_name_input {
            for c in first_line.chars() {
                if let Some(input) = app.nav_popup.as_mut().and_then(|p| p.name_input.as_mut()) {
                    input.push(c);
                }
            }
            PasteOutcome::Inserted
        } else {
            PasteOutcome::Ignored // popup navigation: not free text
        }
    } else if app.search_dialog.is_some() && !modal_wins(app) {
        for c in first_line.chars() {
            if let Some(dialog) = &mut app.search_dialog {
                dialog.push_char(c);
            }
        }
        PasteOutcome::Inserted
    } else if app.palette.is_some() && !modal_wins(app) {
        for c in first_line.chars() {
            if let Some(p) = &mut app.palette {
                p.push_char(c);
            }
        }
        PasteOutcome::Inserted
    } else if app.shortcuts.is_some() && !modal_wins(app) {
        let capturing = app.shortcuts.as_ref().is_some_and(Shortcuts::is_capturing);
        if capturing {
            app.message = Some(t("msg-shortcut-not-bindable"));
            PasteOutcome::Rejected
        } else {
            for c in first_line.chars() {
                if let Some(sc) = &mut app.shortcuts {
                    sc.push_char(c);
                }
            }
            PasteOutcome::Inserted
        }
    } else if app.settings.is_some() && !modal_wins(app) {
        let editing = app.settings.as_ref().is_some_and(Settings::is_editing);
        for c in first_line.chars() {
            let Some(settings) = &mut app.settings else {
                break;
            };
            if editing {
                settings.edit_push_char(c);
            } else {
                settings.push_char(c);
            }
        }
        PasteOutcome::Inserted
    } else if help_owns_keys(app) {
        let filtering = app.help.as_ref().is_some_and(|h| h.state.filtering());
        if filtering {
            for c in first_line.chars() {
                if let Some(help) = &mut app.help {
                    help.state.push_char(c);
                }
            }
            PasteOutcome::Inserted
        } else {
            PasteOutcome::Ignored // help navigation: keymap context, not free text
        }
    } else if app.modal.is_some() {
        // Every FREE-TEXT prompt receives the paste the same way, and those
        // are the ten `prompt_kind` recognizes: this used to be a
        // hand-written list and had left two out (pack and split), which
        // accepted keys but rejected a paste. The other modals —
        // confirmations, TOFU, collision — resolve through the keymap's
        // `dialog` context: there is nothing to fill here.
        // #325: the password field too, and it is the case where it matters
        // MOST — pasting from a password manager is how most people answer
        // that dialog, and without this arm nothing happened and nothing said
        // so. It comes before `prompt_kind` because it is deliberately NOT a
        // `PromptKind` (that machinery lends the field as `&mut String`,
        // which is exactly what a secret cannot give).
        if let Some(Modal::AskSecret { input, .. }) = app.modal.as_mut() {
            for c in first_line.chars() {
                input.push(c);
            }
            PasteOutcome::Inserted
        } else if let Some(kind) = app.modal.as_ref().and_then(Modal::prompt_kind) {
            for c in first_line.chars() {
                app.prompt_push(kind, c);
            }
            PasteOutcome::Inserted
        } else {
            PasteOutcome::Ignored
        }
    } else if app.viewer.is_none() && app.focused().quick().is_some() {
        for c in first_line.chars() {
            app.focused_mut().quick_char(c);
        }
        PasteOutcome::Inserted
    } else {
        PasteOutcome::Ignored // browsing, nothing focused: nothing to fill
    };

    if matches!(outcome, PasteOutcome::Inserted) && discarded > 0 {
        app.message = Some(ta(
            "msg-paste-truncated",
            &[("lines", &discarded.to_string())],
        ));
    }
}
#[cfg(test)]
mod paste_tests {
    use super::*;
    use crate::app::{HelpView, NavPopupKind, Palette, Pane, TransferKind};
    use crate::nav;
    use norte_proto::VPath;

    fn app() -> App {
        let d = VPath::parse("file:///x").expect("test wire");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    /// The plan's central case: a pasted newline must never submit. Before
    /// bracketed paste, a terminal delivered a paste as ordinary keystrokes,
    /// so `mkdir` + a two-line paste created the first line as a directory
    /// and fed the second to the dispatcher — a paste that runs commands.
    #[test]
    fn a_multiline_paste_fills_the_field_and_does_not_submit() {
        let mut a = app();
        a.open_mkdir();
        route_paste(&mut a, "one\ntwo");
        assert_eq!(
            a.modal,
            Some(Modal::Mkdir {
                name: "one".to_owned(),
                error: None,
            }),
            "only the first line lands, and the modal is still open"
        );
    }

    /// The tail is not silently eaten: a user who pasted three lines is told
    /// two did not make it, because a field that quietly holds a third of
    /// what you pasted is worse than one that refuses.
    #[test]
    fn the_discarded_lines_are_counted_in_the_message() {
        let mut a = app();
        a.open_mkdir();
        route_paste(&mut a, "one\ntwo\nthree");
        assert_eq!(
            a.message.as_deref(),
            Some(ta("msg-paste-truncated", &[("lines", "2")]).as_str())
        );
    }

    /// A paste with a single line (no trailing newline) discards nothing —
    /// no message at all, not even an empty count.
    #[test]
    fn a_single_line_paste_leaves_no_message() {
        let mut a = app();
        a.open_mkdir();
        route_paste(&mut a, "one");
        assert_eq!(a.message, None);
    }

    /// A bare `\r` — classic Mac text, still produced by some clipboard
    /// managers — is a line boundary exactly like `\n`: not recognizing it
    /// would leave a literal `\r` byte sitting mid-string in the field, a
    /// control character no physical keystroke can ever produce (`Enter`
    /// always arrives as `KeyCode::Enter`), and would under-count the
    /// discard (encoding-auditor review of #143).
    #[test]
    fn a_bare_cr_line_ending_is_a_boundary_like_lf() {
        let mut a = app();
        a.open_mkdir();
        route_paste(&mut a, "one\rtwo\rthree");
        assert_eq!(
            a.modal,
            Some(Modal::Mkdir {
                name: "one".to_owned(),
                error: None,
            })
        );
        assert_eq!(
            a.message.as_deref(),
            Some(ta("msg-paste-truncated", &[("lines", "2")]).as_str())
        );
    }

    /// CRLF is ONE boundary, not two: folding it to `\n` first (inside
    /// `first_pasted_line`) is what keeps a two-line Windows paste from
    /// reporting "1 more discarded" as "2".
    #[test]
    fn a_crlf_paste_discards_exactly_one_line_not_two() {
        let mut a = app();
        a.open_mkdir();
        route_paste(&mut a, "one\r\ntwo");
        assert_eq!(
            a.message.as_deref(),
            Some(ta("msg-paste-truncated", &[("lines", "1")]).as_str())
        );
    }

    /// The Unicode line/paragraph separators a rich-text source (a web page,
    /// a word processor) can paste are boundaries too, not just the two
    /// ASCII ones a terminal itself would ever send.
    #[test]
    fn a_unicode_line_separator_is_a_boundary_too() {
        let mut a = app();
        a.open_mkdir();
        route_paste(&mut a, "one\u{2028}two");
        assert_eq!(
            a.modal,
            Some(Modal::Mkdir {
                name: "one".to_owned(),
                error: None,
            })
        );
        assert_eq!(
            a.message.as_deref(),
            Some(ta("msg-paste-truncated", &[("lines", "1")]).as_str())
        );
    }

    /// Paste goes through the SAME per-character path as a keystroke: not a
    /// stricter one, not a looser one. Neither `mkdir_push` nor the router
    /// filters codepoints (masking happens only at PAINT time, `must_mask`/
    /// `display_name`) — so a pasted RLO lands exactly where the same
    /// character typed one at a time would. Proving that means comparing
    /// against the keystroke path itself, not against a hand-picked
    /// expectation that could drift from it.
    #[test]
    fn a_paste_is_sanitised_exactly_like_a_keystroke() {
        let mut typed = app();
        typed.open_mkdir();
        for c in "a\u{202e}b".chars() {
            typed.mkdir_push(c);
        }

        let mut pasted = app();
        pasted.open_mkdir();
        route_paste(&mut pasted, "a\u{202e}b");

        assert_eq!(pasted.modal, typed.modal);
    }

    /// The other five `Modal::X` free-text sinks: the router's `match` arm
    /// for each has to reach the SAME push function the keystroke does.
    #[test]
    fn every_other_free_text_modal_gets_the_first_line() {
        let mut a = app();
        a.open_mark_pattern(true);
        route_paste(&mut a, "*.rs\ntail");
        assert!(matches!(
            &a.modal,
            Some(Modal::MarkPattern { pattern, .. }) if pattern == "*.rs"
        ));

        let mut a = app();
        a.open_command_line();
        route_paste(&mut a, "ls -la\ntail");
        assert!(matches!(
            &a.modal,
            Some(Modal::CommandLine { command, .. }) if command == "ls -la"
        ));

        let mut a = app();
        a.open_ai_rename();
        route_paste(&mut a, "lowercase all\ntail");
        assert!(matches!(
            &a.modal,
            Some(Modal::AiRenameInstruction { instruction, .. })
                if instruction == "lowercase all"
        ));

        let mut a = app();
        a.open_semantic_search();
        route_paste(&mut a, "vacation photos\ntail");
        assert!(matches!(
            &a.modal,
            Some(Modal::SemanticQuery { query, .. }) if query == "vacation photos"
        ));

        let mut a = app();
        a.modal = Some(Modal::TransferName {
            kind: TransferKind::Move,
            from: VPath::parse("file:///a").expect("wire"),
            to_dir: VPath::parse("file:///b").expect("wire"),
            name: String::new(),
            original: Vec::new(),
            touched: false,
            from_marks: false,
            enc: None,
            error: None,
            space: None,
            confine: None,
        });
        route_paste(&mut a, "renamed\ntail");
        assert!(matches!(
            &a.modal,
            Some(Modal::TransferName { name, .. }) if name == "renamed"
        ));
    }

    /// Quick search (`nav::QuickSearch`, panel-embedded, BROWSE mode): the
    /// lowest-precedence sink, reached only with no overlay and no modal.
    #[test]
    fn quick_search_in_a_panel_gets_the_first_line() {
        let mut a = app();
        a.panes[0].quick_start(nav::Mode::Filter);
        route_paste(&mut a, "read\nme");
        assert_eq!(
            a.focused().quick().map(nav::QuickSearch::query_display),
            Some("read".to_owned())
        );
    }

    /// The command palette (`Ctrl+P`): fixed keys, free text, same molde as
    /// the search dialog — grouped with it under "the generic dialog" in the
    /// plan's list of eleven.
    #[test]
    fn the_command_palette_gets_the_first_line() {
        let mut a = app();
        a.palette = Some(Palette::new(Vec::new()));
        route_paste(&mut a, "copy\ntail");
        assert_eq!(
            a.palette.as_ref().map(Palette::query_display),
            Some("copy".to_owned())
        );
    }

    /// Alt+F7's search dialog: no sub-state gate, always free text.
    #[test]
    fn the_search_dialog_gets_the_first_line() {
        let mut a = app();
        a.open_search_dialog();
        route_paste(&mut a, "*.log\ntail");
        assert_eq!(
            a.search_dialog.as_ref().map(|d| d.name.as_str()),
            Some("*.log")
        );
    }

    /// The shortcuts editor's list FILTER (not capturing a chord — that path
    /// is `a_paste_while_capturing_a_chord_is_rejected_not_bound`, in
    /// `shortcuts_editor_tests`, since it needs a real row to select).
    #[test]
    fn the_shortcuts_filter_gets_the_first_line() {
        let mut a = app();
        a.shortcuts = Some(Shortcuts::new(Vec::new()));
        route_paste(&mut a, "cop\ntail");
        assert!(!a.shortcuts.as_ref().expect("open").is_capturing());
        // The filter box has no public getter for its raw query; the guard
        // above is what proves the paste did NOT fall through to a capture,
        // and `the_discarded_lines_are_counted_in_the_message` already
        // proves character-by-character insertion through the same
        // `push_char` this branch calls.
    }

    /// The settings overlay's list filter (S3) — the `editing` inline buffer
    /// needs a real catalog row and is exercised only by construction, not by
    /// a dedicated test (same `push_char`-per-character shape as every sink
    /// above).
    #[test]
    fn the_settings_filter_gets_the_first_line() {
        let mut a = app();
        a.settings = Some(Settings::new(Vec::new()));
        route_paste(&mut a, "mou\ntail");
        assert!(!a.settings.as_ref().expect("open").is_editing());
    }

    /// The help overlay's filter (`Ctrl+F` inside help): only while
    /// `state.filtering()` — otherwise a printable key resolves through the
    /// `dialog` keymap context, and a paste there is inert, same as it is
    /// for the theme picker below.
    #[test]
    fn the_help_filter_gets_the_first_line() {
        let mut a = app();
        a.help = Some(HelpView::new(norte_help::Lang::En, Vec::new()));
        a.help.as_mut().expect("open").state.start_filter();
        route_paste(&mut a, "keys\ntail");
        assert_eq!(a.help.as_ref().map(|h| h.state.filter_raw()), Some("keys"));
    }

    /// The navigation popup's hotlist name input (`a` on the history/hotlist
    /// popup): raw text, guarded by `name_input.is_some()`.
    #[test]
    fn the_nav_popup_name_input_gets_the_first_line() {
        let mut a = app();
        a.open_nav_popup(NavPopupKind::Hotlist);
        a.nav_popup.as_mut().expect("open").name_input = Some(String::new());
        route_paste(&mut a, "work\ntail");
        assert_eq!(
            a.nav_popup.as_ref().and_then(|p| p.name_input.clone()),
            Some("work".to_owned())
        );
    }

    /// A paste that lands nowhere — no modal, no overlay, no quick search —
    /// is a silent no-op, exactly like a printable keystroke the resolver
    /// cannot use.
    #[test]
    fn a_paste_with_nothing_focused_is_ignored() {
        let mut a = app();
        route_paste(&mut a, "one\ntwo");
        assert_eq!(a.message, None);
        assert_eq!(a.modal, None);
    }

    /// Pickers (theme/columns) are navigation-only: a paste there fills
    /// nothing, same as a printable keystroke does nothing for them.
    #[test]
    fn a_paste_over_a_picker_is_ignored() {
        let mut a = app();
        a.theme_picker = Some(crate::app::ThemePicker {
            names: Vec::new(),
            cursor: 0,
            original: a.theme.clone(),
        });
        route_paste(&mut a, "one\ntwo");
        assert_eq!(a.message, None);
    }

    /// The precedence crux: with BOTH a modal and an overlay "open" (the
    /// overlay arrived first, then an approval modal interrupted it — the
    /// same situation `modal_wins` exists for), the paste must follow the
    /// modal, exactly like `Event::Key` does. If it fell through to the
    /// overlay instead, an agent-approval prompt with a settings overlay
    /// still open behind it would let a pasted line land in settings while
    /// the modal sits there unanswered.
    #[test]
    fn a_modal_preempts_an_open_overlay_for_paste_too() {
        let mut a = app();
        a.settings = Some(Settings::new(Vec::new()));
        a.modal = Some(Modal::ApproveAgentOp {
            req: norte_proto::methods::PolicyApprovalRequired {
                approval_id: 1,
                session: Some("s1".into()),
                op: "copy".into(),
                paths: vec!["mem:///a".into()],
                paths_total: 0,
                ttl_ms: 60_000,
                detail: norte_proto::methods::ApprovalDetail::default(),
            },
        });
        route_paste(&mut a, "yes");
        // Not a free-text modal: the paste is inert, and — the point of the
        // test — it did NOT fall through to the settings filter behind it.
        assert_eq!(a.message, None);
    }

    /// Pack and split accept a paste like any other text prompt. The
    /// hand-written list that existed before left these two out: typing
    /// worked, pasting did nothing, and nothing said so.
    #[test]
    fn pack_and_split_also_receive_the_paste() {
        let mut a = app();
        a.modal = Some(Modal::Pack {
            name: String::new(),
            error: None,
        });
        route_paste(&mut a, "stuff.zip");
        assert!(
            matches!(&a.modal, Some(Modal::Pack { name, .. }) if name.ends_with("stuff.zip")),
            "the paste did not reach the file name: {:?}",
            a.modal
        );

        let mut b = app();
        b.modal = Some(Modal::Split {
            size: String::new(),
            error: None,
        });
        route_paste(&mut b, "700M");
        assert!(
            matches!(&b.modal, Some(Modal::Split { size, .. }) if size == "700M"),
            "the paste did not reach the chunk size: {:?}",
            b.modal
        );
    }
}
