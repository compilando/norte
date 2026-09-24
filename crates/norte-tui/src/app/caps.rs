//! What the core counts about each location and what the help does with it:
//! attribute catalogues, `Capabilities` cache by location, the read-only
//! predicate and the facts that dim the help.

use super::plugins::plugin_label;
use super::{App, CAPS_CACHE_MAX, caps_key};
use norte_proto::{EntryKind, VPath};

impl App {
    /// The given scheme's cached catalogue, if it arrived (#117): `None` =
    /// `fs.capabilities` hasn't run yet or failed — it paints with Opaque
    /// defaults and the header falls back to the id, the render never
    /// blocks.
    #[must_use]
    pub fn attr_catalog(&self, scheme: &str) -> Option<&norte_proto::AttrCatalog> {
        self.attr_catalogs.get(scheme)
    }

    /// Caches `scheme`'s catalogue (#117): called by startup and the cd flow
    /// after its `fs.capabilities` — once per scheme and session (a failure
    /// caches nothing: the next cd to the scheme retries).
    pub fn insert_attr_catalog(&mut self, scheme: String, catalog: norte_proto::AttrCatalog) {
        self.attr_catalogs.insert(scheme, catalog);
    }

    /// The cached capability flags of the location `at` belongs to, if the
    /// response has landed.
    ///
    /// Takes the PATH and not a scheme so that the caller cannot accidentally
    /// ask a coarser question than the cache answers: the key is scheme plus
    /// authority (see the `caps` field), and a `&str` parameter would have made
    /// "`sftp`" a legal thing to ask about.
    ///
    /// `None` means "not asked yet, or the call failed" and never "no
    /// capabilities": a caller must degrade rather than read absence as a
    /// denial (see [`Self::pane_read_only`] for the shape of that).
    #[must_use]
    pub fn caps(&self, at: &VPath) -> Option<&norte_proto::Capabilities> {
        self.caps.get(&caps_key(at))
    }

    /// Caches the capability flags of the location `at` belongs to.
    ///
    /// Called from the same place as [`Self::insert_attr_catalog`] and with
    /// the halves of ONE `fs.capabilities` response — see [`Self::caps`]'
    /// field docs for why keeping only the attrs was waste.
    pub fn insert_caps(&mut self, at: &VPath, caps: norte_proto::Capabilities) {
        let key = caps_key(at);
        if !self.caps.contains_key(&key) && self.caps.len() >= CAPS_CACHE_MAX {
            // The oldest one by ARRIVAL ORDER, which is what `caps_order`
            // remembers: a `HashMap` has no order, and picking "whichever"
            // would leave the cache dropping the location that was just
            // looked at as often as the one from half an hour ago.
            if let Some(oldest) = self.caps_order.pop_front() {
                self.caps.remove(&oldest);
            }
        }
        if self.caps.insert(key.clone(), caps).is_none() {
            self.caps_order.push_back(key);
        }
    }

    /// Whether the pane's location refuses mutation.
    ///
    /// Answered from the capability flags when they have arrived, and
    /// SYNTACTICALLY from the scheme until they do
    /// ([`norte_frontend::availability::scheme_is_read_only`]) — an archive
    /// scheme is read-only by construction, so the guess is right for the case
    /// that matters.
    ///
    /// Where the guess is wrong it errs toward WRITABLE, never toward
    /// read-only: a read-only SFTP export answers `false` here until its flags
    /// land, so the help offers a copy into it and the submitted task fails
    /// with an error the reader sees. That is the direction to be wrong in.
    /// Once the flags are in they decide, `READ_ONLY` included — its own
    /// contract is that the UI vetoes upfront.
    ///
    /// An index outside `0|1` is `false`, i.e. "writable, as far as this
    /// knows": the callers are the `Facts` assembly and the help, and a
    /// verdict is not the place to panic over a bad index.
    #[must_use]
    pub fn pane_read_only(&self, pane: usize) -> bool {
        let Some(p) = self.panes.get(pane) else {
            return false;
        };
        // The pair "flag if there is one, scheme if not" is decided by the
        // SHARED spot: the window had it written on its own, which is the
        // shape a decision takes when it drifts apart without anyone
        // noticing (ADR 0077).
        norte_frontend::availability::read_only(self.caps(p.dir()).copied(), p.dir().scheme())
    }

    /// The context the help's verdict table is asked against.
    ///
    /// Every predicate here is the one the matching `dispatch` arm uses, and
    /// that is the whole contract of this function: a fact derived a second way
    /// dims a row the app would have run, which is worse than not dimming at
    /// all — the reader stops trying.
    ///
    /// - `enterable`: `nav.enter`'s — `Dir | Symlink`, or an archive the TUI
    ///   knows how to compose a scheme for ([`crate::nav::archive_root_for`],
    ///   so a `.zip` FILE counts). This is where the TUI and the GUI genuinely
    ///   disagree, which is why the table takes the boolean rather than a kind.
    /// - `viewable`: `pane.view`'s — `File | Symlink` (a symlink to a
    ///   directory fails in the viewer with a visible message, which is the
    ///   dispatch arm's own decision).
    /// - `rename_single`: `true`, ALWAYS, and that is the honest answer rather
    ///   than a shortcut. `Command::PaneRename` (`App::open_rename`) targets
    ///   `selected()` and never looks at the marks, so shift+F6 renames exactly
    ///   one entry no matter how many are marked. Filling this from the marked
    ///   set — the obvious reading of the field's old name — dimmed the row for
    ///   a batch the TUI renames one entry of quite happily. The GUI fills the
    ///   same fact from its count, because its rename does refuse a multiple
    ///   selection.
    /// - the two read-only flags: [`Self::pane_read_only`] for the focused pane
    ///   and the other one. "Source" is the focused pane because every command
    ///   in the table acts FROM the focus.
    /// - `degraded`: [`Self::degraded_for`] on the focused pane's scheme. It
    ///   vetoes nothing today — see the field's rustdoc in
    ///   [`norte_frontend::availability::Facts`].
    ///
    /// NOT computed: policy denial. See [`crate::help::TuiChords`]'
    /// `availability` for why faking it would dim a row for a rule that does
    /// not apply to the human sitting here.
    #[must_use]
    pub fn help_facts(&self) -> norte_frontend::availability::Facts {
        let pane = self.focused();
        let sel = pane.selected();
        norte_frontend::availability::Facts {
            // The DISPATCH predicate, whole and without re-deriving it: this
            // list used to redo by hand what `enter_target` decides (and the
            // window answered it a third way, ADR 0077), but it also asked
            // `selected()`, which over the `..` row answers `None` — the
            // operand's funnel — and there the help dimmed a key that goes
            // up. `nav_enter_target` is what runs on pressing it, the parent
            // row included.
            enterable: crate::trail::nav_enter_target(self).is_some(),
            viewable: sel.is_some_and(|e| matches!(e.kind, EntryKind::File | EntryKind::Symlink)),
            rename_single: true,
            source_read_only: self.pane_read_only(self.focus),
            // The destination is the ROLE's, as in everything else. With
            // none designated (three or more panels) it answers with its
            // own: it's a WARNING, and saying "read only" when it isn't
            // blocks nothing.
            dest_read_only: self
                .pane_read_only(self.target_index().unwrap_or_else(|| self.focus())),
            degraded: self.degraded_for(pane.dir().scheme()).is_some(),
            journalled: self.backend_journalled,
            // Phase 9: the handoff's two blockers, decided ONCE at startup
            // (the backend's branch doesn't change during the process'
            // life, and a desktop doesn't appear mid-session either).
            daemon: self.backend_daemon,
            windowed: self.has_desktop,
        }
    }

    /// Freezes [`Self::help_facts`] into the resolver the help overlay renders
    /// through.
    ///
    /// Called when the overlay OPENS, before the first layout, so every row of
    /// every page the reader walks is judged against one context — see
    /// [`crate::help::TuiChords`]' `facts` for why a live read would make a
    /// page disagree with itself.
    ///
    /// And called AGAIN whenever the listing underneath is replaced with the
    /// overlay still open (`main::after_panes_refresh`): the freeze is against
    /// the reader moving, not against the world moving, and `enterable` /
    /// `viewable` describe an entry a finished task can delete.
    pub fn freeze_help_facts(&mut self) {
        let facts = self.help_facts();
        self.help_chords = std::sync::Arc::new(self.help_chords.with_facts(facts));
    }

    /// Freezes the plugin catalogue into the overlay AND into the resolver it
    /// renders through (H3e).
    ///
    /// Both halves of one snapshot, so they cannot disagree: the sidebar offers
    /// a page for every plugin with a `help.md`
    /// ([`super::HelpView::set_plugins`]), and the resolver dims the command rows of
    /// the ones that are not approved-and-enabled
    /// ([`crate::help::TuiChords::with_plugins`]).
    ///
    /// A PHOTOGRAPH, taken when the help opens and thrown away with it. That is
    /// the same discipline [`Self::freeze_help_facts`] follows and it buys the
    /// same thing — no verdict changes under the reader's cursor — plus two
    /// practical ones: there is no plugin cache in [`App`] to invalidate, and no
    /// round trip to the daemon while a frame is being painted.
    ///
    /// The resolver is updated even with no overlay open. It is the same
    /// snapshot either way, and `with_plugins` carries the frozen facts across
    /// exactly as `with_facts` carries the plugins across — so the re-freeze the
    /// refresh funnel performs mid-overlay cannot drop either half.
    pub fn freeze_help_plugins(&mut self, plugins: &[norte_proto::methods::PluginInfo]) {
        // The id gate of [`super::HelpView::set_plugins`], applied to the resolver's
        // half of the snapshot as well, so no structure the help owns can hold
        // an id the host had no business announcing. `set_plugins` re-applies it
        // rather than trusting this: it is public and exercised directly by
        // tests, so it stays safe by construction like
        // `norte_frontend::palette::plugin_rows`.
        let plugins: Vec<&norte_proto::methods::PluginInfo> = plugins
            .iter()
            .filter(|p| norte_core::is_valid_plugin_id(&p.id))
            .collect();
        let active: std::collections::BTreeSet<String> = plugins
            .iter()
            .filter(|p| p.approved && p.enabled)
            .map(|p| p.id.clone())
            .collect();
        // The manifest's name for every contributed command, keyed by its
        // DISPATCH key — built with the same `format!` the palette uses
        // (`norte_frontend::palette::plugin_rows`) so these keys and the ones a
        // page carries in `topic.commands` cannot be spelled differently. The
        // command id has no validated charset and may contain `:`, which is why
        // nothing here ever splits one; it is only ever appended.
        //
        // NOT filtered by `approved && enabled`, unlike `active` above: an
        // inactive plugin's page still lists its rows — dimmed, which is the
        // answer the reader came for — and a dimmed row deserves its name as
        // much as a live one. The wire agrees; `commands` is discovery data a
        // human inspects BEFORE approving (`norte_core::plugins`).
        //
        // A blank title is dropped rather than stored: `label` would return it
        // verbatim and `norte_help::label_or_id` would then fall back to the id
        // anyway, so keeping it would only make the map lie about what it knows.
        let titles: std::collections::HashMap<String, String> = plugins
            .iter()
            .flat_map(|p| {
                let plugin_id = p.id.clone();
                p.commands.iter().map(move |c| {
                    (
                        format!("plugin:{plugin_id}:{}", c.id),
                        plugin_label(&c.title),
                    )
                })
            })
            .filter(|(_, title)| !title.is_empty())
            .collect();
        if let Some(help) = self.help.as_mut() {
            let owned: Vec<norte_proto::methods::PluginInfo> =
                plugins.into_iter().cloned().collect();
            help.set_plugins(&owned);
        }
        self.help_chords = std::sync::Arc::new(self.help_chords.with_plugins(active, titles));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::pane::Pane;
    use crate::app::testutil::*;
    use norte_proto::{Entry, EntryKind};

    /// Caps are cached by LOCATION, for the same reason the attr catalogue
    /// is requested: `fs.capabilities` returns both halves in ONE call and
    /// the TUI already does it for the columns. Dropping the caps half and
    /// then probing again would be paying two round trips for data that
    /// already arrived.
    #[test]
    fn caps_are_cached_by_location() {
        let mut app = app_two_panes();
        let mem = vp("mem:///");
        assert!(app.caps(&mem).is_none(), "nothing gets made up unseeded");
        app.insert_caps(&mem, test_caps());
        assert!(app.caps(&mem).is_some());
        assert!(
            app.caps(&vp("sftp://example.org/")).is_none(),
            "one scheme doesn't answer for another"
        );
    }

    /// MAJOR-1: `sftp` isn't ONE place. Two hosts of the same scheme are two
    /// different backends, and the cache has to count them apart or the
    /// first one to answer decides for all the others for the whole
    /// session. Today no provider in the tree declares `READ_ONLY` per
    /// location (the archive and the plugins decide it by scheme), so the
    /// scheme-only key wasn't failing — by luck, not by design, and
    /// `App::caps` is a general accessor that invites reading any flag.
    #[test]
    fn two_authorities_of_the_same_scheme_dont_answer_for_each_other() {
        let a = vp("sftp://a.org/");
        let b = vp("sftp://b.org/");
        let mut app = App::new(
            Pane::new(a.clone(), Vec::new()),
            Pane::new(b.clone(), Vec::new()),
        );
        app.insert_caps(
            &a,
            norte_proto::Capabilities {
                flags: norte_proto::CapabilityFlags::READ_ONLY,
                max_path: None,
            },
        );
        assert!(app.pane_read_only(0), "a.org said it's read-only");
        assert!(
            app.caps(&b).is_none(),
            "b.org hasn't been asked anything yet"
        );
        assert!(
            !app.pane_read_only(1),
            "b.org can't inherit a.org's veto: they're two backends"
        );
    }

    /// Before the first answer arrives, the honest answer is "don't know",
    /// and whoever asks falls back to the SYNTACTIC criterion (the scheme
    /// says whether it's a compressed archive). What it cannot do is claim
    /// it can be written to.
    #[test]
    fn without_caps_yet_the_scheme_decides_read_only() {
        let app = app_two_panes();
        assert!(!app.pane_read_only(0), "mem:// isn't read-only");

        let inside_a_zip = app_en("zip+file:///a.zip/!", "file:///home");
        assert!(
            inside_a_zip.pane_read_only(0),
            "an archive scheme is read-only by construction"
        );
        assert!(!inside_a_zip.pane_read_only(1));
    }

    /// Once caps DO arrive they take over: a provider that announces
    /// `READ_ONLY` over a scheme that isn't syntactically one (a read-only
    /// remote mount) gets vetoed just the same.
    #[test]
    fn once_caps_arrive_the_read_only_flag_rules() {
        let mut app = app_two_panes();
        let dir = app.panes[0].dir().clone();
        app.insert_caps(
            &dir,
            norte_proto::Capabilities {
                flags: norte_proto::CapabilityFlags::READ_ONLY,
                max_path: None,
            },
        );
        assert!(app.pane_read_only(0));
        app.insert_caps(&dir, test_caps());
        assert!(!app.pane_read_only(0), "without the flag, writable");
    }

    /// The facts the help freezes come from the SAME predicates the
    /// `dispatch` arms use: a `.zip` gets ENTERED in the TUI (`nav.enter`
    /// composes the scheme) even though it's a File, and `pane.view` wants a
    /// File or a Symlink. Re-deriving them here would dim rows the app
    /// would run.
    #[test]
    fn help_facts_follow_the_dispatch_predicates() {
        let mut app = app_two_panes();
        // The cursor is on a plain File: it isn't entered, it's viewed.
        let f = app.help_facts();
        assert!(!f.enterable, "an ordinary file isn't entered");
        assert!(f.viewable);
        assert!(f.rename_single, "shift+F6 renames ONE: the cursor's");
        assert!(!f.source_read_only && !f.dest_read_only);
        assert!(!f.degraded);

        // A `.zip` IS enterable in the TUI even though its kind is File.
        let zip = Pane::new(
            root(),
            vec![Entry {
                attrs: std::collections::BTreeMap::new(),
                path: root().join(norte_proto::Segment::new(b"a.zip".to_vec()).unwrap()),
                kind: EntryKind::File,
                size: Some(1),
                mtime_ms: None,
            }],
        );
        let app_zip = App::new(zip, pane_con(&["b"]));
        assert!(
            app_zip.help_facts().enterable,
            "in the TUI a .zip is entered: the help can't say otherwise"
        );

        // And the focused pane's scheme degradation reaches the fact.
        app.note_degraded(test_degraded("mem", "no-host"));
        assert!(app.help_facts().degraded);
    }

    /// And with the cursor on `..` the help offers `Enter`, which is what
    /// the key does there: GO UP.
    ///
    /// The fact used to come from `selected()`, which answers `None` over
    /// the parent row on purpose — that's the funnel that keeps F8 from
    /// deleting the parent — so `enterable` was `false` right where the
    /// cursor is born after every `cd`. Dispatch never asked it that way:
    /// `nav_enter_target` checks `cursor_is_parent_row()` first. The usual
    /// trap, "describing" by reading through "operating"'s door, and the
    /// help was dimming a key that goes up perfectly well.
    #[test]
    fn with_the_cursor_on_the_up_row_the_help_offers_enter() {
        let mut app = App::new(
            Pane::new(vp("mem:///home"), vec![file("a")]),
            pane_con(&["b"]),
        );
        app.set_parent_row(true);
        assert!(
            app.focused().cursor_is_parent_row(),
            "the premise: the cursor is born on `..`"
        );
        assert!(
            crate::trail::nav_enter_target(&app).is_some(),
            "the premise: the key DOES do something here"
        );

        assert!(
            app.help_facts().enterable,
            "`Enter` goes up from `..`: the help can't say \"doesn't apply here\""
        );
    }

    /// With SEVERAL marks the help does NOT dim shift+F6, because the TUI
    /// runs it: `Command::PaneRename` goes to `open_rename`, which renames
    /// `selected()` and doesn't look at the marks. Dimming it would be the
    /// exact mistake H3d exists to not make — turning off a row the app
    /// would have run, which teaches the reader not to try it again.
    ///
    /// (The GUI does refuse with a multiple selection, and its menu still
    /// does: `norte_gui::context_menu`,
    /// `renombrar_es_una_sola_entrada_y_la_de_ia_es_otra`. The fact belongs
    /// to the caller precisely because both answers are correct.)
    #[test]
    fn with_several_marks_the_help_does_not_dim_rename() {
        use norte_help::ChordResolver as _;

        let mut app = App::new(pane_con(&["a", "b", "c"]), pane_con(&["z"]));
        app.focused_mut().toggle_mark_and_advance();
        app.focused_mut().toggle_mark_and_advance();
        assert_eq!(
            app.focused().marked_paths().len(),
            2,
            "there are TWO marks: the case that was being dimmed"
        );
        assert!(app.help_facts().rename_single);

        app.freeze_help_facts();
        assert!(
            app.help_chords.availability("pane.rename").is_available(),
            "the TUI renames the cursor's with marks set: the help can't deny it"
        );
        // And inside an archive it does turn off, via the backend — the real
        // veto still stands.
        let mut zip = App::new(
            Pane::new(vp("zip+file:///a.zip/!"), Vec::new()),
            pane_con(&["z"]),
        );
        zip.freeze_help_facts();
        assert_eq!(
            zip.help_chords.availability("pane.rename").reason(),
            Some(norte_help::Reason::ReadOnlyBackend)
        );
    }

    /// Freezing the facts on opening the help: the resolver the view uses
    /// starts answering with the facts from THAT moment.
    #[test]
    fn freezing_the_facts_rewrites_the_help_resolver() {
        use norte_help::ChordResolver as _;

        let mut app = app_en("zip+file:///a.zip/!", "zip+file:///b.zip/!");
        assert!(
            app.help_chords.availability("pane.copy").is_available(),
            "before freezing the resolver knows nothing about the context"
        );
        app.freeze_help_facts();
        assert_eq!(
            app.help_chords.availability("pane.copy").reason(),
            Some(norte_help::Reason::ReadOnlyBackend),
            "both panes are read-only: copying has no destination"
        );
    }
}
