//! Comparar dos directorios, y el panel que enseña las filas mientras llegan.

use crossterm::event::{EventStream, KeyCode};
use norte_core::backend::{Backend, TaskRef};
use norte_frontend::layout::BySlot;
use norte_i18n::ta;

use super::search::SearchRun;
use crate::app::{App, CompareState, detail_for_bar, error_category, error_message};
use crate::fill::Fill;
use crate::navigate::{apply_cd, cd};
use crate::probes::{DecorateFetch, Probed};

/// Una comparación de directorios EN CURSO (`Shift+F2`,
/// 2026-08-11-directory-comparison.md): la Task cancelable y el canal de lotes
/// de filas. Mismo molde que [`SearchRun`] — vive en el run loop, se drena en
/// el `select!` y se suelta al cerrarse el panel, cancelando la Task (regla 3).
pub struct CompareRun {
    /// Task de `fs.compare` (cancelable con `TaskRef::cancel`).
    pub task: TaskRef,
    /// Canal de lotes de filas (embebido: lo cierra el walk; remoto: la bomba
    /// del `RemoteBackend` lo cierra al terminal).
    pub rx: tokio::sync::mpsc::Receiver<norte_proto::methods::CompareRowsBatch>,
    /// Filas RECIBIDAS. Contador propio y no `pane.len()` por lo mismo que en
    /// la búsqueda: no depender de lo que el modelo haga con ellas.
    pub rows: usize,
    /// Estado del run; terminal tras cerrarse el canal.
    pub state: CompareState,
}

/// Lanza `fs.compare` sobre los dos panes y abre el panel de diferencias.
///
/// Los params ya vienen resueltos y validados por
/// [`App::request_compare`](crate::app::App::request_compare) — este lado
/// solo es dueño del canal y de la Task. Un panel anterior se reemplaza y su
/// Task se cancela (regla 3): dos comparaciones a la vez serían dos flujos
/// alimentando un solo panel.
pub async fn launch_compare(
    app: &mut App,
    backend: &Backend,
    compare_run: &mut Option<CompareRun>,
    params: norte_proto::methods::FsCompareParams,
) {
    let (left_root, right_root) = (params.left.clone(), params.right.clone());
    match backend.compare(params).await {
        Ok((task, rx)) => {
            app.message = None;
            // Las dos reinterpretaciones (#57) salen de los dos panes de
            // los que salieron las raíces, en ese mismo orden.
            let left = app.focus();
            let (left_encoding, right_encoding) = (
                app.panes[left].name_encoding(),
                app.panes[left ^ 1].name_encoding(),
            );
            app.compare = Some(crate::app::CompareView::new(
                left_root,
                right_root,
                left,
                left_encoding,
                right_encoding,
            ));
            // #157: la caché de tamaños hidratados y su dedup son de ESTA
            // comparación — una nueva empieza sin nada pedido, igual que
            // `last_probed` se vacía con cada listado nuevo. Y avanza la
            // generación (#198): una sonda de la comparación anterior sigue en
            // vuelo, y sin la marca aterrizaría en estas tablas recién
            // vaciadas.
            app.begin_compare_generation();
            if let Some(old) = compare_run.replace(CompareRun {
                task,
                rx,
                rows: 0,
                state: CompareState::Running,
            }) {
                old.task.cancel();
            }
        }
        // El panel NO se abre: un panel vacío que dice «fallo» es peor que la
        // frase en la barra, porque además hay que cerrarlo. La categoría va
        // saneada, jamás el error crudo del provider.
        Err(e) => {
            app.message = Some(ta(
                "compare-status-failed",
                &[("error", &detail_for_bar(&error_category(&e)))],
            ));
        }
    }
}

/// Aplica un lote de filas (o el cierre del canal) al panel de diferencias.
///
/// `None` = fin del flujo. Y ahí está la diferencia con la búsqueda, que es lo
/// que C6 descubrió y el plan no dice: **que el canal se cierre NO significa
/// que hayan llegado todas las filas**. La bomba de filas y la del snapshot
/// terminal son tasks independientes — pero la CUENTA que decide `Done` contra
/// [`CompareState::Incomplete`] ya no vive aquí (#158), y desde la revisión
/// de rama (MAJOR-1) tampoco vive aquí el MAPEO entero: los cuatro brazos son
/// [`norte_frontend::compare::CompareView::finish_from_task`], que es lo que
/// también llama la GUI. Lo que queda de este lado es el aviso PASAJERO de la
/// barra, que la GUI no tiene.
pub fn drain_compare(
    app: &mut App,
    compare_run: &mut Option<CompareRun>,
    batch: Option<norte_proto::methods::CompareRowsBatch>,
) {
    let Some(c) = compare_run else {
        return;
    };
    if let Some(b) = batch {
        // El panel se cerró bajo el drenador: se suelta el run cancelando la
        // Task, igual que hace la búsqueda al salir del modo virtual.
        let Some(view) = app.compare.as_mut() else {
            c.task.cancel();
            *compare_run = None;
            return;
        };
        c.rows += b.rows.len();
        view.pane.extend(b.rows);
    } else {
        let mut rx = c.task.progress();
        let snapshot = rx.borrow_and_update().clone();
        let expected = snapshot.entries_done;
        if let Some(view) = app.compare.as_mut() {
            // El mapeo entero —los cuatro brazos— es del modelo. Aquí solo
            // queda el aviso PASAJERO de la barra, que es lo único que esta
            // superficie tiene y la GUI no.
            let notice = view
                .finish_from_task(
                    &snapshot.state,
                    expected,
                    c.rows as u64,
                    norte_i18n::active(),
                )
                .map(error_message);
            c.state = view.state;
            if let Some(m) = notice {
                app.message = Some(m);
            }
        } else {
            // El panel ya se cerró: no hay nada que pintar, y el único uso
            // de `c.state` es una comprobación de `== Running` (aquí abajo
            // en `on_compare_key`, y en el `select!` del run loop) —
            // cualquier variante terminal le sirve, así que no se vuelve a
            // decidir cuál (era una CUARTA copia de la cuenta `Completed` vs
            // `Incomplete`, y la única que nadie podía ver equivocarse).
            c.state = CompareState::Done;
        }
    }
}

/// Cuántas filas mueve una página en el panel de diferencias.
///
/// Una constante y no el alto del frame: el layout del panel lo decide
/// `ui::draw` y todavía no devuelve su geometría al run loop como hace
/// `pane_geometry` para los panes. Diez filas es lo que un `PageDown` recorre
/// en un pane de altura media, así que el error es de RECORRIDO y no de
/// corrección — el cursor nunca sale de la lista, porque `move_by` clampa.
pub const COMPARE_PAGE_STEP: isize = 10;

/// Lo que una tecla SIGNIFICA en el panel de diferencias.
///
/// Separado del despacho para poder afirmarlo sin un backend ni un terminal:
/// lo que se puede equivocar aquí es la DECISIÓN —y una de ellas, la salida,
/// es la diferencia entre un overlay y una trampa—, no el `cd` que viene
/// después.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareKey {
    /// Nada que hacer con esta tecla.
    Ignore,
    /// Salir de norte (`Ctrl+C`, como en todos los demás overlays).
    Quit,
    /// Pedir la cancelación de la Task y quedarse: las filas ya llegadas se
    /// conservan.
    CancelTask,
    /// Cerrar el panel, cancelando la Task si sigue viva.
    Close,
    /// Cambiar el lado activo.
    SwapSide,
    /// Mover el cursor.
    Move(isize),
    /// Al principio de lo visible.
    First,
    /// Al final de lo visible.
    Last,
    /// Alternar el filtro de la categoría `n` de `CATEGORIES`.
    Filter(usize),
    /// Ir a donde vive la fila.
    Open,
    /// Marcar o desmarcar la fila del cursor: lo que marque siembra el
    /// `include` del plan.
    Mark,
    /// Planificar una sincronización en este modo.
    Sync(norte_proto::methods::SyncMode),
}

/// Traduce una tecla del panel de diferencias.
///
/// * `Ctrl+C` sale de norte. Este panel se queda el teclado ENTERO y `ISIG`
///   está apagado en modo raw, así que sin esto era la única pantalla de
///   norte de la que no se sale (review BLOCKER-1); los otros nueve overlays
///   lo atienden igual.
/// * El primer `Esc` sobre una comparación viva la cancela y conserva sus
///   filas; **cualquier `Esc` posterior cierra**, sin mirar el estado de la
///   Task. Condicionar el cierre a un estado terminal dejaba encerrado al
///   lector cuando el canal de filas no llegaba a cerrarse nunca — un daemon
///   caído, un provider colgado en una NFS muerta.
/// * Cualquier otro modificador no pinta nada: sin ese filtro `Alt+1`
///   togglea un filtro y `Alt+Tab` cambia de lado.
#[must_use]
pub fn compare_key(
    mods: crossterm::event::KeyModifiers,
    code: KeyCode,
    running: bool,
    cancel_requested: bool,
) -> CompareKey {
    use crossterm::event::KeyModifiers as M;
    if mods.contains(M::CONTROL) {
        return if code == KeyCode::Char('c') {
            CompareKey::Quit
        } else {
            CompareKey::Ignore
        };
    }
    if mods.contains(M::ALT) {
        return CompareKey::Ignore;
    }
    match code {
        KeyCode::Esc => {
            if running && !cancel_requested {
                CompareKey::CancelTask
            } else {
                CompareKey::Close
            }
        }
        KeyCode::Tab => CompareKey::SwapSide,
        KeyCode::Up => CompareKey::Move(-1),
        KeyCode::Down => CompareKey::Move(1),
        KeyCode::PageUp => CompareKey::Move(-COMPARE_PAGE_STEP),
        KeyCode::PageDown => CompareKey::Move(COMPARE_PAGE_STEP),
        KeyCode::Home => CompareKey::First,
        KeyCode::End => CompareKey::Last,
        // El rango del patrón es exactamente el de `CATEGORIES`, así que el
        // índice no puede salirse.
        KeyCode::Char(c @ '1'..='5') => CompareKey::Filter(c as usize - '1' as usize),
        KeyCode::Enter => CompareKey::Open,
        KeyCode::Insert => CompareKey::Mark,
        // LETRAS PELADAS, y eso es la decisión (#159): bajo tmux ninguna tecla
        // de función con modificador llega, así que un `Shift+F5` aquí sería
        // un atajo documentado y muerto — este repo ya envió uno. Dentro de
        // este panel el teclado es entero suyo, así que no hay nada con lo que
        // chocar.
        KeyCode::Char('s') => CompareKey::Sync(norte_proto::methods::SyncMode::Update),
        KeyCode::Char('m') => CompareKey::Sync(norte_proto::methods::SyncMode::Mirror),
        _ => CompareKey::Ignore,
    }
}

/// Despacha una tecla del panel de diferencias (`Shift+F2`). Fijas, como las
/// del diálogo de búsqueda: no hay vocabulario `dialog.*` para «cambia de
/// lado» ni para «esconde los iguales». El significado lo decide
/// [`compare_key`]; esto solo lo ejecuta.
#[allow(clippy::too_many_arguments)]
pub async fn on_compare_key(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
    compare_run: &mut Option<CompareRun>,
    mods: crossterm::event::KeyModifiers,
    code: KeyCode,
) {
    use norte_frontend::compare::CATEGORIES;

    let Some(cancel_requested) = app.compare.as_ref().map(|v| v.cancel_requested) else {
        return;
    };
    let running = compare_run
        .as_ref()
        .is_some_and(|c| c.state == CompareState::Running);
    match compare_key(mods, code, running, cancel_requested) {
        CompareKey::Ignore => {}
        CompareKey::Quit => {
            if let Some(c) = compare_run.take() {
                c.task.cancel();
            }
            app.quit = true;
        }
        CompareKey::CancelTask => {
            if let Some(c) = compare_run.as_ref() {
                c.task.cancel();
            }
            if let Some(view) = app.compare.as_mut() {
                view.cancel_requested = true;
            }
        }
        CompareKey::Close => {
            // Soltar el `CompareRun` no cancela nada (`TaskRef` no tiene
            // `Drop`): en remoto el daemon seguiría recorriendo los dos
            // árboles enteros para un panel que ya no existe (review
            // MAJOR-4). Se cancela SIEMPRE al salir.
            if let Some(c) = compare_run.take() {
                c.task.cancel();
            }
            app.close_compare();
        }
        CompareKey::Open => {
            on_compare_enter(
                app,
                backend,
                events,
                fill,
                decorate_fetch,
                last_probed,
                search_run,
                compare_run,
            )
            .await;
        }
        // Fuera del brazo que toma prestado el `view`: resolver los params
        // necesita el `App` entero (las dos raíces, el journal del backend y
        // la barra donde se dice que no). Solo RESUELVE; lanzar es del run
        // loop, igual que al abrir este mismo panel.
        CompareKey::Sync(sync_mode) => {
            app.request_sync(sync_mode);
        }
        otra => {
            let Some(view) = app.compare.as_mut() else {
                return;
            };
            match otra {
                CompareKey::SwapSide => view.pane.swap_active_side(),
                CompareKey::Move(delta) => view.pane.move_by(delta),
                CompareKey::First => view.pane.select_first(),
                CompareKey::Last => view.pane.select_last(),
                CompareKey::Filter(i) => {
                    if let Some(cat) = CATEGORIES.get(i) {
                        view.pane.toggle_filter(*cat);
                    }
                }
                CompareKey::Mark => {
                    if let Some(id) = view.pane.selected_id() {
                        view.pane.toggle_mark(id);
                    }
                }
                _ => {}
            }
        }
    }
}

/// `Enter` sobre una fila del panel de diferencias: navega al directorio REAL
/// del lado ACTIVO y cierra el panel.
///
/// Un huérfano que el walk emitió como UNA fila sin enumerar su subárbol se
/// expande así, que es el motivo por el que la fila lleva el `Entry` entero y
/// no solo un nombre. Sin nada en el lado activo NO se cae al otro: se dice.
#[allow(clippy::too_many_arguments)]
pub async fn on_compare_enter(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
    compare_run: &mut Option<CompareRun>,
) {
    let Some(view) = app.compare.as_ref() else {
        return;
    };
    if view.pane.target_entry().is_none() {
        let side = view.pane.active_side();
        // La lengua ambiente, la misma que resuelven `t`/`ta` a dos líneas
        // de aquí: pasar una distinta daría una frase medio traducida.
        let side_word = norte_frontend::compare::side_label(side, norte_i18n::active());
        app.message = Some(ta("compare-no-target", &[("side", &side_word)]));
        return;
    }
    // El directorio al que ir lo decide el MODELO (regla 7): el propio path
    // si la fila es un directorio, su padre si es un fichero — la misma regla
    // que necesitará la GUI.
    let Some(dest) = view.pane.navigation_target() else {
        // Hay entrada pero no hay a dónde ir: un fichero colgado de la raíz
        // de su scheme no tiene padre. Se DICE, igual que el caso de arriba —
        // un `Enter` que no hace nada y no explica por qué se lee como que la
        // tecla está rota. Lo arregló la GUI y aquí faltaba (revisión de
        // rama, MAJOR-4).
        let side_word =
            norte_frontend::compare::side_label(view.pane.active_side(), norte_i18n::active());
        app.message = Some(ta("compare-no-target", &[("side", &side_word)]));
        return;
    };
    // Y el cursor cae sobre la entrada de la que se salió, byte-exacto (lo
    // consume el listado al aterrizar; si ya no existe, cae al default). La
    // GUI lo hacía y esta rama no, mientras su comentario reclamaba paridad
    // (revisión de rama, MINOR-8).
    let focus = view.pane.target_path().cloned();
    // Al pane del lado ACTIVO, y el foco con él: mandar SIEMPRE al pane con
    // foco le costaba al lector el otro directorio para ir a ver este.
    let dest_pane = app.compare_active_pane().unwrap_or_else(|| app.focus());
    if let Some(c) = compare_run.take() {
        c.task.cancel();
    }
    app.close_compare();
    app.set_focus(dest_pane);
    if let Some(p) = focus {
        app.panes[dest_pane].set_pending_focus(p);
    }
    let outcome = cd(app, backend, events, dest).await;
    apply_cd(
        &app.panes,
        fill,
        decorate_fetch,
        last_probed,
        search_run,
        outcome,
    );
}

/// The diff pane's run-loop wiring (`Shift+F2`,
/// 2026-08-11-directory-comparison.md). The MODEL is tested in
/// `norte-frontend`, without a terminal; what is pinned here is the part only
/// this crate can get wrong.
#[cfg(test)]
mod compare_tests {
    use super::{CompareRun, drain_compare, launch_compare};
    use crate::app::{App, CompareState, Pane};
    use norte_core::backend::TaskRef;
    use norte_proto::VPath;
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, CompareRow, CompareRowsBatch, CompareVerdict,
    };

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire de test")
    }

    fn app_en(left: &str, right: &str) -> App {
        App::new(
            Pane::new(vp(left), Vec::new()),
            Pane::new(vp(right), Vec::new()),
        )
    }

    /// A synthetic run whose task reports `state` and `entries_done`, plus the
    /// sender that stays alive so the channel is only closed on purpose.
    fn run_con(
        state: norte_proto::TaskState,
        entries_done: u64,
        recibidas: usize,
    ) -> (
        CompareRun,
        tokio::sync::mpsc::Sender<CompareRowsBatch>,
        tokio::sync::watch::Sender<norte_proto::TaskProgress>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel::<CompareRowsBatch>(4);
        let id = norte_proto::TaskId::new(1);
        let (progreso, prx) = tokio::sync::watch::channel(norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Compare,
            state,
            bytes_done: 0,
            bytes_total: None,
            entries_done,
            entries_total: None,
            current: None,
        });
        (
            CompareRun {
                task: TaskRef::synthetic_for_tests(id, prx),
                rx,
                rows: recibidas,
                state: CompareState::Running,
            },
            tx,
            progreso,
        )
    }

    fn row(id: u64) -> CompareRow {
        CompareRow {
            id,
            left: None,
            right: None,
            verdict: CompareVerdict::Error,
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Unknown,
            newer: None,
            reason: Some(norte_proto::methods::CompareReason::Unreadable),
            side: None,
            paired_under: None,
        }
    }

    /// El panel que LANZA es el izquierdo, aunque sea el pane derecho de la
    /// pantalla. La spec lo dice y no es cosmético: el lector que pulsa la
    /// tecla desde la derecha espera que su directorio sea «el suyo», y todas
    /// las marcas `<`/`>` del panel cuelgan de esa elección.
    #[test]
    fn el_pane_con_foco_es_el_lado_izquierdo() {
        let mut app = app_en("file:///a", "file:///b");
        app.switch_focus();
        app.request_compare();
        let params = app.pending_compare.expect("deja params pendientes");
        assert_eq!(params.left, vp("file:///b"));
        assert_eq!(params.right, vp("file:///a"));
    }

    /// C6, hallazgo 4: el daemon responde `InvalidPath` a dos raíces iguales,
    /// y tiene razón — pero la frase no depende de una vuelta por la red, y
    /// ninguna Task llega a existir.
    #[test]
    fn los_dos_panes_en_el_mismo_sitio_se_rechazan_aqui() {
        let mut app = app_en("file:///a", "file:///a");
        app.request_compare();
        assert!(app.pending_compare.is_none(), "no puede pedirse la Task");
        assert!(app.message.is_some(), "y el lector tiene que enterarse");
    }

    /// Una lista de hits no es un directorio: no hay raíz que mandar. Misma
    /// negativa que ya dan mirror y pull sobre un pane virtual.
    #[test]
    fn un_pane_virtual_no_puede_compararse() {
        let mut app = app_en("file:///a", "file:///b");
        app.panes[1].begin_search(vp("file:///b"));
        app.request_compare();
        assert!(app.pending_compare.is_none());
        assert_eq!(app.message, Some(norte_i18n::t("msg-pane-not-a-location")));
    }

    /// C6, hallazgo 3: no hay toggle de symlinks en la UI porque el engine
    /// acepta el campo y lo ignora, así que `Backend::compare` responde
    /// `Unsupported` a `true` antes de que exista Task. Pedir `true` desde
    /// aquí sería pedir una promesa que nadie cumple.
    #[test]
    fn jamas_se_pide_seguir_symlinks() {
        let mut app = app_en("file:///a", "file:///b");
        app.request_compare();
        assert!(!app.pending_compare.expect("params").follow_symlinks);
    }

    /// La copia local del default del wire no puede separarse del wire.
    #[test]
    fn la_tolerancia_por_defecto_sigue_al_wire() {
        let del_wire: norte_proto::methods::FsCompareParams =
            serde_json::from_str(r#"{"left":"file:///a","right":"file:///b"}"#)
                .expect("params mínimos");
        let mut app = app_en("file:///a", "file:///b");
        app.request_compare();
        assert_eq!(
            app.pending_compare.expect("params").mtime_tolerance_ms,
            del_wire.mtime_tolerance_ms
        );
    }

    /// **C6, hallazgo 2 — el que el plan no dice.** Que se cierre el canal de
    /// filas NO significa que hayan llegado todas: las dos bombas son tasks
    /// independientes. Con la Task `Completed` contando MÁS filas de las
    /// recibidas, el panel dice `Incomplete` — decir «hecho» sería mentir
    /// sobre lo completa que está la respuesta, que en una comparación es la
    /// respuesta entera.
    #[tokio::test]
    async fn un_lote_perdido_se_dice_en_vez_de_pasar_por_hecho() {
        let mut app = app_en("file:///a", "file:///b");
        app.compare = Some(crate::app::CompareView::new(
            vp("file:///a"),
            vp("file:///b"),
            0,
            None,
            None,
        ));
        // La task contó 9 filas; llegaron 7.
        let (run, _tx, _p) = run_con(norte_proto::TaskState::Completed, 9, 7);
        let mut run = Some(run);
        drain_compare(&mut app, &mut run, None);
        let view = app.compare.expect("el panel sigue abierto");
        assert_eq!(view.state, CompareState::Incomplete);
        assert_eq!(view.rows_expected, 9);
    }

    /// Y el caso contrario, que es el normal: todas las filas contadas
    /// llegaron, así que `Done` — sin acusar de pérdida a nadie.
    #[tokio::test]
    async fn cuando_llegan_todas_es_hecho_y_no_incompleto() {
        let mut app = app_en("file:///a", "file:///b");
        app.compare = Some(crate::app::CompareView::new(
            vp("file:///a"),
            vp("file:///b"),
            0,
            None,
            None,
        ));
        let (run, _tx, _p) = run_con(norte_proto::TaskState::Completed, 7, 7);
        let mut run = Some(run);
        drain_compare(&mut app, &mut run, None);
        assert_eq!(
            app.compare.expect("el panel").state,
            CompareState::Done,
            "una carrera benigna no puede leerse como pérdida"
        );
    }

    /// Cancelar conserva lo que llegó: la comparación no escribe nada, así
    /// que las filas ya vistas siguen siendo ciertas.
    #[tokio::test]
    async fn cancelar_conserva_las_filas_que_llegaron() {
        let mut app = app_en("file:///a", "file:///b");
        app.compare = Some(crate::app::CompareView::new(
            vp("file:///a"),
            vp("file:///b"),
            0,
            None,
            None,
        ));
        let (run, _tx, _p) = run_con(norte_proto::TaskState::Cancelled, 2, 2);
        let mut run = Some(run);
        drain_compare(
            &mut app,
            &mut run,
            Some(CompareRowsBatch {
                task_id: norte_proto::TaskId::new(1),
                rows: vec![row(1), row(2)],
            }),
        );
        drain_compare(&mut app, &mut run, None);
        let view = app.compare.expect("el panel");
        assert_eq!(view.state, CompareState::Cancelled);
        assert_eq!(view.pane.len(), 2);
    }

    /// Un lote que llega con el panel YA cerrado suelta el run y cancela la
    /// Task (regla 3): sin esto, el drenador seguiría vivo alimentando un
    /// panel que no existe.
    #[tokio::test]
    async fn un_lote_con_el_panel_cerrado_cosecha_el_run() {
        let mut app = app_en("file:///a", "file:///b");
        let (run, _tx, _p) = run_con(norte_proto::TaskState::Running, 0, 0);
        let mut run = Some(run);
        drain_compare(
            &mut app,
            &mut run,
            Some(CompareRowsBatch {
                task_id: norte_proto::TaskId::new(1),
                rows: vec![row(1)],
            }),
        );
        assert!(run.is_none(), "el run tiene que soltarse");
    }

    /// **Lo que cazó el arnés de tmux y ninguna aserción del modelo podía
    /// ver.** `Enter` sobre una fila navega al pane del lado ACTIVO, no al
    /// pane con FOCO: mirando el lado derecho, el Enter mandaba el pane
    /// izquierdo —el que tenía el foco— a ver el directorio de la derecha, y
    /// el lector se quedaba con los dos panes en el mismo sitio y su
    /// izquierda perdida.
    ///
    /// Se pinea sobre `compare_active_pane` y no sobre el `cd`, que necesita
    /// un backend y un `EventStream`: lo que puede equivocarse es la ELECCIÓN
    /// del pane, y es lo que esto fija.
    #[test]
    fn el_enter_va_al_pane_del_lado_activo_y_no_al_del_foco() {
        let mut app = app_en("file:///a", "file:///b");
        // Lanzada desde el pane DERECHO: el izquierdo del panel es panes[1].
        app.switch_focus();
        app.compare = Some(crate::app::CompareView::new(
            vp("file:///b"),
            vp("file:///a"),
            app.focus(),
            None,
            None,
        ));
        assert_eq!(
            app.compare_active_pane(),
            Some(1),
            "lado izquierdo = panes[1]"
        );
        app.compare
            .as_mut()
            .expect("el panel")
            .pane
            .swap_active_side();
        assert_eq!(
            app.compare_active_pane(),
            Some(0),
            "el lado derecho del panel es el OTRO pane, sea cual sea el foco"
        );
        // Y con el panel cerrado no hay pane que elegir.
        app.close_compare();
        assert_eq!(app.compare_active_pane(), None);
    }

    /// **Review BLOCKER-1.** El panel se queda el teclado ENTERO y `ISIG`
    /// está apagado en modo raw, así que si ninguna tecla cierra
    /// incondicionalmente, el panel es una trampa: el estado solo pasa a
    /// terminal cuando se cierra el canal de filas, y hay formas de que no se
    /// cierre nunca —un daemon que se cae publica el fallo en el watch sin
    /// tocar la ruta, un provider colgado en una NFS muerta no mira su token
    /// hasta que vuelva el syscall—. Las DOS salidas, afirmadas:
    #[test]
    fn del_panel_de_diferencias_siempre_se_puede_salir() {
        use crossterm::event::KeyModifiers as M;

        // Ctrl+C sale de norte, con la Task viva o muerta, igual que en los
        // otros nueve overlays.
        for viva in [true, false] {
            assert_eq!(
                super::compare_key(
                    M::CONTROL,
                    crossterm::event::KeyCode::Char('c'),
                    viva,
                    false
                ),
                super::CompareKey::Quit,
                "viva={viva}"
            );
        }
        // Primer Esc con la Task viva: cancela y CONSERVA las filas.
        assert_eq!(
            super::compare_key(M::NONE, crossterm::event::KeyCode::Esc, true, false),
            super::CompareKey::CancelTask
        );
        // El segundo cierra AUNQUE la Task siga diciendo que corre — que es
        // justo el caso en el que el canal no se cierra nunca.
        assert_eq!(
            super::compare_key(M::NONE, crossterm::event::KeyCode::Esc, true, true),
            super::CompareKey::Close
        );
        // Y con la Task ya terminal, el primer Esc cierra.
        assert_eq!(
            super::compare_key(M::NONE, crossterm::event::KeyCode::Esc, false, false),
            super::CompareKey::Close
        );
    }

    /// Un modificador que el panel no usa no puede COLARSE como la tecla
    /// pelada: sin el filtro, `Alt+1` escondía una categoría y `Alt+Tab`
    /// cambiaba de lado (review MINOR).
    #[test]
    fn una_tecla_con_alt_no_hace_nada_en_el_panel() {
        use crossterm::event::{KeyCode as C, KeyModifiers as M};
        for code in [C::Tab, C::Char('1'), C::Enter, C::Down] {
            assert_eq!(
                super::compare_key(M::ALT, code, false, false),
                super::CompareKey::Ignore,
                "{code:?}"
            );
            // Y sin modificador la MISMA tecla sí significa algo: el filtro
            // no puede haberse comido el caso normal.
            assert_ne!(
                super::compare_key(M::NONE, code, false, false),
                super::CompareKey::Ignore,
                "{code:?}"
            );
        }
        // Un Ctrl que no es Ctrl+C tampoco: se traga la tecla, no sale.
        assert_eq!(
            super::compare_key(M::CONTROL, C::Down, false, false),
            super::CompareKey::Ignore
        );
    }

    /// Un fallo al pedir la Task NO abre el panel: un panel vacío que dice
    /// «fallo» es peor que la frase en la barra, porque además hay que
    /// cerrarlo. Y el detalle va por categoría, jamás crudo.
    #[tokio::test]
    async fn un_lanzamiento_fallido_no_abre_el_panel() {
        let engine = norte_core::Engine::new();
        let backend = norte_core::backend::Backend::Embedded(std::sync::Arc::new(engine));
        let mut app = app_en("file:///a", "file:///b");
        let mut run: Option<CompareRun> = None;
        // `follow_symlinks: true` es `Unsupported` antes de que exista Task
        // alguna (C6, hallazgo 3): el fallo más barato de provocar aquí.
        launch_compare(
            &mut app,
            &backend,
            &mut run,
            norte_proto::methods::FsCompareParams {
                left: vp("file:///a"),
                right: vp("file:///b"),
                criteria: norte_proto::methods::CompareCriteria::default(),
                max_depth: None,
                mtime_tolerance_ms: 2000,
                follow_symlinks: true,
                descend_orphans: None,
            },
        )
        .await;
        assert!(app.compare.is_none(), "el panel no puede abrirse vacío");
        assert!(run.is_none());
        assert!(app.message.is_some());
    }
}
