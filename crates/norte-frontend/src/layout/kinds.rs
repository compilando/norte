//! What the engine needs to know about a panel, and the registry that tells
//! it.
//!
//! What is NOT here is how it is painted: that is a per-frontend table,
//! because the TUI paints ratatui and the GUI paints GPUI. The engine only
//! needs minimum sizes, whether it takes focus, whether it takes keys,
//! whether it allows several instances and which roles it may hold.

use super::{KindId, RoleId};

/// What the engine needs to know about a kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindDecl {
    /// Which kind it describes.
    pub id: KindId,
    /// MINIMUM width and height in cells. Below this, [`super::resolve`]
    /// collapses the `Split` that contains it.
    pub min: (u16, u16),
    /// Can it take focus?
    pub focusable: bool,
    /// Does it consume keys from its own namespace?
    pub takes_keys: bool,
    /// Can several instances coexist?
    pub multi: bool,
    /// Which roles this kind may hold.
    pub roles: &'static [RoleId],
}

/// The roles a `browser` may take: both of them.
const ROLES_BROWSER: &[RoleId] = &[RoleId::Active, RoleId::Target];
/// No role.
const SIN_ROLES: &[RoleId] = &[];

/// The slot name of a panel contributed by a plugin (phase 3).
///
/// `plugin:<id>:<kind>`, and the prefix is the guarantee: [`KindId`] is an
/// unvalidated `String`, so the only thing stopping a plugin from declaring
/// a panel called `browser` and hijacking the listing is that its real name
/// never starts with `plugin:`. Both frontends form it HERE and not each on
/// its own, which is how two surfaces end up opening different slots for
/// the same panel.
///
/// ```
/// use norte_frontend::layout::panel_kind_id;
///
/// let id = panel_kind_id("org.norte.git-panel", "git");
/// assert_eq!(id.as_str(), "plugin:org.norte.git-panel:git");
/// ```
#[must_use]
pub fn panel_kind_id(plugin_id: &str, kind: &str) -> KindId {
    KindId::new(format!("plugin:{plugin_id}:{kind}"))
}

/// The kinds this binary knows how to paint.
///
/// OPEN by construction: [`KindRegistry::get`] returns `None` for what it
/// does not know and that is **not an error** — whoever paints draws a box
/// with the name and the layout keeps the node intact. That is what lets a
/// frontend open the other one's layout without erasing anything, and
/// later lets a plugin contribute a kind.
#[derive(Debug, Clone, Default)]
pub struct KindRegistry {
    decls: Vec<KindDecl>,
}

impl KindRegistry {
    /// The five kinds that exist today, reframed: `browser`, `tasks`,
    /// `viewer`, `compare` and `sync`.
    ///
    /// The minimums come from what today's screen genuinely needs: a
    /// `browser` under 20 columns does not even paint a name with its
    /// size, and `compare`/`sync` carry two sides and a header.
    #[must_use]
    pub fn builtin() -> Self {
        let decl = |id: &str, min, focusable, takes_keys, multi, roles| KindDecl {
            id: KindId::new(id),
            min,
            focusable,
            takes_keys,
            multi,
            roles,
        };
        Self {
            decls: vec![
                decl("browser", (20, 5), true, true, true, ROLES_BROWSER),
                // The tasks strip: it is looked at, not focused, and there
                // is one.
                decl("tasks", (20, 3), false, false, false, SIN_ROLES),
                // The status bar: one row, nobody focuses it.
                decl("status", (1, 1), false, false, false, SIN_ROLES),
                // The places sidebar (L3): focusable and takes keys, but
                // does NOT hold any role — a sidebar is never a copy's
                // target. And there is one: two identical lists of drives
                // are not a layout, they are a bug. The 14-column minimum
                // is what `/boot 402M` takes with the frame around it.
                decl("places", (14, 5), true, true, false, SIN_ROLES),
                decl("viewer", (20, 5), true, true, false, SIN_ROLES),
                decl("compare", (40, 8), true, true, false, SIN_ROLES),
                decl("sync", (40, 8), true, true, false, SIN_ROLES),
                // The processes panel: the `tasks` strip still exists and
                // is still what `orthodox` brings. This is the real
                // panel — it is focused, browsed and cancels the cursor's
                // row — and there is one. The 30x4 minimum is what one row
                // with a name, a bar and a percentage takes.
                decl("processes", (30, 4), true, true, false, SIN_ROLES),
                // The attribute sheet: follows the `active` role with the
                // same binding as the docked viewer. 24 columns is the
                // longest label with its value next to it.
                //
                // Does NOT take keys, and declaring that was half of #243:
                // the sheet follows the listing's cursor, so with the
                // keyboard inside it would stop following anything. It is
                // focusable — layout counts it — but consumes no key.
                decl("metadata", (24, 4), true, false, false, SIN_ROLES),
                // The directory tree (#136): focusable, takes keys and
                // there is ONE. Holds no role — a tree is not a copy's
                // target, same as the sidebar — and 16 columns is what a
                // short name with two indent levels and the frame takes.
                decl("tree", (16, 5), true, true, false, SIN_ROLES),
                // The log (#323): focusable, takes keys — filters by level
                // and by text — and there is ONE. Holds no role: nobody
                // copies into a log. 30 columns is what `13:36:50 WARN`
                // takes with a short message and the frame; below that the
                // time and level eat the whole line and there is no room
                // left for what it says.
                decl("log", (30, 4), true, true, false, SIN_ROLES),
                // The disk map (phase 4): focusable, takes keys — you walk
                // the rectangles and enter one — and there is ONE. Holds no
                // role: a map is looked at and browsed, and nobody copies
                // into a treemap.
                //
                // 24x6 is the minimum at which it is still a MAP.
                // Sideways, 24 columns is what a label like `documents
                // 1.2G` takes with the frame around it; below that the
                // rectangles no longer fit with their name and what is
                // left is a mosaic of colors with no legend. Vertically,
                // six rows are two strips with their label plus the frame:
                // with fewer, only one strip fits, and a single strip lays
                // out nothing — it is a bar.
                decl("disk-map", (24, 6), true, true, false, SIN_ROLES),
                // The journal's timeline (phase 7): focusable, takes keys
                // — you walk the rows and undo up to one — and there is
                // ONE. Holds no role: nobody copies into a history.
                //
                // 34x4 is the minimum at which a row still says something:
                // the time, who, the verb and a short name. Below that the
                // name disappears entirely and only the time and the verb
                // remain, which do not distinguish two copies in a row —
                // and this is a screen you UNDO from, so a row that does
                // not identify what it is about to revert is worse than
                // not having it.
                decl("timeline", (34, 4), true, true, false, SIN_ROLES),
                // The terminal panel: focusable, takes keys and there is
                // ONE. Holds no role: nobody copies INTO a terminal, and a
                // copy's target is a directory, not a shell.
                //
                // `multi: false`, and the plan said the opposite ("two
                // terminals are two terminals"). What changed it:
                // `KeyOwner` is compared by equality in eighty-six places
                // and NO variant carries a payload, which only holds
                // because keyboard-holding panels are one at a time. A
                // multi-instance terminal would require carrying WHICH one
                // has the keys inside it, and that is a change to all
                // eighty-six for a capability the reference — Krusader —
                // does not give either. The tree, the log, processes, the
                // map and the timeline are all singletons; this one too.
                //
                // "Takes keys" means more here than for any other kind: the
                // others consume catalogue commands, and a terminal
                // consumes BYTES, i.e. also the chords that would be
                // norte's. The exit is the SAME `layout.terminal` that
                // opened it — a lone chord, the only one the panel does not
                // pass to the shell — and not a separate command: one
                // called `layout.terminal-escape` would have been a second
                // binding in all seven presets for what the entry key
                // already says.
                //
                // 20x4 is the minimum at which it is still a shell:
                // sideways, a short prompt and a command with one argument;
                // vertically, the prompt, what is typed and two lines of
                // response. Below that each command erases the previous
                // one and what remains is not a terminal, it is a little
                // window that blinks.
                decl("terminal", (20, 4), true, true, false, SIN_ROLES),
            ],
        }
    }

    /// All declarations, IN REGISTRATION ORDER: the built-in ones first,
    /// then whatever was added.
    ///
    /// The order is part of the contract and not a detail: the panel bar
    /// (#324) uses it to paint the usual ones in the same place and
    /// contributed ones after, which is what lets the position be learned
    /// by the finger.
    #[must_use]
    pub fn decls(&self) -> &[KindDecl] {
        &self.decls
    }

    /// `id`'s declaration, or `None` if this binary does not know that
    /// kind.
    #[must_use]
    pub fn get(&self, id: &KindId) -> Option<&KindDecl> {
        self.decls.iter().find(|d| &d.id == id)
    }

    /// Declares the panels CONSENTED plugins contribute (phase 3).
    ///
    /// A panel is called `plugin:<id>:<kind>`, and that prefix is what
    /// stops it from colliding with a built-in one: `KindId` validates
    /// nothing — it is a `String` — so the guarantee comes from the NAME,
    /// not the type. A plugin called `browser` cannot hijack the listing.
    ///
    /// Only the approved AND enabled ones, with the same criterion as
    /// columns (`validated_plugin_requests`): a plugin's panel the reader
    /// has not consented to does not exist for layout, so its slot is not
    /// placed and its button does not appear in the bar.
    ///
    /// REPLACES what is contributed, does not add to it: withdrawing
    /// consent from a plugin has to withdraw its panel in the same
    /// session. Adding instead, a plugin disabled in the manager kept its
    /// declared kind until the next startup — its slot kept being placed
    /// and taking focus — which is the opposite of what the paragraph
    /// above promises. The built-in ones are not touched, and what is
    /// contributed is rebuilt whole on every catalogue.
    ///
    /// Within that the order is kept: `decls()` promises the built-in ones
    /// first and contributed ones after.
    pub fn insert_panels(&mut self, plugins: &[norte_proto::methods::PluginInfo]) {
        // The alphabet of a name headed into a `KindId`: ASCII alphanumeric
        // and `. _ -`, with a ceiling. Leaves out space, colons — which are
        // the prefix's own separator — controls, line breaks and anything
        // double-width or right-to-left.
        let valid = |s: &str| {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        };
        self.decls.retain(|d| !d.id.as_str().starts_with("plugin:"));
        for p in plugins.iter().filter(|p| p.approved && p.enabled) {
            for panel in &p.panels {
                // The id and the kind are a THIRD PARTY's text and end up
                // in a `KindId`, which validates nothing: from there come
                // the name that is painted and the key saved in the
                // session. A kind with a line break, a double-width
                // character or an escape sequence breaks the bar and the
                // layout file, so whatever does not fit the alphabet is
                // not declared — the panel disappears, which is the fail
                // safe.
                if !valid(&p.id) || !valid(&panel.kind) {
                    continue;
                }
                self.insert(KindDecl {
                    id: panel_kind_id(&p.id, &panel.kind),
                    // Whatever the manifest asks for, and if it asks for
                    // nothing, the minimum of any side panel: below that
                    // not even one line fits with its frame.
                    min: (panel.min_cols.unwrap_or(20), panel.min_rows.unwrap_or(4)),
                    // Focusable and takes keys: a panel that could not
                    // receive a key could not offer anything beyond a
                    // click, and the guest receives COMMANDS for exactly
                    // that.
                    focusable: true,
                    takes_keys: true,
                    // One of each: two copies of the same git panel are
                    // not a layout, they are a bug. Same criterion as the
                    // built-in side panels.
                    multi: false,
                    // No role: a plugin panel is not a copy's target, same
                    // as the sidebar or the tree.
                    roles: SIN_ROLES,
                });
            }
        }
    }

    /// Adds or replaces a declaration.
    pub fn insert(&mut self, decl: KindDecl) {
        if let Some(slot) = self.decls.iter_mut().find(|d| d.id == decl.id) {
            *slot = decl;
        } else {
            self.decls.push(decl);
        }
    }

    /// A kind's minimum, or `(1, 1)` if it is not known: an unknown kind is
    /// painted the same (a box with its name), so it cannot demand room
    /// nobody knows the size of.
    #[must_use]
    pub fn min_of(&self, id: &KindId) -> (u16, u16) {
        self.get(id).map_or((1, 1), |d| d.min)
    }

    /// Can this kind hold role `role`? An unknown kind, never.
    #[must_use]
    pub fn holds_role(&self, id: &KindId, role: RoleId) -> bool {
        self.get(id).is_some_and(|d| d.roles.contains(&role))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Phase A's two kinds. Neither holds a role: a processes panel and an
    /// attribute sheet are never a copy's target, and leaving them
    /// `Target` is how a copy key ends up pointing at a box that is not a
    /// directory.
    ///
    /// And only ONE of the two takes keys. The sheet follows the listing's
    /// cursor, so with the keyboard inside it would stop following
    /// anything; declaring it the other way round was half of #243 — the
    /// other half was that nobody read the `KeyOwner` that got set — and
    /// the result on screen was a panel with a focus border whose arrows
    /// moved the list next to it.
    #[test]
    fn processes_y_metadata_se_enfocan_pero_no_son_destino() {
        let reg = KindRegistry::builtin();
        for id in ["processes", "metadata"] {
            let d = reg.get(&KindId::new(id)).expect("declared");
            assert!(d.focusable, "{id} is focusable");
            assert!(!d.multi, "{id} is one only");
            assert!(d.roles.is_empty(), "{id} holds no role");
            assert!(!reg.holds_role(&KindId::new(id), RoleId::Target));
        }
        assert!(
            reg.get(&KindId::new("processes"))
                .expect("declared")
                .takes_keys,
            "the processes panel DOES take keys: browsed and cancels"
        );
        assert!(
            !reg.get(&KindId::new("metadata"))
                .expect("declared")
                .takes_keys,
            "the attribute sheet does NOT: it follows the listing's cursor"
        );
        assert_eq!(reg.min_of(&KindId::new("processes")), (30, 4));
        assert_eq!(reg.min_of(&KindId::new("metadata")), (24, 4));
    }

    /// A kind the registry does not know does not blow up: it returns
    /// `None` and whoever paints draws the box with the name. It is the
    /// model's rule 3.
    #[test]
    fn un_kind_fuera_del_registro_no_es_un_error() {
        let reg = KindRegistry::builtin();
        // The name says what it needs to be: one that will NEVER be
        // registered. This used to say `terminal`, and when the terminal
        // panel got registered this test stopped testing what its name
        // says without turning red.
        let none = KindId::new("un-kind-que-no-existe");
        assert!(reg.get(&none).is_none());
        assert_eq!(reg.min_of(&none), (1, 1));
        assert!(!reg.holds_role(&none, RoleId::Target));
    }

    /// A CONTRIBUTED panel does not appear in the bar, even if focusable.
    ///
    /// A button's command is `layout.<kind>`, and for a contributed one it
    /// would be `layout.plugin:git:status`, which does not exist in any
    /// catalogue: the TUI silently dropped it and the window answered
    /// "cmd-not-here". The same decision with two answers is exactly what
    /// ADR 0077 forbids, so until the command that opens and closes it
    /// exists, there is no button.
    #[test]
    fn un_panel_de_plugin_no_tiene_boton_en_la_barra() {
        let mut reg = KindRegistry::builtin();
        reg.insert_panels(&[panel_de_plugin("git", "status", None, true)]);
        let d = reg
            .get(&KindId::new("plugin:git:status"))
            .expect("is declared");
        assert!(d.focusable, "is focusable");
        assert!(
            !crate::panelbar::es_boton(d),
            "and still does not appear in the bar"
        );
    }

    /// The minimums are the only thing the engine consults to collapse, so
    /// declaring them wrong shows across the whole screen.
    #[test]
    fn el_browser_declara_su_minimo_y_puede_tomar_los_dos_roles() {
        let reg = KindRegistry::builtin();
        let d = reg.get(&KindId::browser()).expect("browser is there");
        assert_eq!(d.min, (20, 5));
        assert!(d.focusable && d.takes_keys && d.multi);
        assert_eq!(d.roles, &[RoleId::Active, RoleId::Target]);
    }

    /// `tasks` is the bottom strip: does not take focus, does not take
    /// keys, and there is ONE.
    #[test]
    fn tasks_es_unico_y_no_toma_foco() {
        let reg = KindRegistry::builtin();
        let d = reg.get(&KindId::new("tasks")).expect("tasks is there");
        assert!(!d.focusable && !d.takes_keys && !d.multi);
        assert!(d.roles.is_empty());
    }

    /// The sidebar holds no role and does not allow two. The first is what
    /// stops a copy from ending up targeting a list of drives.
    #[test]
    fn places_no_toma_roles_y_es_unico() {
        let reg = KindRegistry::builtin();
        let d = reg.get(&KindId::new("places")).expect("places is there");
        assert_eq!(d.min, (14, 5));
        assert!(d.focusable && d.takes_keys);
        assert!(!d.multi);
        assert!(d.roles.is_empty());
        assert!(!reg.holds_role(&KindId::new("places"), RoleId::Target));
    }

    fn panel_de_plugin(
        id: &str,
        kind: &str,
        min: Option<(u16, u16)>,
        ok: bool,
    ) -> norte_proto::methods::PluginInfo {
        use norte_proto::methods::PluginInfo;
        PluginInfo {
            id: id.to_owned(),
            name: id.to_owned(),
            publisher: String::new(),
            version: "1.0.0".to_owned(),
            category: "panel".to_owned(),
            capabilities: Vec::new(),
            approved: ok,
            enabled: ok,
            description: None,
            commands: Vec::new(),
            columns: Vec::new(),
            panels: vec![norte_proto::methods::PluginPanelInfo {
                kind: kind.to_owned(),
                title: "Git".to_owned(),
                min_cols: min.map(|(c, _)| c),
                min_rows: min.map(|(_, r)| r),
            }],
            has_help: false,
            manifest_digest: None,
        }
    }

    /// A NON-consented plugin's panel does not exist for layout.
    ///
    /// Same criterion as columns: the slot is not placed and its button
    /// does not appear in the bar until the reader approves and enables
    /// the plugin.
    #[test]
    fn un_panel_sin_consentir_no_aporta_kind() {
        let mut reg = KindRegistry::builtin();
        reg.insert_panels(&[panel_de_plugin("org.norte.git", "git", None, false)]);
        assert!(
            reg.get(&panel_kind_id("org.norte.git", "git")).is_none(),
            "not approved or enabled, no panel"
        );
    }

    /// Consented, the kind exists, carries the prefix that stops
    /// collisions, and takes keys.
    #[test]
    fn un_panel_consentido_es_un_kind_con_su_prefijo() {
        let mut reg = KindRegistry::builtin();
        reg.insert_panels(&[panel_de_plugin("org.norte.git", "git", None, true)]);
        let decl = reg
            .get(&panel_kind_id("org.norte.git", "git"))
            .expect("the panel is declared");
        assert_eq!(decl.id.as_str(), "plugin:org.norte.git:git");
        assert!(decl.focusable && decl.takes_keys);
        assert!(
            !decl.multi,
            "one of each panel, like the built-in side ones"
        );
    }

    /// The size is decided by the MANIFEST when it says so, and there is a
    /// fallback when it does not: a panel with no declared minimums cannot
    /// be left with none, or layout would place it in two columns.
    #[test]
    fn los_minimos_del_manifiesto_mandan_y_hay_respaldo() {
        let mut reg = KindRegistry::builtin();
        reg.insert_panels(&[
            panel_de_plugin("org.norte.git", "git", Some((40, 9)), true),
            panel_de_plugin("org.norte.otro", "x", None, true),
        ]);
        assert_eq!(
            reg.get(&panel_kind_id("org.norte.git", "git"))
                .expect("is there")
                .min,
            (40, 9)
        );
        assert_eq!(
            reg.get(&panel_kind_id("org.norte.otro", "x"))
                .expect("is there")
                .min,
            (20, 4)
        );
    }

    /// Contributed ones go AFTER the built-in ones.
    ///
    /// Not cosmetic: the panel bar paints in the registry's order, and the
    /// usual ones being where they always are is what lets a button's
    /// position be learned by the finger.
    #[test]
    fn lo_aportado_no_se_cuela_delante_de_lo_de_serie() {
        let before: Vec<String> = KindRegistry::builtin()
            .decls()
            .iter()
            .map(|d| d.id.as_str().to_owned())
            .collect();
        let mut reg = KindRegistry::builtin();
        reg.insert_panels(&[panel_de_plugin("org.norte.git", "git", None, true)]);
        let after: Vec<String> = reg
            .decls()
            .iter()
            .map(|d| d.id.as_str().to_owned())
            .collect();
        assert_eq!(
            &after[..before.len()],
            &before[..],
            "the built-in ones, untouched"
        );
        assert_eq!(
            after.last().map(String::as_str),
            Some("plugin:org.norte.git:git")
        );
    }

    /// `insert` REPLACES: two declarations of the same kind would make
    /// `get` return one and `min_of` the other depending on order, which
    /// is the kind of bug that only shows up when someone adds a kind.
    #[test]
    fn insertar_el_mismo_kind_dos_veces_reemplaza() {
        let mut reg = KindRegistry::builtin();
        reg.insert(KindDecl {
            id: KindId::browser(),
            min: (99, 99),
            focusable: false,
            takes_keys: false,
            multi: false,
            roles: SIN_ROLES,
        });
        assert_eq!(reg.min_of(&KindId::browser()), (99, 99));
        assert!(!reg.holds_role(&KindId::browser(), RoleId::Target));
    }
}
