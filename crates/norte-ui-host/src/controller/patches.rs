//! Los parches: qué cruza el bridge y con qué generación.
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
    pub(super) fn aplicada(&self) -> ActionAck {
        // La secuencia que lo reflejará: la siguiente que se emita.
        ActionAck::Applied {
            sequence: self.sequence + 1,
        }
    }

    /// Una carrera normal entre lo que el renderer creía y lo que hay.
    pub(super) fn obsoleta(reason: StaleAction) -> ActionAck {
        ActionAck::Stale { reason }
    }

    pub(super) fn sobre(&mut self, u: UiUpdate) -> BridgeEnvelope<UiUpdate> {
        // Una foto entera lleva la barra dentro: es lo último que el
        // renderer vio de ella, y lo que el siguiente parche compara.
        if let UiUpdate::Snapshot(s) = &u {
            self.ultima_barra = Some(s.panel_bar.clone());
            self.ultimos_elementos = Some(s.status_items.clone());
            // La foto se armó con el ajuste de AHORA (`ajuste_de` es puro).
            let _ = self.ajustes_movidos();
        }
        self.sequence += 1;
        BridgeEnvelope::new(self.instance.clone(), self.sequence, u)
    }

    pub(super) fn parche(&mut self, mut changes: Vec<ViewChange>) -> BridgeEnvelope<UiUpdate> {
        // La barra de paneles va en CUALQUIER parche que la cambie, sin que
        // el sitio que arma el parche lo sepa (#324). Es la traducción del
        // «se deriva por frame» de la TUI: allí `panel_buttons` corre en cada
        // pintado; aquí el puente solo habla cuando algo cambia, así que se
        // compara con la última que cruzó. Un panel abierto por tecla, por
        // menú, por paleta o por la barra misma actualiza la barra igual.
        let barra = self.vista_barra_de_paneles();
        if self.ultima_barra.as_ref() != Some(&barra) {
            changes.push(ViewChange::PanelBar {
                panel_bar: barra.clone(),
            });
            self.ultima_barra = Some(barra);
        }
        // Y los elementos de la barra de estado (ADR 0132), por el mismo
        // mecanismo: los mueven el cursor, las marcas, el orden y el tablero,
        // y ninguno de esos caminos sabe que hay una barra que los cuenta.
        let elementos = self.vista_elementos_de_estado();
        if self.ultimos_elementos.as_ref() != Some(&elementos) {
            changes.push(ViewChange::StatusItems {
                status_items: elementos.clone(),
            });
            self.ultimos_elementos = Some(elementos);
        }
        // Y el ajuste de columnas de cada hueco: lo mueven el ancho del
        // hueco y los nombres del listado, y ninguno de los caminos que los
        // cambian manda cabecera. Cabecera y filas van JUNTAS, detrás de lo
        // que el parche ya traía, así que mandan sobre ello.
        for slot in self.ajustes_movidos() {
            if let Some(h) = self.huecos.get(&slot) {
                changes.push(ViewChange::Columns {
                    slot_id: slot,
                    columns: self.cabeceras(slot, h),
                });
            }
            changes.push(self.cambio_de_filas_de(slot));
        }
        let base = self.sequence;
        self.sobre(UiUpdate::Patch(ViewPatch {
            base_sequence: base,
            changes,
        }))
    }

    /// Solo la ventana visible viaja: un directorio de cien mil entradas no
    /// cruza el bridge para pintar cuarenta filas.
    pub(super) fn filas_visibles(&self) -> Vec<RowView> {
        self.filas_de(self.activo(), self.hueco())
    }

    /// Las filas visibles de un hueco cualquiera.
    pub(super) fn filas_de(&self, slot: u32, hueco: &Hueco) -> Vec<RowView> {
        let primera = usize::try_from(hueco.primera_visible).unwrap_or(0);
        let cuantas = usize::try_from(hueco.visibles).unwrap_or(0);
        // UNA vez por lote, no una por fila: recorre el mapa de tasks entero y
        // clona un `VPath` por task viva. Dentro de `fila` eso era un recorrido
        // y un clon por ENTRADA visible, en cada repintado, y los repintados
        // los provoca justo lo que llena esa lista: el progreso.
        let operandos = self.operandos_vivos();
        // Y el ajuste de columnas, por lo mismo: la cabecera y todas las
        // filas del lote tienen que salir del MISMO.
        let columnas = self.ajuste_de(slot, hueco);
        hueco
            .pane
            .entries()
            .iter()
            .enumerate()
            .skip(primera)
            .take(cuantas.min(MAX_ROWS_PER_BATCH))
            .map(|(i, e)| self.fila(hueco, i, e, &operandos, &columnas))
            .collect()
    }

    /// La generación de un listado: la ÉPOCA de `PaneState`, que sube en
    /// cada cosa que mueve los índices —un re-listado, un re-orden, un
    /// filtro de ocultos—, no solo al cambiar de directorio.
    pub(super) fn generacion(&self) -> u64 {
        self.hueco().pane.listing_epoch()
    }

    /// Mover el cursor manda el cursor, no el listado.
    pub(super) fn parche_cursor(&mut self) -> BridgeEnvelope<UiUpdate> {
        let cambio = ViewChange::Cursor {
            slot_id: self.activo(),
            generation: self.generacion(),
            cursor: Some(RowKey(self.hueco().pane.cursor() as u64)),
        };
        self.parche(vec![cambio])
    }

    /// Sondea lo que la ventana visible de un hueco todavía no sabe.
    ///
    /// El listado local es PEREZOSO a propósito (#52): `readdir` da el tipo
    /// pero no el tamaño, y statear medio millón de entradas para pintar
    /// cuarenta filas es justo lo que esa decisión evita. Quien enseña
    /// columnas de tamaño y fecha tiene que pedirlas para lo que se ve — el
    /// TUI lo hace desde su bucle, y esto es lo mismo con la ventana que el
    /// renderer declaró. La regla de QUÉ hace falta es la compartida
    /// (`needs_stat_at`), no una de aquí.
    /// Pide a los plugins lo que quieran decir de la VENTANA VISIBLE.
    ///
    /// Dos cosas en un viaje —insignias y valores de columna `plugin:`—
    /// porque son la misma pregunta sobre las mismas rutas, y el TUI ya lo
    /// hace así.
    ///
    /// De la ventana y NO del listado, que es donde esto se separa del TUI:
    /// el terminal decora «todas las entradas cargadas» porque su pane no
    /// declara una ventana, y aquí el renderer sí la declara. Cada llamada
    /// levanta una instancia de wasm por plugin y #224 midió **167 ms por
    /// página de 20 sobre 2000 entradas**: pedirlo para lo que no se ve es
    /// pagar ese precio por nada, multiplicado por el tamaño del directorio.
    ///
    /// Un hueco OCULTO no pregunta. Lo que no se ve no se trae, igual que su
    /// listado.
    ///
    /// Todo fail-soft: sin decoradores consentidos, con el catálogo caído o
    /// con la RPC rota, el listado se pinta igual y sin insignias. Una
    /// decoración es cosmética por contrato (ADR 0037).
    pub(super) fn adornar(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        if self.oculto(slot) {
            return;
        }
        let columnas = self
            .huecos
            .get(&slot)
            // Las pintadas y las de la barra de estado (ADR 0137): la MISMA
            // lista que pide la TUI.
            .map(|h| {
                norte_frontend::columns::plugin_requests(
                    &self.columnas,
                    &self.config.common.ui_status_plugins,
                    h.pane.dir().scheme(),
                )
            })
            .unwrap_or_default();
        let Some(hueco) = self.huecos.get_mut(&slot) else {
            return;
        };
        // Una tanda por hueco, comprobada ANTES de elegir candidatos: al
        // revés, los elegidos quedarían marcados como pedidos sin haberlo
        // sido y no se pedirían nunca más. Es la misma trampa que `sondear`
        // documenta, y se cae en ella igual de fácil.
        if hueco.adornando {
            return;
        }
        let primera = usize::try_from(hueco.primera_visible).unwrap_or(0);
        let cuantas = usize::try_from(hueco.visibles).unwrap_or(0);
        let (candidatos, clases): (Vec<VPath>, Vec<norte_proto::EntryKind>) = hueco
            .pane
            .entries()
            .iter()
            .skip(primera)
            .take(cuantas)
            .filter(|e| !hueco.adornadas.contains(&e.path))
            .map(|e| (e.path.clone(), e.kind))
            .unzip();
        if candidatos.is_empty() {
            return;
        }
        for p in &candidatos {
            hueco.adornadas.insert(p.clone());
        }
        let dir = hueco.pane.dir().clone();
        hueco.adornando = true;
        let generacion = hueco.gen_adornos;
        let cancelar = hueco.cancelar_sondeo.clone();
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let crudas = backend
                .plugin_decorate(candidatos.clone(), clases)
                .await
                .unwrap_or_default();
            let adornos = norte_frontend::merge_decorations(&candidatos, &crudas);
            let (celdas, rotulos) = celdas_de_plugin(&backend, &columnas, &candidatos, || {
                cancelar.load(std::sync::atomic::Ordering::SeqCst)
            })
            .await;
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::Adornos(Box::new((
                    generacion, slot, dir, adornos, celdas, rotulos,
                ))))))
                .await;
        });
    }

    pub(super) fn sondear(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let attrs = self
            .huecos
            .get(&slot)
            .map(|h| self.attrs_de(h.pane.dir()))
            .unwrap_or_default();
        let Some(hueco) = self.huecos.get_mut(&slot) else {
            return;
        };
        // Una sonda por hueco, y se comprueba ANTES de elegir candidatos: si
        // se eligen y luego se abandona la tanda, esos paths quedan marcados
        // como sondeados sin haberlo sido, y no se piden nunca más. Sin este
        // orden, un scroll con debounce apilaba tandas de doscientos viajes
        // contra la misma conexión y además se comía filas por el camino.
        if hueco.sondeando {
            return;
        }
        let primera = usize::try_from(hueco.primera_visible).unwrap_or(0);
        let cuantas = usize::try_from(hueco.visibles).unwrap_or(0);
        let candidatos: Vec<VPath> = hueco
            .pane
            .needs_stat_at(primera..primera.saturating_add(cuantas))
            .into_iter()
            .filter(|p| !hueco.sondeados.contains(p))
            .take(MAX_SONDEOS)
            .collect();
        if candidatos.is_empty() {
            return;
        }
        for p in &candidatos {
            hueco.sondeados.insert(p.clone());
        }
        let dir = hueco.pane.dir().clone();
        hueco.sondeando = true;
        let cancelar = hueco.cancelar_sondeo.clone();
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            use futures::StreamExt as _;
            // En PARALELO acotado: una sesión remota no puede pagar N viajes
            // en serie (200 sondas a 80 ms de ida y vuelta son dieciséis
            // segundos), y con plazo, porque un provider colgado no puede
            // llevarse por delante las otras 199.
            let sondas: Vec<(VPath, Entry)> = futures::stream::iter(candidatos)
                .map(|p| {
                    let backend = Arc::clone(&backend);
                    let attrs = attrs.clone();
                    async move {
                        let stat = backend.stat(p.clone(), attrs);
                        match tokio::time::timeout(PLAZO_SONDEO, stat).await {
                            // Un sondeo que falla o que tarda no es un error
                            // de pantalla: esa celda se queda en blanco y no
                            // se vuelve a pedir.
                            Ok(Ok(e)) => Some((p, e)),
                            _ => None,
                        }
                    }
                })
                .buffer_unordered(SONDEOS_A_LA_VEZ)
                .filter_map(|x| async move { x })
                .collect()
                .await;
            if cancelar.load(std::sync::atomic::Ordering::SeqCst) || sondas.is_empty() {
                // El listado cambió mientras se sondeaba: lo que vuelve no
                // describe la pantalla que hay.
                let _ = buzon
                    .send(Mensaje::Hidratado(Box::new((dir, slot, Vec::new()))))
                    .await;
                return;
            }
            let _ = buzon
                .send(Mensaje::Hidratado(Box::new((dir, slot, sondas))))
                .await;
        });
    }

    /// Pega un lote del relleno al listado que lo pidió.
    ///
    /// `None` si el hueco desapareció o el lote es de una navegación ya
    /// relevada: pegarlo sería mezclar dos árboles en una pantalla.
    pub(super) fn aplicar_lote(
        &mut self,
        slot: u32,
        token: RequestToken,
        batch: Vec<Entry>,
        ultimo: bool,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let hueco = self.huecos.get_mut(&slot)?;
        if hueco.drenando != Some(token) {
            // Un lote de una navegación que ya fue relevada: pegarlo sería
            // mezclar dos árboles en una pantalla.
            return None;
        }
        if ultimo {
            // Se acabó el stream: este hueco ya no está creciendo.
            hueco.drenando = None;
        }
        if batch.is_empty() && !hueco.filas_por_publicar {
            // Nada que pegar y nada pendiente: el stream cerró sin resto.
            return None;
        }
        // Lo que se ve AHORA, para compararlo con lo que se verá. Un
        // directorio de cien mil entradas se drena en lotes de 500 y cada
        // lote publicaba su parche: doscientos parches en ráfaga contra un
        // canal de 64, o sea que cualquier suscriptor que no drene a esa
        // velocidad recibe `Lagged` y tiene que pedir una foto entera. Y casi
        // todos esos parches llevaban las MISMAS filas: lo que se estaba
        // mezclando caía muy por debajo de la ventana visible (#252).
        let antes = self
            .huecos
            .get(&slot)
            .map(|h| self.filas_de(slot, h))
            .unwrap_or_default();
        if !batch.is_empty()
            && let Some(hueco) = self.huecos.get_mut(&slot)
        {
            hueco.pane.extend(batch);
        }
        let despues = self
            .huecos
            .get(&slot)
            .map(|h| self.filas_de(slot, h))
            .unwrap_or_default();
        // Callar un parche no es gratis: `extend` sube la ÉPOCA del listado y
        // el renderer nombra cada fila con la época en la que la vio, así que
        // un renderer al que se le callan todos los parches se queda con una
        // época vieja y cada clic suyo se rechaza por rancio. Por eso lo que
        // se calla se APUNTA, y el último lote —aunque venga vacío, que pasa
        // cuando el resto es múltiplo exacto del lote— salda la deuda.
        let calla = !ultimo && antes == despues;
        if let Some(h) = self.huecos.get_mut(&slot) {
            // Se calla: queda deuda. Se publica: la deuda se salda, porque el
            // parche lleva la época de AHORA.
            h.filas_por_publicar = calla;
        }
        if calla {
            return None;
        }
        Some(self.parche_filas_de(slot))
    }

    /// Pega lo que un sondeo averiguó al listado que lo pidió.
    ///
    /// `None` si no hay nada que repintar: el hueco desapareció, o el listado
    /// que se sondeó ya fue relevado —pegarle tamaños a otro directorio sería
    /// mentir sobre lo que se ve—.
    pub(super) fn aplicar_sondas(
        &mut self,
        slot: u32,
        dir: &VPath,
        sondas: &[(VPath, Entry)],
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let hueco = self.huecos.get_mut(&slot)?;
        // La bandera se baja SOLO si lo que llega describe este listado. Una
        // tanda cancelada que aterriza tarde bajaba la de la tanda NUEVA, y
        // entonces `sondear` dejaba lanzar una segunda sobre el mismo hueco.
        if hueco.pane.dir() != dir {
            // El hueco está en OTRO directorio: pegarle estos tamaños sería
            // mentir sobre lo que se ve. (Un lote de relleno, en cambio, no
            // invalida nada: sube la época y desplaza índices, y aquí se casa
            // por ruta.)
            return None;
        }
        hueco.sondeando = false;
        for (pedido, e) in sondas {
            // Por la ruta que se PIDIÓ: la que devuelve el provider puede ser
            // otra ortografía del mismo nombre (NFD en HFS+, otra caja en
            // SMB, el destino de un enlace) y entonces no casa con nada — y
            // como ya está en `sondeados`, no se reintenta jamás.
            hueco.pane.hydrate(pedido, e.size, e.mtime_ms);
        }
        Some(self.parche_filas_de(slot))
    }

    /// Las filas visibles de UN hueco concreto, no del que tenga el foco.
    pub(super) fn parche_filas_de(&mut self, slot: u32) -> BridgeEnvelope<UiUpdate> {
        let cambio = self.cambio_de_filas_de(slot);
        // La cabecera va CON las filas: `pane.names-encoding` retranscribe la
        // ruta igual que los nombres, y ocultar mueve entradas dentro y fuera
        // del listado. Mandar solo las filas dejaba el título con la lectura
        // vieja.
        let cabecera = self
            .huecos
            .get(&slot)
            .map(|h| self.cabecera_de(slot, h))
            .into_iter();
        self.parche(std::iter::once(cambio).chain(cabecera).collect())
    }

    /// El cambio de FILAS de un hueco cualquiera, sin envolver.
    fn cambio_de_filas_de(&self, slot: u32) -> ViewChange {
        let (generacion, primera, filas, total, iconos) = match self.huecos.get(&slot) {
            Some(h) => (
                h.pane.listing_epoch(),
                h.primera_visible,
                self.filas_de(slot, h),
                Some(h.pane.entries().len() as u64),
                h.pane.any_icon(),
            ),
            None => (0, 0, Vec::new(), None, false),
        };
        ViewChange::Rows {
            slot_id: slot,
            generation: generacion,
            first_visible: primera,
            rows: filas,
            icon_column: iconos,
            // El total va CON las filas: es la altura del desplazamiento del
            // renderer, y el drenaje paginado no manda otra cosa —tampoco en
            // el último lote—.
            total_rows: total,
        }
    }

    /// Los huecos cuyo ajuste de columnas ya no es el último que cruzó, con
    /// el nuevo ya apuntado como cruzado.
    fn ajustes_movidos(&mut self) -> Vec<u32> {
        let ahora: Vec<(u32, Vec<norte_frontend::columns::Fitted>)> = self
            .huecos
            .iter()
            .map(|(id, h)| (*id, self.ajuste_de(*id, h)))
            .collect();
        let mut movidos = Vec::new();
        for (id, ajuste) in ahora {
            if self.ultimo_ajuste.get(&id) != Some(&ajuste) {
                self.ultimo_ajuste.insert(id, ajuste);
                movidos.push(id);
            }
        }
        self.ultimo_ajuste
            .retain(|id, _| self.huecos.contains_key(id));
        movidos
    }

    /// Lo que cambia una marca o un scroll: las filas visibles.
    pub(super) fn parche_filas(&mut self) -> BridgeEnvelope<UiUpdate> {
        let cambio = self.cambio_de_filas();
        let cabecera = self.cabecera_de(self.activo(), self.hueco());
        self.parche(vec![cambio, cabecera])
    }

    /// El cambio de FILAS del hueco activo, sin envolver: para quien tenga
    /// que mandarlo junto a otros en un mismo parche.
    pub(super) fn cambio_de_filas(&self) -> ViewChange {
        ViewChange::Rows {
            slot_id: self.activo(),
            generation: self.generacion(),
            first_visible: self.hueco().primera_visible,
            rows: self.filas_visibles(),
            icon_column: self.hueco().pane.any_icon(),
            // Marcar u ocultar no cambia solo qué filas se ven: `toggle-hidden`
            // mueve entradas dentro y fuera del listado, o sea que el total y
            // la altura del desplazamiento se mueven con ellas.
            total_rows: Some(self.hueco().pane.entries().len() as u64),
        }
    }

    /// Una fila del listado, con los operandos vivos YA calculados.
    ///
    /// Los recibe en vez de pedirlos: `operandos_vivos` recorre el mapa de
    /// tasks y clona una ruta por task viva, y hacer eso por fila convertía
    /// un repintado de cincuenta filas con veinte tareas en mil recorridos y
    /// mil clones.
    pub(super) fn fila(
        &self,
        hueco: &Hueco,
        i: usize,
        e: &Entry,
        operandos: &[(VPath, Option<u8>)],
        columnas: &[norte_frontend::columns::Fitted],
    ) -> RowView {
        let bytes = e
            .path
            .file_name()
            .map_or(&[][..], norte_proto::Segment::as_bytes);
        // CON la reinterpretación que el panel tenga puesta (#57): sin ella
        // `pane.names-encoding` ciclaba por dentro y la pantalla no cambiaba,
        // que es un comando que solo se puede leer como roto. Lo que se
        // reinterpreta es el PINTADO; los bytes no se tocan, y la fila sigue
        // marcada como hostil (regla 1).
        // La fila de SUBIR se pinta `..` y no el nombre del directorio padre,
        // que es lo que dice su ruta: el nombre del padre en la primera fila
        // se lee como «hay aquí un directorio que se llama así». Ni badge
        // hostil ni reinterpretación — dos ASCII no son el nombre de nadie.
        let (texto, hostil) = if hueco.pane.is_parent_row(i) {
            ("..".to_owned(), false)
        } else {
            norte_frontend::display_name_with(bytes, hueco.pane.name_encoding())
        };
        // Lo sirve el PANE, que re-enmascara al servir: el host acumula pero
        // no es quien decide qué se pinta.
        let adorno = hueco.pane.decoration_for(&e.path);
        // El color del tema para ESTA entrada (`[files.ext]` / `[files.kind]`).
        // Contra los bytes CRUDOS, no contra `texto`: ese va enmascarado y
        // reinterpretado para pintar, y el enmascarado no es inyectivo — una
        // extensión casada sobre él sería la extensión de otro nombre.
        // El mapeo es de `norte-frontend` y no de aquí: lo necesitan los dos
        // frontends y es la MISMA decisión, que escrita dos veces diverge en
        // silencio (ADR 0077).
        let estilo = self.tema.estilo_de_entrada(
            bytes,
            norte_frontend::theme::file_kind_of(e.kind),
            self.esquema_oscuro,
        );
        RowView {
            key: RowKey(i as u64),
            display_name: clamp_display(texto),
            hostile: hostil,
            kind: match e.kind {
                EntryKind::Dir => RowKind::Dir,
                EntryKind::File => RowKind::File,
                EntryKind::Symlink => RowKind::Symlink,
                EntryKind::Other => RowKind::Other,
            },
            // Por dónde va la tarea que trabaja sobre ESTA fila (spec
            // 2026-09-15): lo decide el crate compartido, que casa por ruta
            // exacta y se queda con la menos avanzada.
            progress: norte_frontend::processes::progress_for(
                operandos.iter().map(|(r, p)| (r, *p)),
                &e.path,
            ),
            selected: i == hueco.pane.cursor(),
            marked: hueco.pane.is_marked(e),
            cells: self.celdas(hueco, e, columnas),
            badge: adorno
                .and_then(|d| d.badge.clone())
                .map(clamp_display)
                .unwrap_or_default(),
            badge_hostile: adorno.is_some_and(|d| d.badge_hostile),
            badge_role: adorno
                .and_then(|d| d.role)
                .map_or_else(String::new, |r| r.as_kebab().to_owned()),
            icon: adorno
                .and_then(|d| d.icon.clone())
                .map(clamp_display)
                .unwrap_or_default(),
            icon_hostile: adorno.is_some_and(|d| d.icon_hostile),
            name_color: estilo.color,
            name_bold: estilo.bold,
            name_dim: estilo.dim,
            name_italic: estilo.italic,
            name_underline: estilo.underline,
        }
    }

    /// Sobre qué está trabajando cada task viva, con su porcentaje.
    ///
    /// La ruta es la del ÚLTIMO progreso (`current`), que es lo que el wire
    /// dice que se está tocando ahora; una task terminada no cuenta, porque su
    /// fila ya no está esperando a nadie.
    pub(super) fn operandos_vivos(&self) -> Vec<(VPath, Option<u8>)> {
        self.tasks
            .values()
            .filter(|t| !Self::terminal(t.vista.state))
            .filter_map(|t| {
                // El progreso EN VIVO y no la vista: la vista es una foto que
                // se proyecta al salir el mensaje del buzón, y la fila de un
                // listado se pinta mucho más a menudo que eso.
                let p = t.progreso.borrow();
                p.current
                    .as_ref()
                    .map(|ruta| (ruta.clone(), norte_frontend::tasks::progress_pct(&p)))
            })
            .collect()
    }

    /// El catálogo de la localización de un path, si ya llegó.
    pub(super) fn catalogo_de(&self, path: &VPath) -> Option<&norte_proto::AttrCatalog> {
        self.catalogos.get(path.scheme())
    }

    /// Las celdas de una fila, una por columna configurada.
    ///
    /// Las construye `norte_frontend::columns::styled_cell`, que es la misma
    /// función que usa el TUI: el formato de un tamaño o de una fecha no
    /// puede depender de quién pinta. `None` es AUSENCIA —un directorio sin
    /// tamaño, un atributo que el provider no mandó— y viaja como tal: jamás
    /// un `0` fabricado.
    ///
    /// Solo las de `columnas`, el ajuste del hueco (`ajuste_de`): una celda
    /// de una columna que la cabecera cedió se pintaría sin ancho.
    pub(super) fn celdas(
        &self,
        hueco: &Hueco,
        e: &Entry,
        columnas: &[norte_frontend::columns::Fitted],
    ) -> Vec<crate::dto::CellView> {
        use norte_frontend::columns::{ColumnId, styled_cell_in};
        let ahora = ahora_ms();
        let esquema = hueco.pane.dir().scheme().to_owned();
        columnas
            .iter()
            .filter(|f| {
                !matches!(
                    f.id,
                    ColumnId::Builtin(norte_frontend::columns::Builtin::Name)
                )
            })
            .map(|f| {
                let col = &f.id;
                let texto = match col {
                    // Las de plugin no viven en la `Entry` sino en el
                    // side-map del pane: se resuelven por ese camino.
                    ColumnId::Plugin { plugin, column } => hueco.pane.plugin_cell(
                        &norte_frontend::columns::plugin_display_id(plugin, column),
                        &e.path,
                    ),
                    // El estilo CONFIGURADO, igual que la cabecera: con
                    // `default_for_id` un `format = "iso"` no hacía nada aquí
                    // mientras el terminal sí lo honraba, y la fecha salía
                    // siempre relativa.
                    //
                    // Y con el IDIOMA del host: la clase, un booleano y la
                    // fecha relativa traducen, y salían en el del proceso —
                    // cada celda de fecha del listado bajo una cabecera en
                    // otro idioma.
                    otra => styled_cell_in(
                        e,
                        otra,
                        ahora,
                        &self
                            .columnas
                            .style_for_id(&esquema, otra, self.catalogo_de(&e.path))
                            .compacted(f.compact),
                        self.lang,
                    ),
                };
                crate::dto::CellView {
                    // Identidad: entera o vacía, jamás recortada.
                    column: identidad_de_columna(col),
                    text: texto.map(clamp_display),
                }
            })
            .collect()
    }
}
