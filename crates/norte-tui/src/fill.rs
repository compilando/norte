//! El canal por el que un listado paginado va llegando, uno por HUECO.
//!
//! Un dir de 100k entradas no se lista de una vez: el proveedor lo va dando y
//! un drenador coalesce lotes de [`FILL_BATCH`] —o parciales cada
//! [`FILL_INTERVAL`], para que un listado remoto lento se vea avanzar— y los
//! manda por este canal. [`apply_fill_msg`] es quien los aplica.
//!
//! Un `Fill` por hueco y no uno global: con un solo hueco, cualquier `cd` que
//! empieza un listado nuevo dejaba colgado a quien estuviera drenando — y
//! `pane.mirror` es UNA tecla sin cambio de foco, así que el pane que se queda
//! a medias bajo un «cargando…» permanente es justo el que se está mirando.
//!
//! Vivía en el root del binario `ntc`, un crate DISTINTO de esta lib, repartido
//! en cuatro sitios: las dos constantes arriba, los dos tipos en medio, y
//! `spawn_fill` a diez mil líneas de distancia.

use futures::StreamExt as _;
use norte_core::backend::EntryStream;
use norte_frontend::layout::{BySlot, SlotId};
use norte_i18n::t;
use norte_proto::Entry;

use crate::app::App;
use crate::probes::Probed;

/// Lote que el drenador coalesce antes de enviar (evita un re-sort por
/// entrada; el re-sort completo lo hace [`crate::app::Pane::extend_listing`]). Un dir de
/// 100k son ~24 lotes ⇒ ~24 re-sorts de tamaño creciente durante el fill; el
/// merge incremental (claves persistidas) es la optimización diferida a issue.
pub const FILL_BATCH: usize = 4096;
/// El drenador vacía un lote PARCIAL cada tanto (además de al llenarlo): en un
/// listado remoto lento (páginas por RTT) el usuario ve progreso y el
/// contador `cargando… (n)` avanza en vez de saltar de 4096 en 4096.
pub const FILL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Mensaje del drenador de un listado paginado al run loop.
pub enum FillMsg {
    /// Un lote más de entradas para el pane.
    Batch(Vec<Entry>),
    /// El listado se cortó a mitad (error del provider/daemon): no es
    /// silencioso (la UI avisa y limpia el `loading`).
    Failed,
}

/// A listing being FILLED in the background: the drainer's channel, nothing
/// else. Dropping it drops the `rx` → the drainer dies on its next send →
/// releases the stream → cooperative cancellation (rule 3).
///
/// It does NOT carry its pane. The run loop keeps `[Option<Fill>; 2]`, one
/// slot per pane, so the array index IS the pane — the same shape as
/// `decorate_fetch`. A `pane` field beside it would be a second source of the
/// same fact, and two sources of one fact drift; a `pane.swap` would then have
/// to keep them in step by hand instead of just swapping the two slots.
///
/// One slot per pane is also the whole point rather than a tidiness: with a
/// single global slot, any cd that starts a new paginated listing dropped
/// whoever was draining — and `pane.mirror` makes that ONE keystroke with no
/// change of focus, so the pane left half-listed under a permanent
/// «cargando…» is the one the reader is looking at.
pub struct Fill {
    /// Los lotes que el drenador va coalesciendo. Soltar el `Fill` cierra el
    /// canal, que es cómo se cancela un listado a medias (regla 3).
    pub rx: tokio::sync::mpsc::Receiver<FillMsg>,
}

/// Núcleo del ritual post-refresh (#117 review, #118): suelta el drenador
/// paginado SOLO si su pane fue re-listado de verdad (soltarlo a ciegas tras
/// un Esc a medias dejaría el pane colgado en `loading` para siempre, #78) e
/// invalida la dedup de la sonda #52 (un listado nuevo re-lazifica las
/// entries). Cuerpo ÚNICO para `after_panes_refresh` (run loop) y el brazo
/// `Cd::Refreshed` de `apply_cd` (Ctrl+R vía `dispatch`).
pub fn release_refreshed_fill(
    panes: &crate::panel::PaneSlots,
    refreshed: &[bool],
    fill: &mut BySlot<Fill>,
    last_probed: &mut Probed,
) {
    if !refreshed.iter().any(|r| *r) {
        return;
    }
    for (pane, _) in refreshed.iter().enumerate().filter(|(_, r)| **r) {
        fill.remove(panes.slot_of(pane));
    }
    last_probed.clear();
}

/// Aplica un mensaje del drenador de paginación (ADR 0017) al pane. Si el pane
/// pasó a modo virtual de búsqueda (Alt+F7 sobre un dir aún paginándose,
/// review MAJOR T6), el fill quedó OBSOLETO —`begin_search` vació las
/// entries— y su drenador alimentaría el listado REAL como si fueran hits (el
/// propio root de la búsqueda colándose entre resultados): se suelta el fill y
/// se DESCARTA el lote. Cinturón simétrico al drain-guard de `drain_search`;
/// el tirante es soltar el fill en `launch_search`.
pub fn apply_fill_msg(app: &mut App, fill: &mut BySlot<Fill>, slot: SlotId, msg: Option<FillMsg>) {
    // El lote va a SU hueco, no a una posición. Si ese hueco ya no existe
    // —se cerró el panel, se cerró la pestaña— el lote se TIRA: aplicarlo a
    // quien ocupe ahora esa posición sería pintar en un listado las entradas
    // de otro directorio, y nada lo diría.
    let Some(pane) = app.panes.browser_mut(slot) else {
        fill.remove(slot);
        return;
    };
    if pane.virtual_search {
        fill.remove(slot);
        return;
    }
    match msg {
        Some(FillMsg::Batch(batch)) => pane.extend_listing(batch),
        Some(FillMsg::Failed) => {
            pane.finish_listing();
            app.message = Some(t("msg-list-incomplete"));
            fill.remove(slot);
        }
        None => {
            pane.finish_listing();
            fill.remove(slot);
        }
    }
}

/// Arranca el drenador del RESTO del listado: envía lotes coalescidos al run
/// loop, que los aplica con [`crate::app::Pane::extend_listing`]. Soltar el `rx` (un cd
/// nuevo DEL MISMO PANE) mata el drenador en su próximo envío → suelta el
/// stream (regla 3). Sin `pane`: quién lo recibe lo decide el hueco donde el
/// run loop lo archive (ver [`Fill`]).
#[must_use]
pub fn spawn_fill(mut stream: EntryStream) -> Fill {
    // Bounded a 1: el drenador no corre por delante del run loop más de un
    // lote (backpressure); el pico de memoria es un lote, no todo el dir.
    let (tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
    tokio::spawn(async move {
        let mut batch = Vec::with_capacity(FILL_BATCH);
        let mut flush = tokio::time::interval(FILL_INTERVAL);
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        flush.tick().await; // consume el tick inmediato del interval
        loop {
            tokio::select! {
                item = stream.next() => match item {
                    Some(Ok(e)) => {
                        batch.push(e);
                        if batch.len() >= FILL_BATCH
                            && tx
                                .send(FillMsg::Batch(std::mem::take(&mut batch)))
                                .await
                                .is_err()
                        {
                            return; // el run loop soltó el rx (cd nuevo)
                        }
                    }
                    Some(Err(_)) => {
                        let _ = tx.send(FillMsg::Failed).await;
                        return;
                    }
                    None => {
                        if !batch.is_empty() {
                            let _ = tx.send(FillMsg::Batch(batch)).await;
                        }
                        return; // fin: drop(tx) cierra el canal → finish_listing
                    }
                },
                _ = flush.tick() => {
                    // Vacía un lote PARCIAL (progreso en streams lentos).
                    if !batch.is_empty()
                        && tx
                            .send(FillMsg::Batch(std::mem::take(&mut batch)))
                            .await
                            .is_err()
                    {
                        return;
                    }
                }
            }
        }
    });
    Fill { rx }
}
