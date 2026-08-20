//! El único escritor del estado.
//!
//! Todo lo que puede cambiar la pantalla —una acción del renderer, una
//! respuesta del daemon, un temporizador— entra por UN buzón acotado y lo
//! aplica UNA task. No hay locks que cruzar, y por tanto no hay lock que se
//! quede tomado durante una llamada al backend: lo que hay es una cola.
//!
//! De ahí salen las dos garantías que el bridge promete y el renderer
//! necesita: las acciones se aplican EN ORDEN, y la secuencia de
//! actualizaciones no salta. Cuando un suscriptor no drena y se queda atrás,
//! no se le acumulan parches: se le manda un snapshot y vuelve a estar al
//! día (ADR 0066).

use std::collections::BTreeSet;
use std::sync::Arc;

use norte_proto::{Entry, EntryKind, VPath};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::action::UiAction;
use crate::backend::HostBackend;
use crate::bridge::{
    ActionAck, BridgeEnvelope, InstanceId, MAX_ROWS_PER_BATCH, RowKey, StaleAction, clamp_display,
};
use crate::dto::{
    BrowserSlotView, ConnectionView, RowKind, RowView, SlotState, SlotView, StatusView, UiNotice,
    UiUpdate, ViewChange, ViewPatch, ViewSnapshot,
};

/// Capacidad del buzón del actor. Acotado a propósito: si el renderer manda
/// más rápido de lo que el host aplica, se le hace esperar — jamás se crece
/// sin límite.
const INBOX: usize = 256;

/// Actualizaciones retenidas para un suscriptor lento. Al pasarse, el
/// suscriptor se entera de que se quedó atrás y pide un snapshot: es la
/// recuperación barata, y la que no gasta memoria del host.
const UPDATE_BUFFER: usize = 64;

/// Cómo arrancar el host.
pub struct UiHostOptions {
    /// Con quién habla.
    pub backend: Arc<dyn HostBackend>,
    /// Dónde empieza el listado.
    pub initial_dir: VPath,
    /// Idioma ya negociado, para que el renderer pida su catálogo.
    pub locale: String,
}

/// Lo que un suscriptor recibe.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// Una actualización en su sobre.
    Message(Box<BridgeEnvelope<UiUpdate>>),
    /// Este suscriptor se quedó atrás y se perdió actualizaciones. Lo que
    /// tiene que hacer es pedir un snapshot ([`UiAction::Resync`]), no
    /// intentar deducir lo que faltó.
    Lagged,
}

/// Suscripción a las actualizaciones del host.
pub struct UiSubscription {
    rx: broadcast::Receiver<BridgeEnvelope<UiUpdate>>,
}

impl UiSubscription {
    /// La siguiente actualización, o `None` si el host se apagó.
    pub async fn recv(&mut self) -> Option<Update> {
        match self.rx.recv().await {
            Ok(m) => Some(Update::Message(Box::new(m))),
            Err(broadcast::error::RecvError::Lagged(_)) => Some(Update::Lagged),
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }
}

/// Lo que quedó sin terminar al apagar.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShutdownReport {
    /// Había trabajo vivo al apagar. Se DICE: apagar en silencio con una
    /// copia a medias es como se pierde una operación sin que nadie lo sepa.
    pub incomplete: bool,
}

/// El host: un asa barata de clonar sobre el único escritor.
#[derive(Clone)]
pub struct UiHost {
    inbox: mpsc::Sender<Mensaje>,
    updates: broadcast::Sender<BridgeEnvelope<UiUpdate>>,
    instance: InstanceId,
}

enum Mensaje {
    Accion(Box<UiAction>, oneshot::Sender<ActionAck>),
    Apagar(oneshot::Sender<ShutdownReport>),
}

impl UiHost {
    /// Arranca el host y devuelve su PRIMER snapshot.
    ///
    /// El snapshot inicial es la secuencia 0 y hay exactamente uno: un
    /// renderer que arranca no tiene que preguntar por el estado, ya lo
    /// tiene.
    ///
    /// # Errors
    /// Hoy ninguno; la firma lo reserva para cuando arrancar hable con el
    /// daemon (sesión, capacidades) en la tarea 2.5.
    pub async fn start(options: UiHostOptions) -> Result<(Self, ViewSnapshot), UiError> {
        let instance = InstanceId::new(nueva_instancia());
        let (updates, _) = broadcast::channel(UPDATE_BUFFER);
        let (tx, rx) = mpsc::channel(INBOX);

        let mut estado = Estado::nuevo(instance.clone(), options.locale, options.initial_dir);
        // El primer listado se pide ANTES de publicar nada: el snapshot 0
        // describe una pantalla que ya existe, no una promesa.
        estado.listar(options.backend.as_ref()).await;
        let primero = estado.snapshot();

        let host = Self {
            inbox: tx,
            updates: updates.clone(),
            instance,
        };
        tokio::spawn(actor(rx, estado, options.backend, updates));
        Ok((host, primero))
    }

    /// La identidad de esta instancia. Toda acción que no la lleve es de otra
    /// vida del host.
    #[must_use]
    pub fn instance(&self) -> &InstanceId {
        &self.instance
    }

    /// Manda una acción y espera su acuse.
    ///
    /// # Errors
    /// [`UiError::Down`] si el host ya no está.
    pub async fn dispatch(&self, action: UiAction) -> Result<ActionAck, UiError> {
        let (tx, rx) = oneshot::channel();
        self.inbox
            .send(Mensaje::Accion(Box::new(action), tx))
            .await
            .map_err(|_| UiError::Down)?;
        rx.await.map_err(|_| UiError::Down)
    }

    /// Se engancha a las actualizaciones. Varios suscriptores son legales; el
    /// escritor sigue siendo uno.
    #[must_use]
    pub fn subscribe(&self) -> UiSubscription {
        UiSubscription {
            rx: self.updates.subscribe(),
        }
    }

    /// Apaga el host y cuenta qué quedó sin terminar.
    ///
    /// # Errors
    /// [`UiError::Down`] si ya estaba apagado.
    pub async fn shutdown(&self) -> Result<ShutdownReport, UiError> {
        let (tx, rx) = oneshot::channel();
        self.inbox
            .send(Mensaje::Apagar(tx))
            .await
            .map_err(|_| UiError::Down)?;
        rx.await.map_err(|_| UiError::Down)
    }
}

/// Lo que puede fallar al hablar con el host.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UiError {
    /// El host se apagó (o se cayó): su estado ya no existe.
    #[error("el host no está")]
    Down,
}

/// El bucle del ÚNICO escritor.
async fn actor(
    mut rx: mpsc::Receiver<Mensaje>,
    mut estado: Estado,
    _backend: Arc<dyn HostBackend>,
    updates: broadcast::Sender<BridgeEnvelope<UiUpdate>>,
) {
    while let Some(msg) = rx.recv().await {
        match msg {
            Mensaje::Accion(accion, responde) => {
                let (ack, salidas) = estado.aplicar(&accion);
                for u in salidas {
                    // Sin suscriptores no es un error: el host sigue vivo
                    // aunque el renderer se haya ido a hacer otra cosa.
                    let _ = updates.send(u);
                }
                let _ = responde.send(ack);
            }
            Mensaje::Apagar(responde) => {
                let informe = Estado::apagar();
                let _ = updates.send(estado.sobre(UiUpdate::Notice(UiNotice::Shutdown {
                    incomplete: informe.incomplete,
                })));
                let _ = responde.send(informe);
                return;
            }
        }
    }
}

/// Identidad única de una instancia: pid más el instante de arranque. No
/// necesita ser impredecible —no autoriza nada—, solo distinta de la de la
/// vida anterior del proceso.
fn nueva_instancia() -> String {
    let ahora = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("host-{}-{ahora}", std::process::id())
}

/// Un listado abierto en un hueco.
struct Hueco {
    id: u32,
    generacion: u64,
    dir: VPath,
    entradas: Vec<Entry>,
    cursor: usize,
    marcas: BTreeSet<usize>,
    primera_visible: u64,
    visibles: u32,
    estado: SlotState,
}

/// El estado semántico. Solo el actor lo toca.
struct Estado {
    instance: InstanceId,
    sequence: u64,
    locale: String,
    hueco: Hueco,
    status: StatusView,
    conexion: ConnectionView,
}

impl Estado {
    fn nuevo(instance: InstanceId, locale: String, dir: VPath) -> Self {
        Self {
            instance,
            sequence: 0,
            locale,
            hueco: Hueco {
                id: 1,
                generacion: 0,
                dir,
                entradas: Vec::new(),
                cursor: 0,
                marcas: BTreeSet::new(),
                primera_visible: 0,
                visibles: 64,
                estado: SlotState::Loading,
            },
            status: StatusView::default(),
            conexion: ConnectionView::Connected,
        }
    }

    /// Pide el listado y lo deja ORDENADO con el mismo comparador que el
    /// resto de frontends: dos superficies que ordenan distinto se leen como
    /// si dijeran cosas distintas.
    async fn listar(&mut self, backend: &dyn HostBackend) {
        let dir = self.hueco.dir.clone();
        match backend.list(dir).await {
            Ok(mut entradas) => {
                norte_frontend::sort_entries(&mut entradas);
                self.hueco.entradas = entradas;
                self.hueco.cursor = 0;
                self.hueco.marcas.clear();
                self.hueco.generacion += 1;
                self.hueco.estado = SlotState::Ready;
            }
            Err(e) => {
                self.hueco.entradas.clear();
                self.hueco.estado = SlotState::Error {
                    reason_key: norte_frontend::error::error_key(&e).to_owned(),
                    detail: None,
                };
            }
        }
    }

    /// Aplica una acción y devuelve su acuse más lo que haya que publicar.
    ///
    /// Síncrona MIENTRAS ninguna acción hable con el daemon. En cuanto entre
    /// la navegación (tarea 2.3) volverá a ser `async`, y el contrato que hay
    /// que conservar entonces es el de ahora: el actor sigue siendo el único
    /// escritor, así que aquí no puede quedarse tomado ningún lock mientras
    /// se espera al backend — no hay ninguno que tomar.
    fn aplicar(&mut self, accion: &UiAction) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match accion {
            UiAction::MoveCursor { slot_id, delta } => {
                let (slot_id, delta) = (*slot_id, *delta);
                if slot_id != self.hueco.id {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                let n = self.hueco.entradas.len();
                if n == 0 {
                    return (self.aplicada(), Vec::new());
                }
                let actual = i128::try_from(self.hueco.cursor).unwrap_or(0);
                let destino =
                    (actual + i128::from(delta)).clamp(0, i128::try_from(n - 1).unwrap_or(0));
                self.hueco.cursor = usize::try_from(destino).unwrap_or(0);
                let cambio = ViewChange::Cursor {
                    slot_id: self.hueco.id,
                    generation: self.hueco.generacion,
                    cursor: Some(RowKey(self.hueco.cursor as u64)),
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            UiAction::SelectRow { slot_id, key } | UiAction::ToggleMark { slot_id, key } => {
                let (slot_id, key) = (*slot_id, *key);
                let marcar = matches!(accion, UiAction::ToggleMark { .. });
                if slot_id != self.hueco.id {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                let Ok(i) = usize::try_from(key.0) else {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                };
                if i >= self.hueco.entradas.len() {
                    // Una fila que ya no existe: el listado cambió bajo el
                    // click. Ni se interpreta ni es un error.
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                if marcar {
                    if !self.hueco.marcas.remove(&i) {
                        self.hueco.marcas.insert(i);
                    }
                } else {
                    self.hueco.cursor = i;
                }
                let filas = self.filas_visibles();
                let cambio = ViewChange::Rows {
                    slot_id: self.hueco.id,
                    generation: self.hueco.generacion,
                    first_visible: self.hueco.primera_visible,
                    rows: filas,
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            UiAction::SetVisibleRange {
                slot_id,
                first,
                count,
            } => {
                let (slot_id, first, count) = (*slot_id, *first, *count);
                if slot_id != self.hueco.id {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                self.hueco.primera_visible = first;
                self.hueco.visibles =
                    count.min(u32::try_from(MAX_ROWS_PER_BATCH).unwrap_or(u32::MAX));
                let filas = self.filas_visibles();
                let cambio = ViewChange::Rows {
                    slot_id: self.hueco.id,
                    generation: self.hueco.generacion,
                    first_visible: self.hueco.primera_visible,
                    rows: filas,
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            UiAction::FocusSlot { slot_id } => {
                if *slot_id != self.hueco.id {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                (self.aplicada(), Vec::new())
            }
            UiAction::Resync => {
                let snap = self.snapshot();
                (self.aplicada(), vec![self.sobre(UiUpdate::Snapshot(snap))])
            }
            // Lo que todavía no hace este host se DICE, no se traga: un
            // renderer que pida navegar tiene que poder distinguir «aún no»
            // de «no pasó nada» (tareas 2.3 a 2.6).
            UiAction::Activate { .. }
            | UiAction::Parent { .. }
            | UiAction::History { .. }
            | UiAction::Dialog { .. }
            | UiAction::DialogInput { .. }
            | UiAction::CancelTask { .. } => (
                ActionAck::Unavailable {
                    reason_key: "host-action-not-implemented".to_owned(),
                },
                Vec::new(),
            ),
        }
    }

    /// Qué queda sin terminar al apagar. Hoy nada: las tasks y la sesión
    /// entran en las tareas 2.5 y 2.6, y entonces esto tendrá algo que
    /// contar.
    fn apagar() -> ShutdownReport {
        ShutdownReport { incomplete: false }
    }

    fn aplicada(&self) -> ActionAck {
        // La secuencia que lo reflejará: la siguiente que se emita.
        ActionAck::Applied {
            sequence: self.sequence + 1,
        }
    }

    /// Una carrera normal entre lo que el renderer creía y lo que hay.
    fn obsoleta(reason: StaleAction) -> ActionAck {
        ActionAck::Stale { reason }
    }

    fn sobre(&mut self, u: UiUpdate) -> BridgeEnvelope<UiUpdate> {
        self.sequence += 1;
        BridgeEnvelope::new(self.instance.clone(), self.sequence, u)
    }

    fn parche(&mut self, changes: Vec<ViewChange>) -> BridgeEnvelope<UiUpdate> {
        let base = self.sequence;
        self.sobre(UiUpdate::Patch(ViewPatch {
            base_sequence: base,
            changes,
        }))
    }

    /// Solo la ventana visible viaja: un directorio de cien mil entradas no
    /// cruza el bridge para pintar cuarenta filas.
    fn filas_visibles(&self) -> Vec<RowView> {
        let primera = usize::try_from(self.hueco.primera_visible).unwrap_or(0);
        let cuantas = usize::try_from(self.hueco.visibles).unwrap_or(0);
        self.hueco
            .entradas
            .iter()
            .enumerate()
            .skip(primera)
            .take(cuantas.min(MAX_ROWS_PER_BATCH))
            .map(|(i, e)| self.fila(i, e))
            .collect()
    }

    fn fila(&self, i: usize, e: &Entry) -> RowView {
        let bytes = e
            .path
            .file_name()
            .map_or(&[][..], norte_proto::Segment::as_bytes);
        let (texto, hostil) = norte_frontend::display_name(bytes);
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
            selected: i == self.hueco.cursor,
            marked: self.hueco.marcas.contains(&i),
            cells: Vec::new(),
        }
    }

    fn snapshot(&self) -> ViewSnapshot {
        let (path, hostil) = norte_frontend::path_display(&self.hueco.dir);
        ViewSnapshot {
            connection: self.conexion.clone(),
            slots: vec![SlotView::Browser(BrowserSlotView {
                slot_id: self.hueco.id,
                generation: self.hueco.generacion,
                path_display: clamp_display(path),
                path_hostile: hostil,
                total_rows: Some(self.hueco.entradas.len() as u64),
                first_visible: self.hueco.primera_visible,
                rows: self.filas_visibles(),
                cursor: (!self.hueco.entradas.is_empty())
                    .then_some(RowKey(self.hueco.cursor as u64)),
                marks: self.hueco.marcas.len() as u64,
                state: self.hueco.estado.clone(),
            })],
            focus: Some(self.hueco.id),
            status: self.status.clone(),
            dialogs: Vec::new(),
            tasks: Vec::new(),
            locale: self.locale.clone(),
        }
    }
}
