//! Contestar un modal, y mandar la mutación que la respuesta autoriza.
//!
//! Todo lo que ESCRIBE en el disco de alguien pasa por aquí, y por eso es el
//! módulo con la regla más estricta: una respuesta se traduce a una Task y nada
//! más. La decisión —qué se pregunta, con qué aviso, y cuántas veces— es del
//! modelo (`crate::app`) y de su allowlist; esto solo la ejecuta.
//!
//! Vivía en el root del binario `ntc`, un crate DISTINTO de esta lib, así que
//! el único test que podía escribirse era el de la función pura que reparte
//! destinos ([`transfer_dests`]).

use crossterm::event::{EventStream, KeyCode, KeyModifiers};
use norte_core::TransferOptions;
use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use norte_proto::{DeleteMode, VPath};

use crate::app::{
    App, DialogOutcome, Modal, TransferKind, detail_for_bar, dialog_action, error_category,
    error_message,
};
use crate::keymap::{Resolution, Resolver, chord_from_crossterm};
use crate::navigate::Cd;
use crate::navigate::{semantic_hit_cd, settle_suspended_trail, trust_host_retry};
use crate::overlays::{modal_help_toggle, modal_scroll};
use crate::tasks::RetrySpec;

/// Teclas de un modal abierto, resueltas contra el contexto `dialog` del
/// keymap (H1 T2, issue #24 CERRADO — rebindeable) y filtradas por el
/// ALLOWLIST del modal concreto ([`crate::app::dialog_action`]): la semántica de
/// seguridad vive en código, solo la ASIGNACIÓN tecla→comando es keymap.
/// `Modal::TrustLuaInit` nunca llega aquí (interceptado antes en el run
/// loop, decisión 8). `events` es para el reintento de navegación del modal
/// TOFU (#45): confiar en la host key relanza el `cd`, que tiene su propio
/// loop de eventos.
#[allow(clippy::too_many_arguments)] // wiring del run loop, no API
pub async fn on_dialog_key(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
    lang: norte_help::Lang,
    help_lines: &[ratatui::text::Line<'static>],
) -> Cd {
    let Some(modal) = app.modal.clone() else {
        return Cd::Cancelled;
    };
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return Cd::Cancelled; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        // Sin semántica de secuencia definida para overlays (T2), y lo mismo
        // para una tecla ligada a algo que esta build no corre (K1 T4):
        // ignorar y reiniciar el estado de resolución.
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return Cd::Cancelled;
        }
        Resolution::Reset => return Cd::Cancelled,
    };
    // H3c: ANTES del allowlist, que dejaría caer `app.help` — no es un verbo
    // `dialog.*`. La ayuda se abre sobre el modal y se queda las teclas
    // (`help_owns_keys`); el modal sigue intacto detrás.
    if modal_help_toggle(app, cmd.as_str(), lang, help_lines) {
        return Cd::Cancelled;
    }
    if modal_scroll(app, cmd.as_str()) {
        return Cd::Cancelled;
    }
    let Some(outcome) = dialog_action(&modal, &cmd) else {
        return Cd::Cancelled; // comando fuera del allowlist de ESTE modal
    };
    match outcome {
        DialogOutcome::Open => {} // dialog_action nunca lo devuelve: defensivo
        DialogOutcome::Cancelled => {
            app.modal = None;
            app.open_next_pending();
            match modal {
                // Cerrar el diálogo de aprobación ES denegar (fail-safe): el
                // agente recibe `not-approved`, jamás una espera colgada.
                Modal::ApproveAgentOp { req } => {
                    decide_approval(app, backend, req.approval_id, false).await;
                }
                // DENEGAR la host key abandona la navegación que el TOFU
                // suspendió: no hay reintento que la termine, así que el paso
                // del rastro que `walk_trail` dejó dado vuelve aquí. Es el
                // camino MÁS probable de los tres (decir que no a un host
                // desconocido es lo normal), y el único que no pasa por
                // `trust_host_retry`.
                Modal::TrustHostKey {
                    dir, pane, trail, ..
                } => settle_suspended_trail(app, pane, &dir, trail, &Cd::Cancelled),
                _ => {}
            }
        }
        DialogOutcome::Confirmed => {
            if let Some(cd) = confirm_modal(app, backend, events, modal).await {
                return cd;
            }
        }
        DialogOutcome::Retry(policy) => {
            app.modal = None;
            if let Modal::Collision { retry } = modal {
                // Conserva las opciones ORIGINALES; solo cambia la política.
                let opts = TransferOptions {
                    on_collision: policy,
                    ..retry.opts
                };
                submit_transfer(app, backend, retry.kind, retry.from, retry.to, opts).await;
            }
            app.open_next_pending();
        }
    }
    // Salvo el retry TOFU (que hace `return cd(...)`), un modal no navega.
    Cd::Cancelled
}

/// Lo que hace CONFIRMAR cada modal.
///
/// Extraída del `match` de `on_dialog_key` cuando éste pasó de cien líneas
/// (#132 le añadió dos brazos). El `Some(cd)` es el único camino que NAVEGA —
/// el retry TOFU y el salto a un hit semántico—, y por eso vuelve al llamante
/// en vez de resolverse aquí: es él quien decide qué hacer con un `Cd`.
///
/// El `match` sigue siendo EXHAUSTIVO a propósito: nombrar los modales que no
/// confirman nada es lo que hace que añadir uno nuevo sea un error de
/// compilación en vez de un Enter que hace algo a escondidas.
pub async fn confirm_modal(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    modal: Modal,
) -> Option<Cd> {
    app.modal = None;
    // OJO (MAJOR del rust-reviewer): NO abrir la siguiente pendiente
    // ANTES del match — el retry TOFU (`return cd`) puede reabrir un
    // TrustHostKey y PISAR una aprobación de agente ya sacada de la
    // cola (quedaría huérfana hasta su TTL). Se difiere al final.
    match modal {
        Modal::ConfirmDelete { items, permanent } => {
            submit_deletes(app, backend, &items, permanent).await;
        }
        Modal::ConfirmTransfer {
            kind, items, to, ..
        } => {
            let o = TransferOptions::default();
            submit_transfers(app, backend, kind, &items, &to, o).await;
        }
        // TrustLuaInit se intercepta ANTES en el run loop (necesita
        // el LuaHost); MarkPattern (#103 T9) también, como texto
        // libre (mismo motivo que la búsqueda) — `dialog_action`
        // devuelve `None` para ambos, así que `on_dialog_key` ya
        // habría retornado antes de llegar a este match: inalcanzable
        // aquí, no-op defensivo.
        // Y las propiedades (#139) tampoco: `dialog_action` solo les
        // entiende cancelar, así que un «confirmar» no llega aquí —
        // nombrarlas es lo que hace que añadir uno sea un error de
        // compilación y no un Enter que hace algo a escondidas.
        Modal::Properties { .. }
        | Modal::Collision { .. }
        | Modal::TrustLuaInit { .. }
        | Modal::MarkPattern { .. }
        | Modal::Mkdir { .. }
        | Modal::CommandLine { .. }
        | Modal::AiRenameInstruction { .. }
        | Modal::SemanticQuery { .. }
        | Modal::TransferDest { .. }
        | Modal::Pack { .. }
        | Modal::Split { .. }
        | Modal::TransferName { .. } => {}
        // `AiRenamePlan` (M4-IA) SÍ es una superficie de decisión:
        // confirmar aplica el plan REVISADO por el ejecutor
        // transaccional de lotes (§17) — UNA task gobernada (journal
        // + policy) para el lote entero, en el orden que decidió el
        // core. Se aplican TODAS las parejas, no solo la ventana
        // visible: el scroll (audit MAJOR-3) hace revisable el plan
        // entero.
        Modal::AiRenamePlan {
            dir, entries, plan, ..
        } => {
            apply_ai_rename(app, backend, &dir, &entries, &plan).await;
        }
        // M4-IA-2: confirmar NAVEGA al hit bajo el cursor
        // (`semantic_hit_cd`). El `Cd` vuelve al caller (apply_cd +
        // decorate), como el retry TOFU; si el cd abrió un modal
        // (otro HostKeyUnknown), la siguiente pendiente espera —
        // jamás pisar.
        Modal::SemanticHits { hits, cursor, .. } => {
            let outcome = semantic_hit_cd(app, backend, events, &hits, cursor).await;
            if app.modal.is_none() {
                app.open_next_pending();
            }
            return Some(outcome);
        }
        // S2 (`[ui] confirm_quit`): confirmar cierra — el run loop
        // lo detecta en su chequeo de `app.quit` de cada vuelta
        // (main.rs, tope del `loop`).
        Modal::ConfirmQuit => app.quit = true,
        Modal::ApproveAgentOp { req } => {
            decide_approval(app, backend, req.approval_id, true).await;
        }

        // TOFU (#45): confía en la host key y REINTENTA la navegación.
        m @ Modal::TrustHostKey { .. } => {
            if let Some(outcome) = trust_host_retry(app, backend, events, m).await {
                return Some(outcome);
            }
        }
    }
    // Todas las ramas salvo el retry TOFU (que ya volvió) abren aquí
    // la siguiente pendiente, con el modal ya cerrado.
    app.open_next_pending();
    None
}

/// Resuelve una aprobación de policy (`policy.decide`, M3-3b T5). Un error
/// (id ya vencido/decidido por otro frontend, daemon caído) sale por la
/// barra: la pendiente, si sigue viva, vencerá por TTL — jamás se cuelga.
pub async fn decide_approval(app: &mut App, backend: &Backend, approval_id: u64, approve: bool) {
    if let Err(e) = backend.policy_decide(approval_id, approve).await {
        app.message = Some(error_message(&e));
    }
}

/// Los pares `(origen, destino)` de un lote: cada ítem aterriza en el
/// DIRECTORIO `to` con SU MISMO nombre — el nombre son BYTES (`Segment`,
/// regla 1), jamás texto, así que un `Папка` o un `\xff` viaja intacto. Un
/// ítem sin nombre (la raíz de un scheme) no es transferible y se descarta:
/// no hay nada que colgar del destino.
///
/// PURA a propósito: el lote entero se ve sin levantar backend.
#[must_use]
pub fn transfer_dests(items: &[VPath], to: &VPath) -> Vec<(VPath, VPath)> {
    items
        .iter()
        .filter_map(|from| {
            let name = from.file_name()?.clone();
            Some((from.clone(), to.join(name)))
        })
        .collect()
}

/// Envía el lote de copia/movimiento: UNA task POR ÍTEM (#103 T10), cada una
/// con su progreso, su cancelación y sus entradas de journal propias —
/// cancelar una no toca a las demás.
///
/// Un fallo NO aborta el lote: los ítems restantes se envían igual y el
/// último error queda en la barra. Abandonar 4..n porque el 3 falló dejaría
/// media selección hecha sin decirlo; el panel de tasks muestra el resultado
/// de cada una por separado. Las colisiones no viajan por aquí: llegan
/// ASÍNCRONAS al terminar la task y `on_tick` las ENCOLA
/// (`pending_collisions`) para no pisar jamás un modal abierto.
///
/// Las marcas se consumen al ENVIAR el lote, no al completarse.
pub async fn submit_transfers(
    app: &mut App,
    backend: &Backend,
    kind: TransferKind,
    items: &[VPath],
    to: &VPath,
    opts: TransferOptions,
) {
    for (from, dest) in transfer_dests(items, to) {
        let _submitted = submit_transfer(app, backend, kind, from, dest, opts).await;
    }
    app.consume_marks();
}

/// Envía el lote de borrado: UNA task POR ÍTEM, mismo criterio que
/// [`submit_transfers`] (un fallo no abandona el resto). El objetivo de
/// papelera viaja con cada task para que un `Unsupported` reofrezca el
/// PERMANENTE de ESE ítem (ADR 0009), no del lote entero.
pub async fn submit_deletes(app: &mut App, backend: &Backend, items: &[VPath], permanent: bool) {
    let del_mode = if permanent {
        DeleteMode::Permanent
    } else {
        DeleteMode::Trash
    };
    for target in items {
        match backend.delete(target, del_mode).await {
            Ok(task) => {
                app.board
                    .push_full(&task, None, (!permanent).then(|| target.clone()));
            }
            Err(e) => app.message = Some(error_message(&e)),
        }
    }
    app.consume_marks();
}

/// Lanza un recuento de tamaño y lo registra en el panel de tasks (#139).
///
/// `para_el_dialogo` ata la Task al modal de propiedades abierto, para que su
/// resultado llegue AHÍ y no solo a la barra de estado.
///
/// El total no vuelve por aquí: llega en el progreso terminal de la Task, que
/// es lo que `on_tick` ya está mirando para todas las demás.
pub async fn launch_size_count(
    app: &mut App,
    backend: &Backend,
    paths: Vec<VPath>,
    para_el_dialogo: bool,
) {
    if paths.is_empty() {
        return;
    }
    match backend
        .dir_size(norte_proto::methods::FsDirSizeParams { paths })
        .await
    {
        Ok(task) => {
            if para_el_dialogo {
                app.properties_counting(task.id());
            } else {
                app.message = Some(t("msg-dir-size-counting"));
            }
            app.board.push(&task, None);
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// `pane.unpack` (#132): copia el INTERIOR del contenedor bajo el cursor al
/// otro panel.
///
/// No lleva método propio y no le hace falta: el motor de copia ya acepta el
/// interior de un archivo como origen, así que desempaquetar es la copia que
/// el usuario podría haber hecho a mano, con el journal, el undo, la política
/// de colisiones y la cancelación que la copia ya tiene.
pub async fn unpack(app: &mut App, backend: &Backend) {
    let Some(entry) = app.focused().selected().cloned() else {
        return;
    };
    let Some(raiz) = crate::nav::archive_root_for(&entry) else {
        app.message = Some(t("msg-unpack-not-archive"));
        return;
    };
    // El destino es el OTRO panel, que es donde un gestor ortodoxo
    // desempaqueta. Con uno solo, el mismo — que es lo que hace F5 cuando no
    // hay otro sitio al que apuntar.
    // La misma noción de «el otro» que usa partir: por POSICIÓN visible, y con
    // un solo panel el mismo. `focus() ^ 1` daba un índice fuera de rango con
    // tres o cuatro paneles, y ahí `pane_read_only` contesta `false` sin mirar
    // nada — el gate quedaba inerte justo donde hay más sitios a los que
    // apuntar por error.
    let other = app.split_dest_pane();
    if app.pane_read_only(other) {
        app.message = Some(t("msg-pack-read-only"));
        return;
    }
    let dest = app.panes[other].dir().clone();
    match backend
        .copy(&raiz, &dest, TransferOptions::default())
        .await
    {
        Ok(task) => {
            app.message = Some(t("msg-unpack-started"));
            app.board.push(&task, None);
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// `pane.test-archive` (#132): comprueba el contenedor bajo el cursor.
pub async fn test_archive(app: &mut App, backend: &Backend) {
    let Some(entry) = app.focused().selected().cloned() else {
        return;
    };
    if crate::nav::archive_root_for(&entry).is_none() {
        app.message = Some(t("msg-unpack-not-archive"));
        return;
    }
    match backend
        .test_archive(norte_proto::methods::ArchiveTestParams {
            path: entry.path.clone(),
        })
        .await
    {
        Ok(task) => {
            app.message = Some(t("msg-test-archive-started"));
            app.board.push(&task, None);
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// `pane.combine-files` (#132): junta los trozos a partir del `.001` bajo el
/// cursor.
pub async fn combine_pieces(app: &mut App, backend: &Backend) {
    let Some(entry) = app.focused().selected().cloned() else {
        return;
    };
    let name = entry
        .path
        .file_name()
        .map(|s| s.as_bytes().to_vec())
        .unwrap_or_default();
    // Solo desde el PRIMER trozo: empezar por el `.007` uniría media cosa, y
    // el core ya solo sabe buscar hacia delante.
    let Some(base) = name
        .len()
        .checked_sub(4)
        .filter(|n| name[*n] == b'.' && &name[n + 1..] == b"001")
        .map(|n| name[..n].to_vec())
    else {
        app.message = Some(t("msg-combine-needs-first"));
        return;
    };
    let Ok(seg) = norte_proto::Segment::new(base) else {
        app.message = Some(t("msg-combine-needs-first"));
        return;
    };
    let destino = app.focused().dir().join(seg);
    match backend
        .combine_files(norte_proto::methods::FileCombineParams {
            first: entry.path,
            dest: destino,
        })
        .await
    {
        Ok(task) => {
            app.message = Some(t("msg-combine-started"));
            app.board.push(&task, None);
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// Encola una transferencia y la registra en el panel con su contexto de
/// reintento (para el diálogo de colisión).
/// Devuelve `true` si la task ENCOLÓ (#105: el modal de nombre editable
/// solo se cierra entonces); un fallo deja el error en la barra.
pub async fn submit_transfer(
    app: &mut App,
    backend: &Backend,
    kind: TransferKind,
    from: VPath,
    to: VPath,
    opts: TransferOptions,
) -> bool {
    let res = match kind {
        TransferKind::Copy => backend.copy(&from, &to, opts).await,
        TransferKind::Move => backend.move_(&from, &to, opts).await,
    };
    match res {
        Ok(task) => {
            // #98/M1: el enc del pane origen viaja con el retry — la
            // colisión llega async y el foco puede haber cambiado.
            let name_encoding = app.focused().name_encoding();
            app.board.push(
                &task,
                Some(RetrySpec {
                    kind,
                    from,
                    to,
                    opts,
                    name_encoding,
                }),
            );
            true
        }
        Err(e) => {
            app.message = Some(error_message(&e));
            false
        }
    }
}

/// Aplica un plan de rename IA CONFIRMADO (M4-IA) por el ejecutor
/// TRANSACCIONAL de lotes (spec §17, ADR 0042): UNA task, UNA unidad
/// deshacible del journal, rollback si un paso falla.
///
/// Sustituye al bucle de un `fs.move` por pareja, que no era una
/// transacción (el quinto fallo dejaba cuatro aplicados), no comprobaba el
/// plan contra sí mismo, y no podía hacer una permutación — el caso NORMAL
/// del rename IA («numera bien estos episodios»), donde `a→b, b→c` chocaba
/// en el primer move.
///
/// Tres negativas, en orden, y ninguna encola nada:
///
/// - una pareja que no es un [`norte_proto::Segment`] = plan adulterado
///   (cinturón [`norte_frontend::rename_pairs`], COMPARTIDO con la GUI —
///   quality review 78eb243 MAJOR-1, audit MAJOR-2);
/// - sin plan de lote no hay `plan_hash` aprobado que mandar;
/// - con veredictos el core no ejecutaría nada, así que ni se pide.
///
/// Las tres son cinturón: la tecla de confirmar ya está muda sin un plan
/// aplicable (`dialog_action`). Lo que llega aquí es un solo submit, y su
/// fallo va entero a la barra.
pub async fn apply_ai_rename(
    app: &mut App,
    backend: &Backend,
    dir: &VPath,
    entries: &[norte_proto::methods::AiRenameEntry],
    plan: &norte_frontend::BatchPlan,
) {
    let Some(pairs) = norte_frontend::rename_pairs(entries) else {
        app.message = Some(t("msg-ai-rename-invalid-plan"));
        return;
    };
    let Some(resuelto) = plan.ready() else {
        app.message = Some(t("msg-rename-batch-no-plan"));
        return;
    };
    if !resuelto.executable {
        app.message = Some(t("msg-rename-batch-collisions"));
        return;
    }
    // Lo que se anuncia son los renames que el core se comprometió a hacer,
    // no las parejas PEDIDAS: el planificador tira las nulas (`from == to`),
    // y prometer más de lo que va a pasar es mentir en la barra.
    let n = plan.real_steps();
    match backend.rename_batch(dir, &pairs, &resuelto.plan_hash).await {
        Ok(task) => {
            app.board.push(&task, None);
            app.message = Some(ta("msg-rename-batch-applied", &[("n", &n.to_string())]));
        }
        Err(e) => {
            app.message = Some(ta(
                "msg-rename-batch-failed",
                &[("error", &detail_for_bar(&error_category(&e)))],
            ));
        }
    }
}

#[cfg(test)]
mod bulk_tests {
    use super::transfer_dests;
    use norte_proto::VPath;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire válido")
    }

    /// #103 T10: el lote se envía ENTERO — un par por ítem, cada uno con SU
    /// nombre colgado del directorio destino. (Mutación de control: hacer
    /// que el envío use solo el primer ítem rompe este test.)
    #[test]
    fn a_bulk_transfer_submits_every_item_not_just_the_first() {
        let items = vec![vp("mem:///src/a"), vp("mem:///src/b"), vp("mem:///src/c")];
        let pairs = transfer_dests(&items, &vp("mem:///dst"));
        assert_eq!(pairs.len(), 3, "una task POR ítem");
        assert_eq!(
            pairs.iter().map(|(_, d)| d.clone()).collect::<Vec<_>>(),
            vec![vp("mem:///dst/a"), vp("mem:///dst/b"), vp("mem:///dst/c")],
        );
    }

    /// Regla 1: el nombre son BYTES. Un nombre no-UTF8 llega al destino
    /// byte a byte — el destino jamás se construye desde el texto pintado.
    #[test]
    fn a_bulk_transfer_keeps_non_utf8_names_byte_exact() {
        let raw = b"caf\xff\xfe.txt".to_vec();
        let seg = norte_proto::Segment::new(raw.clone()).expect("segmento");
        let from = vp("mem:///src").join(seg);
        let pairs = transfer_dests(std::slice::from_ref(&from), &vp("mem:///dst"));
        assert_eq!(pairs.len(), 1);
        assert_eq!(
            pairs[0].1.file_name().map(|s| s.as_bytes().to_vec()),
            Some(raw),
            "los bytes del nombre viajan intactos al destino",
        );
    }

    /// El destino IGUAL que el origen (mismo dir en ambos panes) rinde un
    /// par `from == to`: la decisión de qué hacer con eso es del engine
    /// (colisión), no del frontend — que no debe inventarse un descarte.
    #[test]
    fn a_same_directory_transfer_maps_each_item_onto_itself() {
        let items = vec![vp("mem:///src/a")];
        let pairs = transfer_dests(&items, &vp("mem:///src"));
        assert_eq!(pairs[0].0, pairs[0].1);
    }

    /// Una raíz de scheme no tiene nombre que colgar del destino: se
    /// descarta en vez de fabricar una ruta.
    #[test]
    fn a_rootless_item_is_dropped_from_the_batch() {
        let root = VPath::root(norte_proto::Scheme::new("mem").unwrap(), None);
        assert!(transfer_dests(&[root], &vp("mem:///dst")).is_empty());
    }
}
