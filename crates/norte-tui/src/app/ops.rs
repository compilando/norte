//! Los modales que nacen de las marcas del listado: la cola de colisiones y
//! de aprobaciones, copiar y mover, borrar, ordenar por esquema o por tecla y
//! la hoja de propiedades con su hidratación.

use super::App;
use super::modal::{Modal, TransferKind};
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
    /// que no pasa por el ALLOWLIST de [`dialog_action`] y por tanto no
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
        if !matches!(self.modal, Some(Modal::MarkPattern { .. })) {
            debug_assert!(
                false,
                "solo los modales de texto libre se cierran sin decisión; \
                 un modal de DECISIÓN debe denegar por on_dialog_key"
            );
            return;
        }
        self.modal = None;
        self.open_next_pending();
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
        let mut total: u64 = 0;
        for path in items {
            let entry = self.panes[pane]
                .entries()
                .iter()
                .find(|e| &e.path == path)?;
            if entry.kind != norte_proto::EntryKind::File {
                return None;
            }
            total = total.checked_add(entry.size?)?;
        }
        Some(total)
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
