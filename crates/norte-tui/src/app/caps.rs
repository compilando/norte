//! Lo que el core cuenta de cada localización y lo que la ayuda hace con
//! ello: catálogos de atributos, caché de `Capabilities` por localización,
//! el predicado de solo lectura y los hechos con los que se atenúa la ayuda.

use super::plugins::plugin_label;
use super::{App, CAPS_CACHE_MAX, caps_key};
use norte_proto::{EntryKind, VPath};

impl App {
    /// El catálogo cacheado del scheme dado, si llegó (#117): `None` = el
    /// `fs.capabilities` aún no corrió o falló — se pinta con defaults
    /// Opaque y la cabecera cae al id, jamás se bloquea el render.
    #[must_use]
    pub fn attr_catalog(&self, scheme: &str) -> Option<&norte_proto::AttrCatalog> {
        self.attr_catalogs.get(scheme)
    }

    /// Cachea el catálogo de `scheme` (#117): lo llaman el arranque y el
    /// flujo de cd tras su `fs.capabilities` — una vez por scheme y sesión
    /// (un fallo no cachea nada: el próximo cd al scheme reintenta).
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
            // El más viejo por ORDEN DE LLEGADA, que es lo que
            // `caps_order` recuerda: un `HashMap` no tiene orden y elegir
            // «cualquiera» dejaría la caché tirando la ubicación que se acaba
            // de mirar tan a menudo como la de hace media hora.
            if let Some(viejo) = self.caps_order.pop_front() {
                self.caps.remove(&viejo);
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
        match self.caps(p.dir()) {
            Some(c) => c.flags.contains(norte_proto::CapabilityFlags::READ_ONLY),
            None => norte_frontend::availability::scheme_is_read_only(p.dir().scheme()),
        }
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
            enterable: sel.is_some_and(|e| {
                matches!(e.kind, EntryKind::Dir | EntryKind::Symlink)
                    || crate::nav::archive_root_for(e).is_some()
            }),
            viewable: sel.is_some_and(|e| matches!(e.kind, EntryKind::File | EntryKind::Symlink)),
            rename_single: true,
            source_read_only: self.pane_read_only(self.focus),
            dest_read_only: self.pane_read_only(self.focus ^ 1),
            degraded: self.degraded_for(pane.dir().scheme()).is_some(),
            journalled: self.backend_journalled,
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
    /// ([`HelpView::set_plugins`]), and the resolver dims the command rows of
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
        // The id gate of [`HelpView::set_plugins`], applied to the resolver's
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
