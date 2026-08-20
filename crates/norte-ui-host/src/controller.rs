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

use std::sync::Arc;

use norte_frontend::PaneState;
use norte_frontend::keymap::{Availability, Effective, Resolution, Resolver};
use norte_frontend::layout::{KindRegistry, Node, Rect, Resolved, RoleId, Roles, SlotId, resolve};
use norte_frontend::nav::{History, Trail, TrailStep};
use norte_proto::{Entry, EntryKind, Error, VPath};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::action::UiAction;
use crate::backend::HostBackend;
use crate::bridge::{
    ActionAck, BridgeEnvelope, InstanceId, MAX_ROWS_PER_BATCH, RequestToken, RowKey, StaleAction,
    clamp_display,
};
use crate::commands::{Efecto, efecto_de};
use crate::dto::{
    BrowserSlotView, ConnectionView, PendingView, RowKind, RowView, SlotState, SlotView,
    StatusView, UiNotice, UiUpdate, ViewChange, ViewPatch, ViewSnapshot,
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
    /// El keymap EFECTIVO de la pantalla de listado, ya fusionado
    /// (preset + capas del usuario). Lo construye quien arranca el host —
    /// leer configuración no es asunto suyo—; [`crate::keys::keymap_de_preset`]
    /// hace lo mínimo para un test o un arranque sin configuración.
    pub keymap: Effective,
    /// La disposición: el árbol de huecos. De un preset de fábrica
    /// (`norte_frontend::layout::presets::tree`) o de la configuración del
    /// usuario; el host no lee ficheros.
    pub layout: Node,
    /// El tamaño INICIAL de la ventana, en celdas de layout. El renderer lo
    /// corrige en cuanto sepa el suyo ([`UiAction::SetViewport`]).
    pub viewport: (u16, u16),
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
    /// La respuesta de un listado que se pidió antes. Vuelve al actor como
    /// un mensaje más: así el estado lo sigue tocando un solo escritor.
    Listado(Box<(RequestToken, VPath, Result<Vec<Entry>, Error>)>),
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
        // El actor conserva un remite a SU propio buzón: por ahí vuelven las
        // respuestas de lo que tarda.
        let tx2 = tx.clone();

        let mut estado = Estado::nuevo(
            instance.clone(),
            options.locale,
            &options.initial_dir,
            options.keymap,
            options.layout,
            options.viewport,
        );
        // La sesión primero: dice DÓNDE estaba cada hueco, y listar antes
        // sería traer un directorio para tirarlo.
        estado.leer_sesion(options.backend.as_ref()).await;
        // El primer listado se pide ANTES de publicar nada: el snapshot 0
        // describe una pantalla que ya existe, no una promesa.
        estado.listar_inicial(options.backend.as_ref()).await;
        let primero = estado.snapshot();

        let host = Self {
            inbox: tx,
            updates: updates.clone(),
            instance,
        };
        tokio::spawn(actor(rx, estado, options.backend, updates, tx2));
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
    backend: Arc<dyn HostBackend>,
    updates: broadcast::Sender<BridgeEnvelope<UiUpdate>>,
    buzon: mpsc::Sender<Mensaje>,
) {
    while let Some(msg) = rx.recv().await {
        match msg {
            Mensaje::Accion(accion, responde) => {
                let (ack, salidas) = estado.aplicar(&accion, &backend, &buzon);
                for u in salidas {
                    // Sin suscriptores no es un error: el host sigue vivo
                    // aunque el renderer se haya ido a hacer otra cosa.
                    let _ = updates.send(u);
                }
                let _ = responde.send(ack);
            }
            Mensaje::Listado(datos) => {
                let (token, dir, res) = *datos;
                if estado.hueco().en_vuelo != Some(token) {
                    // Llegó tarde: otra navegación la relevó. Se descarta
                    // AQUÍ, no se esconde en el renderer.
                    continue;
                }
                estado.aterriza(dir, res);
                // Un `cd` cambia la pantalla entera —directorio, filas,
                // cursor, marcas—, así que se manda una foto en vez de
                // enumerar parches que el renderer tendría que casar.
                let snap = estado.snapshot();
                let _ = updates.send(estado.sobre(UiUpdate::Snapshot(snap)));
            }
            Mensaje::Apagar(responde) => {
                let informe = estado.apagar(backend.as_ref()).await;
                let _ = updates.send(estado.sobre(UiUpdate::Notice(UiNotice::Shutdown {
                    incomplete: informe.incomplete,
                })));
                let _ = responde.send(informe);
                return;
            }
        }
    }
}

/// El área que el renderer dice tener, en celdas de layout.
///
/// El árbol se reparte en una rejilla y no en píxeles a propósito: los
/// mínimos de cada kind están declarados así y los comparte con el TUI, que
/// es lo que hace que «este panel no cabe» signifique lo mismo en las dos
/// superficies.
fn rect((width, height): (u16, u16)) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width,
        height,
    }
}

/// ¿Este hueco del árbol es un listado?
fn es_listado(arbol: &Node, slot: SlotId, kinds: &KindRegistry) -> bool {
    kind_de(arbol, slot).is_some_and(|k| k.as_str() == "browser" && kinds.get(&k).is_some())
}

/// El kind declarado de un hueco del árbol.
fn kind_de(arbol: &Node, slot: SlotId) -> Option<norte_frontend::layout::KindId> {
    fn buscar(n: &Node, slot: SlotId) -> Option<norte_frontend::layout::KindId> {
        match n {
            Node::Slot { id, kind, .. } if *id == slot => Some(kind.clone()),
            Node::Slot { .. } => None,
            Node::Split { children, .. } | Node::Tabs { children, .. } => {
                children.iter().find_map(|c| buscar(c, slot))
            }
        }
    }
    buscar(arbol, slot)
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
///
/// El estado del listado NO es de este crate: es
/// [`norte_frontend::pane::PaneState`], el mismo que usa el TUI. Cursor,
/// marcas, ocultos, memoria de cursor por directorio y la ÉPOCA del listado
/// salen de ahí, así que las dos superficies no pueden divergir en lo que
/// significa «bajar el cursor» (ADR 0066, D14).
struct Hueco {
    pane: PaneState,
    /// De dónde vengo y a dónde vuelvo. También compartido.
    historial: History,
    primera_visible: u64,
    visibles: u32,
    /// La petición de listado EN VUELO, si la hay. Una respuesta con otro
    /// testigo llegó tarde: se descarta aquí, en Rust, no se esconde en el
    /// renderer.
    en_vuelo: Option<RequestToken>,
    estado: SlotState,
}

/// El estado semántico. Solo el actor lo toca.
struct Estado {
    instance: InstanceId,
    sequence: u64,
    /// Contador de peticiones. Cada listado se lleva el suyo, y una
    /// respuesta con un testigo viejo se descarta.
    token: u64,
    locale: String,
    /// El resolver de teclas, con SU keymap efectivo dentro (mismo tipo y
    /// mismo contrato que el del TUI).
    resolver: Resolver,
    /// La disposición: el árbol que el usuario configuró. NO se toca al
    /// redimensionar — un layout guardado es su intención, y reescribirlo
    /// porque la ventana encogió significa que abrir el host un minuto se
    /// come el layout del TUI (ADR 0058 D5).
    arbol: Node,
    /// Los kinds que este host sabe declarar (mínimos, foco, roles).
    kinds: KindRegistry,
    /// El reparto del ÚLTIMO tamaño conocido: quién se pinta, quién no, y en
    /// qué orden se tabula. Vive y muere con el tamaño, no con el árbol.
    reparto: Resolved,
    /// Quién tiene el foco y quién es el destino.
    roles: Roles,
    /// Los huecos con estado, por id.
    huecos: std::collections::BTreeMap<u32, Hueco>,
    /// La sesión de UI: qué revisión se leyó, si esta ventana es su dueña, y
    /// si el esquema que hay guardado es de una versión que este host no
    /// entiende (ADR 0059).
    sesion: Sesion,
    status: StatusView,
    conexion: ConnectionView,
}

/// Lo que el host sabe de la sesión guardada.
#[derive(Debug)]
struct Sesion {
    /// La revisión sobre la que se escribe. Escribir sobre otra es pisar a
    /// quien escribió en medio, y el core lo rechaza.
    revision: u64,
    /// Esta ventana es la dueña. Una SUELTA (`detached`) no escribe: la
    /// sesión es un documento con un solo escritor.
    owner: bool,
    /// Lo guardado es de un esquema MÁS NUEVO que el que este host entiende.
    /// Entonces no se aplica y —sobre todo— no se sobrescribe: arrancar de la
    /// configuración es recuperable; machacar la sesión de una versión futura
    /// no lo es.
    futuro: bool,
    /// La política compartida de escritura: qué recortar, cuándo no repetir
    /// y cada cuánto vuelve a preguntar una ventana suelta.
    policy: norte_frontend::session::PushPolicy,
}

impl Estado {
    fn nuevo(
        instance: InstanceId,
        locale: String,
        dir: &VPath,
        keymap: Effective,
        arbol: Node,
        viewport: (u16, u16),
    ) -> Self {
        let kinds = KindRegistry::builtin();
        let reparto = resolve(rect(viewport), &arbol, &kinds);
        // Un hueco de listado por cada `browser` del árbol, todos en el
        // mismo directorio: de dónde arranca cada uno es cosa de la sesión
        // (y hasta que exista, arrancar los dos donde arrancó el host es lo
        // honesto).
        let mut huecos = std::collections::BTreeMap::new();
        for SlotId(id) in arbol.slot_ids() {
            if es_listado(&arbol, SlotId(id), &kinds) {
                huecos.insert(
                    id,
                    Hueco {
                        pane: PaneState::new(dir.clone(), Vec::new()),
                        historial: History::default(),
                        primera_visible: 0,
                        visibles: 64,
                        en_vuelo: None,
                        estado: SlotState::Loading,
                    },
                );
            }
        }
        let activo = huecos.keys().copied().next().unwrap_or(1);
        let mut roles = Roles::con_active(SlotId(activo));
        // El DESTINO es el otro listado visible, si lo hay: es lo que hace
        // que copiar tenga a dónde ir sin preguntar.
        if let Some(otro) = huecos.keys().copied().find(|k| *k != activo) {
            roles.set(RoleId::Target, SlotId(otro));
        }
        Self {
            instance,
            sequence: 0,
            token: 0,
            locale,
            resolver: Resolver::new(keymap),
            arbol,
            kinds,
            reparto,
            roles,
            huecos,
            sesion: Sesion {
                revision: 0,
                owner: false,
                futuro: false,
                // Una ventana suelta vuelve a preguntar por la propiedad
                // cada treinta ticks: la dueña puede cerrarse en cualquier
                // momento y entonces alguien tiene que recogerla.
                policy: norte_frontend::session::PushPolicy::new(30),
            },
            status: StatusView::default(),
            conexion: ConnectionView::Connected,
        }
    }

    /// El hueco con el foco. Siempre hay uno: si el rol apunta a un hueco
    /// que ya no existe, cae al primero que haya.
    fn activo(&self) -> u32 {
        let preferido = self.roles.get(RoleId::Active).map(|SlotId(id)| id);
        preferido
            .filter(|id| self.huecos.contains_key(id))
            .or_else(|| self.huecos.keys().copied().next())
            .unwrap_or(1)
    }

    fn hueco(&self) -> &Hueco {
        let id = self.activo();
        self.huecos.get(&id).expect("el hueco activo existe")
    }

    fn hueco_mut(&mut self) -> &mut Hueco {
        let id = self.activo();
        self.huecos.get_mut(&id).expect("el hueco activo existe")
    }

    /// Deja los roles apuntando a huecos que EXISTEN y se VEN.
    ///
    /// El destino es el otro listado visible; si no hay otro, no hay
    /// destino — y eso es más honesto que apuntar al mismo hueco que tiene el
    /// foco, que haría que copiar pareciera posible cuando no lo es.
    fn reconcilia_roles(&mut self) {
        let activo = self.activo();
        self.roles.set(RoleId::Active, SlotId(activo));
        let otro = self
            .huecos
            .keys()
            .copied()
            .find(|id| *id != activo && !self.oculto(*id));
        match otro {
            Some(id) => self.roles.set(RoleId::Target, SlotId(id)),
            None => self.roles.clear(RoleId::Target),
        }
    }

    /// ¿Está este hueco fuera del reparto de ESTE tamaño?
    ///
    /// Un hueco oculto —una pestaña de atrás, un panel que no cabe— no pide
    /// listados ni proyecta filas: lo que no se ve no se trae.
    fn oculto(&self, id: u32) -> bool {
        self.reparto.hidden.contains(&SlotId(id))
    }

    /// El listado inicial de cada hueco VISIBLE, el único que se espera EN
    /// LÍNEA: hasta que exista no hay pantalla que enseñar, así que no hay
    /// nada que congelar.
    ///
    /// Un hueco oculto no se lista: lo que no se ve no se trae, y en cuanto
    /// el reparto lo saque a la luz se pedirá entonces.
    async fn listar_inicial(&mut self, backend: &dyn HostBackend) {
        let visibles: Vec<u32> = self
            .huecos
            .keys()
            .copied()
            .filter(|id| !self.oculto(*id))
            .collect();
        for id in visibles {
            let dir = self.huecos[&id].pane.dir().clone();
            let res = backend.list(dir.clone()).await;
            self.aterriza_en(id, dir, res);
        }
    }

    /// Aplica el resultado de un listado. El orden y el cursor los decide
    /// `PaneState`, que es quien sabe qué hacer con la memoria del cursor y
    /// con un foco pendiente.
    fn aterriza(&mut self, dir: VPath, res: Result<Vec<Entry>, Error>) {
        let id = self.activo();
        self.aterriza_en(id, dir, res);
    }

    /// Igual, sobre un hueco concreto.
    fn aterriza_en(&mut self, id: u32, dir: VPath, res: Result<Vec<Entry>, Error>) {
        let Some(hueco) = self.huecos.get_mut(&id) else {
            return;
        };
        hueco.en_vuelo = None;
        match res {
            Ok(entradas) => {
                hueco.pane.set_listing(dir, entradas);
                hueco.primera_visible = 0;
                hueco.estado = SlotState::Ready;
            }
            Err(e) => {
                hueco.pane.set_listing(dir, Vec::new());
                hueco.estado = SlotState::Error {
                    reason_key: norte_frontend::error::error_key(&e).to_owned(),
                    detail: None,
                };
            }
        }
    }

    /// Aplica una acción y devuelve su acuse más lo que haya que publicar.
    ///
    /// Lo que TARDA no se hace aquí. Una navegación deja la petición en
    /// vuelo y devuelve; su respuesta vuelve al actor como un mensaje más y
    /// se aplica en [`Estado::aterriza`]. Por eso el cursor sigue
    /// respondiendo mientras un NFS muerto piensa: el único escritor no está
    /// esperando a nadie.
    fn aplicar(
        &mut self,
        accion: &UiAction,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match accion {
            UiAction::MoveCursor { slot_id, delta } => {
                let (slot_id, delta) = (*slot_id, *delta);
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
            UiAction::SelectRow { slot_id, key } | UiAction::ToggleMark { slot_id, key } => {
                let (slot_id, key) = (*slot_id, *key);
                let marcar = matches!(accion, UiAction::ToggleMark { .. });
                if slot_id != self.activo() {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                let Some(i) = self.fila_valida(key) else {
                    // Una fila que ya no existe: el listado cambió bajo el
                    // click. Ni se interpreta ni es un error.
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                };
                if marcar {
                    let marcada = self
                        .hueco()
                        .pane
                        .entries()
                        .get(i)
                        .is_some_and(|e| self.hueco().pane.is_marked(e));
                    self.hueco_mut().pane.set_mark(i, !marcada);
                } else {
                    self.hueco_mut().pane.set_cursor(i);
                }
                (self.aplicada(), vec![self.parche_filas()])
            }
            UiAction::SetVisibleRange {
                slot_id,
                first,
                count,
            } => {
                let (slot_id, first, count) = (*slot_id, *first, *count);
                if slot_id != self.activo() {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                self.hueco_mut().primera_visible = first;
                self.hueco_mut().visibles =
                    count.min(u32::try_from(MAX_ROWS_PER_BATCH).unwrap_or(u32::MAX));
                (self.aplicada(), vec![self.parche_filas()])
            }
            UiAction::FocusSlot { slot_id } => {
                let slot_id = *slot_id;
                // Enfocar algo que no existe o que no se ve es una carrera
                // con un reparto anterior, no una orden.
                if !self.huecos.contains_key(&slot_id) || self.oculto(slot_id) {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                self.roles.set(RoleId::Active, SlotId(slot_id));
                self.reconcilia_roles();
                let snap = self.snapshot();
                (self.aplicada(), vec![self.sobre(UiUpdate::Snapshot(snap))])
            }
            UiAction::Activate { .. } | UiAction::Parent { .. } | UiAction::History { .. } => {
                self.navegacion(accion, backend, buzon)
            }
            UiAction::SetViewport { width, height } => {
                self.reparto = resolve(rect((*width, *height)), &self.arbol, &self.kinds);
                // El destino no puede apuntar a algo que no se ve: una copia
                // que aterriza en un panel oculto es una copia que el usuario
                // no verá llegar.
                self.reconcilia_roles();
                let snap = self.snapshot();
                (self.aplicada(), vec![self.sobre(UiUpdate::Snapshot(snap))])
            }
            UiAction::Key(k) => self.tecla(k, backend, buzon),
            UiAction::Resync => {
                let snap = self.snapshot();
                (self.aplicada(), vec![self.sobre(UiUpdate::Snapshot(snap))])
            }
            // Lo que todavía no hace este host se DICE, no se traga: un
            // renderer tiene que poder distinguir «aún no» de «no pasó nada»
            // (tareas 2.4 a 2.6).
            UiAction::Dialog { .. }
            | UiAction::DialogInput { .. }
            | UiAction::CancelTask { .. } => (
                ActionAck::Unavailable {
                    reason_key: "host-action-not-implemented".to_owned(),
                },
                Vec::new(),
            ),
        }
    }

    /// Una tecla: la resuelve el keymap COMPARTIDO y el host solo ejecuta.
    ///
    /// Los cuatro desenlaces son los del resolver, y ninguno se queda
    /// callado: un comando corre, un prefijo o un contador a medias se
    /// PINTAN (lo que no se ve no se puede cancelar), una tecla ligada a algo
    /// que aquí no se puede hacer lo dice, y una tecla sin binding se
    /// descarta dejando el estado limpio.
    fn tecla(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Ok(chord) = k.to_chord() else {
            // Una tecla que el adaptador no entiende no se adivina.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-key-unmapped".to_owned(),
                },
                Vec::new(),
            );
        };
        match self.resolver.push(chord) {
            Resolution::Run { command, count } => {
                let veces = count.times();
                let Some(efecto) = efecto_de(&command, veces) else {
                    // En el catálogo, ligada, y este host no la hace. Se
                    // dice con la MISMA frase que el TUI.
                    let frase = norte_frontend::keymap::unavailable_message(
                        &command,
                        Availability::NotHere,
                    );
                    self.status.message = Some(clamp_display(frase));
                    self.status.pending = None;
                    let cambio = ViewChange::Status(self.status.clone());
                    return (
                        ActionAck::Unavailable {
                            reason_key: "cmd-not-here".to_owned(),
                        },
                        vec![self.parche(vec![cambio])],
                    );
                };
                self.status.pending = None;
                self.aplicar_efecto(efecto, backend, buzon)
            }
            Resolution::Pending(_) | Resolution::Counting(_) => {
                self.status.pending = Some(PendingView {
                    chords: self
                        .resolver
                        .pending()
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" "),
                    count: self.resolver.count(),
                });
                let cambio = ViewChange::Status(self.status.clone());
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            Resolution::Unavailable { command, why } => {
                let frase = norte_frontend::keymap::unavailable_message(&command, why);
                self.status.message = Some(clamp_display(frase));
                self.status.pending = None;
                let cambio = ViewChange::Status(self.status.clone());
                (
                    ActionAck::Unavailable {
                        reason_key: match why {
                            Availability::Here => "cmd-here",
                            Availability::NotBuilt { .. } => "cmd-not-built",
                            Availability::NotHere => "cmd-not-here",
                        }
                        .to_owned(),
                    },
                    vec![self.parche(vec![cambio])],
                )
            }
            Resolution::Reset => {
                let habia = self.status.pending.take().is_some();
                if habia {
                    let cambio = ViewChange::Status(self.status.clone());
                    return (self.aplicada(), vec![self.parche(vec![cambio])]);
                }
                (self.aplicada(), Vec::new())
            }
        }
    }

    /// Ejecuta lo que un comando pide sobre el hueco con el foco.
    ///
    /// Es el MISMO camino que toman las acciones directas del renderer (un
    /// click, un arrastre): que una tecla y un gesto que significan lo mismo
    /// hagan lo mismo no puede depender de que alguien se acuerde.
    fn aplicar_efecto(
        &mut self,
        efecto: Efecto,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot = self.activo();
        match efecto {
            Efecto::Cursor(delta) => self.aplicar(
                &UiAction::MoveCursor {
                    slot_id: slot,
                    delta,
                },
                backend,
                buzon,
            ),
            Efecto::Pagina(paginas) => {
                let filas = i64::from(self.hueco().visibles.max(1));
                self.aplicar(
                    &UiAction::MoveCursor {
                        slot_id: slot,
                        delta: paginas.saturating_mul(filas),
                    },
                    backend,
                    buzon,
                )
            }
            Efecto::Extremo { al_final } => {
                if al_final {
                    self.hueco_mut().pane.end();
                } else {
                    self.hueco_mut().pane.home();
                }
                (self.aplicada(), vec![self.parche_cursor()])
            }
            Efecto::Entrar => {
                let key = RowKey(self.hueco().pane.cursor() as u64);
                self.navegacion(&UiAction::Activate { slot_id: slot, key }, backend, buzon)
            }
            Efecto::Subir => self.navegacion(&UiAction::Parent { slot_id: slot }, backend, buzon),
            Efecto::Rastro { atras } => self.navegacion(
                &UiAction::History {
                    slot_id: slot,
                    back: atras,
                },
                backend,
                buzon,
            ),
            Efecto::Marcar => {
                let key = RowKey(self.hueco().pane.cursor() as u64);
                self.aplicar(&UiAction::ToggleMark { slot_id: slot, key }, backend, buzon)
            }
            Efecto::DesmarcarTodo => {
                self.hueco_mut().pane.clear_marks();
                (self.aplicada(), vec![self.parche_filas()])
            }
        }
    }

    /// Las tres acciones que CAMBIAN de directorio.
    ///
    /// Aparte de las de arriba porque son las únicas que dejan trabajo en
    /// vuelo: las demás terminan dentro de esta función.
    fn navegacion(
        &mut self,
        accion: &UiAction,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match accion {
            UiAction::Activate { slot_id, key } => {
                let (slot_id, key) = (*slot_id, *key);
                if slot_id != self.activo() {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                let Some(i) = self.fila_valida(key) else {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                };
                let Some(entrada) = self.hueco().pane.entries().get(i) else {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                };
                if entrada.kind != EntryKind::Dir {
                    // Abrir un FICHERO es otra cosa (visor, opener externo) y
                    // llega con la tarea 2.6: decirlo es más honesto que
                    // navegar a algo que no es un directorio.
                    return (
                        ActionAck::Unavailable {
                            reason_key: "host-open-file-not-implemented".to_owned(),
                        },
                        Vec::new(),
                    );
                }
                let destino = entrada.path.clone();
                (
                    self.aplicada(),
                    self.navegar(&destino, Trail::Record, backend, buzon),
                )
            }
            UiAction::Parent { slot_id } => {
                if *slot_id != self.activo() {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                let actual = self.hueco().pane.dir().clone();
                let Some(padre) = actual.parent() else {
                    return (
                        ActionAck::Unavailable {
                            reason_key: "msg-nav-at-root".to_owned(),
                        },
                        Vec::new(),
                    );
                };
                // El cursor aterriza en el directorio del que se sale, no en
                // la primera fila: es lo que hace que subir y bajar sea
                // reversible. Lo resuelve `PaneState` al recibir el listado.
                self.hueco_mut().pane.set_pending_focus(actual);
                (
                    self.aplicada(),
                    self.navegar(&padre, Trail::Record, backend, buzon),
                )
            }
            UiAction::History { slot_id, back } => {
                let (slot_id, back) = (*slot_id, *back);
                if slot_id != self.activo() {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                let actual = self.hueco().pane.dir().clone();
                let paso = if back {
                    TrailStep::Back
                } else {
                    TrailStep::Forward
                };
                let destino = if back {
                    self.hueco_mut().historial.step_back(actual)
                } else {
                    self.hueco_mut().historial.step_forward(actual)
                };
                let Some(destino) = destino else {
                    // Una tecla que se queda muda no se distingue de una
                    // rota: el rastro agotado lo DICE.
                    return (
                        ActionAck::Unavailable {
                            reason_key: paso.empty_message().to_owned(),
                        },
                        Vec::new(),
                    );
                };
                (
                    self.aplicada(),
                    self.navegar(&destino, Trail::Replay(paso), backend, buzon),
                )
            }
            _ => (Self::obsoleta(StaleAction::Generation), Vec::new()),
        }
    }

    /// Arranca una navegación: registra el paso en el rastro, marca el hueco
    /// como cargando y deja la petición EN VUELO con su testigo.
    fn navegar(
        &mut self,
        destino: &VPath,
        trail: Trail,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let anterior = self.hueco().pane.dir().clone();
        let destino = destino.clone();
        // Un `Replay` es el rastro reproduciéndose: registrar ahí haría que
        // `back` se alimentara de sí mismo y el lector oscilara entre dos
        // directorios.
        if anterior != destino && trail == Trail::Record {
            self.hueco_mut().historial.record(anterior);
        }
        // La memoria del cursor se toma con el dir que se ABANDONA todavía
        // puesto (contrato de `remember_cursor`).
        self.hueco_mut().pane.remember_cursor();
        self.hueco_mut().estado = SlotState::Loading;

        self.token += 1;
        let token = RequestToken(self.token);
        self.hueco_mut().en_vuelo = Some(token);

        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let dir = destino.clone();
        tokio::spawn(async move {
            let res = backend.list(dir.clone()).await;
            // Si el actor ya no está, la respuesta no le importa a nadie.
            let _ = buzon
                .send(Mensaje::Listado(Box::new((token, dir, res))))
                .await;
        });

        let cambio = ViewChange::SlotState {
            slot_id: self.activo(),
            state: SlotState::Loading,
        };
        vec![self.parche(vec![cambio])]
    }

    /// El índice de una fila, si la clave es de ESTA generación y existe.
    fn fila_valida(&self, key: RowKey) -> Option<usize> {
        let i = usize::try_from(key.0).ok()?;
        (i < self.hueco().pane.entries().len()).then_some(i)
    }

    /// Apaga: vuelca la sesión si esta ventana es su dueña, y DICE si algo
    /// quedó sin escribir.
    ///
    /// Volcar aquí y no solo en un tick es lo que hace que cerrar justo
    /// después de navegar guarde el directorio nuevo y no el anterior. Un
    /// conflicto en el último momento no se reintenta a lo loco: se informa,
    /// que es lo único honesto cuando ya no hay pantalla que corregir.
    async fn apagar(&mut self, backend: &dyn HostBackend) -> ShutdownReport {
        if !self.sesion.owner || self.sesion.futuro {
            // Una ventana suelta no escribe, y una sesión del futuro no se
            // machaca.
            return ShutdownReport { incomplete: false };
        }
        let mut body = self.capturar_sesion();
        let vivos: Vec<SlotId> = self.huecos.keys().map(|id| SlotId(*id)).collect();
        if self.sesion.policy.prepare(&mut body, &vivos, 0).is_none() {
            // Nada cambió desde lo último que se mandó.
            return ShutdownReport { incomplete: false };
        }
        let Ok(json) = serde_json::to_value(&body) else {
            return ShutdownReport { incomplete: true };
        };
        let puesta = backend
            .session_put(
                norte_frontend::session::SCHEMA_VERSION,
                self.sesion.revision,
                json,
            )
            .await;
        // Un conflicto es otra ventana que escribió en medio: lo suyo se
        // queda, y se dice que lo nuestro no llegó. Pisarlo sería perder la
        // sesión de alguien.
        let Ok(rev) = puesta else {
            self.sesion.policy.resend();
            return ShutdownReport { incomplete: true };
        };
        self.sesion.revision = rev;
        self.sesion.policy.sent(std::sync::Arc::new(body));
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
        Self::filas_de(self.hueco())
    }

    /// Las filas visibles de un hueco cualquiera.
    fn filas_de(hueco: &Hueco) -> Vec<RowView> {
        let primera = usize::try_from(hueco.primera_visible).unwrap_or(0);
        let cuantas = usize::try_from(hueco.visibles).unwrap_or(0);
        hueco
            .pane
            .entries()
            .iter()
            .enumerate()
            .skip(primera)
            .take(cuantas.min(MAX_ROWS_PER_BATCH))
            .map(|(i, e)| Self::fila(hueco, i, e))
            .collect()
    }

    /// La generación de un listado: la ÉPOCA de `PaneState`, que sube en
    /// cada cosa que mueve los índices —un re-listado, un re-orden, un
    /// filtro de ocultos—, no solo al cambiar de directorio.
    fn generacion(&self) -> u64 {
        self.hueco().pane.listing_epoch()
    }

    /// Mover el cursor manda el cursor, no el listado.
    fn parche_cursor(&mut self) -> BridgeEnvelope<UiUpdate> {
        let cambio = ViewChange::Cursor {
            slot_id: self.activo(),
            generation: self.generacion(),
            cursor: Some(RowKey(self.hueco().pane.cursor() as u64)),
        };
        self.parche(vec![cambio])
    }

    /// Lo que cambia una marca o un scroll: las filas visibles.
    fn parche_filas(&mut self) -> BridgeEnvelope<UiUpdate> {
        let cambio = ViewChange::Rows {
            slot_id: self.activo(),
            generation: self.generacion(),
            first_visible: self.hueco().primera_visible,
            rows: self.filas_visibles(),
        };
        self.parche(vec![cambio])
    }

    fn fila(hueco: &Hueco, i: usize, e: &Entry) -> RowView {
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
            selected: i == hueco.pane.cursor(),
            marked: hueco.pane.is_marked(e),
            cells: Vec::new(),
        }
    }

    /// Lee la sesión y la aplica, si se puede.
    ///
    /// Tres cosas se deciden aquí, y las tres son de ADR 0059:
    ///
    /// - **Quién escribe.** Una ventana SUELTA no escribe. La sesión es un
    ///   documento con un solo escritor, y dos ventanas guardando la suya
    ///   encima de la otra es exactamente lo que produce una pantalla que
    ///   nadie pidió.
    /// - **Qué se aplica.** Solo lo que este host entiende. Un hueco de un
    ///   kind desconocido NO se toca, ni siquiera para borrarlo.
    /// - **Qué NO se sobrescribe.** Si lo guardado es de un esquema más
    ///   nuevo, se arranca de la configuración y se deja quieto: arrancar sin
    ///   sesión es recuperable; machacar la de una versión futura no.
    async fn leer_sesion(&mut self, backend: &dyn HostBackend) {
        let Ok((sesion, owner)) = backend.session_get().await else {
            // Sin sesión legible se arranca igual: es memoria de dónde
            // estabas, no un requisito para existir.
            return;
        };
        self.sesion.revision = sesion.revision;
        self.sesion.owner = owner;
        if sesion.version > norte_frontend::session::SCHEMA_VERSION {
            self.sesion.futuro = true;
            return;
        }
        if sesion.version == 0 {
            // Nadie la ha escrito todavía.
            return;
        }
        let Ok(body) = serde_json::from_value::<norte_frontend::session::SessionBody>(sesion.body)
        else {
            return;
        };
        self.aplicar_sesion(&body);
    }

    /// Coloca cada hueco donde la sesión dice que estaba.
    fn aplicar_sesion(&mut self, body: &norte_frontend::session::SessionBody) {
        for (id, hueco) in &mut self.huecos {
            let Some(estado) = body.slots.get(id) else {
                continue;
            };
            hueco.pane.begin_loading(estado.path.clone());
            hueco
                .historial
                .seed(estado.back.clone(), estado.forward.clone());
        }
    }

    /// La pantalla de AHORA como cuerpo de sesión.
    ///
    /// Las MARCAS no entran: son una selección de trabajo, no un sitio donde
    /// estabas, y restaurarlas haría que una ventana nueva abriese con media
    /// docena de ficheros elegidos que nadie eligió.
    fn capturar_sesion(&self) -> norte_frontend::session::SessionBody {
        let mut body = norte_frontend::session::SessionBody::default();
        body.layouts
            .insert("default".to_owned(), self.arbol.clone());
        for (id, hueco) in &self.huecos {
            body.slots.insert(
                *id,
                norte_frontend::session::SlotState {
                    path: hueco.pane.dir().clone(),
                    cursor: hueco.pane.cursor() as u64,
                    back: hueco.historial.trail().to_vec(),
                    forward: hueco.historial.forward_trail().to_vec(),
                    sort: hueco.pane.sort(),
                    columns: Vec::new(),
                    show_hidden: hueco.pane.show_hidden(),
                    // El sello de edad lo pone quien escribe, con su reloj:
                    // aquí no hay ninguno, y una hora inventada haría que la
                    // barrida de huérfanos se llevara lo que no toca.
                    touched_ms: 0,
                },
            );
        }
        body
    }

    /// La pantalla entera: TODOS los huecos que el reparto pinta, cada uno
    /// proyectado según lo que es.
    ///
    /// Los ocultos no viajan. Un kind que este host todavía no proyecta sí
    /// viaja, en gris y con su nombre: preservar lo que no se entiende es la
    /// regla de la sesión (ADR 0059), y hacerlo desaparecer sería peor que
    /// enseñarlo apagado.
    fn snapshot(&self) -> ViewSnapshot {
        let mut slots = Vec::new();
        for (slot, _) in &self.reparto.placements {
            let SlotId(id) = *slot;
            if let Some(hueco) = self.huecos.get(&id) {
                slots.push(SlotView::Browser(Self::browser(id, hueco)));
            } else {
                let nombre = kind_de(&self.arbol, *slot)
                    .map_or_else(|| "unknown".to_owned(), |k| k.as_str().to_owned());
                slots.push(SlotView::Unsupported {
                    slot_id: id,
                    kind_name: clamp_display(nombre),
                });
            }
        }
        ViewSnapshot {
            connection: self.conexion.clone(),
            slots,
            focus: Some(self.activo()),
            status: self.status.clone(),
            dialogs: Vec::new(),
            tasks: Vec::new(),
            locale: self.locale.clone(),
        }
    }

    /// La proyección de UN listado.
    fn browser(id: u32, hueco: &Hueco) -> BrowserSlotView {
        let (path, hostil) = norte_frontend::path_display(hueco.pane.dir());
        BrowserSlotView {
            slot_id: id,
            generation: hueco.pane.listing_epoch(),
            path_display: clamp_display(path),
            path_hostile: hostil,
            total_rows: Some(hueco.pane.entries().len() as u64),
            first_visible: hueco.primera_visible,
            rows: Self::filas_de(hueco),
            cursor: (!hueco.pane.entries().is_empty())
                .then_some(RowKey(hueco.pane.cursor() as u64)),
            marks: hueco.pane.marks_len() as u64,
            state: hueco.estado.clone(),
        }
    }
}
