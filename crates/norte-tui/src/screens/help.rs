//! The help overlay (H3b): one key inside, and the command the event loop
//! has to dispatch on exit.
//!
//! It used to live in the `ntc` binary's root — a crate DISTINCT from this
//! lib — so its 951 lines of tests could not be a `tests/` file, which is
//! what they are.
//!
//! A file separate from [`crate::help`] (the chords help PAINTS), from
//! [`crate::help_context`] (which topic opens on which screen) and from
//! [`crate::help_render`] (the render pipeline): this is only the one that
//! reads its keys.
//!
//! [`on_help_key`]'s rustdoc was SPLIT in two by `main.rs`: the
//! introduction and the "TWO REGIMES ...:" that closes it had ended up over
//! [`HelpDispatch`], and the bullet list that colon announces was still over
//! the function. Here they go back to being a single block, without a word
//! changed.

use crossterm::event::{KeyCode, KeyModifiers};
use norte_core::backend::Backend;
use norte_i18n::{t, ta};

use crate::app::{App, HelpOutcome, PAGE, Palette, detail_for_bar, error_message};
use crate::keymap::{Command, Resolution, Resolver, chord_from_crossterm, parse_plugin_key};

/// Keys that MOVE, in the sidebar or in the body: arrows, page, ends and
/// section jumps.
///
/// A page, in the sidebar, is ten topics; in the body it is the window that
/// is visible, which is what a reader means by "one screen" — with a fixed
/// ten, `PgDn` scrolled down half a screen on a tall terminal.
fn scroll(help: &mut crate::app::HelpView, outcome: HelpOutcome) {
    let page = if help.state.focus() == norte_frontend::help::Focus::Body {
        help.page()
    } else {
        PAGE
    };
    match outcome {
        HelpOutcome::Up => help.line_up(),
        HelpOutcome::Down => help.line_down(),
        HelpOutcome::PageUp => help.state.page_up(page),
        HelpOutcome::PageDown => help.state.page_down(page),
        HelpOutcome::Top => help.state.top(),
        HelpOutcome::Bottom => help.state.bottom(),
        HelpOutcome::SectionPrev => help.section_prev(),
        HelpOutcome::SectionNext => help.section_next(),
        _ => {}
    }
}

/// What [`on_help_key`] hands the run loop to execute.
///
/// Two variants because a help row can name two different KINDS of thing, and
/// only the run loop can run either: this function is sync (its tests are, and
/// its callers are async), while both destinations need an `await`.
///
/// Keeping them apart in the type rather than collapsing to a string is the
/// point — `Command` is the closed, parsed vocabulary of the app (#112), and a
/// plugin key is deliberately NOT in it: its `command_id` half comes from a
/// third-party manifest with no validated charset, so it must never be handed
/// to a lookup as though it were one of ours.
#[derive(Debug, PartialEq, Eq)]
pub enum HelpDispatch {
    /// A built-in command, already parsed against `COMMANDS`.
    Command(Command),
    /// A plugin-contributed command: `(plugin_id, command_id)`, split at the
    /// FIRST colon after the prefix ([`parse_plugin_key`]).
    Plugin(String, String),
}

/// Runs a plugin's command and announces the result (P1, H3e).
///
/// The ONE place either surface dispatches one. It was written inline in the
/// palette's `Enter` arm and the help overlay grew a second need for it in
/// H3e; a copy would have been a second path with its own answer to what a
/// failure looks like, and H3b's rule is that executing from the help goes
/// through the SAME dispatch as the palette, with nothing bypassed.
///
/// Authorisation is the SERVER's: `plugin.run_command` resolves the command
/// against the catalogue and enforces approved+enabled itself
/// (`resolve_runnable`), independently of any snapshot a client froze. What a
/// client-side check buys is agreement with what the reader is looking at, and
/// it is never what permits the call.
///
/// The plugin's output is UNTRUSTED text: it goes through `detail_for_bar`
/// (masked and capped, pattern #73) before it reaches the status bar.
pub async fn run_plugin_command(app: &mut App, backend: &Backend, id: &str, command: &str) {
    app.message = Some(match backend.plugin_run_command(id, command, "").await {
        Ok(output) => ta("msg-plugin-run-ok", &[("output", &detail_for_bar(&output))]),
        Err(e) => error_message(&e),
    });
}

/// Routes one key inside the help overlay (H3b), and answers with the command
/// the run loop must DISPATCH — `Some` only for `Enter` on a runnable body
/// row, and only after this function has already closed the overlay.
///
/// Extracted from the run loop for the same reason as
/// [`on_columns_key`](crate::screens::pickers::on_columns_key):
/// everything here is decidable from `App` plus the `dialog` resolver, and the
/// dispatch it hands back is the one thing that is not.
///
/// TWO REGIMES, the same split the palette and the search dialog already have:
///
/// * While the sidebar filter is open the keys are FIXED. There is no
///   `dialog.*` verb for "type a character", so resolving through the keymap
///   here would make every printable key mean whatever it is bound to instead
///   of itself. `Esc` LEAVES the box keeping the text — the model's contract:
///   leaving a search is not undoing it, `Backspace` is what empties it.
/// * Otherwise the key resolves through the shared `dialog` resolver like
///   every other overlay's, and the resulting command is filtered through
///   [`crate::app::help_action`]/`ALLOW_HELP` — the SAME list the footer
///   hint is generated from. A verb outside it is inert.
///
/// Two keys keep their global meaning ahead of both regimes (H1 T2, as in
/// every other overlay): `ctrl+c` quits, and `ctrl+p` hands what the reader
/// has typed to the command palette — the two are the same model at different
/// speeds (the `help` topic says as much), so the filter should not have to be
/// retyped to cross between them.
///
/// That second bridge is REFUSED while the page covers a modal
/// (`HelpView::over_modal`), the same guard the `Action::Run` arm makes: the
/// palette would open behind a live dialog, painted but unable to receive a
/// key, and every keystroke meant for its filter would be answering the dialog
/// instead.
pub fn on_help_key(
    app: &mut App,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) -> Option<HelpDispatch> {
    // Global emergency exit, hardcoded BEFORE resolving — as in every
    // overlay (this file's, never `Command::AppQuit`: it does not ask).
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return None;
    }
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('p') {
        let help = app.help.as_ref()?;
        // Review H3c MAJOR-1: the SAME guard as the `Action::Run` arm
        // below, and for a worse reason. Crossing to the palette would
        // leave the modal standing, and the run loop's palette branch is
        // gated by `!modal_wins`: the palette would end up PAINTED looking
        // alive and receiving not a single key — all of them fall into the
        // modal branch and resolve against its allowlist. Typing `copy` to
        // filter over an agent approval would discard `c`, `o`, `p`, and
        // the `y` would APPROVE the mutation. Help stays open and says why.
        if help.over_modal {
            app.message = Some(t("msg-help-modal-waiting"));
            return None;
        }
        let filter = help.state.filter_raw().to_owned();
        app.help = None;
        // With no plugin rows: `Command::AppPalette` requests them from the
        // backend and this function is SYNC on purpose (everything else
        // here is too). A known, bounded degradation — the built-ins, which
        // are what help documents, are all there.
        let mut palette = Palette::new(crate::palette::rows_for_context(
            &app.palette_rows,
            app.viewer.is_some(),
        ));
        // The RAW filter (`filter_raw`, not the masked one used for
        // painting): it is what gets matched, and the palette masks it
        // again when painting it.
        for c in filter.chars() {
            palette.push_char(c);
        }
        app.palette = Some(palette);
        return None;
    }
    // Regime 1: filter editor. FIXED keys (see doc above).
    if app.help.as_ref()?.state.filtering() {
        // `plain` as in the palette: SHIFT is part of typing an uppercase
        // letter, not a modifier that changes the key's meaning.
        let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
        let help = app.help.as_mut()?;
        match code {
            KeyCode::Char(c) if plain => help.state.push_char(c),
            KeyCode::Backspace if plain => help.state.backspace(),
            // Both LEAVE the box keeping the text: Esc because the model
            // promises it, Enter because the filter is already applied
            // (the sidebar rebuilds on every character) and all that is
            // left to do is give the arrows back to navigation.
            KeyCode::Esc | KeyCode::Enter if plain => help.state.end_filter(),
            // Without leaving the box: picking a hit while still refining
            // the search is the gesture that makes a filter useful.
            KeyCode::Up if plain => help.state.up(),
            KeyCode::Down if plain => help.state.down(),
            _ => {}
        }
        return None;
    }

    // Regime 2: the keymap decides (`dialog` context, rebindable).
    let chord = chord_from_crossterm(mods, code)?;
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        // No sequence semantics defined for overlays (T2), and likewise for
        // a key bound to something this build does not run (K1 T4): ignore
        // and reset the resolution state.
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return None;
        }
        Resolution::Reset => return None,
    };
    // The key that OPENS help CLOSES it. `app.help` is a `[global]`
    // command, not a dialog verb, so it does not live in `ALLOW_HELP` and
    // without this branch F1 would be inert inside help — the one key on
    // the keyboard the reader is guaranteed for this overlay, with no
    // effect. It resolves through the keymap like everything else (a
    // rebind of `app.help` moves BOTH halves of the switch at once); what
    // is hardcoded is the meaning, not the key. Same criterion as F9 in
    // `on_theme_picker_key`.
    if cmd == "app.help" {
        app.help = None;
        return None;
    }
    // Outside `ALLOW_HELP` the key is INERT (same discipline as the rest of
    // the overlays: semantics live in code, the keymap only assigns keys).
    let outcome = crate::app::help_action(&cmd)?;

    let help = app.help.as_mut()?;
    // Read BEFORE the `match`: the arm that consults it no longer has
    // `help` at hand (it assigns `app.message`, which claims the borrow
    // back).
    let over_modal = help.over_modal;
    match outcome {
        HelpOutcome::TogglePane => help.state.toggle_focus(),
        HelpOutcome::StartFilter => help.state.start_filter(),
        // With history, it goes back; with NO history, it closes. This is
        // what turns `Backspace` into an honest key instead of a dead one
        // at the root: "back" from where you cannot go back further is
        // exiting.
        HelpOutcome::Back => {
            if !help.state.back() {
                app.help = None;
            }
        }
        HelpOutcome::Close => app.help = None,
        HelpOutcome::Activate => match help.state.action().cloned() {
            // A link is followed and help STAYS open: reading is not
            // leaving.
            Some(norte_frontend::help::Action::Open(id)) => help.state.open(&id),
            Some(norte_frontend::help::Action::Run(cmd)) => {
                // H3c: help open ON TOP of a modal dispatches nothing on
                // the panes. `dispatch` plants its own modals (a copy
                // confirmation), so the command would REPLACE the one
                // waiting for an answer: an agent approval would vanish
                // from the screen with nobody having answered it. It is
                // said and help stays open — same treatment as the
                // non-dispatchable row below.
                if over_modal {
                    app.message = Some(t("msg-help-modal-waiting"));
                    return None;
                }
                // A topic's `commands` list can name a `dialog.*` verb —
                // the `help` topic documents three — and those are overlay
                // vocabulary, not something a pane can run: they are not in
                // `COMMANDS` and `Command::parse` rejects them. It is said
                // and help stays open; eating the Enter silently would
                // read as though the command ran. (The rest of the
                // corpus's ids DO parse: the documentation gate checks them
                // byte for byte against `COMMANDS ∪ DIALOG_COMMANDS`.)
                // (H3e) A PLUGIN row. Its key is `plugin:{id}:{cmd}`, which
                // does not live in `COMMANDS` and which `Command::parse`
                // rejects — so without this arm the Enter fell into the
                // `msg-help-not-runnable` below and the app refused to run
                // exactly the row it had just painted as available, with
                // the footer promising `⏎ run`. The dimming was decorative.
                if let Some((id, command)) = parse_plugin_key(&cmd) {
                    // The frozen snapshot DIMS; it never AUTHORIZES.
                    // Refusing here is consistent with what the reader has
                    // in front of them — a dimmed row that ran when
                    // clicked would be worse than not dimming anything —
                    // but authority still belongs to `resolve_runnable` on
                    // the server, which checks approved+enabled on its own
                    // and trusts no client. Two checks saying the same
                    // thing, one polite and one binding.
                    if !norte_help::ChordResolver::availability(&*app.help_chords, &cmd)
                        .is_available()
                    {
                        app.message = Some(t("msg-help-not-runnable"));
                        return None;
                    }
                    let (id, command) = (id.to_owned(), command.to_owned());
                    // Close BEFORE dispatching, as below.
                    app.help = None;
                    return Some(HelpDispatch::Plugin(id, command));
                }
                let Some(parsed) = Command::parse(&cmd) else {
                    // The status bar is visible: the overlay takes the
                    // frame minus one row on top and one on the bottom, and
                    // the bar is that last row (`ui::help_layout`).
                    app.message = Some(t("msg-help-not-runnable"));
                    return None;
                };
                // Closing BEFORE dispatching is deliberate: the command
                // acts on the panes below and help would cover whatever
                // confirmation it opens.
                app.help = None;
                return Some(HelpDispatch::Command(parsed));
            }
            // Focus in the sidebar. Steering the cursor already PREVIEWS
            // (opens whatever it lands on), so the highlighted topic is
            // usually ALREADY the open one and `open` would do nothing: a
            // silent Enter, indistinguishable from a failure. When they
            // match, Enter enters THE BODY; when they do not — the only
            // remaining case, following a `see_also` from a filtered list,
            // where the highlight stayed on the nearest visible row —
            // opens. In both branches Enter means the same thing: "go to
            // what I am looking at."
            None => {
                let selected = help.state.selected_topic().cloned();
                if selected.is_some_and(|id| id != *help.state.current()) {
                    help.state.open_selected();
                } else {
                    help.state.toggle_focus();
                }
            }
        },
        // What is left are the scrolling keys (arrows, page, ends,
        // sections): every other variant has its arm above.
        outcome => scroll(help, outcome),
    }
    None
}

/// The help overlay's key routing, driven through [`on_help_key`] — the same
/// seam the run loop uses, so these exercise the WIRING (allowlist, the two
/// regimes, what closes the overlay, what the run loop is asked to dispatch)
/// and not the model underneath, which has its own tests in
/// `norte_frontend::help`.
#[cfg(test)]
mod help_key_tests {
    use super::*;
    use crate::app::{HelpView, Modal, Pane};
    use crate::keymap::{COMMANDS, DIALOG_COMMANDS, Effective, Screen, presets};
    use crate::overlays::{
        close_stale_overlays, help_owns_keys, modal_help_toggle, open_contextual_help,
        refuses_over_modal, settle_help_over_modal,
    };
    use norte_frontend::help::{Focus, SidebarRow};
    use norte_help::{Lang, TopicId};
    use norte_proto::VPath;

    /// An effective of the orthodox preset over the WHOLE vocabulary: the
    /// `dialog` screen merges `[global]` too, so `DIALOG_COMMANDS` alone
    /// would make `build_for` reject the preset outright.
    fn eff(screen: Screen) -> Effective {
        let (_, preset) = presets()
            .into_iter()
            .find(|(n, _)| *n == "orthodox")
            .expect("orthodox preset");
        let known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect();
        Effective::build_for(&preset, &[], &known, screen).expect("preset effective")
    }

    fn dialog_resolver() -> Resolver {
        Resolver::new(eff(Screen::Dialog))
    }

    /// Under **vim** the preset binds `app.help` to `f1` AND to `?`. The TUI
    /// resolves closing through the keymap (`cmd == "app.help"` in
    /// `on_help_key`), so both close with nothing having to enumerate
    /// them — it is the property the GUI did not have, which its
    /// `closes_help` now gives it. The test pins it here so a change in the
    /// `dialog` context's resolution does not lose it silently.
    #[test]
    fn under_vim_both_help_keys_close() {
        use norte_frontend::keymap::Resolution;

        let (_, preset) = presets()
            .into_iter()
            .find(|(n, _)| *n == "vim")
            .expect("vim preset");
        let known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect();
        let dialog = Effective::build_for(&preset, &[], &known, Screen::Dialog)
            .expect("vim preset's dialog effective");

        for (mods, code) in [
            (KeyModifiers::NONE, KeyCode::F(1)),
            (KeyModifiers::NONE, KeyCode::Char('?')),
        ] {
            let mut app = app_with_help();
            let mut resolver = Resolver::new(dialog.clone());
            let chord = chord_from_crossterm(mods, code).expect("modeled chord");
            assert!(
                matches!(resolver.push(chord), Resolution::Run { command: cmd, .. } if cmd == "app.help"),
                "{code:?} is `app.help` in the dialog context"
            );
            let mut resolver = Resolver::new(dialog.clone());
            assert!(on_help_key(&mut app, &mut resolver, mods, code).is_none());
            assert!(app.help.is_none(), "{code:?} closes help");
        }
    }

    fn app_with_help() -> App {
        let d = VPath::parse("file:///x").expect("test wire");
        let mut app = App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()));
        app.help = Some(HelpView::new(Lang::En, Vec::new()));
        app
    }

    /// The same app WITHOUT the overlay: the H3c tests open it through the
    /// production seam instead of planting a `HelpView` by hand, because what
    /// they are checking is what that seam decides.
    fn app_with_help_closed() -> App {
        let mut app = app_with_help();
        app.help = None;
        app
    }

    /// `Command::AppHelp`'s whole body ([`open_contextual_help`]), which is
    /// what `F1` runs.
    fn open_help(app: &mut App) {
        open_contextual_help(app, Lang::En, &[], None);
    }

    /// The page `context` opens today, or the documented fallback. The corpus
    /// half of the map is data being written page by page (H3h): a test that
    /// hard-coded `copying` here would fail the day a context is claimed and
    /// pass for the wrong reason until then.
    fn page_of(context: &str) -> String {
        norte_help::topic_for_context(Lang::En, context)
            .map_or_else(|| "index".to_owned(), |t| t.id.as_str().to_owned())
    }

    fn test_collision_modal() -> Modal {
        Modal::Collision {
            retry: crate::tasks::RetrySpec {
                kind: crate::app::TransferKind::Move,
                from: VPath::parse("file:///a").expect("test wire"),
                to: VPath::parse("file:///b").expect("test wire"),
                opts: norte_core::TransferOptions::default(),
                name_encoding: None,
            },
        }
    }

    /// An agent approval: the modal whose hijacking by an overlay is the
    /// defect `modal_wins` exists to close (H1 MINOR-4).
    fn test_approval_modal() -> Modal {
        Modal::ApproveAgentOp {
            req: norte_proto::methods::PolicyApprovalRequired {
                approval_id: 1,
                session: Some("s1".into()),
                op: "copy".into(),
                paths: vec!["mem:///a".into()],
                paths_total: 0,
                ttl_ms: 60_000,
                detail: norte_proto::methods::ApprovalDetail::default(),
            },
        }
    }

    /// A host key's TOFU: the SECURITY surface help CAN cover, because
    /// `dialog.trust-host` has a page (`remote`) and the agent approval does
    /// not — over that one `F1` no longer opens anything (MAJOR-2), so the
    /// "the verbs underneath are inert" tests live here. The consequence is
    /// of the same class: `dialog.approve` (the preset's `y`) TRUSTS an
    /// unverified key.
    fn test_trust_host_modal() -> Modal {
        Modal::TrustHostKey {
            host: "h".into(),
            port: Some(22),
            algo: "ssh-ed25519".into(),
            fingerprint: "SHA256:AAAA".into(),
            dir: VPath::parse("sftp://h/").expect("test wire"),
            pane: 0,
            trail: crate::app::Trail::Record,
        }
    }

    /// `F1` from a pane opens the PANE's page, not the index, and arrives
    /// with an empty history: `Esc` closes the overlay, it does not walk
    /// back to a place the reader did not ask for.
    #[test]
    fn f1_opens_the_contexts_page_with_no_history() {
        let mut app = app_with_help_closed();
        open_help(&mut app);
        let help = app.help.as_ref().expect("help opened");
        assert_eq!(
            help.state.current().as_str(),
            "panes",
            "the corpus claims `browse`"
        );
        assert!(!help.over_modal, "there was no modal at all");

        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Backspace);
        assert!(
            app.help.is_none(),
            "\"back\" at the root closes: the contextual page is NOT a navigation step"
        );
    }

    /// Review H3c MAJOR-2: over a modal with NO page written, `F1` opens
    /// NOTHING — it says so and leaves the question answerable.
    ///
    /// Falling back to the index is fine from a pane or from the viewer
    /// (nobody expects a decision), but over a dialog it covered a live
    /// question with "Welcome to norte — norte is an orthodox file manager.
    /// Two panes…," froze its verbs, replaced its footer and let the reader
    /// walk from the index to `copying` to read ANOTHER dialog's `y`/`n`
    /// prose while the approval waited behind it. It is the same decision
    /// [`palette_help`] had already made for an undocumented row, applied
    /// where it matters most.
    #[test]
    fn f1_over_a_modal_with_no_page_does_not_cover_the_question() {
        // The GUARD is tested, not the gap: since H3h every context has a
        // page (the documentation gate went allowlist-free), so a test that
        // needed an undocumented modal would be left with no subject and
        // would have to be rewritten with every new page. The context is
        // synthetic; what is pinned is that over a modal the answer to "no
        // page" is to open nothing.
        for lang in [Lang::En, Lang::Es] {
            assert!(
                refuses_over_modal(lang, "dialog.no-such-context", true),
                "over a modal, with no page, F1 opens nothing"
            );
            assert!(
                !refuses_over_modal(lang, "dialog.no-such-context", false),
                "from a pane the index IS a reasonable landing"
            );
            assert!(
                !refuses_over_modal(lang, "dialog.approval", true),
                "and with a page written, that page opens"
            );
        }
    }

    /// The other half, and what really changed in H3h: no modal the TUI
    /// knows how to open is left without a page. It is the same thing the
    /// documentation gate checks (`tests/help_gate.rs`), verified here from
    /// the reader's side — `F1` over a live question opens prose about THAT
    /// question, and never the message above.
    #[test]
    fn every_modal_context_has_a_page() {
        for lang in [Lang::En, Lang::Es] {
            for context in crate::help_context::CONTEXTS {
                assert!(
                    !refuses_over_modal(lang, context, true),
                    "[{lang:?}] context `{context}` has no page to open"
                );
            }
        }
    }

    /// And over a modal with a page, `F1` opens it without covering the
    /// question.
    #[test]
    fn f1_over_an_approval_opens_its_page() {
        let mut app = app_with_help_closed();
        app.modal = Some(test_approval_modal());
        open_help(&mut app);
        let help = app.help.as_ref().expect("help opened");
        assert_eq!(help.state.current().as_str(), "agents");
        assert!(help.over_modal, "help knows there is a question behind it");
        assert!(app.modal.is_some(), "and the question is still there");
    }

    /// `F1` over a modal opens THAT modal's page (or the index while nobody
    /// has written it) and leaves the modal where it was.
    #[test]
    fn f1_over_a_modal_opens_the_modals_page() {
        let mut app = app_with_help_closed();
        app.modal = Some(test_collision_modal());
        open_help(&mut app);
        let help = app.help.as_ref().expect("help opened");
        assert_eq!(
            help.state.current().as_str(),
            page_of("dialog.collision"),
            "the modal's context's page, never another modal's"
        );
        assert!(help.over_modal, "it opened ON TOP of a modal");
        assert!(app.modal.is_some(), "and the modal is still there");
        assert!(
            help_owns_keys(&app),
            "…with the keys: without this the modal branch would keep them and \
             the reader could not even move the cursor of the help that just opened"
        );
    }

    /// Help opened from a modal keeps the keys, and `Esc` closes ONLY help:
    /// the modal is not answered by accident.
    #[test]
    fn esc_closes_help_and_leaves_the_modal_intact() {
        let mut app = app_with_help_closed();
        app.modal = Some(test_trust_host_modal());
        open_help(&mut app);
        assert!(help_owns_keys(&app), "the help branch is the one that runs");
        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Esc);
        assert!(app.help.is_none(), "help closed");
        assert!(
            app.modal.is_some(),
            "an unknown host key is NOT answered by closing help"
        );
        assert!(
            !help_owns_keys(&app),
            "and closed, the next key goes back to the modal"
        );
    }

    /// While help covers the modal, the modal's verbs are inert: it is
    /// decided with help closed, looking at it.
    #[test]
    fn the_modals_verbs_are_not_reached_underneath_help() {
        let mut app = app_with_help_closed();
        app.modal = Some(test_trust_host_modal());
        open_help(&mut app);
        // The branch that runs is help's (`help_owns_keys`), so the
        // modal's — the only one that calls `dialog_action` — does not see
        // this key.
        assert!(help_owns_keys(&app));
        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Char('y')); // dialog.approve
        assert!(app.modal.is_some(), "nothing was trusted blindly");
        assert!(app.help.is_some(), "and `y` does not close help either");
    }

    /// A modal that ARRIVES over an open help closes it: the next key has
    /// to go where the pixels point (the modal is painted LAST, over
    /// everything), and an approval is not answered through a page.
    #[test]
    fn a_modal_that_arrives_closes_help() {
        let mut app = app_with_help_closed();
        open_help(&mut app); // no modal: over_modal == false
        assert!(!app.help.as_ref().expect("open").over_modal);
        app.modal = Some(test_approval_modal());
        assert!(
            !help_owns_keys(&app),
            "help that was already open does NOT keep the modal's key"
        );
        close_stale_overlays(&mut app);
        assert!(app.help.is_none(), "help yields the screen");
    }

    /// Review MINOR-1: `over_modal` is a fact about the PRESENT, not a
    /// memory.
    ///
    /// If the modal help was opened over disappears and ANOTHER arrives,
    /// the old flag would make help keep the keys and
    /// `close_stale_overlays` would never remove it: the new modal would be
    /// unanswerable until a page over a dialog that no longer exists was
    /// closed. `settle_help_over_modal` clears it as soon as there is no
    /// modal, so the second modal is treated as what it is — one that
    /// ARRIVES over an open help.
    #[test]
    fn over_modal_does_not_survive_the_modal_that_justified_it() {
        let mut app = app_with_help_closed();
        app.modal = Some(test_collision_modal());
        open_help(&mut app);
        assert!(app.help.as_ref().expect("open").over_modal);

        // The modal is answered; help stays open (Esc would close only
        // help, but the modal can leave through its own path: a retry).
        app.modal = None;
        settle_help_over_modal(&mut app);
        assert!(
            !app.help.as_ref().expect("still open").over_modal,
            "the flag does not survive what was a memory OF"
        );

        // …and now another modal arrives, which does NOT inherit help's
        // keys.
        app.modal = Some(test_approval_modal());
        assert!(
            !help_owns_keys(&app),
            "the new modal keeps the key: nobody asked for a page over IT"
        );
        close_stale_overlays(&mut app);
        assert!(app.help.is_none(), "and help expires like the rest");
    }

    /// And help that covers a modal does not DISPATCH either: `dispatch`
    /// plants its own modals, so running `pane.copy` from the page would
    /// replace the question waiting for an answer — it would vanish from
    /// the screen with nobody having answered it. It is said and the page
    /// stays.
    #[test]
    fn a_runnable_row_is_not_dispatched_over_a_modal() {
        let mut app = app_with_help_closed();
        app.modal = Some(test_trust_host_modal());
        open_help(&mut app);
        let mut r = dialog_resolver();
        // To a page with runnable rows (the context's may not have any
        // yet) and to the body, where Enter lives.
        app.help
            .as_mut()
            .expect("open")
            .state
            .open(&TopicId::new("copying"));
        press(&mut app, &mut r, KeyCode::Tab);
        assert_eq!(state(&app).focus(), Focus::Body);
        assert!(matches!(
            state(&app).action(),
            Some(norte_frontend::help::Action::Run(_))
        ));

        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(cmd, None, "nothing the run loop can dispatch");
        assert!(app.help.is_some(), "and help does not close on its own");
        assert!(app.modal.is_some(), "the question is still standing");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-help-modal-waiting").as_str()),
            "the Enter cannot vanish silently"
        );
    }

    /// And the BRIDGE to the palette does not cross over a modal either
    /// (review H3c MAJOR-1), for the same reason as the arm above and with
    /// a worse consequence.
    ///
    /// `Ctrl+P` used to close help and open the palette leaving the modal
    /// standing. The run loop's palette branch is gated by `!modal_wins`,
    /// so the palette ended up PAINTED looking alive but receiving not a
    /// single key: all of them fell into the modal branch and resolved
    /// against its allowlist. Typing `copy` to filter over this TOFU
    /// discards `c`, `o`, `p`… and the `y` TRUSTS the host key.
    #[test]
    fn the_bridge_to_the_palette_does_not_cross_over_a_modal() {
        let mut app = app_with_help_closed();
        app.modal = Some(test_trust_host_modal());
        open_help(&mut app);
        assert!(
            app.help.as_ref().expect("open").over_modal,
            "precondition: help opened ON TOP of the modal"
        );
        let mut r = dialog_resolver();

        let cmd = on_help_key(&mut app, &mut r, KeyModifiers::CONTROL, KeyCode::Char('p'));
        assert_eq!(cmd, None, "nothing to dispatch");
        assert!(
            app.palette.is_none(),
            "the palette does NOT open: its keys would be kept by the modal"
        );
        assert!(app.help.is_some(), "help stays where it was");
        assert!(
            app.modal.is_some(),
            "and the modal is still waiting for an answer"
        );
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-help-modal-waiting").as_str()),
            "the Ctrl+P cannot vanish silently"
        );
    }

    /// Help's own key has to ARRIVE with a modal open. `app.help` is a
    /// `[global]` command, not a `dialog.*` verb, so the modal's allowlist
    /// (`dialog_action`) drops it: without `modal_help_toggle`'s branch in
    /// `on_dialog_key`, `F1` over a dialog is INERT and this whole task
    /// cannot be used. (Caught piloting the TUI in tmux: the green suite
    /// did not see it because it opened help through `dispatch`.)
    #[test]
    fn f1_resolves_and_opens_help_with_a_modal_open() {
        let mut app = app_with_help_closed();
        app.modal = Some(test_collision_modal());
        let mut r = dialog_resolver();

        // The SAME path as the run loop: F1's chord resolved against the
        // `dialog` effective — the key is the keymap's (rebindable), the
        // meaning is this file's.
        let chord = chord_from_crossterm(KeyModifiers::NONE, KeyCode::F(1)).expect("F1 is a chord");
        let cmd = match r.push(chord) {
            Resolution::Run { command: cmd, .. } => cmd,
            other => panic!("F1 resolves to a command in the dialog context: {other:?}"),
        };
        assert_eq!(
            cmd, "app.help",
            "the orthodox preset binds F1 to `app.help`"
        );

        assert!(
            modal_help_toggle(&mut app, &cmd, Lang::En, &[]),
            "the key is CONSUMED: the modal does not see it as a decision"
        );
        let help = app.help.as_ref().expect("F1 opened help over the modal");
        assert_eq!(help.state.current().as_str(), page_of("dialog.collision"));
        assert!(help.over_modal);
        assert!(app.modal.is_some(), "and the modal is still standing");

        // And with help already open the SAME key closes it (the switch
        // lives in `on_help_key`), so this hook cannot reopen it: help's
        // branch wins the key before reaching here.
        assert!(help_owns_keys(&app));
    }

    /// Any other `dialog.*` command is untouched by the hook: what decides
    /// is still the modal's allowlist.
    #[test]
    fn the_help_hook_does_not_eat_the_modals_verbs() {
        let mut app = app_with_help_closed();
        app.modal = Some(test_approval_modal());
        assert!(!modal_help_toggle(
            &mut app,
            "dialog.approve",
            Lang::En,
            &[]
        ));
        assert!(app.help.is_none(), "nor does it open anything");
    }

    /// Review H3c MINOR-3: the SIX modals the run loop intercepts before
    /// `on_dialog_key` do not admit help on top, and now that is a DECISION
    /// (`help_context::help_over_modal_allowed`) instead of a hangover from
    /// key routing.
    ///
    /// Before, they were left out only because each one does `continue`
    /// 3000 lines further up; moving one to the `dialog` keymap — a
    /// plausible cleanup — would have silently opened the hole over a free
    /// text editor and over `init.lua`'s TOFU, which has NO TTL.
    #[test]
    fn the_intercepted_modals_do_not_admit_help_on_top() {
        let intercepted = [
            Modal::TrustLuaInit {
                path: "repo/.norte/init.lua".into(),
                hash_abbrev: "ab12cd34ef56ab78ab12cd34ef56ab78".into(),
            },
            Modal::MarkPattern {
                mark: true,
                pattern: "*.rs".into(),
                error: None,
            },
            Modal::Mkdir {
                name: "new".into(),
                error: None,
            },
            Modal::CommandLine {
                command: "make test".into(),
                error: None,
            },
            Modal::AiRenameInstruction {
                instruction: "in snake_case".into(),
                error: None,
            },
            Modal::SemanticQuery {
                query: "invoices".into(),
                error: None,
            },
            Modal::TransferName {
                kind: crate::app::TransferKind::Copy,
                from: VPath::parse("file:///x/a").expect("test wire"),
                to_dir: VPath::parse("file:///y").expect("test wire"),
                name: "a".into(),
                original: b"a".to_vec(),
                touched: false,
                from_marks: false,
                enc: None,
                error: None,
                space: None,
                confine: None,
            },
        ];
        for modal in intercepted {
            let label = format!("{modal:?}");
            let mut app = app_with_help_closed();
            app.modal = Some(modal);
            assert!(
                !modal_help_toggle(&mut app, "app.help", Lang::En, &[]),
                "{label}: the hook cannot CONSUME the key of a modal that \
                 does not admit help — what decides goes back to the allowlist"
            );
            assert!(
                app.help.is_none(),
                "{label}: F1 does not open a page over a free text editor \
                 nor over Lua's TOFU"
            );
            assert!(app.modal.is_some(), "{label}: and the modal is still there");
        }
    }

    /// …and the reverse direction is NOT true: help the reader opened FROM
    /// the modal survives the cleanup, or `F1` over a dialog would open a
    /// page the next key takes away.
    #[test]
    fn help_opened_from_the_modal_survives_the_cleanup() {
        let mut app = app_with_help_closed();
        app.modal = Some(test_trust_host_modal());
        open_help(&mut app);
        app.palette = Some(Palette::new(Vec::new()));
        close_stale_overlays(&mut app);
        assert!(app.palette.is_none(), "the palette does expire");
        assert!(
            app.help.is_some(),
            "the help the reader asked for over THIS modal stays"
        );
    }

    /// One unmodified key press.
    fn press(app: &mut App, resolver: &mut Resolver, code: KeyCode) -> Option<HelpDispatch> {
        on_help_key(app, resolver, KeyModifiers::NONE, code)
    }

    fn state(app: &App) -> &norte_frontend::help::HelpState {
        &app.help.as_ref().expect("overlay open").state
    }

    fn topic_ids(app: &App) -> Vec<String> {
        state(app)
            .rows()
            .iter()
            .filter_map(|r| match r {
                SidebarRow::Topic { id, .. } => Some(id.as_str().to_owned()),
                SidebarRow::Group { .. } => None,
            })
            .collect()
    }

    /// `/` opens the filter, the characters narrow the sidebar, and `Esc`
    /// leaves the box KEEPING what was typed — the model's contract, and the
    /// reason the filter is not a modal editor: leaving a search is not
    /// undoing it.
    #[test]
    fn the_filter_editor_types_narrows_and_keeps_its_text_on_esc() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        let all = topic_ids(&app);
        assert!(all.len() > 3, "the corpus brings several pages: {all:?}");

        press(&mut app, &mut r, KeyCode::Char('/'));
        assert!(state(&app).filtering(), "`/` opens the filter");

        for c in "copying".chars() {
            press(&mut app, &mut r, KeyCode::Char(c));
        }
        let filtered = topic_ids(&app);
        assert_eq!(
            filtered,
            vec!["copying".to_owned()],
            "the sidebar narrows to what was typed"
        );
        assert!(
            filtered.len() < all.len(),
            "the filter has to remove something or it filters nothing"
        );

        // And the keys are FIXED: `/` is one more character inside the box,
        // not the `dialog.filter` verb again.
        press(&mut app, &mut r, KeyCode::Char('/'));
        assert_eq!(state(&app).filter_raw(), "copying/");
        press(&mut app, &mut r, KeyCode::Backspace);
        assert_eq!(state(&app).filter_raw(), "copying");

        press(&mut app, &mut r, KeyCode::Esc);
        assert!(!state(&app).filtering(), "Esc leaves the box");
        assert_eq!(
            state(&app).filter_raw(),
            "copying",
            "…KEEPING the text: leaving a search is not undoing it"
        );
        assert!(
            app.help.is_some(),
            "and Esc in the box does NOT close the overlay"
        );
    }

    /// Enter on an `Action::Run` row returns the command the run loop must
    /// dispatch — the SAME id the palette would send — and leaves the
    /// overlay CLOSED: the command acts on the panes below.
    #[test]
    fn enter_on_a_runnable_row_hands_the_command_over_and_closes() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        app.help
            .as_mut()
            .expect("open")
            .state
            .open(&TopicId::new("copying"));
        press(&mut app, &mut r, KeyCode::Tab);
        assert_eq!(state(&app).focus(), Focus::Body, "Tab moves to the body");

        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(
            cmd,
            Some(HelpDispatch::Command(Command::PaneCopy)),
            "the first row of `copying` is `pane.copy`"
        );
        assert!(app.help.is_none(), "the overlay closes BEFORE dispatching");
    }

    /// Leaves help open on `acme.ftp`'s page, with a runnable row and focus
    /// already in the body: what a reader who arrived via `F1` from the
    /// extension manager sees.
    fn app_with_plugin_page(active: bool) -> (App, Resolver) {
        let mut app = app_with_help();
        let mut plugin = norte_proto::methods::PluginInfo {
            id: "acme.ftp".into(),
            name: "FTP".into(),
            publisher: "ACME".into(),
            version: "1.0.0".into(),
            category: "command".into(),
            capabilities: Vec::new(),
            approved: active,
            enabled: active,
            description: None,
            commands: vec![norte_proto::methods::PluginCommandInfo {
                id: "sync".into(),
                title: "Sync".into(),
                kind: norte_proto::methods::PluginCommandKind::Command,
            }],
            columns: Vec::new(),
            panels: Vec::new(),
            has_help: true,
            manifest_digest: None,
        };
        plugin.has_help = true;
        app.freeze_help_plugins(std::slice::from_ref(&plugin));
        let help = app.help.as_mut().expect("open");
        help.state.open(&TopicId::new("acme.ftp"));
        let parsed = norte_help::parse_untrusted(
            b"+++\nid = \"acme.ftp\"\ntitle = \"FTP\"\n\
              commands = [\"plugin:acme.ftp:sync\"]\n+++\nbody",
            "acme.ftp",
            None,
        );
        help.state.install_plugin_topic(parsed.topic);
        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Tab);
        assert_eq!(state(&app).focus(), Focus::Body, "Tab moves to the body");
        (app, r)
    }

    /// H3e: Enter on an ACTIVE plugin's row really dispatches it.
    ///
    /// It did not use to. Its key is `plugin:{id}:{cmd}`, which does not
    /// live in `COMMANDS` and which `Command::parse` rejects, so the Enter
    /// fell into the "this row is not runnable" arm — over a row the
    /// resolver itself had just painted as AVAILABLE, with the footer
    /// promising `⏎ run`. `verdict_with_plugins`'s dimming was decorative:
    /// the app refused whether the row was lit or dimmed.
    #[test]
    fn enter_on_an_active_plugins_row_dispatches_it() {
        let (mut app, mut r) = app_with_plugin_page(true);
        assert!(
            norte_help::ChordResolver::availability(&*app.help_chords, "plugin:acme.ftp:sync")
                .is_available(),
            "premise: the resolver paints it available"
        );
        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(
            cmd,
            Some(HelpDispatch::Plugin(
                "acme.ftp".to_owned(),
                "sync".to_owned()
            )),
            "the run loop receives which plugin and which command, already split"
        );
        assert!(
            app.help.is_none(),
            "and the overlay closes BEFORE dispatching, as with a built-in"
        );
    }

    /// And a DISABLED plugin's row is refused. The frozen snapshot
    /// authorizes nothing — `plugin.run_command` checks approved+enabled on
    /// its own on the server — but a dimmed row that ran when clicked would
    /// be worse than not dimming anything: the reader would learn that
    /// dimming means nothing.
    #[test]
    fn enter_on_a_disabled_plugins_row_is_refused() {
        let (mut app, mut r) = app_with_plugin_page(false);
        assert_eq!(
            norte_help::ChordResolver::availability(&*app.help_chords, "plugin:acme.ftp:sync")
                .reason(),
            Some(norte_help::Reason::PluginInactive),
            "premise: the resolver paints it dimmed"
        );
        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(cmd, None, "nothing is dispatched");
        assert!(app.help.is_some(), "and help stays open");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-help-not-runnable").as_str()),
            "eating the Enter silently would read as though the command ran"
        );
    }

    /// A topic's `commands` list can name a `dialog.*` verb (the `help`
    /// topic documents three): they are not dispatchable from a pane.
    /// Nothing is dispatched, the overlay STAYS open, and it says so —
    /// eating the Enter silently would read as though the command ran.
    #[test]
    fn enter_on_a_dialog_verb_row_dispatches_nothing_and_says_so() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        app.help
            .as_mut()
            .expect("open")
            .state
            .open(&TopicId::new("help"));
        press(&mut app, &mut r, KeyCode::Tab);
        // The `help` topic's `commands`: app.help, app.palette, dialog.filter…
        press(&mut app, &mut r, KeyCode::Down);
        press(&mut app, &mut r, KeyCode::Down);
        assert_eq!(
            state(&app).action(),
            Some(&norte_frontend::help::Action::Run("dialog.filter".into())),
            "the `help` topic's third row is an overlay verb"
        );

        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(cmd, None, "a `dialog.*` is not dispatched from a pane");
        assert!(app.help.is_some(), "and the overlay stays where it was");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-help-not-runnable").as_str()),
            "the Enter cannot vanish silently"
        );
    }

    /// Enter on a link FOLLOWS it and the overlay stays open (reading is
    /// not leaving); `dialog.back` returns to the page it came from.
    #[test]
    fn enter_on_a_link_follows_it_and_back_returns() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        assert_eq!(state(&app).current().as_str(), "index");
        // The index has no `commands`: all its actions are `see_also`.
        press(&mut app, &mut r, KeyCode::Tab);
        assert_eq!(state(&app).focus(), Focus::Body);
        let dest = match state(&app).action() {
            Some(norte_frontend::help::Action::Open(id)) => id.as_str().to_owned(),
            other => panic!("the index's first action is a link: {other:?}"),
        };

        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(cmd, None, "a link dispatches nothing");
        assert!(app.help.is_some(), "…and the overlay STAYS open");
        assert_eq!(state(&app).current().as_str(), dest);

        press(&mut app, &mut r, KeyCode::Backspace);
        assert!(app.help.is_some(), "going back does not close it either");
        assert_eq!(state(&app).current().as_str(), "index");
    }

    /// Enter in the sidebar OVER THE ALREADY-OPEN TOPIC moves into the
    /// body. Steering the sidebar previews, so that is the normal case and
    /// `open` would be a no-op: a silent Enter nobody can tell apart from a
    /// failure.
    #[test]
    fn enter_on_the_open_topic_moves_the_focus_into_the_body() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        assert_eq!(state(&app).focus(), Focus::Topics);
        assert_eq!(
            state(&app).selected_topic().map(TopicId::as_str),
            Some(state(&app).current().as_str()),
            "the sidebar's cursor rests on the open topic"
        );

        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(cmd, None, "entering the body dispatches nothing");
        assert!(app.help.is_some(), "…nor closes the overlay");
        assert_eq!(
            state(&app).focus(),
            Focus::Body,
            "Enter means \"go to what I am looking at\""
        );
    }

    /// The other branch: with the highlight on a topic DIFFERENT from the
    /// open one — what happens after following a `see_also` from a
    /// filtered list, where the highlight stays on the nearest visible row
    /// — Enter opens it.
    #[test]
    fn enter_on_a_topic_that_is_not_the_open_one_opens_it() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        // Filter to `copying` and follow its first link: the destination is
        // not in the filtered sidebar, so the highlight stays on `copying`.
        press(&mut app, &mut r, KeyCode::Char('/'));
        for c in "copying".chars() {
            press(&mut app, &mut r, KeyCode::Char(c));
        }
        press(&mut app, &mut r, KeyCode::Esc);
        assert_eq!(topic_ids(&app), vec!["copying".to_owned()]);
        press(&mut app, &mut r, KeyCode::Tab);
        assert_eq!(state(&app).focus(), Focus::Body);
        while !matches!(
            state(&app).action(),
            Some(norte_frontend::help::Action::Open(_))
        ) {
            press(&mut app, &mut r, KeyCode::Down);
        }
        press(&mut app, &mut r, KeyCode::Enter);
        let open = state(&app).current().as_str().to_owned();
        assert_ne!(open, "copying", "the link led to another page");
        assert_eq!(
            state(&app).selected_topic().map(TopicId::as_str),
            Some("copying"),
            "…and the highlight stayed where the filter left it"
        );

        // Enter in the sidebar opens what is highlighted, which is NOT
        // what is open.
        press(&mut app, &mut r, KeyCode::Tab);
        assert_eq!(state(&app).focus(), Focus::Topics);
        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(cmd, None);
        assert_eq!(
            state(&app).current().as_str(),
            "copying",
            "Enter opens the highlighted topic"
        );
    }

    /// `dialog.back` at the ROOT (with no history) closes the overlay. It is
    /// what turns `Backspace` into an honest key instead of a dead one.
    #[test]
    fn back_at_the_root_closes_the_overlay() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Backspace);
        assert!(
            app.help.is_none(),
            "with no history, \"back\" can only mean leaving"
        );
    }

    /// A `dialog.*` verb OUTSIDE `ALLOW_HELP` is INERT here, even if the
    /// keymap has it well bound: each overlay's semantics live in code. `y`
    /// is `dialog.approve` in the orthodox preset.
    #[test]
    fn a_verb_outside_the_allowlist_is_inert() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        let before = state(&app).current().clone();
        let cmd = press(&mut app, &mut r, KeyCode::Char('y'));
        assert_eq!(cmd, None);
        assert!(app.help.is_some(), "`dialog.approve` does not close help");
        assert_eq!(state(&app).current(), &before, "nor does it navigate");
    }

    /// The key that opens help closes it: F1 resolves to `app.help`, which
    /// is NOT in `ALLOW_HELP` (it belongs to `[global]`), and without its
    /// own branch it would be inert right inside the overlay it opens.
    #[test]
    fn the_key_that_opens_the_help_closes_it() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        let cmd = press(&mut app, &mut r, KeyCode::F(1));
        assert_eq!(cmd, None, "closing dispatches nothing");
        assert!(app.help.is_none(), "F1 inside help closes it");
    }

    /// …but not while typing in the filter: there the box consumes the
    /// key, as in the palette and the search dialog.
    #[test]
    fn the_filter_box_keeps_the_toggle_key() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Char('/'));
        assert!(state(&app).filtering());
        press(&mut app, &mut r, KeyCode::F(1));
        assert!(
            app.help.is_some(),
            "a function key inside the editor does not close the overlay"
        );
    }

    /// `ctrl+c` keeps its global quit and `ctrl+p` crosses to the palette
    /// TAKING the filter along — both are the same model at two speeds (the
    /// `help` topic says so), so there is no need to retype it.
    #[test]
    fn ctrl_c_quits_and_ctrl_p_hands_the_filter_to_the_palette() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        on_help_key(&mut app, &mut r, KeyModifiers::CONTROL, KeyCode::Char('c'));
        assert!(app.quit, "the emergency exit comes before anything else");

        let mut app = app_with_help();
        app.palette_rows = crate::palette::build_rows(&eff(Screen::Browse), &eff(Screen::Viewer));
        press(&mut app, &mut r, KeyCode::Char('/'));
        for c in "copy".chars() {
            press(&mut app, &mut r, KeyCode::Char(c));
        }
        on_help_key(&mut app, &mut r, KeyModifiers::CONTROL, KeyCode::Char('p'));
        assert!(app.help.is_none(), "help yields its spot");
        let palette = app.palette.as_ref().expect("the palette opened");
        assert!(
            !palette.visible().is_empty(),
            "the filter arrived and still matches something"
        );
        assert!(
            palette.visible().len() < palette.rows().len(),
            "…and it really filtered: {} of {}",
            palette.visible().len(),
            palette.rows().len()
        );
    }
}
