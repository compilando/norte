//! La disposición: huecos, columnas y el reparto de la pantalla.
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
    /// Abre el selector de disposiciones.
    ///
    /// Las del usuario ya vienen leídas del arranque: el selector pinta la
    /// FORMA de cada una, y leerlas al mover el cursor sería I/O en el bucle
    /// de eventos.
    pub(super) fn abrir_disposiciones(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.selector_disposicion = Some(norte_frontend::layout_picker::LayoutPicker::open(
            self.disposiciones.clone(),
        ));
        let cambio = ViewChange::Layouts {
            layouts: self.vista_disposiciones(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Abre el selector de COLUMNAS sobre el esquema del hueco enfocado.
    ///
    /// Sobre SU esquema y no sobre el conjunto por defecto: las columnas se
    /// configuran por esquema (`sftp` no enseña lo mismo que `file`), y
    /// abrirlo sobre otro sería editar una pantalla distinta de la que se
    /// está mirando.
    pub(super) fn abrir_columnas(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let hueco = self.hueco();
        let esquema = hueco.pane.dir().scheme().to_owned();
        let orden = hueco.pane.sort();
        let catalogo = self.catalogos.get(esquema.as_str()).cloned();
        self.selector_columnas = Some(
            norte_frontend::columns_picker::ColumnsPicker::open_with_catalog(
                &self.columnas,
                &esquema,
                orden,
                catalogo.as_ref(),
                // Sin catálogo de plugins todavía: el selector ofrece lo
                // CONFIGURADO más los attrs que el provider anuncia, y una
                // columna de plugin que nadie ha configurado no aparece
                // aún. Ofrecerlas pide cachear `plugin.list` en el host, que
                // hoy se pide por tanda de decoración y se tira.
                &[],
            ),
        );
        let cambio = ViewChange::ColumnsPicker {
            columns: self.vista_columnas(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// La proyección del selector de columnas.
    pub(super) fn vista_columnas(&self) -> Option<crate::dto::ColumnsPickerView> {
        let p = self.selector_columnas.as_ref()?;
        let esquema = p.scheme().to_owned();
        Some(crate::dto::ColumnsPickerView {
            // El título ya trae el ALCANCE, con la clave que la TUI usa y
            // su `$target`: inventarme una segunda colisionaba —Fluent se
            // queda con la PRIMERA definición— y la mía habría quedado
            // muerta con el catálogo diciendo que estaba.
            title: clamp_display(norte_i18n::ta_in(
                self.lang,
                "columns-picker-title",
                &[(
                    "target",
                    &if p.scheme_override() {
                        esquema.clone()
                    } else {
                        norte_i18n::t_in(self.lang, "columns-picker-target-default")
                    },
                )],
            )),
            rows: p
                .rows()
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    // La etiqueta de un `attr:` o un `plugin:` la da su
                    // catálogo, o sea texto de TERCERO. La de un builtin la
                    // da Fluent y es nuestra.
                    let (label, hostil) =
                        etiqueta_de_columna(r, &esquema, &self.columnas, self.lang);
                    crate::dto::ColumnsPickerRowView {
                        // Identidad: entera o vacía, jamás recortada — es lo
                        // que vuelve para encender, apagar y mover.
                        id: identidad_de_texto(&r.id),
                        label: clamp_display(label),
                        hostile: hostil,
                        enabled: r.enabled,
                        format: r.format.clone().unwrap_or_default(),
                        format_locked: r.format_locked,
                        // La primera fila es el NOMBRE, que por contrato del
                        // render va primero y no se apaga.
                        fixed: i == 0,
                    }
                })
                .collect(),
            cursor: p.cursor() as u64,
            // Esta ventana todavía NO escribe configuración: lo elegido vale
            // para ella y se pierde al cerrarla. Callarlo dejaría al usuario
            // creyendo que acaba de configurar norte.
            note: clamp_display(norte_i18n::t_in(self.lang, "columns-picker-session-only")),
            hint: clamp_display(self.pie_de_columnas()),
        })
    }

    /// El pie del selector de columnas, con los acordes que el keymap ATA.
    ///
    /// Se compone aquí y no en el renderer porque los verbos se pueden
    /// reatar, y una cadena traducida que nombra teclas concretas deja de ser
    /// cierta en cuanto alguien lo hace. Un verbo sin atar se cae del pie
    /// entero: anunciarlo sin tecla no ayuda a nadie.
    pub(super) fn pie_de_columnas(&self) -> String {
        let partes = [
            ("dialog.toggle-enabled", "columns-picker-hint-toggle"),
            ("dialog.move-up", "columns-picker-hint-move"),
            ("dialog.sort", "columns-picker-hint-sort"),
            ("dialog.cycle-format", "columns-picker-hint-format"),
            ("dialog.confirm", "columns-picker-hint-apply"),
            ("dialog.cancel", "columns-picker-hint-close"),
        ];
        partes
            .iter()
            .filter_map(|(cmd, clave)| {
                let acorde = self.acorde_de_dialogo(cmd);
                if acorde.is_empty() {
                    return None;
                }
                Some(format!("{acorde} {}", norte_i18n::t_in(self.lang, clave)))
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// Las teclas del selector de columnas.
    ///
    /// Las mismas que el overlay del TUI, por el mismo modelo: encender y
    /// apagar, subir y bajar la fila, elegir por qué se ordena y ciclar el
    /// formato de la que lo admita.
    pub(super) fn tecla_en_columnas(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.selector_columnas.is_none() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        // Por el resolutor COMPARTIDO (#287), no por teclas fijas: un preset
        // que reata `dialog.move-up` tiene que cambiar esta ventana igual que
        // cambia el TUI, que es para lo que existe el catálogo común. Y el pie
        // se pinta con los acordes que salen de aquí, no con un literal: un
        // pie que dice `Shift+↑/↓` sobre un código que escucha otra cosa es
        // una mentira que solo se descubre probando.
        let Some(verbo) = self.verbo_de_dialogo(k) else {
            return (self.aplicada(), Vec::new());
        };
        let Some(p) = self.selector_columnas.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match verbo.as_str() {
            "dialog.cancel" => self.selector_columnas = None,
            "dialog.down" => p.down(),
            "dialog.up" => p.up(),
            "dialog.move-down" => p.move_down(),
            "dialog.move-up" => p.move_up(),
            "dialog.toggle-enabled" => p.toggle(),
            "dialog.sort" => p.sort_current(),
            "dialog.cycle-format" => p.cycle_format(),
            "dialog.confirm" => return self.aplicar_columnas(backend, buzon),
            _ => return (self.aplicada(), Vec::new()),
        }
        let cambio = ViewChange::ColumnsPicker {
            columns: self.vista_columnas(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Aplica lo elegido A ESTA VENTANA.
    ///
    /// No escribe `norte.toml`: esta fase no muta nada del disco, y el
    /// selector lo DICE en su propia nota. Es el mismo trato que el selector
    /// de disposiciones.
    ///
    /// Si cambia el conjunto de columnas `attr:`/`plugin:` hay que RE-LISTAR:
    /// los valores de un attr solo llegan pidiéndolos en `fs.list`, así que
    /// una columna nueva sobre el listado viejo se quedaría en blanco —
    /// indistinguible de «este fichero no tiene ese atributo»— hasta el
    /// siguiente `cd`. La huella que lo decide es la COMPARTIDA
    /// (`pane_fingerprint`), no una cuenta de aquí.
    pub(super) fn aplicar_columnas(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(p) = self.selector_columnas.as_ref() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let elegido = p.finish();
        self.selector_columnas = None;
        let antes = self.huellas_de_columnas();
        self.columnas
            .apply_picked(elegido.scheme_target.as_deref(), &elegido.ids, elegido.sort);
        for (id, fmt) in &elegido.formats {
            self.columnas.apply_format(id, fmt);
        }
        for id in self.huecos.keys().copied().collect::<Vec<_>>() {
            if antes.get(&id) != self.huellas_de_columnas().get(&id) {
                self.re_listar(id, backend, buzon);
            }
        }
        let snap = self.snapshot();
        (
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// Vuelve a pedir el listado de un hueco, sin moverse de sitio.
    ///
    /// Lo pide el cambio de columnas: los valores de un `attr:` solo llegan
    /// si se piden en `fs.list`, así que una columna nueva sobre el listado
    /// viejo se quedaría en blanco — indistinguible de «este fichero no
    /// tiene ese atributo».
    pub(super) fn re_listar(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        // Un solo camino de recarga. Este tenía su propia copia y le faltaban
        // las dos cosas que hacen que recargar no se note: no anclaba el
        // cursor ni conservaba las marcas, así que cambiar de columnas
        // mandaba el cursor a la primera fila y borraba la selección.
        let _ = self.refrescar(slot, backend, buzon);
    }

    /// La huella de columnas de cada hueco: qué `attr:`/`plugin:` pinta.
    pub(super) fn huellas_de_columnas(&self) -> std::collections::BTreeMap<u32, Vec<String>> {
        self.huecos
            .iter()
            .map(|(id, h)| (*id, self.columnas.pane_fingerprint(h.pane.dir().scheme())))
            .collect()
    }

    /// La proyección del selector de disposiciones, con su vista previa.
    ///
    /// La miniatura la pinta el MISMO motor que reparte la pantalla de
    /// verdad, así que no puede mentir sobre lo que va a salir.
    pub(super) fn vista_disposiciones(&self) -> Option<crate::dto::LayoutPickerView> {
        /// Tamaño de la miniatura, en caracteres.
        const MINIATURA: (u16, u16) = (32, 12);

        let p = self.selector_disposicion.as_ref()?;
        let actual = p.current();
        // El diagnóstico del parser puede CITAR el fichero del usuario: entra
        // por la misma puerta que el resto, y con su bandera (#266) — lo que
        // se enmascara se dice.
        let diagnostico = actual.and_then(|r| r.problem.clone()).map_or_else(
            || (String::new(), false),
            |p| norte_frontend::display_name(p.as_bytes()),
        );
        Some(crate::dto::LayoutPickerView {
            title: clamp_display(norte_i18n::t_in(self.lang, "layout-picker-title")),
            rows: p
                .rows()
                .iter()
                .map(|r| {
                    // El nombre es un nombre de FICHERO: bytes (regla 1). Se
                    // pinta por la puerta compartida y NO viaja como clave —
                    // para elegir una fila se manda su índice.
                    let (pintable, hostil) =
                        norte_frontend::display::display_os_name(r.name.as_os_str());
                    crate::dto::LayoutRowView {
                        name: clamp_display(pintable),
                        hostile: hostil,
                        factory: r.factory,
                        shares_keymap_name: r.shares_keymap_name,
                        broken: r.tree.is_none(),
                    }
                })
                .collect(),
            cursor: p.cursor() as u64,
            preview: actual
                .and_then(|r| r.tree.as_ref())
                .map_or_else(Vec::new, |t| {
                    norte_frontend::layout_picker::preview(t, MINIATURA.0, MINIATURA.1, &self.kinds)
                }),
            problem: clamp_display(diagnostico.0),
            problem_hostile: diagnostico.1,
        })
    }

    /// Las teclas mientras el selector de disposiciones está abierto.
    pub(super) fn tecla_en_disposiciones(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.selector_disposicion.is_none() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        let verbo = self.verbo_de_dialogo(k);
        let Some(p) = self.selector_disposicion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match verbo.as_deref() {
            Some("dialog.cancel") => self.selector_disposicion = None,
            Some("dialog.down") => p.down(),
            Some("dialog.up") => p.up(),
            Some("dialog.confirm") => return self.aplicar_disposicion_elegida(backend, buzon),
            _ => return (self.aplicada(), Vec::new()),
        }
        let cambio = ViewChange::Layouts {
            layouts: self.vista_disposiciones(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Un click en una fila del selector: la elige Y la aplica.
    pub(super) fn elegir_disposicion(
        &mut self,
        row: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(p) = self.selector_disposicion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // Fuera de rango NO se recorta: el `while` de abajo para en la
        // última fila, así que un índice viejo aplicaba LA ÚLTIMA disposición
        // de la lista —la operación más invasiva del host— en vez de no hacer
        // nada. Las tres acciones hermanas que solo señalan ya lo hacen así.
        if row as usize >= p.rows().len() {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        // El selector compartido no tiene un `set_cursor`: se camina hasta
        // la fila, que para una lista de cinco a diez es lo mismo y no le
        // añade superficie a un modelo que ya está probado.
        while p.cursor() > row as usize {
            p.up();
        }
        while p.cursor() < row as usize && p.cursor() + 1 < p.rows().len() {
            p.down();
        }
        self.aplicar_disposicion_elegida(backend, buzon)
    }

    /// Aplica la disposición del cursor.
    ///
    /// Una que no parsea NO se aplica y lo dice: la fila ya lleva su motivo,
    /// y cambiar la pantalla por un fichero roto sería peor que no hacer
    /// nada. Se aplica para ESTA ventana y no se escribe en la
    /// configuración: escribir es mutar, y llega con la fase 5.
    pub(super) fn aplicar_disposicion_elegida(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(p) = self.selector_disposicion.as_ref() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let Some(arbol) = p.current().and_then(|r| r.tree.clone()) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-layout-broken".to_owned(),
                },
                Vec::new(),
            );
        };
        self.selector_disposicion = None;
        self.aplicar_disposicion(arbol, backend, buzon)
    }

    /// Cambia el ÁRBOL entero: otra disposición.
    ///
    /// Manda una FOTO y no un parche: cambia el reparto, qué huecos hay y qué
    /// hay dentro de cada uno. Los listados que la disposición nueva coloca y
    /// no existían arrancan en el directorio del que ya estaba, que es lo
    /// menos sorprendente: cambiar de forma de pantalla no es irse a otro
    /// sitio.
    pub(super) fn aplicar_disposicion(
        &mut self,
        arbol: Node,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.aplicar_disposicion_con(arbol, None, backend, buzon)
    }

    /// Como [`Self::aplicar_disposicion`], diciendo qué hueco QUEDA activo.
    ///
    /// `None` = lo decide la reconciliación, que es lo que hace falta cuando
    /// el árbol viene de fuera. `Some` es para quien acaba de crear un hueco
    /// y quiere el foco ahí: en dos pasos serían dos fotos, y la primera
    /// enseñaría el foco donde ya no está.
    pub(super) fn aplicar_disposicion_con(
        &mut self,
        arbol: Node,
        activo: Option<SlotId>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.poner_arbol(arbol, activo);
        self.despertar_visibles(backend, buzon);
        if self.hueco_de_sitios().is_some() {
            self.sembrar_sitios();
            self.pedir_sitios(backend, buzon);
        }
        // El árbol cambió: a la sesión AHORA, sin esperar al tic. Un panel
        // abierto o una plantilla elegida es justo lo que el lector espera
        // encontrar al volver, y un cierre que no llegue a tiempo no debe
        // perderlo.
        self.empujar_sesion(backend, buzon);
        let snap = self.snapshot();
        (
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// Pone `arbol` como el árbol de la pantalla y siembra los huecos que
    /// estrena, SIN pedir ningún listado ni tocar la sesión.
    ///
    /// Es la mitad sin I/O de [`Self::aplicar_disposicion_con`], y lo que el
    /// arranque usa para la disposición que la sesión guardó: ahí los
    /// listados se piden después, una vez la sesión haya dicho dónde estaba
    /// cada uno, y despertarlos aquí pediría el directorio del arranque para
    /// Recalcula el reparto con el árbol y el registro de AHORA.
    ///
    /// Declarar un kind cambia mínimos y enfocabilidad, y hasta la fase 3 el
    /// reparto solo se rehacía al poner un árbol o al cambiar el viewport: los
    /// paneles que aportan los plugins llegan DESPUÉS de la primera foto, así
    /// que el hueco ya colocado se quedaba con el reparto que lo desconocía.
    pub(super) fn rehacer_reparto(&mut self) {
        self.reparto = resolve(rect(self.viewport), &self.arbol, &self.kinds);
    }

    /// tirarlo un instante después.
    pub(super) fn poner_arbol(&mut self, arbol: Node, activo: Option<SlotId>) {
        let dir = self.hueco().pane.dir().clone();
        // Un hueco que se estrena nace como los del arranque: con la
        // ocultación de la configuración puesta.
        let ocultos = self.config.common.ui_show_hidden.unwrap_or(true);
        self.arbol = arbol;
        self.reparto = resolve(rect(self.viewport), &self.arbol, &self.kinds);
        // Desde el ÁRBOL, no desde el reparto. `placements` y `hidden`
        // PARTICIONAN el árbol, así que sembrar desde `placements` borra el
        // hueco de un listado que el reparto no coloca —un `Tabs` cuyo activo
        // es otro kind, o un split todo-ponderado que no cabe—, y con él
        // puede irse el ÚLTIMO: `huecos` queda vacío y la siguiente tecla
        // muere en el `expect` de `hueco()`, dentro de la task del actor.
        // `validate` garantiza que el árbol TENGA un listado, no que el
        // reparto lo coloque, así que la garantía hay que tomarla del árbol.
        let nuevos: Vec<u32> = self
            .arbol
            .slot_ids()
            .into_iter()
            .map(|SlotId(id)| id)
            .filter(|id| es_listado(&self.arbol, SlotId(*id), &self.kinds))
            .collect();
        self.huecos.retain(|id, _| nuevos.contains(id));
        for id in nuevos {
            if let std::collections::btree_map::Entry::Vacant(hueco) = self.huecos.entry(id) {
                hueco.insert(Hueco::vacio(
                    dir.clone(),
                    ocultos,
                    self.columnas.sort_for(dir.scheme()),
                    self.config.common.ui_parent_entry.unwrap_or(true),
                ));
            }
        }
        match activo {
            Some(id) => self.roles.set(RoleId::Active, id),
            None => self.roles.clear(RoleId::Active),
        }
        self.reconcilia_roles();
    }

    /// Cambia el tamaño del hueco con el FOCO, no del listado activo.
    ///
    /// Del foco a propósito: la única forma de ensanchar la barra lateral es
    /// tenerla enfocada y crecer, y `activo()` —que se salta lo que no es un
    /// listado— habría redimensionado el panel de al lado.
    ///
    /// Redimensionar es una decisión sobre EL ÁRBOL, así que se guarda en él:
    /// el reparto se recalcula desde el árbol nuevo, y no al revés.
    pub(super) fn redimensionar(
        &mut self,
        delta: i64,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let paso = i16::try_from(delta.clamp(i64::from(i16::MIN), i64::from(i16::MAX)))
            .unwrap_or(if delta < 0 { -1 } else { 1 });
        let nuevo = self.arbol.resize(SlotId(self.enfocado()), paso);
        self.aplicar_arbol(nuevo, backend, buzon)
    }

    /// Iguala el peso de los hermanos del hueco con el foco.
    pub(super) fn igualar(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let nuevo = self.arbol.equalize(SlotId(self.enfocado()));
        self.aplicar_arbol(nuevo, backend, buzon)
    }

    /// Sustituye el árbol y vuelve a repartir.
    ///
    /// Si el reparto no cambia —el hueco estaba en su tope, o no tiene
    /// hermanos con los que repartir— NO se manda nada: un parche que no
    /// cambia nada obliga a repintar para nada, y la tecla ya dijo lo suyo
    /// sin moverse.
    pub(super) fn aplicar_arbol(
        &mut self,
        nuevo: Node,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let antes = self.reparto.clone();
        self.arbol = nuevo;
        self.reparto = resolve(rect(self.viewport), &self.arbol, &self.kinds);
        if self.reparto.placements == antes.placements {
            return (self.aplicada(), Vec::new());
        }
        self.reconcilia_roles();
        // El reparto cambió: lo que acaba de salir de `hidden` no tiene
        // listado y nadie más se lo va a pedir.
        self.despertar_visibles(backend, buzon);
        // Y a la sesión ahora: un tamaño es una decisión sobre el árbol.
        self.empujar_sesion(backend, buzon);
        let cambio = ViewChange::Layout(self.disposicion());
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// El reparto de ESTE tamaño, con los papeles puestos.
    ///
    /// Sale del mismo `resolve` que usa el TUI: el renderer recibe rectángulos
    /// en celdas y no una lista de huecos que tenga que colocar él, que sería
    /// una segunda regla de disposición escrita en otro lenguaje (decisión
    /// D14).
    pub(super) fn disposicion(&self) -> LayoutView {
        let activo = self.roles.get(RoleId::Active);
        let destino = self.roles.get(RoleId::Target);
        let placements = self
            .reparto
            .placements
            .iter()
            .map(|(slot, r)| {
                let SlotId(id) = *slot;
                let role = if Some(*slot) == activo {
                    Some(SlotRole::Active)
                } else if Some(*slot) == destino {
                    Some(SlotRole::Target)
                } else {
                    None
                };
                let focus_index = self
                    .reparto
                    .focus_order
                    .iter()
                    .position(|s| s == slot)
                    .unwrap_or(usize::MAX);
                SlotPlacement {
                    slot_id: id,
                    x: r.x,
                    y: r.y,
                    width: r.width,
                    height: r.height,
                    role,
                    focus_index: u32::try_from(focus_index).unwrap_or(u32::MAX),
                }
            })
            .collect();
        LayoutView {
            cells: self.viewport,
            tabs: self.grupos_de_pestanas(),
            placements,
            // Se cuentan los huecos que PUEDEN ser destino, no los colocados:
            // un reparto lleva también la barra de estado y la franja de
            // tareas, y contarlas haría que el «tres o más» se cumpliera
            // siempre — que es como no tener la regla.
            mark_target: norte_frontend::layout::target_worth_marking(
                self.reparto
                    .placements
                    .iter()
                    .filter(|(slot, _)| {
                        self.arbol
                            .kind_of(*slot)
                            .is_some_and(|k| self.kinds.holds_role(k, RoleId::Target))
                    })
                    .count(),
            ),
        }
    }

    /// Los grupos de PESTAÑAS que hay en pantalla.
    ///
    /// Uno por hueco colocado que viva dentro de una `Tabs`: las inactivas no
    /// se colocan —el repartidor compartido no las pinta— y sin esto la
    /// ventana enseñaría la de delante sin decir que hay otras dos abiertas.
    pub(super) fn grupos_de_pestanas(&self) -> Vec<crate::dto::TabGroupView> {
        let mut fuera = Vec::new();
        for (slot, _) in &self.reparto.placements {
            let Some((huecos, activo)) = self.arbol.tabs_of(*slot) else {
                continue;
            };
            if huecos.len() < 2 {
                // Un grupo de UNA no es un grupo: pintarle una barra de
                // pestañas es cromo que no dice nada y que roba una fila.
                continue;
            }
            let SlotId(id) = *slot;
            let panels = huecos
                .iter()
                .all(|t| kind_de(&self.arbol, *t).is_some_and(|k| k.as_str() != "browser"));
            fuera.push(crate::dto::TabGroupView {
                slot_id: id,
                tabs: huecos.iter().map(|t| self.pestana(*t)).collect(),
                active: activo as u64,
                panels,
            });
        }
        fuera
    }

    /// Una pestaña: qué hueco lleva dentro y cómo se llama.
    ///
    /// El rótulo es el nombre del DIRECTORIO de su listado —no la ruta
    /// entera, que no cabe— enmascarado como cualquier otro nombre: uno
    /// hostil dentro de una pestaña es tan hostil como dentro de un listado.
    /// Un directorio raíz no tiene nombre: se cae al esquema, que es lo único
    /// que lo distingue de otro.
    pub(super) fn pestana(&self, slot: SlotId) -> crate::dto::TabView {
        let SlotId(id) = slot;
        let (titulo, hostil) = if let Some(h) = self.huecos.get(&id) {
            let dir = h.pane.dir();
            match dir.file_name() {
                Some(seg) => norte_frontend::display_name(seg.as_bytes()),
                // Una raíz no tiene nombre: se cae al esquema, que es lo
                // único que la distingue de otra.
                None => (dir.scheme().to_owned(), false),
            }
        } else {
            // Lo que no es un listado se nombra como su botón de la barra de
            // paneles («Visor», «Detalles»): desde que los paneles de un
            // borde se agrupan en pestañas (fase F) este rótulo se LEE, y el
            // id del kind no es un nombre. El kind sale de un fichero de
            // disposición, así que el resultado pasa por la misma puerta.
            let kind = kind_de(&self.arbol, slot)
                .map_or_else(|| "unknown".to_owned(), |k| k.as_str().to_owned());
            let nombre =
                norte_frontend::panelbar::label_in(self.lang, &kind, &format!("layout.{kind}"));
            norte_frontend::display_name(nombre.as_bytes())
        };
        crate::dto::TabView {
            slot_id: id,
            title: clamp_display(titulo),
            title_hostile: hostil,
        }
    }
}
