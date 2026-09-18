//! La ayuda: temas, contexto y páginas de extensión.
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
    /// Abre la ayuda sobre la página del CONTEXTO donde está el lector.
    ///
    /// Quien pulsa `F1` mirando una pregunta quiere la respuesta a ESA
    /// pregunta, no el índice. El contexto es una palabra cerrada
    /// (`dialog.confirm`, `viewer`, `browse`) y quién la reclama lo dice el
    /// propio corpus en su portada, así que añadir una página para un
    /// diálogo nuevo no toca este código.
    pub(super) fn abrir_ayuda(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Sobre un diálogo que se está TECLEANDO, no. La ayuda se queda el
        // teclado mientras está abierta, así que abrirla encima de un campo
        // de texto convierte el `⌫` que corrige una errata en un paso atrás
        // de la ayuda y el `enter` que confirma en otra cosa. El TUI lo
        // prohíbe por su nombre desde H3c y con el mismo razonamiento.
        if self
            .dialogos
            .last()
            .is_some_and(|d| d.vista.input.is_some())
        {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-help-over-input".to_owned(),
                },
                Vec::new(),
            );
        }
        let contexto = self.contexto_de_ayuda();
        self.ayuda = Some(crate::help::Ayuda::abrir(
            self.lang,
            contexto,
            &self.efectivo,
            &self.efectivo_visor,
            self.hechos(),
        ));
        // El catálogo de extensiones se pide y NO se espera: la ayuda se
        // pinta ya. La documentación es cosmética, y una ventana en blanco
        // hasta que el daemon conteste es peor que una lateral que gana
        // filas medio segundo más tarde. Un fallo no se dice: se pinta la
        // ayuda sin páginas de extensión.
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(PLAZO_PLUGINS, backend.plugin_list()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::PluginsDeAyuda(res))))
                .await;
        });
        let cambio = ViewChange::Help {
            help: self.vista_ayuda(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// El parche de la ayuda, y de paso la página que haya que pedir.
    ///
    /// UN sitio para las dos cosas a propósito: cualquier gesto que cambie de
    /// página —una flecha, un click, un enlace— puede aterrizar en la de una
    /// extensión, y esa página no está en el corpus, hay que pedirla. Un
    /// segundo camino que solo pintara sería una página de extensión que se
    /// queda en blanco según cómo se llegue a ella.
    pub(super) fn parche_de_ayuda(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.pedir_pagina_de_plugin(backend, buzon);
        let cambio = ViewChange::Help {
            help: self.vista_ayuda(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// El catálogo llegó: entra al modelo y, si el lector ya está sobre una
    /// página de extensión, se pide esa página.
    ///
    /// Si la ayuda se cerró mientras volaba, no hay nada que hacer: la foto
    /// era de una apertura que ya no existe, y guardarla para la siguiente
    /// sería enseñar un catálogo viejo.
    pub(super) fn aplicar_catalogo_de_plugins(
        &mut self,
        res: Result<norte_proto::methods::PluginListResult, Error>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Ok(lista) = res else {
            return Vec::new();
        };
        let Some(a) = self.ayuda.as_mut() else {
            return Vec::new();
        };
        let antes = a.estado.rows().to_vec();
        a.set_plugins(&lista.plugins);
        if a.estado.rows() == antes {
            // Un catálogo que no añade ninguna fila —ninguna extensión, o
            // ninguna con página y con id válido— no cambia la pantalla, y
            // un parche que no cambia nada obliga a un renderer a repintar
            // la ayuda entera para nada.
            return Vec::new();
        }
        self.parche_de_ayuda(backend, buzon)
    }

    /// Pide la página de la extensión abierta, si hay una y no se ha pedido
    /// ya en esta apertura de la ayuda.
    pub(super) fn pedir_pagina_de_plugin(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let Some(id) = self
            .ayuda
            .as_mut()
            .and_then(crate::help::Ayuda::reclamar_pagina)
        else {
            return;
        };
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res =
                match tokio::time::timeout(PLAZO_PLUGINS, backend.plugin_help(id.clone())).await {
                    Ok(r) => r,
                    Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
                };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::PaginaDePlugin(id, res))))
                .await;
        });
    }

    /// La página de una extensión llegó. Un fallo deja la página VACÍA con su
    /// nombre, que es mejor respuesta que un error encima de la ayuda — y es
    /// también lo que ve un daemon N-1 sin el método. Se pide una vez por
    /// apertura: cerrar y volver a abrir la ayuda es el reintento.
    pub(super) fn aplicar_pagina_de_plugin(
        &mut self,
        id: &str,
        res: Option<&norte_proto::methods::PluginHelpResult>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let a = self.ayuda.as_mut()?;
        a.instalar_pagina(id, res?);
        let cambio = ViewChange::Help {
            help: self.vista_ayuda(),
        };
        Some(self.parche(vec![cambio]))
    }

    /// Dónde está el lector, en el vocabulario del corpus.
    ///
    /// El diálogo de más arriba gana: es lo que tapa la pantalla y lo que el
    /// lector está mirando. Un diálogo que solo informa no tiene página
    /// propia y cae al listado, que es de lo que estaba hablando.
    pub(super) fn contexto_de_ayuda(&self) -> &'static str {
        debug_assert!(
            CONTEXTOS.contains(&self.contexto_calculado()),
            "un contexto fuera del vocabulario declarado"
        );
        self.contexto_calculado()
    }

    /// El cálculo, sin el ancla.
    pub(super) fn contexto_calculado(&self) -> &'static str {
        if let Some(d) = self.dialogos.last() {
            return match d.al_confirmar {
                // Una transferencia comparte página con el borrado: las dos
                // son «la pregunta que hay que responder antes de que algo
                // cambie», y el corpus tiene UNA que habla de eso.
                // La colisión tiene su PROPIA página en el corpus: sus teclas
                // son otras (`dialog.overwrite`, `dialog.skip`…), y mandar al
                // lector a la de confirmar le enseñaría las que no valen.
                Some(Pendiente::Reintentar { .. }) => "dialog.collision",
                // Cerrar cae aquí por lo mismo: es «responde antes de que
                // algo se pierda», y lo que se pierde es una copia a medias.
                Some(
                    Pendiente::Borrar { .. }
                    | Pendiente::Transferir { .. }
                    | Pendiente::Soltar { .. }
                    // Desinstalar es un borrado que pregunta: misma página
                    // que el borrado.
                    | Pendiente::DesinstalarExtension { .. }
                    | Pendiente::Salir,
                ) => "dialog.confirm",
                Some(
                    Pendiente::Decidir { .. }
                    | Pendiente::AprobarExtension { .. }
                    | Pendiente::DeshacerSesion { .. },
                ) => "dialog.approval",
                // Buscar comparte página con crear: los dos son el diálogo
                // que pide que teclees un nombre, y el corpus tiene UNA que
                // habla de eso.
                // Renombrar comparte página con crear y con buscar: los tres
                // son el diálogo que pide que teclees algo, y el corpus tiene
                // UNA que habla de eso.
                Some(Pendiente::InstruccionIa { .. }) => "dialog.ai-rename",
                // La plantilla del lote (#310): la misma página que la TUI le
                // da a su prompt, la del renombrado.
                Some(Pendiente::PlantillaLote { .. }) => "dialog.rename",
                // #327: el de la contraseña tiene su PROPIA página, la misma
                // que usa la TUI (`remote.md`, junto al TOFU). Mandarlo a la
                // de «teclea un nombre» sería la divergencia entre frontends
                // que ADR 0077 existe para evitar, cometida en el mismo cambio
                // que añade su test de paridad.
                Some(Pendiente::EntregarSecreto { .. }) => "dialog.ask-secret",
                // #311: el diálogo de sumas es un cuadro de LECTURA sobre lo
                // que hay bajo el cursor, como las propiedades.
                Some(Pendiente::CopiarSumas { .. }) => "dialog.properties",
                Some(
                    Pendiente::CrearDirectorio { .. }
                    | Pendiente::CrearFichero { .. }
                    | Pendiente::Buscar { .. }
                    | Pendiente::Renombrar { .. }
                    // La consulta semántica es otro diálogo que pide que
                    // teclees algo, y el corpus tiene UNA página que habla
                    // de eso.
                    | Pendiente::ConsultaSemantica
                    // Marcar por patrón, igual: un diálogo que pide que
                    // teclees algo.
                    | Pendiente::Patron { .. }
                    // Y empaquetar: lo que se teclea es el nombre del
                    // contenedor, de donde sale el formato.
                    | Pendiente::Empaquetar { .. }
                    // Partir pide un TAMAÑO, pero es el mismo diálogo de un
                    // campo de texto y una confirmación.
                    | Pendiente::Partir { .. }
                    // Y guardar el perfil pide un NOMBRE, que acaba siendo un
                    // directorio: mismo diálogo de un campo (#318).
                    | Pendiente::GuardarPerfil
                    // Y el valor de un ajuste de texto: un campo prellenado
                    // y dos botones.
                    | Pendiente::EditarAjuste { .. }
                    // Y los permisos piden un MODO, con la misma forma (#314).
                    // El corpus los documenta en la página de las propiedades,
                    // pero el CONTEXTO de teclas es este: un campo y dos
                    // botones.
                    | Pendiente::Permisos { .. }
                    // Y el nombre de un favorito (#309): un campo prellenado
                    // y dos botones, la misma forma que todos los de arriba.
                    | Pendiente::GuardarFavorito { .. },
                ) => "dialog.mkdir",
                None => "browse",
            };
        }
        if self.visor.is_some() {
            return "viewer";
        }
        "browse"
    }

    /// Los hechos con los que la ayuda atenúa una fila, congelados al abrir.
    ///
    /// Los dos de solo lectura salen de las capacidades del hueco, que se
    /// piden al aterrizar cada listado. Estuvieron cableados a `false` con un
    /// comentario que decía que el host no llevaba esa cuenta: la llevaba
    /// —desde #268— y tiraba todo menos el modo de plegado, así que dentro de
    /// un contenedor el terminal atenuaba F5/F8 y esta ventana los ofrecía
    /// encendidos. El ORIGEN es el hueco activo y el DESTINO es el que tiene
    /// el rol, que son exactamente los dos huecos por los que pregunta la
    /// tabla compartida.
    ///
    /// Sin destino designado —tres o más huecos sin rol, o ninguno más— la
    /// respuesta es `false`, y la divergencia con `App::help_facts` del
    /// terminal es DELIBERADA: allí se contesta con el hueco propio, que
    /// atenúa F5 diciendo «solo lectura» cuando el impedimento real es que no
    /// hay a dónde copiar. Una causa falsa enseña al lector algo que no es;
    /// aquí la tecla se ofrece y el rechazo llega con su nombre
    /// (`host-no-target-designated`, `host-no-other-slot`), que es
    /// información. Si alguien iguala los dos frontends, que sea moviendo el
    /// terminal hacia aquí.
    pub(super) fn hechos(&self) -> norte_frontend::availability::Facts {
        let hueco = self.hueco();
        // `cursor_entry`, que es la puerta de DESCRIBIR: con un quick filter
        // puesto el cursor crudo no se mueve y la fila señalada es otra, así
        // que indexar `entries()` por él describía una entrada que no es la
        // que el lector tiene delante.
        let entrada = hueco.pane.cursor_entry();
        norte_frontend::availability::Facts {
            // Lo mismo que navega `Activate`, y por el sitio compartido: un
            // contenedor y un enlace se entran igual que un directorio (ADR
            // 0077). Preguntarlo aquí por `kind == Dir` era atenuar `enter`
            // sobre un `.zip` que la tecla abre sin problema.
            enterable: entrada.is_some_and(|e| norte_frontend::nav::enter_target(e).is_some()),
            // Lo MISMO que rehúsa `pedir_visor`, que solo rehúsa un
            // directorio: un symlink se abre en el visor sin problema, y
            // atenuar F3 sobre uno decía «esto no aplica» de una tecla que
            // funciona. Los dos sitios se mueven juntos.
            viewable: entrada.is_some_and(|e| e.kind != EntryKind::Dir),
            rename_single: hueco.pane.marks_len() <= 1,
            source_read_only: self.solo_lectura(self.activo()),
            dest_read_only: self
                .hueco_destino()
                .is_ok_and(|destino| self.solo_lectura(destino)),
            // `degraded` en la tabla significa que la sesión va SIN CIFRAR,
            // que no es ninguno de los tres estados que este host proyecta
            // (conectado, reintentando, perdido). Mientras el wire de la
            // conexión no llegue hasta aquí, la respuesta honesta es que no
            // consta.
            degraded: false,
            // El host habla por el SDK contra el daemon, que es quien lleva
            // el journal (ADR 0066): lo que muta por aquí se registra y se
            // puede deshacer.
            journalled: true,
            // Y por lo mismo hay daemon con quien compartir la sesión: esta
            // ventana no tiene otro brazo (fase 9).
            daemon: true,
            // Una ventana se está pintando, así que hay escritorio donde
            // abrir la otra mitad del relevo. Preguntárselo al entorno aquí
            // sería preguntar si existe lo que se está usando.
            windowed: true,
        }
    }

    /// Re-congela los hechos de la ayuda si está abierta, y devuelve su
    /// parche (#262).
    ///
    /// El congelado de `abrir_ayuda` es contra que se mueva el LECTOR, no
    /// contra que se mueva el mundo. Dos de los hechos —`enterable` y
    /// `viewable`— describen la entrada bajo el cursor, y una copia o un
    /// borrado que terminan con la ayuda delante re-listan el panel por
    /// debajo: la frase de motivo se quedaba explicando por qué no aplica a
    /// una selección que ya no existe. No había despacho incorrecto
    /// —`activar_en_ayuda` vuelve a preguntar antes de correr—, pero una
    /// pantalla que explica algo falso es una pantalla que miente.
    pub(super) fn recongelar_ayuda(&mut self) -> Option<BridgeEnvelope<UiUpdate>> {
        if !self.recongelar_hechos_de_ayuda() {
            return None;
        }
        let cambio = ViewChange::Help {
            help: self.vista_ayuda(),
        };
        Some(self.parche(vec![cambio]))
    }

    /// Re-congela y NO fabrica parche. Para los caminos que ya mandan una
    /// foto: `parche` gasta un número de secuencia, y tirar el sobre después
    /// de gastarlo deja un HUECO en la secuencia — que es exactamente la
    /// condición que obliga al renderer a pedir una foto entera.
    ///
    /// Devuelve si la ayuda estaba abierta Y sus hechos han CAMBIADO.
    pub(super) fn recongelar_hechos_de_ayuda(&mut self) -> bool {
        if self.ayuda.is_none() {
            return false;
        }
        let hechos = self.hechos();
        self.ayuda.as_mut().is_some_and(|a| a.recongelar(hechos))
    }

    /// Una parte del detalle de un veredicto, en LÍNEAS separadas (#273).
    ///
    /// La causa y el nombre van en líneas distintas, que es la forma que
    /// tiene esta superficie de separarlos FUERA de banda: componerlos en
    /// una sola dejaba que un fichero llamado `✗ 4. ya existe: otro.txt`
    /// fabricara una entrada de la lista que no existe. El nombre viaja solo
    /// y con su marca, que es lo único que un tercero controla.
    pub(super) fn lineas_de_detalle(
        &self,
        parte: &norte_frontend::DetailPart,
    ) -> Vec<crate::dto::DialogLine> {
        use norte_frontend::DetailPart;
        let plana = |texto: String| crate::dto::DialogLine {
            text: clamp_display(texto),
            hostile: false,
        };
        match parte {
            DetailPart::Temp { count } => vec![plana(norte_i18n::ta_in(
                self.lang,
                "modal-rename-batch-temp",
                &[("n", &count.to_string())],
            ))],
            DetailPart::Collision {
                index,
                kind_key,
                name,
                hostile,
            } => {
                let kind = norte_i18n::t_in(self.lang, kind_key);
                let causa = match index {
                    Some(n) => norte_i18n::ta_in(
                        self.lang,
                        "modal-rename-batch-collision-prefix",
                        &[("n", &n.to_string()), ("kind", &kind)],
                    ),
                    None => norte_i18n::ta_in(
                        self.lang,
                        "modal-rename-batch-collision-prefix-unindexed",
                        &[("kind", &kind)],
                    ),
                };
                vec![
                    plana(causa),
                    crate::dto::DialogLine {
                        text: clamp_display(name.clone()),
                        hostile: *hostile,
                    },
                ]
            }
            DetailPart::More {
                shown,
                total,
                hostile,
            } => vec![crate::dto::DialogLine {
                text: clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "modal-rename-batch-collision-more",
                    &[("shown", &shown.to_string()), ("total", &total.to_string())],
                )),
                // El resumen no lleva nombre, pero SÍ la marca de que alguna
                // de las ocultas lo tiene hostil: lo escondido no se cuela
                // limpio.
                hostile: *hostile,
            }],
        }
    }

    /// La proyección de la ayuda.
    pub(super) fn vista_ayuda(&self) -> Option<crate::dto::HelpView> {
        let a = self.ayuda.as_ref()?;
        Some(a.vista(self.lang, self.efectos, self.visor.is_some()))
    }

    /// Las teclas mientras la ayuda está abierta.
    ///
    /// FIJAS a propósito, como las de la paleta y por el mismo motivo: el
    /// catálogo no tiene comandos para «filtrar esta lista», «cambiar de
    /// mitad» o «seguir este enlace». Son las que la propia ayuda anuncia en
    /// su pie (`help-hint-gui`), y esa cadena y este `match` cambian juntos.
    pub(super) fn tecla_en_ayuda(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // `Ctrl+P` sale de la ayuda a la paleta, que es lo que su pie
        // promete. Los DOS cambios viajan en el mismo parche: un renderer que
        // solo recibiera el de la paleta seguiría pintando la ayuda debajo.
        if k.ctrl && !k.alt && !k.meta && k.key.eq_ignore_ascii_case("p") {
            self.ayuda = None;
            self.paleta = Some(norte_frontend::palette_state::Palette::with_recent(
                self.filas_de_paleta(),
                &self.paleta_recientes,
            ));
            self.pedir_filas_de_plugin(backend, buzon);
            let cambios = vec![
                ViewChange::Help { help: None },
                ViewChange::Palette {
                    palette: self.vista_paleta(),
                },
            ];
            return (self.aplicada(), vec![self.parche(cambios)]);
        }
        // FILTRANDO, las teclas son LETRAS: resolverlas por el keymap
        // convertiría escribir «documento» en abrir, cerrar y filtrar. Solo
        // `Escape`, `Backspace` y `Enter` siguen significando algo, y esos
        // tres van por su nombre porque el filtro es un campo de texto.
        let filtrando = self.ayuda.as_ref().is_some_and(|a| a.estado.filtering());
        let verbo = if filtrando {
            None
        } else {
            self.verbo_de_dialogo(k)
        };
        let Some(a) = self.ayuda.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match (verbo.as_deref(), k.key.as_str()) {
            (Some("dialog.cancel"), _) | (None, "Escape" | "esc") => {
                // Filtrando, `esc` deja de filtrar y no cierra: cerrar la
                // ayuda entera por abandonar una búsqueda es perder la
                // página que se estaba leyendo.
                if a.estado.filtering() {
                    a.estado.end_filter();
                } else {
                    self.ayuda = None;
                }
            }
            (Some("dialog.pane"), _) => a.estado.toggle_focus(),
            (Some("dialog.down"), _) => a.estado.down(),
            (Some("dialog.up"), _) => a.estado.up(),
            // La página la da el MODELO, que sabe lo que significa en la
            // LATERAL: camina por los temas y enseña uno, en vez de diez
            // transiciones de página por tecla.
            //
            // En el CUERPO no. El cuerpo de una página cruza el puente
            // entero y quien lo desplaza es el DOM, que es lo que un
            // renderer con scroll nativo hace bien y sin preguntar; el
            // renderer ni siquiera manda estas teclas cuando el cuerpo tiene
            // el foco. Moverlo aquí crearía una SEGUNDA verdad sobre por
            // dónde va la ayuda —el `scrollTop` del DOM y el `body_scroll`
            // del modelo— y solo una de las dos se pinta (#267). El modelo
            // conserva su paginación de cuerpo porque el TUI la usa: ahí no
            // hay scroll nativo que delegar.
            (Some("dialog.page-down"), _)
                if a.estado.focus() == norte_frontend::help::Focus::Topics =>
            {
                a.estado.page_down(PAGINA_DE_AYUDA);
            }
            (Some("dialog.page-up"), _)
                if a.estado.focus() == norte_frontend::help::Focus::Topics =>
            {
                a.estado.page_up(PAGINA_DE_AYUDA);
            }
            (Some("dialog.back"), _) | (None, "Backspace" | "backspace") => {
                if a.estado.filtering() {
                    a.estado.backspace();
                } else if !a.estado.back() {
                    // En la raíz, «atrás» es cerrar: el lector no tiene a
                    // dónde volver y una tecla que no hace nada se lee como
                    // una ventana colgada.
                    self.ayuda = None;
                }
            }
            (Some("dialog.confirm"), _) | (None, "Enter" | "enter") => {
                return self.enter_en_ayuda(backend, buzon);
            }
            (Some("dialog.filter"), _) if !a.estado.filtering() => a.estado.start_filter(),
            (_, otra) => {
                // Una tecla de TEXTO es un punto de código, no un nombre de
                // tecla (`ArrowLeft` no se teclea), y solo cuenta con el
                // filtro abierto: teclear «d» leyendo una página no puede
                // ponerse a filtrar sola.
                let mut chars = otra.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if a.estado.filtering() && !k.ctrl && !k.alt && !k.meta => {
                        a.estado.push_char(c);
                    }
                    _ => return (self.aplicada(), Vec::new()),
                }
            }
        }
        (self.aplicada(), self.parche_de_ayuda(backend, buzon))
    }

    /// `enter` sobre la ayuda: abrir la página elegida, seguir un enlace o
    /// correr un comando.
    pub(super) fn enter_en_ayuda(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(a) = self.ayuda.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        if a.estado.focus() == norte_frontend::help::Focus::Topics {
            a.estado.open_selected();
            return (self.aplicada(), self.parche_de_ayuda(backend, buzon));
        }
        let i = a.estado.action_cursor();
        self.activar_en_ayuda(u32::try_from(i).unwrap_or(u32::MAX), backend, buzon)
    }

    /// Actúa sobre la fila `i` del cuerpo, venga de `enter` o de un click.
    ///
    /// La comprobación de si se PUEDE vive aquí, en el host, y no en quien
    /// pinta: el renderer no le pone escuchador a una fila apagada, pero el
    /// teclado no pasa por ahí, así que delegarla dejaba que `enter` corriera
    /// una fila atenuada.
    pub(super) fn activar_en_ayuda(
        &mut self,
        i: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let (lang, efectos, hay_visor) = (self.lang, self.efectos, self.visor.is_some());
        let Some(a) = self.ayuda.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let i = i as usize;
        let veredicto = a.accion_ejecutable(i, lang, efectos, hay_visor);
        // Señalar la fila es la mitad del click que NO ejecuta, y se hace
        // igual: tras seguir un enlace, la siguiente flecha se mueve por
        // donde el lector señaló.
        a.senalar(i);
        match veredicto {
            Ok(accion) => self.actuar_en_ayuda(Some(accion), backend, buzon),
            Err(clave) if clave.is_empty() => {
                // Una fila que ya no existe: el renderer iba un frame por
                // detrás, y eso no es un error.
                (Self::obsoleta(StaleAction::Modal), Vec::new())
            }
            Err(clave) => {
                // Atenuada: se dice por qué y la página SIGUE abierta. Cerrar
                // la ayuda para negarse sería quitarle al lector la página
                // donde está la explicación.
                self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, &clave)));
                let cambios = vec![
                    ViewChange::Status(self.status.clone()),
                    ViewChange::Help {
                        help: self.vista_ayuda(),
                    },
                ];
                (
                    ActionAck::Unavailable { reason_key: clave },
                    vec![self.parche(cambios)],
                )
            }
        }
    }

    /// Lo que hace una acción del cuerpo, venga de `enter` o de un click.
    pub(super) fn actuar_en_ayuda(
        &mut self,
        accion: Option<norte_frontend::help::Action>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match accion {
            Some(norte_frontend::help::Action::Open(id)) => {
                if let Some(a) = self.ayuda.as_mut() {
                    a.estado.open(&id);
                }
                (self.aplicada(), self.parche_de_ayuda(backend, buzon))
            }
            Some(norte_frontend::help::Action::Run(cmd)) => {
                // Correr cierra la ayuda: el comando actúa sobre el listado
                // que la ayuda estaba tapando. El cierre viaja PRIMERO y en
                // su propio parche, de modo que el acuse que devuelve el
                // efecto sigue apuntando a la actualización que lo refleja.
                self.ayuda = None;
                let cierre = self.parche(vec![ViewChange::Help { help: None }]);
                let Some(efecto) = efecto_de(&cmd, 1) else {
                    let (ack, mut resto) = self.no_implementado(&cmd);
                    let mut envios = vec![cierre];
                    envios.append(&mut resto);
                    return (ack, envios);
                };
                let (ack, mut resto) = self.aplicar_efecto(efecto, backend, buzon);
                let mut envios = vec![cierre];
                envios.append(&mut resto);
                (ack, envios)
            }
            None => (self.aplicada(), Vec::new()),
        }
    }

    /// Un click en una fila de la lateral de la ayuda: ENSEÑA lo que haya,
    /// que es lo mismo que hace la flecha.
    pub(super) fn elegir_pagina(
        &mut self,
        row: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(a) = self.ayuda.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        a.estado.click_row(row as usize);
        (self.aplicada(), self.parche_de_ayuda(backend, buzon))
    }
}
