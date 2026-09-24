//! The popup selectors: theme, volumes, connections and favorites.
//!
//! Part of `controller`: these are `Estado` methods, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl Estado` split into pieces, so they use
// the same imports as the parent. Enumerating them here would be a
// forty-line list per file, in 32 files, that goes stale the moment the
// parent imports something — `super::*` tracks it on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

impl Estado {
    /// The theme's projection.
    pub(super) fn vista_tema(&self) -> Option<crate::dto::ThemeView> {
        let sel = self.tema_elegido.as_ref()?;
        let mut vista = self.tema.vista();
        vista.choices.clone_from(&sel.nombres);
        vista.cursor = sel.cursor as u64;
        Some(vista)
    }

    /// Opens the volumes selector and REQUESTS the mount table.
    ///
    /// Same as the extensions catalog: it opens saying it is asking, not
    /// waiting.
    pub(super) fn abrir_volumenes(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.abrir_volumenes_en(self.activo(), backend, buzon)
    }

    /// The volumes for the slot on a SIDE of the screen.
    ///
    /// `pane.select-drive-left`/`-right` name a side and not the focus — that
    /// is what `Alt+F1`/`Alt+F2` do — and in a tree of slots the only honest
    /// meaning of "left" is the layout's GEOMETRY: the leftmost visible
    /// listing. With none on that side it is reported, instead of falling
    /// back to the focused one: mounting a volume in the wrong pane is
    /// exactly what this command exists to prevent.
    pub(super) fn abrir_volumenes_de_lado(
        &mut self,
        derecha: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(slot) = self.listado_del_lado(derecha) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-other-slot".to_owned(),
                },
                Vec::new(),
            );
        };
        self.abrir_volumenes_con(
            crate::pickers::Selector::volumenes_de_lado(slot, derecha),
            backend,
            buzon,
        )
    }

    /// The leftmost — or rightmost — visible listing in THIS layout's size.
    ///
    /// Only among the visible ones: a background tab is on no side of the
    /// screen. Ties break by `y` and then by id, so two listings in the
    /// same column always give the same answer.
    pub(super) fn listado_del_lado(&self, derecha: bool) -> Option<u32> {
        let mut candidatos: Vec<(u16, u16, u32)> = self
            .reparto
            .placements
            .iter()
            .filter(|(s, _)| self.huecos.contains_key(&s.0))
            .map(|(s, r)| (r.x, r.y, s.0))
            .collect();
        candidatos.sort_unstable();
        if derecha {
            candidatos.last().map(|(_, _, id)| *id)
        } else {
            candidatos.first().map(|(_, _, id)| *id)
        }
    }

    /// Opens the volumes selector for a specific slot and REQUESTS the
    /// table.
    pub(super) fn abrir_volumenes_en(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.abrir_volumenes_con(crate::pickers::Selector::volumenes(slot), backend, buzon)
    }

    /// The shared body: opens THIS selector and requests the mount table.
    pub(super) fn abrir_volumenes_con(
        &mut self,
        selector: crate::pickers::Selector,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.selector = Some(selector);
        self.gen_selector += 1;
        let apertura = self.gen_selector;
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(PLAZO_PLUGINS, backend.volumes()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::Volumenes(apertura, res))))
                .await;
        });
        let cambio = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// The mount table arrived.
    ///
    /// A failure applies the same way: it stops asking with an empty list,
    /// which already knows how to say itself. And if the selector closed
    /// while it was in flight, there is nothing to do.
    pub(super) fn aplicar_volumenes(
        &mut self,
        apertura: u64,
        res: Result<Vec<norte_proto::methods::Volume>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        // From THIS opening: the generation goes up on open, so an answer
        // from the previous one does not match.
        if apertura != self.gen_selector {
            return None;
        }
        let lang = self.lang;
        let s = self.selector.as_mut()?;
        s.set_volumenes(&res.unwrap_or_default(), lang);
        self.gen_selector += 1;
        let cambio = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        Some(self.parche(vec![cambio]))
    }

    /// `pane.connect` (#264): the configured connections selector.
    ///
    /// The list is given by the DAEMON, not this process: reading
    /// `connections.toml` here would pull russh, opendal, age and the
    /// keyring into a binary that only wants to paint names. Choosing one
    /// NAVIGATES to its URL, and that establishes the session through the
    /// usual path — with its TOFU and its policy.
    pub(super) fn abrir_conexiones(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.selector = Some(crate::pickers::Selector::conexiones(self.activo()));
        self.gen_selector += 1;
        let apertura = self.gen_selector;
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(PLAZO_PLUGINS, backend.connections()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::Conexiones(apertura, res))))
                .await;
        });
        let cambio = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Closes the active pane's session and moves it out of there (#140).
    ///
    /// On a LOCAL pane there is nothing to close and it IS SAID: a key that
    /// answers "done" about something that did nothing teaches you not to
    /// trust the message.
    ///
    /// The destination is decided HERE, before releasing the session,
    /// because afterward the pane's path no longer works as a key.
    pub(super) fn desconectar(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot = self.activo();
        let dir = self.hueco().pane.dir().clone();
        if dir.scheme() == "file" {
            let fuera = self.decir("msg-disconnect-local");
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-disconnect-local".to_owned(),
                },
                fuera,
            );
        }
        let destino = self.donde_volver_tras_desconectar(&dir);
        let backend_c = Arc::clone(backend);
        let buzon_c = buzon.clone();
        let clave = dir.clone();
        tokio::spawn(async move {
            let res = backend_c.close_connection(clave).await;
            let _ = buzon_c
                .send(Mensaje::Fondo(Box::new(Fondo::Desconectada(
                    slot, res, destino,
                ))))
                .await;
        });
        (self.aplicada(), Vec::new())
    }

    /// Where a pane whose session has just closed goes.
    ///
    /// The decision — the trail going backward, skipping the machine that is
    /// closing, and home when nothing is left — lives in `norte-frontend` and
    /// is shared by both frontends: when it lived here, the TUI always went
    /// home and this window retraced its trail, under the same key and the
    /// same name.
    pub(super) fn donde_volver_tras_desconectar(&self, cerrada: &VPath) -> VPath {
        norte_frontend::nav::regreso_tras_desconectar(cerrada, self.hueco().historial.trail())
            .unwrap_or_else(norte_frontend::shell::home_vpath)
    }

    /// The session closed (or there was none): it is reported and the pane
    /// leaves.
    pub(super) fn aplicar_desconexion(
        &mut self,
        slot: u32,
        res: Result<bool, Error>,
        destino: &VPath,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let clave = match res {
            Ok(true) => "msg-disconnect-done",
            // `false` is not a failure: there was no open session. And the
            // pane leaves anyway, because staying there would require
            // reopening it.
            Ok(false) => "msg-disconnect-none",
            Err(e) => {
                // A close that fails does NOT navigate: the pane stays where
                // it was and the session is still alive, which is what the
                // error says.
                return self.decir(norte_frontend::error::error_key(&e));
            }
        };
        self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
        self.navegar_hueco(slot, destino, Trail::Record, backend, buzon)
    }

    /// The connections arrived (#264). Same opening guard as the volumes: an
    /// answer from the previous list does not fill it in.
    pub(super) fn aplicar_conexiones(
        &mut self,
        apertura: u64,
        res: Result<norte_proto::methods::ConnectionListResult, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        if apertura != self.gen_selector {
            return None;
        }
        let s = self.selector.as_mut()?;
        // A failure is painted as an EMPTY list with its own phrase, not as
        // an unexplained list: "you have none" and "could not be asked" are
        // not the same thing, and without the phrase both read alike.
        let (buenas, inservibles) = res.map(|r| (r.connections, r.unusable)).unwrap_or_default();
        s.con_conexiones(buenas, inservibles);
        self.gen_selector += 1;
        let cambio = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        Some(self.parche(vec![cambio]))
    }

    /// The selector's projection.
    pub(super) fn vista_selector(&self) -> Option<crate::dto::PickerView> {
        let mut v = self.selector.as_ref()?.vista(self.lang);
        v.generation = self.gen_selector;
        Some(v)
    }

    /// The keys while the theme is being looked at. It only closes.
    /// Profile selector keys.
    ///
    /// Fixed, like the rest of this window's selectors: arrows to scroll,
    /// `Enter` to choose and `Escape` to close without changing anything.
    pub(super) fn tecla_en_perfiles(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(p) = self.selector_perfil.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match k.key.as_str() {
            "Escape" | "esc" => {
                self.selector_perfil = None;
                let cambio = ViewChange::Profiles { profiles: None };
                return (self.aplicada(), vec![self.parche(vec![cambio])]);
            }
            "ArrowUp" | "up" => p.up(),
            "ArrowDown" | "down" => p.down(),
            "Enter" | "enter" => {
                let elegido = p.chosen().map(std::ffi::OsStr::to_os_string);
                return match elegido {
                    Some(nombre) => {
                        let envios = self.elegir_perfil(&nombre, backend, buzon);
                        (self.aplicada(), envios)
                    }
                    // A row that cannot be loaded changes nothing, and the
                    // selector stays open: closing it would be answering yes
                    // to something that did not happen.
                    None => (
                        ActionAck::Unavailable {
                            reason_key: "host-profile-broken".to_owned(),
                        },
                        Vec::new(),
                    ),
                };
            }
            _ => return (self.aplicada(), Vec::new()),
        }
        let cambio = ViewChange::Profiles {
            profiles: self.vista_perfiles(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Chooses a profile from the list: if it is already the active one,
    /// nothing happens, and if not, the switch begins.
    pub(super) fn elegir_perfil(
        &mut self,
        nombre: &std::ffi::OsStr,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let _ = backend;
        if self.perfil_activo.as_deref() == Some(nombre) {
            // Already on it: it closes and stays quiet. Dropping and
            // reloading the screen only to leave it the same would be work
            // for nothing.
            self.selector_perfil = None;
            return vec![self.parche(vec![ViewChange::Profiles { profiles: None }])];
        }
        self.cambiar_de_perfil(nombre, buzon)
    }

    pub(super) fn tecla_en_tema(
        &mut self,
        k: &crate::keys::KeyInput,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(sel) = self.tema_elegido.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match k.key.as_str() {
            // `Escape` GOES BACK to whatever there was. A selector with a
            // live preview that closes leaving whatever the cursor last
            // brushed against set is not a selector: it is a way to change
            // theme by accident.
            "Escape" | "esc" => {
                let previo = *sel.previo.clone();
                self.tema_elegido = None;
                // The WHOLE theme is restored, its name is not resolved
                // again: the one there was may not be a preset. The
                // notification goes out the same way, so whoever is hosting
                // undoes their side too.
                let nombre = previo.name.clone();
                self.tema = previo;
                self.nativo(crate::dto::NativeEffect::ThemeChanged { name: nombre });
                let cambio = ViewChange::Theme { theme: None };
                // And the rows: the live preview left the names with the
                // colors of whichever theme the cursor brushed, and going
                // back has to bring those back too (bridge 66).
                let mut fuera = vec![self.parche(vec![cambio])];
                fuera.extend(self.parches_de_filas_de_todos());
                return (self.aplicada(), fuera);
            }
            "ArrowUp" | "up" => sel.cursor = sel.cursor.saturating_sub(1),
            "ArrowDown" | "down" => {
                sel.cursor = (sel.cursor + 1).min(sel.nombres.len().saturating_sub(1));
            }
            "Enter" | "enter" => {
                let Some(elegido) = sel.nombres.get(sel.cursor).cloned() else {
                    return (self.aplicada(), Vec::new());
                };
                self.tema_elegido = None;
                self.aplicar_tema(&elegido, buzon);
                // And it IS SAVED, which is what separates choosing a theme
                // from just looking at it. To the active profile if there is
                // one: writing it to the user layer while a profile pins its
                // own leaves it shadowed (ADR 0079 D1).
                self.persistir_tema(&elegido, buzon);
                let cambio = ViewChange::Theme { theme: None };
                let mut fuera = vec![self.parche(vec![cambio])];
                fuera.extend(self.parches_de_filas_de_todos());
                return (self.aplicada(), fuera);
            }
            _ => return (self.aplicada(), Vec::new()),
        }
        // LIVE preview: moving through the list shows the theme, not its
        // name.
        let bajo_el_cursor = sel.nombres.get(sel.cursor).cloned();
        if let Some(nombre) = bajo_el_cursor {
            self.aplicar_tema(&nombre, buzon);
        }
        let cambio = ViewChange::Theme {
            theme: self.vista_tema(),
        };
        // The preview moves ENTRIES' colors the same as chrome's (bridge 66):
        // without this, scrolling the list changed the background and
        // borders under the cursor but left the names in the previous
        // theme's colors, which is half of the comparison the selector
        // exists to offer.
        let mut fuera = vec![self.parche(vec![cambio])];
        fuera.extend(self.parches_de_filas_de_todos());
        (self.aplicada(), fuera)
    }

    /// Sets a theme by its name: the host's, and the one hosting it.
    ///
    /// Both, and that is why this is in one place instead of two: the host
    /// keeps the colors for its own theme screen, and whoever hosts it has
    /// to resolve its own again — the webview's CSS variables — because it
    /// resolved them once at startup. A name that does not exist leaves the
    /// theme as it was instead of leaving the screen without colors.
    ///
    /// A PRESET is applied right here; a PATH goes off to be read.
    ///
    /// The split is decided by `norte_frontend::theme::is_preset`, shared by
    /// both frontends: resolving a preset is arithmetic over colors and
    /// sending it to another thread would add a frame of lag to something
    /// the reader sees changing under the cursor, while reading a file
    /// inside the actor is rule 2 broken — and with a theme on a dropped
    /// mount it freezes the whole window.
    ///
    /// It used to be that only presets were applied. The selector offers
    /// presets, so through that door it made no difference; through the
    /// PROFILE SWITCH door it did, because a profile can carry `theme =
    /// "…/mine.toml"` (ADR 0020) and that used to go silently unapplied,
    /// with the terminal applying it.
    pub(super) fn aplicar_tema(&mut self, nombre: &str, buzon: &mpsc::Sender<Mensaje>) {
        if let Ok(Some(tema)) = norte_theme::Theme::preset(nombre) {
            self.tema_puesto(nombre, &tema);
            return;
        }
        let spec = nombre.to_owned();
        let buzon = buzon.clone();
        tokio::task::spawn_blocking(move || {
            let resuelto = norte_frontend::theme::resolve_theme(Some(&spec));
            // The error does NOT travel: it carries the spec inside, which
            // is a path, and what the status bar says comes from the
            // catalog (#73). The key says whether the file could not be
            // read or does not validate, which is the actionable part.
            let salida = resuelto.map_err(|e| match e {
                norte_frontend::theme::ResolveError::Io { .. } => "host-theme-unreadable",
                norte_frontend::theme::ResolveError::Parse { .. } => "host-theme-invalid",
            });
            let _ = buzon.blocking_send(Mensaje::TemaResuelto(Box::new((spec, salida))));
        });
    }

    /// The already-resolved theme becomes the active one, and whoever hosts
    /// it is told.
    pub(super) fn tema_puesto(&mut self, nombre: &str, tema: &norte_theme::Theme) {
        self.tema = crate::pickers::HostTheme::de(nombre, tema);
        self.nativo(crate::dto::NativeEffect::ThemeChanged {
            name: nombre.to_owned(),
        });
    }

    /// The ROW patches for every slot, for after a theme change.
    ///
    /// Since bridge 66 an entry's color is BAKED into its row
    /// (`RowView::name_color`), so changing theme and not repainting the
    /// rows leaves the names in the previous theme's colors until the
    /// reader moves, marks something or changes directory. The rest of the
    /// screen — background, borders, palette, status bar — does change,
    /// which is what makes the bug so odd to read: half the window obeys
    /// and the other half does not.
    ///
    /// For ALL slots and not just the active one: `filas_visibles` looks at
    /// `self.hueco()`, and the pane next door also has names.
    ///
    /// Lives here, next to `tema_puesto`, because the two paths that set a
    /// theme both go through it: the preset one, which resolves on the spot,
    /// and the file one, which arrives through `Mensaje::TemaResuelto`. It
    /// is the same bug `readornar_todo` fixed for the icons.
    pub(super) fn parches_de_filas_de_todos(&mut self) -> Vec<BridgeEnvelope<UiUpdate>> {
        let slots: Vec<u32> = self.huecos.keys().copied().collect();
        slots
            .into_iter()
            .map(|slot| self.parche_filas_de(slot))
            .collect()
    }

    /// Saves the chosen theme to the right layer, OUTSIDE the actor.
    ///
    /// The actor is the only writer of the state and this is I/O with an
    /// inter-process lock behind it (`persist_ui_theme_to` blocks while
    /// another norte is writing): doing it here would freeze the whole
    /// window. It comes back through the mailbox like everything else.
    pub(super) fn persistir_tema(&mut self, nombre: &str, buzon: &mpsc::Sender<Mensaje>) {
        let Some(dir) = self.dir_de_escritura() else {
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "host-no-config-dir",
            )));
            return;
        };
        let nombre = nombre.to_owned();
        let buzon = buzon.clone();
        tokio::task::spawn_blocking(move || {
            let clave = match norte_config::persist_ui_theme_to(&dir, &nombre) {
                Ok(_) => None,
                // The error does NOT travel: it can carry the file's path,
                // and what the status bar says comes from the catalog (#73).
                // The category is enough to know what happened.
                Err(e) => Some(clave_de_io(&e)),
            };
            let _ = buzon.blocking_send(Mensaje::TemaPersistido(clave));
        });
    }

    /// Where this window writes its configuration.
    ///
    /// The HIGHEST of the layers that can be edited: the active profile if
    /// there is one, and the user's if not. Never the system's (it is not
    /// the one in front of it) nor the project's (it belongs to the
    /// directory, not to the person).
    ///
    /// Comes from the layers whoever started the host resolved, not from
    /// looking at the environment again: the window writes where it really
    /// read from (ADR 0066 D14). And the profile is the one currently SET,
    /// not the one at startup: startup's layers only carry one if it started
    /// with `--profile`, and a profile chosen live does not touch them.
    /// Without this, with a profile set from the selector, the setting was
    /// written to the user layer and the profile shadowed it on re-read:
    /// "saved" and with no effect, silently.
    pub(super) fn dir_de_escritura(&self) -> Option<std::path::PathBuf> {
        use crate::settings::ConfigLayer;
        let usuario = self
            .paths
            .config_layers
            .iter()
            .find(|(capa, _)| matches!(capa, ConfigLayer::User))
            .map(|(_, p)| p.path.clone());
        match (&self.perfil_activo, usuario) {
            (Some(perfil), Some(usuario)) => Some(usuario.join("profiles").join(perfil)),
            (None, usuario) => usuario,
            // Profile set with no user layer to hang it off: it can only
            // have come from startup, and then it is in the layers.
            (Some(_), None) => self
                .paths
                .config_layers
                .iter()
                .rfind(|(capa, _)| matches!(capa, ConfigLayer::Profile))
                .map(|(_, p)| p.path.clone()),
        }
    }

    /// Asks for a new favorite's NAME pointing to the pane's directory
    /// (#309), with the field already prefilled.
    ///
    /// The suggestion comes from the SHARED model
    /// (`norte_frontend::places::suggested_hotlist_name`), the one the
    /// terminal uses: it dodges taken names because saving REPLACES the
    /// favorite already named that, and with the field prefilled the reflex
    /// of accepting without reading would overwrite one pointing somewhere
    /// else. A TYPED name that collides still replaces — that is what was
    /// asked for.
    pub(super) fn pedir_favorito(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let destino = self.hueco().pane.dir().clone();
        self.pedir_favorito_de(destino)
    }

    /// The same, for a directory that need not be the pane's: a history
    /// list's cursor row (spec 2026-09-15 D2).
    pub(super) fn pedir_favorito_de(
        &mut self,
        destino: VPath,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let ocupados: Vec<&str> = self
            .config
            .common
            .hotlist
            .iter()
            .map(|h| h.name.as_str())
            .collect();
        let sugerido = norte_frontend::places::suggested_hotlist_name(&destino, &ocupados);
        let location_line = Self::linea_de_ruta(&destino);
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-hotlist-name-title".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![location_line],
            overflow_note: String::new(),
            overflow_hostile: false,
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    destructive: false,
                },
                DialogChoice {
                    id: "cancel".to_owned(),
                    label_key: "dialog-cancel".to_owned(),
                    destructive: false,
                },
            ],
            input: Some(clamp_display(sugerido.clone())),
            input_hostile: false,
            input_secret: false,
            fields: Vec::new(),
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id,
            vista,
            tecleado: Tecleado::Texto(sugerido),
            reconocido: true,
            al_confirmar: Some(Pendiente::GuardarFavorito { destino }),
        });
        // The selector closes: the dialog answers the question, and leaving
        // the list underneath would give two live cursors at once.
        self.selector = None;
        let cambios = vec![
            ViewChange::Picker { picker: None },
            ViewChange::Dialogs {
                dialogs: self.vistas_de_dialogos(),
            },
        ];
        (self.aplicada(), vec![self.parche(cambios)])
    }

    /// Saves the favorite `nombre` = `destino` in the config layer this
    /// window writes (#309).
    ///
    /// Through `spawn_blocking` (rule 2): `persist_hotlist_add` writes a file
    /// with a lock and tmp+rename. The in-memory copy is only touched if the
    /// disk went well — which is what the terminal does, and for the same
    /// reason: a list claiming to have a favorite that is not in the file
    /// lies until the next startup.
    pub(super) fn guardar_favorito(
        &mut self,
        destino: &VPath,
        nombre: &str,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let nombre = nombre.trim().to_owned();
        if nombre.is_empty() {
            // No name, no favorite: it is what the terminal does with an
            // empty field, and it is more honest than saving one with no
            // name.
            return (Some("hotlist-name-empty"), self.decir("hotlist-name-empty"));
        }
        let Some(dir) = self.dir_de_escritura() else {
            return (Some("host-no-config-dir"), self.decir("host-no-config-dir"));
        };
        let wire = destino.to_wire();
        let a_donde = destino.clone();
        let buzon = buzon.clone();
        let n = nombre.clone();
        tokio::task::spawn_blocking(move || {
            let clave = match norte_config::persist_hotlist_add(&dir, &n, &wire) {
                Ok(_) => None,
                Err(e) => Some(clave_de_io(&e)),
            };
            let _ = buzon.blocking_send(Mensaje::FavoritoPersistido(Box::new((n, a_donde, clave))));
        });
        (None, Vec::new())
    }

    /// Asks for the NAME to save the workspace under (#318).
    ///
    /// Prefilled with the ACTIVE profile, which is what a "save as" does
    /// everywhere: the normal thing is to start from what you have and give
    /// it another name. With no active profile the field is born empty —
    /// inventing one would be proposing a directory the reader never asked
    /// for.
    pub(super) fn pedir_guardar_perfil(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let sugerido = self
            .perfil_activo
            .as_ref()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            // The TERMINAL's keys, not new ones: it is the same dialog, and
            // Fluent keeps the FIRST definition — a duplicate key with
            // different text leaves the old one dead without saying so.
            title_key: "modal-profile-save-as".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![crate::dto::DialogLine {
                text: clamp_display(norte_i18n::t_in(self.lang, "modal-profile-save-as-hint")),
                hostile: false,
            }],
            overflow_note: String::new(),
            overflow_hostile: false,
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    destructive: false,
                },
                DialogChoice {
                    id: "cancel".to_owned(),
                    label_key: "dialog-cancel".to_owned(),
                    destructive: false,
                },
            ],
            input: Some(clamp_display(sugerido.clone())),
            input_hostile: false,
            input_secret: false,
            fields: Vec::new(),
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id,
            vista,
            tecleado: Tecleado::Texto(sugerido),
            reconocido: true,
            al_confirmar: Some(Pendiente::GuardarPerfil),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Writes `profiles/<name>/` with what is on screen (#318).
    ///
    /// The CONTENT is not decided here: it is assembled by
    /// [`Self::instantanea_de_perfil`] and written by
    /// `norte_config::save_profile`, the same writer the terminal uses. It
    /// is ADR 0077's requirement — a decision duplicated between frontends
    /// silently drifts apart — and this would be the worst place for it to
    /// drift: two "save as" that produce different profiles turn the
    /// profile into something that depends on where you saved it from.
    ///
    /// The name is validated BEFORE touching disk, and the disk part
    /// happens outside the actor.
    pub(super) fn guardar_perfil(
        &mut self,
        nombre: &str,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let nombre = std::ffi::OsString::from(nombre.trim());
        if !norte_config::valid_profile_name(&nombre) {
            return (
                Some("msg-profile-name-invalid"),
                self.decir("msg-profile-name-invalid"),
            );
        }
        let Some(dir) = self.dir_de_perfiles() else {
            return (Some("host-no-config-dir"), self.decir("host-no-config-dir"));
        };
        let snap = self.instantanea_de_perfil();
        let buzon = buzon.clone();
        let visible = nombre.to_string_lossy().into_owned();
        tokio::task::spawn_blocking(move || {
            let clave = match norte_config::save_profile(&dir, &nombre, &snap) {
                Ok(_) => None,
                Err(e) => Some(clave_de_io(&e)),
            };
            let _ = buzon.blocking_send(Mensaje::PerfilGuardado(Box::new((visible, clave))));
        });
        (None, Vec::new())
    }

    /// What is on screen, in the shape `norte-config` writes (#318).
    ///
    /// The CONTENT is decided by `norte_frontend::config::profile_snapshot`,
    /// the same one the terminal calls: here it only answers where each
    /// listing is. See its rustdoc for why there are not two copies of this.
    pub(super) fn instantanea_de_perfil(&self) -> norte_config::ProfileSnapshot {
        norte_frontend::config::profile_snapshot(
            &self.arbol,
            // Only the slots that ARE a listing have a directory, and those
            // are the ones `huecos` stores: the viewer, processes and
            // places have nothing to put in `[profile.start]`.
            &|SlotId(n)| self.huecos.get(&n).map(|h| h.pane.dir().clone()),
            self.dir_de_escritura()
                .and_then(|d| std::fs::read(d.join("keymap.toml")).ok()),
        )
    }

    /// Removes the favorite the cursor points at (#309).
    ///
    /// No confirmation, like in the terminal: a favorite is a shortcut, not
    /// a file, and creating it again costs one key. The name comes RAW from
    /// the row and not from its label, which is sanitized.
    pub(super) fn quitar_favorito(
        &mut self,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(nombre) = self
            .selector
            .as_ref()
            .and_then(|s| s.nombre_crudo())
            .map(str::to_owned)
        else {
            return (
                ActionAck::Unavailable {
                    reason_key: "picker-hotlist-empty".to_owned(),
                },
                Vec::new(),
            );
        };
        let Some(dir) = self.dir_de_escritura() else {
            let fuera = self.decir("host-no-config-dir");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-config-dir".to_owned(),
                },
                fuera,
            );
        };
        let buzon = buzon.clone();
        let n = nombre;
        tokio::task::spawn_blocking(move || {
            let clave = match norte_config::persist_hotlist_remove(&dir, &n) {
                Ok(_) => None,
                Err(e) => Some(clave_de_io(&e)),
            };
            let _ = buzon.blocking_send(Mensaje::FavoritoQuitado(Box::new((n, clave))));
        });
        (self.aplicada(), Vec::new())
    }

    /// The disk answered a saved favorite (#309): it is reflected or
    /// reported. The profile got written, or not (#318).
    ///
    /// It does not activate on its own: saving is saving, and switching
    /// profile is something else with its own key. The terminal does the
    /// same, and the parity test requires it.
    pub(super) fn perfil_guardado(
        &mut self,
        nombre: &str,
        fallo: Option<&'static str>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if let Some(clave) = fallo {
            return self.decir(clave);
        }
        self.status.message = Some(clamp_display(norte_i18n::ta_in(
            self.lang,
            "msg-profile-saved",
            &[("name", &clamp_display(nombre.to_owned()))],
        )));
        let cambio = ViewChange::Status(self.status.clone());
        vec![self.parche(vec![cambio])]
    }

    pub(super) fn favorito_persistido(
        &mut self,
        nombre: &str,
        destino: Option<VPath>,
        fallo: Option<&'static str>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if let Some(clave) = fallo {
            return self.decir(clave);
        }
        match destino {
            Some(destino) => {
                // REPLACES the one with the same name, which is what the
                // file does: if the in-memory copy added one more, the list
                // would show two where the disk has one.
                self.config.common.hotlist.retain(|h| h.name != nombre);
                self.config.common.hotlist.push(norte_config::HotlistItem {
                    name: nombre.to_owned(),
                    target: Ok(destino),
                });
            }
            None => self.config.common.hotlist.retain(|h| h.name != nombre),
        }
        // The side panel paints the favorites: it is re-seeded from the copy
        // that just changed, and that bumps its generation. Without this the
        // places list would keep showing the old one.
        self.sembrar_sitios();
        // And the selector, if it is still open, is rebuilt with the new
        // list: it is the very surface being edited from, and leaving it
        // unchanged would be answering "done" over a list that does not
        // show it.
        if self
            .selector
            .as_ref()
            .is_some_and(crate::pickers::Selector::es_hotlist)
        {
            let slot = self.activo();
            let favoritos: Vec<(String, Result<VPath, String>)> = self
                .config
                .common
                .hotlist
                .iter()
                .map(|h| (h.name.clone(), h.target.clone()))
                .collect();
            self.selector = Some(crate::pickers::Selector::hotlist(
                slot, &favoritos, self.lang,
            ));
            self.gen_selector += 1;
        }
        // A whole SNAPSHOT, same as when the volumes arrive and for the same
        // reason: this changes two surfaces at once — the side panel and the
        // selector — and the rows get renumbered, so an index-based patch
        // would name rows that are no longer what they were.
        let snap = self.snapshot();
        vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))]
    }

    /// The keys while a selector is open.
    pub(super) fn tecla_en_selector(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        /// How many rows a page moves.
        const PAGINA: i64 = 10;
        if self.selector.is_none() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        // While filtering a history, text keys belong to the filter.
        if let Some(salida) = self.tecla_de_filtro(k) {
            return salida;
        }
        // `Home`/`End` stay fixed keys: the shared catalog has no verb for
        // "to the start" inside a dialog, and waiting for it to have one
        // would have left the list with no extremes.
        let extremo = match k.key.as_str() {
            "Home" | "home" => Some(i64::MIN / 2),
            "End" | "end" => Some(i64::MAX / 2),
            _ => None,
        };
        let verbo = if extremo.is_some() {
            None
        } else {
            self.verbo_de_dialogo(k)
        };
        let Some(s) = self.selector.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        if let Some(salto) = extremo {
            s.mover(salto);
        } else {
            match verbo.as_deref() {
                Some("dialog.cancel") => self.selector = None,
                Some("dialog.down") => s.mover(1),
                Some("dialog.up") => s.mover(-1),
                Some("dialog.page-down") => s.mover(PAGINA),
                Some("dialog.page-up") => s.mover(-PAGINA),
                Some("dialog.confirm") => return self.elegir_del_selector(backend, buzon),
                // Favorites are the only list in this window that gets
                // EDITED (#309), and they are the two verbs the catalog
                // already had for that: in the terminal they are `a` and `d`
                // on the same popup. On any other selector they mean nothing
                // and are ignored, like any key that selector does not bind.
                Some("dialog.add") if s.es_hotlist() => return self.pedir_favorito(),
                Some("dialog.remove") if s.es_hotlist() => return self.quitar_favorito(buzon),
                // History and frequent are also edited (spec 2026-09-15 D2),
                // and opening in the other slot works for any navigating
                // list.
                Some("dialog.remove") if s.es_historia() => return self.quitar_de_historia(),
                Some("dialog.clear") if s.es_historia() => return self.vaciar_historia(),
                Some("dialog.filter") if s.es_historia() => {
                    return self.filtrar_historia(Some(String::new()));
                }
                // `a` over a history row saves it as a favorite.
                Some("dialog.add") if s.es_historia() => {
                    return match s.elegir() {
                        Some(destino) => self.pedir_favorito_de(destino),
                        None => (self.aplicada(), Vec::new()),
                    };
                }
                Some("dialog.confirm-other") => {
                    return self.elegir_del_selector_en_otro(backend, buzon);
                }
                _ => return (self.aplicada(), Vec::new()),
            }
        }
        let cambio = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Choosing from the selector: navigate to the volume.
    ///
    /// Navigating is READING, so the volume DOES open — unlike a connection,
    /// which this window does not even enumerate yet.
    ///
    /// The close travels in its OWN patch and before the navigation, like
    /// the palette's and for the same reason: a renderer applying patches
    /// would end up with the selector painted over the new listing.
    pub(super) fn elegir_del_selector(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(s) = self.selector.as_ref() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // The slot was said by the selector when it OPENED:
        // `pane.select-drive-left` names a side, and reading focus here
        // would let moving it with the list up mount the volume in another
        // pane.
        let slot = s.slot();
        if !self.huecos.contains_key(&slot) || self.oculto(slot) {
            // The layout changed with the list up: the slot the selector
            // captured on opening is no longer there, or stopped being
            // visible. Navigating there would bring a listing nobody is
            // going to look at — against "what is not seen is not fetched"
            // — or would do nothing and silently close the selector.
            self.selector = None;
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-other-slot".to_owned(),
                },
                vec![self.parche(vec![ViewChange::Picker { picker: None }])],
            );
        }
        let Some(destino) = s.elegir() else {
            if s.hay_fila() {
                // There is a row and it goes nowhere: a favorite whose path
                // does not parse. It IS REPORTED, which is what the row was
                // already warning about.
                return (
                    ActionAck::Unavailable {
                        reason_key: "hotlist-invalid".to_owned(),
                    },
                    Vec::new(),
                );
            }
            // No rows yet (or the table came back empty): nowhere to go.
            return (self.aplicada(), Vec::new());
        };
        self.selector = None;
        let cierre = self.parche(vec![ViewChange::Picker { picker: None }]);
        let mut envios = vec![cierre];
        envios.extend(self.navegar_hueco(slot, &destino, Trail::Record, backend, buzon));
        (self.aplicada(), envios)
    }

    /// A click on a selector row: chooses it.
    pub(super) fn elegir_fila_del_selector(
        &mut self,
        row: u32,
        generation: u64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if generation != self.gen_selector {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let Some(s) = self.selector.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        s.senalar(row as usize);
        let cambio = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// The connection to the daemon changed state: it is PAINTED and
    /// REPORTED.
    ///
    /// Both things, through the same queue: losing the daemon halfway
    /// through an operation cannot be noticed only in an icon.
    pub(super) fn cambio_de_conexion(
        &mut self,
        ev: norte_client::ConnEvent,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let (vista, clave) = match ev {
            norte_client::ConnEvent::Restored => (ConnectionView::Connected, "msg-daemon-restored"),
            // The daemon warns BEFORE closing, and this is the only thing
            // that tells a handoff apart from a stop: as soon as the
            // connection drops, the two look the same. It stays as a
            // PERSISTENT notice because it stays true for as long as it
            // lasts, and the connection view is not touched yet — the
            // connection, right now, is still up.
            norte_client::ConnEvent::GoingAway { reconnect } => {
                self.aviso_de_daemon = Some(if reconnect {
                    "msg-daemon-handover"
                } else {
                    "msg-daemon-stopping"
                });
                let clave = self.aviso_de_daemon.unwrap_or("msg-daemon-stopping");
                let banners = self.cambio_de_banners();
                let parche = self.parche(vec![banners]);
                let aviso = self.sobre(UiUpdate::Notice(UiNotice::Message {
                    key: clave.to_owned(),
                    detail: None,
                }));
                return vec![parche, aviso];
            }
            // `Lost` and the wildcard together: `ConnEvent` is NOT
            // exhaustive, and an event from a newer SDK reads as a loss,
            // which is the conservative choice — it is painted as
            // reconnecting instead of pretending everything is still the
            // same.
            norte_client::ConnEvent::Lost | _ => (ConnectionView::Reconnecting, "msg-daemon-lost"),
        };
        // Coming back TURNS OFF the notice: one that does not know how to
        // become "all clear" lies the moment the daemon reappears, and the
        // handoff ends up coming back.
        // And it starts a new EPOCH: on the other side there can be a NEW
        // daemon, with its id counter starting from 1. Whatever is left on
        // the board with those numbers belongs to before, and from here on
        // nothing of its is inherited.
        if matches!(ev, norte_client::ConnEvent::Restored) {
            self.aviso_de_daemon = None;
            self.epoca_conexion = self.epoca_conexion.saturating_add(1);
            // The light bar stops counting the previous daemon's work
            // (ADR 0146): without this its burst would never close.
            self.anotar_tira(buzon);
            // "Go to" (#357): whatever the PREVIOUS daemon answers with —
            // its connections, its index — no longer belongs to this one. A
            // new opening invalidates it.
            self.gen_ir_a = self.gen_ir_a.saturating_add(1);
            // A plan requested from the PREVIOUS daemon is not going to be
            // answered by the new one: its id starts over at 1, and leaving
            // the request hanging would make the panel open with someone
            // else's Task.
            if let Some(pedida) = self.sync_pedida.take() {
                pedida
                    .abandonada
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        self.conexion = vista.clone();
        // And the pane that opened only for that work closes.
        let mut envios = self.procesos_automaticos(backend, buzon);
        let banners = self.cambio_de_banners();
        let parche = self.parche(vec![ViewChange::Connection(vista), banners]);
        let aviso = self.sobre(UiUpdate::Notice(UiNotice::Message {
            key: clave.to_owned(),
            detail: None,
        }));
        envios.extend([parche, aviso]);
        envios
    }

    /// A provider session travels unencrypted (#44): it is noted and
    /// reported.
    ///
    /// Persistent and not ephemeral: a message the next key erases cannot
    /// describe how what is being looked at is traveling. The phrase and the
    /// cap are set by `norte_frontend::banners`, the SAME code that composes
    /// the TUI's notice.
    pub(super) fn sesion_degradada(
        &mut self,
        d: norte_proto::methods::ConnectionDegraded,
    ) -> BridgeEnvelope<UiUpdate> {
        self.degradadas.note(d);
        let cambio = self.cambio_de_banners();
        self.parche(vec![cambio])
    }

    /// The connection asks for its password (#325/#327): the dialog opens.
    ///
    /// The question names the connection **and where it connects to**, and
    /// that second line is the point: the name was chosen by a config file,
    /// and a file can arrive from someone else's dotfiles or from an edited
    /// line, so "work" says nothing about whether that entry still points
    /// where it pointed yesterday. It is the same reason the host key
    /// dialog shows a fingerprint. Each part in its OWN FIELD, never
    /// interpolated into the sentence.
    ///
    /// The field is born empty and confirming over it is INERT (see
    /// `ejecutar_pendiente`): delivering the empty string reproduces #320,
    /// where an empty secret made the connection authenticate with the
    /// ambient string — an identity nobody asked for.
    pub(super) fn pedir_secreto(
        &mut self,
        conn: String,
        endpoint: &str,
        slot: u32,
        dir: VPath,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // ONE question per connection. Two panes over the same `prompt`
        // entry — or a refresh while the dialog is up front — used to stack
        // another identical question, with its own empty field; and under
        // enough of those, `apilar_dialogo`'s cap-based eviction sweeps away
        // unacknowledged agent APPROVALS, which are the first thing it
        // sacrifices.
        if self.dialogos.iter().any(|d| {
            matches!(&d.al_confirmar, Some(Pendiente::EntregarSecreto { conn: c, .. }) if *c == conn)
        }) {
            return Vec::new();
        }
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        // Both come from the CORE, not from the remote server, but they are
        // masked the same way: the name comes from a file and the endpoint
        // from a URL, and neither origin is trustworthy for what is
        // painted.
        let linea = |s: &str| {
            let (pintable, hostil) = norte_frontend::display_name(s.as_bytes());
            crate::dto::DialogLine {
                text: clamp_display(pintable),
                hostile: hostil,
            }
        };
        let vista = DialogView {
            id,
            title_key: "modal-ask-secret-title".to_owned(),
            // WHERE the password is going. In `destination` and not in the
            // body, per what that field's rustdoc says: a separator inside
            // the text could be written by the data itself.
            destination: Some(linea(endpoint)),
            // WHICH entry is asking for it.
            subject: Some(linea(&conn)),
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            // Where what is typed ends up, and it is the same phrase the TUI
            // says: the secret lives in the daemon's memory until it stops,
            // and it is never written to any file. Whoever is about to type
            // a password has the right to know that BEFORE.
            //
            // `hostile: false` because it is a catalog phrase, not data: it
            // has not gone through `display_name` because it does not come
            // from outside.
            body: vec![crate::dto::DialogLine {
                text: clamp_display(norte_i18n::t_in(self.lang, "modal-ask-secret-note")),
                hostile: false,
            }],
            overflow_note: String::new(),
            overflow_hostile: false,
            choices: vec![
                crate::dto::DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    // Not destructive: it deletes nothing. What makes it
                    // sensitive — a secret goes out — is not what that flag
                    // means, and using it here would devalue a delete's.
                    destructive: false,
                },
                crate::dto::DialogChoice {
                    id: "cancel".to_owned(),
                    label_key: "dialog-cancel".to_owned(),
                    destructive: false,
                },
            ],
            // Empty, and with `Some`: that is what tells the renderer typing
            // HAPPENS here. Whatever travels through here will always be
            // dots.
            input: Some(String::new()),
            input_hostile: false,
            input_secret: true,
            fields: Vec::new(),
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        let mut fuera = self.apilar_dialogo(Dialogo {
            id,
            vista,
            tecleado: Tecleado::Secreto,
            // `true`: this was opened by a GESTURE of the reader's — the
            // navigation they just made — so the next answer already IS an
            // answer. The "I already see it" rule is for what appears
            // without anyone asking for it (an agent approval, a batch's
            // report), and here the question was asked by whoever is in
            // front. It is the same thing the TUI does, where Enter answers
            // directly.
            //
            // And it opens no gap: confirming without typing anything is
            // inert, so the worst case of a finger getting ahead of itself
            // is doing nothing.
            reconocido: true,
            al_confirmar: Some(Pendiente::EntregarSecreto { conn, slot, dir }),
        });
        // `apilar_dialogo` only stacks — and returns what fell off the cap —
        // the patch that PAINTS it is sent by whoever opens it, like
        // everything else.
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        fuera.push(self.parche(vec![cambio]));
        fuera
    }

    /// The secret was delivered (or not): the navigation is retried, or it
    /// is reported.
    ///
    /// The retry is of THAT navigation — its slot and its destination —
    /// which is what the pending action carries. If delivering it failed,
    /// there is no retry: the slot stays as the error left it and the status
    /// bar counts it.
    pub(super) fn secreto_entregado(
        &mut self,
        slot: u32,
        dir: &VPath,
        res: Result<(), Error>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        match res {
            // `Record` and not `Replay`, even though this is a retry: a
            // `Replay` needs a DIRECTION for the trail, and there is none
            // here — this navigation could have been born from a key, a
            // favorite or a `back`, and the error did not carry it.
            // Inventing one would be worse than not having it.
            //
            // And it does not duplicate: the failed listing left the slot
            // SHOWING the directory it could not enter, so on retry
            // `anterior == destino` and `navegar_hueco` records nothing.
            Ok(()) => self.navegar_hueco(slot, dir, Trail::Record, backend, buzon),
            Err(e) => self.decir(norte_frontend::error::error_key(&e)),
        }
    }

    /// A connection could NOT be opened, and why (#322).
    ///
    /// A `Notice` and not a persistent banner, unlike degradation:
    /// degradation describes a session that exists and keeps existing while
    /// it is being looked at; this describes an attempt that has already
    /// ended, and a permanent indicator over something that is not open
    /// would never turn off.
    ///
    /// The line is composed by `norte_frontend::banners::failure_line`, the
    /// SAME code the TUI uses: two phrases about why a machine could not be
    /// entered silently drift apart, which is exactly what ADR 0077 exists
    /// to prevent.
    ///
    /// **The order with the listing's error is NOT guaranteed here.** In the
    /// TUI it is (the handler holds the `select!` while waiting, so the
    /// category arrives first and this phrase overwrites it); in the window
    /// they are two independent producers against the same mailbox, and the
    /// generic category can be processed AFTER. It is accepted: both texts
    /// describe the same failure and neither is wrong. If it ever matters,
    /// it has to be made explicit instead of trusting the scheduler.
    /// Returns TWO things, and the patch is the one that is seen: the
    /// renderer only listens to `fatal`-class `Notice`s, and its status text
    /// comes from `status.message`, which only moves with a patch. Sending
    /// only the notice left the window painting nothing — with the test
    /// green, because it asserted on the bridge's envelope and not on the
    /// state. It is the same pattern as `cambio_de_conexion`, which already
    /// returns `vec![parche, aviso]`.
    pub(super) fn conexion_fallida(
        &mut self,
        f: &norte_proto::methods::ConnectionFailed,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let linea = norte_frontend::banners::failure_line(self.lang, f);
        self.status.message = Some(clamp_display(linea.clone()));
        let parche = self.parche(vec![ViewChange::Status(self.status.clone())]);
        let aviso = self.sobre(UiUpdate::Notice(UiNotice::Message {
            key: "status-connection-failed".to_owned(),
            detail: Some(linea),
        }));
        vec![parche, aviso]
    }

    /// A `plugin.notice` (0.69.0, ADR 0100): a hook's phrase, attributed to
    /// the plugin, as a status message — the same patch + notice pair as
    /// [`Self::conexion_fallida`], and for the same reason. An unknown
    /// class with no text paints nothing.
    pub(super) fn aviso_de_plugin(
        &mut self,
        n: &norte_proto::methods::PluginNotice,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(linea) = norte_frontend::banners::plugin_notice_line(self.lang, n) else {
            return Vec::new();
        };
        self.status.message = Some(clamp_display(linea.clone()));
        let parche = self.parche(vec![ViewChange::Status(self.status.clone())]);
        // The key follows `kind`: a renderer listening to `Notice`s by key
        // tells a hook's phrase apart from the notice that it turned off.
        let key = match n.kind.as_str() {
            "hooks-disabled" => "msg-plugin-hooks-disabled",
            "effect-denied" => "msg-plugin-effect-denied",
            _ => "msg-plugin-notice",
        };
        let aviso = self.sobre(UiUpdate::Notice(UiNotice::Message {
            key: key.to_owned(),
            detail: Some(linea),
        }));
        vec![parche, aviso]
    }

    /// Rebuilds the status bar's persistent notices and returns its change.
    ///
    /// ONE spot for all three, in this order: the journal talks about the
    /// WHOLE session and about whether anything can be undone, the daemon
    /// about whether this window is going to keep being served, and
    /// degradation about how a connection travels. Picking only one would
    /// hide the others forever, which is exactly what the TUI already
    /// decided not to do.
    pub(super) fn cambio_de_banners(&mut self) -> ViewChange {
        let frase = |clave: &str| crate::dto::BannerView {
            text: clamp_display(norte_i18n::t_in(self.lang, clave)),
            subject: None,
        };
        let mut banners = Vec::new();
        if self.journal_rehusado {
            banners.push(frase("status-journal-refused"));
        }
        if let Some(clave) = self.aviso_de_daemon {
            banners.push(frase(clave));
        }
        // A LOOSE window — another one holds the session, or the saved one
        // belongs to a newer binary — does not write the screen, and until
        // now nobody said so: it closed and silently lost where each pane
        // was. The same indicator as the terminal, and in the same place
        // (ADR 0077).
        if !self.sesion.owner || self.sesion.futuro {
            banners.push(frase("status-session-detached"));
        }
        if let Some(aviso) = self.degradadas.banner(self.lang) {
            // The connection goes in its own field, never inside the
            // sentence: see `connection_banner`'s rustdoc.
            banners.push(crate::dto::BannerView {
                text: clamp_display(aviso.text),
                subject: Some(crate::dto::BannerSubjectView {
                    scheme: clamp_display(aviso.scheme),
                    host: clamp_display(aviso.host),
                    reason: clamp_display(aviso.reason),
                    detail: aviso.detail.map(clamp_display),
                    hostile: aviso.hostile,
                }),
            });
        }
        self.status.banners = banners;
        ViewChange::Status(self.status.clone())
    }
}
