//! Copiar, mover y renombrar.
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
    /// El DIRECTORIO al que va una transferencia, o por qué no hay uno.
    ///
    /// El destino declarado tiene que seguir sirviendo: existir, verse, y no
    /// ser uno mismo —copiarse encima no es una operación—.
    ///
    /// Sin destino hay dos situaciones DISTINTAS, y decir la misma frase en
    /// las dos manda a buscar otro panel a quien tiene tres. La capa
    /// compartida deja el rol SIN FIJAR cuando hay varios candidatos y
    /// ninguno elegido (ADR 0058 D7): eso no es «no hay otro», es «elige
    /// cuál».
    pub(super) fn directorio_destino(&self) -> Result<VPath, &'static str> {
        self.hueco_destino()
            .map(|id| self.huecos[&id].pane.dir().clone())
    }

    /// El HUECO destino, con la misma regla que [`Self::directorio_destino`].
    ///
    /// Los dos por el mismo camino: una comparación necesita el hueco (para
    /// navegar el lado derecho) y una transferencia necesita su directorio,
    /// y dos formas de decidir «el otro panel» son dos sitios donde divergir.
    pub(super) fn hueco_destino(&self) -> Result<u32, &'static str> {
        let activo = self.activo();
        let destino_id = self
            .roles
            .get(RoleId::Target)
            .map(|SlotId(id)| id)
            .filter(|id| *id != activo && self.huecos.contains_key(id) && !self.oculto(*id));
        if let Some(id) = destino_id {
            return Ok(id);
        }
        let candidatos = self
            .huecos
            .keys()
            .filter(|id| **id != activo && !self.oculto(**id))
            .count();
        Err(if candidatos > 1 {
            "host-no-target-designated"
        } else {
            "host-no-other-slot"
        })
    }

    /// Abre la confirmación de una copia o un movimiento. NO transfiere.
    ///
    /// El origen son las marcas del hueco activo (o el cursor si no hay
    /// ninguna) y el destino es el DIRECTORIO del hueco con el rol `Target`.
    /// Ni una ni otro los nombra el renderer: manda `pane.copy` y punto. Es
    /// la misma regla que dejó sin parámetro al comando de los bytes de una
    /// imagen (ADR 0069), y por el mismo motivo — un nombre que viene de la
    /// webview es un nombre que la webview puede elegir.
    /// ¿Hay dos entradas del lote cuyos NOMBRES son uno solo en el destino?
    ///
    /// Se pliega con la clave compartida bajo el modo del DESTINO —la trampa
    /// del dominio de siempre: la caja y la normalización las decide el sitio
    /// al que van, no el del que salen—. Sin modo todavía (el hueco acaba de
    /// aterrizar, o el daemon no contestó) no se pliega: esto es una cortesía
    /// del cliente y la autoridad es el core.
    pub(super) fn dos_marcas_pliegan_igual(&self, paths: &[VPath]) -> bool {
        let Some(modo) = self
            .hueco_destino()
            .ok()
            .and_then(|id| self.huecos.get(&id))
            .and_then(|h| h.pliegue)
        else {
            return false;
        };
        if modo == norte_encoding::FoldMode::None {
            return false;
        }
        let mut vistas = std::collections::HashSet::new();
        paths
            .iter()
            .filter_map(|p| p.file_name())
            .any(|n| !vistas.insert(norte_encoding::name_key(n.as_bytes(), modo)))
    }

    /// Los dos topes de un lote (#271), o `None` si cabe.
    ///
    /// Se preguntan antes de abrir diálogo alguno: preguntar por algo que no
    /// se va a poder hacer es peor que decirlo de entrada.
    pub(super) fn lote_no_cabe(&self, cuantas: usize) -> Option<&'static str> {
        if cuantas > MAX_TRANSFER_BATCH {
            return Some("host-batch-too-large");
        }
        // Y que quepa en lo que el host RETIENE: el desalojo solo puede tirar
        // tasks terminales, así que un lote sobre un tablero ya lleno de vivas
        // no tendría dónde caer.
        if self.tasks.len().saturating_add(cuantas) > MAX_TASKS_RETAINED {
            return Some("host-task-board-full");
        }
        None
    }

    /// Sobre QUÉ opera una transferencia hacia `destino`, o el motivo por el
    /// que no se puede preguntar siquiera. Devuelve `(origen_dir, paths)`.
    ///
    /// El destino llega por PARÁMETRO desde #284: casi siempre sale del rol
    /// compartido, pero con un solo listado en pantalla lo elige el lector en
    /// el selector del escritorio, y las dos formas tienen que pasar por las
    /// mismas comprobaciones.
    pub(super) fn operandos_de_transferencia(
        &self,
        destino: &VPath,
    ) -> Result<(VPath, Vec<VPath>), &'static str> {
        let destino = destino.clone();
        let origen_dir = self.hueco().pane.dir().clone();
        if origen_dir == destino {
            // Los dos listados en el mismo sitio. El daemon lo rechazaría
            // igual, pero abrir un diálogo que promete algo imposible es
            // peor que decirlo antes.
            //
            // BYTE A BYTE a propósito (#269): en un volumen que pliega,
            // `/casa/docs` y `/casa/DOCS` son el mismo sitio y este atajo NO
            // los ve. Saberlo cuesta un `fs.capabilities` —o sea un viaje al
            // daemon delante de CADA diálogo de copia—, y el error de este
            // lado solo puede ser por PERMISIVO: la autoridad es
            // `norte_core::ops`, que sí pliega (#215) y devuelve
            // `InvalidPath`. Ser más estricto aquí sí rompería algo: negaría
            // una operación legítima en un volumen sensible a la caja.
            return Err("host-same-directory");
        }
        // `marked_paths` ya cae al cursor cuando no hay marcas: es la fuente
        // única de «sobre qué opera esto», y duplicar aquí ese respaldo
        // sería un segundo sitio del que se pueden separar.
        let paths: Vec<VPath> = self.hueco().pane.marked_paths();
        if paths.is_empty() {
            return Err("msg-nothing-selected");
        }
        if let Some(motivo) = self.lote_no_cabe(paths.len()) {
            return Err(motivo);
        }
        // Dos marcas que PLIEGAN al mismo nombre en el destino (#268): en un
        // ext4 `README.txt` y `readme.txt` son dos ficheros, y en NTFS o APFS
        // son uno. Encolar las dos deja que una gane —cuál, no es
        // determinista— y que la otra falle sin explicación sobre un miembro
        // arbitrario de la pareja. Con `CollisionPolicy::Fail` el resultado es
        // al menos un error visible; el día que la ventana ofrezca elegir
        // sobrescribir, el mismo lote pierde un fichero en silencio.
        if self.dos_marcas_pliegan_igual(&paths) {
            return Err("host-batch-folds-to-one");
        }
        // Una entrada sin último segmento es una RAÍZ, y una raíz no tiene
        // nombre que componer en el destino. Se rechaza el lote entero en vez
        // de saltársela: transferir «casi todo lo que pediste» en silencio es
        // exactamente lo que no puede hacer una mutación.
        if paths.iter().any(|p| p.file_name().is_none()) {
            return Err("host-cannot-transfer-root");
        }
        let _ = destino;
        Ok((origen_dir, paths))
    }

    /// Abre la confirmación de copiar o mover, resolviendo el destino.
    ///
    /// Con un solo listado en pantalla no hay panel destino, y hasta #284 eso
    /// era el final del camino: la operación se rehusaba y quien no había
    /// partido la ventana no podía copiar. Ahora se le pregunta al escritorio.
    pub(super) fn pedir_transferencia(
        &mut self,
        mover: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match self.directorio_destino() {
            Ok(destino) => self.confirmar_transferencia(&destino, mover),
            // Sin OTRO hueco al que apuntar: lo elige el lector fuera.
            Err("host-no-other-slot") => self.pedir_destino_al_escritorio(mover),
            Err(reason_key) => (
                ActionAck::Unavailable {
                    reason_key: reason_key.to_owned(),
                },
                Vec::new(),
            ),
        }
    }

    /// Cuántas filas caben en el visor, según lo mide el renderer.
    ///
    /// Al menos una: un visor de cero filas no pinta nada y su paginación
    /// dividiría por cero.
    pub(super) fn fijar_filas_del_visor(
        &mut self,
        rows: u32,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.visor_filas = Some(usize::try_from(rows).unwrap_or(1).max(1));
        let cambio = ViewChange::Viewer {
            viewer: self.vista_visor(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Le pide al ESCRITORIO que el lector elija el destino (#284).
    ///
    /// Se recuerda solo el VERBO —copiar o mover—, no los operandos: cuando la
    /// respuesta vuelva se recalculan del estado de entonces. Congelar aquí
    /// las marcas sería prometer una operación sobre un listado que el lector
    /// pudo cambiar mientras el selector estaba abierto.
    pub(super) fn pedir_destino_al_escritorio(
        &mut self,
        mover: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // El directorio del panel es solo la SUGERENCIA de dónde abrir el
        // selector, y por eso no se exige que sea local: lo que el selector
        // devuelve es siempre una carpeta de esta máquina, y copiar de un
        // `sftp://` a una carpeta local es una operación legítima que el core
        // hace desde siempre. Con un panel remoto, quien ejecuta abre donde
        // pueda — la sugerencia se pierde, la operación no.
        let desde = self.hueco().pane.dir().clone();
        if !self.nativo(crate::dto::NativeEffect::PickDirectory { desde }) {
            return Self::sin_escritorio();
        }
        self.destino_pendiente = Some(mover);
        (self.aplicada(), self.decir("host-pick-destination"))
    }

    /// Volvió el selector del escritorio (#284).
    ///
    /// `None` = se cerró sin elegir, y entonces no pasa nada: cancelar es una
    /// respuesta. Con una ruta, se confirma como cualquier otra transferencia
    /// — y eso significa que el destino se ENSEÑA antes de mover un byte, que
    /// es lo que acota que la ruta haya pasado por el renderer.
    pub(super) fn destino_elegido(
        &mut self,
        path: Option<String>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(mover) = self.destino_pendiente.take() else {
            // Nadie pidió un destino: una respuesta que no contesta a ninguna
            // pregunta no se interpreta.
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let Some(nativa) = path else {
            return (self.aplicada(), Vec::new());
        };
        let Some(destino) = norte_frontend::shell::vpath_de_ruta_nativa(&nativa) else {
            let fuera = self.decir("host-bad-destination");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-bad-destination".to_owned(),
                },
                fuera,
            );
        };
        self.confirmar_transferencia(&destino, mover)
    }

    /// Llegaron ficheros soltados desde el escritorio (#283).
    ///
    /// No copia: abre la misma confirmación que copiar, con el destino en su
    /// campo y los nombres enmascarados. La lista la compone OTRO proceso, así
    /// que enseñarla antes de escribir no es cortesía —es la única ocasión que
    /// tiene el lector de ver que lo que llegó no es lo que arrastró.
    ///
    /// Lo que no convierte a `VPath` se descarta, y el recorte se DICE: quedan
    /// nueve de diez y copiar sin avisar sería mentir sobre el lote.
    pub(super) fn soltados(
        &mut self,
        paths: &[String],
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let llegaron = paths.len();
        let usables: Vec<VPath> = paths
            .iter()
            .filter_map(|p| norte_frontend::shell::vpath_de_ruta_nativa(p))
            // Una raíz no tiene nombre que darle en el destino, y el envío la
            // saltaría en silencio: se cae aquí, donde todavía se puede contar.
            .filter(|v| v.file_name().is_some())
            .collect();
        if usables.is_empty() {
            let clave = if llegaron == 0 {
                "host-drop-empty"
            } else {
                "host-drop-unusable"
            };
            let fuera = self.decir(clave);
            return (
                ActionAck::Unavailable {
                    reason_key: clave.to_owned(),
                },
                fuera,
            );
        }
        if let Some(motivo) = self.lote_no_cabe(usables.len()) {
            let fuera = self.decir(motivo);
            return (
                ActionAck::Unavailable {
                    reason_key: motivo.to_owned(),
                },
                fuera,
            );
        }
        // Mismo motivo que en una copia normal (#268): dos que pliegan al
        // mismo nombre dejan que una gane sin decir cuál. Y aquí el lote no lo
        // eligió el lector marcando, así que descubrirlo después sería aún
        // menos explicable.
        if self.dos_marcas_pliegan_igual(&usables) {
            let fuera = self.decir("host-batch-folds-to-one");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-batch-folds-to-one".to_owned(),
                },
                fuera,
            );
        }
        // El panel ACTIVO, y sin exigir que sea local: subir al servidor lo que
        // se arrastra del escritorio es el caso cómodo, y el core copia entre
        // providers desde siempre.
        let destino = self.hueco().pane.dir().clone();
        let destino_linea = Self::linea_de_ruta(&destino);
        let cuerpo: Vec<crate::dto::DialogLine> = usables
            .iter()
            .take(Self::MAX_LINEAS_DIALOGO)
            .map(Self::linea_de_ruta)
            .collect();
        // El recorte cuenta contra lo que LLEGÓ, no contra lo que se pudo
        // convertir: «se enseñan 16 de 40» tiene que seguir siendo cierto
        // cuando cuatro de esas 40 se cayeron por el camino.
        let nota = self.nota_de_recorte(cuerpo.len(), llegaron.max(usables.len()));
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-drop-title".to_owned(),
            destination: Some(destino_linea),
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: cuerpo,
            overflow_note: nota,
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
            input: None,
            input_hostile: false,
            input_secret: false,
        };
        self.dialogos.push(Dialogo {
            id,
            vista,
            tecleado: Tecleado::Texto(String::new()),
            reconocido: true,
            al_confirmar: Some(Pendiente::Soltar {
                paths: usables,
                destino,
            }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// La confirmación propiamente dicha, con el destino ya resuelto.
    pub(super) fn confirmar_transferencia(
        &mut self,
        destino: &VPath,
        mover: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let activo = self.activo();
        let destino = destino.clone();
        let (origen_dir, paths) = match self.operandos_de_transferencia(&destino) {
            Ok(t) => t,
            Err(reason_key) => {
                return (
                    ActionAck::Unavailable {
                        reason_key: reason_key.to_owned(),
                    },
                    Vec::new(),
                );
            }
        };
        // El destino va en SU CAMPO, no como una línea con una flecha: un
        // directorio puede llamarse `docs → /casa/BORRAR` y esa flecha es
        // legítima, no se enmascara y no se marca, así que la línea se leería
        // como dos rutas y quien confirma creería estar mandando sus ficheros
        // a la segunda (fixture `arrow_join_spoof` del corpus canónico).
        let destino_linea = Self::linea_de_ruta(&destino);
        // Y los orígenes, enmascarados y acotados igual que el listado: estos
        // nombres los controla quien haya escrito en el directorio.
        let cuerpo: Vec<crate::dto::DialogLine> = paths
            .iter()
            .take(Self::MAX_LINEAS_DIALOGO)
            .map(Self::linea_de_ruta)
            .collect();
        let nota = self.nota_de_recorte(cuerpo.len(), paths.len());
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: if mover {
                "modal-move-title"
            } else {
                "modal-copy-title"
            }
            .to_owned(),
            destination: Some(destino_linea),
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: cuerpo,
            overflow_note: nota,
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    // Ni copiar ni mover se marcan destructivos, y es una
                    // decisión: `destructive` es lo que hace que `Enter`
                    // elija cancelar, y F5/F6 son las dos teclas que más se
                    // pulsan de un gestor ortodoxo. Lo que destruye es el
                    // borrado, y ese sí lo lleva.
                    destructive: false,
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
        };
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            tecleado: Tecleado::Texto(String::new()),
            reconocido: true,
            al_confirmar: Some(Pendiente::Transferir {
                origen: activo,
                origen_dir,
                paths,
                destino,
                mover,
            }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Resuelve el nombre confirmado y encola el rename, o dice por qué no.
    pub(super) fn confirmar_rename(
        &mut self,
        from: &VPath,
        siembra: &str,
        escrito: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        match Self::bytes_del_rename(from, siembra, escrito) {
            Ok(destino) => {
                Self::lanzar_rename(from.clone(), destino, backend, buzon);
                (None, Vec::new())
            }
            Err(clave) => {
                // El motivo vuelve para que el ACUSE lo diga, no solo la
                // barra: un renderer que recibe `Applied` cree que la
                // operación salió, y la misma superficie contestaba
                // `Unavailable` cuando el rechazo era por las marcas.
                self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
                let cambio = ViewChange::Status(self.status.clone());
                (Some(clave), vec![self.parche(vec![cambio])])
            }
        }
    }

    /// Encola el `fs.move` de UN rename, con el destino ya compuesto.
    ///
    /// Aparte de [`Self::lanzar_transferencia`] porque el destino de un
    /// rename es una RUTA COMPLETA y el de una transferencia es un
    /// DIRECTORIO sobre el que se compone el nombre del origen. Pasar el uno
    /// por el otro renombraría a `nuevo/nombre-viejo`, que es exactamente el
    /// tipo de error que un parámetro con dos significados produce.
    ///
    /// Mismo verbo del wire, misma entrada de journal y mismo camino de
    /// deshacer que mover: lo que cambia es la pregunta, no el efecto.
    pub(super) fn lanzar_rename(
        from: VPath,
        to: VPath,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        // El directorio del que sale y al que llega es el MISMO, así que una
        // sola entrada: relistarlo dos veces sería pedir el mismo listado dos
        // veces.
        let afectados: Vec<VPath> = from.parent().into_iter().collect();
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let mensaje = match backend
                .move_(from, to, norte_proto::CollisionPolicy::Fail)
                .await
            {
                Ok(task) => Mensaje::TaskNueva(Box::new((task, afectados, None))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon.send(mensaje).await;
        });
    }

    /// Encola las Tasks del lote y engancha su progreso al actor.
    ///
    /// El nombre del destino se compone AQUÍ, con el último segmento del
    /// origen tal cual: bytes, sin normalizar y sin pasar por pantalla. Un
    /// nombre que ha ido a la webview y ha vuelto es otro nombre (ADR 0061).
    /// Lo que la composición byte a byte NO puede resolver es un nombre legal
    /// en el origen e ilegal en el destino (`CON`, un punto final, un `:`
    /// yendo de ext4 a NTFS): eso es cosa del provider de destino, y está en
    /// la issue #217.
    ///
    /// La política de colisión es `Fail`, el default seguro del wire: si el
    /// destino existe, la Task falla y el tablero lo dice. Sobrescribir o
    /// renombrar son decisiones del lector, y esta ventana todavía no tiene
    /// dónde tomarlas — elegirlas por él sería la clase de silencio que borra
    /// ficheros.
    /// Abre el lote y lanza el envío. Las dos cosas van juntas siempre.
    ///
    /// El lote se abre ANTES de lanzar, con el número que se va a pedir: la
    /// cuenta tiene que existir antes de que llegue el primer desenlace, que
    /// con una task que nace terminal puede ser antes de que el bucle de envío
    /// haya pedido la segunda. Uno solo no es un lote: su desenlace ya se dice
    /// en su fila y su rechazo en la barra, con la frase tipada del error.
    pub(super) fn enviar_lote(
        &mut self,
        paths: &[VPath],
        origen_dir: &VPath,
        destino: &VPath,
        mover: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        self.lote = (paths.len() > 1).then(|| Lote {
            total: paths.len(),
            ..Lote::default()
        });
        Self::lanzar_transferencia(paths, origen_dir, destino, mover, backend, buzon);
    }

    pub(super) fn lanzar_transferencia(
        paths: &[VPath],
        origen_dir: &VPath,
        destino: &VPath,
        mover: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        // Los directorios que el desenlace deja desactualizados. En una copia
        // solo el destino; en un movimiento, también de donde sale — y el de
        // origen se toma del HUECO, no del padre de cada entrada: el padre lo
        // escribe el provider y el hueco puede venir de la config o de la
        // sesión, así que en NFD contra NFC, o contra un servidor sin
        // distinción de caja, son dos cadenas para el mismo sitio y la
        // comparación byte a byte del refresco no encontraría el panel
        // (ADR 0061). Se apuntan los dos: uno de ellos casa.
        let mut afectados = vec![destino.clone()];
        if mover {
            afectados.push(origen_dir.clone());
        }
        let mut trabajos: Vec<(VPath, VPath)> = Vec::with_capacity(paths.len());
        for path in paths {
            let Some(nombre) = path.file_name() else {
                // Imposible aquí: `pedir_transferencia` rechaza el lote
                // entero si alguna entrada es una raíz. Se comprueba igual
                // porque la alternativa es un `unwrap` en el camino de una
                // mutación.
                continue;
            };
            if mover
                && let Some(padre) = path.parent()
                && !afectados.contains(&padre)
            {
                afectados.push(padre);
            }
            trabajos.push((path.clone(), destino.join(nombre.clone())));
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        // UNA task de envío para el lote entero, y las llamadas EN SERIE. Un
        // `spawn` por entrada abría tantas RPC simultáneas como marcas
        // hubiera: marcar unos miles de ficheros y pulsar F5 es el flujo
        // normal de un gestor ortodoxo, y contra SFTP eso no es una copia,
        // es una denegación de servicio contra el propio daemon. En serie el
        // daemon sigue haciendo el trabajo en paralelo si quiere; lo que se
        // acota es cuántas peticiones hay volando a la vez.
        tokio::spawn(async move {
            for (from, to) in trabajos {
                // El par ORIGINAL viaja con la task (#274): si esto choca, es
                // lo único con lo que se puede volver a intentar con otra
                // política. Recomponerlo desde el progreso no vale — dice qué
                // fichero va por dentro, no qué se pidió.
                let reintento = Reintento {
                    from: from.clone(),
                    to: to.clone(),
                    mover,
                };
                let encolada = if mover {
                    backend
                        .move_(from, to, norte_proto::CollisionPolicy::Fail)
                        .await
                } else {
                    backend
                        .copy(from, to, norte_proto::CollisionPolicy::Fail)
                        .await
                };
                let mensaje = match encolada {
                    Ok(task) => {
                        Mensaje::TaskNueva(Box::new((task, afectados.clone(), Some(reintento))))
                    }
                    // A la CUENTA del lote, no a la barra: N rechazos eran N
                    // mensajes de los que solo sobrevivía el último (#271).
                    Err(e) => Mensaje::TaskDeLoteRechazada(Box::new(e)),
                };
                if buzon.send(mensaje).await.is_err() {
                    // El actor ya no está: lo que quede del lote no le
                    // importa a nadie, y seguir pidiéndolo sí importaría.
                    return;
                }
            }
        });
    }
}
