//! Los selectores emergentes: tema, volúmenes, conexiones y favoritos.
//!
//! Parte de `controller`: son métodos de `Estado`, movidos aquí sin
//! tocarlos (ADR 0086). El único escritor sigue siendo el actor.

// Estos módulos son el mismo `impl Estado` partido en trozos, así que usan
// los mismos imports que el padre. Enumerarlos aquí sería una lista de
// cuarenta líneas por fichero, en 32 ficheros, que se desincroniza en cuanto
// el padre importa algo — `super::*` la sigue sola.
#[allow(clippy::wildcard_imports)]
use super::*;

impl Estado {
    /// La proyección del tema.
    pub(super) fn vista_tema(&self) -> Option<crate::dto::ThemeView> {
        let sel = self.tema_elegido.as_ref()?;
        let mut vista = self.tema.vista();
        vista.choices.clone_from(&sel.nombres);
        vista.cursor = sel.cursor as u64;
        Some(vista)
    }

    /// Abre el selector de volúmenes y PIDE la tabla de montaje.
    ///
    /// Igual que el catálogo de extensiones: se abre diciendo que está
    /// preguntando, no esperando.
    pub(super) fn abrir_volumenes(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.abrir_volumenes_en(self.activo(), backend, buzon)
    }

    /// Los volúmenes para el hueco de un LADO de la pantalla.
    ///
    /// `pane.select-drive-left`/`-right` nombran un lado y no el foco —es lo
    /// que hacen `Alt+F1`/`Alt+F2`—, y en un árbol de huecos el único
    /// significado honesto de «izquierda» es la GEOMETRÍA del reparto: el
    /// listado que se ve más a la izquierda. Sin ninguno de ese lado se dice,
    /// en vez de caer al del foco: montar un volumen en el panel equivocado
    /// es exactamente lo que este comando existe para evitar.
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

    /// El listado que se ve más a la izquierda —o más a la derecha— del
    /// reparto de ESTE tamaño.
    ///
    /// Solo entre los que se ven: una pestaña de atrás no está en ningún
    /// lado de la pantalla. Empata por `y` y luego por id, para que dos
    /// listados en la misma columna den siempre la misma respuesta.
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

    /// Abre el selector de volúmenes para un hueco concreto y PIDE la tabla.
    pub(super) fn abrir_volumenes_en(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.abrir_volumenes_con(crate::pickers::Selector::volumenes(slot), backend, buzon)
    }

    /// El cuerpo compartido: abre ESTE selector y pide la tabla de montaje.
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

    /// La tabla de montaje llegó.
    ///
    /// Un fallo se aplica igual: deja de estar preguntando con la lista
    /// vacía, que ya sabe decirse. Y si el selector se cerró mientras volaba,
    /// no hay nada que hacer.
    pub(super) fn aplicar_volumenes(
        &mut self,
        apertura: u64,
        res: Result<Vec<norte_proto::methods::Volume>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        // De ESTA apertura: la generación sube al abrir, así que una
        // respuesta de la anterior no casa.
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

    /// `pane.connect` (#264): el selector de conexiones configuradas.
    ///
    /// La lista la da el DAEMON, no este proceso: leer `connections.toml`
    /// aquí metería russh, opendal, age y el keyring en un binario que solo
    /// quiere pintar nombres. Elegir una NAVEGA a su URL, y eso establece la
    /// sesión por el camino de siempre — con su TOFU y su política.
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

    /// Cierra la sesión del panel activo y lo saca de ahí (#140).
    ///
    /// En un panel LOCAL no hay nada que cerrar y se DICE: una tecla que
    /// contesta «hecho» sobre algo que no ha hecho nada enseña a no fiarse del
    /// mensaje.
    ///
    /// El destino se decide AQUÍ, antes de soltar la sesión, porque después la
    /// ruta del panel ya no sirve de clave.
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

    /// A dónde va un panel cuya sesión se acaba de cerrar.
    ///
    /// La decisión —el rastro hacia atrás saltándose la máquina que se cierra,
    /// y casa cuando no queda nada— vive en `norte-frontend` y la comparten los
    /// dos frontends: cuando estaba aquí, la TUI se iba a casa siempre y esta
    /// ventana volvía sobre su rastro, con la misma tecla y el mismo nombre.
    pub(super) fn donde_volver_tras_desconectar(&self, cerrada: &VPath) -> VPath {
        norte_frontend::nav::regreso_tras_desconectar(cerrada, self.hueco().historial.trail())
            .unwrap_or_else(norte_frontend::shell::home_vpath)
    }

    /// La sesión se cerró (o no había ninguna): se dice y el panel se va.
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
            // `false` no es un fallo: no había sesión abierta. Y aun así el
            // panel se va, porque seguir ahí exigiría reabrirla.
            Ok(false) => "msg-disconnect-none",
            Err(e) => {
                // Un cierre que falla NO navega: el panel sigue donde estaba y
                // la sesión sigue viva, que es lo que el error dice.
                return self.decir(norte_frontend::error::error_key(&e));
            }
        };
        self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
        self.navegar_hueco(slot, destino, Trail::Record, backend, buzon)
    }

    /// Llegaron las conexiones (#264). Misma guarda de apertura que los
    /// volúmenes: una respuesta de la lista anterior no la rellena.
    pub(super) fn aplicar_conexiones(
        &mut self,
        apertura: u64,
        res: Result<Vec<norte_proto::methods::ConnectionEntry>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        if apertura != self.gen_selector {
            return None;
        }
        let s = self.selector.as_mut()?;
        // Un fallo se pinta como lista VACÍA con su frase, no como una lista
        // sin explicación: «no tienes ninguna» y «no se pudo preguntar» no son
        // lo mismo, y sin la frase las dos se leen igual.
        s.con_conexiones(res.unwrap_or_default());
        self.gen_selector += 1;
        let cambio = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        Some(self.parche(vec![cambio]))
    }

    /// La proyección del selector.
    pub(super) fn vista_selector(&self) -> Option<crate::dto::PickerView> {
        let mut v = self.selector.as_ref()?.vista(self.lang);
        v.generation = self.gen_selector;
        Some(v)
    }

    /// Las teclas mientras se mira el tema. Solo se cierra.
    /// Teclas del selector de perfiles.
    ///
    /// Fijas, como las de los demás selectores de esta ventana: flechas para
    /// recorrer, `Enter` para elegir y `Escape` para cerrar sin cambiar nada.
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
                    // Una fila que no se puede cargar no cambia nada, y el
                    // selector se queda abierto: cerrarlo sería contestar que
                    // sí a algo que no pasó.
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

    /// Elige un perfil de la lista: si ya es el activo no pasa nada, y si no,
    /// empieza el cambio.
    pub(super) fn elegir_perfil(
        &mut self,
        nombre: &std::ffi::OsStr,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let _ = backend;
        if self.perfil_activo.as_deref() == Some(nombre) {
            // Ya estabas en él: se cierra y se calla. Tirar y recargar la
            // pantalla para dejarla igual sería trabajo para nada.
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
            // `Escape` VUELVE al que había. Un selector con preview en vivo
            // que se cierra dejando puesto lo último que rozó el cursor no
            // es un selector: es una forma de cambiar de tema sin querer.
            "Escape" | "esc" => {
                let previo = *sel.previo.clone();
                self.tema_elegido = None;
                // Se restaura el tema ENTERO, no se vuelve a resolver su
                // nombre: el que había puede no ser un preset. El aviso sale
                // igual, para que quien hospeda deshaga lo suyo.
                let nombre = previo.name.clone();
                self.tema = previo;
                self.nativo(crate::dto::NativeEffect::ThemeChanged { name: nombre });
                let cambio = ViewChange::Theme { theme: None };
                return (self.aplicada(), vec![self.parche(vec![cambio])]);
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
                // Y se GUARDA, que es lo que separa elegir un tema de mirarlo.
                // Al perfil activo si lo hay: escribirlo en la capa del
                // usuario mientras un perfil fija el suyo lo deja tapado
                // (ADR 0079 D1).
                self.persistir_tema(&elegido, buzon);
                let cambio = ViewChange::Theme { theme: None };
                return (self.aplicada(), vec![self.parche(vec![cambio])]);
            }
            _ => return (self.aplicada(), Vec::new()),
        }
        // Preview EN VIVO: moverse por la lista enseña el tema, no su nombre.
        let bajo_el_cursor = sel.nombres.get(sel.cursor).cloned();
        if let Some(nombre) = bajo_el_cursor {
            self.aplicar_tema(&nombre, buzon);
        }
        let cambio = ViewChange::Theme {
            theme: self.vista_tema(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Pone un tema por su nombre: el del host, y el de quien lo hospeda.
    ///
    /// Los dos, y por eso está aquí en vez de en dos sitios: el host guarda
    /// los colores para su propia pantalla de tema, y quien hospeda tiene que
    /// volver a resolver lo suyo —las variables CSS de la webview— porque las
    /// resolvió una vez al arrancar. Un nombre que no existe deja el tema como
    /// estaba en vez de dejar la pantalla sin colores.
    ///
    /// Un PRESET se aplica aquí mismo; una RUTA se va a leer fuera.
    ///
    /// El corte lo decide `norte_frontend::theme::is_preset`, que es de los
    /// dos frontends: resolver un preset es aritmética sobre colores y
    /// mandarlo a otro hilo añadiría un frame de retraso a algo que el lector
    /// ve cambiar bajo el cursor, mientras que leer un fichero dentro del
    /// actor es la regla 2 rota — y con un tema en un montaje caído congela la
    /// ventana entera.
    ///
    /// Antes solo se aplicaban presets. El selector ofrece presets, así que
    /// por esa puerta daba igual; por la del CAMBIO DE PERFIL no, porque un
    /// perfil puede traer `theme = "…/mio.toml"` (ADR 0020) y eso se quedaba
    /// sin aplicar en silencio, con el terminal aplicándolo.
    pub(super) fn aplicar_tema(&mut self, nombre: &str, buzon: &mpsc::Sender<Mensaje>) {
        if let Ok(Some(tema)) = norte_theme::Theme::preset(nombre) {
            self.tema_puesto(nombre, &tema);
            return;
        }
        let spec = nombre.to_owned();
        let buzon = buzon.clone();
        tokio::task::spawn_blocking(move || {
            let resuelto = norte_frontend::theme::resolve_theme(Some(&spec));
            // El error NO viaja: lleva el spec dentro, que es una ruta, y lo
            // que la barra dice sale del catálogo (#73). La clave dice si el
            // fichero no se pudo leer o si no valida, que es lo accionable.
            let salida = resuelto.map_err(|e| match e {
                norte_frontend::theme::ResolveError::Io { .. } => "host-theme-unreadable",
                norte_frontend::theme::ResolveError::Parse { .. } => "host-theme-invalid",
            });
            let _ = buzon.blocking_send(Mensaje::TemaResuelto(Box::new((spec, salida))));
        });
    }

    /// El tema ya resuelto pasa a ser el vigente, y se le dice a quien hospeda.
    pub(super) fn tema_puesto(&mut self, nombre: &str, tema: &norte_theme::Theme) {
        self.tema = crate::pickers::HostTheme::de(nombre, tema);
        self.nativo(crate::dto::NativeEffect::ThemeChanged {
            name: nombre.to_owned(),
        });
    }

    /// Guarda el tema elegido en la capa que toca, FUERA del actor.
    ///
    /// El actor es el único escritor del estado y esto es I/O con un lock
    /// entre procesos detrás (`persist_ui_theme_to` bloquea mientras otro
    /// norte escribe): hacerlo aquí congelaría la ventana entera. Vuelve por
    /// el buzón como todo lo demás.
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
                // El error NO viaja: puede llevar la ruta del fichero, y lo
                // que la barra dice sale del catálogo (#73). La categoría
                // basta para saber qué pasó.
                Err(e) => Some(clave_de_io(&e)),
            };
            let _ = buzon.blocking_send(Mensaje::TemaPersistido(clave));
        });
    }

    /// Dónde escribe esta ventana su configuración.
    ///
    /// La capa MÁS ALTA de las que se pueden editar: el perfil activo si lo
    /// hay, y la del usuario si no. Nunca la del sistema (no es de quien está
    /// delante) ni la del proyecto (es del directorio, no de la persona).
    ///
    /// Sale de las capas que quien arrancó el host resolvió, no de volver a
    /// mirar el entorno: la ventana escribe donde de verdad leyó (ADR 0066
    /// D14).
    pub(super) fn dir_de_escritura(&self) -> Option<std::path::PathBuf> {
        use crate::settings::ConfigLayer;
        self.paths
            .config_layers
            .iter()
            .rfind(|(capa, _)| matches!(capa, ConfigLayer::Profile | ConfigLayer::User))
            .map(|(_, p)| p.path.clone())
    }

    /// Pide el NOMBRE de un favorito nuevo que apunta al directorio del panel
    /// (#309), con el campo ya prellenado.
    ///
    /// La sugerencia sale del modelo COMPARTIDO
    /// (`norte_frontend::places::suggested_hotlist_name`), que es el que usa el
    /// terminal: esquiva los nombres ocupados porque guardar REEMPLAZA el
    /// favorito que ya se llame así, y con el campo prellenado el reflejo de
    /// aceptar sin leer pisaría uno que apuntaba a otro sitio. Un nombre
    /// TECLEADO que colisione sigue reemplazando — eso es lo que se pidió.
    pub(super) fn pedir_favorito(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let destino = self.hueco().pane.dir().clone();
        let ocupados: Vec<&str> = self
            .config
            .common
            .hotlist
            .iter()
            .map(|h| h.name.as_str())
            .collect();
        let sugerido = norte_frontend::places::suggested_hotlist_name(&destino, &ocupados);
        let donde = Self::linea_de_ruta(&destino);
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
            body: vec![donde],
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
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id,
            vista,
            tecleado: Tecleado::Texto(sugerido),
            reconocido: true,
            al_confirmar: Some(Pendiente::GuardarFavorito { destino }),
        });
        // El selector se cierra: la pregunta la contesta el diálogo, y dejar
        // la lista debajo daría dos cursores vivos a la vez.
        self.selector = None;
        let cambios = vec![
            ViewChange::Picker { picker: None },
            ViewChange::Dialogs {
                dialogs: self.vistas_de_dialogos(),
            },
        ];
        (self.aplicada(), vec![self.parche(cambios)])
    }

    /// Guarda el favorito `nombre` = `destino` en la capa de configuración que
    /// esta ventana escribe (#309).
    ///
    /// Por `spawn_blocking` (regla 2): `persist_hotlist_add` escribe un fichero
    /// con lock y tmp+rename. La copia en memoria solo se toca si el disco fue
    /// bien — que es lo que hace el terminal, y por lo mismo: una lista que
    /// dice tener un favorito que no está en el fichero miente hasta el
    /// siguiente arranque.
    pub(super) fn guardar_favorito(
        &mut self,
        destino: &VPath,
        nombre: &str,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let nombre = nombre.trim().to_owned();
        if nombre.is_empty() {
            // Sin nombre no hay favorito: es lo que hace el terminal con el
            // campo vacío, y es más honesto que guardar uno sin nombre.
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

    /// Pide el NOMBRE con el que guardar el espacio de trabajo (#318).
    ///
    /// Prellenado con el perfil ACTIVO, que es lo que un «guardar como» hace
    /// en todas partes: lo normal es partir del que tienes y darle otro
    /// nombre. Sin perfil activo el campo nace vacío — inventar uno sería
    /// proponer un directorio que el lector no ha pedido.
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
            // Las claves del TERMINAL, no unas nuevas: es el mismo diálogo, y
            // Fluent se queda con la PRIMERA definición — una clave duplicada
            // con otro texto deja muerta a la vieja sin decirlo.
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

    /// Escribe `profiles/<nombre>/` con lo que hay en pantalla (#318).
    ///
    /// El CONTENIDO no se decide aquí: lo monta [`Self::instantanea_de_perfil`]
    /// y lo escribe `norte_config::save_profile`, que es el mismo escritor que
    /// usa el terminal. Es la exigencia de la ADR 0077 —una decisión duplicada
    /// entre frontends diverge en silencio—, y aquí sería el peor sitio para
    /// que divergiera: dos «guardar como» que producen perfiles distintos
    /// convierten el perfil en algo que depende de por dónde lo guardaste.
    ///
    /// El nombre se valida ANTES de tocar disco, y el disco va fuera del actor.
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

    /// Lo que hay en pantalla, en la forma que `norte-config` escribe (#318).
    ///
    /// El CONTENIDO lo decide `norte_frontend::config::profile_snapshot`, que
    /// es la misma que llama el terminal: aquí solo se contesta dónde está
    /// cada listado. Ver su rustdoc para por qué no hay dos copias de esto.
    pub(super) fn instantanea_de_perfil(&self) -> norte_config::ProfileSnapshot {
        norte_frontend::config::profile_snapshot(
            &self.arbol,
            // Solo los huecos que SON un listado tienen directorio, y son los
            // que `huecos` guarda: el visor, los procesos y los sitios no
            // tienen nada que poner en `[profile.start]`.
            &|SlotId(n)| self.huecos.get(&n).map(|h| h.pane.dir().clone()),
            self.dir_de_escritura()
                .and_then(|d| std::fs::read(d.join("keymap.toml")).ok()),
        )
    }

    /// Quita el favorito que el cursor señala (#309).
    ///
    /// Sin confirmación, como en el terminal: un favorito es un atajo, no un
    /// fichero, y volver a crearlo cuesta una tecla. El nombre sale CRUDO de
    /// la fila y no de su etiqueta, que va saneada.
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

    /// El disco contestó a un favorito guardado (#309): se refleja o se dice.
    /// El perfil quedó escrito, o no (#318).
    ///
    /// No se activa solo: guardar es guardar, y cambiar de perfil es otra
    /// cosa con su propia tecla. El terminal hace lo mismo, y el test de
    /// paridad lo pide.
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
                // REEMPLAZA el que se llame igual, que es lo que hace el
                // fichero: si la copia en memoria añadiera uno más, la lista
                // enseñaría dos donde el disco tiene uno.
                self.config.common.hotlist.retain(|h| h.name != nombre);
                self.config.common.hotlist.push(norte_config::HotlistItem {
                    name: nombre.to_owned(),
                    target: Ok(destino),
                });
            }
            None => self.config.common.hotlist.retain(|h| h.name != nombre),
        }
        // La barra lateral pinta los favoritos: se resiembra desde la copia
        // que acaba de cambiar, y eso sube su generación. Sin ello la lista de
        // sitios seguiría enseñando la de antes.
        self.sembrar_sitios();
        // Y el selector, si sigue abierto, se rehace con la lista nueva: es la
        // superficie desde la que se acaba de editar, y dejarla igual sería
        // contestar «hecho» sobre una lista que no lo enseña.
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
        // Una FOTO entera, como cuando llegan los volúmenes y por lo mismo:
        // esto cambia dos superficies a la vez —la barra y el selector— y las
        // filas se numeran de nuevo, así que un parche por índice nombraría
        // filas que ya no son las que eran.
        let snap = self.snapshot();
        vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))]
    }

    /// Las teclas mientras un selector está abierto.
    pub(super) fn tecla_en_selector(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        /// Cuántas filas mueve una página.
        const PAGINA: i64 = 10;
        if self.selector.is_none() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        // `Home`/`End` siguen siendo teclas fijas: el catálogo compartido no
        // tiene verbo para «al principio» dentro de un diálogo, y esperar a
        // que lo tenga habría dejado la lista sin extremos.
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
                // Los favoritos son la única lista de esta ventana que se
                // EDITA (#309), y son los dos verbos que el catálogo ya tenía
                // para eso: en el terminal son la `a` y la `d` del mismo
                // popup. Sobre cualquier otro selector no significan nada y se
                // ignoran, como cualquier tecla que ese selector no ata.
                Some("dialog.add") if s.es_hotlist() => return self.pedir_favorito(),
                Some("dialog.remove") if s.es_hotlist() => return self.quitar_favorito(buzon),
                _ => return (self.aplicada(), Vec::new()),
            }
        }
        let cambio = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Elegir del selector: navegar al volumen.
    ///
    /// Navegar es LECTURA, así que el volumen sí se abre — al contrario que
    /// una conexión, que esta ventana ni enumera todavía.
    ///
    /// El cierre viaja en su PROPIO parche y antes de la navegación, como el
    /// de la paleta y por lo mismo: un renderer que aplica parches se
    /// quedaría el selector pintado encima del listado nuevo.
    pub(super) fn elegir_del_selector(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(s) = self.selector.as_ref() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // El hueco lo dijo el selector al ABRIRSE: `pane.select-drive-left`
        // nombra un lado, y leer el foco aquí haría que moverlo con la lista
        // puesta montara el volumen en otro panel.
        let slot = s.slot();
        if !self.huecos.contains_key(&slot) || self.oculto(slot) {
            // El reparto cambió con la lista puesta: el hueco que el selector
            // capturó al abrirse ya no está, o dejó de verse. Navegar ahí
            // traería un listado que nadie va a mirar —contra «lo que no se
            // ve no se trae»— o no haría nada y cerraría el selector en
            // silencio.
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
                // Hay fila y no lleva a ninguna parte: un favorito cuya ruta
                // no parsea. Se DICE, que es lo que la fila ya avisaba.
                return (
                    ActionAck::Unavailable {
                        reason_key: "hotlist-invalid".to_owned(),
                    },
                    Vec::new(),
                );
            }
            // Sin filas todavía (o la tabla llegó vacía): no hay a dónde ir.
            return (self.aplicada(), Vec::new());
        };
        self.selector = None;
        let cierre = self.parche(vec![ViewChange::Picker { picker: None }]);
        let mut envios = vec![cierre];
        envios.extend(self.navegar_hueco(slot, &destino, Trail::Record, backend, buzon));
        (self.aplicada(), envios)
    }

    /// Un click en una fila del selector: la elige.
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

    /// La conexión con el daemon cambió de estado: se PINTA y se DICE.
    ///
    /// Las dos cosas, y por la misma cola: perder el daemon a mitad de una
    /// operación no puede notarse solo en un icono.
    pub(super) fn cambio_de_conexion(
        &mut self,
        ev: norte_client::ConnEvent,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let (vista, clave) = match ev {
            norte_client::ConnEvent::Restored => (ConnectionView::Connected, "msg-daemon-restored"),
            // El daemon avisa ANTES de cerrar, y esto es lo único que
            // distingue un relevo de una parada: en cuanto la conexión caiga,
            // las dos se ven igual. Se queda como aviso PERSISTENTE porque
            // sigue siendo verdad mientras dure, y la vista de conexión no se
            // toca todavía — la conexión, ahora mismo, sigue en pie.
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
            // `Lost` y el comodín juntos: `ConnEvent` es NO EXHAUSTIVO, y un
            // evento de un SDK más nuevo se lee como una pérdida, que es lo
            // conservador — se pinta reconectando en vez de fingir que todo
            // sigue igual.
            norte_client::ConnEvent::Lost | _ => (ConnectionView::Reconnecting, "msg-daemon-lost"),
        };
        // Volver APAGA el aviso: uno que no sabe volverse «ya está» miente en
        // cuanto el daemon reaparece, y el relevo termina volviendo.
        // Y estrena ÉPOCA: al otro lado puede haber un daemon NUEVO, con su
        // contador de ids desde 1. Lo que quede en el tablero con esos
        // números es de antes, y a partir de aquí no se hereda nada suyo.
        if matches!(ev, norte_client::ConnEvent::Restored) {
            self.aviso_de_daemon = None;
            self.epoca_conexion = self.epoca_conexion.saturating_add(1);
            // Un plan pedido al daemon ANTERIOR no lo va a contestar el
            // nuevo: su id empieza otra vez en 1, y dejar la petición colgada
            // haría que el panel se abriera con la Task de otro.
            if let Some(pedida) = self.sync_pedida.take() {
                pedida
                    .abandonada
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        self.conexion = vista.clone();
        let banners = self.cambio_de_banners();
        let parche = self.parche(vec![ViewChange::Connection(vista), banners]);
        let aviso = self.sobre(UiUpdate::Notice(UiNotice::Message {
            key: clave.to_owned(),
            detail: None,
        }));
        vec![parche, aviso]
    }

    /// Una sesión de provider viaja sin cifrar (#44): se apunta y se dice.
    ///
    /// Persistente y no efímero: un mensaje que borra la siguiente tecla no
    /// puede describir cómo viaja lo que se está mirando. La frase y el techo
    /// los pone `norte_frontend::banners`, que es el MISMO código que compone
    /// el aviso del TUI.
    pub(super) fn sesion_degradada(
        &mut self,
        d: norte_proto::methods::ConnectionDegraded,
    ) -> BridgeEnvelope<UiUpdate> {
        self.degradadas.note(d);
        let cambio = self.cambio_de_banners();
        self.parche(vec![cambio])
    }

    /// La conexión pide su contraseña (#325/#327): se abre el diálogo.
    ///
    /// La pregunta nombra la conexión **y a dónde se conecta**, y esa segunda
    /// línea es el punto: el nombre lo eligió un fichero de configuración, y un
    /// fichero puede llegar de los dotfiles de otro o de una línea editada, así
    /// que «trabajo» no dice nada sobre si esa entrada sigue apuntando donde
    /// apuntaba ayer. Es la misma razón por la que el diálogo de host key
    /// enseña una huella. Cada parte en su CAMPO, jamás interpolada en la
    /// frase.
    ///
    /// El campo nace vacío y confirmar sobre él es INERTE (ver
    /// `ejecutar_pendiente`): entregar la cadena vacía reproduce #320, donde un
    /// secreto vacío hacía que la conexión autenticara con la cadena ambiente
    /// —una identidad que nadie pidió—.
    pub(super) fn pedir_secreto(
        &mut self,
        conn: String,
        endpoint: &str,
        slot: u32,
        dir: VPath,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // UNA pregunta por conexión. Dos paneles sobre la misma entrada
        // `prompt` —o un refresco mientras el diálogo está delante— apilaban
        // otra pregunta idéntica, con su propio campo vacío; y bajo suficientes
        // de esas, el desalojo por tope de `apilar_dialogo` se lleva por
        // delante las APROBACIONES de agente sin reconocer, que es lo primero
        // que sacrifica.
        if self.dialogos.iter().any(|d| {
            matches!(&d.al_confirmar, Some(Pendiente::EntregarSecreto { conn: c, .. }) if *c == conn)
        }) {
            return Vec::new();
        }
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        // Los dos vienen del CORE, no del servidor remoto, pero se enmascaran
        // igual: el nombre sale de un fichero y el endpoint de una URL, y
        // ninguno de los dos orígenes es de fiar para lo que se pinta.
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
            // A DÓNDE va la contraseña. En `destination` y no en el cuerpo por
            // lo que dice el rustdoc de ese campo: un separador dentro del
            // texto lo puede escribir el propio dato.
            destination: Some(linea(endpoint)),
            // QUÉ entrada la pide.
            subject: Some(linea(&conn)),
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            // Dónde acaba lo que se teclea, y es la misma frase que dice la
            // TUI: el secreto vive en la memoria del daemon hasta que pare, y
            // no se escribe en ningún fichero. Quien va a teclear una
            // contraseña tiene derecho a saberlo ANTES.
            //
            // `hostile: false` porque es una frase del catálogo, no un dato:
            // no ha pasado por `display_name` porque no viene de fuera.
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
                    // No es destructivo: no borra nada. Lo que lo hace
                    // delicado —que sale un secreto— no es lo que esa marca
                    // significa, y usarla aquí devaluaría la de un borrado.
                    destructive: false,
                },
                crate::dto::DialogChoice {
                    id: "cancel".to_owned(),
                    label_key: "dialog-cancel".to_owned(),
                    destructive: false,
                },
            ],
            // Vacío, y con `Some`: es lo que le dice al renderer que aquí SE
            // ESCRIBE. Lo que viaje por aquí serán siempre puntos.
            input: Some(String::new()),
            input_hostile: false,
            input_secret: true,
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        let mut fuera = self.apilar_dialogo(Dialogo {
            id,
            vista,
            tecleado: Tecleado::Secreto,
            // `true`: esto lo abrió un GESTO del lector —la navegación que
            // acaba de hacer—, así que la siguiente respuesta ya ES una
            // respuesta. La regla del «ya lo veo» es para lo que aparece sin
            // que nadie lo pida (una aprobación de agente, el informe de un
            // lote), y aquí la pregunta la hizo quien está delante. Es lo
            // mismo que hace la TUI, donde Enter contesta directo.
            //
            // Y no abre ningún hueco: confirmar sin teclear nada es inerte,
            // así que el peor caso de un dedo adelantado es no hacer nada.
            reconocido: true,
            al_confirmar: Some(Pendiente::EntregarSecreto { conn, slot, dir }),
        });
        // `apilar_dialogo` solo apila —y devuelve lo que se cayó por el tope—:
        // el parche que lo PINTA lo manda quien abre, como el resto.
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        fuera.push(self.parche(vec![cambio]));
        fuera
    }

    /// El secreto se entregó (o no): se reintenta la navegación, o se dice.
    ///
    /// El reintento es de ESA navegación —su hueco y su destino—, que es lo
    /// que la pendiente transporta. Si entregarlo falló, no hay reintento: el
    /// hueco se queda como el error lo dejó y la barra lo cuenta.
    pub(super) fn secreto_entregado(
        &mut self,
        slot: u32,
        dir: &VPath,
        res: Result<(), Error>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        match res {
            // `Record` y no `Replay`, aunque esto sea un reintento: un
            // `Replay` necesita un SENTIDO del rastro, y aquí no lo hay —esta
            // navegación pudo nacer de una tecla, de un favorito o de un
            // `back`, y el error no lo transportó—. Inventarse uno sería peor
            // que no tenerlo.
            //
            // Y no duplica: el listado fallido dejó el hueco ENSEÑANDO el
            // directorio al que no se pudo entrar, así que en el reintento
            // `anterior == destino` y `navegar_hueco` no registra nada.
            Ok(()) => self.navegar_hueco(slot, dir, Trail::Record, backend, buzon),
            Err(e) => self.decir(norte_frontend::error::error_key(&e)),
        }
    }

    /// Una conexión NO se pudo abrir, y por qué (#322).
    ///
    /// Un `Notice` y no un banner persistente, al revés que la degradación: la
    /// degradación describe una sesión que existe y sigue existiendo mientras
    /// se mira; esto describe un intento que ya terminó, y un indicador
    /// permanente sobre algo que no está abierto no se apagaría nunca.
    ///
    /// La línea la compone `norte_frontend::banners::failure_line`, que es el
    /// MISMO código que usa la TUI: dos frases sobre por qué no se pudo entrar
    /// en una máquina divergen en silencio, que es justo lo que ADR 0077
    /// existe para evitar.
    ///
    /// **El orden con el error del listado NO está garantizado aquí.** En la
    /// TUI sí lo está (el manejador retiene el `select!` mientras espera, así
    /// que la categoría llega primero y esta frase la pisa); en la ventana son
    /// dos productores independientes contra el mismo buzón, y la categoría
    /// genérica puede procesarse DESPUÉS. Se acepta: los dos textos describen
    /// el mismo fallo y ninguno es incorrecto. Si algún día importa, hay que
    /// hacerlo explícito y no confiar en el planificador.
    /// Devuelve DOS cosas, y el parche es la que se ve: el renderer solo
    /// atiende los `Notice` de clase `fatal`, y su texto de estado sale de
    /// `status.message`, que solo se mueve con un parche. Mandar el aviso solo
    /// dejaba a la ventana sin pintar nada — con el test verde, porque
    /// afirmaba sobre el sobre del puente y no sobre el estado. Es el mismo
    /// patrón que `cambio_de_conexion`, que ya devuelve `vec![parche, aviso]`.
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

    /// Recompone los avisos persistentes de la barra y devuelve su cambio.
    ///
    /// UN sitio para los tres, y en este orden: el journal habla de TODA la
    /// sesión y de si algo se puede deshacer, el daemon de si esta ventana
    /// va a seguir sirviendo, y la degradación de cómo viaja una conexión.
    /// Elegir uno solo escondería los otros para siempre, que es justo lo
    /// que el TUI ya decidió no hacer.
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
        if let Some(aviso) = self.degradadas.banner(self.lang) {
            // La conexión va en su propio campo, jamás dentro de la frase:
            // ver el rustdoc de `connection_banner`.
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
