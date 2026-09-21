//! El catálogo de extensiones, su ficha y su gobierno.
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
    /// Los directorios que hay AHORA en pantalla, sin repetir.
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

    /// Reparte una respuesta de fondo a la superficie que la pidió.
    ///
    /// UN sitio para las cinco: todas comprueban lo mismo —que su superficie
    /// siga abierta— y todas contestan lo mismo: los parches que haya que
    /// mandar, o ninguno.
    ///
    /// El `match` es una LISTA: cada brazo delega en su método, así que crece
    /// una línea por respuesta nueva y ninguna de ellas tiene lógica aquí.
    /// Por eso lleva el `expect` en vez de partirse en dos mitades sin nombre.
    #[expect(clippy::too_many_lines, reason = "un match que solo reparte")]
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
            // Fase 8: el árbol de organizar no necesita un segundo viaje, así
            // que no lleva ni `backend` ni `buzon`.
            Fondo::PlanOrganizar(epoca, res) => self.aplicar_plan_de_organizar(epoca, *res),
            // #311: las dos mitades de comprobar unas sumas — el fichero que
            // se lee antes de lanzar nada, y el informe que llega después.
            Fondo::FicheroDeSumas(sums, bytes) => {
                self.fichero_de_sumas(&sums, *bytes, backend, buzon)
            }
            Fondo::InformeDeSumas(task, estado, informe) => {
                self.informe_de_sumas(task, &estado, *informe)
            }
            Fondo::PlanDeLote(epoca, res) => self.aplicar_plan_de_lote(epoca, *res),
            Fondo::PluginsDeAyuda(res) => self.aplicar_catalogo_de_plugins(res, backend, buzon),
            // Los paneles que aportan los plugins consentidos pasan a ser
            // kinds de verdad (fase 3). Sin superficie de la que depender: un
            // panel de plugin tiene que poder colocarse aunque nadie haya
            // abierto la ayuda ni el gestor. El filtro de aprobado/activado
            // vive en `insert_panels`, compartido con el terminal.
            //
            // Un fallo deja la sesión sin paneles de plugin, que es la
            // pantalla de siempre: lo cosmético se degrada.
            Fondo::PanelesDePlugin(res) => match res {
                Ok(lista) => {
                    self.kinds.insert_panels(&lista.plugins);
                    // Declarar un kind NO repinta por su cuenta: el reparto se
                    // rehace aquí —los mínimos del panel recién declarado
                    // cambian dónde cabe— y el parche vacío lleva la barra,
                    // que `parche` añade sola. Sin esto, la pantalla seguía
                    // repartida como si el kind no existiera hasta el
                    // siguiente cambio ajeno.
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
                // El catálogo del gestor redeclara los paneles (fase 3): es el
                // mismo dato, y es el momento en que un plugin acaba de ser
                // aprobado, activado o desinstalado. Sin esto, quitarle el
                // consentimiento a un plugin dejaba su kind declarado —y su
                // hueco tomando foco— hasta el siguiente arranque.
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
            Fondo::PaginaDeLinea(slot, token, desde, res) => self
                .aterrizar_pagina(slot, token, desde, res)
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
                        // Ambiguo: el pestillo se QUEDA echado. La pantalla no
                        // puede decir «no se aplicó» de algo que quizá se está
                        // aplicando, ni ofrecer repetirlo.
                        fuera.extend(self.decir("msg-sync-apply-unknown"));
                    }
                }
                // Con su parche: `on_apply_abandoned` cambia lo que la
                // pantalla ofrece, y sin repintar, la `a` que acaba de
                // devolverse parece muerta.
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

    /// Pide el catálogo para declarar qué PANELES aportan los plugins.
    ///
    /// Al arrancar y una sola vez: lo que trae es qué huecos existen, no el
    /// contenido de ninguno. Por su propio camino —y no por el de la ayuda o
    /// el del gestor— porque aquellos salen pronto si su superficie está
    /// cerrada, y un panel de plugin tiene que poder colocarse sin que nadie
    /// haya abierto ninguna de las dos (fase 3).
    ///
    /// También en SOLO LECTURA, y es a propósito. La regla de la paleta
    /// —«ofrecer lo que se va a rehusar es prometer algo que no se hará»— no
    /// aplica aquí: esto no ofrece nada, es una LECTURA que trae la
    /// declaración de qué huecos existen, y sin ella una disposición guardada
    /// con un panel de plugin deja un hueco de kind desconocido, que se
    /// coloca con mínimo `(1, 1)`, no se enfoca, no se pinta y no se puede ni
    /// nombrar: una caja en blanco que roba sitio y que el lector no puede
    /// identificar. Lo que sí se gatea por efectos es la INTERACCIÓN del
    /// panel —sus zonas pulsables y sus comandos—, donde la promesa se hace.
    ///
    /// Y sin gate también porque el terminal pregunta siempre: una ventana y
    /// una TUI en solo lectura tienen que enseñar la misma pantalla.
    ///
    /// Fail-soft: si la RPC falla o vence, esta sesión se queda sin paneles
    /// de plugin, que es la pantalla de siempre.
    /// Sin `self` a propósito: desde que no hay puerta de efectos, no depende
    /// de nada del estado.
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

    /// El catálogo llegó al gestor.
    ///
    /// Un fallo también se aplica: deja de estar «cargando» y la lista queda
    /// vacía, que con el aviso apagado significa «no hay ninguna». Quedarse
    /// cargando para siempre sería la única respuesta peor.
    pub(super) fn aplicar_catalogo_de_extensiones(
        &mut self,
        apertura: u64,
        peticion: u64,
        res: Result<norte_proto::methods::PluginListResult, Error>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // De ESTA apertura. «Sigue abierta» no es «es la misma».
        if apertura != self.gen_extensiones {
            return Vec::new();
        }
        // Y la más NUEVA de las que haya en vuelo: un catálogo viejo que
        // aterriza después del nuevo deja la columna «aprobada» diciendo lo
        // de antes, sobre un cambio que ya se hizo.
        if peticion <= self.catalogo_aplicado {
            return Vec::new();
        }
        self.catalogo_aplicado = peticion;
        let Some(e) = self.extensiones.as_mut() else {
            return Vec::new();
        };
        // Un fallo se aplica igual: deja de estar «cargando» con la lista
        // vacía, que ya sabe decirse. Quedarse cargando para siempre es la
        // única respuesta peor.
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

    /// Pide la ficha de la extensión elegida.
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

    /// La ficha llegó.
    pub(super) fn aplicar_ficha(
        &mut self,
        id: &str,
        res: Option<&norte_proto::methods::PluginGetConfigResult>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let lang = self.lang;
        let e = self.extensiones.as_mut()?;
        match res {
            Some(r) => e.set_ficha(id, r, lang),
            // Un fallo también se APLICA: solo salir dejaba `pedida` puesta,
            // así que `reclamar_ficha` devolvía `None` para siempre y esa
            // fila no se podía volver a abrir —`enter` no hacía nada y no
            // decía nada— salvo moviendo el cursor a otra y volviendo. Es el
            // mismo criterio que este fichero ya aplica dos veces al
            // catálogo: quedarse cargando para siempre es la única respuesta
            // peor que un error.
            None => e.cerrar_ficha(),
        }
        let cambio = ViewChange::Extensions {
            extensions: self.vista_extensiones(),
        };
        Some(self.parche(vec![cambio]))
    }

    /// La proyección del gestor.
    pub(super) fn vista_extensiones(&self) -> Option<crate::dto::ExtensionsView> {
        Some(self.extensiones.as_ref()?.vista())
    }

    /// Las teclas mientras el gestor está abierto.
    pub(super) fn tecla_en_extensiones(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        /// Cuántas filas mueve una página.
        const PAGINA: i64 = 10;
        let Some(e) = self.extensiones.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // TRES REGÍMENES, y el orden importa. Mientras se TECLEA un valor,
        // las letras son letras: resolver `a` como «aprobar» ahí convierte
        // escribir la palabra «casa» en dos concesiones de capabilities.
        if e.editando() {
            return self.tecla_editando_config(k, backend, buzon);
        }
        // `Home`/`End` se quedan fijas: el catálogo compartido no tiene verbo
        // para «al principio» dentro de un diálogo.
        let verbo = match k.key.as_str() {
            "Home" | "home" | "End" | "end" => None,
            _ => self.verbo_de_dialogo(k),
        };
        let Some(e) = self.extensiones.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match (verbo.as_deref(), k.key.as_str()) {
            (Some("dialog.cancel"), _) => {
                // El primer `esc` cierra la FICHA, no el gestor: dejar la
                // lista por cerrar un detalle pierde dónde estaba el lector.
                if e.tiene_ficha() {
                    e.cerrar_ficha();
                } else {
                    self.extensiones = None;
                }
            }
            // Con la ficha abierta, las flechas recorren SUS claves: mover el
            // catálogo por debajo tiraría la ficha que se está leyendo.
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
            // Las de página y los extremos, por la misma puerta que las
            // flechas: con la ficha abierta recorren SUS claves, y solo
            // cuando no hay nada que andar caen al catálogo.
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
                // Una rota no tiene ajustes que abrir: se dice, en vez de una
                // tecla que no hace nada.
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
            // Aprobar es `dialog.add` —conceder— y encender/apagar es
            // `dialog.toggle-enabled`: los dos verbos del catálogo que
            // significan justo eso, en vez de dos letras que solo esta
            // ventana conocía.
            (Some("dialog.add"), _) => {
                return self.gobernar_elegida(Cambio::Aprobacion, backend, buzon);
            }
            (Some("dialog.toggle-enabled"), _) => {
                return self.gobernar_elegida(Cambio::Encendido, backend, buzon);
            }
            // Desinstalar es `dialog.remove`, el verbo que en la lista de
            // favoritos quita una entrada: aquí quita la extensión entera, y
            // por eso pregunta antes.
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

    /// Las teclas mientras se TECLEA el valor de una clave.
    ///
    /// Régimen FIJO, como el de cualquier campo de este host: aquí una letra
    /// es una letra. `Enter` confirma —y entonces se escribe—, `Escape`
    /// cancela sin escribir, y el resto de teclas no significan nada.
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
                // Una tecla imprimible es su carácter; cualquier otra —y
                // cualquier combinación con modificador— no es texto.
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

    /// `Enter` sobre una clave: cicla, o abre el buffer para teclearla.
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
        // Un `bool` o un `enum` YA cambiaron de valor en el modelo: lo que
        // queda es contárselo al daemon. Un `string`/`int` solo abrió el
        // buffer y todavía no hay nada que escribir.
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

    /// `Enter` con el buffer abierto: valida y escribe, o dice por qué no.
    pub(super) fn confirmar_config(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // El buffer solo lo abre `activar_clave`, que ya comprueba esto, así
        // que hoy es inalcanzable — igual que `rechaza_por_solo_lectura`, que
        // existe de todas formas. Una puerta que escribe se comprueba en la
        // puerta.
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
            // La validación de ESTE lado no es la que permite —el daemon
            // vuelve a validar contra el esquema— pero decirlo aquí ahorra un
            // viaje y, sobre todo, dice CUÁL era la cota.
            Err(norte_frontend::settings::SettingsEditError::NotAnInt) => (
                ActionAck::Unavailable {
                    reason_key: "host-not-an-int".to_owned(),
                },
                self.decir("host-not-an-int"),
            ),
            Err(norte_frontend::settings::SettingsEditError::OutOfRange { min, max }) => {
                // El aviso lleva las cotas; el ACUSE no puede: nadie
                // sustituye variables en esa clave, así que un `{ $min }` en
                // el acuse se registra literalmente. Dos claves, y la que
                // lleva números es la que sí se traduce con ellos.
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
            // Un campo de plugin no tiene vocabulario cerrado hoy (solo el
            // editor de los ajustes de norte devuelve esto); se dice como
            // cualquier valor rechazado.
            Err(norte_frontend::settings::SettingsEditError::Invalid { .. }) => (
                ActionAck::Unavailable {
                    reason_key: "host-value-rejected".to_owned(),
                },
                self.decir("host-value-rejected"),
            ),
        }
    }

    /// Manda UNA clave al daemon.
    ///
    /// El valor ya está puesto en el modelo (optimismo): lo que corrige un
    /// fallo es REPEDIR la ficha, no adivinar qué había antes.
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

    /// La escritura contestó.
    pub(super) fn aplicar_escritura(
        &mut self,
        apertura: u64,
        id: &str,
        res: Result<(), Error>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Err(e) = res else {
            // Un ajuste que cambió puede cambiar lo que un decorador pinta
            // —el estilo de los iconos, sin ir más lejos—: los listados se
            // vuelven a pedir.
            return self.readornar_todo(backend, buzon);
        };
        let mut fuera = self.decir(norte_frontend::error::error_key(&e));
        if apertura != self.gen_extensiones {
            return fuera;
        }
        // Y se REPIDE la ficha: el valor optimista de la pantalla es ahora
        // mismo una mentira sobre lo que el plugin tiene configurado, y
        // adivinar el anterior es inventarse un tercer estado.
        //
        // Salvo si se está TECLEANDO: repedirla tira el `PluginConfigState`
        // entero, y con él lo que el lector lleva escrito de otra clave. Un
        // valor viejo en pantalla es malo; comerse lo que alguien acaba de
        // teclear, peor — y la corrección llega igual en cuanto cierre el
        // campo.
        if let Some(ext) = self.extensiones.as_mut()
            && ext.es_ficha_de(id)
            && !ext.editando()
        {
            ext.cerrar_ficha();
            // El cierre viaja SIEMPRE en su parche: `pedir_ficha` no manda
            // ninguno por su camino bueno, así que sin esto el renderer
            // seguía pintando una ficha que el host ya no tiene —y las
            // flechas, que ya no la encuentran, movían el catálogo por
            // debajo—.
            let cambio = ViewChange::Extensions {
                extensions: self.vista_extensiones(),
            };
            fuera.push(self.parche(vec![cambio]));
            let (_, partes) = self.pedir_ficha(backend, buzon);
            fuera.extend(partes);
        }
        fuera
    }

    /// `a`/`e` sobre la extensión elegida.
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
            // Una que NO cargó: no hay capabilities que leer ni nada que
            // encender, y lo único que se le puede pedir es que se quite —si
            // su directorio se llama como un id, que es lo que se borra—.
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
            // Conceder PREGUNTA; retirar, no.
            Cambio::Aprobacion if !aprobada => self.preguntar_por_aprobacion(&id),
            Cambio::Aprobacion => {
                let fuera = self.gobernar(&id, Gobierno::Aprobar(false, None), backend, buzon);
                (self.aplicada(), fuera)
            }
            // ENCENDER un plugin sin aprobar no es una decisión que esta
            // pantalla pueda tomar por su cuenta: sin capabilities aprobadas
            // el core no lo va a cargar, y decir «encendido» sobre algo que
            // no corre es la pantalla que miente. APAGARLO sí, siempre: va en
            // la dirección segura, y negarlo dejaba sin poder apagar a una
            // extensión encendida a la que se le acababan de revocar las
            // capabilities —o sea, prohibía justo lo que hay que poder hacer.
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
            // Desinstalar SIEMPRE pregunta: borra ficheros y no tiene vuelta.
            Cambio::Desinstalacion => self.preguntar_por_desinstalacion(&id),
        }
    }

    /// Lo que un BOTÓN hace sobre una fila (puente 61): señalarla y gobernar
    /// la señalada, por el mismo camino que la tecla. Que sea el mismo camino
    /// es el punto: las preguntas —conceder enumera, desinstalar avisa— se
    /// hacen una vez, aquí, y ningún botón las esquiva.
    pub(super) fn gobernar_por_raton(
        &mut self,
        row: u32,
        id: &str,
        cambio: Cambio,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Con un diálogo delante, no: el gestor es modal para el teclado
        // (`input.rs` corta antes de llegar aquí) y tiene que serlo para el
        // ratón, o un clic detrás de la pregunta de consentimiento revocaría
        // sin preguntar, o apilaría una segunda pregunta sobre la primera.
        if !self.dialogos.is_empty() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        // Antes de mover nada: una ventana de solo lectura no repinta un
        // cursor movido por una acción que va a rehusar.
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
            // El cursor se movió con el clic, y eso se pinta aunque lo que
            // sigue sea una pregunta: la fila resaltada es la que el diálogo
            // describe.
            let cambio = ViewChange::Extensions {
                extensions: self.vista_extensiones(),
            };
            fuera.push(self.parche(vec![cambio]));
        }
        (ack, fuera)
    }

    /// Señala la fila que un clic nombra, si sigue siendo la que el
    /// renderer vio. `None` si ya no está o ya no es esa: el catálogo se
    /// repide de fondo y una fila borrada por encima corre las de debajo.
    /// `Some(movio)` dice si el cursor cambió de sitio.
    fn fila_de_extension(
        e: &mut crate::extensions::Extensiones,
        row: u32,
        id: &str,
    ) -> Option<bool> {
        if e.id_de_fila(row as usize)? != id {
            return None;
        }
        // Por la FILA, no por `elegida()`: esa solo mira las cargadas y
        // devuelve `None` para una rota, así que un clic sobre una rota ya
        // señalada decía haber movido el cursor y empujaba un parche entero
        // que no cambiaba nada.
        let movio = e.cursor() != row as usize;
        e.senalar(row as usize);
        Some(movio)
    }

    /// Abre la pregunta de desinstalar, con el nombre y el id dentro.
    ///
    /// El cuerpo dice lo que se pierde: los ficheros de la extensión Y su
    /// consentimiento —uno instalado después bajo el mismo id nace sin él—,
    /// porque «¿desinstalar?» a secas se lee como «¿apagar del todo?», y no
    /// es eso.
    pub(super) fn preguntar_por_desinstalacion(
        &mut self,
        id: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Una que no cargó no tiene nombre de manifiesto: se enseña su
        // directorio, que ya viene saneado y con su bandera.
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
            // `confirm`, como el borrado de ficheros: es la respuesta
            // afirmativa de un diálogo normal, y la ETIQUETA es la que dice
            // qué se confirma. `approve` queda para conceder capabilities.
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

    /// La ayuda de la extensión de esa fila (puente 61): lo que `app.help`
    /// hace sobre la fila elegida en el terminal, y por el mismo molde — el
    /// gestor se cierra y la ayuda se abre con esa página como RAÍZ, con el
    /// catálogo que el gestor ya tenía para que la lateral no espere al
    /// daemon. Sin página se dice y no se abre nada: una ayuda que se abre en
    /// el índice cuando se pidió la de UNA extensión es la ventana
    /// contestando otra pregunta.
    pub(super) fn ayuda_de_extension(
        &mut self,
        row: u32,
        id: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Modal para el ratón como para el teclado: abrir la ayuda cerraría
        // el gestor bajo una pregunta pendiente, y el sí de esa pregunta se
        // encontraría sin catálogo con el que comparar lo que concede.
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
            // El modelo compartido no abre lo que no tiene, y lo hace en
            // silencio: un id que no llegó a ser nodo dejaría al lector en la
            // página del contexto, que no es lo que pidió. Se dice, y el
            // gestor se queda.
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

    /// Abre la pregunta de conceder capabilities, con las capabilities
    /// dentro.
    pub(super) fn preguntar_por_aprobacion(
        &mut self,
        id: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(concesion) = self.extensiones.as_ref().and_then(|e| e.concesion(id)) else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let (nombre, capabilities, ancla) =
            (concesion.nombre, concesion.capabilities, concesion.digest);
        // Una capability por LÍNEA, y el nombre de la extensión aparte: son
        // los operandos de la decisión, y meterlos en la frase es lo que
        // deja a un nombre de tercero imitando el texto de la ventana. Cada
        // una con SU bandera: la que se pinta distinta de lo que dice es
        // justo la que un manifiesto hostil escribe para colarse.
        // Y NINGUNA se recorta. El tope de líneas de un diálogo existe para
        // una lista de rutas de la que sobra ver una parte; aquí la lista ES
        // la concesión, y enseñar dieciséis de cuarenta mientras el sí
        // concede las cuarenta es exactamente el hueco por el que se cuela la
        // capability que nadie leyó. Si son tantas que no caben, no se
        // pregunta: se rehúsa.
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
            // El id reverse-DNS, que es lo ÚNICO que el core valida: dos
            // extensiones pueden llamarse igual, y el nombre que el diálogo
            // enseña lo escribe el manifiesto. Sin esto, la pantalla donde se
            // conceden permisos no dice a quién.
            subject: Some(crate::dto::DialogLine {
                text: clamp_display(id.to_owned()),
                hostile: false,
            }),
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: cuerpo,
            overflow_note: nota,
            // Este diálogo no recorta nada: su cuerpo son las líneas que le
            // dan hechas, no una lista de rutas que se acote.
            overflow_hostile: false,
            choices: vec![
                DialogChoice {
                    id: "approve".to_owned(),
                    label_key: "dialog-approve".to_owned(),
                    // Conceder permisos no borra nada, pero tampoco es la
                    // respuesta inocua de un diálogo cualquiera: se marca
                    // para que el renderer no la pinte como el «Aceptar» de
                    // un aviso.
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

    /// Concede las capabilities LEÍDAS, o vuelve a preguntar si han cambiado.
    ///
    /// El diálogo se queda las TECLAS, no los mensajes de fondo: un catálogo
    /// que aterrice entre la pregunta y el sí puede traer otras capabilities
    /// para esa extensión, y entonces el sí concedería algo que nadie leyó.
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
            // El ancla que viaja es la de LA PREGUNTA, jamás la del catálogo
            // de ahora (#282): releerla aquí certificaría al core «esto es lo
            // que el humano leyó» sobre lo que el humano no leyó, que es
            // exactamente el agujero que el campo cierra. Y la comparación de
            // capabilities de arriba no lo tapa: `category` y `contributions`
            // entran en el ancla y no en la lista pintada.
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

    /// Manda el cambio al daemon. La verdad la dirá el catálogo repedido.
    pub(super) fn gobernar(
        &mut self,
        id: &str,
        que: Gobierno,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let apertura = self.gen_extensiones;
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        let id2 = id.to_owned();
        tokio::spawn(async move {
            let llamada = match que {
                Gobierno::Aprobar(v, digest) => backend2.plugin_set_approval(id2, v, digest),
                Gobierno::Encender(v) => backend2.plugin_set_enabled(id2, v),
                // Si tenía consentimiento no cambia lo que sigue: el catálogo
                // se repide igual, y la pregunta ya lo dijo antes del sí.
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

    /// El cambio de gobierno contestó.
    ///
    /// Con un OK NO se toca el `bool` local: se REPIDE el catálogo. Un
    /// optimismo que el daemon no confirmó es, en esta pantalla, una
    /// afirmación sobre quién puede leer tus ficheros.
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
        // El desenlace se DICE aunque el gestor ya esté cerrado: una
        // concesión que falló y nadie contó es la ventana callándose sobre
        // quién puede leer tus ficheros.
        let mut fuera = match res {
            Ok(()) => self.decir("host-extension-updated"),
            Err(e) => self.decir(norte_frontend::error::error_key(e)),
        };
        // Y el catálogo se repide EN LOS DOS CASOS. El fallo incluye el plazo
        // de ESTE lado, que no es «no pasó» sino «no se sabe»: el daemon pudo
        // conceder las capabilities y tardar en contestar, y entonces dejar
        // la fila diciendo «sin aprobar» es la misma mentira que el optimismo
        // local, en pesimista. Lo único que resuelve un desconocido es ir a
        // preguntar.
        if self.extensiones.is_some() {
            self.repedir_catalogo(backend, buzon);
        }
        // Y los LISTADOS, por lo mismo: lo que un decorador o una columna de
        // plugin dijeron de cada fila lo dijo con el catálogo de antes.
        fuera.extend(self.readornar_todo(backend, buzon));
        fuera
    }

    /// Olvida lo que los plugins dijeron de CADA listado abierto y lo vuelve
    /// a pedir: es lo que sigue a cualquier cambio de gobierno o de ajustes
    /// de un plugin. Apagar el decorador de iconos dejaba los iconos en las
    /// filas hasta el siguiente `cd`, y el lector concluía que apagar no
    /// apaga.
    ///
    /// Una tanda en vuelo no se espera: la generación de adornos sube, y
    /// cuando aterrice se tira y se repide. El parche de filas va YA, con las
    /// filas desnudas, para que la pantalla no siga enseñando lo que el
    /// gestor acaba de decir que no está.
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

    /// Vuelve a pedir el catálogo para la apertura VIVA.
    pub(super) fn repedir_catalogo(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        self.pedir_catalogo_de_extensiones(backend, buzon);
    }

    /// Pide el catálogo para el gestor, numerando la PETICIÓN.
    ///
    /// Dos números y no uno: la APERTURA dice si el gestor sigue siendo el
    /// mismo, y la PETICIÓN cuál de varias en vuelo es la más nueva. Dos
    /// gobiernos seguidos piden dos catálogos dentro de la misma apertura, y
    /// pueden contestar en cualquier orden — sin el segundo número, el viejo
    /// pisaba al nuevo y la columna «aprobada» se quedaba atrás para siempre.
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

    /// La salida de un comando llegó.
    ///
    /// Solo la del ÚLTIMO que se lanzó: dos comandos en vuelo y el lento
    /// aterrizando después pintaría la salida de uno bajo el título del
    /// otro, que en un panel que dice quién imprimió qué es mentir.
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
                // Texto de TERCERO: se ACOTA primero —enmascarar un megabyte
                // para quedarse con cuatro mil caracteres es hacer el trabajo
                // entero por nada—, se parte en líneas, y cada una se
                // enmascara por su cuenta. Que se haya cortado se DICE: el
                // receptor no puede deducirlo, porque lo que le llega ya
                // viene corto.
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

    /// Cierra el panel de salida.
    pub(super) fn cerrar_salida(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.escritorio.salida = None;
        (
            self.aplicada(),
            vec![self.parche(vec![ViewChange::PluginOutput { output: None }])],
        )
    }

    /// Un click en una fila del gestor: la elige.
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
