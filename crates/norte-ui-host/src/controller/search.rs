//! Buscar: el filtro rápido, la búsqueda normal y la semántica.
//!
//! Parte de `controller`: son métodos de `Estado`, movidos aquí sin
//! tocarlos (ADR 0086). El único escritor sigue siendo el actor.

// Estos módulos son el mismo `impl Estado` partido en trozos, así que usan
// los mismos imports que el padre. Enumerarlos aquí sería una lista de
// cuarenta líneas por fichero, en 32 ficheros, que se desincroniza en cuanto
// el padre importa algo — `super::*` la sigue sola.
#[allow(clippy::wildcard_imports)]
use super::*;

/// En qué acabó una búsqueda. El tipo y la precedencia de sus frases son del
/// crate compartido: aquí estaban escritos aparte y ya discrepaban.
use norte_frontend::search_status::Outcome as Desenlace;

/// Una búsqueda viva y lo que lleva encontrado.
pub(super) struct Busqueda {
    /// Cuál de todas las búsquedas de esta ventana es.
    ///
    /// La identidad NO puede ser la Task: el id lo trae el daemon y llega
    /// tarde, así que hasta entonces no habría con qué distinguir un lote de
    /// la búsqueda anterior. La época se conoce al LANZAR, que es cuando hace
    /// falta.
    pub(super) epoca: u64,
    /// La Task del daemon, en cuanto se sabe. Cero mientras no se sabe.
    pub(super) task: norte_proto::TaskId,
    /// La vista se cerró y lo que quede de esta búsqueda sobra.
    ///
    /// La comparte con su reenviador, que es quien puede cancelar antes de
    /// que el id llegue al actor: `esc` justo tras lanzar es la ventana en la
    /// que nadie más tiene a quién cancelar.
    abandonada: Arc<std::sync::atomic::AtomicBool>,
    /// Lo que se buscó, para poder decirlo.
    query: String,
    /// Dónde se buscó.
    root: VPath,
    /// Lo encontrado, en el orden en que llegó.
    hits: Vec<Hallazgo>,
    /// Esta búsqueda es SEMÁNTICA: se preguntó por significado contra el
    /// índice, no por nombre contra el árbol.
    semantica: bool,
    /// Dónde está el cursor.
    cursor: usize,
    /// En qué acabó, o que sigue corriendo.
    ///
    /// Un `bool` decía solo si sigue viva, y entonces TODO desenlace se
    /// pintaba «N hallazgos» — o sea que una búsqueda que falló al segundo
    /// directorio y otra que recorrió el árbol entero se leían igual. Eso no
    /// es una imprecisión de la interfaz: es una afirmación falsa sobre el
    /// disco, y quien la lee deja de buscar.
    ///
    /// El tipo es del crate COMPARTIDO, y con él la precedencia de las
    /// frases: los dos frontends la decidían aparte y ya discrepaban en el
    /// par «cancelada justo en el tope» (ADR 0077).
    pub(super) desenlace: Desenlace,
    /// El tope que se pidió: alcanzarlo significa que hay más.
    tope: u32,
}

/// Un hallazgo de una búsqueda, venga de donde venga.
#[derive(Clone)]
struct Hallazgo {
    /// Dónde está.
    path: VPath,
    /// Qué es, si se sabe. `None` en un hallazgo SEMÁNTICO: el índice
    /// devuelve rutas y parecidos, no clases, y decir «fichero» porque suele
    /// serlo es inventarse la respuesta.
    kind: Option<EntryKind>,
    /// Cuánto se parece a lo que se preguntó, en `[-1, 1]`. `None` en una
    /// búsqueda por nombre: ahí no hay grados, o casa o no casa.
    score: Option<f64>,
}

impl Estado {
    /// Tope de resultados de UNA búsqueda.
    ///
    /// Acota el mensaje y la memoria del host: un árbol grande con un patrón
    /// laxo devuelve todo lo que hay. Alcanzarlo NO es un fallo —la Task
    /// completa— y se DICE, porque «100 resultados» y «los primeros 100 de
    /// no se sabe cuántos» son dos respuestas distintas.
    pub(super) const MAX_RESULTADOS: u32 = 2000;

    /// Abre el prompt de buscar. Lo que se teclea es el patrón.
    /// Abre el prompt de un GLOB para marcar —o desmarcar— por patrón.
    ///
    /// Un prompt y no una tecla: el operando es un patrón que se teclea, y
    /// eso ya tiene forma en este host. Lo que se marca lo decide el modelo
    /// COMPARTIDO (`mark_glob`), que pliega el nombre antes de casar y sabe
    /// que un `*` sobre nombres enmascarados no puede significar «todos los
    /// que se pintan raro».
    pub(super) fn pedir_patron(
        &mut self,
        marcar: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        self.dialogos.push(Dialogo {
            id,
            reconocido: true,
            vista: DialogView {
                id,
                title_key: if marcar {
                    "modal-mark-pattern-title"
                } else {
                    "modal-unmark-pattern-title"
                }
                .to_owned(),
                destination: None,
                subject: None,
                asker: None,
                deadline: None,
                deadline_at_ms: None,
                body: Vec::new(),
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
            },
            tecleado: Tecleado::Texto(String::new()),
            al_confirmar: Some(Pendiente::Patron { marcar }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Aplica el patrón tecleado.
    pub(super) fn aplicar_patron(
        &mut self,
        marcar: bool,
        patron: &str,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        if patron.is_empty() {
            // Un glob vacío no casa nada, y decirlo es mejor que no hacer
            // nada: quien pulsó cree que marcó.
            return (Some("err-empty-pattern"), self.decir("err-empty-pattern"));
        }
        match self.hueco_mut().pane.mark_glob(patron, marcar) {
            Ok(n) => {
                // La clave del TUI, que ya existía y dice «N marcas
                // cambiadas»: sirve para las dos direcciones, y una segunda
                // definición de la misma clave la tira Fluent en silencio —
                // la trampa que este repo ya se ha comido dos veces.
                let mut fuera = self.decir_con("msg-marked-by-pattern", &[("n", &n.to_string())]);
                fuera.push(self.parche_filas());
                (None, fuera)
            }
            // Un glob que no compila se DICE: es lo que el lector acaba de
            // teclear, y callar deja una tecla que no hizo nada.
            Err(_) => (Some("err-bad-pattern"), self.decir("err-bad-pattern")),
        }
    }

    pub(super) fn pedir_busqueda(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let root = self.hueco().pane.dir().clone();
        let donde = Self::linea_de_ruta(&root);
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        self.dialogos.push(Dialogo {
            id,
            reconocido: true,
            vista: DialogView {
                id,
                title_key: "modal-search-title".to_owned(),
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
                input: Some(String::new()),
                input_hostile: false,
                input_secret: false,
                dest_check: crate::dto::DestCheckView::NotAsked,
            },
            tecleado: Tecleado::Texto(String::new()),
            al_confirmar: Some(Pendiente::Buscar { root }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Lanza la búsqueda y engancha el canal por el que llegan sus lotes.
    ///
    /// El patrón va como GLOB de nombre, que es lo que un usuario teclea
    /// cuando busca `*.rs`. La búsqueda por CONTENIDO es otra cosa —otro
    /// campo, otro coste— y llega con su propia rebanada.
    pub(super) fn lanzar_busqueda(
        &mut self,
        root: VPath,
        patron: String,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let params = norte_proto::methods::FsSearchParams {
            name_glob: Some(patron.clone()),
            max_hits: Some(Self::MAX_RESULTADOS),
            ..norte_proto::methods::FsSearchParams::new(root.clone())
        };
        let backend = Arc::clone(backend);
        let buzon2 = buzon.clone();
        self.epoca_busqueda += 1;
        let epoca = self.epoca_busqueda;
        let abandonada = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let abandonada2 = Arc::clone(&abandonada);
        // La búsqueda se lanza y CONTESTA por el buzón, como todo lo demás:
        // el actor sigue atendiendo teclas mientras el daemon camina el árbol.
        tokio::spawn(async move {
            let (task, mut rx) = match backend.search(params).await {
                Ok(par) => par,
                Err(e) => {
                    // Por los DOS caminos: la barra lo dice una vez y la
                    // vista de la búsqueda deja de afirmar que sigue
                    // buscando. Sin lo segundo se quedaba en «buscando…»
                    // para siempre sobre algo que nunca llegó a existir.
                    let _ = buzon2
                        .send(Mensaje::Fondo(Box::new(Fondo::BusquedaRota(
                            epoca,
                            Box::new(e.clone()),
                        ))))
                        .await;
                    let _ = buzon2.send(Mensaje::TaskFallida(Box::new(e))).await;
                    return;
                }
            };
            let id = task.id;
            let cancel = Arc::clone(&task.cancel);
            let _ = buzon2
                .send(Mensaje::TaskNueva(Box::new((task, Vec::new(), None))))
                .await;
            // Bautizada AQUÍ y no con el primer lote: puede no haber primer
            // lote —el core no manda lotes vacíos— y entonces la búsqueda se
            // quedaba sin nombre, sin poder terminar y sin poder cancelarse.
            let _ = buzon2
                .send(Mensaje::Fondo(Box::new(Fondo::BusquedaViva(epoca, id))))
                .await;
            // La vista pudo cerrarse mientras el daemon aceptaba la Task: en
            // esa ventana el actor no tiene a quién cancelar, así que cancela
            // quien sí lo tiene.
            if abandonada2.load(std::sync::atomic::Ordering::SeqCst) {
                cancel();
                return;
            }
            // La bomba vive lo que el canal: cuando el daemon lo cierra, la
            // búsqueda terminó y el progreso ya lo dijo por su lado.
            while let Some(lote) = rx.recv().await {
                if abandonada2.load(std::sync::atomic::Ordering::SeqCst) {
                    cancel();
                    return;
                }
                if buzon2
                    .send(Mensaje::Fondo(Box::new(Fondo::Resultados(
                        epoca,
                        Box::new(lote),
                    ))))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        // La vista se abre YA, vacía y diciendo que corre: esperar al primer
        // lote es una ventana que no reacciona a una tecla que sí hizo algo.
        self.busqueda = Some(Busqueda {
            semantica: false,
            epoca,
            // Todavía no se sabe: `Fondo::BusquedaViva` la trae. Cero jamás
            // es una Task real.
            task: norte_proto::TaskId::new(0),
            abandonada,
            query: patron,
            root,
            hits: Vec::new(),
            cursor: 0,
            desenlace: Desenlace::Running,
            tope: Self::MAX_RESULTADOS,
        });
        let cambio = ViewChange::Search {
            search: self.vista_busqueda(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// Un lote de resultados.
    ///
    /// Casa por ÉPOCA, que se conoce al lanzar. Con el id de la Task no
    /// bastaba: hasta que llegaba, `b.task` era cero y el primer lote que
    /// apareciese bautizaba la búsqueda —incluido uno rezagado de la
    /// ANTERIOR, cuyo reenviador sigue vivo—, así que los hallazgos de un
    /// patrón llenaban la lista rotulada con otro.
    pub(super) fn aplicar_resultados(
        &mut self,
        epoca: u64,
        lote: &norte_proto::methods::SearchHits,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let b = self.busqueda.as_mut()?;
        if b.epoca != epoca {
            return None;
        }
        let sitio = usize::try_from(b.tope).unwrap_or(usize::MAX);
        for e in &lote.entries {
            if b.hits.len() >= sitio {
                break;
            }
            b.hits.push(Hallazgo {
                path: e.path.clone(),
                kind: Some(e.kind),
                score: None,
            });
        }
        let cambio = ViewChange::Search {
            search: self.vista_busqueda(),
        };
        Some(self.parche(vec![cambio]))
    }

    /// La proyección de la búsqueda.
    pub(super) fn vista_busqueda(&self) -> Option<crate::dto::SearchView> {
        let b = self.busqueda.as_ref()?;
        let (donde, root_hostil) = norte_frontend::path_display(&b.root);
        Some(crate::dto::SearchView {
            semantic: b.semantica,
            query: clamp_display(norte_frontend::display_name(b.query.as_bytes()).0),
            root: clamp_display(donde),
            root_hostile: root_hostil,
            rows: b
                .hits
                .iter()
                .map(|e| {
                    let nombre = e
                        .path
                        .file_name()
                        .map_or_else(Vec::new, |s| s.as_bytes().to_vec());
                    let (pintable, hostil) = norte_frontend::display_name(&nombre);
                    let (padre, padre_hostil) = e.path.parent().map_or_else(
                        || (String::new(), false),
                        |p| norte_frontend::path_display(&p),
                    );
                    crate::dto::SearchRowView {
                        name: clamp_display(pintable),
                        hostile: hostil,
                        parent: clamp_display(padre),
                        parent_hostile: padre_hostil,
                        is_dir: e.kind == Some(EntryKind::Dir),
                        score: e.score,
                    }
                })
                .collect(),
            // `then` y no `then_some`: el argumento de `then_some` se evalúa
            // SIEMPRE, y con cero hallazgos el `len() - 1` se desbordaba.
            cursor: (!b.hits.is_empty()).then(|| b.cursor.min(b.hits.len() - 1) as u64),
            status: clamp_display(Self::estado_de_busqueda(b, self.lang)),
            running: b.desenlace == Desenlace::Running,
        })
    }

    /// Una búsqueda que no llegó a encolarse: deja de decir que busca.
    pub(super) fn busqueda_rota(&mut self, epoca: u64, e: &Error) -> Vec<BridgeEnvelope<UiUpdate>> {
        let categoria = clamp_display(norte_frontend::error::error_category_in(self.lang, e));
        let Some(b) = self.busqueda.as_mut().filter(|b| b.epoca == epoca) else {
            return Vec::new();
        };
        b.desenlace = Desenlace::Failed(categoria);
        let cambio = ViewChange::Search {
            search: self.vista_busqueda(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// La frase de estado de una búsqueda.
    ///
    /// Reutiliza la familia del TUI (`search-status-*`) en vez de inventar
    /// otra: es la misma información y no hay dos maneras de decirla.
    ///
    /// La PRECEDENCIA la decide el crate compartido, que es donde estaba el
    /// desacuerdo: parar la búsqueda justo en el tope decía «cancelada» en el
    /// terminal y «hay más» aquí.
    ///
    /// El fallo no lleva recuento: lo que hay que leer ahí no es cuántos se
    /// encontraron, sino que la respuesta está incompleta y por qué.
    pub(super) fn estado_de_busqueda(b: &Busqueda, lang: norte_i18n::Lang) -> String {
        let al_tope = b.hits.len() >= usize::try_from(b.tope).unwrap_or(usize::MAX);
        let clave = norte_frontend::search_status::status_key(&b.desenlace, al_tope);
        if let Desenlace::Failed(categoria) = &b.desenlace {
            return norte_i18n::ta_in(lang, clave, &[("error", categoria)]);
        }
        norte_i18n::ta_in(lang, clave, &[("n", &b.hits.len().to_string())])
    }

    /// Las teclas mientras la búsqueda está abierta.
    pub(super) fn tecla_en_busqueda(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        /// Cuántas filas mueve una página.
        const PAGINA: usize = 10;
        let Some(b) = self.busqueda.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let ultimo = b.hits.len().saturating_sub(1);
        match k.key.as_str() {
            "Escape" | "esc" => {
                // Cerrar la búsqueda CANCELA la Task: seguir caminando un
                // árbol para nadie es gastar el daemon en un resultado que ya
                // no tiene dónde aparecer.
                b.abandonada
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                let task = b.task;
                self.busqueda = None;
                if task.get() != 0 {
                    self.cancelar(task.get());
                }
                // Y si era una consulta SEMÁNTICA, se aborta: no tiene Task
                // que cancelar —es una llamada directa— y lo que la para es
                // soltarla, que hace que el SDK mande `rpc.cancel`.
                if let Some(vuelo) = self.semantica_en_vuelo.take() {
                    vuelo.abort();
                }
            }
            "ArrowDown" | "down" => b.cursor = (b.cursor + 1).min(ultimo),
            "ArrowUp" | "up" => b.cursor = b.cursor.saturating_sub(1),
            "PageDown" | "pgdn" => b.cursor = (b.cursor + PAGINA).min(ultimo),
            "PageUp" | "pgup" => b.cursor = b.cursor.saturating_sub(PAGINA),
            "Home" | "home" => b.cursor = 0,
            "End" | "end" => b.cursor = ultimo,
            "Enter" | "enter" => {
                let fila = u32::try_from(b.cursor).unwrap_or(u32::MAX);
                return self.ir_al_resultado(fila, backend, buzon);
            }
            _ => return (self.aplicada(), Vec::new()),
        }
        let cambio = ViewChange::Search {
            search: self.vista_busqueda(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Va al resultado `fila`: el panel navega a su directorio y el cursor
    /// queda ENCIMA de él.
    ///
    /// Sin reconstruir ninguna ruta: la del hallazgo es la que mandó el
    /// daemon, y se le pasa entera al panel para que la case byte a byte
    /// cuando aterrice el listado. Un nombre pintado no vuelve a ser un path
    /// nunca — por ahí es por donde se acaba abriendo otro fichero.
    pub(super) fn ir_al_resultado(
        &mut self,
        fila: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(b) = self.busqueda.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // Fuera de rango no se recorta: recortar navegaba al ÚLTIMO hallazgo
        // en vez de no hacer nada. Los hallazgos solo se añaden por el final,
        // así que un índice válido nombra siempre el mismo y esta lista no
        // necesita generación; uno que se pasa es que la lista se vació.
        let Some(hit) = b.hits.get(fila as usize).cloned() else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        b.cursor = fila as usize;
        // Un directorio se abre por dentro; un fichero, en su carpeta con el
        // cursor encima.
        // Sin clase —un hallazgo semántico— se trata como fichero: se abre
        // su carpeta con el cursor encima. Es lo conservador; entrar EN algo
        // que resulta no ser un directorio no lleva a ninguna parte.
        let (destino, foco) = if hit.kind == Some(EntryKind::Dir) {
            (hit.path.clone(), None)
        } else {
            match hit.path.parent() {
                Some(p) => (p, Some(hit.path.clone())),
                None => (hit.path.clone(), None),
            }
        };
        let task = b.task;
        self.busqueda = None;
        if task.get() != 0 {
            self.cancelar(task.get());
        }
        if let Some(child) = foco {
            self.hueco_mut().pane.set_pending_focus(child);
        }
        let cierre = self.parche(vec![ViewChange::Search { search: None }]);
        let mut envios = vec![cierre];
        envios.extend(self.navegar(&destino, Trail::Record, backend, buzon));
        (self.aplicada(), envios)
    }

    /// La tecla, cuando el buscador incremental está abierto.
    ///
    /// `None` = esta tecla no es suya y sigue su camino normal (una tecla de
    /// función, un atajo con modificador): abrir el buscador NO desconecta el
    /// resto del teclado, solo se queda el texto, el borrado y las tres
    /// teclas que lo gobiernan.
    pub(super) fn tecla_en_quick(
        &mut self,
        k: &crate::keys::KeyInput,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        if k.ctrl || k.alt || k.meta {
            return None;
        }
        let pane = &mut self.hueco_mut().pane;
        match k.key.as_str() {
            "Escape" | "esc" => pane.quick_cancel(),
            "Enter" | "enter" => {
                pane.quick_confirm();
            }
            "Backspace" | "backspace" => pane.quick_backspace(),
            "ArrowDown" | "down" => pane.quick_down(),
            "ArrowUp" | "up" => pane.quick_up(),
            otro => {
                let mut chars = otro.chars();
                let (Some(c), None) = (chars.next(), chars.next()) else {
                    return None;
                };
                pane.quick_char(c);
            }
        }
        Some((self.aplicada(), vec![self.parche_filas()]))
    }

    /// Abre el prompt de una consulta SEMÁNTICA.
    /// Abre el prompt de una consulta SEMÁNTICA.
    ///
    /// No lleva raíz, y eso es lo que dice el diálogo: el índice se construye
    /// por raíces y no por lo que se esté mirando, así que acotar la búsqueda
    /// al directorio del panel prometería un alcance que el índice puede no
    /// tener. Se pregunta al índice ENTERO, igual que el TUI.
    pub(super) fn pedir_consulta_semantica(
        &mut self,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-semantic-title".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![crate::dto::DialogLine {
                text: clamp_display(norte_i18n::t_in(self.lang, "modal-semantic-scope")),
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
            al_confirmar: Some(Pendiente::ConsultaSemantica),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Lanza la consulta contra el índice. La respuesta vuelve al actor.
    ///
    /// Época nueva por consulta: la respuesta tarda —hay un embed de por
    /// medio— y quien pregunta dos veces no puede acabar mirando los
    /// resultados de la primera.
    pub(super) fn lanzar_semantica(
        &mut self,
        consulta: String,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if consulta.trim().is_empty() {
            // Una consulta vacía no sale del proceso: no significa nada, y
            // lo que sale va a un proveedor externo.
            //
            // El mensaje es el MISMO que el de la terminal (#122). Antes era
            // `err-empty-pattern`, que es el de buscar por nombre y dice «uno
            // vacío casa el árbol entero» — falso aquí: una consulta semántica
            // vacía no casa nada, no hay con qué comparar. Dos frontends
            // negando lo mismo por dos motivos distintos, y uno de ellos
            // inventado.
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "modal-semantic-empty-query",
            )));
            // Y el campo VUELVE, que es la otra mitad de la paridad: la
            // terminal deja el modal abierto con el error debajo, así que un
            // mensaje pidiendo escribir una consulta sobre una pantalla sin
            // dónde escribirla no era una negativa, era un callejón. El campo
            // estaba vacío, así que no se pierde nada al rehacerlo.
            let (_, mut fuera) = self.pedir_consulta_semantica();
            let cambio = ViewChange::Status(self.status.clone());
            fuera.push(self.parche(vec![cambio]));
            return fuera;
        }
        self.epoca_busqueda += 1;
        let epoca = self.epoca_busqueda;
        self.busqueda = Some(Busqueda {
            semantica: true,
            epoca,
            task: norte_proto::TaskId::new(0),
            abandonada: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            query: consulta.clone(),
            // El alcance es el índice entero: no hay raíz que enseñar, y la
            // vista lo dice por `semantic`.
            root: self.hueco().pane.dir().clone(),
            hits: Vec::new(),
            cursor: 0,
            desenlace: Desenlace::Running,
            tope: norte_proto::methods::INDEX_SEMANTIC_MAX_K,
        });
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let handle = tokio::spawn(async move {
            let hits = backend
                .semantic_search(consulta, norte_frontend::SEMANTIC_K)
                .await;
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::Semanticos(epoca, hits))))
                .await;
        });
        // Relanzar ABORTA la anterior, y abortar la cancela de verdad: el SDK
        // manda `rpc.cancel` al soltar la llamada. Dejarla correr sería pagar
        // un embed y un barrido del índice por una respuesta que la época ya
        // condena a descartarse.
        if let Some(vieja) = self.semantica_en_vuelo.replace(handle) {
            vieja.abort();
        }
        let cambio = ViewChange::Search {
            search: self.vista_busqueda(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// La respuesta del índice: entra si sigue siendo la consulta de ahora.
    pub(super) fn aplicar_semanticos(
        &mut self,
        epoca: u64,
        hits: Result<Vec<norte_proto::methods::SemanticHit>, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // La vista pudo cerrarse o relevarse mientras el embed corría.
        if self.busqueda.as_ref().is_none_or(|b| b.epoca != epoca) {
            return Vec::new();
        }
        let hits = match hits {
            // El barrido del wire es el COMPARTIDO: acota la `k` y rehúsa un
            // score no finito, que serializado como `null` envenenaría el
            // orden.
            Ok(h) => {
                let Some(h) = norte_frontend::validate_semantic_hits(h) else {
                    self.busqueda = None;
                    let mut fuera = vec![self.parche(vec![ViewChange::Search { search: None }])];
                    fuera.extend(self.decir("msg-semantic-bad-hits"));
                    return fuera;
                };
                h
            }
            Err(e) => {
                self.busqueda = None;
                let clave = match e {
                    // `NotFound` aquí NO es «no hay resultados»: es que ese
                    // root no tiene filas en el índice. Leerlo como una
                    // búsqueda vacía deja al lector creyendo que no hay nada
                    // parecido a lo que preguntó.
                    Error::NotFound => "msg-semantic-no-index",
                    Error::Unsupported => "msg-semantic-unsupported",
                    _ => norte_frontend::error::error_key(&e),
                };
                // La vista se QUEDA, diciendo por qué se rompió, en vez de
                // cerrarse dejando el motivo en la barra: ahí se lo lleva la
                // siguiente tecla, y entonces el lector se queda sin índice y
                // sin saberlo. Mismo trato que una búsqueda normal que falla
                // (`Desenlace::Failed` es persistente); las claves propias de
                // la semántica —«no hay índice», «no está soportado»— son las
                // que de verdad explican esto, así que ganan a la categoría
                // genérica del error.
                let motivo = clamp_display(norte_i18n::t_in(self.lang, clave));
                if let Some(b) = self.busqueda.as_mut() {
                    b.desenlace = Desenlace::Failed(motivo);
                }
                let mut fuera = vec![self.parche(vec![ViewChange::Search {
                    search: self.vista_busqueda(),
                }])];
                fuera.extend(self.decir(clave));
                return fuera;
            }
        };
        self.semantica_en_vuelo = None;
        if let Some(b) = self.busqueda.as_mut() {
            b.hits = hits
                .into_iter()
                .map(|h| Hallazgo {
                    path: h.path,
                    // El índice devuelve rutas y parecidos, no clases.
                    kind: None,
                    score: Some(h.score),
                })
                .collect();
            b.desenlace = Desenlace::Done;
        }
        let cambio = ViewChange::Search {
            search: self.vista_busqueda(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// Abre el prompt de crear directorio, con su campo de texto vacío.
    /// Arranca el buscador incremental del listado.
    ///
    /// Filtrar es el modo por DEFECTO —el que no mueve el listado bajo el
    /// cursor mientras se teclea—, pero lo elige `[ui] quick_search`, igual
    /// que en el terminal.
    pub(super) fn buscar_rapido(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // El modo lo dice `[ui] quick_search`, como en el terminal. Estaba a
        // fuego en `Filter`, así que `quick_search = "jump"` movía el cursor
        // en `ntc` y acotaba el listado en la ventana: la misma clave con dos
        // comportamientos.
        let modo = self.config.quick_search_mode;
        self.hueco_mut().pane.quick_start(modo);
        (self.aplicada(), vec![self.parche_filas()])
    }
}
