//! El plan de renombrado que propone un modelo, y su revisión.
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
    /// Abre el prompt de la instrucción para un plan de renombrado.
    ///
    /// Lo que se teclea NO es un nombre: es lo que se le pide a un modelo.
    /// Nada muta aquí, y por eso el prompt no lleva la disciplina de bytes
    /// que lleva el de renombrar — el texto es para el daemon, no para el
    /// disco.
    pub(super) fn pedir_instruccion_ia(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let dir = self.hueco().pane.dir().clone();
        self.pedir_instruccion_ia_sobre(dir)
    }

    /// Como [`Self::pedir_instruccion_ia`], pero sobre un directorio DADO.
    ///
    /// Existe para reabrir el campo tras una instrucción vacía: ahí el
    /// operando ya está en la mano, y volver a derivarlo del hueco activo
    /// sería reabrir sobre otro sitio si algo lo movió por debajo.
    pub(super) fn pedir_instruccion_ia_sobre(
        &mut self,
        dir: VPath,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-ai-rename".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![Self::linea_de_ruta(&dir)],
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
            input: Some(String::new()),
            input_hostile: false,
            input_secret: false,
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            tecleado: Tecleado::Texto(String::new()),
            reconocido: true,
            al_confirmar: Some(Pendiente::InstruccionIa { dir }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Abre el prompt de la PLANTILLA del renombrado en lote (#310),
    /// prellenado con `[N].[E]` —el nombre tal y como está— o con lo que se
    /// tecleó si se vuelve a abrir tras un diagnóstico.
    ///
    /// Prellenar con la identidad y no en blanco, como la TUI: así lo primero
    /// que se ve es la forma que tiene una plantilla. Los nombres sobre los
    /// que actúa se fijan AQUÍ —lo marcado, o el del cursor—, el mismo
    /// operando que cualquier otra operación; solo los que son texto, porque
    /// un par del plan viaja UTF-8 por protocolo.
    pub(super) fn pedir_plantilla_de_lote(
        &mut self,
        siembra: Option<String>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let dir = self.hueco().pane.dir().clone();
        let nombres: Vec<String> = self
            .hueco()
            .pane
            .marked_paths()
            .iter()
            .filter_map(|p| p.file_name())
            .filter_map(|s| String::from_utf8(s.as_bytes().to_vec()).ok())
            .collect();
        if nombres.is_empty() {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-rename-batch-nothing".to_owned(),
                },
                Vec::new(),
            );
        }
        let texto = siembra.unwrap_or_else(|| "[N].[E]".to_owned());
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-rename-batch".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![
                Self::linea_de_ruta(&dir),
                crate::dto::DialogLine {
                    text: clamp_display(norte_i18n::t_in(self.lang, "modal-rename-batch-hint")),
                    hostile: false,
                },
            ],
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
            input: Some(texto.clone()),
            input_hostile: false,
            input_secret: false,
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            tecleado: Tecleado::Texto(texto),
            reconocido: true,
            al_confirmar: Some(Pendiente::PlantillaLote { dir, nombres }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Genera el plan de la plantilla y lo mete en la MISMA revisión que el
    /// de la IA (#310): lo que hace segura la operación no es de dónde
    /// salieron los nombres.
    ///
    /// Una plantilla que no sirve se explica en la barra y el prompt se
    /// vuelve a abrir con lo tecleado, en vez de tirar el texto: la TUI lo
    /// deja abierto con el diagnóstico debajo, y esto es lo mismo con
    /// diálogos que se cierran al confirmar.
    pub(super) fn lanzar_plan_de_plantilla(
        &mut self,
        dir: VPath,
        nombres: &[String],
        plantilla: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let texto = plantilla.trim().to_owned();
        if let Err(e) = norte_frontend::rename_pattern::check(&texto, nombres) {
            let clave = norte_frontend::rename_pattern::error_key(e);
            self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
            let mut salidas = vec![self.parche(vec![ViewChange::Status(self.status.clone())])];
            let (_, reabierto) = self.pedir_plantilla_de_lote(Some(texto));
            salidas.extend(reabierto);
            return salidas;
        }
        // Los pares que NO cambian se descartan, como en la TUI: un plan de
        // identidad no renombra nada, y confirmar sin tocar la plantilla es
        // inocuo.
        let entradas: Vec<norte_proto::methods::AiRenameEntry> =
            norte_frontend::rename_pattern::plan(&texto, nombres, 1)
                .into_iter()
                .filter(|(from, to)| from != to)
                .map(|(from, to)| norte_proto::methods::AiRenameEntry { from, to })
                .collect();
        if entradas.is_empty() {
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "msg-rename-batch-no-changes",
            )));
            return vec![self.parche(vec![ViewChange::Status(self.status.clone())])];
        }
        // CONTRA el directorio que se planeó, con el mismo cinturón que el
        // plan del modelo: un `from` que no esté ahí no entra.
        let del_dir: Vec<Vec<u8>> = self
            .hueco()
            .pane
            .entries()
            .iter()
            .filter_map(|e| e.path.file_name().map(|s| s.as_bytes().to_vec()))
            .collect();
        self.epoca_ia += 1;
        let epoca = self.epoca_ia;
        let Some(parejas) = norte_frontend::rename_pairs_in(&entradas, Some(&del_dir)) else {
            return self.decir_de_ia(epoca, "msg-ai-rename-invalid-plan");
        };
        self.abrir_revision(epoca, dir, entradas, parejas, backend, buzon)
    }

    /// Una fila de RENAMER de la paleta (C3, ADR 0095): le pide el plan al
    /// plugin sobre lo marcado —o lo señalado—, y la respuesta entra por
    /// `Fondo::PlanIa`, que es el camino del plan de la IA: misma revisión,
    /// mismo veredicto del core, mismo `plan_hash`. Lo que hace segura la
    /// operación no es quién propuso los nombres.
    pub(super) fn ejecutar_de_renamer(
        &mut self,
        clave: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some((id, renamer)) = norte_frontend::palette::parse_renamer_key(clave) else {
            return self.no_implementado(clave);
        };
        if self.efectos == crate::commands::Efectos::SoloLectura {
            // El plan acaba en un rename: una ventana sin efectos no lo pide.
            return Self::no_muta();
        }
        let nombres: Vec<String> = self
            .hueco()
            .pane
            .marked_paths()
            .iter()
            .filter_map(|p| p.file_name())
            .filter_map(|s| String::from_utf8(s.as_bytes().to_vec()).ok())
            .collect();
        if nombres.is_empty() {
            let dicho = self.decir("msg-rename-batch-nothing");
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-rename-batch-nothing".to_owned(),
                },
                dicho,
            );
        }
        let dir = self.hueco().pane.dir().clone();
        self.epoca_ia += 1;
        let epoca = self.epoca_ia;
        let del_dir: Vec<Vec<u8>> = self
            .hueco()
            .pane
            .entries()
            .iter()
            .filter_map(|e| e.path.file_name().map(|s| s.as_bytes().to_vec()))
            .collect();
        self.ia_en_vuelo = Some((epoca, dir.clone(), del_dir));
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let (id, renamer) = (id.to_owned(), renamer.to_owned());
        tokio::spawn(async move {
            let res = (tokio::time::timeout(
                PLAZO_IA,
                backend.plugin_rename_plan(id, renamer, dir, nombres),
            )
            .await)
                .unwrap_or(Err(Error::ProviderUnavailable { retryable: true }));
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::PlanIa(
                    epoca,
                    Box::new(res),
                ))))
                .await;
        });
        (self.aplicada(), self.decir("host-plan-asking"))
    }

    /// Le pide el plan al modelo. La respuesta vuelve al actor.
    ///
    /// Una época nueva por petición: entre pedirlo y que llegue, el lector
    /// puede haber descartado la revisión o haber pedido otra, y un plan
    /// viejo no se abre encima del que hay.
    pub(super) fn lanzar_plan_ia(
        &mut self,
        dir: VPath,
        instruccion: String,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if instruccion.trim().is_empty() {
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "modal-ai-rename-empty-instruction",
            )));
            // Y el campo VUELVE. El terminal deja el modal abierto con el
            // error debajo; aquí el diálogo ya se había ido de la pila, así
            // que un mensaje pidiendo que escribas una instrucción sobre una
            // pantalla sin dónde escribirla no era una negativa: era un
            // callejón. El campo estaba vacío, así que no se pierde nada al
            // rehacerlo — y esto es exactamente lo que su caso gemelo, la
            // consulta semántica, ya hacía tres ficheros más allá.
            let (_, mut fuera) = self.pedir_instruccion_ia_sobre(dir);
            let cambio = ViewChange::Status(self.status.clone());
            fuera.push(self.parche(vec![cambio]));
            return fuera;
        }
        self.epoca_ia += 1;
        let epoca = self.epoca_ia;
        // Los nombres del directorio que se PLANEA, guardados con la
        // petición: el cinturón de #275 exige que cada `from` exista donde se
        // va a aplicar, y para cuando el modelo conteste el lector puede
        // estar en otro sitio. Preguntarle al panel entonces validaría el
        // plan contra un directorio que no es el suyo.
        let nombres: Vec<Vec<u8>> = self
            .hueco()
            .pane
            .entries()
            .iter()
            .filter_map(|e| e.path.file_name().map(|s| s.as_bytes().to_vec()))
            .collect();
        self.ia_en_vuelo = Some((epoca, dir.clone(), nombres));
        // Lo MARCADO, si hay marcas (#121): un plan sobre cinco ficheros no
        // puede mandar los mil del directorio al proveedor. Los nombres que no
        // son UTF-8 se quedan fuera —el wire los lleva como texto y el engine
        // los rechaza fail-loud antes de enviar nada—, así que marcarlos y
        // pedir un plan es pedirlo sobre los demás, no sobre el directorio
        // entero.
        // `marked_entries` y NO `marked_paths`: el segundo cae al cursor
        // cuando no hay marcas, y aquí eso convertiría «sin marcar nada» —que
        // significa el directorio entero— en «este fichero suelto».
        let marcados: Vec<String> = self
            .hueco()
            .pane
            .marked_entries()
            .iter()
            .filter_map(|e| e.path.file_name())
            .filter_map(|s| String::from_utf8(s.as_bytes().to_vec()).ok())
            .collect();
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = (tokio::time::timeout(
                PLAZO_IA,
                backend.ai_rename_plan(dir, instruccion, marcados),
            )
            .await)
                .unwrap_or(Err(Error::ProviderUnavailable { retryable: true }));
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::PlanIa(
                    epoca,
                    Box::new(res),
                ))))
                .await;
        });
        // Y se DICE que se está pidiendo. Sin esto la tecla no producía nada
        // visible, así que el lector la volvía a pulsar — que es justo lo que
        // destapaba la carrera de las dos peticiones.
        self.status.message = Some(clamp_display(norte_i18n::t_in(
            self.lang,
            "host-plan-asking",
        )));
        let cambio = ViewChange::Status(self.status.clone());
        vec![self.parche(vec![cambio])]
    }

    /// Lo que contestó el modelo, revisado antes de enseñarlo.
    ///
    /// Dos cinturones, y los dos son de INGESTIÓN —no de presentación—, así
    /// que rechazan EN BLOQUE y ni abren la revisión:
    ///
    /// - un plan con más parejas de las que un directorio puede tener delata
    ///   a un daemon hostil inflando la respuesta;
    /// - una pareja que no es un `Segment` legal delata a uno roto o
    ///   adulterado, y aplicar «lo que valga» de un plan adulterado es
    ///   exactamente lo que no se puede hacer.
    pub(super) fn aplicar_plan_ia(
        &mut self,
        epoca: u64,
        res: Result<norte_proto::methods::AiRenamePlanResult, Error>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // La petición EN VUELO tiene que ser esta. Un plan de otra época es
        // uno que el lector abandonó, y abrirlo es la aplicación moviéndose
        // sola.
        // `take_if` y NO `take().filter(...)`: `take` vacía el hueco ANTES de
        // que el filtro mire, así que una respuesta VIEJA se llevaba por
        // delante la petición VIVA. La secuencia era normal —pedir, no ver
        // nada, volver a pedir— y se quedaban las dos sin abrir, sin decir
        // nada y sin poder distinguirse de un daemon muerto.
        let Some((_, dir, nombres)) = self.ia_en_vuelo.take_if(|(e, _, _)| *e == epoca) else {
            return Vec::new();
        };
        let plan = match res {
            Ok(p) => p,
            Err(e) => return self.decir_de_ia(epoca, norte_frontend::error::error_key(&e)),
        };
        // El productor dijo POR QUÉ no propone (#332): un renamer que
        // rehusó. La frase viene ya enmascarada y acotada por el daemon, y
        // aquí se enseña, no se interpreta.
        if let Some(why) = plan.refused {
            self.status.message = Some(clamp_display(norte_i18n::ta_in(
                self.lang,
                "msg-rename-plan-refused",
                &[("why", &why)],
            )));
            let mut cambios = vec![ViewChange::Status(self.status.clone())];
            if self.revision_ia.take_if(|r| r.epoca == epoca).is_some() {
                cambios.push(ViewChange::AiRename { ai_rename: None });
            }
            return vec![self.parche(cambios)];
        }
        if plan.entries.is_empty() {
            return self.decir_de_ia(epoca, "msg-ai-rename-empty");
        }
        if plan.entries.len() > norte_frontend::MAX_AI_PLAN_ENTRIES {
            return self.decir_de_ia(epoca, "msg-ai-rename-invalid-plan");
        }
        // CONTRA el directorio que se PLANEÓ (#275), no contra lo que el
        // panel enseñe ahora: un plan adulterado no puede renombrar algo que
        // no estaba ahí, y el lector puede haberse ido a otro sitio mientras
        // el modelo pensaba.
        let Some(parejas) = norte_frontend::rename_pairs_in(&plan.entries, Some(&nombres)) else {
            return self.decir_de_ia(epoca, "msg-ai-rename-invalid-plan");
        };
        self.abrir_revision(epoca, dir, plan.entries, parejas, backend, buzon)
    }

    /// Abre la revisión de un plan —del modelo o de una plantilla (#310)— y
    /// le pide al core el veredicto EN EL MISMO viaje: la revisión necesita
    /// el `plan_hash` para que aprobar haga algo, y un plan que se quedara
    /// esperando a que alguien se lo pidiera después no tendría quién. Va
    /// spawneado porque contra un directorio enorme es un `fs.list` entero,
    /// y esperarlo aquí congelaría el actor.
    fn abrir_revision(
        &mut self,
        epoca: u64,
        dir: VPath,
        entradas: Vec<norte_proto::methods::AiRenameEntry>,
        parejas: Vec<norte_proto::methods::RenamePair>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let b = Arc::clone(backend);
        let buz = buzon.clone();
        let d = dir.clone();
        let p = parejas.clone();
        tokio::spawn(async move {
            let res = b.rename_batch_plan(d, p).await;
            let _ = buz
                .send(Mensaje::Fondo(Box::new(Fondo::PlanDeLote(
                    epoca,
                    Box::new(res),
                ))))
                .await;
        });
        self.revision_ia = Some(RevisionIa {
            dir,
            entradas,
            parejas,
            plan: norte_frontend::BatchPlan::Pending,
            primera: 0,
            visto_hasta: norte_frontend::AI_RENAME_PAIR_LIMIT,
            reconocida: false,
            epoca,
        });
        let cambio = ViewChange::AiRename {
            ai_rename: self.vista_ia(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// El veredicto del core sobre el plan que hay en revisión.
    pub(super) fn aplicar_plan_de_lote(
        &mut self,
        epoca: u64,
        res: Result<norte_proto::methods::FsRenameBatchPlanResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(r) = self.revision_ia.as_mut().filter(|r| r.epoca == epoca) else {
            return Vec::new();
        };
        let fallo = res.as_ref().err().map(norte_frontend::error::error_key);
        r.plan = match res {
            Ok(p) => norte_frontend::BatchPlan::Ready(Box::new(p)),
            // `Failed` no es «no aplicable»: es «no hay plan», o sea que no
            // hay `plan_hash` aprobado que mandar. El motivo concreto va a la
            // barra; aquí solo se sabe que aprobar no puede hacer nada.
            Err(_) => norte_frontend::BatchPlan::Failed,
        };
        let mut cambios = vec![ViewChange::AiRename {
            ai_rename: self.vista_ia(),
        }];
        if let Some(clave) = fallo {
            self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
            cambios.push(ViewChange::Status(self.status.clone()));
        }
        vec![self.parche(cambios)]
    }

    /// La proyección de la revisión, o `None` si no hay ninguna.
    ///
    /// Los nombres los propone un MODELO sobre nombres que escribió cualquiera:
    /// van los dos por el saneado canónico y cada uno dice si lo pintado
    /// difiere de lo real. Y van ENTEROS y por separado, jamás concatenados
    /// con una flecha — el mismo motivo que el destino de una transferencia.
    pub(super) fn vista_ia(&self) -> Option<crate::dto::AiRenameView> {
        let r = self.revision_ia.as_ref()?;
        let linea = |texto: &str| {
            let (pintable, hostil) = norte_frontend::display_name(texto.as_bytes());
            crate::dto::DialogLine {
                text: clamp_display(pintable),
                hostile: hostil,
            }
        };
        let pairs = r
            .entradas
            .iter()
            .skip(r.primera)
            .take(norte_frontend::AI_RENAME_PAIR_LIMIT)
            .map(|e| crate::dto::AiRenamePairView {
                from: linea(&e.from),
                to: linea(&e.to),
            })
            .collect();
        let total = r.entradas.len();
        let hasta = (r.primera + norte_frontend::AI_RENAME_PAIR_LIMIT).min(total);
        Some(crate::dto::AiRenameView {
            dir: Self::linea_de_ruta(&r.dir),
            pairs,
            first_visible: r.primera as u64,
            total: total as u64,
            more_note: if hasta >= total {
                String::new()
            } else {
                clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "modal-ai-rename-more",
                    &[("shown", &hasta.to_string()), ("total", &total.to_string())],
                ))
            },
            // Lo que NO se ve también se dice: la marca de una línea solo
            // existe para la línea, y la pareja alterada puede estar en la
            // posición doce.
            hidden_hostile: r
                .entradas
                .iter()
                .enumerate()
                .filter(|(i, _)| *i < r.primera || *i >= hasta)
                .any(|(_, e)| {
                    norte_frontend::display_name(e.from.as_bytes()).1
                        || norte_frontend::display_name(e.to.as_bytes()).1
                }),
            // Traducido AQUÍ: un renderer no traduce, y de todo el cuerpo
            // esta es la línea que no se puede perder.
            status: clamp_display(norte_i18n::t_in(self.lang, r.plan.status_key())),
            // El detalle sale ENTERO de la capa compartida, marcas incluidas:
            // cada superficie pinta nombres que un atacante controla, y una
            // que lo derive por su cuenta es donde se pierde el saneado.
            detail: r
                .plan
                .detail_parts(r.parejas.len(), self.lang)
                .into_iter()
                .flat_map(|parte| self.lineas_de_detalle(&parte))
                .collect(),
            // Aprobar exige las DOS cosas: que el core lo acepte y que el
            // lector haya llegado al final. Lo segundo no lo puede saber el
            // core y lo primero no lo puede saber el lector. La regla vive en
            // el crate COMPARTIDO desde que se vio que el terminal solo pedía
            // la primera: una firma sobre algo que no se ha leído no es una
            // firma, y con doscientos renombrados los que importan pueden
            // estar en la fila ciento ochenta.
            confirmable: norte_frontend::approval_ready(r.plan.confirmable(), r.visto_hasta, total),
            real_steps_note: if r.plan.ready().is_none() {
                String::new()
            } else {
                clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "modal-ai-rename-real-steps",
                    &[("n", &r.plan.real_steps().to_string())],
                ))
            },
            seen_all: r.visto_hasta >= total,
        })
    }

    /// Las teclas mientras la revisión está abierta.
    ///
    /// FIJAS, como las de la paleta y la ayuda, y por el mismo motivo: el
    /// catálogo no tiene comandos para «recorrer este plan» ni «aprobarlo».
    /// Son las que la propia pantalla anuncia en su pie.
    pub(super) fn tecla_en_revision_ia(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Un acorde CON modificador no es una respuesta a esta pantalla: es
        // una tecla que iba a otro sitio. `tecla_en_quick` los rehúsa por lo
        // mismo, y aquí importa más — `ctrl+y` aprobaba un lote.
        if k.ctrl || k.alt || k.meta {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-key-unmapped".to_owned(),
                },
                Vec::new(),
            );
        }
        // La PRIMERA tecla solo reconoce la pantalla. Esta se abre sola,
        // decenas de segundos después del gesto que la pidió, y se queda el
        // teclado: sin este paso, la tecla que el lector iba a mandar a otra
        // cosa contestaba una pregunta que aún no sabía que tenía delante.
        // `Escape` es la excepción y no necesita reconocimiento: descartar es
        // seguro en los dos estados, y quien no quiere esto tiene que poder
        // quitárselo de encima a la primera.
        let reconocida = self.revision_ia.as_ref().is_some_and(|r| r.reconocida);
        if !reconocida && k.key != "Escape" && k.key != "esc" {
            if let Some(r) = self.revision_ia.as_mut() {
                r.reconocida = true;
            }
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "host-plan-acknowledge",
            )));
            let cambios = vec![
                ViewChange::AiRename {
                    ai_rename: self.vista_ia(),
                },
                ViewChange::Status(self.status.clone()),
            ];
            return (self.aplicada(), vec![self.parche(cambios)]);
        }
        let total = self.revision_ia.as_ref().map_or(0, |r| r.entradas.len());
        let ventana = norte_frontend::AI_RENAME_PAIR_LIMIT;
        let tope = total.saturating_sub(ventana);
        let pagina = i64::try_from(ventana).unwrap_or(1);
        let mover = |r: &mut RevisionIa, delta: i64| {
            let destino = i64::try_from(r.primera).unwrap_or(0).saturating_add(delta);
            r.primera = usize::try_from(destino.max(0)).unwrap_or(0).min(tope);
            // La marca de agua solo SUBE: recorrer hacia atrás no deshace lo
            // que ya se leyó.
            r.visto_hasta = r.visto_hasta.max((r.primera + ventana).min(total));
        };
        match k.key.as_str() {
            "ArrowDown" | "j" => {
                if let Some(r) = self.revision_ia.as_mut() {
                    mover(r, 1);
                }
            }
            "ArrowUp" | "k" => {
                if let Some(r) = self.revision_ia.as_mut() {
                    mover(r, -1);
                }
            }
            "PageDown" => {
                if let Some(r) = self.revision_ia.as_mut() {
                    mover(r, pagina);
                }
            }
            "PageUp" => {
                if let Some(r) = self.revision_ia.as_mut() {
                    mover(r, -pagina);
                }
            }
            "Escape" | "n" | "N" => return self.cerrar_revision_ia(),
            // `Enter` NO aprueba, y esto rompe la paridad con el TUI a
            // propósito. Allí el plan lo abre una tecla del lector y la
            // siguiente tecla es una respuesta; aquí la pantalla se abre sola
            // decenas de segundos después, y `Enter` es justo la tecla con la
            // que se estaba recorriendo el árbol mientras el modelo pensaba.
            // Dos `Enter` seguidos entrando en directorios anidados son
            // normales; que el segundo apruebe un renombrado de lote, no.
            // Queda `y` —que el reconocimiento protege— y el botón, que es un
            // gesto que no se puede confundir con otra cosa.
            "y" | "Y" => return self.aprobar_revision_ia(backend, buzon),
            _ => {
                return (
                    ActionAck::Unavailable {
                        reason_key: "host-key-unmapped".to_owned(),
                    },
                    Vec::new(),
                );
            }
        }
        let cambio = ViewChange::AiRename {
            ai_rename: self.vista_ia(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Contesta a la revisión con un gesto DIRIGIDO a ella (un botón).
    ///
    /// No necesita el reconocimiento que sí necesita una tecla: un clic en
    /// un botón de esta pantalla no puede ser un gesto que iba a otro sitio.
    pub(super) fn decidir_revision_ia(
        &mut self,
        approve: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.revision_ia.is_none() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        if let Some(r) = self.revision_ia.as_mut() {
            r.reconocida = true;
        }
        if approve {
            self.aprobar_revision_ia(backend, buzon)
        } else {
            self.cerrar_revision_ia()
        }
    }

    /// Descarta el plan sin aplicar nada.
    pub(super) fn cerrar_revision_ia(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // No se sube la época, y no se toca `ia_en_vuelo`. Las dos cosas
        // parecen prudencia y una de ellas era un bug:
        //
        // - La época no hace falta. El veredicto tardío ya no encuentra
        //   revisión que actualizar, y una petición NUEVA sube la época ella
        //   misma.
        // - `ia_en_vuelo` NO puede ser la petición de esta revisión: se la
        //   llevó `aplicar_plan_ia` al abrirla. Si hay algo ahí es una
        //   petición POSTERIOR, y soltarla aquí la mataba en silencio —
        //   descartar un plan que se está leyendo no es abandonar el que se
        //   acaba de pedir.
        self.revision_ia = None;
        let cambio = ViewChange::AiRename { ai_rename: None };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Aprueba el plan: UNA Task para el lote entero, un solo deshacer.
    ///
    /// Solo si el CORE lo marcó aplicable, y con el `plan_hash` que él mismo
    /// devolvió: lo que se ejecuta es exactamente lo que se enseñó. Un plan
    /// sin veredicto, o con uno que dice que no, no se aprueba y se dice.
    pub(super) fn aprobar_revision_ia(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(r) = self.revision_ia.as_ref() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // La segunda cerradura, también aquí. `rechaza_por_solo_lectura` mira
        // los DIÁLOGOS, y esta es una pantalla propia: hoy es inalcanzable en
        // solo lectura porque las dos vías que la abren están cerradas, pero
        // esa es exactamente la condición que deja de valer en cuanto alguien
        // añade la tercera. Aprobar un plan ejecuta N movimientos.
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        if r.visto_hasta < r.entradas.len() {
            // Y se dice CUÁL de las dos cosas falta: «el core no lo acepta» y
            // «todavía no lo has leído entero» se arreglan de formas
            // distintas.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-plan-unseen".to_owned(),
                },
                Vec::new(),
            );
        }
        let Some(plan) = r.plan.ready().filter(|p| p.executable) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-plan-not-applicable".to_owned(),
                },
                Vec::new(),
            );
        };
        let (dir, parejas, hash) = (r.dir.clone(), r.parejas.clone(), plan.plan_hash.clone());
        let afectados = vec![dir.clone()];
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        tokio::spawn(async move {
            let mensaje = match backend2.rename_batch(dir, parejas, hash).await {
                Ok(task) => Mensaje::TaskNueva(Box::new((task, afectados, None))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon2.send(mensaje).await;
        });
        self.cerrar_revision_ia()
    }

    /// Lo dice en la barra y no abre nada. Cierra la revisión si la había:
    /// un plan que no se pudo pedir no deja media pantalla abierta.
    pub(super) fn decir_de_ia(&mut self, epoca: u64, clave: &str) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
        let mut cambios = vec![ViewChange::Status(self.status.clone())];
        // Solo se cierra la revisión de ESTA época. Cerrar la que hubiera
        // tiraba un plan bueno, ya con veredicto y a punto de aprobarse,
        // porque OTRA petición posterior había fallado.
        if self.revision_ia.take_if(|r| r.epoca == epoca).is_some() {
            cambios.push(ViewChange::AiRename { ai_rename: None });
        }
        vec![self.parche(cambios)]
    }

    /// Abre el nombre de la entrada bajo el cursor, para editarlo. NO
    /// renombra.
    ///
    /// Con VARIAS marcas se niega, y eso NO es lo mismo que hace el TUI: el
    /// TUI renombra la del cursor e ignora las marcas. La tabla compartida
    /// documenta la asimetría en `Facts::rename_single` y deja que cada
    /// frontend conteste; este host ya contestaba «una sola» en `hechos()`,
    /// así que atenuar la fila y luego renombrar de todas formas habría sido
    /// la ayuda mintiendo sobre la tecla.
    pub(super) fn pedir_rename(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let hueco = self.hueco();
        if hueco.pane.marks_len() > 1 {
            return (
                ActionAck::Unavailable {
                    reason_key: norte_frontend::availability::reason_key(
                        norte_help::Reason::WrongTarget,
                    )
                    .to_owned(),
                },
                Vec::new(),
            );
        }
        let Some(from) = hueco.pane.selected().map(|e| e.path.clone()) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-nothing-selected".to_owned(),
                },
                Vec::new(),
            );
        };
        let Some(nombre) = from.file_name() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-cannot-transfer-root".to_owned(),
                },
                Vec::new(),
            );
        };
        // La siembra es lo que la FILA pinta, con el saneado canónico Y con
        // la reinterpretación que el panel tenga puesta: editar produce el
        // texto que se ve, y desde #57 la fila puede estar transcodificada.
        // Sembrar sin ella dejaba `CAF<FFFD>.TXT` bajo una fila que decía
        // `CAFÉ.TXT`. Para un nombre que sigue sin ser representable eso
        // lleva un U+FFFD, y ese residuo es justo lo que el guard de la
        // confirmación no deja escribir.
        let (pintable, hostil) =
            norte_frontend::display_name_with(nombre.as_bytes(), hueco.pane.name_encoding());
        let siembra = clamp_display(pintable.clone());
        if siembra != pintable {
            // El recorte le pega una elipsis al final, y `…` es un carácter
            // LEGAL en un nombre: ni se enmascara ni se marca. Editar ese
            // campo y confirmar escribiría el recorte en el disco como parte
            // del nombre, sin que nada lo dijera — y el guard del U+FFFD no
            // lo ve, porque el recorte pasa DESPUÉS de que `display_name`
            // haya dado su veredicto. Se rehúsa abrirlo, que es lo único
            // honesto: el nombre no cabe, así que aquí no se puede editar.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-name-not-editable".to_owned(),
                },
                Vec::new(),
            );
        }
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-rename-title".to_owned(),
            // Un rename no va a ninguna parte: se queda donde está.
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![Self::linea_de_ruta(&from)],
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
            input: Some(siembra.clone()),
            input_hostile: hostil,
            input_secret: false,
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            // El crudo arranca IGUAL que la siembra: es lo que permite
            // reconocer «no lo ha tocado» sin llevar una bandera aparte.
            tecleado: Tecleado::Texto(siembra.clone()),
            reconocido: true,
            al_confirmar: Some(Pendiente::Renombrar { from, siembra }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Los bytes que un rename confirmado va a escribir, o la clave del
    /// motivo por el que no hay ninguno.
    ///
    /// Tres reglas, y las tres son de la regla 1:
    ///
    /// - **Sin tocar**, se reconstruyen los BYTES ORIGINALES — y entonces el
    ///   destino es el origen, así que el resultado es siempre «mismo nombre,
    ///   mismo sitio». La rama no renombra NADA, y está para que la siembra
    ///   no pueda convertirse en el operando: la proyección de pantalla no es
    ///   reversible para un nombre que no es UTF-8.
    ///
    ///   La consecuencia hay que decirla porque no es evidente: un nombre que
    ///   no es UTF-8 válido **no se puede renombrar desde esta ventana**. Sin
    ///   tocar da «mismo nombre»; tocado lleva el U+FFFD que la pantalla puso
    ///   y no se puede teclear alrededor de él. Es fail-closed y deliberado
    ///   —lo contrario sería escribir mojibake— pero es una limitación, no una
    ///   protección que funcione.
    /// - **Tocado y con un U+FFFD dentro**, se rehúsa: ese carácter lo puso
    ///   la pantalla, y confirmarlo escribiría mojibake de verdad. El guard
    ///   no distingue residuo de intención, así que también rehúsa un U+FFFD
    ///   TECLEADO — asimetría deliberada con crear un directorio, que no
    ///   tiene siembra de la que heredar residuos.
    /// - **El mismo nombre en el mismo sitio** no es una operación.
    pub(super) fn bytes_del_rename(
        from: &VPath,
        siembra: &str,
        escrito: &str,
    ) -> Result<VPath, &'static str> {
        let bytes = if escrito == siembra {
            from.file_name()
                .map(|n| n.as_bytes().to_vec())
                .unwrap_or_default()
        } else {
            if escrito.contains('\u{FFFD}') {
                return Err("msg-transfer-name-fffd");
            }
            escrito.as_bytes().to_vec()
        };
        let seg = norte_proto::Segment::new(bytes).map_err(|_| "err-bad-name")?;
        let destino = from.parent().ok_or("host-cannot-transfer-root")?.join(seg);
        if destino == *from {
            return Err("msg-transfer-name-same");
        }
        Ok(destino)
    }
}
