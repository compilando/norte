//! The extensions catalog, its detail card and its governance.
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
    /// The directories on screen RIGHT NOW, without repeats.
    pub(super) fn dirs_visibles(&self) -> Vec<VPath> {
        let mut v: Vec<VPath> = Vec::new();
        for h in self.huecos.values() {
            let dir = h.pane.dir();
            if !v.contains(dir) {
                v.push(dir.clone());
            }
        }
        v
    }

    pub(super) fn abrir_extensiones(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.extensiones = Some(crate::extensions::Extensiones::abrir());
        self.gen_extensiones += 1;
        self.pedir_catalogo_de_extensiones(backend, buzon);
        let cambio = ViewChange::Extensions {
            extensions: self.vista_extensiones(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Dispatches a background answer to the surface that requested it.
    ///
    /// ONE spot for all of them: they all check the same thing — that their
    /// surface is still open — and they all answer the same thing: whatever
    /// patches need to be sent, or none.
    ///
    /// The `match` is a LIST: every arm delegates to its own method, so it
    /// grows one line per new answer and none of them carries logic here.
    /// That is why it has the `expect` instead of splitting into two nameless
    /// halves.
    #[expect(clippy::too_many_lines, reason = "a match that only dispatches")]
    pub(super) fn aplicar_de_fondo(
        &mut self,
        f: Fondo,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        match f {
            Fondo::Perfiles(perfiles, vecino) => self.con_los_perfiles(perfiles, vecino, buzon),
            Fondo::PerfilCargado(nombre, res) => self.aplicar_perfil(&nombre, *res, backend, buzon),
            Fondo::AjusteEscrito(hecho) => self.ajuste_escrito(*hecho, backend, buzon),
            Fondo::AjusteRestablecido(hecho) => self.ajuste_restablecido(*hecho, backend, buzon),
            Fondo::PlanIa(epoca, res) => self.aplicar_plan_ia(epoca, *res, backend, buzon),
            // Phase 8: the organize tree needs no second trip, so it carries
            // neither `backend` nor `buzon`.
            Fondo::PlanOrganizar(epoca, res) => self.aplicar_plan_de_organizar(epoca, *res),
            // #311: the two halves of checking checksums — the file read
            // before launching anything, and the report that arrives
            // afterward.
            Fondo::FicheroDeSumas(sums, bytes) => {
                self.fichero_de_sumas(&sums, *bytes, backend, buzon)
            }
            Fondo::InformeDeSumas(task, estado, informe) => {
                self.informe_de_sumas(task, &estado, *informe)
            }
            Fondo::PlanDeLote(epoca, res) => self.aplicar_plan_de_lote(epoca, *res),
            Fondo::PluginsDeAyuda(res) => self.aplicar_catalogo_de_plugins(res, backend, buzon),
            // The panes contributed by consented plugins become real kinds
            // (phase 3). With no surface to depend on: a plugin pane has to
            // be placeable even if nobody has opened help or the manager. The
            // approved/enabled filter lives in `insert_panels`, shared with
            // the terminal.
            //
            // A failure leaves the session with no plugin panes, which is
            // the usual screen: the cosmetic part degrades.
            Fondo::PanelesDePlugin(res) => match res {
                Ok(lista) => {
                    self.kinds.insert_panels(&lista.plugins);
                    // Declaring a kind does NOT repaint on its own: the
                    // layout is rebuilt here — the newly declared pane's
                    // minimums change what fits — and the empty patch
                    // carries the status bar, which `parche` adds on its
                    // own. Without this, the screen stayed laid out as if
                    // the kind did not exist until the next unrelated
                    // change.
                    self.rehacer_reparto();
                    vec![self.parche(Vec::new())]
                }
                Err(_) => Vec::new(),
            },
            Fondo::PaginaDePlugin(id, res) => self
                .aplicar_pagina_de_plugin(&id, res.as_ref().ok())
                .into_iter()
                .collect(),
            Fondo::Catalogo(apertura, peticion, res) => {
                // The manager's catalog redeclares the panes (phase 3): it
                // is the same data, and it is the moment a plugin has just
                // been approved, enabled or uninstalled. Without this,
                // revoking a plugin's consent left its kind declared — and
                // its slot taking focus — until the next startup.
                if let Ok(lista) = &res {
                    self.kinds.insert_panels(&lista.plugins);
                    self.rehacer_reparto();
                }
                self.aplicar_catalogo_de_extensiones(apertura, peticion, res, backend, buzon)
            }
            Fondo::FichaDePlugin(id, res) => self
                .aplicar_ficha(&id, res.as_ref().ok())
                .into_iter()
                .collect(),
            Fondo::AvisosDeDestino(id, avisos) => self.avisos_de_destino(id, avisos),
            Fondo::UndoDeSesion(task_id, sesion) => {
                self.agencia.undos.insert(task_id, sesion);
                Vec::new()
            }
            Fondo::PluginsDePaleta(apertura, res) => self.aplicar_filas_de_plugin(apertura, res),
            Fondo::Gobernada(apertura, res) => {
                self.aplicar_gobierno(apertura, &res, backend, buzon)
            }
            Fondo::ConfigEscrita(apertura, id, res) => {
                self.aplicar_escritura(apertura, &id, res, backend, buzon)
            }
            Fondo::SalidaDeComando(apertura, datos) => self.aplicar_salida(apertura, *datos),
            Fondo::Volumenes(apertura, res) => {
                self.aplicar_volumenes(apertura, res).into_iter().collect()
            }
            Fondo::Conexiones(apertura, res) => {
                self.aplicar_conexiones(apertura, res).into_iter().collect()
            }
            Fondo::PaginaDeLinea(slot, token, start, res) => self
                .aterrizar_pagina(slot, token, start, res)
                .into_iter()
                .collect(),
            Fondo::ConexionesDeIrA(apertura, res) => {
                self.conexiones_de_ir_a(apertura, res).into_iter().collect()
            }
            Fondo::IndiceDeIrA(apertura, consulta, res) => self
                .indice_de_ir_a(apertura, &consulta, res)
                .into_iter()
                .collect(),
            Fondo::Desconectada(slot, res, destino) => {
                self.aplicar_desconexion(slot, res, &destino, backend, buzon)
            }
            Fondo::SitiosVolumenes(res) => self.aplicar_sitios(res).into_iter().collect(),
            Fondo::VolumenesDePie(res) => self.aplicar_volumenes_de_pie(res).into_iter().collect(),
            Fondo::RamasDeArbol(dir, hijos) => self
                .aplicar_ramas(dir, hijos, backend, buzon)
                .into_iter()
                .collect(),
            Fondo::Resultados(epoca, lote) => {
                self.aplicar_resultados(epoca, &lote).into_iter().collect()
            }
            Fondo::Semanticos(epoca, hits) => self.aplicar_semanticos(epoca, hits),
            Fondo::ComparacionViva(epoca, id) => {
                if let Some(c) = self.comparacion.as_mut()
                    && c.epoca == epoca
                {
                    c.task = id;
                }
                Vec::new()
            }
            Fondo::FilasComparadas(epoca, lote) => self.aplicar_filas_comparadas(epoca, *lote),
            Fondo::PlanDeSyncVivo(epoca, id) => self.abrir_panel_de_sync(epoca, id),
            Fondo::SyncAplicando(epoca, id) => self.sync_aplicando(epoca, id, backend, buzon),
            Fondo::SyncNoAplicado(epoca, seguro) => {
                let mut fuera = Vec::new();
                if let Some(s) = self.sincronizacion.as_mut().filter(|s| s.epoca == epoca) {
                    if seguro {
                        s.vista.on_apply_abandoned();
                    } else {
                        // Ambiguous: the latch STAYS thrown. The screen
                        // cannot say "did not apply" about something that
                        // might still be applying, nor offer to retry it.
                        fuera.extend(self.decir("msg-sync-apply-unknown"));
                    }
                }
                // With its patch: `on_apply_abandoned` changes what the
                // screen offers, and without repainting, the `a` that just
                // came back looks dead.
                fuera.push(self.parche(vec![ViewChange::Sync {
                    sync: self.vista_sincronizacion(),
                }]));
                fuera
            }
            Fondo::InformeDeSync(epoca, estado, informe) => {
                self.informe_de_sync(epoca, &estado, *informe)
            }
            Fondo::PlanDeSyncFallido(epoca) => {
                if self.sync_pedida.as_ref().is_some_and(|p| p.epoca == epoca) {
                    self.sync_pedida = None;
                }
                Vec::new()
            }
            Fondo::EventoDeSync(epoca, ev) => self.aplicar_evento_de_sync(epoca, *ev),
            Fondo::Adornos(datos) => self
                .aplicar_adornos(*datos, backend, buzon)
                .into_iter()
                .collect(),
            Fondo::Imagen(token, leido) => self.aplicar_imagen(token, leido).into_iter().collect(),
            Fondo::Estilo(token, preview) => {
                self.aplicar_estilo(token, preview).into_iter().collect()
            }
            Fondo::Miniatura(token, thumb) => {
                self.aplicar_miniatura(token, thumb).into_iter().collect()
            }
            Fondo::BusquedaViva(epoca, id) => {
                if let Some(b) = self.busqueda.as_mut()
                    && b.epoca == epoca
                {
                    b.task = id;
                }
                Vec::new()
            }
            Fondo::BusquedaRota(epoca, e) => self.busqueda_rota(epoca, &e),
        }
    }

    /// Requests the catalog to declare which PANES the plugins contribute.
    ///
    /// On startup and only once: what it brings is which slots exist, not
    /// any of their contents. Through its own path — and not help's or the
    /// manager's — because those exit early if their surface is closed, and
    /// a plugin pane has to be placeable without either of the two having
    /// been opened (phase 3).
    ///
    /// Also in READ-ONLY, and it is deliberate. The palette's rule — "offer
    /// what is going to be refused is promising something that will not
    /// happen" — does not apply here: this offers nothing, it is a READ that
    /// brings the declaration of which slots exist, and without it a saved
    /// layout with a plugin pane leaves a slot of unknown kind, which is
    /// placed with a `(1, 1)` minimum, does not take focus, does not paint
    /// and cannot even be named: a blank box stealing space that the reader
    /// cannot identify. What IS gated by effects is the pane's INTERACTION —
    /// its clickable zones and its commands — where the promise is made.
    ///
    /// And with no gate also because the terminal always asks: a window and
    /// a TUI in read-only have to show the same screen.
    ///
    /// Fail-soft: if the RPC fails or times out, this session is left with
    /// no plugin panes, which is the usual screen.
    /// With no `self` on purpose: since there is no effects gate, it depends
    /// on nothing from the state.
    pub(super) fn pedir_paneles(backend: &Arc<dyn HostBackend>, buzon: &mpsc::Sender<Mensaje>) {
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(PLAZO_PLUGINS, backend.plugin_list()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::PanelesDePlugin(res))))
                .await;
        });
    }

    /// The catalog arrived at the manager.
    ///
    /// A failure is also applied: it stops being "loading" and the list ends
    /// up empty, which with the warning off means "there are none". Staying
    /// "loading" forever would be the only worse answer.
    pub(super) fn aplicar_catalogo_de_extensiones(
        &mut self,
        apertura: u64,
        peticion: u64,
        res: Result<norte_proto::methods::PluginListResult, Error>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // From THIS opening. "Still open" is not "is the same one".
        if apertura != self.gen_extensiones {
            return Vec::new();
        }
        // And the NEWEST of whichever are in flight: an old catalog that
        // lands after the new one leaves the "approved" column saying the
        // old thing, about a change that has already happened.
        if peticion <= self.catalogo_aplicado {
            return Vec::new();
        }
        self.catalogo_aplicado = peticion;
        let Some(e) = self.extensiones.as_mut() else {
            return Vec::new();
        };
        // A failure applies the same way: it stops being "loading" with an
        // empty list, which already knows how to say itself. Staying
        // "loading" forever is the only worse answer.
        e.set_catalogo(&res.unwrap_or(norte_proto::methods::PluginListResult {
            plugins: Vec::new(),
            errors: Vec::new(),
        }));
        let _ = (backend, buzon);
        let cambio = ViewChange::Extensions {
            extensions: self.vista_extensiones(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// Requests the chosen extension's detail card.
    pub(super) fn pedir_ficha(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(e) = self.extensiones.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let Some(id) = e.reclamar_ficha() else {
            return (self.aplicada(), Vec::new());
        };
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(PLAZO_PLUGINS, backend.plugin_config(id.clone()))
                .await
            {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::FichaDePlugin(id, res))))
                .await;
        });
        (self.aplicada(), Vec::new())
    }

    /// The detail card arrived.
    pub(super) fn aplicar_ficha(
        &mut self,
        id: &str,
        res: Option<&norte_proto::methods::PluginGetConfigResult>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let lang = self.lang;
        let e = self.extensiones.as_mut()?;
        match res {
            Some(r) => e.set_ficha(id, r, lang),
            // A failure is also APPLIED: just returning left `pedida` set,
            // so `reclamar_ficha` returned `None` forever and that row could
            // never be reopened — `enter` did nothing and said nothing —
            // short of moving the cursor to another and back. It is the
            // same criterion this file already applies twice to the
            // catalog: staying "loading" forever is the only answer worse
            // than an error.
            None => e.cerrar_ficha(),
        }
        let cambio = ViewChange::Extensions {
            extensions: self.vista_extensiones(),
        };
        Some(self.parche(vec![cambio]))
    }

    /// The manager's projection.
    pub(super) fn vista_extensiones(&self) -> Option<crate::dto::ExtensionsView> {
        Some(self.extensiones.as_ref()?.vista())
    }

    /// The keys while the manager is open.
    pub(super) fn tecla_en_extensiones(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        /// How many rows a page moves.
        const PAGINA: i64 = 10;
        let Some(e) = self.extensiones.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // THREE REGIMES, and the order matters. While a value is being
        // TYPED, letters are letters: resolving `a` as "approve" there would
        // turn typing the word "cat" into two capability grants.
        if e.editando() {
            return self.tecla_editando_config(k, backend, buzon);
        }
        // `Home`/`End` stay fixed: the shared catalog has no verb for "to
        // the start" inside a dialog.
        let verbo = match k.key.as_str() {
            "Home" | "home" | "End" | "end" => None,
            _ => self.verbo_de_dialogo(k),
        };
        let Some(e) = self.extensiones.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match (verbo.as_deref(), k.key.as_str()) {
            (Some("dialog.cancel"), _) => {
                // The first `esc` closes the DETAIL CARD, not the manager:
                // leaving the list for closing a detail loses where the
                // reader was.
                if e.tiene_ficha() {
                    e.cerrar_ficha();
                } else {
                    self.extensiones = None;
                }
            }
            // With the detail card open, the arrows scroll THROUGH ITS keys:
            // moving the catalog underneath would drop the card being read.
            (Some("dialog.down"), _) => {
                if !e.mover_en_ficha(1) {
                    e.mover(1);
                }
            }
            (Some("dialog.up"), _) => {
                if !e.mover_en_ficha(-1) {
                    e.mover(-1);
                }
            }
            // The page and the extremes, through the same door as the
            // arrows: with the card open they scroll THROUGH ITS keys, and
            // only when there is nothing left to walk do they fall back to
            // the catalog.
            (Some("dialog.page-down"), _) => {
                if !e.mover_en_ficha(PAGINA) {
                    e.mover(PAGINA);
                }
            }
            (Some("dialog.page-up"), _) => {
                if !e.mover_en_ficha(-PAGINA) {
                    e.mover(-PAGINA);
                }
            }
            (_, "Home" | "home") => {
                if !e.mover_en_ficha(i64::MIN / 2) {
                    e.mover(i64::MIN / 2);
                }
            }
            (_, "End" | "end") => {
                if !e.mover_en_ficha(i64::MAX / 2) {
                    e.mover(i64::MAX / 2);
                }
            }
            (Some("dialog.confirm"), _) => {
                // A broken one has no settings to open: it is reported,
                // instead of a key that does nothing.
                if e.rota_elegida().is_some() {
                    return (
                        ActionAck::Unavailable {
                            reason_key: "ext-broken-only-uninstall".to_owned(),
                        },
                        self.decir("ext-broken-only-uninstall"),
                    );
                }
                if e.tiene_ficha() {
                    return self.activar_clave(backend, buzon);
                }
                return self.pedir_ficha(backend, buzon);
            }
            // Approving is `dialog.add` — granting — and enabling/disabling
            // is `dialog.toggle-enabled`: the two catalog verbs that mean
            // exactly that, instead of two letters only this window knew.
            (Some("dialog.add"), _) => {
                return self.gobernar_elegida(Cambio::Aprobacion, backend, buzon);
            }
            (Some("dialog.toggle-enabled"), _) => {
                return self.gobernar_elegida(Cambio::Encendido, backend, buzon);
            }
            // Uninstalling is `dialog.remove`, the verb that removes an
            // entry in the favorites list: here it removes the whole
            // extension, which is why it asks first.
            (Some("dialog.remove"), _) => {
                return self.gobernar_elegida(Cambio::Desinstalacion, backend, buzon);
            }
            _ => return (self.aplicada(), Vec::new()),
        }
        let cambio = ViewChange::Extensions {
            extensions: self.vista_extensiones(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// The keys while a key's value is being TYPED.
    ///
    /// FIXED regime, like any other field on this host: here a letter is a
    /// letter. `Enter` confirms — and then it is written — `Escape` cancels
    /// without writing, and every other key means nothing.
    pub(super) fn tecla_editando_config(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(e) = self.extensiones.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match k.key.as_str() {
            "Escape" | "esc" => e.cancelar_edicion(),
            "Backspace" | "backspace" => e.borrar(),
            "Enter" | "enter" => return self.confirmar_config(backend, buzon),
            otra => {
                // A printable key is its character; any other one — and any
                // combination with a modifier — is not text.
                let mut cs = otra.chars();
                match (cs.next(), cs.next()) {
                    (Some(c), None) if !k.ctrl && !k.alt && !k.meta => e.escribir(c),
                    _ => return (self.aplicada(), Vec::new()),
                }
            }
        }
        let cambio = ViewChange::Extensions {
            extensions: self.vista_extensiones(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// `Enter` over a key: cycles, or opens the buffer to type it.
    pub(super) fn activar_clave(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        let Some(e) = self.extensiones.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let escritura = e.activar_clave();
        let cambio = ViewChange::Extensions {
            extensions: self.vista_extensiones(),
        };
        let mut fuera = vec![self.parche(vec![cambio])];
        // A `bool` or an `enum` ALREADY changed value in the model: what is
        // left is telling the daemon. A `string`/`int` only opened the
        // buffer and there is nothing to write yet.
        if let Some((id, escritura)) = escritura {
            fuera.extend(Self::escribir_config(
                self.gen_extensiones,
                &id,
                escritura,
                backend,
                buzon,
            ));
        }
        (self.aplicada(), fuera)
    }

    /// `Enter` with the buffer open: validates and writes, or says why not.
    pub(super) fn confirmar_config(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // The buffer is only opened by `activar_clave`, which already checks
        // this, so today it is unreachable — same as
        // `rechaza_por_solo_lectura`, which exists anyway. A door that
        // writes is checked at the door.
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        let Some(e) = self.extensiones.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let Some(resultado) = e.confirmar_edicion() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match resultado {
            Ok((id, escritura)) => {
                let cambio = ViewChange::Extensions {
                    extensions: self.vista_extensiones(),
                };
                let mut fuera = vec![self.parche(vec![cambio])];
                fuera.extend(Self::escribir_config(
                    self.gen_extensiones,
                    &id,
                    escritura,
                    backend,
                    buzon,
                ));
                (self.aplicada(), fuera)
            }
            // This side's validation is not the one that authorizes — the
            // daemon validates again against the schema — but reporting it
            // here saves a trip and, above all, says WHAT the bound was.
            Err(norte_frontend::settings::SettingsEditError::NotAnInt) => (
                ActionAck::Unavailable {
                    reason_key: "host-not-an-int".to_owned(),
                },
                self.decir("host-not-an-int"),
            ),
            Err(norte_frontend::settings::SettingsEditError::OutOfRange { min, max }) => {
                // The warning carries the bounds; the ACK cannot: nobody
                // substitutes variables into that key, so a `{ $min }` in
                // the ack gets logged literally. Two keys, and the one
                // carrying numbers is the one translated with them.
                let fuera = self.decir_con(
                    "host-out-of-range",
                    &[("min", &min.to_string()), ("max", &max.to_string())],
                );
                (
                    ActionAck::Unavailable {
                        reason_key: "host-value-rejected".to_owned(),
                    },
                    fuera,
                )
            }
            // A plugin field has no closed vocabulary today (only norte's
            // settings editor returns this); it is reported like any
            // rejected value.
            Err(norte_frontend::settings::SettingsEditError::Invalid { .. }) => (
                ActionAck::Unavailable {
                    reason_key: "host-value-rejected".to_owned(),
                },
                self.decir("host-value-rejected"),
            ),
        }
    }

    /// Sends ONE key to the daemon.
    ///
    /// The value is already set in the model (optimism): what fixes a
    /// failure is RE-REQUESTING the card, not guessing what there was
    /// before.
    pub(super) fn escribir_config(
        apertura: u64,
        id: &str,
        escritura: norte_frontend::plugin_config::PendingConfigWrite,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        let (id2, key, value) = (id.to_owned(), escritura.key, escritura.value);
        tokio::spawn(async move {
            let res = match tokio::time::timeout(
                PLAZO_PLUGINS,
                backend2.plugin_set_config(id2.clone(), key, value),
            )
            .await
            {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon2
                .send(Mensaje::Fondo(Box::new(Fondo::ConfigEscrita(
                    apertura, id2, res,
                ))))
                .await;
        });
        Vec::new()
    }

    /// The write answered.
    pub(super) fn aplicar_escritura(
        &mut self,
        apertura: u64,
        id: &str,
        res: Result<(), Error>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Err(e) = res else {
            // A setting that changed can change what a decorator paints —
            // the icons' style, for one — listings are requested again.
            return self.readornar_todo(backend, buzon);
        };
        let mut fuera = self.decir(norte_frontend::error::error_key(&e));
        if apertura != self.gen_extensiones {
            return fuera;
        }
        // And the card is RE-REQUESTED: the screen's optimistic value is
        // right now a lie about what the plugin has configured, and
        // guessing the previous one is inventing a third state.
        //
        // Unless it is being TYPED into: re-requesting it drops the whole
        // `PluginConfigState`, and with it whatever the reader has typed for
        // another key. A stale value on screen is bad; eating what someone
        // just typed, worse — and the correction arrives just the same as
        // soon as the field closes.
        if let Some(ext) = self.extensiones.as_mut()
            && ext.es_ficha_de(id)
            && !ext.editando()
        {
            ext.cerrar_ficha();
            // The close ALWAYS travels in its own patch: `pedir_ficha` sends
            // none along its happy path, so without this the renderer kept
            // painting a card the host no longer has — and the arrows,
            // unable to find it anymore, moved the catalog underneath it.
            let cambio = ViewChange::Extensions {
                extensions: self.vista_extensiones(),
            };
            fuera.push(self.parche(vec![cambio]));
            let (_, partes) = self.pedir_ficha(backend, buzon);
            fuera.extend(partes);
        }
        fuera
    }

    /// `a`/`e` over the chosen extension.
    pub(super) fn gobernar_elegida(
        &mut self,
        cambio: Cambio,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        let Some(e) = self.extensiones.as_ref() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let Some(fila) = e.fila_elegida() else {
            // One that did NOT load: there are no capabilities to read and
            // nothing to turn on, and all it can be asked is to be removed —
            // if its directory is named like an id, which is what gets
            // deleted.
            let Some(rota) = e.rota_elegida() else {
                return (
                    ActionAck::Unavailable {
                        reason_key: "host-no-extension".to_owned(),
                    },
                    Vec::new(),
                );
            };
            let clave = match (cambio, rota.id.clone()) {
                (Cambio::Desinstalacion, Some(id)) => {
                    return self.preguntar_por_desinstalacion(&id);
                }
                (Cambio::Desinstalacion, None) => "ext-broken-not-id",
                _ => "ext-broken-only-uninstall",
            };
            return (
                ActionAck::Unavailable {
                    reason_key: clave.to_owned(),
                },
                self.decir(clave),
            );
        };
        let (id, aprobada, encendida) = (fila.id.clone(), fila.approved, fila.enabled);
        match cambio {
            // Granting ASKS; revoking does not.
            Cambio::Aprobacion if !aprobada => self.preguntar_por_aprobacion(&id),
            Cambio::Aprobacion => {
                let fuera = self.gobernar(&id, Gobierno::Aprobar(false, None), backend, buzon);
                (self.aplicada(), fuera)
            }
            // TURNING ON a plugin with no approval is not a decision this
            // screen can make on its own: with no approved capabilities the
            // core is not going to load it, and saying "on" about something
            // that is not running is the screen lying. TURNING IT OFF, yes,
            // always: it goes in the safe direction, and denying it would
            // leave no way to turn off an enabled extension that just had
            // its capabilities revoked — i.e. it would forbid exactly what
            // must be possible.
            Cambio::Encendido if !aprobada && !encendida => (
                ActionAck::Unavailable {
                    reason_key: "host-extension-not-approved".to_owned(),
                },
                self.decir("host-extension-not-approved"),
            ),
            Cambio::Encendido => {
                let fuera = self.gobernar(&id, Gobierno::Encender(!encendida), backend, buzon);
                (self.aplicada(), fuera)
            }
            // Uninstalling ALWAYS asks: it deletes files and there is no
            // going back.
            Cambio::Desinstalacion => self.preguntar_por_desinstalacion(&id),
        }
    }

    /// What a BUTTON does over a row (bridge 61): point at it and govern the
    /// pointed-at one, through the same path as the key. That it is the same
    /// path is the point: the questions — granting enumerates, uninstalling
    /// warns — are asked once, here, and no button dodges them.
    pub(super) fn gobernar_por_raton(
        &mut self,
        row: u32,
        id: &str,
        cambio: Cambio,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // With a dialog in front, no: the manager is modal for the keyboard
        // (`input.rs` cuts it off before reaching here) and it has to be for
        // the mouse too, or a click behind the consent question would revoke
        // without asking, or stack a second question on the first.
        if !self.dialogos.is_empty() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        // Before moving anything: a read-only window does not repaint a
        // cursor moved by an action that is about to be refused.
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        let Some(e) = self.extensiones.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let Some(movio) = Self::fila_de_extension(e, row, id) else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let (ack, mut fuera) = self.gobernar_elegida(cambio, backend, buzon);
        if movio {
            // The cursor moved with the click, and that is painted even if
            // what follows is a question: the highlighted row is the one
            // the dialog describes.
            let cambio = ViewChange::Extensions {
                extensions: self.vista_extensiones(),
            };
            fuera.push(self.parche(vec![cambio]));
        }
        (ack, fuera)
    }

    /// Points at the row a click names, if it is still the one the renderer
    /// saw. `None` if it is no longer there or no longer that one: the
    /// catalog is re-requested in the background and a row deleted above
    /// shifts the ones below. `Some(movio)` says whether the cursor changed
    /// place.
    fn fila_de_extension(
        e: &mut crate::extensions::Extensiones,
        row: u32,
        id: &str,
    ) -> Option<bool> {
        if e.id_de_fila(row as usize)? != id {
            return None;
        }
        // By ROW, not by `elegida()`: that only looks at the loaded ones and
        // returns `None` for a broken one, so a click on an already-pointed-
        // -at broken one used to claim the cursor had moved and push a
        // whole patch that changed nothing.
        let movio = e.cursor() != row as usize;
        e.senalar(row as usize);
        Some(movio)
    }

    /// Opens the uninstall question, with the name and the id inside.
    ///
    /// The body says what is lost: the extension's files AND its consent —
    /// one installed later under the same id is born without it — because a
    /// plain "uninstall?" reads as "turn it fully off?", and that is not it.
    pub(super) fn preguntar_por_desinstalacion(
        &mut self,
        id: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // One that did not load has no manifest name: its directory is
        // shown, which already comes sanitized and with its flag.
        let Some(nombre) = self.extensiones.as_ref().and_then(|e| {
            e.concesion(id)
                .map(|c| c.nombre)
                .or_else(|| e.rota(id).map(|r| (r.dir.clone(), r.hostile)))
        }) else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let modal = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id: modal,
            title_key: "modal-extension-uninstall-title".to_owned(),
            destination: None,
            subject: Some(crate::dto::DialogLine {
                text: clamp_display(id.to_owned()),
                hostile: false,
            }),
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![
                crate::dto::DialogLine {
                    text: nombre.0,
                    hostile: nombre.1,
                },
                crate::dto::DialogLine {
                    text: norte_i18n::t_in(self.lang, "modal-extension-uninstall-note"),
                    hostile: false,
                },
            ],
            overflow_note: String::new(),
            overflow_hostile: false,
            // `confirm`, like deleting files: it is a normal dialog's
            // affirmative answer, and the LABEL is what says what is being
            // confirmed. `approve` is reserved for granting capabilities.
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-uninstall".to_owned(),
                    destructive: true,
                },
                DialogChoice {
                    id: "cancel".to_owned(),
                    label_key: "dialog-cancel".to_owned(),
                    destructive: false,
                },
            ],
            input: None,
            input_hostile: false,
            input_secret: false,
            fields: Vec::new(),
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id: modal,
            vista: vista.clone(),
            tecleado: Tecleado::Texto(String::new()),
            reconocido: true,
            al_confirmar: Some(Pendiente::DesinstalarExtension { id: id.to_owned() }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// That row's extension's help (bridge 61): what `app.help` does over
    /// the chosen row in the terminal, and cast in the same mold — the
    /// manager closes and help opens with that page as ROOT, with the
    /// catalog the manager already had so the side panel does not wait on
    /// the daemon. With no page it is reported and nothing opens: help
    /// opening at the index when it was asked for ONE extension's is the
    /// window answering a different question.
    pub(super) fn ayuda_de_extension(
        &mut self,
        row: u32,
        id: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Modal for the mouse just as for the keyboard: opening help would
        // close the manager under a pending question, and that question's
        // yes would find no catalog to compare what it grants against.
        if !self.dialogos.is_empty() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        let Some(e) = self.extensiones.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        if Self::fila_de_extension(e, row, id).is_none() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        let Some(fila) = e.fila_elegida() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        if !fila.has_help {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-extensions-no-help".to_owned(),
                },
                self.decir("msg-extensions-no-help"),
            );
        }
        let catalogo = e.catalogo().to_vec();
        let mut ayuda = crate::help::Ayuda::abrir(
            self.lang,
            self.contexto_de_ayuda(),
            &self.efectivo,
            &self.efectivo_visor,
            self.hechos(),
        );
        ayuda.set_plugins(&catalogo);
        let pagina = norte_help::TopicId::new(id);
        ayuda.estado.open_as_root(&pagina);
        if ayuda.estado.current() != &pagina {
            // The shared model does not open what it does not have, and it
            // does so silently: an id that never became a node would leave
            // the reader on the context page, which is not what they asked
            // for. It is reported, and the manager stays.
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-extensions-no-help".to_owned(),
                },
                self.decir("msg-extensions-no-help"),
            );
        }
        self.extensiones = None;
        self.ayuda = Some(ayuda);
        let mut fuera = vec![self.parche(vec![ViewChange::Extensions { extensions: None }])];
        fuera.extend(self.parche_de_ayuda(backend, buzon));
        (self.aplicada(), fuera)
    }

    /// Opens the question to grant capabilities, with the capabilities
    /// inside.
    pub(super) fn preguntar_por_aprobacion(
        &mut self,
        id: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(concesion) = self.extensiones.as_ref().and_then(|e| e.concesion(id)) else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let (nombre, capabilities, ancla) =
            (concesion.nombre, concesion.capabilities, concesion.digest);
        // One capability per LINE, and the extension's name apart: they are
        // the decision's operands, and folding them into a sentence is what
        // lets a third party's name impersonate the window's text. Each with
        // ITS OWN flag: the one that paints differently from what it says is
        // exactly the one a hostile manifest writes to sneak through.
        // And NONE is trimmed. A dialog's line cap exists for a list of
        // paths where seeing part of it is enough; here the list IS the
        // grant, and showing sixteen of forty while the yes grants all forty
        // is exactly the gap the capability nobody read sneaks through. If
        // there are too many to fit, it is not asked about: it is refused.
        if capabilities.len() > MAX_CAPABILIDADES {
            let fuera = self.decir("host-extension-too-many-caps");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-extension-too-many-caps".to_owned(),
                },
                fuera,
            );
        }
        let mut cuerpo = vec![crate::dto::DialogLine {
            text: nombre.0,
            hostile: nombre.1,
        }];
        cuerpo.extend(
            capabilities
                .iter()
                .cloned()
                .map(|(text, hostile)| crate::dto::DialogLine { text, hostile }),
        );
        let nota = String::new();
        let modal = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id: modal,
            title_key: "modal-extension-approve-title".to_owned(),
            destination: None,
            // The reverse-DNS id, which is the ONLY thing the core
            // validates: two extensions can share a name, and the name the
            // dialog shows is written by the manifest. Without this, the
            // screen where permissions are granted does not say to whom.
            subject: Some(crate::dto::DialogLine {
                text: clamp_display(id.to_owned()),
                hostile: false,
            }),
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: cuerpo,
            overflow_note: nota,
            // This dialog trims nothing: its body is the lines it is given
            // ready-made, not a list of paths to be capped.
            overflow_hostile: false,
            choices: vec![
                DialogChoice {
                    id: "approve".to_owned(),
                    label_key: "dialog-approve".to_owned(),
                    // Granting permissions deletes nothing, but it is not a
                    // plain dialog's harmless answer either: it is flagged
                    // so the renderer does not paint it like a notice's "OK".
                    destructive: true,
                },
                DialogChoice {
                    id: "deny".to_owned(),
                    label_key: "dialog-deny".to_owned(),
                    destructive: false,
                },
            ],
            input: None,
            input_hostile: false,
            input_secret: false,
            fields: Vec::new(),
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id: modal,
            vista: vista.clone(),
            tecleado: Tecleado::Texto(String::new()),
            reconocido: true,
            al_confirmar: Some(Pendiente::AprobarExtension {
                id: id.to_owned(),
                capabilities: capabilities.into_iter().map(|(t, _)| t).collect(),
                digest: ancla,
            }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Grants the capabilities READ, or asks again if they have changed.
    ///
    /// The dialog keeps hold of KEYS, not of background messages: a catalog
    /// landing between the question and the yes can bring different
    /// capabilities for that extension, and then the yes would grant
    /// something nobody read.
    pub(super) fn conceder(
        &mut self,
        id: &str,
        leidas: &[String],
        ancla_leida: Option<String>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let ahora = self
            .extensiones
            .as_ref()
            .and_then(|e| e.concesion(id))
            .map(|c| {
                c.capabilities
                    .into_iter()
                    .map(|(t, _)| t)
                    .collect::<Vec<_>>()
            });
        if ahora.as_deref() == Some(leidas) {
            // The anchor that travels is THE QUESTION's, never the current
            // catalog's (#282): re-reading it here would certify to the core
            // "this is what the human read" about what the human did not
            // read, which is exactly the gap the field closes. And the
            // capability comparison above does not cover it: `category` and
            // `contributions` go into the anchor and not into the painted
            // list.
            return (
                None,
                self.gobernar(id, Gobierno::Aprobar(true, ancla_leida), backend, buzon),
            );
        }
        let mut fuera = self.decir("host-extension-changed");
        let (_, partes) = self.preguntar_por_aprobacion(id);
        fuera.extend(partes);
        (Some("host-extension-changed"), fuera)
    }

    /// Sends the change to the daemon. The truth will come from the
    /// re-requested catalog.
    pub(super) fn gobernar(
        &mut self,
        id: &str,
        change: Gobierno,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let apertura = self.gen_extensiones;
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        let id2 = id.to_owned();
        tokio::spawn(async move {
            let llamada = match change {
                Gobierno::Aprobar(v, digest) => backend2.plugin_set_approval(id2, v, digest),
                Gobierno::Encender(v) => backend2.plugin_set_enabled(id2, v),
                // Whether it had consent does not change what follows: the
                // catalog is re-requested regardless, and the question
                // already said so before the yes.
                Gobierno::Desinstalar => {
                    Box::pin(async move { backend2.plugin_uninstall(id2).await.map(|_| ()) })
                }
            };
            let res = match tokio::time::timeout(PLAZO_PLUGINS, llamada).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon2
                .send(Mensaje::Fondo(Box::new(Fondo::Gobernada(apertura, res))))
                .await;
        });
        Vec::new()
    }

    /// The governance change answered.
    ///
    /// With an OK the local `bool` is NOT touched: the catalog is
    /// RE-REQUESTED. An optimism the daemon did not confirm is, on this
    /// screen, an assertion about who can read your files.
    pub(super) fn aplicar_gobierno(
        &mut self,
        apertura: u64,
        res: &Result<(), Error>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if apertura != self.gen_extensiones {
            return Vec::new();
        }
        // The outcome IS STATED even if the manager is already closed: a
        // grant that failed and that nobody was told about is the window
        // keeping quiet about who can read your files.
        let mut fuera = match res {
            Ok(()) => self.decir("host-extension-updated"),
            Err(e) => self.decir(norte_frontend::error::error_key(e)),
        };
        // And the catalog is re-requested IN BOTH CASES. The failure
        // includes THIS side's timeout, which is not "it didn't happen" but
        // "it isn't known": the daemon might have granted the capabilities
        // and been slow to answer, and then leaving the row saying
        // "unapproved" is the same lie as local optimism, in pessimistic
        // form. The only thing that resolves an unknown is going to ask.
        if self.extensiones.is_some() {
            self.repedir_catalogo(backend, buzon);
        }
        // And the LISTINGS, for the same reason: whatever a decorator or a
        // plugin column said about each row said it with the old catalog.
        fuera.extend(self.readornar_todo(backend, buzon));
        fuera
    }

    /// Forgets what the plugins said about EVERY open listing and requests
    /// it again: it is what follows any change of governance or of a
    /// plugin's settings. Turning off the icon decorator left the icons on
    /// rows until the next `cd`, and the reader concluded that turning off
    /// does not turn off.
    ///
    /// A batch in flight is not awaited: the decoration generation goes up,
    /// and when it lands it is dropped and re-requested. The row patch goes
    /// out RIGHT AWAY, with bare rows, so the screen does not keep showing
    /// what the manager just said is not there.
    pub(super) fn readornar_todo(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let slots: Vec<u32> = self.huecos.keys().copied().collect();
        let mut fuera = Vec::new();
        for slot in slots {
            if let Some(hueco) = self.huecos.get_mut(&slot) {
                hueco.olvidar_adornos();
                hueco.pane.set_decorations(std::collections::HashMap::new());
                hueco
                    .pane
                    .set_plugin_columns(std::collections::HashMap::new());
            }
            self.adornar(slot, backend, buzon);
            fuera.push(self.parche_filas_de(slot));
        }
        fuera
    }

    /// Requests the catalog again for the LIVE opening.
    pub(super) fn repedir_catalogo(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        self.pedir_catalogo_de_extensiones(backend, buzon);
    }

    /// Requests the catalog for the manager, numbering the REQUEST.
    ///
    /// Two numbers and not one: the OPENING says whether the manager is
    /// still the same one, and the REQUEST which of several in flight is the
    /// newest. Two governance changes in a row request two catalogs within
    /// the same opening, and they can answer in any order — without the
    /// second number, the old one used to overwrite the new one and the
    /// "approved" column stayed behind forever.
    pub(super) fn pedir_catalogo_de_extensiones(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let apertura = self.gen_extensiones;
        self.gen_catalogo += 1;
        let peticion = self.gen_catalogo;
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(PLAZO_PLUGINS, backend.plugin_list()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::Catalogo(
                    apertura, peticion, res,
                ))))
                .await;
        });
    }

    /// A command's output arrived.
    ///
    /// Only the LAST one launched's: two commands in flight with the slow
    /// one landing later would paint one's output under the other's title,
    /// which on a pane that says who printed what is lying.
    pub(super) fn aplicar_salida(
        &mut self,
        apertura: u64,
        datos: SalidaPedida,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let SalidaPedida {
            id,
            plugin,
            comando,
            res,
        } = datos;
        if apertura != self.gen_salida {
            return Vec::new();
        }
        match res {
            Ok(texto) => {
                // THIRD-PARTY text: it is CLAMPED first — masking a
                // megabyte only to keep four thousand characters is doing
                // the whole job for nothing — it is split into lines, and
                // each one is masked on its own. That it was cut is STATED:
                // the receiver cannot infer it, because what arrives is
                // already short.
                let recortado: String = texto.chars().take(MAX_SALIDA).collect();
                let mut truncado = texto.chars().nth(MAX_SALIDA).is_some();
                let mut lineas = Vec::new();
                let mut hostil = false;
                for linea in recortado.lines().take(MAX_SALIDA_LINEAS) {
                    let (pintable, marcada) = norte_frontend::display_name(linea.as_bytes());
                    hostil |= marcada;
                    lineas.push(clamp_display(pintable));
                }
                truncado |= recortado.lines().nth(MAX_SALIDA_LINEAS).is_some();
                self.escritorio.salida = Some(crate::dto::ExtensionOutputView {
                    plugin: crate::dto::MaskedTextView {
                        text: plugin.0,
                        hostile: plugin.1,
                    },
                    plugin_id: id,
                    command: crate::dto::MaskedTextView {
                        text: comando.0,
                        hostile: comando.1,
                    },
                    lines: lineas,
                    text_hostile: hostil,
                    truncated: truncado,
                });
                let cambio = ViewChange::PluginOutput {
                    output: self.escritorio.salida.clone(),
                };
                vec![self.parche(vec![cambio])]
            }
            Err(e) => self.decir(norte_frontend::error::error_key(&e)),
        }
    }

    /// Closes the output panel.
    pub(super) fn cerrar_salida(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.escritorio.salida = None;
        (
            self.aplicada(),
            vec![self.parche(vec![ViewChange::PluginOutput { output: None }])],
        )
    }

    /// A click on a row of the manager: selects it.
    pub(super) fn elegir_extension(
        &mut self,
        row: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(e) = self.extensiones.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        e.senalar(row as usize);
        let (_, mut envios) = self.pedir_ficha(backend, buzon);
        let cambio = ViewChange::Extensions {
            extensions: self.vista_extensiones(),
        };
        envios.push(self.parche(vec![cambio]));
        (self.aplicada(), envios)
    }
}
