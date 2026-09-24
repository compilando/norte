//! Comparing and syncing as seen from `App`: resolving WHAT gets compared
//! and what gets synced, closing both panels, and the size probe that
//! hydrates the diff panel's selected row (#157).

use super::{App, CompareView};
/// A sync's two roots and how their names get read.
///
/// Lives in [`norte_frontend::sync`] since #161, together with the function
/// that decides them: the GUI came to have the SAME rule written by hand
/// (its "the focused pane is the source" arm), and two copies of "which tree
/// gets overwritten" is the kind of drift that produces a perfectly
/// plausible plan against the wrong tree.
use norte_frontend::sync::SyncRoots;
use norte_i18n::t;
use norte_proto::{EntryKind, VPath};

impl App {
    /// `Shift+F2`: resolves WHAT to compare and leaves it pending for the
    /// run loop.
    ///
    /// The left one is the FOCUSED pane (spec: "the panel that launched the
    /// comparison is the left one"), the right one is the other — not
    /// `panes[0]` and `panes[1]`, because a reader who presses the key from
    /// the right pane expects their directory to be theirs.
    ///
    /// Two refusals happen HERE, without a round trip to the daemon:
    ///
    /// * **Both panes in the same place.** The daemon answers `-32602` to
    ///   that (C6) and it's right, but the phrase the reader needs doesn't
    ///   depend on a trip over the network.
    /// * **A virtual pane.** A list of hits isn't a directory, so there's no
    ///   root to send — the same refusal mirror and pull already give.
    pub fn request_compare(&mut self) {
        // The viewer replaces the panes on screen and `ui::draw` gives it
        // precedence over this panel, so opening it behind would leave the
        // pixels saying one thing and the keyboard going to another — the
        // exact bug `modal_wins`'s rustdoc is written against.
        if self.viewer.is_some() {
            return;
        }
        if self.panes[0].virtual_search || self.panes[1].virtual_search {
            self.message = Some(t("msg-pane-not-a-location"));
            return;
        }
        let left = self.focused().dir().clone();
        let right = self.panes[self.focus() ^ 1].dir().clone();
        if left == right {
            self.message = Some(t("compare-same-path"));
            return;
        }
        self.pending_compare = Some(norte_proto::methods::FsCompareParams {
            left,
            right,
            criteria: norte_proto::methods::CompareCriteria::default(),
            max_depth: None,
            // Lives with the model (#158), not here: the GUI requests the
            // SAME comparison, and two copies that drifted apart would give
            // different verdicts for the same two directories.
            mtime_tolerance_ms: norte_frontend::compare::MTIME_TOLERANCE_MS,
            // No toggle in the UI, on purpose: `Backend::compare` answers
            // `Unsupported` for `true` before any Task exists, because the
            // engine accepts the field and ignores it. Offering the checkbox
            // would be offering a promise nobody keeps.
            follow_symlinks: false,
            // No toggle either: the diff pane shows an orphan as ONE row,
            // and descending it is what a sync plan requests on its own
            // (spec 2).
            descend_orphans: None,
        });
    }

    /// A sync's two roots, in `(source, dest)` order.
    ///
    /// With the diff panel open they're decided by its ACTIVE side, which is
    /// what `Tab` changes: nothing is inferred from focus or from the panes'
    /// order, because a sync's direction is half of what has to be approved.
    /// With no panel open they're the focused pane and the other one, the
    /// same split as [`Self::request_compare`].
    ///
    /// The WHOLE decision is [`norte_frontend::sync::sync_roots`] (#161),
    /// not a local copy of its two arms: the GUI needs exactly the same one
    /// — active-side arm included, which is the one that arrives with its
    /// panel — and here it's only given what this TUI knows.
    #[must_use]
    fn sync_roots(&self) -> SyncRoots {
        let other = &self.panes[self.target_index().unwrap_or_else(|| self.focus())];
        norte_frontend::sync::sync_roots(
            self.sync_source_view(),
            &norte_frontend::sync::Panes {
                focused_root: self.focused().dir(),
                focused_encoding: self.focused().name_encoding(),
                other_root: other.dir(),
                other_encoding: other.name_encoding(),
            },
        )
    }

    /// The diff panel the selection comes from, if there is one.
    fn sync_source_view(&self) -> Option<&CompareView> {
        self.compare.as_ref()
    }

    /// `sync.plan`: resolves WHAT to sync and leaves it pending for the run
    /// loop, or says why not.
    ///
    /// Returns the params it left pending, so a test can read the decision
    /// without a run loop.
    ///
    /// The refusals that happen HERE, without a round trip to the daemon:
    ///
    /// * **No journal.** Said by
    ///   [`norte_core::backend::Backend::is_journalled`], which is `false`
    ///   in embedded: since #167 that engine does carry the state
    ///   directory's journal, but doesn't install a spool, and with no spool
    ///   `sync.plan` refuses outright (hard rule 4). Planning against it
    ///   would show a plan nobody can approve. The same truth the reference
    ///   sheet's [`norte_frontend::availability::Facts::journalled`] already
    ///   dims; this is what happens if the reader gets there anyway.
    /// * **A virtual pane**, and **both roots in the same place**: identical
    ///   to comparison's, for the same reasons.
    /// * **More marks than [`SYNC_MAX_INCLUDE`]**. `Backend::sync_plan`
    ///   rejects it with `InvalidPath`, which doesn't say how many are too
    ///   many.
    ///
    /// [`SYNC_MAX_INCLUDE`]: norte_proto::methods::SYNC_MAX_INCLUDE
    pub fn request_sync(
        &mut self,
        mode: norte_proto::methods::SyncMode,
    ) -> Option<&norte_proto::methods::SyncPlanParams> {
        if self.viewer.is_some() {
            return None;
        }
        if !self.backend_journalled {
            self.message = Some(t("msg-sync-needs-daemon"));
            return None;
        }
        if self.compare.is_none() && (self.panes[0].virtual_search || self.panes[1].virtual_search)
        {
            self.message = Some(t("msg-pane-not-a-location"));
            return None;
        }
        let SyncRoots {
            source,
            dest,
            source_encoding,
            dest_encoding,
        } = self.sync_roots();
        if source == dest {
            self.message = Some(t("compare-same-path"));
            return None;
        }
        let include = match self.sync_include(&source, &dest) {
            Ok(include) => include,
            Err(e) => {
                self.message = Some(norte_frontend::sync::include_error_message(
                    &e,
                    norte_i18n::active(),
                ));
                return None;
            }
        };
        self.pending_sync = Some(norte_proto::methods::SyncPlanParams {
            source,
            dest,
            mode,
            // The criteria are comparison's and the wire's default already
            // carries them. `follow_symlinks` and `descend_orphans` stay at
            // their default on purpose: `Backend::sync_plan` answers
            // `Unsupported` for both, because the second one isn't the
            // caller's — the planner fixes it alongside the source — and
            // nobody satisfies the first.
            compare: norte_proto::methods::SyncCompareOptions::default(),
            on_unknown: norte_proto::methods::OnUnknown::default(),
            include,
        });
        // The SOURCE's reinterpretation gets frozen here, with the roots,
        // and travels to the panel: a reader who had pressed `Alt+E` to read
        // a CP1251 share can't get `????.txt` back when syncing it (#57, the
        // same bug the diff panel fixed in its review).
        self.pending_sync_encoding = (source_encoding, dest_encoding);
        self.pending_sync.as_ref()
    }

    /// The `include` list built from the diff panel's marks, or the reason
    /// there isn't one.
    ///
    /// WHAT counts as a refusal — and which root each mark is measured
    /// against — is decided by [`norte_frontend::sync::include_from_rows`],
    /// which lives next to `anchor_of` because it answers the same question
    /// from the other end of the trip.
    ///
    /// # Errors
    /// Whatever that one returns; [`Self::request_sync`] translates it into
    /// a phrase.
    fn sync_include(
        &self,
        source: &VPath,
        dest: &VPath,
    ) -> Result<Option<Vec<norte_proto::methods::RelPath>>, norte_frontend::sync::IncludeError>
    {
        let marked = self
            .compare
            .as_ref()
            .map(|v| v.pane.marked_rows())
            .unwrap_or_default();
        norte_frontend::sync::include_from_rows(source, dest, &marked)
    }

    /// Closes the sync panel. Cancelling the Task belongs to the run loop
    /// (it's its own); this only drops the presentation state.
    pub fn close_sync(&mut self) {
        self.sync = None;
        self.pending_sync_apply = None;
    }

    /// The pane the diff panel's ACTIVE side belongs to.
    ///
    /// `None` with the panel closed. It's what makes a row's `Enter` take
    /// the reader to where that row REALLY lives without costing them the
    /// other directory.
    #[must_use]
    pub fn compare_active_pane(&self) -> Option<usize> {
        let view = self.compare.as_ref()?;
        Some(match view.pane.active_side() {
            norte_proto::methods::Side::Right => view.left_pane ^ 1,
            _ => view.left_pane,
        })
    }

    /// Closes the diff panel. Cancelling the Task belongs to the run loop
    /// (it's its own); this only drops the presentation state.
    pub fn close_compare(&mut self) {
        self.compare = None;
    }

    /// Paths of the diff panel's SELECTED row worth probing with a `stat`
    /// (#157): a side with an entry, of kind `File` (directories and links
    /// don't have a size an ordinary `stat` resolves — same criterion as
    /// [`Self::focused_needs_stat`]), without `size` already, and that
    /// [`Self::compare_size_probed`] hasn't requested yet.
    ///
    /// `None` when there's no open panel or its selected row has nothing to
    /// hydrate — which is the normal case as soon as the probe already
    /// answered, so the run loop doesn't request the same thing every
    /// frame.
    #[must_use]
    pub fn compare_size_probe_targets(&self) -> Vec<VPath> {
        let Some(view) = &self.compare else {
            return Vec::new();
        };
        let Some(row) = view.pane.selected_row() else {
            return Vec::new();
        };
        [row.left.as_ref(), row.right.as_ref()]
            .into_iter()
            .flatten()
            .filter(|e| {
                e.kind == EntryKind::File
                    && e.size.is_none()
                    && !self.compare_size_probed.contains(&e.path)
            })
            .map(|e| e.path.clone())
            .collect()
    }

    /// Puts the #157 probe's result into the presentation cache
    /// ([`Self::compare_size_hints`]) and marks it probed
    /// ([`Self::compare_size_probed`]) no matter what — a `stat` that failed
    /// doesn't get retried until the next comparison either, same criterion
    /// as the normal pane's `last_probed`.
    pub fn hydrate_compare_size(&mut self, generation: u64, path: VPath, size: Option<u64>) {
        // #198: from ANOTHER comparison. Neither the size nor the probed
        // mark — marking it would leave the live comparison never
        // requesting it, which is the silent half of the same bug.
        if generation != self.compare_generation {
            return;
        }
        self.compare_size_probed.insert(path.clone());
        if let Some(size) = size {
            self.compare_size_hints.insert(path, size);
        }
    }

    /// The comparison that's starting. Clears the size cache and its dedup,
    /// and ADVANCES the generation: one without the other is #198's bug.
    pub fn begin_compare_generation(&mut self) {
        self.compare_size_hints.clear();
        self.compare_size_probed.clear();
        self.compare_generation = self.compare_generation.wrapping_add(1);
    }

    /// The comparison the size tables currently belong to. The run loop
    /// saves it when launching the probe and returns it on hydrating.
    #[must_use]
    pub fn compare_generation(&self) -> u64 {
        self.compare_generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::pane::Pane;
    use crate::app::testutil::*;
    use norte_proto::EntryKind;

    /// #157: an orphan `File` with no `size` is a candidate for the selected
    /// row's probe, and stops being one as soon as `hydrate_compare_size`
    /// resolves it — successfully or not, so it isn't retried every frame.
    #[test]
    fn compare_size_probe_targets_lone_file_with_no_size_and_no_repeat() {
        let mut app = App::new(Pane::new(root(), vec![]), Pane::new(root(), vec![]));
        let mut view = CompareView::new(vp("mem:///a"), vp("mem:///b"), 0, None, None);
        let row = orphan_row(1, EntryKind::File, None);
        let path = row.left.as_ref().unwrap().path.clone();
        view.pane.extend(vec![row]);
        app.compare = Some(view);

        assert_eq!(
            app.compare_size_probe_targets(),
            vec![path.clone()],
            "an orphan File with no size is a candidate"
        );

        // Probed SUCCESSFULLY: no longer a candidate, and the hint is set.
        app.hydrate_compare_size(app.compare_generation(), path.clone(), Some(42));
        assert!(
            app.compare_size_probe_targets().is_empty(),
            "already hydrated, doesn't repeat"
        );
        assert_eq!(app.compare_size_hints.get(&path), Some(&42));
    }

    /// #198: a probe launched for comparison A cannot land on B. The probe
    /// lives in the run loop and `launch_compare` doesn't see it, so the
    /// only defense is that the result carries the generation it was
    /// requested under — without that, the cache the rustdoc calls "of THIS
    /// comparison" ends up holding a size from the previous one, in the
    /// panel whose whole point is whether what you're looking at is exact.
    #[test]
    fn a_probe_from_the_previous_comparison_doesnt_land_in_the_new_one() {
        let mut app = App::new(Pane::new(root(), vec![]), Pane::new(root(), vec![]));
        let mut view = CompareView::new(vp("mem:///a"), vp("mem:///b"), 0, None, None);
        let row = orphan_row(1, EntryKind::File, None);
        let path = row.left.as_ref().expect("left").path.clone();
        view.pane.extend(vec![row.clone()]);
        app.compare = Some(view);
        let old = app.compare_generation();

        // Another comparison starts: the cache clears and the generation
        // advances.
        app.begin_compare_generation();
        let mut view = CompareView::new(vp("mem:///c"), vp("mem:///d"), 0, None, None);
        view.pane.extend(vec![row]);
        app.compare = Some(view);
        assert_ne!(app.compare_generation(), old);

        // The OLD comparison's probe arrives.
        app.hydrate_compare_size(old, path.clone(), Some(42));
        assert!(
            app.compare_size_hints.is_empty(),
            "not even the previous one's size"
        );
        assert_eq!(
            app.compare_size_probe_targets(),
            vec![path.clone()],
            "not marked probed either: the new one still has to request it"
        );

        // And the new one's does land.
        let now = app.compare_generation();
        app.hydrate_compare_size(now, path.clone(), Some(7));
        assert_eq!(app.compare_size_hints.get(&path), Some(&7));
    }

    /// A `stat` that fails (`None`) also gets marked probed: it isn't
    /// retried every frame against a broken provider, same criterion as
    /// `last_probed` in the normal pane.
    #[test]
    fn compare_size_probe_targets_doesnt_retry_a_failed_stat() {
        let mut app = App::new(Pane::new(root(), vec![]), Pane::new(root(), vec![]));
        let mut view = CompareView::new(vp("mem:///a"), vp("mem:///b"), 0, None, None);
        let row = orphan_row(1, EntryKind::File, None);
        let path = row.left.as_ref().unwrap().path.clone();
        view.pane.extend(vec![row]);
        app.compare = Some(view);

        app.hydrate_compare_size(app.compare_generation(), path, None);
        assert!(
            app.compare_size_probe_targets().is_empty(),
            "a failure also gets marked probed"
        );
        assert!(app.compare_size_hints.is_empty(), "no hint over a failure");
    }

    /// A directory or an orphan that ALREADY carries `size` aren't
    /// candidates — same criterion as `focused_needs_stat` for the normal
    /// pane: a `Dir` has no size an ordinary `stat` resolves.
    #[test]
    fn compare_size_probe_targets_ignores_dir_and_already_hydrated() {
        let mut app = App::new(Pane::new(root(), vec![]), Pane::new(root(), vec![]));
        let mut view = CompareView::new(vp("mem:///a"), vp("mem:///b"), 0, None, None);
        view.pane.extend(vec![
            orphan_row(1, EntryKind::Dir, None),
            orphan_row(2, EntryKind::File, Some(7)),
        ]);
        app.compare = Some(view);

        assert!(
            app.compare_size_probe_targets().is_empty(),
            "a Dir with no size and a File that already has one aren't candidates"
        );
    }
}
