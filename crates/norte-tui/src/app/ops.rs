//! Los modales que nacen de las marcas del listado: la cola de colisiones y
//! de aprobaciones, copiar y mover, borrar, ordenar por esquema o por tecla y
//! la hoja de propiedades con su hidratación.

use super::App;
use super::modal::{Modal, PromptKind, TransferKind};
use norte_proto::VPath;

impl App {
    /// Si no hay modal abierto, abre el diálogo de la siguiente colisión
    /// encolada. Llamar tras cerrar un modal y en cada tick.
    pub fn open_next_collision(&mut self) {
        if self.modal.is_none()
            && let Some(retry) = self.pending_collisions.pop_front()
        {
            self.modal = Some(Modal::Collision { retry });
            self.abandon_shortcut_capture();
            return;
        }
        // Los informes, detrás: una colisión PREGUNTA algo con una copia
        // esperando, un informe solo cuenta lo que ya pasó.
        if self.modal.is_none()
            && let Some((kind, lines)) = self.pending_reports.pop_front()
        {
            self.modal = Some(Modal::Report { kind, lines });
            self.abandon_shortcut_capture();
        }
    }

    /// A modal is taking the keyboard, so the shortcut editor stops ASKING for
    /// a blind keypress (K3c).
    ///
    /// Capture mode paints "press the new key" and the reader is primed to
    /// press anything at all. A modal that arrives on its own — a policy
    /// approval off the bus, a collision at the end of a copy — takes the keys
    /// and is painted on top, so that next key answers a question the reader
    /// did not know was being asked, and on `Modal::ApproveAgentOp` the letter
    /// `y` approves an agent operation. The key still reaches the modal (that
    /// part is the modal's right); what norte must not do is keep inviting it.
    ///
    /// The editor itself SURVIVES: the reader gets their list back after
    /// answering, unless the modal arm of the key chain retires it
    /// (`main::close_stale_overlays`).
    fn abandon_shortcut_capture(&mut self) {
        if let Some(sc) = &mut self.shortcuts {
            sc.cancel_capture();
        }
    }

    /// Si no hay modal abierto, abre el siguiente diálogo pendiente:
    /// aprobaciones de policy PRIMERO (tienen TTL en el daemon), colisiones
    /// después. Llamar tras cerrar un modal y al llegar una aprobación.
    pub fn open_next_pending(&mut self) {
        if self.modal.is_none()
            && let Some(req) = self.pending_approvals.pop_front()
        {
            self.modal = Some(Modal::ApproveAgentOp { req });
            self.abandon_shortcut_capture();
            return;
        }
        self.open_next_collision();
    }

    /// Cancela `Modal::MarkPattern` SIN marcar nada — el equivalente de un
    /// `DialogOutcome::Cancelled` para ESTE modal de texto libre (#103 T9),
    /// que no pasa por el ALLOWLIST de [`super::dialog_action`] y por tanto no
    /// tiene su propio Esc en `on_dialog_key`. Abre la siguiente pendiente
    /// en cola, misma disciplina que cerrar cualquier otro modal (jamás
    /// pisar una aprobación/colisión que llegó mientras este estaba
    /// abierto).
    ///
    /// Deliberadamente NO genérico sobre `self.modal` (review rust MAJOR
    /// M1): para `Modal::ApproveAgentOp` cerrar sin más deja al agente sin
    /// respuesta hasta el TTL del daemon — el cierre real de ESE modal
    /// (`on_dialog_key`, `DialogOutcome::Cancelled`) empareja el cierre con
    /// un `policy.decide(approve: false)` async, algo que un método
    /// síncrono no puede hacer. El guard estructural (`debug_assert!`) hace
    /// del allowlist "solo modales de texto libre" algo que el compilador
    /// de tests, no la disciplina del caller, hace cumplir.
    pub fn cancel_mark_pattern(&mut self) {
        self.cancel_prompt(PromptKind::MarkPattern);
    }

    /// Abre la confirmación de una copia o un movimiento `from` → `to`.
    ///
    /// **Fuente ÚNICA de qué somete una transferencia**, la tecla (F5/F6) y
    /// el arrastre por igual. No es estilo: un drop es una mutación, y una
    /// segunda ruta —aunque hoy naciera idéntica— se quedaría sin la
    /// confirmación, sin el modal de colisión, sin la entrada de journal o
    /// sin el undo en cuanto una de las dos cambiara. Por eso el drop no
    /// construye ningún modal: pide el mismo que pediría `pane.copy`.
    /// Gemela de `transfer_modal` en la GUI.
    ///
    /// `promoted` es la única diferencia entre las dos entradas, y solo dice
    /// SOBRE QUÉ actúa: `None` = las marcas del pane (o el cursor si no hay
    /// ninguna — `marked_paths`, la fuente única de siempre); `Some(idx)` =
    /// esa fila sola, porque el gesto se promovió desde una fila SIN marcar
    /// y las marcas del pane —si las hay— son otra cosa que el usuario no
    /// está arrastrando.
    ///
    /// Con UN solo ítem el nombre de destino es EDITABLE (#105); el lote
    /// multi sigue en el confirm de lista (no hay un nombre único). No-op si
    /// no hay nada que transferir: jamás un diálogo sobre un lote vacío.
    pub fn open_transfer(
        &mut self,
        kind: TransferKind,
        from: usize,
        to: usize,
        promoted: Option<usize>,
    ) {
        let to_dir = self.panes[to].dir().clone();
        self.open_transfer_to_dir(kind, from, to_dir, promoted);
    }

    /// Como [`Self::open_transfer`] pero contra un DIRECTORIO, no contra un
    /// panel.
    ///
    /// Existe porque no siempre hay «el otro panel»: con un solo listado
    /// —`simple`— el destino lo teclea el lector ([`Self::open_transfer_dest`]),
    /// y esa transferencia tiene que entrar por la MISMA puerta que F5, o se
    /// queda sin confirmación, sin colisión y sin undo.
    pub fn open_transfer_to_dir(
        &mut self,
        kind: TransferKind,
        from: usize,
        to_dir: VPath,
        promoted: Option<usize>,
    ) {
        let items: Vec<VPath> = match promoted {
            Some(idx) => self.panes[from]
                .entries()
                .get(idx)
                .map(|e| vec![e.path.clone()])
                .unwrap_or_default(),
            None => self.panes[from].marked_paths(),
        };
        match items.as_slice() {
            [] => {}
            [one] => {
                // `from_marks` decide si el envío CONSUME la selección
                // ([`Self::transfer_name_submitted`]). Un arrastre promovido
                // jamás la consume: la promoción cambia lo que el gesto
                // HACE, no lo que está seleccionado — y lo marcado puede ser
                // otra cosa que el usuario no ha soltado.
                let from_marks = promoted.is_none() && self.panes[from].marks_len() > 0;
                self.open_transfer_name_with(kind, from, one.clone(), to_dir, from_marks);
            }
            _ => {
                // El total SOLO si TODOS los ítems traen tamaño (#149): un
                // directorio no lo trae en el listado, y sumar lo que sí
                // avisaría con un número menor que el real — peor que callar.
                self.pending_dest_check = Some(crate::app::DestCheck {
                    to: to_dir.clone(),
                    total: self.transfer_total(from, &items),
                });
                self.modal = Some(Modal::ConfirmTransfer {
                    kind,
                    items,
                    to: to_dir,
                    space: None,
                    confine: None,
                });
            }
        }
    }

    /// Los bytes que una transferencia va a escribir, o `None` si alguno de
    /// los ítems no lo dice (#149).
    ///
    /// Todo o nada, y a propósito: un directorio no trae tamaño en el listado
    /// y un listado perezoso puede no traerlo ni para un fichero. Sumar solo
    /// lo conocido daría un total MENOR que el real, y avisar con él es avisar
    /// de menos — que sobre «no cabe» es exactamente el error que no se puede
    /// cometer.
    fn transfer_total(&self, pane: usize, items: &[VPath]) -> Option<u64> {
        // La REGLA —todo o nada— vive en el crate compartido: la ventana hace
        // la misma pregunta en el mismo diálogo, y un total calculado con otro
        // criterio es una alarma que sale en un frontend y no en el otro.
        norte_frontend::space::total_to_write(self.panes[pane].entries(), items)
    }

    /// Abre el modal de borrado (F8, #103 T10) sobre las MARCAS del pane con
    /// foco (o el cursor si no hay ninguna). `permanent` lo decide el caller:
    /// es `shift+F8`, o la ausencia de papelera en el provider — que se
    /// sondea UNA vez por lote, no una por ítem (serían N round-trips de red
    /// para responder siempre lo mismo). No-op si no hay nada que borrar.
    pub fn open_delete_modal(&mut self, permanent: bool) {
        let items = self.focused().marked_paths();
        if items.is_empty() {
            return;
        }
        self.modal = Some(Modal::ConfirmDelete { items, permanent });
    }

    /// Las marcas las CONSUME la operación (mc/Total Commander): se limpian
    /// al ENVIAR el lote, no al completarse, para que jamás exista una
    /// selección a medio consumir cuyo significado dependa de qué task
    /// terminó (#103).
    pub fn consume_marks(&mut self) {
        self.focused_mut().clear_marks();
    }

    /// Aplica a `pane` el orden de SU scheme según la config (#108 b4):
    /// llamado al aterrizar un cd (el scheme puede haber cambiado) y al
    /// arrancar. `set_sort` es no-op si el spec no cambia.
    pub fn apply_scheme_sort(&mut self, pane: usize) {
        let scheme = self.panes[pane].dir().scheme().to_owned();
        let spec = self.columns.sort_for(&scheme);
        self.panes[pane].set_sort(spec);
    }

    /// Ordena el pane con el FOCO por `col`, con la semántica del click de
    /// cabecera (#138).
    ///
    /// La columna activa invierte su dirección; una nueva ordena ascendente.
    /// `dirs_first` no lo toca ninguna tecla de orden: es una preferencia del
    /// usuario, no un criterio de columna — se cambia en el diálogo de
    /// columnas, que es donde vive.
    ///
    /// Solo el pane enfocado: el orden es de UN listado, igual que el cursor.
    pub fn sort_focused_by(&mut self, col: norte_frontend::SortColumn) {
        let spec = self.focused().sort().after_click(col);
        self.focused_mut().set_sort(spec);
    }

    /// Abre las propiedades de la entrada bajo el cursor (#139).
    ///
    /// Devuelve la ruta cuyo tamaño hay que contar, si es una carpeta: el
    /// diálogo no habla con el backend —esto es `App`, no el run loop— así que
    /// dice qué hace falta y quien puede lo pide.
    pub fn open_properties(&mut self) -> Option<VPath> {
        let entry = self.focused().selected()?.clone();
        let count = (entry.kind == norte_proto::EntryKind::Dir).then(|| entry.path.clone());
        self.modal = Some(Modal::Properties {
            entry: Box::new(entry),
            size_task: None,
            size: None,
        });
        count
    }

    /// Mete en el diálogo la entrada RECIÉN pedida al backend.
    ///
    /// Un listado perezoso (#52) no trae ni tamaño ni fecha, y de una carpeta
    /// no los trae NUNCA: sin esto, las propiedades de un directorio decían
    /// «lo desconoce el backend» de algo que un `stat` sabe perfectamente.
    /// Conserva el recuento —es de otra pregunta— y no pisa el diálogo si el
    /// humano ya lo cerró.
    pub fn properties_hydrate(&mut self, fresca: norte_proto::Entry) {
        if let Some(Modal::Properties { entry, .. }) = &mut self.modal
            && entry.path == fresca.path
        {
            **entry = fresca;
        }
    }

    /// Ata al diálogo de propiedades el recuento que se acaba de lanzar.
    pub fn properties_counting(&mut self, task: norte_proto::TaskId) {
        if let Some(Modal::Properties { size_task, .. }) = &mut self.modal {
            *size_task = Some(task);
        }
    }

    /// Mete en el diálogo el resultado de SU recuento (#139).
    ///
    /// Por `task_id` y no «el último que llegue»: entre abrir el diálogo y que
    /// termine la cuenta cabe otra cuenta —la que el humano lanzó a mano sobre
    /// una selección—, y enseñar ese número aquí sería contestar otra pregunta.
    ///
    /// Devuelve `true` si era el suyo.
    pub fn properties_sized(
        &mut self,
        task: norte_proto::TaskId,
        bytes: u64,
        entries: u64,
    ) -> bool {
        let Some(Modal::Properties {
            size_task, size, ..
        }) = &mut self.modal
        else {
            return false;
        };
        if *size_task != Some(task) {
            return false;
        }
        *size = Some((bytes, entries));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::app::dialog_action;
    use crate::app::pane::Pane;
    use crate::app::testutil::*;
    use norte_proto::{Entry, EntryKind, VPath};

    /// #139: las propiedades salen del LISTADO, y sobre una carpeta piden lo
    /// único que el listado no sabe.
    #[test]
    fn las_propiedades_de_una_carpeta_piden_contarla() {
        let mut app = app_with_entries(&["a.txt"]);
        // Sobre un fichero no hay nada que contar: su tamaño ya está.
        assert!(app.open_properties().is_none());
        assert!(matches!(app.modal, Some(Modal::Properties { .. })));
    }

    /// El resultado de un recuento va al diálogo que lo pidió, y a NINGÚN
    /// otro: entre abrir el diálogo y que termine la cuenta cabe otra cuenta
    /// —la que el humano lanzó sobre una selección— y enseñar ese número aquí
    /// sería contestar otra pregunta.
    #[test]
    fn el_recuento_ajeno_no_entra_en_el_dialogo() {
        use norte_proto::TaskId;

        let mut app = app_with_entries(&["a.txt"]);
        app.open_properties();
        let mine = TaskId::new(7);
        app.properties_counting(mine);
        assert!(
            !app.properties_sized(TaskId::new(8), 1, 1),
            "el de otro no entra"
        );
        assert!(app.properties_sized(mine, 4096, 12), "el mío sí");
        let Some(Modal::Properties { size, .. }) = &app.modal else {
            panic!("sigue abierto")
        };
        assert_eq!(*size, Some((4096, 12)));
    }

    /// Sin diálogo abierto, un recuento no tiene dónde entrar y lo dice: es lo
    /// que hace que el run loop mande el número a la barra de estado.
    #[test]
    fn sin_dialogo_el_recuento_no_encuentra_donde_ir() {
        let mut app = app_with_entries(&["a.txt"]);
        assert!(!app.properties_sized(norte_proto::TaskId::new(1), 10, 1));
    }

    /// #138: la tecla de orden hace lo mismo que un click en la cabecera —
    /// invierte si ya está activa, ordena ascendente si es nueva— y SOLO sobre
    /// el panel con el foco: el orden es de un listado, como el cursor.
    #[test]
    fn una_tecla_de_orden_solo_toca_el_panel_con_el_foco() {
        use norte_frontend::{SortColumn, SortDir};

        let mut app = app_dos_panes();
        let other = app.panes[1].sort();
        app.sort_focused_by(SortColumn::Size);
        assert_eq!(app.focused().sort().column, SortColumn::Size);
        assert_eq!(
            app.focused().sort().dir,
            SortDir::Asc,
            "una nueva, ascendente"
        );
        assert_eq!(app.panes[1].sort(), other, "el otro panel no se entera");

        app.sort_focused_by(SortColumn::Size);
        assert_eq!(
            app.focused().sort().dir,
            SortDir::Desc,
            "la misma otra vez invierte"
        );
        app.sort_focused_by(SortColumn::Extension);
        assert_eq!(app.focused().sort().column, SortColumn::Extension);
        assert_eq!(app.focused().sort().dir, SortDir::Asc);
    }

    /// Y `dirs_first` no lo toca ninguna tecla de orden: es una preferencia,
    /// no un criterio de columna.
    #[test]
    fn una_tecla_de_orden_no_toca_los_directorios_primero() {
        use norte_frontend::SortColumn;

        let mut app = app_dos_panes();
        let mut spec = app.focused().sort();
        spec.dirs_first = false;
        app.focused_mut().set_sort(spec);
        app.sort_focused_by(SortColumn::Mtime);
        assert!(!app.focused().sort().dirs_first);
    }

    /// #108 b4: `apply_scheme_sort` aplica el orden de la config al pane
    /// según su scheme — el hook de cd y el arranque pasan por aquí.
    #[test]
    fn apply_scheme_sort_ordena_por_la_config() {
        use norte_frontend::columns::ColumnsSettings;
        let dir = VPath::parse("mem:///").unwrap();
        let mk = |n: &str, size: Option<u64>| {
            let mut e = e(&format!("mem:///{n}"), EntryKind::File);
            e.size = size;
            e
        };
        let mut app = App::new(
            Pane::new(dir.clone(), vec![mk("a", Some(3)), mk("b", Some(1))]),
            Pane::new(dir, Vec::new()),
        );
        let cfg = norte_config::ColumnsConfig {
            default_columns: None,
            sort: Some(norte_config::SortChoice {
                column: norte_config::SortColumnKey::Size,
                descending: false,
                dirs_first: true,
            }),
            schemes: std::collections::BTreeMap::new(),
            ..Default::default()
        };
        app.columns = ColumnsSettings::resolve(&cfg);
        app.apply_scheme_sort(0);
        let order: Vec<_> = app.panes[0]
            .entries()
            .iter()
            .map(|e| e.path.clone())
            .collect();
        assert_eq!(
            order,
            vec![
                VPath::parse("mem:///b").unwrap(),
                VPath::parse("mem:///a").unwrap()
            ],
            "size asc desde la config"
        );
    }

    /// #105 review MAJOR-1: el submit de UN ítem que vino de la MARCA la
    /// CONSUME (doctrina mc/TC del lote); un rename (cursor) jamás toca
    /// las marcas, y Esc tampoco.
    #[test]
    fn el_submit_de_un_item_consume_la_marca_y_el_rename_no() {
        let dir = VPath::parse("mem:///").unwrap();
        let mk = |n: &str| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(norte_proto::Segment::new(n.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        };
        let mut app = App::new(
            Pane::new(dir.clone(), vec![mk("a"), mk("b")]),
            Pane::new(VPath::parse("mem:///dst").unwrap(), Vec::new()),
        );
        app.focused_mut().toggle_mark(); // marca "a"
        app.focused_mut().move_down(1); // cursor en "b"
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        let (_, from, _) = app.transfer_name_confirm().expect("válido");
        assert_eq!(
            from,
            VPath::parse("mem:///a").unwrap(),
            "la MARCA, no el cursor"
        );
        app.transfer_name_submitted();
        assert_eq!(app.focused().marks_len(), 0, "el envío consume la marca");

        // Esc no consume.
        app.focused_mut().toggle_mark(); // marca "b" (cursor sigue ahí)
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        app.cancel_transfer_name();
        assert_eq!(app.focused().marks_len(), 1, "cancelar conserva la marca");

        // Rename (cursor) no toca marcas ajenas.
        app.open_rename();
        app.transfer_name_push('2');
        assert!(app.transfer_name_confirm().is_some());
        app.transfer_name_submitted();
        assert_eq!(app.focused().marks_len(), 1, "el rename no consume marcas");
    }

    /// #103 T10: F5/F6 construyen el modal desde TODAS las marcas, y el
    /// destino es el DIRECTORIO del otro pane (con varios ítems no hay un
    /// nombre único que editar — eso es #105).
    #[test]
    fn copy_builds_the_modal_from_every_mark() {
        let mut app = app_with_two_panes(&["a", "b", "c"], "mem:///dst");
        app.focused_mut().mark_all();
        assert_eq!(app.focused().marks_len(), 3, "las tres quedaron marcadas");
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        let Some(Modal::ConfirmTransfer { items, to, .. }) = &app.modal else {
            panic!("no transfer modal");
        };
        assert_eq!(items.len(), 3);
        assert_eq!(to, &VPath::parse("mem:///dst").unwrap());
    }

    /// Sin ninguna marca, F5 sigue operando sobre el CURSOR (el gesto
    /// clásico no se pierde) — `marked_paths` cae al seleccionado. Con UN
    /// solo ítem la puerta abre el nombre EDITABLE (#105), no el confirm de
    /// lista: es la misma decisión para la tecla y para un drop.
    #[test]
    fn copy_without_marks_still_uses_the_cursor_entry() {
        let mut app = app_with_two_panes(&["a", "b"], "mem:///dst");
        assert_eq!(app.focused().marks_len(), 0, "sin marcas de partida");
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        let Some(Modal::TransferName {
            from,
            to_dir,
            from_marks,
            ..
        }) = &app.modal
        else {
            panic!("no transfer modal");
        };
        assert_eq!(
            from,
            &VPath::parse("mem:///a").unwrap(),
            "marked_paths falls back to the cursor"
        );
        assert_eq!(to_dir, &VPath::parse("mem:///dst").unwrap());
        assert!(!from_marks, "no había marca que consumir");
    }

    /// Un arrastre PROMOVIDO lleva la fila del press y NADA más: ni las
    /// marcas del pane (que son otra cosa que el usuario no ha soltado) ni
    /// su consumo al enviar. La promoción cambia lo que el gesto HACE, no lo
    /// que está seleccionado.
    #[test]
    fn a_promoted_transfer_carries_one_row_and_does_not_consume_the_marks() {
        let mut app = app_with_two_panes(&["a", "b", "c"], "mem:///dst");
        app.focused_mut().mark_all();
        assert_eq!(app.focused().marks_len(), 3);
        app.open_transfer(TransferKind::Copy, 0, 1, Some(2));
        let Some(Modal::TransferName {
            from, from_marks, ..
        }) = &app.modal
        else {
            panic!("un solo ítem: nombre editable");
        };
        assert_eq!(
            from,
            &VPath::parse("mem:///c").unwrap(),
            "la fila promovida, no las tres marcas"
        );
        assert!(!from_marks, "el envío NO puede consumir las marcas");
        app.transfer_name_submitted();
        assert_eq!(app.focused().marks_len(), 3, "las marcas siguen ahí");
    }

    /// Un índice promovido que ya no nombra ninguna fila (el listado encogió
    /// entre el gesto y el drop) no abre nada: jamás un diálogo sobre un
    /// lote vacío, y jamás cayendo hacia las marcas —que sería copiar lo que
    /// nadie arrastró—.
    #[test]
    fn a_promoted_index_out_of_range_opens_nothing() {
        let mut app = app_with_two_panes(&["a", "b"], "mem:///dst");
        app.focused_mut().mark_all();
        app.open_transfer(TransferKind::Copy, 0, 1, Some(9));
        assert!(app.modal.is_none());
    }

    /// Las marcas las CONSUME el ENVÍO del lote (mc/Total Commander): tras
    /// `consume_marks` no queda una selección a medio consumir.
    #[test]
    fn submitting_a_bulk_operation_consumes_the_marks() {
        let mut app = app_with_two_panes(&["a", "b"], "mem:///dst");
        app.focused_mut().mark_all();
        assert_eq!(app.focused().marks_len(), 2, "marcadas antes de enviar");
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        app.consume_marks();
        assert_eq!(app.focused().marks_len(), 0);
    }

    /// F8 sobre las marcas: el modal lleva el lote entero y el modo
    /// (papelera/permanente) que decidió el caller tras sondear la
    /// capability UNA vez.
    #[test]
    fn delete_builds_the_modal_from_every_mark() {
        let mut app = app_with_two_panes(&["a", "b", "c"], "mem:///dst");
        app.focused_mut().mark_all();
        app.open_delete_modal(true);
        let Some(Modal::ConfirmDelete { items, permanent }) = &app.modal else {
            panic!("no delete modal");
        };
        assert_eq!(items.len(), 3);
        assert!(*permanent);
    }

    /// Un pane VACÍO no abre modal: no hay nada que copiar ni que borrar
    /// (ni marcas ni cursor) — jamás un diálogo sobre un lote vacío.
    #[test]
    fn an_empty_pane_opens_no_bulk_modal() {
        let mut app = app_with_two_panes(&[], "mem:///dst");
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        assert!(app.modal.is_none(), "sin ítems no hay modal de copia");
        app.open_delete_modal(false);
        assert!(app.modal.is_none(), "sin ítems no hay modal de borrado");
    }

    /// #103 T9 review MINOR: `Modal::MarkPattern` no tiene ALLOWLIST — es
    /// texto libre, el run loop lo intercepta ANTES del contexto `dialog`
    /// (main.rs). Esto pinea la mitad de seguridad de esa afirmación:
    /// NINGÚN comando del vocabulario `dialog.*`, ni siquiera
    /// `dialog.confirm` (Enter), puede confirmarlo a través de
    /// `dialog_action` — si alguna vez este modal se colara al contexto
    /// `dialog` por un bug de enrutado, seguiría siendo inerte ahí.
    #[test]
    fn dialog_action_es_siempre_none_para_mark_pattern() {
        let m = Modal::MarkPattern {
            mark: true,
            pattern: String::new(),
            error: None,
        };
        for cmd in crate::keymap::DIALOG_COMMANDS {
            assert_eq!(
                dialog_action(&m, cmd),
                None,
                "{cmd} no debe confirmar/cancelar MarkPattern vía dialog_action"
            );
        }
    }

    /// Cancelar (`cancel_mark_pattern`, el Esc de este modal de texto libre)
    /// no marca nada, aunque el usuario ya hubiera tecleado un patrón — y
    /// cierra el modal, la propiedad real que este test debía pinear.
    #[test]
    fn the_pattern_modal_cancels_without_marking() {
        let mut app = app_with_entries(&["a.rs"]);
        app.open_mark_pattern(true);
        app.mark_pattern_push('*');
        app.cancel_mark_pattern();
        assert!(app.modal.is_none(), "cancel closes the modal");
        assert_eq!(app.focused().marks_len(), 0);
    }

    /// Review rust MAJOR M1: `cancel_mark_pattern` NO es un cierre genérico
    /// — con un `Modal::ApproveAgentOp` abierto (llegado, p. ej., mientras
    /// el usuario tecleaba un patrón que luego se sustituyó), debe dejarlo
    /// INTACTO. Cerrarlo sin el `policy.decide(approve: false)` async que
    /// hace `on_dialog_key` dejaría al agente sin respuesta hasta el TTL
    /// del daemon, y al humano sin volver a ver la pregunta.
    ///
    /// El guard es un `debug_assert!`: en ESTE build (test = dev,
    /// `debug-assertions` activas) panica ANTES de tocar `self.modal` — se
    /// captura con `catch_unwind` para poder comprobar el estado posterior
    /// en la misma aserción; en release sería un no-op y la función
    /// devolvería temprano igual, mismo resultado sobre el modal.
    #[test]
    fn cancel_mark_pattern_leaves_an_approval_modal_untouched() {
        let mut app = app_with_entries(&["a.rs"]);
        let approval = Modal::ApproveAgentOp {
            req: norte_proto::methods::PolicyApprovalRequired {
                approval_id: 7,
                session: Some("s1".into()),
                op: "copy".into(),
                paths: vec!["mem:///proj/a".into(), "mem:///proj/b".into()],
                paths_total: 0,
                ttl_ms: 60_000,
                detail: norte_proto::methods::ApprovalDetail::default(),
            },
        };
        app.modal = Some(approval.clone());
        // Silencia el hook de pánico por defecto: el panic se captura y se
        // espera, no debe ensuciar la salida de este test con un backtrace.
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            app.cancel_mark_pattern();
        }));
        std::panic::set_hook(prev_hook);
        assert!(
            result.is_err(),
            "el guard debe panicar en debug ante el mal uso"
        );
        assert_eq!(
            app.modal,
            Some(approval),
            "an approval modal must not be closeable without a decision"
        );
    }
}
