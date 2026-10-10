//! Who eats the key when overlays are stacked.
//!
//! Two questions and nothing else: whether the modal wins over any other
//! overlay ([`modal_wins`]), and whether a help page OPEN OVER a modal has
//! taken the keyboard away from it ([`help_owns_keys`]). And the two writes
//! that keep that answer honest: [`settle_help_over_modal`], which clears
//! the memory at the start of every turn, and [`close_stale_overlays`],
//! which closes what a new modal has just made stale. The reads used to be
//! consulted by the binary's key chain, and also by the paste router
//! ([`crate::paste::route_paste`]) — which is what made it impossible to
//! pull paste out of `main.rs` without pulling this out first.

use norte_core::backend::Backend;
use norte_i18n::t;

use crate::app::{App, HelpView, Modal};

/// MINOR-4 (H1 close): a modal can arrive ASYNCHRONOUSLY (e.g.
/// `Modal::ApproveAgentOp`, via `ConnEvent` — an agent can ask for approval
/// at any moment) while the search box is open. Without this guard, the run
/// loop resolved the key against the box FIRST (`app.goto.is_some()` was
/// checked before `app.modal.is_some()`): an Enter pressed to answer the
/// modal actually ran the box's highlighted row IN SILENCE, and the security
/// modal kept waiting for an answer that never arrived through that key. The
/// modal ALWAYS wins: the run loop's box arm excludes this case from its
/// condition (it stops consuming the key) and the modal arm closes the box,
/// now stale, the moment it enters — the SAME key falls through to the modal
/// in the same iteration.
///
/// GENERALIZED to ALL overlays: the guard only used to hold for the box
/// and the settings overlay, but the modal is painted LAST — over ANY
/// overlay ([`crate::ui::draw`]) — while the run loop's key chain resolved
/// BEFORE it against the theme selector, the column picker, the extension
/// manager, the nav popup, the search dialog and the help. The pixels said
/// "answer the modal" and the key went somewhere else: into the search
/// dialog and the popup's name field it slipped in as TYPED text, and into
/// the extension manager as a `dialog.toggle-enabled`/`dialog.remove` over
/// the highlighted plugin — the same silent bug as MINOR-4, with a worse
/// outcome.
#[must_use]
pub fn modal_wins(app: &App) -> bool {
    app.modal.is_some()
}

/// Does the open help overlay own this key press? (H3c)
///
/// The ONE hole in [`modal_wins`], and it is shaped by which of the two
/// arrived first — the flag `HelpView::over_modal` is what remembers:
///
/// * help opened FROM a modal keeps the keys. Otherwise `F1` over a dialog
///   would open a page whose cursor keys all belong to the dialog underneath:
///   an overlay the reader asked for and cannot use.
/// * a modal that ARRIVED over an already-open help does NOT lose the key. The
///   help is the stale one there, and [`close_stale_overlays`] is what retires
///   it — same treatment the search box and the settings overlay already get.
///
/// While the help owns the keys the modal's own verbs are unreachable, which is
/// the point: nothing gets approved through a page covering it. The modal is
/// still painted on top ([`crate::ui::draw`] paints it last), so the
/// question is never HIDDEN — only unanswerable until the help closes, and its
/// TTL running out denies the agent.
///
/// The flag decides ONLY while a modal is live, and it cannot be stale by the
/// time it is read: [`settle_help_over_modal`] clears it at the top of every
/// turn of the run loop, so it always describes the modal that is on screen NOW
/// rather than one that has since been answered (review MINOR-1).
#[must_use]
pub fn help_owns_keys(app: &App) -> bool {
    match app.help.as_ref() {
        // Ownership expressed as the two cases rather than as one boolean: with
        // a modal live the flag decides; with none there is nobody to compete
        // with and the help owns the key anyway.
        Some(help) => {
            if app.modal.is_some() {
                help.over_modal
            } else {
                debug_assert!(
                    !help.over_modal,
                    "`over_modal` with `app.modal` empty: \
                     `settle_help_over_modal` did not run this turn"
                );
                true
            }
        }
        None => false,
    }
}

/// Makes `HelpView::over_modal` a fact about the PRESENT (review MINOR-1).
///
/// The flag is set once, when the overlay opens, and [`help_owns_keys`] and
/// [`close_stale_overlays`] both trust it later. That trust is only sound while
/// it still describes the modal on screen: if the modal an `over_modal` help was
/// opened over went away and a DIFFERENT one arrived, the help would keep the
/// keys and `close_stale_overlays` would never retire it — the new prompt
/// unanswerable until the reader closes a page about a dialog that no longer
/// exists.
///
/// Unreachable today (every async writer refuses to touch a live modal, and the
/// paths that close one are the paths that answer it), but the argument for that
/// spanned four functions. Clearing the flag whenever no modal is live makes it
/// one line: the memory cannot outlive what it is a memory OF, so `over_modal`
/// being `true` means a modal was there on the previous turn AND is there now.
///
/// Called at the top of the run loop, BEFORE the retained modals (the AI plan,
/// the semantic hits) are planted: one of those arriving must find the flag
/// already cleared, so it is treated as a modal arriving over an open help.
pub fn settle_help_over_modal(app: &mut App) {
    if app.modal.is_none()
        && let Some(help) = app.help.as_mut()
    {
        help.over_modal = false;
    }
}

/// Retires the overlays a modal has made obsolete (MINOR-4, H1 close; extended
/// to the help in H3c).
///
/// Called from the modal arm of the run loop's key chain — i.e. exactly when a
/// modal has the key and some overlay is still on screen. The search box and the
/// settings overlay are dropped because their rows EXPIRE (they were built
/// against a state the modal is about to change) and because that same key must
/// reach the modal instead of vanishing into a filter.
///
/// The help is dropped too, but only when it did not open over this modal:
/// `over_modal` help is the reader's deliberate "explain this dialog to me",
/// and it owns the keys ([`help_owns_keys`]), so this function is never even
/// reached while one is open. The guard states that, rather than relying on the
/// caller to.
///
/// The other overlays (theme selector, column picker, extensions, nav popup,
/// search dialog) yield the key but SURVIVE: their rows do not expire and the
/// reader gets them back intact after answering.
pub fn close_stale_overlays(app: &mut App) {
    app.goto = None;
    app.settings = None;
    // K3c: and with settings the shortcuts editor goes too, since it lives
    // ON TOP of it — leaving it orphaned over a closed overlay would make
    // `Esc` fall through to the panes instead of returning where the reader
    // was. Its rows also expire for the same reason the box's do: the
    // modal is about to change the state they were built against.
    app.shortcuts = None;
    if app.help.as_ref().is_some_and(|help| !help.over_modal) {
        app.help = None;
    }
}

/// up/down over the modals that have their own window — the AI plan's
/// scroll (M4-IA, audit MAJOR-3) and the semantic hits cursor (M4-IA-2).
/// They move the WINDOW or the CURSOR and NEVER confirm/cancel: the same
/// pair of commands as the pickers (`ALLOW_PICKER`); for `dialog_action`
/// up/down are OUTSIDE these modals' decision allowlist (returns `None`,
/// pinned in tests/modal.rs), so the routing lives here, the same way the
/// pickers' dispatch lives in their `on_*_key`. `true` = command CONSUMED.
/// `F1` (or whatever the keymap binds to `app.help`) OVER an open modal:
/// opens the help for THAT modal's context. `true` = command CONSUMED.
///
/// Lives here for the same reason as [`modal_scroll`]: `app.help` is a
/// `[global]` command, not a `dialog.*` verb, so the specific modal's
/// allowlist ([`crate::app::dialog_action`]) drops it — and without this
/// branch the one key the reader is guaranteed would be inert exactly where
/// it is needed most, in front of a question they do not understand. It is
/// the twin of the `on_help_key` toggle: the same key that opens the help
/// closes it, and what is hardcoded is the MEANING, never the key.
///
/// Which modals allow it is a DECISION, not routing hangover: it is said by
/// [`crate::help_context::help_over_modal_allowed`], exhaustive over `Modal`
/// and with no wildcard (review MINOR-3). The six FREE-TEXT editors
/// (`Mkdir`, `MarkPattern`, `CommandLine`, `AiRenameInstruction`,
/// `SemanticQuery`, `TransferName`) and Lua's TOFU answer `false`: today they
/// do not even reach here either — the run loop intercepts them earlier to
/// read RAW keys (decision 8 of the H1 plan: the keymap must not
/// reinterpret what is being typed) — and asking it HERE is what stops
/// moving one of them into the `dialog` keymap from silently opening the
/// hole. Their contexts exist in the vocabulary and their pages are reached
/// through the index.
pub fn modal_help_toggle(
    app: &mut App,
    cmd: &str,
    lang: norte_help::Lang,
    help_lines: &[ratatui::text::Line<'static>],
) -> bool {
    if cmd != "app.help" {
        return false;
    }
    // Do not consume the key when the help cannot open: the one who decides
    // is again the modal's allowlist (`dialog_action`), which drops
    // `app.help` — the key stays INERT, which is what is wanted.
    if app
        .modal
        .as_ref()
        .is_some_and(|m| !crate::help_context::help_over_modal_allowed(m))
    {
        return false;
    }
    // No plugin snapshot (H3e): this branch is SYNCHRONOUS — the modal's key
    // chain is — and requesting it costs a round trip to the daemon. The
    // degradation is exactly the one documented for `plugins: None`: help
    // opened over a dialog offers no extension rows. It is the surface where
    // it is missed least: the reader is answering a question, not exploring
    // the catalogue, and the whole group is one `Esc` and one F1 away.
    open_contextual_help(app, lang, help_lines, None);
    true
}

/// `dialog.pane-up/down` over a modal with its own window: moves the window
/// or the cursor and NEVER confirms nor cancels. `true` = command CONSUMED.
pub fn modal_scroll(app: &mut App, cmd: &str) -> bool {
    if !matches!(cmd, "dialog.up" | "dialog.down") {
        return false;
    }
    let down = cmd == "dialog.down";
    match app.modal {
        Some(Modal::AiRenamePlan { .. }) => {
            app.ai_plan_scroll(down);
            true
        }
        // Phase 8: the organize tree has the same window, and without
        // scroll a plan of more than ten lines could never be approved — the
        // gate requires having reached the end.
        Some(Modal::OrganizePlan { .. }) => {
            app.organize_plan_scroll(down);
            true
        }
        Some(Modal::SemanticHits { .. }) => {
            app.semantic_cursor(down);
            true
        }
        // #311: without this, a batch of forty files with the mismatch on
        // row twelve showed five "correct" and "… and 35 more", and there
        // was no key that reached the bad one.
        Some(Modal::Checksums { .. }) => {
            app.checksums_scroll(down);
            true
        }
        _ => false,
    }
}

/// `Command::AppHelp`: opens the overlay on the page about where the reader IS.
///
/// The context comes from [`crate::help_context::help_context`] (the TUI's
/// closed vocabulary, anchored on `Modal`) and the page from the CORPUS, so
/// moving an explanation between pages is an edit to prose.
///
/// A word on the overlays that are NOT in that vocabulary — the search box, the
/// settings overlay, the theme and column pickers, the extension manager, the
/// nav popup, the search dialog. `help_context` answers `browse` for all of
/// them, and that answer is UNREACHABLE: each of those arms sits ahead of this
/// dispatch in the run loop's key chain with its own fixed keys, so `F1` there
/// is inert and never gets here. The one exception proves it — the box's
/// `Enter` can dispatch `app.help`, and it clears `app.goto` BEFORE
/// dispatching, so by the time this runs the box is gone and `browse` (or
/// `viewer`) is the honest answer. Growing the vocabulary for those overlays
/// would be vocabulary for a state that cannot happen.
///
/// `lang` is the NEGOTIATED language (`NORTE_LANG` > `[ui] lang` >
/// environment — the value `main` handed to `norte_i18n::force`), never
/// `Lang::from_env()`: the corpus is per-locale and a page in another language
/// than the chrome around it is the same bug as a half-translated dialog.
/// `help_lines` is the body of the synthetic keyboard entry, snapshotted from
/// the VIGENTE keymap (rebuilt by the hot reload, which also closes an open
/// overlay so no snapshot survives a rebind).
///
/// OVER A MODAL, a context with no page opens NOTHING (review MAJOR-2). The
/// index is the least surprising landing from a pane or the viewer — nothing
/// there is waiting on a decision — but over a dialog it covers a live question
/// with "Welcome to norte — norte is an orthodox file manager. Two panes…",
/// freezes the dialog's verbs, replaces its footer, and lets the reader walk
/// from the index into another dialog's `y`/`n` prose while an agent approval
/// waits behind it. So the reader is TOLD and the prompt stays answerable —
/// the same decision [`goto_help`] already makes for an undocumented row,
/// applied where it matters more. The pages still missing are on the
/// documentation gate's shrinking allowlist, so this is temporary by
/// construction.
/// `plugins` is the catalogue as of the moment the reader pressed the key
/// (H3e), or `None` when it could not be asked for. It arrives as a PARAMETER
/// because this function is SYNC — its callers are async and its tests are not
/// — and it is taken ONCE, on the open path, never while painting. `None` and
/// an empty catalogue land in the same place: no plugin rows in the sidebar and
/// every `plugin:` command dimmed, which is the honest answer to "I could not
/// find out".
/// Whether `F1` must refuse to open here: over a modal, with no page for this
/// context.
///
/// Pulled out of [`open_contextual_help`] so the refusal can be tested for what
/// it IS rather than through whichever modal happens to be undocumented. Since
/// H3h no context is: the documentation gate has no allowlist left, so a new
/// context arrives with its page or fails the build. That makes this guard
/// unreachable through the UI today and worth keeping anyway — it is the
/// fail-safe for the one way a context could still lose its page, which is
/// somebody deleting the page.
#[must_use]
pub fn refuses_over_modal(lang: norte_help::Lang, context: &str, over_modal: bool) -> bool {
    over_modal && norte_help::topic_for_context(lang, context).is_none()
}

/// Opens the help on the page for the CONTEXT the reader is in.
///
/// The context is decided by [`crate::help_context::help_context`], not this
/// function: which page matches which screen is a corpus decision,
/// exhaustive and with no wildcard. If that context has no page and there is
/// a modal in front, it says so and opens nothing
/// ([`refuses_over_modal`]).
pub fn open_contextual_help(
    app: &mut App,
    lang: norte_help::Lang,
    help_lines: &[ratatui::text::Line<'static>],
    plugins: Option<&norte_proto::methods::PluginListResult>,
) {
    let context = crate::help_context::help_context(app);
    let over_modal = app.modal.is_some();
    if refuses_over_modal(lang, context, over_modal) {
        app.message = Some(t("msg-help-no-dialog-page"));
        return;
    }
    // H3d: the context facts are FROZEN here, before the first layout pass —
    // a verdict must not change under the reader's cursor mid-page
    // (`App::freeze_help_facts`).
    app.freeze_help_facts();
    app.help = Some(HelpView::new_at(
        lang,
        help_lines.to_vec(),
        context,
        over_modal,
    ));
    // H3e: the plugin state is FROZEN along with the rest of the facts, in
    // both halves at once (sidebar and resolver) —
    // `App::freeze_help_plugins`.
    //
    // ALWAYS, even with no catalogue: the resolver lives in `App` and
    // survives the overlay closing, so not freezing here would leave the
    // PREVIOUS help's snapshot standing. An empty catalogue is the honest
    // answer to "I could not find out" — no extension rows, and every
    // `plugin:` command dimmed — and fail-closed is the direction to err in.
    app.freeze_help_plugins(plugins.map_or(&[], |l| l.plugins.as_slice()));
}

/// Opens the help on a SPECIFIC corpus page, by its id.
///
/// The sibling of [`open_contextual_help`] for whoever already knows which
/// page they want — the bar's detached-session indicator, which is not a
/// screen the reader is in but a fact about this window — and the same path
/// as `F1` over a box row: [`HelpView::new_at_topic`], with the page as
/// the trail's ROOT so `Esc` closes it instead of walking back to an index
/// nobody asked for. Facts are frozen the same way as in the contextual one.
///
/// An id with no page in this language opens nothing: the corpus binds the
/// ids this crate uses, so getting here would mean a deleted page, and
/// opening the index instead would be answering a different question.
pub fn open_help_topic(
    app: &mut App,
    lang: norte_help::Lang,
    help_lines: &[ratatui::text::Line<'static>],
    topic: &str,
) {
    let Some(page) = norte_help::topic(lang, topic) else {
        return;
    };
    let over_modal = app.modal.is_some();
    app.freeze_help_facts();
    app.help = Some(HelpView::new_at_topic(
        lang,
        help_lines.to_vec(),
        &page.id,
        over_modal,
    ));
    app.freeze_help_plugins(&[]);
}

/// Fetches the page of the plugin node the reader just opened (H3e).
///
/// On demand and once: 64 KiB per plugin must not ride every `plugin.list`, and
/// a page already installed is never asked for again within one overlay. The
/// "once" is [`HelpView::claim_plugin_fetch`]'s job — `plugin_needs_fetch` is a
/// POLLING question and this runs on every turn of the run loop, so without the
/// claim a dead daemon would be re-asked at frame rate.
///
/// A failure is SILENT on purpose — an empty page with the plugin's name is a
/// better answer than an error toast over a help overlay, and a daemon N-1
/// without the handler lands here too (`plugin.help` is 0.34.0). The page stays
/// blank for the life of the overlay; closing and reopening the help is the
/// retry.
///
/// The overlay is re-borrowed AFTER the await: the reader may have closed it, or
/// moved to another page, while the answer was in flight.
pub async fn fetch_plugin_page(backend: &Backend, app: &mut App) {
    let Some(id) = app.help.as_mut().and_then(HelpView::claim_plugin_fetch) else {
        return;
    };
    let Ok(res) = backend.plugin_help(&id).await else {
        return;
    };
    let Some(help) = app.help.as_mut() else {
        return;
    };
    // The publisher comes from the SNAPSHOT, already masked and capped, never
    // from the page: a plugin does not get to say who published it.
    // `parse_untrusted` masks it again, which is harmless.
    let publisher = help.publisher_of(&id);
    // `fold_flags` is not optional: the text arrives already short and already
    // decoded, so this parse comes out clean and the badge — the whole
    // user-facing mitigation for a hostile `help.md` — would go dark.
    let parsed = norte_help::parse_untrusted(res.markdown.as_bytes(), &id, publisher)
        .fold_flags(res.truncated, res.lossy);
    help.state.install_plugin_topic(parsed.topic);
}

/// The page that documents the box row under the cursor, if one does (H3c).
///
/// The other direction of the bridge H3b built: from the help, `Ctrl+P` carries
/// the filter into the box; from the box, `F1` opens the page about the
/// highlighted row. Two views of one model at two densities, so crossing
/// between them should not cost the reader a re-type.
///
/// A plugin row has no page: [`norte_frontend::goto::help_topic`] says why.
fn goto_help_target(app: &App, lang: norte_help::Lang) -> Option<&'static norte_help::Topic> {
    let key = app.goto.as_ref()?.selected()?.key.clone();
    norte_frontend::goto::help_topic(&key, lang)
}

/// `F1` inside the box: open the page for the highlighted row, or say that no
/// page documents it (H3c). `true` when the page opened.
///
/// On success the box CLOSES — the help takes the screen and the next key
/// belongs to what the reader is looking at — and the page arrives as the root
/// of the trail ([`open_help_topic`]), so one `Esc` leaves it. The box's arm
/// of the key chain only runs when no modal is on screen (`modal_wins`), so
/// the help is never opened over one.
///
/// On failure the box STAYS and the status bar says so. Opening the index
/// instead would be worse than nothing: the reader asked about one command and
/// would land on a table of contents, with no way to tell whether their command
/// is in there somewhere or simply undocumented.
pub fn goto_help(
    app: &mut App,
    lang: norte_help::Lang,
    help_lines: &[ratatui::text::Line<'static>],
) -> bool {
    let Some(topic) = goto_help_target(app, lang) else {
        app.message = Some(t("msg-palette-no-help"));
        return false;
    };
    app.goto = None;
    open_help_topic(app, lang, help_lines, topic.id.as_str());
    true
}

/// Can a watch event fire a refresh NOW? (#106, review MAJOR-2): with any
/// overlay open or a quick search being typed, `refresh_panes` would consume
/// the user's keys (its cancellation loop discards everything that is not
/// Esc/Ctrl-C) and Esc would come to mean "abandon the refresh" — never step
/// on the interaction in progress. The event stays queued (capacity 1) and
/// fires once things clear.
#[must_use]
pub fn watch_refresh_allowed(app: &App) -> bool {
    app.modal.is_none()
        && app.goto.is_none()
        && app.settings.is_none()
        && app.help.is_none()
        && app.viewer.is_none()
        && app.theme_picker.is_none()
        && app.columns_picker.is_none()
        && app.extensions.is_none()
        && app.nav_popup.is_none()
        && app.search_dialog.is_none()
        && app.panes.iter().all(|p| p.quick().is_none())
}

#[cfg(test)]
mod palette_modal_guard_tests {
    use super::*;
    use crate::app::{App, Modal, Pane, Settings};
    use crate::nav;
    use norte_frontend::goto::Goto;
    use norte_proto::VPath;

    fn app() -> App {
        let d = VPath::parse("file:///x").expect("test wire");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    fn approval_modal() -> Modal {
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

    /// MINOR-4 (H1 close): with ONLY the box open, there is nothing to
    /// precede — the guard does not fire. With BOTH open (a modal arrived
    /// asynchronously over the box), the modal must win.
    #[test]
    fn modal_preempts_palette_only_when_both_are_open() {
        let mut a = app();
        assert!(!modal_wins(&a), "with no modal, nobody precedes anybody");
        a.goto = Some(Goto::new(Vec::new()));
        assert!(
            !modal_wins(&a),
            "only the box open: the box handles its keys normally"
        );
        a.modal = Some(approval_modal());
        assert!(
            modal_wins(&a),
            "a modal in flight with the box open MUST beat it"
        );
    }

    /// A modal that takes the keys closes the box: its chords and its
    /// "cannot run here" were computed against a state the modal changes.
    #[test]
    fn a_modal_closes_the_box() {
        let mut a = app();
        crate::goto::open(&mut a, &[], &[], ">");
        close_stale_overlays(&mut a);
        assert!(a.goto.is_none());
    }

    /// The guard holds for ANY overlay, not just box/settings: the modal
    /// is painted last (over everything), so the key the user aims at what
    /// they SEE has to reach it. Before, the theme selector, the column
    /// picker, the extension manager, the nav popup, the search dialog and
    /// the help resolved FIRST and ate the answer meant for the modal (in
    /// the two with a text field, as typed text; in extensions, as a
    /// toggle/delete of the highlighted plugin).
    #[test]
    fn the_modal_beats_every_overlay() {
        let mut a = app();
        a.theme_picker = Some(crate::app::ThemePicker {
            names: Vec::new(),
            cursor: 0,
            original: a.theme.clone(),
        });
        a.extensions = Some(crate::app::ExtensionManager {
            plugins: Vec::new(),
            errors: Vec::new(),
            cursor: 0,
            config: None,
            focus: crate::app::ExtFocus::List,
        });
        a.help = Some(crate::app::HelpView::new(norte_i18n::Lang::En, Vec::new()));
        assert!(
            !modal_wins(&a),
            "with no modal, each overlay rules its own key"
        );
        a.modal = Some(approval_modal());
        assert!(
            modal_wins(&a),
            "with overlays open, the modal still wins the key"
        );
    }

    /// #106 (review MAJOR-2): a watch event NEVER refreshes with an overlay
    /// open or a quick search being typed — `refresh_panes` would eat the
    /// keys and Esc would change meaning. The event stays queued and fires
    /// once things clear.
    #[test]
    fn watch_refresh_gated_by_overlays() {
        let mut a = app();
        assert!(watch_refresh_allowed(&a), "no overlays: allowed");
        a.modal = Some(approval_modal());
        assert!(!watch_refresh_allowed(&a), "modal open: queued");
        a.modal = None;
        a.help = Some(crate::app::HelpView::new(norte_i18n::Lang::En, Vec::new()));
        assert!(!watch_refresh_allowed(&a), "help open: queued");
        a.help = None;
        a.panes[0].quick_start(nav::Mode::Filter);
        assert!(
            !watch_refresh_allowed(&a),
            "quick search being typed: queued"
        );
    }

    /// S3: the same case for `app.settings` — a modal in flight (e.g. a
    /// policy approval) wins over the open settings overlay.
    #[test]
    fn modal_preempts_settings_only_when_both_are_open() {
        let mut a = app();
        assert!(!modal_wins(&a));
        a.settings = Some(Settings::new(Vec::new()));
        assert!(
            !modal_wins(&a),
            "only the settings overlay open: it handles its keys normally"
        );
        a.modal = Some(approval_modal());
        assert!(
            modal_wins(&a),
            "a modal in flight with settings open MUST beat it"
        );
    }
}

#[cfg(test)]
mod goto_help_tests {
    use super::*;
    use crate::app::{App, Pane};
    use norte_frontend::goto::{FixedSource, Goto, GotoRow, SECTION_COMMANDS};
    use norte_proto::VPath;

    fn app() -> App {
        let d = VPath::parse("file:///x").expect("test wire");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    /// The box open in commands mode with ONE row, `key`'s, under the
    /// cursor. The row is built by hand and not from the live keymap on
    /// purpose: what is being tested is what `F1` does with the highlighted
    /// row's dispatch key, and a plugin row does not come from `COMMANDS`.
    fn app_with_box_on(key: &str) -> App {
        let mut app = app();
        let mut g = Goto::new(vec![Box::new(FixedSource::new(
            SECTION_COMMANDS,
            vec![GotoRow {
                section: SECTION_COMMANDS.id,
                key: format!("cmd:{key}"),
                text: "test description".to_owned(),
                desc: key.to_owned(),
                ..GotoRow::default()
            }],
        ))]);
        g.set_query(">");
        app.goto = Some(g);
        app
    }

    /// `F1` over a command row opens the page that documents that command:
    /// the two are views of the same model at two densities, so crossing
    /// from the quick one to the explaining one should not cost a re-type.
    #[test]
    fn f1_in_the_box_opens_the_page_for_the_command_under_the_cursor() {
        let app = app_with_box_on("pane.copy");
        let open = goto_help_target(&app, norte_help::Lang::En).expect("pane.copy has a page");
        assert_eq!(open.id.as_str(), "copying");
    }

    /// …and it really opens it: the box CLOSES (the next key belongs to
    /// the help, which is what is seen) and the page arrives as the trail's
    /// ROOT — `Esc` closes the overlay instead of walking to an index the
    /// reader did not ask for, same as a modal's contextual help.
    #[test]
    fn opening_the_page_closes_the_box_and_arrives_with_no_history() {
        let mut app = app_with_box_on("pane.copy");
        assert!(goto_help(&mut app, norte_help::Lang::En, &[]));
        assert!(app.goto.is_none(), "the box closes");
        let help = app.help.as_mut().expect("the help opened");
        assert_eq!(help.state.current().as_str(), "copying");
        assert!(
            !help.over_modal,
            "the box's branch only runs with no modal on screen"
        );
        assert!(!help.state.back(), "no history: Esc closes");
        assert!(app.message.is_none(), "and nothing to apologize for");
    }

    /// A row WITHOUT a page opens nothing and says so: better than opening
    /// the index and leaving the reader hunting for what it had to do with
    /// what they asked for.
    #[test]
    fn a_row_with_no_page_says_so() {
        // A SYNTHETIC id, not a real allowlist command: since H3h none is
        // left without a page, so a test relying on that gap would be
        // measuring the corpus, not the branch. This branch still exists —
        // `topic_for_command` can answer `None` — and what gets painted then
        // is what needs pinning down.
        let mut app = app_with_box_on("app.no-such-command");
        assert!(goto_help_target(&app, norte_help::Lang::En).is_none());
        assert!(!goto_help(&mut app, norte_help::Lang::En, &[]));
        assert!(
            app.help.is_none(),
            "the index is not opened as a consolation"
        );
        assert!(app.goto.is_some(), "and the box stays where it was");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-palette-no-help").as_str())
        );
    }

    /// A PLUGIN row's `key` is `plugin:{id}:{command}` (P1): no corpus topic
    /// documents it and it is not a host command. It takes the "no page"
    /// path — no panic, no unrelated page, no `Command::parse` that is not
    /// its to make.
    #[test]
    fn a_plugin_row_takes_the_no_page_path() {
        let mut app = app_with_box_on("plugin:dev.norte.demo:greet");
        assert!(goto_help_target(&app, norte_help::Lang::En).is_none());
        assert!(!goto_help(&mut app, norte_help::Lang::En, &[]));
        assert!(app.help.is_none());
        assert!(app.goto.is_some());
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-palette-no-help").as_str())
        );
    }

    /// With no row visible (a filter matching nothing) there is no command
    /// to document: same path, with no `unwrap` involved.
    #[test]
    fn with_no_row_visible_there_is_no_page() {
        let mut app = app_with_box_on("pane.copy");
        app.goto.as_mut().expect("open").push_str("zzzz");
        assert!(app.goto.as_ref().expect("open").selected().is_none());
        assert!(goto_help_target(&app, norte_help::Lang::En).is_none());
        assert!(!goto_help(&mut app, norte_help::Lang::En, &[]));
        assert!(app.help.is_none());
        assert!(app.goto.is_some());
    }
}
