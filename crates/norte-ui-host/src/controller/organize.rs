//! El plan de ORGANIZAR y su revisión (fase 8 del programa WOW).
//!
//! Parte de `controller`: son métodos de `Estado`, con la misma disciplina
//! que el plan de renombrar de al lado (ADR 0086). El único escritor sigue
//! siendo el actor.
//!
//! **Qué cambia respecto a su gemelo, y por qué.** Renombrar se revisa como
//! una lista de parejas porque eso es lo que es; organizar cambia la FORMA
//! del directorio, así que lo que se revisa es un ÁRBOL — el mismo que pinta
//! el terminal, calculado por [`norte_frontend::organize::tree_lines`]. Y no
//! hay veredicto que esperar: el token del plan viaja CON el plan, de manera
//! que esta pantalla nace aprobable en vez de abrir en `Pending`.

// Mismos imports que el padre, por lo mismo que el resto de los trozos.
#[allow(clippy::wildcard_imports)]
use super::*;

/// El plan de ORGANIZAR en revisión (fase 8).
///
/// El gemelo de [`super::ai::RevisionIa`] sin su campo más caro: no hay veredicto que
/// esperar, porque el token del plan vino CON el plan. Lo demás es idéntico,
/// y a propósito — es la misma clase de pantalla y la misma defensa.
pub(super) struct RevisionOrganizar {
    /// El directorio sobre el que se planeó.
    dir: VPath,
    /// Los movimientos, tal cual los propuso el productor. Es lo que se manda
    /// a ejecutar, y lo que el `plan_hash` ata.
    moves: Vec<norte_proto::methods::OrganizeMove>,
    /// El árbol ya calculado, que es lo que se revisa.
    lineas: Vec<norte_frontend::organize::TreeLine>,
    /// El token que hay que devolver para aplicarlo.
    plan_hash: norte_proto::methods::PlanHash,
    /// Primera línea visible: la revisión es de todo el árbol, por scroll.
    primera: usize,
    /// Hasta dónde ha LLEGADO el lector. Aprobar lo exige.
    visto_hasta: usize,
    /// Ya se ha enseñado al menos una vez, así que la siguiente tecla es una
    /// respuesta y no una tecla que iba a otro sitio.
    reconocida: bool,
    /// La época que la pidió.
    epoca: u64,
}

impl Estado {
    /// Pide un plan de organizar sobre el directorio del hueco con foco.
    ///
    /// `organizer` elige el productor: `None` es el modelo, `Some((plugin,
    /// organizer))` es una extensión del kind `organizer`. Los dos terminan
    /// en la MISMA revisión, porque lo que hace segura la operación no es de
    /// dónde salieron los nombres.
    ///
    /// **Sin prompt de instrucción**, a diferencia de renombrar: lo que se
    /// pide es «mira este directorio y propón una forma», y una caja de texto
    /// vacía delante sugeriría que hay algo que teclear.
    pub(super) fn pedir_plan_de_organizar(
        &mut self,
        organizer: Option<(String, String)>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Organizar MUTA (crea carpetas y mueve), así que la cerradura de
        // solo lectura se echa al PEDIR, no solo al aprobar: enseñar un plan
        // que no se va a poder aplicar es prometer trabajo.
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        self.epoca_organizar += 1;
        let epoca = self.epoca_organizar;
        let dir = self.hueco().pane.dir().clone();
        // Los nombres del directorio al PEDIR: el árbol necesita saber qué
        // carpeta ya existía, y para cuando el productor conteste el lector
        // puede estar en otro sitio. Sólo los que son TEXTO — el árbol los
        // compara contra segmentos de un `proposed_rel`, que viaja UTF-8.
        let existentes = self.hueco().pane.existing_names();
        // Un plugin NO lista el directorio (regla 9), así que los nombres se
        // los tiene que dar quien llama: con la lista vacía, un organizer
        // contesta —correctamente— que no mueve nada. El modelo es el caso
        // contrario: el engine lista por él, y ahí vacío SÍ significa «todo»,
        // sin tope que respetar.
        let operando = self.hueco().pane.organizable_names();
        if organizer.is_some() && operando.len() > norte_proto::methods::AI_RENAME_NAMES_MAX {
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "msg-organize-too-many",
            )));
            let cambio = ViewChange::Status(self.status.clone());
            return (self.aplicada(), vec![self.parche(vec![cambio])]);
        }
        self.organizar_en_vuelo = Some((epoca, dir.clone(), existentes));
        let b = Arc::clone(backend);
        let buz = buzon.clone();
        tokio::spawn(async move {
            let peticion = match organizer {
                Some((plugin, org)) => b.plugin_organize_plan(plugin, org, dir, operando),
                None => b.ai_organize_plan(dir, String::new(), Vec::new()),
            };
            let res = (tokio::time::timeout(PLAZO_IA, peticion).await)
                .unwrap_or(Err(Error::ProviderUnavailable { retryable: true }));
            let _ = buz
                .send(Mensaje::Fondo(Box::new(Fondo::PlanOrganizar(
                    epoca,
                    Box::new(res),
                ))))
                .await;
        });
        // Y se DICE que se está pidiendo: sin esto el gesto no produce nada
        // visible y el lector lo repite, que es lo que destapa la carrera de
        // dos peticiones.
        self.status.message = Some(clamp_display(norte_i18n::t_in(
            self.lang,
            "msg-organize-running",
        )));
        let cambio = ViewChange::Status(self.status.clone());
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Lo que contestó el productor, revisado antes de enseñarlo.
    ///
    /// Los cinturones son de INGESTIÓN y rechazan EN BLOQUE: un plan más
    /// grande de lo que un directorio puede tener delata a un daemon hostil
    /// inflando la respuesta, y un plan sin token no se puede aprobar —
    /// abrirlo prometería un botón que no puede hacer nada.
    pub(super) fn aplicar_plan_de_organizar(
        &mut self,
        epoca: u64,
        res: Result<norte_proto::methods::AiOrganizePlanResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // La petición en vuelo tiene que ser ESTA. `take_if` y no
        // `take().filter(...)`: vaciar el hueco antes de mirar deja que una
        // respuesta vieja se lleve por delante la petición viva.
        let Some((_, dir, existentes)) = self.organizar_en_vuelo.take_if(|(e, _, _)| *e == epoca)
        else {
            return Vec::new();
        };
        let plan = match res {
            Ok(p) => p,
            Err(e) => {
                return self.decir_de_organizar(epoca, norte_frontend::error::error_key(&e));
            }
        };
        // El productor dijo POR QUÉ no propone (#332). La frase viene ya
        // enmascarada y acotada por el daemon: aquí se enseña, no se
        // interpreta.
        if let Some(why) = plan.refused {
            self.status.message = Some(clamp_display(norte_i18n::ta_in(
                self.lang,
                "msg-rename-plan-refused",
                &[("why", &why)],
            )));
            let mut cambios = vec![ViewChange::Status(self.status.clone())];
            if self
                .revision_organizar
                .take_if(|r| r.epoca == epoca)
                .is_some()
            {
                cambios.push(ViewChange::Organize { organize: None });
            }
            return vec![self.parche(cambios)];
        }
        if plan.moves.is_empty() {
            return self.decir_de_organizar(epoca, "msg-organize-empty");
        }
        if plan.moves.len() > norte_frontend::MAX_AI_PLAN_ENTRIES {
            return self.decir_de_organizar(epoca, "msg-organize-invalid-plan");
        }
        let Some(plan_hash) = plan.plan_hash else {
            return self.decir_de_organizar(epoca, "msg-organize-invalid-plan");
        };
        let lineas = norte_frontend::organize::tree_lines(&plan.moves, &existentes);
        self.revision_organizar = Some(RevisionOrganizar {
            dir,
            moves: plan.moves,
            lineas,
            plan_hash,
            primera: 0,
            visto_hasta: norte_frontend::organize::ORGANIZE_LINE_LIMIT,
            reconocida: false,
            epoca,
        });
        self.status.message = None;
        let cambios = vec![
            ViewChange::Organize {
                organize: self.vista_organizar(),
            },
            ViewChange::Status(self.status.clone()),
        ];
        vec![self.parche(cambios)]
    }

    /// La proyección de la revisión, o `None` si no hay ninguna.
    ///
    /// Los nombres los propone un tercero sobre nombres que escribió
    /// cualquiera: van por el saneado canónico y cada línea dice si lo
    /// pintado difiere de lo real.
    pub(super) fn vista_organizar(&self) -> Option<crate::dto::OrganizeView> {
        use norte_frontend::organize::{ORGANIZE_LINE_LIMIT, TreeKind};

        let r = self.revision_organizar.as_ref()?;
        let linea = |texto: &str| {
            let (pintable, hostil) = norte_frontend::display_name(texto.as_bytes());
            crate::dto::DialogLine {
                text: clamp_display(pintable),
                hostile: hostil,
            }
        };
        let total = r.lineas.len();
        let ultima = (r.primera + ORGANIZE_LINE_LIMIT).min(total);
        let ventana = r.lineas[r.primera..ultima]
            .iter()
            .map(|l| crate::dto::OrganizeLineView {
                depth: u32::try_from(l.depth).unwrap_or(u32::MAX),
                text: linea(&l.text),
                kind: match l.kind {
                    TreeKind::NewDir => crate::dto::OrganizeLineKind::NewDir,
                    TreeKind::ExistingDir => crate::dto::OrganizeLineKind::ExistingDir,
                    TreeKind::Moved => crate::dto::OrganizeLineKind::Moved,
                },
            })
            .collect();
        // Lo ESCONDIDO no se cuela limpio: si una línea fuera de la ventana
        // se pinta distinta de sus bytes, el indicador lo dice. Sin esto la
        // marca sólo existiría para lo que se ve, y basta con poner la línea
        // alterada en la posición doce.
        let hidden_hostile = r.lineas.iter().enumerate().any(|(i, l)| {
            (i < r.primera || i >= ultima) && norte_frontend::display_name(l.text.as_bytes()).1
        });
        let (carpetas, ficheros) = norte_frontend::organize::resumen(&r.lineas);
        Some(crate::dto::OrganizeView {
            dir: {
                let (pintable, hostil) = norte_frontend::path_display(&r.dir);
                crate::dto::DialogLine {
                    text: clamp_display(pintable),
                    hostile: hostil,
                }
            },
            lines: ventana,
            first_visible: r.primera as u64,
            total: total as u64,
            // Traducido AQUÍ y no en el renderer, por lo mismo que la nota
            // del plan de renombrar: el catálogo que cruza el puente lleva
            // las cadenas ya formateadas y sin argumentos.
            more_note: if total > ORGANIZE_LINE_LIMIT {
                clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "modal-ai-rename-more",
                    &[
                        ("shown", &ultima.to_string()),
                        ("total", &total.to_string()),
                    ],
                ))
            } else {
                String::new()
            },
            hidden_hostile,
            summary: clamp_display(norte_i18n::ta_in(
                self.lang,
                "modal-organize-summary",
                &[
                    ("dirs", &carpetas.to_string()),
                    ("files", &ficheros.to_string()),
                ],
            )),
            seen_all: r.visto_hasta >= total,
        })
    }

    /// Las teclas de la revisión: recorrer, descartar y aprobar.
    ///
    /// Misma disciplina que la revisión de renombrar, y por las mismas dos
    /// razones: un acorde con modificador iba a otro sitio, y la PRIMERA
    /// tecla sólo reconoce la pantalla — ésta se abre sola, decenas de
    /// segundos después del gesto que la pidió, y se queda el teclado.
    pub(super) fn tecla_en_revision_organizar(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if k.ctrl || k.alt || k.meta {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-key-unmapped".to_owned(),
                },
                Vec::new(),
            );
        }
        let reconocida = self
            .revision_organizar
            .as_ref()
            .is_some_and(|r| r.reconocida);
        if !reconocida && k.key != "Escape" && k.key != "esc" {
            if let Some(r) = self.revision_organizar.as_mut() {
                r.reconocida = true;
            }
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "host-plan-acknowledge",
            )));
            let cambios = vec![
                ViewChange::Organize {
                    organize: self.vista_organizar(),
                },
                ViewChange::Status(self.status.clone()),
            ];
            return (self.aplicada(), vec![self.parche(cambios)]);
        }
        let total = self
            .revision_organizar
            .as_ref()
            .map_or(0, |r| r.lineas.len());
        let ventana = norte_frontend::organize::ORGANIZE_LINE_LIMIT;
        let tope = total.saturating_sub(ventana);
        let pagina = i64::try_from(ventana).unwrap_or(1);
        let mover = |r: &mut RevisionOrganizar, delta: i64| {
            let destino = i64::try_from(r.primera).unwrap_or(0).saturating_add(delta);
            r.primera = usize::try_from(destino.max(0)).unwrap_or(0).min(tope);
            // Marca de agua ALTA: volver arriba no des-lee lo ya leído.
            r.visto_hasta = r.visto_hasta.max((r.primera + ventana).min(total));
        };
        match k.key.as_str() {
            "ArrowDown" | "j" => {
                if let Some(r) = self.revision_organizar.as_mut() {
                    mover(r, 1);
                }
            }
            "ArrowUp" | "k" => {
                if let Some(r) = self.revision_organizar.as_mut() {
                    mover(r, -1);
                }
            }
            "PageDown" => {
                if let Some(r) = self.revision_organizar.as_mut() {
                    mover(r, pagina);
                }
            }
            "PageUp" => {
                if let Some(r) = self.revision_organizar.as_mut() {
                    mover(r, -pagina);
                }
            }
            "Escape" | "n" | "N" => return self.cerrar_revision_organizar(),
            // `Enter` NO aprueba, por lo mismo que en la revisión de
            // renombrar: esta pantalla se abre sola, y `Enter` es la tecla
            // con la que se estaba recorriendo el árbol mientras el productor
            // pensaba.
            "y" | "Y" => return self.aprobar_revision_organizar(backend, buzon),
            _ => {
                return (
                    ActionAck::Unavailable {
                        reason_key: "host-key-unmapped".to_owned(),
                    },
                    Vec::new(),
                );
            }
        }
        let cambio = ViewChange::Organize {
            organize: self.vista_organizar(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Recorre el árbol con un gesto del ratón (la rueda, o un botón).
    ///
    /// Existe aparte de las teclas por la misma razón que el botón de
    /// decidir: aprobar exige haber llegado al final, y sin una forma de
    /// recorrer con el ratón esa exigencia convertía la pantalla en una que
    /// un lector sin teclado no podía aprobar nunca. Mueve la MISMA marca de
    /// agua que las teclas — leer con la rueda es leer.
    pub(super) fn recorrer_organizar(
        &mut self,
        down: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let total = self
            .revision_organizar
            .as_ref()
            .map_or(0, |r| r.lineas.len());
        let ventana = norte_frontend::organize::ORGANIZE_LINE_LIMIT;
        let tope = total.saturating_sub(ventana);
        let Some(r) = self.revision_organizar.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        r.primera = if down {
            (r.primera + 1).min(tope)
        } else {
            r.primera.saturating_sub(1)
        };
        r.visto_hasta = r.visto_hasta.max((r.primera + ventana).min(total));
        let cambio = ViewChange::Organize {
            organize: self.vista_organizar(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Contesta a la revisión con un gesto DIRIGIDO a ella (un botón), que no
    /// necesita el reconocimiento que sí necesita una tecla.
    pub(super) fn decidir_revision_organizar(
        &mut self,
        approve: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.revision_organizar.is_none() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        if let Some(r) = self.revision_organizar.as_mut() {
            r.reconocida = true;
        }
        if approve {
            self.aprobar_revision_organizar(backend, buzon)
        } else {
            self.cerrar_revision_organizar()
        }
    }

    /// Descarta el plan sin aplicar nada.
    pub(super) fn cerrar_revision_organizar(
        &mut self,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Ni se sube la época ni se toca `organizar_en_vuelo`, por lo mismo
        // que en la revisión de renombrar: lo que haya ahí es una petición
        // POSTERIOR, y soltarla aquí la mataría en silencio.
        self.revision_organizar = None;
        let cambio = ViewChange::Organize { organize: None };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Aprueba el plan: UNA Task para el lote entero, un solo deshacer.
    ///
    /// Con el `plan_hash` que vino CON el plan, así que lo que se ejecuta es
    /// exactamente lo que se enseñó — si el directorio cambió por debajo, el
    /// core contesta `PlanStale` y no mueve nada.
    pub(super) fn aprobar_revision_organizar(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(r) = self.revision_organizar.as_ref() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // La segunda cerradura, también aquí: aprobar ejecuta N movimientos y
        // crea carpetas.
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        if r.visto_hasta < r.lineas.len() {
            // Y se dice CUÁL de las cosas falta: «no lo has leído entero» se
            // arregla de una forma y «no se puede» de otra.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-plan-unseen".to_owned(),
                },
                Vec::new(),
            );
        }
        let (dir, moves, hash) = (r.dir.clone(), r.moves.clone(), r.plan_hash.clone());
        let afectados = vec![dir.clone()];
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        tokio::spawn(async move {
            let mensaje = match backend2.organize(dir, moves, hash).await {
                Ok(task) => Mensaje::TaskNueva(Box::new((task, afectados, None))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon2.send(mensaje).await;
        });
        self.cerrar_revision_organizar()
    }

    /// Lo dice en la barra y no abre nada. Cierra la revisión de ESTA época
    /// si la había: un plan que no se pudo pedir no deja media pantalla
    /// abierta, y cerrar la que hubiera tiraría un plan bueno porque otra
    /// petición posterior falló.
    fn decir_de_organizar(&mut self, epoca: u64, clave: &str) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
        let mut cambios = vec![ViewChange::Status(self.status.clone())];
        if self
            .revision_organizar
            .take_if(|r| r.epoca == epoca)
            .is_some()
        {
            cambios.push(ViewChange::Organize { organize: None });
        }
        vec![self.parche(cambios)]
    }
}
