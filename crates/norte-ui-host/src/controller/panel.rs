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
    pub(super) fn mover_foco(&mut self, atras: bool) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
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
        let siguiente = self.siguiente_que_toma_teclas(actual, atras);
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
        let (inicio, largo) = match (derecha, abajo) {
            (Some((_, rb)), _) => (ra.x, ra.width + rb.width),
            (None, Some((_, rb))) => (ra.y, ra.height + rb.height),
            (None, None) => return (Self::obsoleta(StaleAction::Generation), Vec::new()),
        };
        if largo == 0 {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let frac = f32::from(cells.saturating_sub(inicio)) / f32::from(largo);
        let arbol = self.arbol.drag_border(SlotId(slot), frac, largo);
        if arbol == self.arbol {
            // El borde no se movió: ni foto ni parche. Un arrastre emite un
            // evento por píxel, y repintar la pantalla entera por cada uno
            // sería pagar una foto por temblor de mano.
            return (self.aplicada(), Vec::new());
        }
        self.aplicar_disposicion(arbol, backend, buzon)
    }

    /// El siguiente hueco del recorrido compartido que además TOMA TECLAS.
    ///
    /// Da como mucho una vuelta entera: si ninguno la toma —una pantalla que
    /// solo tenga hoja de atributos, que el reparto permite— devuelve `None`
    /// en vez de girar para siempre.
    pub(super) fn siguiente_que_toma_teclas(&self, desde: SlotId, atras: bool) -> Option<SlotId> {
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
            if kind_de(&self.arbol, siguiente)
                .and_then(|k| self.kinds.get(&k))
                .is_some_and(|d| d.takes_keys)
            {
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
        // Las del ESQUEMA de este hueco: es lo que se pintó, y por tanto lo
        // que el renderer pudo nombrar.
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
                // más columnas de las que se pintan.
                column
                    .parse::<ColumnId>()
                    .ok()
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
        let cabeceras = self.huecos.get(&slot_id).map(|h| self.cabeceras(h));
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
    pub(super) fn cabeceras(&self, hueco: &Hueco) -> Vec<ColumnHeader> {
        use norte_frontend::columns::{ColumnStyle, header_label, sort_column_id};
        let spec = hueco.pane.sort();
        let catalogo = self.catalogo_de(hueco.pane.dir());
        self.columnas_de(hueco.pane.dir())
            .iter()
            .map(|id| {
                let estilo = ColumnStyle::default_for_id(id, catalogo);
                let ordena = sort_column_id(id);
                let sort = ordena.filter(|c| *c == spec.column).map(|_| {
                    match spec.dir {
                        norte_frontend::SortDir::Asc => "asc",
                        norte_frontend::SortDir::Desc => "desc",
                    }
                    .to_owned()
                });
                ColumnHeader {
                    id: identidad_de_columna(id),
                    label: clamp_display(header_label(id, &estilo, catalogo)),
                    sort,
                    sortable: ordena.is_some(),
                }
            })
            .collect()
    }

    /// La proyección de UN listado.
    pub(super) fn browser(&self, id: u32, hueco: &Hueco) -> BrowserSlotView {
        // Con la MISMA reinterpretación que las filas: pintar la cabecera con
        // los bytes crudos mientras las filas van transcodificadas deja
        // `pane.names-encoding` a medias — el mojibake se queda arriba y el
        // lector no puede saber si el comando hizo algo (#57, #293).
        let (path, hostil) =
            norte_frontend::path_display_with(hueco.pane.dir(), hueco.pane.name_encoding());
        BrowserSlotView {
            slot_id: id,
            generation: hueco.pane.listing_epoch(),
            path_display: clamp_display(path),
            path_hostile: hostil,
            total_rows: Some(hueco.pane.entries().len() as u64),
            first_visible: hueco.primera_visible,
            rows: self.filas_de(hueco),
            cursor: (!hueco.pane.entries().is_empty())
                .then_some(RowKey(hueco.pane.cursor() as u64)),
            marks: hueco.pane.marks_len() as u64,
            hidden_note: match hueco.pane.hidden_count() {
                0 => String::new(),
                n => clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "status-hidden",
                    &[("n", &n.to_string())],
                )),
            },
            skipped_note: hueco.pane.skipped().map_or_else(String::new, |n| {
                clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "listing-skipped",
                    &[("n", &n.to_string())],
                ))
            }),
            columns: self.cabeceras(hueco),
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
