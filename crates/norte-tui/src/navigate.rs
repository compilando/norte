//! Changing directory, and the ritual that triggers.
//!
//! A `cd` is not an assignment: it lists the first page ([`first_page`]),
//! opens a paginated fill for the rest, caches capabilities if needed, moves
//! the navigation trail and leaves a [`Cd`] that says WHAT happened —
//! because the event loop has to reconcile its own pane-indexed state with
//! whatever just changed place ([`apply_cd`], [`reconcile_swap`]).
//!
//! That `Cd` is the reason this is not an `App::cd()`: the outcome is
//! consumed by things that live OUTSIDE the model (the fills, the probes,
//! the drainers), and putting it in `App` would force `App` to know about
//! them.
//!
//! It used to live in the `ntc` binary's root, a crate DISTINCT from this
//! lib, in four pieces separated by ten thousand lines. Its six test modules
//! stay for now in `main.rs`: they test the whole ritual, so they also name
//! the search tasks and the pane refresh, which have not come out yet.

use futures::StreamExt as _;
use norte_core::backend::{Backend, EntryStream};
use norte_frontend::busy::{Busy, BusyKind};
use norte_frontend::layout::{BySlot, SlotId};
use norte_proto::{Entry, Error, VPath};

use crate::app::{App, Modal, Trail, TypedSecret, error_message};
use crate::console::{Console, Waited};
use crate::fill::{Fill, release_refreshed_fill, spawn_fill};
use crate::jobs::SearchRun;
use crate::nav;
use crate::probes::{DecorateFetch, Probed};
use crate::trail::{rewind_for, rewind_trail};

/// Entries of the FIRST page a cd paints before filling in in the
/// background (ADR 0017): with this the first render does not wait for the
/// whole listing (spec §11: the first 100 in <16 ms even if the dir has
/// 500k).
pub const FIRST_PAGE: usize = 100;

/// The pane a `cd` outcome just SETTLED (`Filling`/`Replaced`, new listing
/// ALREADY in `app.panes[pane]`), or `None` if the cd touched no pane
/// (`Failed`/`Cancelled`). Does NOT consume `outcome` (borrow): the caller
/// still needs to pass it to [`apply_cd`] right after.
#[must_use]
pub fn cd_landed_pane(outcome: &Cd) -> Option<usize> {
    match outcome {
        Cd::Filling { pane, .. } | Cd::Replaced(pane) => Some(*pane),
        // A refresh re-lists IN PLACE (same dir, order already applied):
        // there is no landing to sort nor new decoration to request — parity
        // with `on_tick`'s path, which does not do it either. A `Swapped`
        // lists nothing either: both listings already existed, they only
        // changed sides (their decorations travel with them in
        // [`reconcile_swap`]).
        Cd::Refreshed(..) | Cd::Swapped | Cd::Failed(..) | Cd::Cancelled | Cd::Suspended => None,
        // The READER's: it is the one that sorts its listing, requests
        // decorations and drags the tree along. The mirror's settles on its
        // own in [`settle_cd`], which splits the two apart.
        Cd::Espejado { lector, .. } => cd_landed_pane(lector),
    }
}

/// A `cd`'s outcome, for the run loop to update the live fill.
pub enum Cd {
    /// The pane was replaced and the REST of its listing fills in the
    /// background. The pane index rides ALONGSIDE the [`Fill`] and not inside
    /// it: the run loop files the fill by pane, and the index is that filing
    /// key, not a property of the drainer.
    Filling {
        /// Pane whose listing is filling.
        pane: usize,
        /// The drainer, headed for `fill[pane]`.
        fill: Fill,
    },
    /// Pane `usize` was replaced and is already complete: an earlier fill
    /// for THAT pane is now stale and has to be released.
    Replaced(usize),
    /// Pane `usize`'s cd FAILED to list: the pane stayed where it was (the
    /// error already went out through the bar) over its PREVIOUS listing, so
    /// an earlier fill for that pane is STILL valid and is kept (#78:
    /// releasing it left the pane hanging with `loading=true` — with
    /// "(partial)" in the quick search — and no drainer to turn it off). The
    /// error TRAVELS for whoever navigates from the history popup (spec
    /// 2026-07-18: `NotFound` retires the entry). With no pane index: since
    /// the fill is no longer touched (#78), nobody consults it.
    Failed(Error),
    /// The cd was ABANDONED and nothing resumes it: `Esc` during the
    /// listing, `Ctrl-C`, or the event stream dying. Nothing changed and the
    /// fill keeps going.
    ///
    /// Different from [`Cd::Suspended`] on purpose: both leave the pane
    /// where it was, but only one of them is going to come back. Whoever
    /// walks the trail needs to know which, and probing `app.modal` to find
    /// out is guessing.
    Cancelled,
    /// The cd STOPPED halfway and something is going to resume THIS SAME
    /// navigation: the TOFU modal (`Modal::TrustHostKey`), which loads the
    /// pane and the trail mode so the retry continues where this one left
    /// off.
    ///
    /// For the fill and for the panes it is identical to [`Cd::Cancelled`]
    /// (the pane was not touched); the difference is read by `rewind_for`,
    /// which does NOT rewind a trail step the retry is going to finish.
    Suspended,
    /// #118: `pane.refresh` (Ctrl+R) re-listed these panes FROM `dispatch`
    /// (which does not see `fill`/`last_probed`): the outcome travels to the
    /// run loop so [`apply_cd`] applies the post-refresh ritual — the same
    /// `[bool; 2]` `refresh_panes` returns (`true` = complete listing
    /// settled).
    Refreshed([bool; 2]),
    /// `pane.swap` crossed the panes FROM `dispatch`, which does not see the
    /// pane-indexed state that lives in the run loop. The outcome travels so
    /// [`reconcile_swap`] crosses that half too — same pattern as
    /// `Refreshed`.
    Swapped,
    /// TWO landings from a single navigation: the one the reader requested
    /// and the one synced navigation (`pane.sync-nav`) repeated in the other
    /// panel.
    ///
    /// They travel together because a `cd` returns ONE outcome and the
    /// twelve sites that file it away have no reason to know about mirrors.
    /// Dropping the mirror's was not an option: its `Fill` IS that listing's
    /// drainer, and without filing it the other panel is left half-filled
    /// and with `loading` set forever (#78).
    Espejado {
        /// The panel's the reader moved.
        lector: Box<Cd>,
        /// The panel's that repeated it.
        espejo: Box<Cd>,
    },
}

/// Asks the backend for the decorations — icons, badges — and the plugin
/// columns of the listing a pane has NOW, with each entry's class (ADR
/// 0105), and leaves the response in flight in `decorate_fetch`.
///
/// Called by every `cd`'s outcome and by STARTUP: the two initial listings
/// used to be built with no request at all, so a freshly opened `ntc` had
/// not even one icon until the first `cd`, and the reader concluded the
/// plugin was not working.
pub fn request_decorations(
    app: &App,
    backend: &Backend,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    pane: usize,
) {
    let dir = app.panes[pane].dir().clone();
    let paths: Vec<VPath> = app.panes[pane]
        .entries()
        .iter()
        .map(|e| e.path.clone())
        .collect();
    let kinds: Vec<norte_proto::EntryKind> =
        app.panes[pane].entries().iter().map(|e| e.kind).collect();
    // The painted columns and the status bar's (ADR 0137): ONE list for
    // both frontends.
    let plugin_cols =
        norte_frontend::columns::plugin_requests(&app.columns, &app.status_plugins, dir.scheme());
    decorate_fetch.set(
        app.panes.slot_of(pane),
        crate::probes::spawn_decorate_fetch(
            backend,
            app.panes.slot_of(pane),
            dir,
            paths,
            kinds,
            plugin_cols,
        ),
    );
}

/// A `cd`'s COMPLETE outcome: the pane that landed is re-sorted by its
/// location's scheme, its plugin decorations are requested, and the result
/// is applied ([`apply_cd`]: paginated fill and probe).
///
/// It used to be copied across the event loop's NINE sites that trigger a
/// cd — the resolver, the palette, the menu, the mouse, the tree, the
/// sidebar, the connections selector, TOFU. What really tells them apart,
/// and is now read at the call site because it is the only thing left
/// there, is whether they also harvest the live search or launch the
/// external opener the command left pending.
pub fn settle_cd(
    app: &mut App,
    backend: &Backend,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
    outcome: Cd,
) {
    // A mirror is TWO navigations that landed: each settles fully — sort,
    // decorations, volumes — because both replaced a listing. Settling only
    // the reader's left the other panel with no icons and the previous
    // scheme's sort order.
    if let Cd::Espejado { lector, espejo } = outcome {
        settle_cd(
            app,
            backend,
            fill,
            decorate_fetch,
            last_probed,
            search_run,
            *lector,
        );
        settle_cd(
            app,
            backend,
            fill,
            decorate_fetch,
            last_probed,
            search_run,
            *espejo,
        );
        return;
    }
    if let Some(pane) = cd_landed_pane(&outcome) {
        app.apply_scheme_sort(pane);
        request_decorations(app, backend, decorate_fetch, pane);
        // The panel's footer states the free space of WHERE it is: a new
        // listing may be on a different volume.
        app.volumes_stale = true;
        // And the tree, if there is one: this listing is where the panel
        // now looks, and the panel next to it has to say the same thing.
        // Only for the FOCUSED one — a listing on the other side finishing
        // loading is not where the reader is working.
        if pane == app.focus() {
            app.follow_tree();
        }
    }
    apply_cd(
        &app.panes,
        fill,
        decorate_fetch,
        last_probed,
        search_run,
        outcome,
    );
}

/// Applies a cd's outcome to the paginated fills in progress: a new one
/// takes THAT PANE's slot (that pane's previous rx, dropped, kills its
/// drainer → releases the stream, rule 3); a REPLACEMENT of the SAME pane
/// releases it (its drainer would drain the old listing over the new one); a
/// FAILURE or an ABANDONED cd touch no pane — it stays on its previous
/// listing, whose fill is still valid — so they do not touch the fill (#78).
/// A `Refreshed` (#118) delegates to [`release_refreshed_fill`]: the same
/// ritual as `after_panes_refresh`.
///
/// The slot is PER PANE ([`Fill`]): a cd on one pane never strangles the
/// other's fill.
///
/// `search_run` travels all the way here ONLY for the `Swapped` arm
/// ([`reconcile_swap`]): it is also indexed by pane, and crossing it has to
/// happen before the `reap_search_run` these same call sites do right
/// after.
pub fn apply_cd(
    panes: &crate::panel::PaneSlots,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
    outcome: Cd,
) {
    match outcome {
        Cd::Filling { pane, fill: f } => {
            // New listing (lazy): probe #52's dedup expires — the same
            // entry, re-focused, must be able to re-hydrate.
            last_probed.clear();
            fill.insert(panes.slot_of(pane), f);
        }
        Cd::Replaced(pane) => {
            last_probed.clear();
            fill.remove(panes.slot_of(pane));
        }
        // The pane did not change: its fill (if there was one) keeps
        // draining the same listing. Releasing it here left it hanging with
        // `loading=true` (#78). `Suspended` (TOFU) goes here for the same
        // reason as `Cancelled`: the pane was not touched, and on top of it
        // the retry is going to re-list it whole.
        Cd::Failed(..) | Cd::Cancelled | Cd::Suspended => {}
        // #118: Ctrl+R from `dispatch` — same semantics as the other
        // triggers' ritual (`after_panes_refresh`), a single body.
        // `reap_search_run` is not needed here: `refresh_panes` SKIPS
        // virtual panes (never pulls them out of the mode), so there is no
        // search run to harvest through this path.
        Cd::Refreshed(refreshed) => release_refreshed_fill(panes, &refreshed, fill, last_probed),
        // `pane.swap`: `App::swap_panes` already crossed the panes and their
        // histories; here the half that lives in the run loop is crossed.
        Cd::Swapped => reconcile_swap(
            panes.slot_of(0),
            panes.slot_of(1),
            fill,
            decorate_fetch,
            last_probed,
            search_run,
        ),
        // Both, each against ITS OWN pane. The order does not matter: they
        // are different panes, and the fill slots are indexed by pane.
        Cd::Espejado { lector, espejo } => {
            apply_cd(
                panes,
                fill,
                decorate_fetch,
                last_probed,
                search_run,
                *lector,
            );
            apply_cd(
                panes,
                fill,
                decorate_fetch,
                last_probed,
                search_run,
                *espejo,
            );
        }
    }
}

/// The other half of `pane.swap`: the per-pane state that lives in the run
/// loop rather than in `App`.
///
/// `App::swap_panes` moves the panes and their histories; these four are
/// indexed by pane too, and leaving any of them behind is a bug a green suite
/// does not catch — the listing keeps arriving, just into the wrong half of
/// the screen, the decorations land on somebody else's rows, and the live
/// search's hits pour into the pane the reader is not looking at.
///
/// The watcher needs nothing here: the run loop re-points it from
/// `watch_targets(app)` at the top of EVERY iteration, so the swapped
/// directories reach it on the next tick.
///
/// That rests on two facts, and only one of them is pinned.
/// `swap_tests::watch_targets_sigue_a_los_panes_tras_el_intercambio` proves
/// `watch_targets` is a pure function of `app.panes` — nobody caches a target
/// per side, which is the half that could rot silently. The other half, that
/// the `rewatch` call really is the first statement of the loop body, is
/// ordering no unit test in this file can observe: if someone moved it below
/// the key handling, each pane would watch the other's directory for one
/// tick after a swap. Read the call site before trusting this comment.
pub fn reconcile_swap(
    slot_a: SlotId,
    slot_b: SlotId,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
) {
    // Swapping panels moves the CONTENT between slots and leaves the ids
    // where they were, so the work in flight has to travel with its
    // listing. Crossing the ids IN THE TREE instead of the content would
    // make this whole reconciliation unnecessary — noted in P6's plan.
    fill.swap(slot_a, slot_b);
    // The LIVE search stores its virtual pane the way the fill stored its
    // own, and this is where it HAS to flip: the same call site harvests
    // with `reap_search_run` right after `apply_cd`, and that harvest looks
    // at `panes[s.pane].virtual_search` — with the index not flipped it sees
    // the ordinary listing that just arrived from the other side and
    // silently cancels the Task, leaving the other pane with half-done hits
    // stuck in `Running` forever. There is at most ONE run (there is one
    // virtual pane), so flipping its index is the whole crossing it needs.
    if let Some(s) = search_run.as_mut() {
        s.pane ^= 1;
    }
    // Each slot carries its `dir` as an anti-stale guard, so crossing them
    // is enough: the fetch still corresponds to the listing that is now on
    // the other side.
    decorate_fetch.swap(slot_a, slot_b);
    // It is a `stat` dedup cache, not state: translating its keys costs more
    // than probing again, and one extra probe is invisible.
    last_probed.clear();
}

/// `dir`'s COMPLETE listing (for `refresh_panes` after a mutation: keeps
/// the cursor by index). An entry with an error cuts the listing short —
/// an honest error beats a silently incomplete listing. #54: does NOT sort
/// here — `refresh_listing`/`PaneState::refill` normalize internally, a
/// manual sort would be duplicated work. `attrs` (#117): the scheme's
/// configured attr ids — requested in the `fs.list`; an id not announced
/// comes back absent (a blank cell), never an error.
/// # Errors
///
/// Whatever the `Backend` returns: an entry with an error cuts the listing
/// short, because a silently incomplete listing is worse than an honest
/// error.
pub async fn listing(
    backend: &Backend,
    dir: &VPath,
    attrs: &[String],
) -> Result<(Vec<Entry>, Option<u64>), Error> {
    let out = backend.list_with_skipped_attrs(dir, attrs).await?;
    // This IS a screen (#301): the anchor for what the human just saw is
    // retained here and not in the backend's funnel, through which the
    // side tree and a script's `fs.list` also pass.
    backend.remember_listing_anchor(dir).await;
    Ok(out)
}

/// Caches in `App` the TWO halves of an `fs.capabilities` response (H3d):
/// the attrs catalogue (#117) and the capability flags.
///
/// One function and not two lines repeated at startup and in the `cd` on
/// purpose: the response carries both and storing them together is the
/// whole point of the change — whoever drops one half here pays another
/// network round trip for data that was already in the process, and that
/// oversight now has a test (`caps_cache_tests`) instead of living inside
/// the run loop's `select!`, where nothing looks at it.
pub fn cache_capabilities(
    app: &mut App,
    dir: &VPath,
    (caps, catalog): (norte_proto::Capabilities, norte_proto::AttrCatalog),
) {
    app.insert_attr_catalog(dir.scheme().to_owned(), catalog);
    app.insert_caps(dir, caps);
}

/// Whether a cd to `dir` still has to ask `fs.capabilities`.
///
/// The two halves of that response are cached with DIFFERENT keys and the gate
/// has to ask about both, which is the whole reason this is a named function
/// and not an `is_none()` inline in [`cd_in`]. The attribute catalogue is per
/// scheme — a wrong column hint is cosmetic. The capability flags are per
/// DIRECTORY (`App::caps`, #215), because a veto that answers for the wrong
/// place is not cosmetic: gating on the catalogue alone meant the first `sftp`
/// host visited answered "is this read-only?" for every other host of the
/// session, and keying by connection meant `/home` answered for the exFAT
/// stick mounted under the same `file://`.
///
/// So it fetches when EITHER half is missing, and the redundant fetch — a
/// second location of a scheme whose catalogue is already cached — is one call
/// per directory, which is what asking a location about itself costs.
#[must_use]
pub fn needs_capabilities(app: &App, dir: &VPath) -> bool {
    app.attr_catalog(dir.scheme()).is_none() || app.caps(dir).is_none()
}

/// What [`first_page`] brings: the entries, the stream with the rest, the
/// container's skipped ones, and the capabilities.
///
/// Named since the wait became shared: whoever lands the result receives it
/// as a parameter, and a four-tuple in a signature does not read.
pub type PrimeraPagina = (
    Vec<Entry>,
    Option<EntryStream>,
    Option<u64>,
    Option<(norte_proto::Capabilities, norte_proto::AttrCatalog)>,
);

/// `dir`'s first page (up to [`FIRST_PAGE`]) plus the stream with the REST
/// (or `None` if the dir fit in the first page) and the container's skipped
/// ones (#93). The first render does not wait for the whole listing (ADR
/// 0017). Rule 7: the TUI does not touch the FS. `attrs`/`fetch_caps`
/// (#117): requests the configured attrs and, when the caller says
/// something is missing from the cache ([`needs_capabilities`]),
/// `fs.capabilities`'s response (the tuple's fourth element).
///
/// H3d: that fourth element is `fs.capabilities`'s TWO halves —
/// `Capabilities` and the catalogue — because the wire brings them together
/// (`Backend::capabilities_and_attrs`). The TUI used to cache only the
/// catalogue and then ask "is it read-only?" with another round trip for
/// data that had already arrived.
/// # Errors
///
/// Whatever the `Backend` returns when requesting the first page or the
/// capabilities. Untranslated: whoever paints it needs the type.
pub async fn first_page(
    backend: &Backend,
    dir: &VPath,
    attrs: &[String],
    fetch_caps: bool,
) -> Result<PrimeraPagina, Error> {
    // Capabilities BEFORE the stream (same connection, and only when
    // something is missing from the cache — `needs_capabilities`); a
    // failure does NOT bring down the cd: with no hints it paints Opaque
    // and read-only falls back to the syntactic criterion.
    let catalog = if fetch_caps {
        backend.capabilities_and_attrs(dir).await.ok()
    } else {
        None
    };
    let (mut stream, skipped) = backend.list_stream_with(dir, attrs).await?;
    // A panel's cd is a screen: the anchor is retained (#301).
    backend.remember_listing_anchor(dir).await;
    let mut first = Vec::with_capacity(FIRST_PAGE);
    while first.len() < FIRST_PAGE {
        match stream.next().await {
            Some(item) => first.push(item?),
            // The dir fit in the first page: there is no rest to drain.
            None => return Ok((first, None, skipped, catalog)),
        }
    }
    Ok((first, Some(stream), skipped, catalog))
}

/// CANCELABLE cd (rule 3): the listing runs against the event stream — Esc
/// abandons it (the pane stays where it was) and Ctrl-C quits the TUI
/// (FIXED shortcuts during a cd: the keymap does not apply here — they are
/// the emergency exit and must not be remappable to something that does not
/// exist). Dropping the listing's future stops the provider's producer
/// (tested in vfs-local). The rest of the keys are discarded while the cd
/// lasts.
pub async fn cd(app: &mut App, backend: &Backend, events: &mut Console<'_>, dir: VPath) -> Cd {
    cd_in(app, backend, events, app.focus(), dir, Trail::Record).await
}

/// The ONE place that decides whether a navigation joins the pane's trail.
///
/// Two conditions, both load-bearing, both easy to lose in the middle of the
/// success arm of [`cd_in`] where they used to live:
///
/// - `prev != dir`: a cd onto the directory the pane is ALREADY showing (a
///   refresh-like navigation) is not a step the reader took. Recording it
///   would make the next `nav.back` do nothing visible. The MRU's consecutive
///   dedup covers the rest of the redundancies.
/// - `trail == Trail::Record`: a `Trail::Replay` is the trail walking ITSELF.
///   Recording there feeds the trail its own steps — going back from B to A
///   would log "I was at B", so the next `nav.back` returns to B and the
///   reader oscillates between two directories forever. This is the single
///   line that stops `nav.back` from doing that, and it is pinned by
///   `record_step_tests::un_replay_no_alimenta_el_rastro`.
///
/// The two conditions now live in [`norte_frontend::history::record_visit`],
/// shared with the window, which also counts the visit for the popular list
/// (spec 2026-09-15 D6): the same event, so the same decision.
pub fn record_step(
    h: &mut nav::History,
    popular: &mut norte_frontend::history::Popular,
    prev: &VPath,
    dir: &VPath,
    trail: Trail,
) {
    norte_frontend::history::record_visit(h, popular, prev, dir, trail);
}

/// Navigates `pane` — which need NOT be the focused one, because
/// `pane.mirror` sends the OTHER pane somewhere while focus stays put.
/// `trail` says whether the move is RECORDED in the pane's trail or is the
/// trail replaying itself ([`Trail`]).
///
/// Everything here that touches pane state goes through the `pane`
/// PARAMETER (`app.panes[pane]`), never through `app.focused()`: they are
/// the same thing only while the caller is the [`cd`] wrapper.
pub async fn cd_in(
    app: &mut App,
    backend: &Backend,
    events: &mut Console<'_>,
    pane: usize,
    dir: VPath,
    trail: Trail,
) -> Cd {
    // History (spec 2026-07-18): the PREVIOUS dir is captured HERE and
    // pushed only on the SUCCESS arm (the pane really got replaced). Living
    // inside `cd_in` covers EVERY path that navigates — nav.enter/nav.parent,
    // quick-Enter (dispatch nav.enter), the TOFU retry and the
    // history/hotlist popups — without repeating it per call site.
    let prev = app.panes[pane].dir().clone();
    // #117: the destination scheme's CONFIGURED attrs are requested in the
    // listing; the provider's catalogue is fetched ONCE per scheme and
    // session (cached in `App::attr_catalogs` — render hints and headers),
    // and the caps once per CONNECTION (see `needs_capabilities`).
    let attrs = app.columns.attr_ids_for(dir.scheme());
    let fetch_caps = needs_capabilities(app, &dir);
    // What is being waited for, so the surfaces can SAY it. A remote
    // destination is `Connecting` and a local one `Listing`: the verb for
    // first contact with a bucket is not "listing", and a reader seeing
    // "connecting…" knows the network is what can be slow, not their disk.
    // `Busy` is not painted until it crosses its threshold, so a local cd —
    // 99% of them — never gets to show anything (`norte_frontend::busy`).
    let started = std::time::Instant::now();
    app.busy = Some(Busy::new(
        if is_local(&dir) {
            BusyKind::Listing
        } else {
            BusyKind::Connecting
        },
        // The RAW VPath: the altered-name badge, the panel's encoding
        // reinterpretation and the available width are only known by
        // whoever paints, and rendering here lost all three at once.
        Some(dir.clone()),
        Some(pane),
    ));
    let out = match crate::console::wait_painting(
        events,
        app,
        started,
        first_page(backend, &dir, &attrs, fetch_caps),
    )
    .await
    {
        Waited::Done(res) => aterrizar(app, pane, dir, trail, &prev, res),
        Waited::Cancelled => Cd::Cancelled,
        Waited::Quit => {
            app.quit = true;
            Cd::Cancelled
        }
    };
    // The wait ended, however it ended: cancelled, failed or good. Leaving
    // the indicator set would be the spinner that never moves. And in case
    // some future path skipped this line, `turn::drain_pending` clears it
    // too in every turn's header: no wait survives one.
    app.busy = None;
    // SYNCED navigation (`pane.sync-nav`): the other panel repeats THIS
    // `cd`. It goes here, at the single point the eleven calls that
    // navigate all pass through — trail, popups, keys, semantic search —
    // and not in the dispatcher: hung there, walking the history did not
    // mirror and the mode half-lied.
    //
    // The echo travels as `Trail::Seed` and only fires if THIS `cd` was not
    // one itself: it is not a reader's step — it does not enter their
    // trail — and it is what cuts the recursion with no separate flag. And
    // it only mirrors what comes out of the FOCUSED panel: a listing that
    // settles on its own does not drag the other along.
    if app.sync_nav
        && !matches!(trail, Trail::Seed)
        && pane == app.focus()
        && cd_landed_pane(&out).is_some()
        && let Some(other) = app.target_index()
        && let Some(dest) = norte_frontend::nav::destino_en_espejo(
            app.panes[pane].dir(),
            app.panes[other].dir(),
            app.panes[other].virtual_search,
        )
    {
        // `Box::pin` because it is recursion in an `async fn`. A single
        // round: the echo comes in with `Seed` and the guard above stops
        // it.
        let mirror = Box::pin(cd_in(app, backend, events, other, dest, Trail::Seed)).await;
        return Cd::Espejado {
            lector: Box::new(out),
            espejo: Box::new(mirror),
        };
    }
    out
}

/// What is NOT local is something you have to CONNECT to.
///
/// The authority and not the scheme: `file://` with no authority is the
/// disk right here, and so is an archive opened over it (`tar+file://…`).
/// Saying "connecting…" when opening a local zip would be exactly the
/// dishonesty this indicator promises not to commit.
fn is_local(dir: &VPath) -> bool {
    dir.authority().is_none()
}

/// What to do with what arrived: replace the pane, open the TOFU modal, or
/// leave the error in the bar.
///
/// Split out of the `select!` because there is no `select!` anymore: the
/// wait is [`crate::console::wait_painting`], shared with the TUI's other
/// two long waits, and this is the only part that was specific to a
/// navigation.
fn aterrizar(
    app: &mut App,
    pane: usize,
    dir: VPath,
    trail: Trail,
    prev: &VPath,
    res: Result<PrimeraPagina, Error>,
) -> Cd {
    match res {
        Ok((first, stream, skipped, catalog)) => {
            // #117: the newly arrived catalogue is cached by scheme — the
            // following frames already paint with hints. H3d: and the SAME
            // response's caps, which is what answers "is this pane
            // read-only?" with no other round trip (`App::pane_read_only`).
            if let Some(both) = catalog {
                cache_capabilities(app, &dir, both);
            }
            // #54: we do NOT sort here — `begin_listing` -> `set_listing`
            // normalizes internally.
            let more = stream.is_some();
            app.panes[pane].begin_listing(dir.clone(), first, more, skipped);
            record_step(&mut app.history[pane], &mut app.popular, prev, &dir, trail);
            // If there is stream left, a drainer fills it in in the
            // background.
            match stream {
                Some(s) => Cd::Filling {
                    pane,
                    fill: spawn_fill(s),
                },
                None => Cd::Replaced(pane),
            }
        }
        // First TOFU contact (#45): instead of an error line with the
        // fingerprint, opens the trust modal — `y` trusts and RETRIES this
        // same navigation.
        Err(Error::HostKeyUnknown {
            host,
            port,
            algo,
            fingerprint,
        }) => {
            // The modal LOADS `pane` and `trail`: the retry has to resume
            // THIS navigation (this pane, this trail mode), not a new one
            // against whatever had focus back then.
            app.modal = Some(Modal::TrustHostKey {
                host,
                port,
                algo,
                fingerprint,
                dir,
                pane,
                trail,
            });
            // The pane was NOT touched (only the modal opened): like
            // `Cancelled`, it keeps a fill in flight for the previous
            // listing, which is still valid. But SUSPENDED and not
            // `Cancelled`: this navigation is going to CONTINUE in the
            // modal's retry, and whoever walks the trail has to tell it
            // apart from an abandoned cd, which does not come back.
            Cd::Suspended
        }
        // #325: the entry says `secret = "prompt"` and none of the three
        // sources has it. Same treatment as TOFU — and for the same
        // reasons: the modal loads `pane` and `trail`, and Enter retries
        // THIS navigation.
        Err(Error::SecretNeeded { conn, endpoint }) => {
            app.modal = Some(Modal::AskSecret {
                conn,
                endpoint,
                input: TypedSecret::default(),
                dir,
                pane,
                trail,
            });
            Cd::Suspended
        }
        // A listing error does NOT bring down the TUI: the pane stays, but
        // an earlier fill for THIS pane no longer applies. The error is
        // CARRIED in the outcome (history popup).
        Err(e) => {
            app.message = Some(error_message(&e));
            Cd::Failed(e)
        }
    }
}

/// Trusts the host key and RETRIES the navigation TOFU interrupted (#45).
/// `Some(cd)` = the outcome must go back to the caller RIGHT AWAY (the next
/// pending one was already handled here); `None` = trusting failed and the
/// message stayed in the bar — the caller continues its usual path.
///
/// Lives outside [`crate::mutations::on_dialog_key`] because the whole arm
/// (destructuring the modal + `trust_host_key` + the retry) does not fit in
/// that function's line budget.
pub async fn trust_host_retry(
    app: &mut App,
    backend: &Backend,
    events: &mut Console<'_>,
    modal: Modal,
) -> Option<Cd> {
    let Modal::TrustHostKey {
        host,
        port,
        algo,
        fingerprint,
        dir,
        pane,
        trail,
    } = modal
    else {
        // The caller only calls with this modal (`Modal::TrustHostKey` arm).
        return None;
    };
    match backend
        .trust_host_key(&host, port, &algo, &fingerprint)
        .await
    {
        Ok(()) => {
            // The engine re-verifies the fingerprint against the key the
            // host presents NOW (anti-TOCTOU, ADR 0015 D); if it still
            // fails, the retry will show it.
            //
            // `cd_in` (not `cd`): resumes the navigation TOFU interrupted —
            // its pane and its trail — which need not be the current
            // focus's.
            let outcome = cd_in(app, backend, events, pane, dir.clone(), trail).await;
            // And if it was a trail step, THIS is where it finishes:
            // `walk_trail` left it taken counting on this retry.
            settle_suspended_trail(app, pane, &dir, trail, &outcome);
            // Only open the next pending one if the retry did NOT leave a
            // modal (another HostKeyUnknown): never step on it.
            if app.modal.is_none() {
                app.open_next_pending();
            }
            Some(outcome)
        }
        Err(e) => {
            app.message = Some(error_message(&e));
            // Trusting FAILED: there is no retry, so the navigation TOFU
            // suspended dies here — for the trail it is identical to an
            // abandoned cd, and the step has to come back.
            settle_suspended_trail(app, pane, &dir, trail, &Cd::Cancelled);
            None
        }
    }
}

/// Hands over the typed secret and RETRIES the navigation `SecretNeeded`
/// interrupted (#325). [`trust_host_retry`]'s twin, with the same return
/// contract: `Some(cd)` = the outcome goes back to the caller RIGHT AWAY,
/// `None` = handing it over failed and the message stayed in the bar.
///
/// Lives here for the same reason as its twin: destructuring the modal, the
/// call and the retry do not fit in
/// [`crate::mutations::on_dialog_key`]'s line budget.
pub async fn provide_secret_retry(
    app: &mut App,
    backend: &Backend,
    events: &mut Console<'_>,
    modal: Modal,
) -> Option<Cd> {
    let Modal::AskSecret {
        conn,
        input,
        dir,
        pane,
        trail,
        ..
    } = modal
    else {
        // The caller only calls with this modal (`Modal::AskSecret` arm).
        return None;
    };
    // An empty field does not even get here: `dialog_action` leaves this
    // modal's confirm INERT while nothing has been typed, so the guard does
    // not need repeating — and repeating it would hide that the decision
    // lives there, alongside the rest of the dialogs' security semantics.
    match backend.provide_secret(&conn, input.expose()).await {
        Ok(()) => {
            // The secret is already in the core; from here it is identical
            // to TOFU. `cd_in` (not `cd`): resumes the navigation the error
            // interrupted — its pane and its trail.
            let outcome = cd_in(app, backend, events, pane, dir.clone(), trail).await;
            settle_suspended_trail(app, pane, &dir, trail, &outcome);
            // The retry can open ANOTHER modal (a TOFU over the same host,
            // or a `SecretNeeded` from another connection): never step on
            // it.
            if app.modal.is_none() {
                app.open_next_pending();
            }
            Some(outcome)
        }
        Err(e) => {
            app.message = Some(error_message(&e));
            // Handing it over FAILED: there is no retry, so the navigation
            // the error suspended dies here — for the trail it is identical
            // to an abandoned cd, and the step has to come back.
            settle_suspended_trail(app, pane, &dir, trail, &Cd::Cancelled);
            None
        }
    }
}

/// Enter over a semantic modal hit (M4-IA-2): cd to the hit's PARENT and
/// leaves the cursor over it by path (a mold of
/// [`crate::jobs::on_search_enter`]; if it landed on a page not yet
/// drained, the cursor stays at the top, v1). Returns the `Cd` for the
/// caller to apply (`apply_cd` + decorate); `Cd::Cancelled` = nothing to
/// navigate (defensively empty hits or a root hit with no parent).
pub async fn semantic_hit_cd(
    app: &mut App,
    backend: &Backend,
    events: &mut Console<'_>,
    hits: &[norte_proto::methods::SemanticHit],
    cursor: usize,
) -> Cd {
    let Some(hit) = hits.get(cursor).map(|h| h.path.clone()) else {
        return Cd::Cancelled;
    };
    let Some(parent) = hit.parent() else {
        return Cd::Cancelled;
    };
    let pane = app.focus();
    let outcome = cd(app, backend, events, parent).await;
    if let Some(i) = app.panes[pane].entries().iter().position(|e| e.path == hit) {
        app.panes[pane].set_cursor(i);
    }
    outcome
}

/// Settles the trail of a navigation the TOFU prompt SUSPENDED, once that
/// prompt has been answered.
///
/// [`crate::trail::walk_trail`] deliberately leaves a `Cd::Suspended` step taken: the retry
/// was going to finish it. But the retry is not guaranteed to happen — the
/// reader can deny the key, trusting it can fail, and the retry itself can
/// fail or be abandoned — and when it does not, the step is left standing for
/// a move that never occurred. That is the same lie [`rewind_for`] exists to
/// stop, on the one path where the navigation OUTLIVES the function that
/// started it, which is why nobody was there to undo it.
///
/// Runs the answer's outcome through the very same [`rewind_for`] the trail
/// walker runs. `trail.step()` of `None` (a `Trail::Record` navigation: a
/// plain cd that happened to meet an unknown host) means there is no step to
/// rewind, so this is a no-op — and a second `Suspended` (another unknown
/// key, or the same one asked again) is a no-op TOO: the modal is open again
/// carrying the same trail, so the step is still going to be settled by
/// whoever answers THAT one.
///
/// Rewinding here cannot double up with [`crate::trail::walk_trail`]: the walker saw
/// `Suspended` and did nothing, so this is the FIRST and only rewind of that
/// step.
pub fn settle_suspended_trail(app: &mut App, pane: usize, dir: &VPath, trail: Trail, outcome: &Cd) {
    if let Some(step) = trail.step() {
        rewind_trail(app, pane, step, dir, rewind_for(outcome));
    }
}
