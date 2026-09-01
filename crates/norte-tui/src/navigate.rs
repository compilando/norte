//! Cambiar de directorio, y el ritual que eso desencadena.
//!
//! Un `cd` no es una asignación: lista la primera página ([`first_page`]), abre
//! un relleno paginado para el resto, cachea capacidades si hacen falta, mueve
//! el rastro de navegación y deja un [`Cd`] que dice QUÉ pasó — porque el bucle
//! de eventos tiene que reconciliar su propio estado indexado por pane con lo
//! que acaba de cambiar de sitio ([`apply_cd`], [`reconcile_swap`]).
//!
//! Ese `Cd` es la razón de que esto no sea un `App::cd()`: el desenlace lo
//! consumen cosas que viven FUERA del modelo (los rellenos, las sondas, los
//! drenadores), y meterlo en `App` obligaría a que `App` los conociera.
//!
//! Vivía en el root del binario `ntc`, un crate DISTINTO de esta lib, en cuatro
//! trozos separados por diez mil líneas. Sus seis módulos de test se quedan por
//! ahora en `main.rs`: prueban el ritual completo, así que nombran también las
//! tareas de búsqueda y el refresco de panes, que todavía no han salido.

use futures::StreamExt as _;
use norte_core::backend::{Backend, EntryStream};
use norte_frontend::busy::{Busy, BusyKind};
use norte_frontend::layout::{BySlot, SlotId};
use norte_proto::{Entry, Error, VPath};

use crate::app::{App, Modal, Trail, TypedSecret, error_message};
use crate::console::{Console, Waited};
use crate::fill::{Fill, release_refreshed_fill, spawn_fill};
use crate::jobs::SearchRun;
use crate::nav;
use crate::probes::{DecorateFetch, Probed};
use crate::trail::{rewind_for, rewind_trail};

/// Entradas de la PRIMERA página que un cd pinta antes de rellenar en
/// background (ADR 0017): con esto el primer render no espera al listado
/// entero (spec §11: primeras 100 en <16 ms aunque el dir tenga 500k).
pub const FIRST_PAGE: usize = 100;

/// Pane que un desenlace de `cd` acaba de ASENTAR (`Filling`/`Replaced`,
/// listado nuevo YA en `app.panes[pane]`), o `None` si el cd no tocó ningún
/// pane (`Failed`/`Cancelled`). NO consume `outcome` (préstamo): el llamante
/// aún necesita pasarlo a [`apply_cd`] justo después.
#[must_use]
pub fn cd_landed_pane(outcome: &Cd) -> Option<usize> {
    match outcome {
        Cd::Filling { pane, .. } | Cd::Replaced(pane) => Some(*pane),
        // Un refresh re-lista IN SITU (mismo dir, orden ya aplicado): no hay
        // aterrizaje que ordenar ni decoración nueva que pedir — paridad con
        // el camino de `on_tick`, que tampoco lo hace. Un `Swapped` tampoco
        // lista nada: los dos listados ya existían, solo cambiaron de lado
        // (sus decoraciones viajan con ellos en [`reconcile_swap`]).
        Cd::Refreshed(..) | Cd::Swapped | Cd::Failed(..) | Cd::Cancelled | Cd::Suspended => None,
    }
}

/// Desenlace de un `cd`, para que el run loop actualice el relleno vivo.
pub enum Cd {
    /// The pane was replaced and the REST of its listing fills in the
    /// background. The pane index rides ALONGSIDE the [`Fill`] and not inside
    /// it: the run loop files the fill by pane, and the index is that filing
    /// key, not a property of the drainer.
    Filling {
        /// Pane whose listing is filling.
        pane: usize,
        /// The drainer, headed for `fill[pane]`.
        fill: Fill,
    },
    /// El pane `usize` se reemplazó y ya está completo: un relleno anterior
    /// de ESE pane queda obsoleto y hay que soltarlo.
    Replaced(usize),
    /// El cd del pane `usize` FALLÓ al listar: el pane se quedó donde
    /// estaba (el error ya salió por la barra) sobre su listado ANTERIOR, así
    /// que un relleno previo de ese pane SIGUE siendo válido y se conserva
    /// (#78: soltarlo dejaba el pane colgado en `loading=true` —con
    /// «(parcial)» en la quick-search— sin drenador que lo apagara). El error
    /// VIAJA para quien navega desde el popup de historial (spec
    /// 2026-07-18: `NotFound` retira la entrada). Sin el índice de pane: al no
    /// tocar ya el relleno (#78) nadie lo consulta.
    Failed(Error),
    /// El cd se ABANDONÓ y nada lo reanuda: `Esc` durante el listado, `Ctrl-C`,
    /// o el stream de eventos muriéndose. Nada cambió y el relleno sigue.
    ///
    /// Distinto de [`Cd::Suspended`] a propósito: los dos dejan el pane donde
    /// estaba, pero solo uno de ellos va a volver. Quien recorre el rastro
    /// necesita saber cuál, y probar `app.modal` para averiguarlo adivina.
    Cancelled,
    /// El cd se PARÓ a medias y algo va a reanudar ESTA MISMA navegación: el
    /// modal TOFU (`Modal::TrustHostKey`), que carga el pane y el modo de
    /// rastro para que el reintento continúe donde esta se quedó.
    ///
    /// Para el relleno y para los panes es idéntico a [`Cd::Cancelled`] (el
    /// pane no se tocó); la diferencia la lee `rewind_for`, que NO rebobina
    /// un paso del rastro que el reintento va a terminar.
    Suspended,
    /// #118: `pane.refresh` (Ctrl+R) re-listó estos panes DESDE `dispatch`
    /// (que no ve `fill`/`last_probed`): el desenlace viaja al run loop para
    /// que [`apply_cd`] aplique el ritual post-refresh — mismo `[bool; 2]`
    /// que devuelve `refresh_panes` (`true` = listado completo asentado).
    Refreshed([bool; 2]),
    /// `pane.swap` cruzó los panes DESDE `dispatch`, que no ve el estado
    /// indexado por pane que vive en el run loop. El desenlace viaja para que
    /// [`reconcile_swap`] cruce también esa mitad — mismo patrón que
    /// `Refreshed`.
    Swapped,
}

/// El desenlace COMPLETO de un `cd`: el pane que aterrizó se reordena por el
/// esquema de su localización, se le piden las decoraciones de plugin y se
/// aplica el resultado ([`apply_cd`]: relleno paginado y sonda).
///
/// Estaba copiado en los NUEVE sitios del bucle de eventos que provocan un
/// cd —el resolver, la palette, el menú, el ratón, el árbol, el sidebar, el
/// selector de conexiones, el TOFU—. Lo que de verdad los distingue, y ahora
/// se lee en el call site porque es lo único que queda ahí, es si además
/// cosechan la búsqueda viva o lanzan el opener externo que el comando dejó
/// pendiente.
pub fn settle_cd(
    app: &mut App,
    backend: &Backend,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
    outcome: Cd,
) {
    if let Some(pane) = cd_landed_pane(&outcome) {
        app.apply_scheme_sort(pane);
        let dir = app.panes[pane].dir().clone();
        let paths: Vec<VPath> = app.panes[pane]
            .entries()
            .iter()
            .map(|e| e.path.clone())
            .collect();
        let plugin_cols = app.columns.plugin_ids_for(dir.scheme());
        decorate_fetch.set(
            app.panes.slot_of(pane),
            crate::probes::spawn_decorate_fetch(
                backend,
                app.panes.slot_of(pane),
                dir,
                paths,
                plugin_cols,
            ),
        );
    }
    apply_cd(
        &app.panes,
        fill,
        decorate_fetch,
        last_probed,
        search_run,
        outcome,
    );
}

/// Aplica el desenlace de un cd a los rellenos paginados en curso: uno nuevo
/// ocupa el hueco DE SU PANE (el rx anterior de ESE pane, dropeado, mata su
/// drenador → suelta el stream, regla 3); un REEMPLAZO del MISMO pane lo
/// suelta (su drenador drenaría el listado viejo sobre el nuevo); un FALLO o
/// un cd ABANDONADO no tocan el pane —sigue en su listado anterior, cuyo
/// relleno continúa siendo válido— así que no tocan el fill (#78). Un
/// `Refreshed` (#118) delega en [`release_refreshed_fill`]: el mismo ritual
/// que `after_panes_refresh`.
///
/// El hueco es POR PANE ([`Fill`]): un cd de un pane jamás estrangula el
/// relleno del otro.
///
/// `search_run` viaja hasta aquí SOLO por el brazo `Swapped`
/// ([`reconcile_swap`]): también está indexado por pane, y su cruce tiene que
/// pasar antes del `reap_search_run` que estos mismos call sites hacen a
/// continuación.
pub fn apply_cd(
    panes: &crate::panel::PaneSlots,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
    outcome: Cd,
) {
    match outcome {
        Cd::Filling { pane, fill: f } => {
            // Listado nuevo (lazy): la dedup de la sonda #52 caduca — la
            // misma entrada re-enfocada debe poder re-hidratarse.
            last_probed.clear();
            fill.insert(panes.slot_of(pane), f);
        }
        Cd::Replaced(pane) => {
            last_probed.clear();
            fill.remove(panes.slot_of(pane));
        }
        // El pane no cambió: su relleno (si lo había) sigue drenando el mismo
        // listado. Soltarlo aquí lo dejaba colgado en `loading=true` (#78).
        // `Suspended` (TOFU) va aquí por la misma razón que `Cancelled`: el
        // pane no se tocó, y encima el reintento lo va a re-listar entero.
        Cd::Failed(..) | Cd::Cancelled | Cd::Suspended => {}
        // #118: Ctrl+R desde `dispatch` — misma semántica que el ritual de
        // los otros disparadores (`after_panes_refresh`), un solo cuerpo.
        // `reap_search_run` no hace falta aquí: `refresh_panes` SALTA los
        // panes virtuales (jamás los saca del modo), así que no hay run de
        // búsqueda que cosechar por este camino.
        Cd::Refreshed(refreshed) => release_refreshed_fill(panes, &refreshed, fill, last_probed),
        // `pane.swap`: `App::swap_panes` ya cruzó panes e historiales; aquí
        // se cruza la mitad que vive en el run loop.
        Cd::Swapped => reconcile_swap(
            panes.slot_of(0),
            panes.slot_of(1),
            fill,
            decorate_fetch,
            last_probed,
            search_run,
        ),
    }
}

/// The other half of `pane.swap`: the per-pane state that lives in the run
/// loop rather than in `App`.
///
/// `App::swap_panes` moves the panes and their histories; these four are
/// indexed by pane too, and leaving any of them behind is a bug a green suite
/// does not catch — the listing keeps arriving, just into the wrong half of
/// the screen, the decorations land on somebody else's rows, and the live
/// search's hits pour into the pane the reader is not looking at.
///
/// The watcher needs nothing here: the run loop re-points it from
/// `watch_targets(app)` at the top of EVERY iteration, so the swapped
/// directories reach it on the next tick.
///
/// That rests on two facts, and only one of them is pinned.
/// `swap_tests::watch_targets_sigue_a_los_panes_tras_el_intercambio` proves
/// `watch_targets` is a pure function of `app.panes` — nobody caches a target
/// per side, which is the half that could rot silently. The other half, that
/// the `rewatch` call really is the first statement of the loop body, is
/// ordering no unit test in this file can observe: if someone moved it below
/// the key handling, each pane would watch the other's directory for one
/// tick after a swap. Read the call site before trusting this comment.
pub fn reconcile_swap(
    slot_a: SlotId,
    slot_b: SlotId,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
) {
    // Intercambiar paneles mueve el CONTENIDO entre huecos y deja los ids
    // donde estaban, así que el trabajo en vuelo tiene que viajar con su
    // listado. Cruzar los ids EN EL ÁRBOL en vez del contenido haría este
    // reconciliado innecesario entero — anotado en el plan de P6.
    fill.swap(slot_a, slot_b);
    // La búsqueda VIVA guarda su pane virtual como el relleno guardaba el suyo,
    // y aquí es donde TIENE que voltear: el mismo call site cosecha con
    // `reap_search_run` justo después de `apply_cd`, y esa cosecha mira
    // `panes[s.pane].virtual_search` — con el índice sin voltear ve el listado
    // ordinario que acaba de llegar del otro lado y cancela la Task en
    // silencio, dejando al otro pane con hits a medias en `Running` para
    // siempre. A lo sumo hay UN run (el pane virtual es uno), así que voltear
    // su índice es todo el cruce que necesita.
    if let Some(s) = search_run.as_mut() {
        s.pane ^= 1;
    }
    // Cada slot lleva su `dir` como guard anti-stale, así que cruzarlos basta:
    // el fetch sigue correspondiendo al listado que ahora está al otro lado.
    decorate_fetch.swap(slot_a, slot_b);
    // Es una caché de dedup de `stat`, no estado: traducir sus claves cuesta
    // más que volver a sondear, y un sondeo de más es invisible.
    last_probed.clear();
}

/// Listado COMPLETO de `dir` (para `refresh_panes` tras una mutación:
/// conserva el cursor por índice). Una entrada con error corta el listado —
/// mejor un error honesto que un listado silenciosamente incompleto. #54: NO
/// ordena aquí — `refresh_listing`/`PaneState::refill` normalizan
/// internamente, un sort manual sería trabajo duplicado. `attrs` (#117):
/// los ids attr configurados del scheme — pedidos en el `fs.list`; un id
/// no anunciado viene ausente (celda en blanco), jamás es error.
/// # Errors
///
/// Lo que devuelva el `Backend`: una entrada con error corta el listado, porque
/// un listado silenciosamente incompleto es peor que un error honesto.
pub async fn listing(
    backend: &Backend,
    dir: &VPath,
    attrs: &[String],
) -> Result<(Vec<Entry>, Option<u64>), Error> {
    let salida = backend.list_with_skipped_attrs(dir, attrs).await?;
    // Esto SÍ es una pantalla (#301): el ancla de lo que el humano acaba de
    // ver se retiene aquí y no en el embudo del backend, por donde también
    // pasan el árbol lateral y el `fs.list` de un script.
    backend.remember_listing_anchor(dir).await;
    Ok(salida)
}

/// Cachea en `App` las DOS mitades de una respuesta de `fs.capabilities`
/// (H3d): el catálogo de attrs (#117) y los flags de capacidad.
///
/// Una función y no dos líneas repetidas en el arranque y en el `cd` a
/// propósito: la respuesta trae ambas y guardarlas juntas es el punto entero
/// del cambio — quien tire una mitad aquí paga otra ronda de red por un dato
/// que ya estaba en el proceso, y ese descuido tiene ahora un test
/// (`caps_cache_tests`) en vez de vivir dentro del `select!` del run loop,
/// donde nada lo mira.
pub fn cache_capabilities(
    app: &mut App,
    dir: &VPath,
    (caps, catalog): (norte_proto::Capabilities, norte_proto::AttrCatalog),
) {
    app.insert_attr_catalog(dir.scheme().to_owned(), catalog);
    app.insert_caps(dir, caps);
}

/// Whether a cd to `dir` still has to ask `fs.capabilities`.
///
/// The two halves of that response are cached with DIFFERENT keys and the gate
/// has to ask about both, which is the whole reason this is a named function
/// and not an `is_none()` inline in [`cd_in`]. The attribute catalogue is per
/// scheme — a wrong column hint is cosmetic. The capability flags are per
/// DIRECTORY (`App::caps`, #215), because a veto that answers for the wrong
/// place is not cosmetic: gating on the catalogue alone meant the first `sftp`
/// host visited answered "is this read-only?" for every other host of the
/// session, and keying by connection meant `/home` answered for the exFAT
/// stick mounted under the same `file://`.
///
/// So it fetches when EITHER half is missing, and the redundant fetch — a
/// second location of a scheme whose catalogue is already cached — is one call
/// per directory, which is what asking a location about itself costs.
#[must_use]
pub fn needs_capabilities(app: &App, dir: &VPath) -> bool {
    app.attr_catalog(dir.scheme()).is_none() || app.caps(dir).is_none()
}

/// Lo que trae [`first_page`]: las entradas, el stream con el resto, las
/// omitidas del contenedor y las capacidades.
///
/// Tiene nombre desde que la espera es compartida: quien aterriza el resultado
/// lo recibe por parámetro, y una tupla de cuatro en una firma no se lee.
pub type PrimeraPagina = (
    Vec<Entry>,
    Option<EntryStream>,
    Option<u64>,
    Option<(norte_proto::Capabilities, norte_proto::AttrCatalog)>,
);

/// Primera página de `dir` (hasta [`FIRST_PAGE`]) más el stream con el RESTO
/// (o `None` si el dir cabía en la primera página) y las omitidas del
/// contenedor (#93). El primer render no espera al listado entero (ADR 0017).
/// Regla 7: el TUI no toca el FS. `attrs`/`fetch_caps` (#117): pide los
/// attrs configurados y, cuando el llamador dice que falta algo por cachear
/// ([`needs_capabilities`]), la respuesta de `fs.capabilities` (cuarto
/// elemento de la tupla).
///
/// H3d: ese cuarto elemento son las DOS mitades de `fs.capabilities` —
/// `Capabilities` y catálogo — porque el wire las trae juntas
/// (`Backend::capabilities_and_attrs`). La TUI cacheaba solo el catálogo y
/// luego preguntaba «¿es de solo lectura?» con otra ronda por un dato que ya
/// había llegado.
/// # Errors
///
/// Lo que devuelva el `Backend` al pedir la primera página o las capacidades.
/// Sin traducir: quien lo pinta necesita el tipo.
pub async fn first_page(
    backend: &Backend,
    dir: &VPath,
    attrs: &[String],
    fetch_caps: bool,
) -> Result<PrimeraPagina, Error> {
    // Las capacidades ANTES del stream (misma conexión, y solo cuando falta
    // algo por cachear — `needs_capabilities`); un fallo NO tumba el cd: sin
    // hints se pinta Opaque y el solo-lectura cae al criterio sintáctico.
    let catalog = if fetch_caps {
        backend.capabilities_and_attrs(dir).await.ok()
    } else {
        None
    };
    let (mut stream, skipped) = backend.list_stream_with(dir, attrs).await?;
    // El cd de un panel es una pantalla: se retiene el ancla (#301).
    backend.remember_listing_anchor(dir).await;
    let mut first = Vec::with_capacity(FIRST_PAGE);
    while first.len() < FIRST_PAGE {
        match stream.next().await {
            Some(item) => first.push(item?),
            // El dir cabía en la primera página: no hay resto que drenar.
            None => return Ok((first, None, skipped, catalog)),
        }
    }
    Ok((first, Some(stream), skipped, catalog))
}

/// cd CANCELABLE (regla 3): el listado corre contra el stream de eventos —
/// Esc lo abandona (el pane se queda donde estaba) y Ctrl-C sale del TUI
/// (atajos FIJOS durante un cd: aquí no aplica el keymap — son la salida de
/// emergencia y no deben ser remapeables a algo que no exista). Soltar el
/// future del listado detiene al productor del provider (testeado en
/// vfs-local). El resto de teclas se descartan mientras dura el cd.
pub async fn cd(app: &mut App, backend: &Backend, events: &mut Console<'_>, dir: VPath) -> Cd {
    cd_in(app, backend, events, app.focus(), dir, Trail::Record).await
}

/// The ONE place that decides whether a navigation joins the pane's trail.
///
/// Two conditions, both load-bearing, both easy to lose in the middle of the
/// success arm of [`cd_in`] where they used to live:
///
/// - `prev != dir`: a cd onto the directory the pane is ALREADY showing (a
///   refresh-like navigation) is not a step the reader took. Recording it
///   would make the next `nav.back` do nothing visible. The MRU's consecutive
///   dedup covers the rest of the redundancies.
/// - `trail == Trail::Record`: a `Trail::Replay` is the trail walking ITSELF.
///   Recording there feeds the trail its own steps — going back from B to A
///   would log "I was at B", so the next `nav.back` returns to B and the
///   reader oscillates between two directories forever. This is the single
///   line that stops `nav.back` from doing that, and it is pinned by
///   `record_step_tests::un_replay_no_alimenta_el_rastro`.
pub fn record_step(h: &mut nav::History, prev: &VPath, dir: &VPath, trail: Trail) {
    if prev != dir && trail == Trail::Record {
        h.record(prev.clone());
    }
}

/// Navega `pane` — que NO tiene por qué ser el enfocado, porque
/// `pane.mirror` manda el OTRO pane a un sitio mientras el foco se queda
/// quieto. `trail` dice si el movimiento se REGISTRA en el rastro del pane o
/// es el rastro reproduciéndose ([`Trail`]).
///
/// Todo lo que aquí toca estado de pane va por el PARÁMETRO `pane`
/// (`app.panes[pane]`), jamás por `app.focused()`: son la misma cosa solo
/// mientras el llamante sea el envoltorio [`cd`].
pub async fn cd_in(
    app: &mut App,
    backend: &Backend,
    events: &mut Console<'_>,
    pane: usize,
    dir: VPath,
    trail: Trail,
) -> Cd {
    // Historial (spec 2026-07-18): el dir ANTERIOR se captura AQUÍ y se
    // empuja solo en el brazo de ÉXITO (el pane se reemplazó de verdad).
    // Al vivir dentro de `cd_in` cubre TODOS los caminos que navegan —
    // nav.enter/nav.parent, quick-Enter (dispatch nav.enter), retry TOFU y
    // los popups de historial/hotlist — sin repetirlo por call-site.
    let prev = app.panes[pane].dir().clone();
    // #117: los attrs CONFIGURADOS del scheme de destino se piden en el
    // listado; el catálogo del provider se trae UNA vez por scheme y sesión
    // (cache en `App::attr_catalogs` — hints y cabeceras del render), y las
    // caps una vez por CONEXIÓN (ver `needs_capabilities`).
    let attrs = app.columns.attr_ids_for(dir.scheme());
    let fetch_caps = needs_capabilities(app, &dir);
    // Lo que se está esperando, para que las superficies puedan DECIRLO. Un
    // destino remoto es `Connecting` y uno local `Listing`: el verbo del
    // primer contacto con un bucket no es «listando», y el lector que ve
    // «conectando…» sabe que lo que puede tardar es la red y no su disco.
    // `Busy` no se pinta hasta cruzar su umbral, así que un cd local —el 99 %—
    // no llega a enseñar nada (`norte_frontend::busy`).
    let started = std::time::Instant::now();
    app.busy = Some(Busy::new(
        if is_local(&dir) {
            BusyKind::Listing
        } else {
            BusyKind::Connecting
        },
        // El VPath CRUDO: el badge de nombre alterado, la reinterpretación de
        // codificación del panel y el ancho disponible solo los sabe quien
        // pinta, y renderizar aquí los perdía los tres a la vez.
        Some(dir.clone()),
        Some(pane),
    ));
    let out = match crate::console::wait_painting(
        events,
        app,
        started,
        first_page(backend, &dir, &attrs, fetch_caps),
    )
    .await
    {
        Waited::Done(res) => aterrizar(app, pane, dir, trail, &prev, res),
        Waited::Cancelled => Cd::Cancelled,
        Waited::Quit => {
            app.quit = true;
            Cd::Cancelled
        }
    };
    // La espera acabó, salga como salga: cancelada, fallida o buena. Dejar el
    // indicador puesto sería el spinner que no avanza nunca. Y por si algún
    // camino futuro se saltara esta línea, `turn::drain_pending` lo limpia
    // también en la cabecera de cada vuelta: ninguna espera sobrevive a una.
    app.busy = None;
    out
}

/// Lo que NO es local es algo con lo que hay que CONECTAR.
///
/// La authority y no el scheme: `file://` sin authority es el disco de aquí, y
/// también lo es un archivo abierto sobre él (`tar+file://…`). Decir
/// «conectando…» al abrir un zip local sería justo la deshonestidad que este
/// indicador promete no cometer.
fn is_local(dir: &VPath) -> bool {
    dir.authority().is_none()
}

/// Qué hacer con lo que llegó: reemplazar el pane, abrir el modal TOFU, o
/// dejar el error en la barra.
///
/// Sale del `select!` porque ya no hay `select!`: la espera es
/// [`crate::console::wait_painting`], compartida con las otras dos esperas
/// largas del TUI, y esto es lo único que era propio de una navegación.
fn aterrizar(
    app: &mut App,
    pane: usize,
    dir: VPath,
    trail: Trail,
    prev: &VPath,
    res: Result<PrimeraPagina, Error>,
) -> Cd {
    match res {
        Ok((first, stream, skipped, catalog)) => {
            // #117: el catálogo recién llegado se cachea por scheme — los
            // frames siguientes ya pintan con hints. H3d: y las caps de la
            // MISMA respuesta, que es lo que responde «¿este pane es de solo
            // lectura?» sin otra ronda (`App::pane_read_only`).
            if let Some(both) = catalog {
                cache_capabilities(app, &dir, both);
            }
            // #54: NO ordenamos aquí — `begin_listing` -> `set_listing`
            // normaliza internamente.
            let more = stream.is_some();
            app.panes[pane].begin_listing(dir.clone(), first, more, skipped);
            record_step(&mut app.history[pane], prev, &dir, trail);
            // Si queda stream, un drenador lo rellena en background.
            match stream {
                Some(s) => Cd::Filling {
                    pane,
                    fill: spawn_fill(s),
                },
                None => Cd::Replaced(pane),
            }
        }
        // Primer contacto TOFU (#45): en vez de una línea de error con la
        // huella, abre el modal de confianza — `y` confía y REINTENTA esta
        // misma navegación.
        Err(Error::HostKeyUnknown {
            host,
            port,
            algo,
            fingerprint,
        }) => {
            // El modal CARGA `pane` y `trail`: el reintento debe reanudar ESTA
            // navegación (este pane, este modo de rastro), no una nueva contra
            // el foco de entonces.
            app.modal = Some(Modal::TrustHostKey {
                host,
                port,
                algo,
                fingerprint,
                dir,
                pane,
                trail,
            });
            // El pane NO se tocó (solo se abrió el modal): como `Cancelled`,
            // conserva un relleno en vuelo del listado anterior, que sigue
            // siendo válido. Pero SUSPENDED y no `Cancelled`: esta navegación
            // va a CONTINUAR en el retry del modal, y quien recorre el rastro
            // tiene que distinguirla de un cd abandonado, que no vuelve.
            Cd::Suspended
        }
        // #325: la entrada dice `secret = "prompt"` y ninguna de las tres
        // fuentes lo tiene. Mismo trato que el TOFU —y por las mismas
        // razones—: el modal carga `pane` y `trail`, y el Enter reintenta
        // ESTA navegación.
        Err(Error::SecretNeeded { conn, endpoint }) => {
            app.modal = Some(Modal::AskSecret {
                conn,
                endpoint,
                input: TypedSecret::default(),
                dir,
                pane,
                trail,
            });
            Cd::Suspended
        }
        // Un error de listado NO tumba el TUI: el pane se queda, pero un
        // relleno previo de ESTE pane ya no aplica. El error se PORTA en el
        // desenlace (popup de historial).
        Err(e) => {
            app.message = Some(error_message(&e));
            Cd::Failed(e)
        }
    }
}

/// Confiar en la host key y REINTENTAR la navegación que el TOFU interrumpió
/// (#45). `Some(cd)` = el desenlace debe volver YA al caller (la pendiente
/// siguiente ya se gestionó aquí); `None` = confiar falló y el mensaje quedó
/// en la barra — el caller sigue por su camino común.
///
/// Vive fuera de [`crate::mutations::on_dialog_key`] porque el brazo entero (destructurar el
/// modal + el `trust_host_key` + el reintento) no cabe en el presupuesto de
/// líneas de esa función.
pub async fn trust_host_retry(
    app: &mut App,
    backend: &Backend,
    events: &mut Console<'_>,
    modal: Modal,
) -> Option<Cd> {
    let Modal::TrustHostKey {
        host,
        port,
        algo,
        fingerprint,
        dir,
        pane,
        trail,
    } = modal
    else {
        // El caller solo llama con este modal (brazo `Modal::TrustHostKey`).
        return None;
    };
    match backend
        .trust_host_key(&host, port, &algo, &fingerprint)
        .await
    {
        Ok(()) => {
            // El engine re-verifica el fingerprint contra la clave que el
            // host presenta AHORA (anti-TOCTOU, ADR 0015 D); si aún falla,
            // el retry lo mostrará.
            //
            // `cd_in` (no `cd`): se reanuda la navegación que el TOFU
            // interrumpió — su pane y su rastro —, que no tiene por qué ser
            // la del foco actual.
            let outcome = cd_in(app, backend, events, pane, dir.clone(), trail).await;
            // Y si era un paso del rastro, ESTE es el sitio donde se termina:
            // `walk_trail` lo dejó dado porque contaba con este reintento.
            settle_suspended_trail(app, pane, &dir, trail, &outcome);
            // Solo abrir la siguiente pendiente si el retry NO dejó un modal
            // (otro HostKeyUnknown): jamás pisar.
            if app.modal.is_none() {
                app.open_next_pending();
            }
            Some(outcome)
        }
        Err(e) => {
            app.message = Some(error_message(&e));
            // Confiar FALLÓ: no hay reintento, así que la navegación que el
            // TOFU suspendió muere aquí — para el rastro es idéntica a un cd
            // abandonado, y el paso tiene que volver.
            settle_suspended_trail(app, pane, &dir, trail, &Cd::Cancelled);
            None
        }
    }
}

/// Entregar el secreto tecleado y REINTENTAR la navegación que
/// `SecretNeeded` interrumpió (#325). Gemelo de [`trust_host_retry`], con el
/// mismo contrato de retorno: `Some(cd)` = el desenlace vuelve YA al caller,
/// `None` = entregarlo falló y el mensaje quedó en la barra.
///
/// Vive aquí por el mismo motivo que su gemelo: destructurar el modal, la
/// llamada y el reintento no caben en el presupuesto de líneas de
/// [`crate::mutations::on_dialog_key`].
pub async fn provide_secret_retry(
    app: &mut App,
    backend: &Backend,
    events: &mut Console<'_>,
    modal: Modal,
) -> Option<Cd> {
    let Modal::AskSecret {
        conn,
        input,
        dir,
        pane,
        trail,
        ..
    } = modal
    else {
        // El caller solo llama con este modal (brazo `Modal::AskSecret`).
        return None;
    };
    // El campo vacío ni siquiera llega aquí: `dialog_action` deja INERTE el
    // confirmar de este modal mientras no haya nada tecleado, así que no hace
    // falta repetir el guard — y repetirlo escondería que la decisión vive
    // allí, junto al resto de la semántica de seguridad de los diálogos.
    match backend.provide_secret(&conn, input.expose()).await {
        Ok(()) => {
            // El secreto ya está en el core; a partir de aquí es idéntico al
            // TOFU. `cd_in` (no `cd`): se reanuda la navegación que el error
            // interrumpió — su pane y su rastro.
            let outcome = cd_in(app, backend, events, pane, dir.clone(), trail).await;
            settle_suspended_trail(app, pane, &dir, trail, &outcome);
            // El reintento puede abrir OTRO modal (un TOFU sobre el mismo
            // host, o un `SecretNeeded` de otra conexión): jamás pisarlo.
            if app.modal.is_none() {
                app.open_next_pending();
            }
            Some(outcome)
        }
        Err(e) => {
            app.message = Some(error_message(&e));
            // Entregarlo FALLÓ: no hay reintento, así que la navegación que
            // el error suspendió muere aquí — para el rastro es idéntica a un
            // cd abandonado, y el paso tiene que volver.
            settle_suspended_trail(app, pane, &dir, trail, &Cd::Cancelled);
            None
        }
    }
}

/// Enter sobre un hit del modal semántico (M4-IA-2): cd al PADRE del hit y
/// deja el cursor sobre él por path (molde [`crate::jobs::on_search_enter`]; si cayó en
/// una página aún no drenada, el cursor se queda arriba, v1). Devuelve el
/// `Cd` para que el caller lo aplique (`apply_cd` + decorate);
/// `Cd::Cancelled` = nada que navegar (hits vacíos defensivo o hit raíz sin
/// padre).
pub async fn semantic_hit_cd(
    app: &mut App,
    backend: &Backend,
    events: &mut Console<'_>,
    hits: &[norte_proto::methods::SemanticHit],
    cursor: usize,
) -> Cd {
    let Some(hit) = hits.get(cursor).map(|h| h.path.clone()) else {
        return Cd::Cancelled;
    };
    let Some(parent) = hit.parent() else {
        return Cd::Cancelled;
    };
    let pane = app.focus();
    let outcome = cd(app, backend, events, parent).await;
    if let Some(i) = app.panes[pane].entries().iter().position(|e| e.path == hit) {
        app.panes[pane].set_cursor(i);
    }
    outcome
}

/// Settles the trail of a navigation the TOFU prompt SUSPENDED, once that
/// prompt has been answered.
///
/// [`crate::trail::walk_trail`] deliberately leaves a `Cd::Suspended` step taken: the retry
/// was going to finish it. But the retry is not guaranteed to happen — the
/// reader can deny the key, trusting it can fail, and the retry itself can
/// fail or be abandoned — and when it does not, the step is left standing for
/// a move that never occurred. That is the same lie [`rewind_for`] exists to
/// stop, on the one path where the navigation OUTLIVES the function that
/// started it, which is why nobody was there to undo it.
///
/// Runs the answer's outcome through the very same [`rewind_for`] the trail
/// walker runs. `trail.step()` of `None` (a `Trail::Record` navigation: a
/// plain cd that happened to meet an unknown host) means there is no step to
/// rewind, so this is a no-op — and a second `Suspended` (another unknown
/// key, or the same one asked again) is a no-op TOO: the modal is open again
/// carrying the same trail, so the step is still going to be settled by
/// whoever answers THAT one.
///
/// Rewinding here cannot double up with [`crate::trail::walk_trail`]: the walker saw
/// `Suspended` and did nothing, so this is the FIRST and only rewind of that
/// step.
pub fn settle_suspended_trail(app: &mut App, pane: usize, dir: &VPath, trail: Trail, outcome: &Cd) {
    if let Some(step) = trail.step() {
        rewind_trail(app, pane, step, dir, rewind_for(outcome));
    }
}
