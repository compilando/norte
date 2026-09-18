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
        // El par «flag si lo hay, esquema si no» lo decide el sitio
        // COMPARTIDO: la ventana lo tenía escrito por su cuenta, que es la
        // forma que tiene una decisión de divergir sin que nadie lo note
        // (ADR 0077).
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
            // El predicado del DESPACHO, entero y sin repetirlo: esta lista
            // rehacía a mano lo que `enter_target` decide (y la ventana lo
            // contestaba de una tercera manera, ADR 0077), pero además lo
            // preguntaba por `selected()`, que sobre la fila `..` contesta
            // `None` — el embudo del operando— y ahí la ayuda atenuaba una
            // tecla que sube. `nav_enter_target` es el que corre al pulsarla,
            // fila de subir incluida.
            enterable: crate::trail::nav_enter_target(self).is_some(),
            viewable: sel.is_some_and(|e| matches!(e.kind, EntryKind::File | EntryKind::Symlink)),
            rename_single: true,
            source_read_only: self.pane_read_only(self.focus),
            // El destino es el del ROL, como en todo lo demás. Sin ninguno
            // designado (tres o más paneles) se contesta por el propio: es un
            // AVISO, y decir «solo lectura» de más no bloquea nada.
            dest_read_only: self
                .pane_read_only(self.target_index().unwrap_or_else(|| self.focus())),
            degraded: self.degraded_for(pane.dir().scheme()).is_some(),
            journalled: self.backend_journalled,
            // Fase 9: los dos impedimentos del relevo, que se deciden UNA vez
            // al arrancar (el brazo del backend no cambia en vida del proceso,
            // y tampoco aparece un escritorio a mitad de sesión).
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

    /// Las caps se cachean por LOCALIZACIÓN, y por la misma razón que el
    /// catálogo de atributos se pide: `fs.capabilities` devuelve las dos
    /// mitades en UNA llamada y la TUI ya la hace para las columnas. Tirar la
    /// mitad de caps y luego sondear otra vez sería pagar dos rondas por un
    /// dato que ya llegó.
    #[test]
    fn las_caps_se_cachean_por_localizacion() {
        let mut app = app_dos_panes();
        let mem = vp("mem:///");
        assert!(app.caps(&mem).is_none(), "sin sembrar, no se inventa nada");
        app.insert_caps(&mem, caps_de_test());
        assert!(app.caps(&mem).is_some());
        assert!(
            app.caps(&vp("sftp://ejemplo.org/")).is_none(),
            "un scheme no responde por otro"
        );
    }

    /// MAJOR-1: `sftp` no es UN sitio. Dos hosts del mismo scheme son dos
    /// backends distintos, y el caché tiene que contarlos aparte o el primero
    /// que contesta decide por todos los demás durante la sesión entera. Hoy
    /// ningún provider del árbol declara `READ_ONLY` por localización (el
    /// archivo y los plugins lo deciden por scheme), así que la clave por
    /// scheme sola no fallaba — por suerte, no por diseño, y `App::caps` es un
    /// accesor general que invita a leer cualquier flag.
    #[test]
    fn dos_authorities_del_mismo_scheme_no_se_responden() {
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
        assert!(app.pane_read_only(0), "a.org dijo que es de solo lectura");
        assert!(
            app.caps(&b).is_none(),
            "a b.org no se le ha preguntado nada todavía"
        );
        assert!(
            !app.pane_read_only(1),
            "b.org no puede heredar el veto de a.org: son dos backends"
        );
    }

    /// Antes de que llegue la primera respuesta, la respuesta honesta es «no
    /// lo sé», y quien pregunta cae al criterio SINTÁCTICO (el scheme dice si
    /// es un archivo comprimido). Lo que no puede hacer es afirmar que se
    /// puede escribir.
    #[test]
    fn sin_caps_todavia_el_solo_lectura_lo_decide_el_scheme() {
        let app = app_dos_panes();
        assert!(!app.pane_read_only(0), "mem:// no es de solo lectura");

        let inside_a_zip = app_en("zip+file:///a.zip/!", "file:///casa");
        assert!(
            inside_a_zip.pane_read_only(0),
            "un scheme de archivo es de solo lectura por construcción"
        );
        assert!(!inside_a_zip.pane_read_only(1));
    }

    /// Cuando las caps SÍ llegaron mandan ellas: un provider que anuncia
    /// `READ_ONLY` sobre un scheme que sintácticamente no lo es (un montaje
    /// remoto en solo lectura) se veta igual.
    #[test]
    fn con_caps_manda_el_flag_read_only() {
        let mut app = app_dos_panes();
        let dir = app.panes[0].dir().clone();
        app.insert_caps(
            &dir,
            norte_proto::Capabilities {
                flags: norte_proto::CapabilityFlags::READ_ONLY,
                max_path: None,
            },
        );
        assert!(app.pane_read_only(0));
        app.insert_caps(&dir, caps_de_test());
        assert!(!app.pane_read_only(0), "sin el flag, escribible");
    }

    /// Los hechos que la ayuda congela salen de los MISMOS predicados que usan
    /// los brazos de `dispatch`: un `.zip` se ENTRA en la TUI (`nav.enter`
    /// compone el scheme) aunque sea un File, y `pane.view` quiere File o
    /// Symlink. Derivarlos otra vez aquí sería atenuar filas que la app
    /// ejecutaría.
    #[test]
    fn los_hechos_de_la_ayuda_siguen_a_los_predicados_del_dispatch() {
        let mut app = app_dos_panes();
        // El cursor está sobre un File normal: no se entra, se ve.
        let f = app.help_facts();
        assert!(!f.enterable, "un fichero cualquiera no se entra");
        assert!(f.viewable);
        assert!(f.rename_single, "shift+F6 renombra UNA: la del cursor");
        assert!(!f.source_read_only && !f.dest_read_only);
        assert!(!f.degraded);

        // Un `.zip` ES entrable en la TUI aunque su kind sea File.
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
            "en la TUI un .zip se entra: la ayuda no puede decir lo contrario"
        );

        // Y la degradación del scheme del pane con foco llega al hecho.
        app.note_degraded(degradacion_de_test("mem", "sin-host"));
        assert!(app.help_facts().degraded);
    }

    /// Y con el cursor sobre `..` la ayuda ofrece `Enter`, que es lo que la
    /// tecla hace ahí: SUBIR.
    ///
    /// El hecho salía de `selected()`, que contesta `None` sobre la fila de
    /// subir a propósito —ese es el embudo que impide que F8 borre el padre—,
    /// así que `enterable` era `false` justo donde nace el cursor después de
    /// cada `cd`. El despacho nunca lo preguntó por ahí: `nav_enter_target`
    /// mira primero `cursor_is_parent_row()`. O sea la trampa de siempre,
    /// «describir» leyendo por la puerta de «operar», y la ayuda atenuaba una
    /// tecla que sube perfectamente.
    #[test]
    fn con_el_cursor_en_la_fila_de_subir_la_ayuda_ofrece_entrar() {
        let mut app = App::new(
            Pane::new(vp("mem:///casa"), vec![file("a")]),
            pane_con(&["b"]),
        );
        app.set_parent_row(true);
        assert!(
            app.focused().cursor_is_parent_row(),
            "la premisa: el cursor nace en `..`"
        );
        assert!(
            crate::trail::nav_enter_target(&app).is_some(),
            "la premisa: la tecla SÍ hace algo aquí"
        );

        assert!(
            app.help_facts().enterable,
            "`Enter` sube desde `..`: la ayuda no puede decir «no aplica a esto»"
        );
    }

    /// Con VARIAS marcas la ayuda NO atenúa shift+F6, porque la TUI lo
    /// ejecuta: `Command::PaneRename` va a `open_rename`, que renombra
    /// `selected()` y no mira las marcas. Atenuarlo sería el fallo exacto que
    /// H3d existe para no cometer — apagar una fila que la app habría corrido,
    /// que enseña al lector a no volver a intentarlo.
    ///
    /// (La GUI sí se niega con selección múltiple, y su menú lo sigue haciendo:
    /// `norte_gui::context_menu`, `renombrar_es_una_sola_entrada_y_la_de_ia_es_otra`.
    /// El hecho es del llamador precisamente porque las dos respuestas son
    /// correctas.)
    #[test]
    fn con_varias_marcas_la_ayuda_no_atenua_renombrar() {
        use norte_help::ChordResolver as _;

        let mut app = App::new(pane_con(&["a", "b", "c"]), pane_con(&["z"]));
        app.focused_mut().toggle_mark_and_advance();
        app.focused_mut().toggle_mark_and_advance();
        assert_eq!(
            app.focused().marked_paths().len(),
            2,
            "hay DOS marcas: el caso que se atenuaba"
        );
        assert!(app.help_facts().rename_single);

        app.freeze_help_facts();
        assert!(
            app.help_chords.availability("pane.rename").is_available(),
            "la TUI renombra la del cursor con marcas puestas: la ayuda no puede negarlo"
        );
        // Y dentro de un archivo sí se apaga, por el backend — el veto real
        // sigue en pie.
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

    /// Congelar los hechos al abrir la ayuda: el resolver que la vista usa
    /// pasa a responder con los hechos de ESE momento.
    #[test]
    fn congelar_los_hechos_reescribe_el_resolver_de_la_ayuda() {
        use norte_help::ChordResolver as _;

        let mut app = app_en("zip+file:///a.zip/!", "zip+file:///b.zip/!");
        assert!(
            app.help_chords.availability("pane.copy").is_available(),
            "antes de congelar el resolver no sabe nada del contexto"
        );
        app.freeze_help_facts();
        assert_eq!(
            app.help_chords.availability("pane.copy").reason(),
            Some(norte_help::Reason::ReadOnlyBackend),
            "los dos panes son de solo lectura: copiar no tiene destino"
        );
    }
}
