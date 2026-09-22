//! El panel: cursor, marcas, foco, orden y columnas.
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
    /// Mueve el cursor del hueco, topando en los extremos.
    pub(super) fn mover_cursor(
        &mut self,
        slot_id: u32,
        delta: i64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if slot_id != self.activo() {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        if self.hueco().pane.entries().is_empty() {
            return (self.aplicada(), Vec::new());
        }
        let actual = i128::try_from(self.hueco().pane.cursor()).unwrap_or(0);
        let ultimo = i128::try_from(self.hueco().pane.entries().len() - 1).unwrap_or(0);
        let destino = (actual + i128::from(delta)).clamp(0, ultimo);
        let i = usize::try_from(destino).unwrap_or(0);
        self.hueco_mut().pane.set_cursor(i);
        (self.aplicada(), vec![self.parche_cursor()])
    }

    /// Pone el cursor en una fila concreta (un click).
    pub(super) fn poner_cursor(
        &mut self,
        slot_id: u32,
        key: RowKey,
        generation: u64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(i) = self.fila_de(slot_id, key, generation) else {
            // Una fila que ya no existe: el listado cambió bajo el click. Ni
            // se interpreta ni es un error.
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        self.hueco_mut().pane.set_cursor(i);
        (self.aplicada(), vec![self.parche_filas()])
    }

    /// Marca o desmarca una fila.
    pub(super) fn marcar(
        &mut self,
        slot_id: u32,
        key: RowKey,
        generation: u64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(i) = self.fila_de(slot_id, key, generation) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        let marcada = self
            .hueco()
            .pane
            .entries()
            .get(i)
            .is_some_and(|e| self.hueco().pane.is_marked(e));
        self.hueco_mut().pane.set_mark(i, !marcada);
        (self.aplicada(), vec![self.parche_filas()])
    }

    /// La fila que una acción nombra, si el hueco es el activo y la clave
    /// sigue valiendo EN LA GENERACIÓN que el renderer dijo.
    ///
    /// El par `(clave, generación)` es lo que hace que la clave signifique
    /// algo: sola es un índice, y un índice de la pantalla anterior nombra
    /// otro fichero. Un lote de relleno que aterriza entre el pintado y el
    /// click reordena el listado y sube la época; sin esta comparación, el
    /// click marca lo que haya caído en esa fila.
    pub(super) fn fila_de(&self, slot_id: u32, key: RowKey, generation: u64) -> Option<usize> {
        if slot_id != self.activo() || self.hueco().pane.listing_epoch() != generation {
            return None;
        }
        self.fila_valida(key)
    }

    /// Este frontend todavía no muta, y lo DICE.
    ///
    /// Una tecla muda es peor que un «aquí no»: el usuario que pulsa F8 y no
    /// ve nada no sabe si borró.
    pub(super) fn no_muta() -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        (
            ActionAck::Unavailable {
                reason_key: "host-read-only".to_owned(),
            },
            Vec::new(),
        )
    }

    /// Mueve el foco al siguiente hueco enfocable, o al anterior.
    ///
    /// Con `solo_listados`, los paneles laterales se saltan: es `pane.switch`,
    /// el `Tab` ortodoxo, y lo que contesta es «el otro panel». Sin él es
    /// `layout.focus-next`, el recorrido de la pantalla entera.
    pub(super) fn mover_foco(
        &mut self,
        atras: bool,
        solo_listados: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // El recorrido es el COMPARTIDO: `focus_order` ya se salta lo
        // que no se ve y lo que no se enfoca (una barra de estado no
        // recibe el foco), así que aquí no hay una segunda regla que
        // pueda divergir de la del TUI.
        //
        // Con una salvedad, y es la que este anillo tiene que aplicar: lo
        // ENFOCABLE no es lo que TOMA TECLAS. La hoja de atributos es lo
        // primero y no lo segundo —sigue al cursor del listado, y con el
        // teclado dentro dejaría de seguir a nada—, así que pararse ahí es
        // una parada de la que ninguna tecla saca. Se salta, y la vuelta se
        // da igual porque el recorrido cicla.
        let actual = SlotId(self.enfocado());
        let siguiente = self.siguiente_del_anillo(actual, atras, solo_listados);
        let Some(SlotId(id)) = siguiente else {
            // Un solo hueco: no hay a dónde ir, y decirlo es más
            // honesto que fingir que pasó algo.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-other-slot".to_owned(),
                },
                Vec::new(),
            );
        };
        self.roles.set(RoleId::Active, SlotId(id));
        self.reconcilia_roles();
        // El árbol sigue al panel activo, y acaba de cambiar cuál es. Va por
        // FOTO porque el árbol no tiene `ViewChange` propio: viaja entero o no
        // viaja, y un cursor de árbol que no cruza deja el panel señalando la
        // rama del panel anterior.
        //
        // Solo si el árbol se MOVIÓ de verdad. Aterrizar en la barra de sitios
        // no lo mueve —`seguir_ramas` solo sigue a un listado—, y mandar la
        // pantalla entera por eso es pagar una foto por un parche de reparto.
        let activo = self.activo();
        if self.seguir_ramas(activo, backend, buzon) {
            let snap = self.snapshot();
            return (
                self.aplicada(),
                vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
            );
        }
        let cambio = ViewChange::Layout(self.disposicion());
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Arrastra el borde entre `slot` y el hueco de al lado hasta `cells`.
    ///
    /// `cells` es dónde está el PUNTERO, y todo lo demás se resuelve aquí: qué
    /// pareja forma ese borde, cuánto ocupan juntos y en qué dirección
    /// reparten. El renderer solo convierte píxeles a celdas, que es lo que ya
    /// hace para declarar su viewport.
    ///
    /// Se busca el vecino en el REPARTO y no en el árbol: lo que el lector ha
    /// agarrado es un borde de la pantalla, y dos huecos son vecinos cuando
    /// uno empieza donde acaba el otro. Sin vecino no hay borde, y entonces
    /// esto no es un arrastre sino una carrera con un reparto anterior.
    pub(super) fn arrastrar_borde(
        &mut self,
        slot: u32,
        cells: u16,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some((_, ra)) = self
            .reparto
            .placements
            .iter()
            .find(|(SlotId(id), _)| *id == slot)
            .copied()
        else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        // El vecino: el que empieza justo donde este acaba, en uno de los dos
        // ejes.
        let derecha = self.reparto.placements.iter().find(|(_, r)| {
            r.x == ra.x + ra.width && r.y < ra.y + ra.height && ra.y < r.y + r.height
        });
        let abajo = self.reparto.placements.iter().find(|(_, r)| {
            r.y == ra.y + ra.height && r.x < ra.x + ra.width && ra.x < r.x + r.width
        });
        let (vecino, dir) = match (derecha, abajo) {
            (Some((b, _)), _) => (*b, norte_frontend::layout::Dir::Horizontal),
            (None, Some((b, _))) => (*b, norte_frontend::layout::Dir::Vertical),
            (None, None) => return (Self::obsoleta(StaleAction::Generation), Vec::new()),
        };
        // La pareja de verdad es la del reparto donde los dos son vecinos, y
        // se mide ENTERA: el borde entre el segundo listado y los detalles
        // separa el cuerpo de los detalles, no ese listado de ellos.
        let Some((izq, der)) = self.arbol.border_pair(SlotId(slot), vecino) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        let Some((inicio, largo)) =
            norte_frontend::layout::border_span(&self.reparto, &izq, &der, dir)
        else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        if largo == 0 {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let frac = f32::from(cells.saturating_sub(inicio)) / f32::from(largo);
        let arbol = self
            .arbol
            .drag_border_between(SlotId(slot), vecino, frac, largo);
        if arbol == self.arbol {
            // El borde no se movió: ni foto ni parche. Un arrastre emite un
            // evento por píxel, y repintar la pantalla entera por cada uno
            // sería pagar una foto por temblor de mano.
            return (self.aplicada(), Vec::new());
        }
        self.aplicar_disposicion(arbol, backend, buzon)
    }

    /// El siguiente hueco del recorrido compartido que sirve como parada.
    ///
    /// Con `solo_listados`, solo los `browser`; sin él, cualquiera que TOME
    /// TECLAS. Lo enfocable no es lo que toma teclas: la hoja de atributos es
    /// lo primero y no lo segundo —sigue al cursor del listado, y con el
    /// teclado dentro dejaría de seguir a nada—, así que pararse ahí sería una
    /// parada de la que ninguna tecla saca.
    ///
    /// Da como mucho una vuelta entera: si ninguno sirve —una pantalla que
    /// solo tenga hoja de atributos, que el reparto permite— devuelve `None`
    /// en vez de girar para siempre.
    pub(super) fn siguiente_del_anillo(
        &self,
        desde: SlotId,
        atras: bool,
        solo_listados: bool,
    ) -> Option<SlotId> {
        let mut actual = desde;
        for _ in 0..self.reparto.focus_order.len() {
            let siguiente = if atras {
                norte_frontend::layout::focus_prev(&self.reparto, actual)?
            } else {
                norte_frontend::layout::focus_next(&self.reparto, actual)?
            };
            if siguiente == desde {
                return None; // dio la vuelta sin encontrar ninguno
            }
            let para = kind_de(&self.arbol, siguiente).is_some_and(|k| {
                if solo_listados {
                    k == norte_frontend::layout::KindId::browser()
                } else {
                    self.kinds.get(&k).is_some_and(|d| d.takes_keys)
                }
            });
            if para {
                return Some(siguiente);
            }
            actual = siguiente;
        }
        None
    }

    /// Designa OTRO hueco visible como destino de la siguiente operación.
    pub(super) fn designar_destino(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // El siguiente que NO sea el enfocado: designarse a uno mismo
        // como destino es pedirle a una copia que se copie encima.
        // Misma regla que el TUI.
        let activo = self.activo();
        let candidatos: Vec<u32> = self
            .huecos
            .keys()
            .copied()
            .filter(|id| *id != activo && !self.oculto(*id))
            .collect();
        let actual = self.roles.get(RoleId::Target).map(|SlotId(id)| id);
        let siguiente = match actual.and_then(|a| candidatos.iter().position(|c| *c == a)) {
            Some(i) => candidatos.get((i + 1) % candidatos.len()).copied(),
            None => candidatos.first().copied(),
        };
        let Some(id) = siguiente else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-other-slot".to_owned(),
                },
                Vec::new(),
            );
        };
        self.roles.set(RoleId::Target, SlotId(id));
        let cambio = ViewChange::Layout(self.disposicion());
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Ordena un listado por una columna, con la regla compartida.
    ///
    /// El id viaja como texto porque así viajó su cabecera; lo que significa
    /// —y si invierte o empieza de nuevo— lo resuelve `norte-frontend`, no
    /// una tabla de aquí (ADR 0066, decisión D14).
    pub(super) fn ordenar_por(
        &mut self,
        slot_id: u32,
        column: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use norte_frontend::columns::{ColumnId, sort_column_id};
        if !self.huecos.contains_key(&slot_id) || self.oculto(slot_id) {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        // Las del ESQUEMA de este hueco, no solo las pintadas: el ajuste
        // (ADR 0124) puede haber cedido una, y ordenar por ella sigue
        // teniendo sentido — es lo mismo que el menú de orden ofrece.
        let configuradas = self
            .huecos
            .get(&slot_id)
            .map(|h| self.columnas_de(h.pane.dir()))
            .unwrap_or_default();
        let col = configuradas
            .iter()
            .find(|c| identidad_de_columna(c) == *column)
            .and_then(sort_column_id)
            .or_else(|| {
                // Un id que no está configurado pero que ES una columna
                // conocida sigue pudiendo ordenar: un menú de orden ofrece
                // más columnas de las que se pintan. Solo las FIJAS: un
                // `attr:` tiene que estar configurado (ADR 0144), o cualquier
                // texto del renderer acabaría en el orden y en la sesión.
                column
                    .parse::<ColumnId>()
                    .ok()
                    .filter(|id| matches!(id, ColumnId::Builtin(_)))
                    .as_ref()
                    .and_then(sort_column_id)
            });
        let Some(col) = col else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-column-not-sortable".to_owned(),
                },
                Vec::new(),
            );
        };
        self.ordenar_por_columna(slot_id, col)
    }

    /// Ordena un listado por una columna ya resuelta.
    ///
    /// Las dos puertas —el click en la cabecera y los `pane.sort-*` del
    /// catálogo— acaban AQUÍ, y por eso ordenan igual: la columna activa
    /// invierte y una nueva empieza ascendente, porque quien lo decide es
    /// `SortSpec::after_click` y no una tabla por superficie.
    pub(super) fn ordenar_por_columna(
        &mut self,
        slot_id: u32,
        col: norte_frontend::SortColumn,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(h) = self.huecos.get_mut(&slot_id) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        let spec = h.pane.sort().after_click(col);
        h.pane.set_sort(spec);
        // Re-ordenar mueve TODAS las filas —así que sube la generación y
        // viaja la ventana entera— Y la marca de orden de la cabecera. Sin lo
        // segundo, el listado se repintaba en el orden nuevo y el `▲` seguía
        // describiendo el anterior.
        let filas = self.parche_filas_de(slot_id);
        let cabeceras = self
            .huecos
            .get(&slot_id)
            .map(|h| self.cabeceras(slot_id, h));
        let mut salidas = vec![filas];
        if let Some(columns) = cabeceras {
            let cambio = ViewChange::Columns { slot_id, columns };
            salidas.push(self.parche(vec![cambio]));
        }
        (self.aplicada(), salidas)
    }

    /// Las columnas configuradas para el esquema de un hueco.
    ///
    /// Por ESQUEMA y no una vez al arrancar: `[ui.columns.schemes.sftp]` es
    /// configuración de verdad, y resolverla en el arranque la dejaba muerta
    /// en cuanto el panel navegaba a otro sitio.
    pub(super) fn columnas_de(&self, dir: &VPath) -> Vec<norte_frontend::columns::ColumnId> {
        self.columnas
            .layout_items_for(dir.scheme())
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    /// Los ids de atributo que pide el esquema de un directorio.
    ///
    /// Viajan en CADA listado: un provider solo entrega lo que se le pide, y
    /// una columna `attr:` que no se pide se queda en blanco para siempre.
    pub(super) fn attrs_de(&self, dir: &VPath) -> Vec<String> {
        self.columnas.attr_ids_for(dir.scheme())
    }

    /// Las cabeceras del listado, con la etiqueta ya traducida y la marca de
    /// orden puesta.
    ///
    /// `header_label` y `sort_column_id` son las MISMAS funciones que usa el
    /// TUI: cómo se llama una columna y si ordena no puede depender de quién
    /// pinta.
    /// Las columnas que se pintan en `hueco`, ajustadas para que sus
    /// nombres se lean: la MISMA regla que el terminal
    /// (`norte_frontend::columns::fitted_columns`), sobre el ancho en celdas
    /// que el reparto le da al hueco.
    ///
    /// El interior descuenta cuatro celdas —bordes, relleno y la casilla de
    /// marca— y lo que va delante del nombre en la fila es la insignia y,
    /// si los hay, los iconos. Un hueco que el reparto no coloca pinta todas
    /// sus columnas: no hay ancho con el que decidir, y ceder sin saber es
    /// quitar por quitar.
    pub(super) fn ajuste_de(
        &self,
        slot: u32,
        hueco: &Hueco,
    ) -> Vec<norte_frontend::columns::Fitted> {
        use norte_frontend::columns::fitted_columns;
        let esquema = hueco.pane.dir().scheme();
        // El catálogo de este pane: decide si se pinta la columna de permisos
        // que pone el listado (spec 2026-09-20). El MISMO que alimenta las
        // cabeceras, para que ancho y cabecera no discrepen.
        let catalogo = self.catalogo_de(hueco.pane.dir());
        let ancho = self
            .reparto
            .placements
            .iter()
            .find(|(s, _)| s.0 == slot)
            .map(|(_, r)| r.width);
        match ancho {
            Some(ancho) => {
                let delante: u16 = 2 + if hueco.pane.any_icon() { 3 } else { 0 };
                let quiere = hueco.pane.name_width_p80().saturating_add(delante);
                fitted_columns(
                    &self.columnas,
                    esquema,
                    ancho.saturating_sub(4),
                    quiere,
                    catalogo,
                )
            }
            None => fitted_columns(&self.columnas, esquema, u16::MAX / 2, 0, catalogo),
        }
    }

    pub(super) fn cabeceras(&self, slot: u32, hueco: &Hueco) -> Vec<ColumnHeader> {
        use norte_frontend::columns::{header_label_in, sort_column_id};
        let spec = hueco.pane.sort();
        let catalogo = self.catalogo_de(hueco.pane.dir());
        let esquema = hueco.pane.dir().scheme().to_owned();
        // La política de ancho de cada columna, UNA vez por cabecera: solo
        // la fija viaja (puente 64); `auto` y `flex` se pintan a lo que
        // midan, que es lo que esta ventana hacía con todas.
        let politicas = self.columnas.layout_items_for(&esquema);
        self.ajuste_de(slot, hueco)
            .iter()
            .map(|f| {
                let id = &f.id;
                let width =
                    politicas
                        .iter()
                        .find(|(c, _)| c == id)
                        .and_then(|(_, item)| match item.policy {
                            // El NOMBRE no lleva ancho fijo: lleva su SUELO, el
                            // del reparto compartido, para que el renderer no
                            // tenga que repetir el número.
                            _ if item.is_name => Some(norte_frontend::columns::NAME_MIN),
                            // Compacta, su ancho es el corto aunque la
                            // política diga otra cosa.
                            _ if f.compact => Some(norte_frontend::columns::COMPACT_WIDTH),
                            norte_frontend::columns::WidthPolicy::Fixed(n) => Some(n),
                            norte_frontend::columns::WidthPolicy::Auto
                            | norte_frontend::columns::WidthPolicy::Flex { .. } => None,
                        });
                // El estilo CONFIGURADO, no el de fábrica: `[ui.columns]`
                // deja poner rótulo propio, formato, alineación y ancho por
                // columna, y pidiendo `default_for_id` todo eso estaba muerto
                // en esta ventana mientras el terminal lo honraba.
                // El rótulo del manifiesto de una columna de plugin viene
                // dentro, por `apply_plugin_headers`: sin él la cabecera
                // enseñaba el id (`ORG.NORTE.SIZE-BAR/BAR` en vez de «Size»).
                let estilo = self
                    .columnas
                    .style_for_id(&esquema, id, catalogo)
                    .compacted(f.compact);
                let ordena = sort_column_id(id);
                // Prestada y no consumida: la misma respuesta dice además si
                // la cabecera es clicable. Con un atributo (ADR 0144) las dos
                // cosas salen de aquí sin código propio.
                let sort = ordena.as_ref().filter(|c| **c == spec.column).map(|_| {
                    match spec.dir {
                        norte_frontend::SortDir::Asc => "asc",
                        norte_frontend::SortDir::Desc => "desc",
                    }
                    .to_owned()
                });
                ColumnHeader {
                    id: identidad_de_columna(id),
                    // Con el idioma de ESTE host, no con el del proceso: los
                    // dos no tienen por qué coincidir, y media pantalla en
                    // cada idioma es peor que ninguna traducción.
                    label: clamp_display(header_label_in(id, &estilo, catalogo, self.lang)),
                    sort,
                    sortable: ordena.is_some(),
                    width,
                    align: match estilo.align {
                        norte_frontend::columns::Align::Left => "left",
                        norte_frontend::columns::Align::Right => "right",
                    }
                    .to_owned(),
                }
            })
            .collect()
    }

    /// Fija el ancho de una columna: el borde de su cabecera arrastrado en
    /// la ventana (puente 64, spec 2026-09-11 V2).
    ///
    /// Solo una columna que este hueco PINTA: el renderer no nombra columnas
    /// que no vio. El ancho se aplica en memoria y se escribe en
    /// `[ui.columns] spec.width` fuera del actor; y como es de la columna y
    /// no del hueco, vuelve la cabecera de TODOS los huecos, que es lo que
    /// el terminal verá también en su siguiente carga.
    pub(super) fn redimensionar_columna(
        &mut self,
        slot_id: u32,
        column: &str,
        cells: u16,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if !self.huecos.contains_key(&slot_id) || self.oculto(slot_id) {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        // Contra el AJUSTE, no contra lo configurado (ADR 0124): un arrastre
        // que llega después de que la columna cediera fijaría su ancho, y un
        // ancho fijo la saca de la escalera para siempre — el nombre volvería
        // a cortarse por un evento viejo.
        let pintada = self
            .huecos
            .get(&slot_id)
            .map(|h| self.ajuste_de(slot_id, h))
            .unwrap_or_default()
            .iter()
            .any(|f| identidad_de_columna(&f.id) == column);
        if !pintada {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let cells = self.columnas.apply_width(column, cells);
        self.persistir_ancho(column, cells, buzon);
        let cambios: Vec<ViewChange> = self
            .huecos
            .iter()
            .map(|(id, h)| ViewChange::Columns {
                slot_id: *id,
                columns: self.cabeceras(*id, h),
            })
            .collect();
        (self.aplicada(), vec![self.parche(cambios)])
    }

    /// Escribe el ancho fuera del actor, como el tema (`persistir_tema`):
    /// `persist_column_width` toma un lock entre procesos y hacerlo aquí
    /// congelaría la ventana. Solo el fallo vuelve por el buzón.
    fn persistir_ancho(&mut self, column: &str, cells: u16, buzon: &mpsc::Sender<Mensaje>) {
        let Some(dir) = self.dir_de_escritura() else {
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "host-no-config-dir",
            )));
            return;
        };
        let column = column.to_owned();
        let buzon = buzon.clone();
        tokio::task::spawn_blocking(move || {
            let clave = match norte_config::persist_column_width(&dir, &column, cells) {
                Ok(_) => None,
                Err(e) => Some(clave_de_io(&e)),
            };
            let _ = buzon.blocking_send(Mensaje::AnchoPersistido(clave));
        });
    }

    /// La proyección de UN listado.
    /// Los campos de CABECERA de un listado, derivados UNA vez.
    ///
    /// Los leen la foto ([`Self::browser`]) y el parche
    /// ([`Self::cabecera_de`]). Dos derivaciones del mismo hecho es de donde
    /// salió media auditoría de paridad, así que aquí hay una sola.
    pub(super) fn cabecera_de(&self, id: u32, hueco: &Hueco) -> crate::dto::ViewChange {
        // Con la MISMA reinterpretación que las filas: pintar la cabecera con
        // los bytes crudos mientras las filas van transcodificadas deja
        // `pane.names-encoding` a medias — el mojibake se queda arriba y el
        // lector no puede saber si el comando hizo algo (#57, #293).
        let (path, hostil) =
            norte_frontend::path_display_with(hueco.pane.dir(), hueco.pane.name_encoding());
        // Una pasada por las marcas para las tres cosas que las cuentan.
        let marcas = hueco.pane.marks_summary(crate::dto::MARK_RULER_SPANS);
        crate::dto::ViewChange::BrowserHeader {
            slot_id: id,
            path_display: clamp_display(path),
            path_hostile: hostil,
            // Las seis las REDACTA el crate compartido, que es donde el
            // terminal las coge también. Aquí estaban escritas a mano y ya
            // habían divergido: la de omitidas usaba otra clave, sin el ⚠ que
            // la hace leerse como aviso, y salía TAMBIÉN con cero — o sea que
            // anunciaba un listado incompleto que estaba completo, gastando la
            // única señal que hay para cuando de verdad falta algo.
            hidden_note: clamp_display(norte_frontend::notes::hidden(
                hueco.pane.hidden_count(),
                self.lang,
            )),
            skipped_note: clamp_display(norte_frontend::notes::skipped(
                hueco.pane.skipped(),
                self.lang,
            )),
            names_note: clamp_display(norte_frontend::notes::names_encoding(
                hueco.pane.name_encoding(),
                self.lang,
            )),
            filling_note: clamp_display(norte_frontend::notes::filling(
                hueco.pane.loading(),
                hueco.pane.entries().len(),
                self.lang,
            )),
            pruned_note: clamp_display(norte_frontend::notes::pruned_marks(
                hueco.pane.pruned_marks(),
                self.lang,
            )),
            marked_note: clamp_display(norte_frontend::notes::marked(
                hueco.pane.marks_len(),
                marcas.bytes,
                marcas.dirs,
                self.lang,
            )),
            footer: clamp_display(self.pie_con(hueco, &marcas)),
            path_segments: Self::migas_de(hueco),
            used_ratio: norte_frontend::space::used_ratio_for(
                hueco.pane.dir(),
                &self.volumenes_pie,
            ),
            marks: hueco.pane.marks_len() as u64,
            mark_ruler: marcas.ruler,
        }
    }

    /// Las migas de la ruta (puente 65): la raíz y un tramo por directorio,
    /// cada uno enmascarado por su cuenta — un tramo es un nombre de fichero
    /// y se trata como tal. La raíz lleva el esquema y, si la hay, la
    /// autoridad, con la misma forma que `path_display` (`⟨file⟩`,
    /// `⟨sftp⟩host`).
    fn migas_de(hueco: &Hueco) -> Vec<String> {
        let dir = hueco.pane.dir();
        let raiz = match dir.authority() {
            Some(a) => format!("⟨{}⟩{}", dir.scheme(), a),
            None => format!("⟨{}⟩", dir.scheme()),
        };
        std::iter::once(clamp_display(raiz))
            .chain(dir.segments().map(|s| {
                let (texto, _hostil) = norte_frontend::display_name(s);
                clamp_display(texto)
            }))
            .collect()
    }

    /// El pie de un listado (spec 2026-09-10), redactado por el crate
    /// compartido; vacío con `[ui] pane_footer` apagado.
    pub(super) fn pie_de(&self, hueco: &Hueco) -> String {
        if !self.config.common.ui_chrome.pane_footer() {
            return String::new();
        }
        self.pie_con(hueco, &hueco.pane.marks_summary(0))
    }

    /// El pie con las marcas ya resumidas: la cabecera lo pide junto con
    /// su propio resumen y no tiene por qué recorrer el listado otra vez.
    fn pie_con(&self, hueco: &Hueco, marcas: &norte_frontend::MarksSummary) -> String {
        if !self.config.common.ui_chrome.pane_footer() {
            return String::new();
        }
        let counts =
            norte_frontend::footer::counts(hueco.pane.entries(), hueco.pane.is_parent_row(0));
        let marked = norte_frontend::footer::Marked {
            n: hueco.pane.marks_len(),
            bytes: marcas.bytes,
            dirs: marcas.dirs,
        };
        let free = norte_frontend::space::free_for(hueco.pane.dir(), &self.volumenes_pie);
        norte_frontend::footer::pane_footer(counts, marked, free, self.lang)
    }

    /// Pide los volúmenes para el pie, si el pie está encendido y no hay ya
    /// una petición en vuelo. Se llama al aterrizar un listado: es cuando
    /// el panel puede haber cambiado de volumen.
    pub(super) fn pedir_volumenes_de_pie(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        if !self.config.common.ui_chrome.pane_footer() || self.pie_en_vuelo {
            return;
        }
        self.pie_en_vuelo = true;
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(PLAZO_PLUGINS, backend.volumes()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::VolumenesDePie(res))))
                .await;
        });
    }

    /// Los volúmenes del pie llegaron: se cachean y, si el pie de algún
    /// listado cambia con ellos, su cabecera viaja de nuevo. Un fallo deja
    /// la cache como estaba: el pie calla el espacio antes que inventarlo.
    pub(super) fn aplicar_volumenes_de_pie(
        &mut self,
        res: Result<Vec<norte_proto::methods::Volume>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        self.pie_en_vuelo = false;
        let vols = res.ok()?;
        let antes: Vec<(u32, String)> = self
            .huecos
            .iter()
            .map(|(id, h)| (*id, self.pie_de(h)))
            .collect();
        self.volumenes_pie = vols;
        let cambios: Vec<ViewChange> = antes
            .into_iter()
            .filter_map(|(id, viejo)| {
                let h = self.huecos.get(&id)?;
                (self.pie_de(h) != viejo).then(|| self.cabecera_de(id, h))
            })
            .collect();
        (!cambios.is_empty()).then(|| self.parche(cambios))
    }

    pub(super) fn browser(&self, id: u32, hueco: &Hueco) -> BrowserSlotView {
        // SIN `..`: la foto tiene que llevar lo mismo que el parche, y un
        // comodín aquí es exactamente cómo un campo nuevo de la cabecera se
        // queda fuera del primer pintado sin que nada se queje.
        let crate::dto::ViewChange::BrowserHeader {
            slot_id: _,
            path_display,
            path_hostile,
            hidden_note,
            skipped_note,
            names_note,
            filling_note,
            pruned_note,
            marked_note,
            footer,
            path_segments,
            used_ratio,
            marks,
            mark_ruler,
        } = self.cabecera_de(id, hueco)
        else {
            unreachable!("`cabecera_de` construye esa variante")
        };
        BrowserSlotView {
            slot_id: id,
            generation: hueco.pane.listing_epoch(),
            path_display,
            path_hostile,
            total_rows: Some(hueco.pane.entries().len() as u64),
            first_visible: hueco.primera_visible,
            rows: self.filas_de(id, hueco),
            icon_column: hueco.pane.any_icon(),
            cursor: (!hueco.pane.entries().is_empty())
                .then_some(RowKey(hueco.pane.cursor() as u64)),
            marks,
            mark_ruler,
            hidden_note,
            skipped_note,
            names_note,
            filling_note,
            pruned_note,
            marked_note,
            footer,
            path_segments,
            used_ratio,
            columns: self.cabeceras(id, hueco),
            state: hueco.estado.clone(),
            quick: hueco.pane.quick().map(|q| crate::dto::QuickView {
                query: clamp_display(q.query_display()),
                mode: match q.mode() {
                    norte_frontend::nav::Mode::Filter => "filter",
                    norte_frontend::nav::Mode::Jump => "jump",
                }
                .to_owned(),
                matches: q.visible().len() as u64,
            }),
        }
    }
}
