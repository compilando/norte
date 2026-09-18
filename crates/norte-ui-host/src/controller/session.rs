//! Leer y volcar la sesión.
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
    /// Lee la sesión y la aplica, si se puede.
    ///
    /// Tres cosas se deciden aquí, y las tres son de ADR 0059:
    ///
    /// - **Quién escribe.** Una ventana SUELTA no escribe. La sesión es un
    ///   documento con un solo escritor, y dos ventanas guardando la suya
    ///   encima de la otra es exactamente lo que produce una pantalla que
    ///   nadie pidió.
    /// - **Qué se aplica.** Solo lo que este host entiende. Un hueco de un
    ///   kind desconocido NO se toca, ni siquiera para borrarlo.
    /// - **Qué NO se sobrescribe.** Si lo guardado es de un esquema más
    ///   nuevo, se arranca de la configuración y se deja quieto: arrancar sin
    ///   sesión es recuperable; machacar la de una versión futura no.
    ///
    /// Y la DISPOSICIÓN guardada bajo la clave de este perfil se pone antes
    /// que los huecos, por encima de la de `--layout` y la configuración —el
    /// mismo orden que el terminal: la sesión es más específica que las dos,
    /// porque es cómo estaba la pantalla al cerrar—. Es la D8 de la ADR 0058:
    /// cierra un frontend, abre el otro, sigue donde estabas. Hasta aquí la
    /// ventana no la leía, así que alternar un panel duraba hasta cerrar.
    pub(super) async fn leer_sesion(&mut self, backend: &dyn HostBackend) {
        let Ok((sesion, owner)) = backend.session_get().await else {
            // Sin sesión legible se arranca igual: es memoria de dónde
            // estabas, no un requisito para existir.
            return;
        };
        self.sesion.revision = sesion.revision;
        self.sesion.owner = owner;
        if sesion.version > norte_frontend::session::SCHEMA_VERSION {
            self.sesion.futuro = true;
            return;
        }
        if sesion.version == 0 {
            // Nadie la ha escrito todavía.
            return;
        }
        // Por el constructor que VALIDA, como el terminal, y no por serde a
        // secas: un cuerpo cuya disposición no tenga listado parsea igual, y
        // ponerla dejaría `huecos` vacío y la siguiente tecla en el `expect`
        // de `hueco()` (#242). Un cuerpo que no vale se deja quieto y se
        // arranca de la configuración, como uno del futuro.
        let Ok(body) =
            norte_frontend::session::SessionBody::from_value(sesion.version, &sesion.body)
        else {
            tracing::warn!("la sesión guardada no se entiende: se arranca de la configuración");
            return;
        };
        if let Some(arbol) = body.layouts.get(&self.clave_de_sesion()).cloned() {
            // Sin despertar nada: los listados se piden después, una vez la
            // sesión haya dicho dónde estaba cada uno. Despertarlos aquí
            // pediría el directorio del arranque para tirarlo un instante
            // después.
            self.poner_arbol(arbol, None);
        }
        self.aplicar_sesion(&body);
        self.paleta_recientes.clone_from(&body.palette_recent);
        self.popular = norte_frontend::history::Popular::from_entries(body.popular.clone());
        self.sesion.conocidos = body.slots.keys().copied().collect();
        for (id, estado) in &body.slots {
            self.sesion.touched.insert(*id, estado.touched_ms);
        }
        self.sesion.leida = body;
    }

    /// Bajo qué clave de `layouts` va la pantalla de esta ventana.
    ///
    /// La misma que el terminal (`App::session_key`): el nombre del perfil
    /// activo, o `default` sin ninguno. Un perfil cuyo directorio no sea UTF-8
    /// cae a `default`, que es lo que el selector avisa con `carries_state`.
    pub(super) fn clave_de_sesion(&self) -> String {
        self.perfil_activo
            .as_ref()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
            .map_or_else(|| "default".to_owned(), ToOwned::to_owned)
    }

    /// Un tic de un segundo sobre el aviso de la barra (spec 2026-09-10,
    /// `[ui] notice_seconds`): pasado el tope, el mensaje sale de la barra,
    /// va al registro por `tracing` y `notices_unread` cuenta uno más. Con
    /// `0` no caduca nada. Abrir el panel de registro pone la cuenta a
    /// cero. Devuelve el parche de estado si algo cambió; los tests lo hacen
    /// avanzar tic a tic, sin reloj.
    ///
    /// La cuenta va por TEXTO: repetir la misma acción dentro del plazo no la
    /// reinicia (revisión m10). Reiniciarla al asignar pediría un setter en
    /// los ~40 sitios que escriben `status.message`; se deja dicho.
    pub(super) fn caducar_aviso(&mut self) -> Option<BridgeEnvelope<UiUpdate>> {
        let mut cambio = false;
        let registro_abierto = self
            .arbol
            .slot_ids()
            .into_iter()
            .any(|id| self.arbol.kind_of(id).is_some_and(|k| k.as_str() == "log"));
        if registro_abierto && self.status.notices_unread != 0 {
            self.status.notices_unread = 0;
            cambio = true;
        }
        match self.status.message.as_deref() {
            None => {
                self.mensaje_ticks = 0;
                self.mensaje_contado = None;
            }
            Some(msg) => {
                if self.mensaje_contado.as_deref() == Some(msg) {
                    self.mensaje_ticks = self.mensaje_ticks.saturating_add(1);
                } else {
                    self.mensaje_contado = Some(msg.to_owned());
                    self.mensaje_ticks = 1;
                }
                let tope = self.config.common.ui_chrome.notice_seconds();
                if tope > 0 && self.mensaje_ticks >= tope {
                    let text = self.status.message.take().unwrap_or_default();
                    self.mensaje_ticks = 0;
                    self.mensaje_contado = None;
                    self.status.notices_unread = self.status.notices_unread.saturating_add(1);
                    // `info`, no `warn`: «copiado 1 fichero» no es un aviso, y
                    // el nivel es por lo que se filtra el panel de registro.
                    tracing::info!(target: "norte::notice", "{text}");
                    cambio = true;
                }
            }
        }
        cambio.then(|| self.parche(vec![ViewChange::Status(self.status.clone())]))
    }

    /// Mira si la pantalla cambió desde lo último escrito y, si cambió, la
    /// escribe FUERA del actor. Es el tic de la sesión, y también lo que
    /// cada cambio del árbol llama sin esperar al tic.
    ///
    /// Una ventana suelta no escribe; una sesión del futuro no se machaca; y
    /// con un diálogo delante no se guarda lo que se está decidiendo, como en
    /// el terminal. Con un `put` en vuelo se espera a que conteste: dos
    /// escrituras cruzadas con la misma revisión son un conflicto seguro.
    pub(super) fn empujar_sesion(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        if !self.sesion.owner
            || self.sesion.futuro
            || self.sesion.en_vuelo.is_some()
            || !self.dialogos.is_empty()
        {
            return;
        }
        let ahora = u64::try_from(ahora_ms()).unwrap_or(0);
        let mut body = self.capturar_sesion();
        if self.sesion.sin_historial {
            body.degrade_for_size();
        }
        let vivos: Vec<SlotId> = self.huecos.keys().map(|id| SlotId(*id)).collect();
        let Some(sellados) = self.sesion.policy.prepare(&mut body, &vivos, ahora) else {
            return;
        };
        for SlotId(id) in sellados {
            self.sesion.touched.insert(id, ahora);
        }
        let Ok(json) = serde_json::to_value(&body) else {
            return;
        };
        let cuerpo = std::sync::Arc::new(body);
        self.sesion.en_vuelo = Some(std::sync::Arc::clone(&cuerpo));
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let revision = self.sesion.revision;
        tokio::spawn(async move {
            let res = backend
                .session_put(norte_frontend::session::SCHEMA_VERSION, revision, json)
                .await;
            let _ = buzon
                .send(Mensaje::SesionPuesta(Box::new((res, cuerpo))))
                .await;
        });
    }

    /// Pide el RELEVO a la terminal (fase 9): vuelca la pantalla CON las
    /// marcas y suelta la sesión.
    ///
    /// Las dos cosas van spawneadas y en ese orden: soltar antes de escribir
    /// dejaría a la terminal leyendo la pantalla de hace un segundo, y
    /// escribir sin soltar la dejaría sin poder escribir la suya. El
    /// desenlace vuelve por el buzón ([`Mensaje::Relevado`]), que es donde se
    /// decide si se lanza la terminal o si esto se queda como estaba.
    ///
    /// **Sólo la DUEÑA releva.** Una ventana suelta no tiene la pantalla que
    /// entregar, y soltar lo ajeno no hace nada: ofrecerlo igualmente sería
    /// prometer un relevo que se queda a medias, con la terminal abierta
    /// sobre el listado de otro.
    pub(super) fn pedir_relevo(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if !self.sesion.owner {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-handoff-not-owner".to_owned(),
                },
                Vec::new(),
            );
        }
        let body = self.capturar_sesion_para_relevo();
        let Ok(json) = serde_json::to_value(&body) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-handoff-failed".to_owned(),
                },
                Vec::new(),
            );
        };
        let revision = self.sesion.revision;
        let b = Arc::clone(backend);
        let buz = buzon.clone();
        tokio::spawn(async move {
            // Sólo se suelta si la pantalla ENTRÓ: soltarla tras un `put` que
            // falló dejaría a la terminal reclamando un cuerpo viejo, que es
            // peor que no relevar.
            let escrito = b
                .session_put(norte_frontend::session::SCHEMA_VERSION, revision, json)
                .await
                .is_ok();
            let soltada = if escrito {
                b.session_release().await.unwrap_or(false)
            } else {
                false
            };
            let _ = buz.send(Mensaje::Relevado { soltada }).await;
        });
        (self.aplicada(), self.decir("msg-handoff-running"))
    }

    /// El relevo contestó: se entrega la pantalla o no ha pasado nada.
    pub(super) fn relevo_terminado(&mut self, soltada: bool) -> Vec<BridgeEnvelope<UiUpdate>> {
        if !soltada {
            return self.decir("msg-handoff-failed");
        }
        // Ya no somos dueños: dejar de escribir es lo honesto, y el indicador
        // de la barra lo dice solo.
        self.sesion.owner = false;
        // Lanzar la terminal y cerrarse es de quien hospeda. Si no puede, lo
        // dice y NO se cierra: la sesión está suelta pero la pantalla sigue
        // aquí, que es el fallo barato.
        if !self.nativo(crate::dto::NativeEffect::HandoffToTerminal { daemon: true }) {
            return self.decir("msg-handoff-no-terminal");
        }
        self.decir("msg-handoff-running")
    }

    /// El `session.put` del tic contestó.
    ///
    /// Cuatro respuestas, y cada una dice algo distinto: entró, y lo mandado
    /// pasa a ser lo último escrito; otra ventana escribió en medio, y se
    /// relee para escribir sobre su revisión; el cuerpo no cabe, y desde
    /// ahora va sin historial; esta ventana ya no es la dueña, y lo dice el
    /// indicador. Lo demás se apunta y se reintenta en el tic siguiente.
    pub(super) fn sesion_puesta(
        &mut self,
        res: Result<u64, Error>,
        cuerpo: std::sync::Arc<norte_frontend::session::SessionBody>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.sesion.en_vuelo = None;
        match res {
            Ok(rev) => {
                self.sesion.revision = rev;
                self.sesion.policy.sent(cuerpo);
                Vec::new()
            }
            Err(Error::Conflict { .. }) => {
                self.sesion.policy.resend();
                let backend = Arc::clone(backend);
                let buzon = buzon.clone();
                // La relectura vuelve por el buzón como todo lo demás, y
                // mientras vuela no se escribe: lo que conflictó sigue
                // pendiente hasta saber sobre qué revisión va.
                self.sesion.en_vuelo = Some(cuerpo);
                tokio::spawn(async move {
                    let res = backend.session_get().await;
                    let _ = buzon.send(Mensaje::SesionReleida(res)).await;
                });
                Vec::new()
            }
            Err(Error::LimitExceeded { .. }) => {
                self.sesion.policy.resend();
                self.sesion.sin_historial = true;
                Vec::new()
            }
            Err(Error::PermissionDenied) => {
                self.sesion.policy.resend();
                self.sesion.owner = false;
                let cambio = self.cambio_de_banners();
                vec![self.parche(vec![cambio])]
            }
            Err(e) => {
                tracing::warn!(error = %e, "la sesión no se pudo escribir; se reintenta");
                self.sesion.policy.resend();
                Vec::new()
            }
        }
    }

    /// La sesión releída tras un conflicto: se toma su revisión y se conserva
    /// lo ajeno, SIN aplicarla a la pantalla —esta ventana es la que acaba de
    /// moverse, y lo suyo va encima en el siguiente tic.
    ///
    /// La revisión solo avanza si el cuerpo se entiende: avanzar con un
    /// cuerpo que no se pudo leer escribiría lo LEÍDO ANTES sobre una
    /// revisión que ya no lo refleja, y eso pisa lo que la otra ventana
    /// acaba de guardar. Sin cuerpo, el siguiente tic vuelve a conflictar y a
    /// releer, que es lo honesto. Un cuerpo del FUTURO apaga la escritura del
    /// todo, como al arrancar.
    pub(super) fn sesion_releida(
        &mut self,
        res: Result<(norte_proto::methods::Session, bool), Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.sesion.en_vuelo = None;
        let Ok((sesion, owner)) = res else {
            return Vec::new();
        };
        let era_duena = self.sesion.owner;
        self.sesion.owner = owner;
        match norte_frontend::session::SessionBody::from_value(sesion.version, &sesion.body) {
            Ok(body) => {
                self.sesion.revision = sesion.revision;
                self.sesion.leida = body;
            }
            Err(norte_frontend::session::SessionError::FromTheFuture { .. }) => {
                self.sesion.futuro = true;
            }
            Err(e) => {
                tracing::warn!(error = %e, "la sesión releída no se entiende; se reintenta");
            }
        }
        if era_duena == owner && !self.sesion.futuro {
            return Vec::new();
        }
        let cambio = self.cambio_de_banners();
        vec![self.parche(vec![cambio])]
    }

    /// Los huecos que `[profile.start]` siembra, ya filtrados a los que ESTA
    /// pantalla tiene.
    ///
    /// Quién gana lo decide [`norte_frontend::config::profile_start_seeds`],
    /// que es de los dos frontends: la sesión manda, y el perfil solo dice
    /// dónde abre un hueco del que la sesión no sabe nada. Un id que el perfil
    /// nombre y esta disposición no coloque no tiene dónde abrir, así que se
    /// cae aquí.
    ///
    /// Apunta lo sembrado. Sin esa cuenta, un lector sin sesión guardada
    /// —instalación nueva, o un `session_get` que ni se pudo leer— volvía al
    /// directorio de arranque del perfil cada vez que entraba y salía de él:
    /// para él la sesión no sabe nunca nada, así que el veto de arriba no veta.
    pub(super) fn siembra_de_perfil(&mut self) -> Vec<(u32, VPath)> {
        let siembra: Vec<(u32, VPath)> = norte_frontend::config::profile_start_seeds(
            &self.config.common.profile_start,
            &self.sesion.conocidos,
            &self.sesion.sembrados,
        )
        .into_iter()
        .filter(|(id, _)| self.huecos.contains_key(id))
        .collect();
        for (id, _) in &siembra {
            self.sesion.sembrados.insert(*id);
        }
        // Un id que el perfil nombra y esta disposición no coloca no tiene
        // dónde abrir. Se DICE, como en el terminal: callarlo es la misma
        // clase de silencio que la clave entera tenía antes de la ADR 0098.
        let colocados: std::collections::BTreeSet<u32> = self.huecos.keys().copied().collect();
        let huerfanos = norte_frontend::config::profile_start_huerfanos(
            &self.config.common.profile_start,
            &colocados,
        );
        if !huerfanos.is_empty() {
            let ids: Vec<String> = huerfanos.iter().map(u32::to_string).collect();
            self.status.message = Some(clamp_display(norte_i18n::ta_in(
                self.lang,
                "msg-profile-start-orphans",
                &[
                    ("n", &huerfanos.len().to_string()),
                    ("ids", &ids.join(", ")),
                ],
            )));
        }
        siembra
    }

    /// Devuelve el panel ACTIVO al directorio que se escribió al arrancar.
    ///
    /// Va DESPUÉS de aplicar la sesión, y ése es todo el arreglo: la sesión
    /// escribe el sitio de todos los huecos, así que un argumento de la línea
    /// de órdenes solo puede ganar volviendo a ponerlo encima. Un directorio
    /// que alguien acaba de teclear es más específico que dónde cerró ayer —
    /// la misma regla que hace que `--layout` gane a `[ui] layout`.
    ///
    /// Solo el activo: el otro panel se queda donde la sesión lo dejó. Y solo
    /// el SITIO — el orden y los ocultos son preferencias, y no se tocan.
    pub(super) fn fijar_dir_pedido(&mut self) {
        let Some(dir) = self.dir_pedido.take() else {
            return;
        };
        let activo = self.activo();
        if let Some(hueco) = self.huecos.get_mut(&activo) {
            hueco.pane.begin_loading(dir);
        }
    }

    /// Coloca cada hueco donde la sesión dice que estaba.
    pub(super) fn aplicar_sesion(&mut self, body: &norte_frontend::session::SessionBody) {
        // El tope ANTES de sembrar (rust-reviewer MAJOR, fase 1): un hueco nace
        // con el de fábrica, y sembrar con él recortaba a 30 una historia de 64
        // que la sesión guardó entera — y la siguiente escritura lo perpetuaba.
        let tope = self.config.common.ui_chrome.history_size();
        for (id, hueco) in &mut self.huecos {
            hueco.historial.set_capacity(tope);
            let Some(estado) = body.slots.get(id) else {
                continue;
            };
            hueco.pane.begin_loading(estado.path.clone());
            // El orden y los ocultos se ESCRIBÍAN en la sesión y no los leía
            // nadie: la ventana se acordaba de dónde estabas y olvidaba cómo
            // lo estabas mirando, así que ordenar por tamaño o apartar los
            // dotfiles duraba hasta cerrar.
            hueco.pane.set_sort(estado.sort);
            hueco.pane.set_show_hidden(estado.show_hidden);
            hueco
                .historial
                .seed(estado.back.clone(), estado.forward.clone());
            hueco.historial.seed_jump(estado.jump.clone());
            // Las marcas de un RELEVO (fase 9), y sólo con `--attach`. Van a
            // `marcas_a_restaurar`, el mecanismo con el que un refresco ya
            // conserva la selección: `aterriza_en` lo consume TRAS
            // `set_listing` —que limpia lo marcado— y por `restore_marks`, que
            // pasa por el embudo de la fila `..`.
            //
            // Por ahí y no por un camino propio, y es la lección de este
            // arreglo: una primera versión las sembraba en
            // `aterrizar_listado`, y el listado del ARRANQUE no pasa por ahí
            // —va por `listar_inicial`—, así que nunca llegaban. `aterriza_en`
            // es por donde pasan todos.
            if self.attach && !estado.marks.is_empty() {
                hueco.marcas_a_restaurar.clone_from(&estado.marks);
            }
        }
    }

    /// La pantalla de AHORA como cuerpo de sesión.
    ///
    /// Las MARCAS no entran: son una selección de trabajo, no un sitio donde
    /// estabas, y restaurarlas haría que una ventana nueva abriese con media
    /// docena de ficheros elegidos que nadie eligió.
    pub(super) fn capturar_sesion(&self) -> norte_frontend::session::SessionBody {
        self.capturar_sesion_con_marcas(false)
    }

    /// La misma pantalla CON lo marcado (fase 9): lo que se vuelca para un
    /// relevo entre frontends.
    ///
    /// La diferencia con su hermana es la única que importa: en un relevo
    /// pasan segundos entre soltar y reclamar, así que devolver lo señalado es
    /// devolver el trabajo que se estaba haciendo. En un arranque cualquiera
    /// han pasado horas, y el razonamiento de arriba sigue en pie.
    pub(super) fn capturar_sesion_para_relevo(&self) -> norte_frontend::session::SessionBody {
        self.capturar_sesion_con_marcas(true)
    }

    fn capturar_sesion_con_marcas(&self, marcas: bool) -> norte_frontend::session::SessionBody {
        // Se parte de lo LEÍDO y se pisa solo lo propio: los huecos de otro
        // frontend y las disposiciones de los OTROS perfiles siguen ahí.
        //
        // La disposición de esta ventana va bajo la clave de su perfil, como
        // la del terminal: la D8 de la ADR 0058 —cierra un frontend, abre el
        // otro, sigue donde estabas— y lo que el lector espera al volver a
        // abrir: los paneles que dejó abiertos. Se dejó de escribir una vez
        // por miedo a que curiosear en el selector cambiara el arranque del
        // terminal, pero eso ES compartir la pantalla, y lo que la D5 protege
        // es otra cosa: que el TAMAÑO de una ventana no reescriba el árbol.
        let mut body = self.sesion.leida.clone();
        body.layouts
            .insert(self.clave_de_sesion(), self.arbol.clone());
        body.palette_recent.clone_from(&self.paleta_recientes);
        body.popular = self.popular.entries().to_vec();
        for (id, hueco) in &self.huecos {
            body.slots.insert(
                *id,
                norte_frontend::session::SlotState {
                    path: hueco.pane.dir().clone(),
                    cursor: hueco.pane.cursor() as u64,
                    back: hueco.historial.trail().to_vec(),
                    forward: hueco.historial.forward_trail().to_vec(),
                    jump: hueco.historial.jump().cloned(),
                    sort: hueco.pane.sort(),
                    columns: Vec::new(),
                    show_hidden: hueco.pane.show_hidden(),
                    // El sello de edad tal como se ESCRIBIÓ la última vez,
                    // no «ahora»: sellar cada captura con el reloj hacía que
                    // ningún cuerpo fuera igual al anterior y el tic escribía
                    // cada segundo. Quien cambia lo sella la política al
                    // preparar el cuerpo, y `touched` recuerda el sello. Un
                    // hueco nunca sellado va a cero y lo sella la primera
                    // escritura, que es lo que hace el terminal.
                    touched_ms: self.sesion.touched.get(id).copied().unwrap_or(0),
                    // Por RUTA, que es la identidad de la fila: un índice
                    // restaurado sobre un listado que cambió señala otro
                    // fichero. El tope es del modelo.
                    marks: if marcas {
                        hueco
                            .pane
                            .marked_entries()
                            .iter()
                            .take(norte_frontend::session::MARKS_CAP)
                            .map(|e| e.path.clone())
                            .collect()
                    } else {
                        Vec::new()
                    },
                },
            );
        }
        body
    }
}
