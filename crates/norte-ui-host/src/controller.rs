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
    ActionAck, BridgeEnvelope, InstanceId, MAX_ROWS_PER_BATCH, MAX_TASKS, ModalId, RequestToken,
    RowKey, StaleAction, clamp_display,
};
use crate::commands::{Efecto, efecto_de};
use crate::dto::{
    BrowserSlotView, ColumnHeader, ConnectionView, DialogChoice, DialogView, LayoutView,
    PendingView, RowKind, RowView, SlotPlacement, SlotRole, SlotState, SlotView, StatusView,
    TaskStateView, TaskView, UiNotice, UiUpdate, ViewChange, ViewPatch, ViewSnapshot,
};

/// Capacidad del buzón del actor. Acotado a propósito: si el renderer manda
/// más rápido de lo que el host aplica, se le hace esperar — jamás se crece
/// sin límite.
const INBOX: usize = 256;

/// Entradas de la PRIMERA página: lo que se pinta antes de seguir drenando.
///
/// El mismo número que usa el TUI (`navigate::FIRST_PAGE`) y por el mismo
/// motivo: el primer frame no espera al listado entero, y un directorio de
/// medio millón de entradas se ve igual de rápido que uno de diez.
const FIRST_PAGE: usize = 100;

/// Entradas por lote mientras se drena el resto. Ni una a una —un mensaje
/// por entrada ahoga el buzón del actor— ni todas de golpe.
const FILL_BATCH: usize = 500;

/// Lo que se espera a la lectura del visor.
const PLAZO_VISOR: std::time::Duration = std::time::Duration::from_secs(20);

/// Los contextos de ayuda que ESTA ventana puede estar viviendo.
///
/// Escrita a mano y cerrada, como la del TUI (`norte_tui::help_context`), y
/// por el mismo motivo: derivada del corpus coincidiría consigo misma por
/// construcción y no cazaría nada. Un test la contrasta contra las páginas
/// del corpus EN LOS DOS SENTIDOS — un contexto que ninguna página reclama es
/// un hallazgo, y una página que reclama un contexto que no está aquí también
/// —, que es lo que impide que renombrar una portada deje a `F1` abriendo el
/// índice en silencio.
pub const CONTEXTOS: &[&str] = &[
    "browse",
    "viewer",
    "dialog.confirm",
    "dialog.approval",
    "dialog.mkdir",
];

/// Plazo de una llamada de extensiones (catálogo o página).
///
/// La ayuda se pinta sin esperarla, así que este plazo no gobierna una
/// pantalla: gobierna una tarea que si no volviera jamás dejaría un `id`
/// reclamado y una página en blanco para siempre.
const PLAZO_PLUGINS: std::time::Duration = std::time::Duration::from_secs(5);

/// Tope de un nombre tecleado, en bytes. Ni `NAME_MAX` (que es del sistema
/// de ficheros y no lo sabemos aquí) ni el de pantalla: un tope generoso que
/// impide que un renderer mande un megabyte, y que RECHAZA en vez de
/// recortar — recortar un nombre es inventarse otro.
const MAX_NOMBRE: usize = 4096;

/// Lo que el visor lee de un fichero: una cabecera de 256 KiB. El resto NO
/// se lee — el mismo presupuesto que el TUI, y por el mismo motivo (ADR
/// 0005): un visor no es una excusa para traerse un fichero de un giga.
const VISOR_CAP: u64 = 256 * 1024;

/// Sondas simultáneas contra el daemon. El mismo número que el TUI, y por el
/// mismo motivo: una sesión remota no puede pagar N viajes en serie.
const SONDEOS_A_LA_VEZ: usize = 8;

/// Lo que se espera a UNA sonda. Un provider colgado no puede llevarse por
/// delante el resto de la tanda.
const PLAZO_SONDEO: std::time::Duration = std::time::Duration::from_secs(5);

/// Cuántas entradas se sondean de una tanda. Es una PANTALLA con holgura:
/// más no se ve, y cada sondeo es un viaje al daemon.
const MAX_SONDEOS: usize = 200;

/// Actualizaciones retenidas para un suscriptor lento. Al pasarse, el
/// suscriptor se entera de que se quedó atrás y pide un snapshot: es la
/// recuperación barata, y la que no gasta memoria del host.
const UPDATE_BUFFER: usize = 64;

/// Cuántas filas mueve una página en la ayuda.
///
/// El renderer es dueño del scroll del cuerpo —una página del corpus cruza
/// entera—, así que este número solo gobierna los CURSORES, que son los que
/// el host lleva.
const PAGINA_DE_AYUDA: usize = 10;

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
    /// El keymap efectivo de la pantalla del VISOR, con las mismas capas.
    /// Va aparte porque es otra pantalla: con el visor abierto las teclas son
    /// suyas, igual que en el TUI, y mezclarlas sería inventarse un tercer
    /// contexto de entrada que no existe en ningún preset.
    pub keymap_viewer: Effective,
    /// La disposición: el árbol de huecos. De un preset de fábrica
    /// (`norte_frontend::layout::presets::tree`) o de la configuración del
    /// usuario; el host no lee ficheros.
    pub layout: Node,
    /// El tamaño INICIAL de la ventana, en celdas de layout. El renderer lo
    /// corrige en cuanto sepa el suyo ([`UiAction::SetViewport`]).
    pub viewport: (u16, u16),
    /// Hasta dónde llega este frontend: solo mirar, o también escribir.
    ///
    /// La ventana gráfica arranca en [`crate::commands::Efectos::SoloLectura`]
    /// hasta que la fase 5 le dé el camino seguro; el TUI y los tests usan
    /// [`crate::commands::Efectos::Completo`].
    pub effects: crate::commands::Efectos,
    /// La configuración YA cargada, para los ajustes en solo lectura.
    ///
    /// La lee quien arranca el host, UNA vez, como todo lo demás: un
    /// frontend que la relee por su cuenta acaba enseñando unos ajustes que
    /// no son los que está usando.
    pub settings: norte_frontend::config::FrontendConfig,
    /// Dónde vive cada cosa, ya resuelto. Ver [`crate::settings::HostPaths`].
    pub paths: crate::settings::HostPaths,
    /// La configuración de columnas ENTERA, no una lista ya resuelta.
    ///
    /// Las columnas se configuran POR ESQUEMA (`[ui.columns.schemes.sftp]`),
    /// y resolverlas una vez al arrancar dejaba muerta esa mitad de la
    /// configuración: una columna `attr:` que solo existe en `sftp` no se
    /// pedía nunca y no se pintaba nunca, porque los atributos que viajan en
    /// cada listado se habían congelado con los del esquema de arranque.
    pub columns: norte_frontend::columns::ColumnsSettings,
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
    /// Quedó algo sin terminar: una task en cola o corriendo, o la sesión sin
    /// escribir. Se DICE: apagar en silencio con una copia a medias es como
    /// se pierde una operación sin que nadie lo sepa.
    pub incomplete: bool,
}

/// El host: un asa barata de clonar sobre el único escritor.
#[derive(Clone)]
pub struct UiHost {
    inbox: mpsc::Sender<Mensaje>,
    updates: broadcast::Sender<BridgeEnvelope<UiUpdate>>,
    instance: InstanceId,
}

/// Lo que vuelve de un listado: el testigo que lo pidió, el hueco al que va,
/// el directorio y el resultado.
type RespuestaListado = (RequestToken, u32, VPath, Result<Vec<Entry>, Error>);

/// Lo que vuelve de una tanda de sondeo: el directorio que se sondeaba, el
/// hueco, y las parejas `(lo que se pidió, lo que contestó el provider)`.
type Sondas = (VPath, u32, Vec<(VPath, Entry)>);

enum Mensaje {
    Accion(Box<UiAction>, oneshot::Sender<ActionAck>),
    /// La respuesta de un listado que se pidió antes. Vuelve al actor como
    /// un mensaje más: así el estado lo sigue tocando un solo escritor.
    ///
    /// Lleva el HUECO que lo pidió y no se resuelve al llegar: si mientras
    /// volaba el foco se movió al panel de al lado, aterrizar «en el activo»
    /// sería meter un directorio en la pantalla equivocada.
    Listado(Box<RespuestaListado>),
    /// El catálogo de atributos de un esquema, ya resuelto.
    Catalogo(Box<(String, norte_proto::AttrCatalog)>),
    /// Una op de agente espera decisión humana.
    Aprobacion(Box<norte_proto::methods::PolicyApprovalRequired>),
    /// Más entradas del listado que se está drenando por detrás.
    MasEntradas(Box<(RequestToken, u32, Vec<Entry>)>),
    /// El catálogo de plugins que la ayuda pidió al abrirse.
    ///
    /// Vuelve al actor como un mensaje más —un solo escritor— y llega tarde
    /// a propósito: la ayuda se PINTA sin esperarlo. La documentación es
    /// cosmética, y una ventana que se queda en blanco hasta que el daemon
    /// conteste es peor que una lateral que gana filas medio segundo después.
    Plugins(Box<Result<norte_proto::methods::PluginListResult, Error>>),
    /// La página de UN plugin, pedida bajo demanda al abrirla.
    PaginaDePlugin(
        Box<(
            String,
            Result<norte_proto::methods::PluginHelpResult, Error>,
        )>,
    ),
    /// El contenido que el visor pidió.
    Contenido(Box<(RequestToken, VPath, Result<Vec<u8>, Error>)>),
    /// Lo que un sondeo averiguó de unas cuantas entradas (tamaño y fecha de
    /// un listado perezoso).
    ///
    /// Lleva el DIRECTORIO que se estaba sondeando, y no un testigo ni una
    /// época. El testigo vale `None` en cuanto la primera página aterriza —o
    /// sea que el guard que lo miraba no guardaba nada—, y la época sube con
    /// cada lote de relleno, que no invalida un sondeo: lo que invalida un
    /// tamaño es que el listado sea de OTRO sitio. Como la hidratación casa
    /// por ruta, un índice desplazado da igual.
    ///
    /// Y lleva PAREJAS `(pedido, respuesta)`, porque un provider puede
    /// contestar con otra ortografía del mismo nombre y la entrada que hay
    /// que hidratar es la que se pidió.
    Hidratado(Box<Sondas>),
    /// Una Task recién encolada, con su progreso y su cancelación.
    TaskNueva(Box<crate::backend::HostTask>),
    /// Encolarla falló. El usuario tiene que enterarse: pidió un borrado.
    TaskFallida(Box<Error>),
    /// La conexión con el daemon cambió de estado.
    Conexion(norte_client::ConnEvent),
    /// Un snapshot de progreso. Por la MISMA cola que todo lo demás, que es
    /// lo que garantiza que un estado terminal no se adelante ni se pierda.
    Progreso(Box<norte_proto::TaskProgress>),
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
    /// [`UiError::NoBrowserSlot`] si la disposición no declara ningún
    /// `browser`: sin listado no hay pantalla que pintar.
    pub async fn start(options: UiHostOptions) -> Result<(Self, ViewSnapshot), UiError> {
        let instance = InstanceId::new(nueva_instancia());
        let (updates, _) = broadcast::channel(UPDATE_BUFFER);
        let (tx, rx) = mpsc::channel(INBOX);
        // El actor conserva un remite a SU propio buzón: por ahí vuelven las
        // respuestas de lo que tarda.
        let tx2 = tx.clone();

        let (mut estado, backend) = Estado::nuevo(instance.clone(), options);
        if estado.huecos.is_empty() {
            return Err(UiError::NoBrowserSlot);
        }
        // La sesión primero: dice DÓNDE estaba cada hueco, y listar antes
        // sería traer un directorio para tirarlo.
        estado.leer_sesion(backend.as_ref()).await;
        // El primer listado se pide ANTES de publicar nada: el snapshot 0
        // describe una pantalla que ya existe, no una promesa.
        estado.listar_inicial(&backend, &tx2).await;
        // Y se sondea lo que ya se ve: el listado local no trae tamaño ni
        // fecha (#52), así que sin esto la primera pantalla nace con dos
        // columnas en blanco y no se llenan hasta que algo la mueva.
        let visibles: Vec<u32> = estado.huecos.keys().copied().collect();
        for slot in visibles {
            estado.sondear(slot, &backend, &tx2);
        }
        let primero = estado.snapshot();

        // Los dos canales de la conexión son del PRIMER dueño, así que se
        // toman una vez, aquí, y su contenido entra por el mismo buzón que
        // todo lo demás: un aviso de conexión perdida tiene que ordenarse
        // con lo que estaba pasando cuando se perdió.
        if let Some(mut eventos) = backend.take_conn_events() {
            let buzon = tx.clone();
            tokio::spawn(async move {
                while let Some(ev) = eventos.recv().await {
                    if buzon.send(Mensaje::Conexion(ev)).await.is_err() {
                        return;
                    }
                }
            });
        }
        // Las aprobaciones de policy son una MUTACIÓN por delegación: decir
        // que sí a la operación de un agente. Un frontend que todavía no
        // puede escribir tampoco puede autorizar que escriba otro, así que
        // en solo lectura el canal ni se toma (y el diálogo no existe, que es
        // más honesto que uno que no responde).
        if estado.efectos == crate::commands::Efectos::Completo
            && let Some(mut aprobaciones) = backend.take_approvals()
        {
            let buzon = tx.clone();
            tokio::spawn(async move {
                while let Some(req) = aprobaciones.recv().await {
                    if buzon
                        .send(Mensaje::Aprobacion(Box::new(req)))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
        if let Some(mut ajenas) = backend.take_foreign_tasks() {
            let buzon = tx.clone();
            tokio::spawn(async move {
                while let Some(task) = ajenas.recv().await {
                    if buzon
                        .send(Mensaje::TaskNueva(Box::new(task)))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
        let host = Self {
            inbox: tx,
            updates: updates.clone(),
            instance,
        };
        tokio::spawn(actor(rx, estado, backend, updates, tx2));
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
    /// La disposición no tiene ningún listado.
    ///
    /// Una pantalla sin listado no es una pantalla: es un cuelgue con
    /// bordes. Se rechaza AQUÍ y no en la primera tecla, donde el pánico
    /// caería dentro de la task del actor —sin log y sin caída visible— y
    /// dejaría la ventana muerta contestando `Down` para siempre (es la
    /// forma de #242 en esta superficie).
    #[error("la disposición no tiene ningún listado")]
    NoBrowserSlot,
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
            Mensaje::Catalogo(datos) => {
                let (scheme, catalogo) = *datos;
                let _ = updates.send(estado.aplicar_catalogo(scheme, catalogo));
            }
            Mensaje::Aprobacion(req) => {
                for u in estado.abrir_aprobacion(&req) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Listado(datos) => {
                let (token, slot, dir, res) = *datos;
                if estado.huecos.get(&slot).and_then(|h| h.en_vuelo) != Some(token) {
                    // Llegó tarde: otra navegación la relevó. Se descarta
                    // AQUÍ, no se esconde en el renderer.
                    continue;
                }
                estado.aterriza_en(slot, dir, res);
                estado.sondear(slot, &backend, &buzon);
                // Un `cd` cambia la pantalla entera —directorio, filas,
                // cursor, marcas—, así que se manda una foto en vez de
                // enumerar parches que el renderer tendría que casar.
                let snap = estado.snapshot();
                let _ = updates.send(estado.sobre(UiUpdate::Snapshot(Box::new(snap))));
            }
            Mensaje::Contenido(datos) => {
                let (token, path, leido) = *datos;
                if let Some(u) = estado.abrir_visor(token, path, leido) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Plugins(res) => {
                for u in estado.aplicar_catalogo_de_plugins(*res, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::PaginaDePlugin(datos) => {
                let (id, res) = *datos;
                if let Some(u) = estado.aplicar_pagina_de_plugin(&id, res.as_ref().ok()) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Hidratado(datos) => {
                let (dir, slot, sondas) = *datos;
                if let Some(u) = estado.aplicar_sondas(slot, &dir, &sondas) {
                    let _ = updates.send(u);
                    // Y se pide la siguiente tanda: `MAX_SONDEOS` acota cada
                    // vuelta, no la ventana. Sin esto, una ventana más alta
                    // que una tanda se quedaba a medias en silencio.
                    estado.sondear(slot, &backend, &buzon);
                }
            }
            Mensaje::MasEntradas(datos) => {
                let (token, slot, batch) = *datos;
                if let Some(u) = estado.aplicar_lote(slot, token, batch) {
                    let _ = updates.send(u);
                    estado.sondear(slot, &backend, &buzon);
                }
            }
            Mensaje::Conexion(ev) => {
                for u in estado.cambio_de_conexion(ev) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::TaskNueva(task) => {
                for u in estado.registrar_task(*task, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::TaskFallida(e) => {
                // `error_key` devuelve una CLAVE Fluent, y el contrato de
                // `StatusView.message` dice «ya traducido por el host»: sin
                // traducir, el usuario leía `err-not-found` en la barra.
                let clave = norte_frontend::error::error_key(&e);
                estado.status.message = Some(clamp_display(norte_i18n::t(clave)));
                let cambio = ViewChange::Status(estado.status.clone());
                let u = estado.parche(vec![cambio]);
                let _ = updates.send(u);
                let n = estado.sobre(UiUpdate::Notice(UiNotice::Message {
                    key: clave.to_owned(),
                    detail: None,
                }));
                let _ = updates.send(n);
            }
            Mensaje::Progreso(p) => {
                for u in estado.progreso(&p) {
                    let _ = updates.send(u);
                }
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
/// El id de una columna, listo para pintar.
///
/// Un id `attr:` o `plugin:` es texto de CONFIGURACIÓN —puede venir de la
/// capa de proyecto— y `ColumnId::Display` lo escribe crudo. Lo que cruza va
/// enmascarado, como cualquier otra cosa ajena.
fn id_pintable(id: &norte_frontend::columns::ColumnId) -> String {
    let (pintable, _) = norte_frontend::display_name(id.to_string().as_bytes());
    pintable
}

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

/// El ahora, en milisegundos. Lo inyecta el proyector para que el formato de
/// una fecha relativa («hace 3 días») no dependa de cuándo se serializó.
fn ahora_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
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
    /// El drenaje que sigue trayendo lotes por detrás, si lo hay.
    ///
    /// SEPARADO de `en_vuelo` porque son dos vidas distintas: la primera
    /// página aterriza y limpia `en_vuelo`, pero el resto del stream sigue
    /// llegando. Compartir un testigo hacía que `aplicar_lote` rechazara
    /// TODOS los lotes de una navegación —un directorio de cinco mil
    /// entradas se quedaba en cien— y que el arranque tuviera que
    /// restituirlo a mano para funcionar.
    drenando: Option<RequestToken>,
    estado: SlotState,
    /// Hay una tanda de sondeo en vuelo para este hueco.
    sondeando: bool,
    /// Se levanta cuando el listado cambia: lo que vuelva de la tanda en
    /// vuelo ya no describe esta pantalla.
    cancelar_sondeo: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Los paths que ya se sondearon (hayan contestado o no). Sin esta
    /// memoria, un `stat` que falla vuelve a pedirse en cada repintado y el
    /// sondeo se convierte en un bucle contra el daemon.
    sondeados: std::collections::HashSet<VPath>,
}

/// Un diálogo abierto y lo que hará si se confirma.
struct Dialogo {
    id: ModalId,
    vista: DialogView,
    /// Lo que la confirmación ejecuta. `None` = solo informa.
    al_confirmar: Option<Pendiente>,
    /// Lo que el usuario tecleó, TAL CUAL.
    ///
    /// Separado de `vista.input`, que es su proyección para pintar —
    /// enmascarada y acotada—, porque este texto acaba siendo un NOMBRE DE
    /// FICHERO. Pasarlo por el recorte de pantalla creaba directorios con
    /// una elipsis dentro: el mismo error que ADR 0061 decidió no volver a
    /// cometer, en miniatura.
    input_crudo: String,
}

/// Lo que un diálogo tiene pendiente de hacer.
enum Pendiente {
    /// Borrar estas entradas, a la papelera o permanentemente.
    Borrar {
        /// Qué se borra, en orden de listado.
        paths: Vec<VPath>,
        /// Permanente (sin papelera): el diálogo lo AVISA.
        permanente: bool,
    },
    /// Decidir sobre una op de agente. La op real la tiene el daemon ligada
    /// al id: aquí solo viaja el sí o el no.
    Decidir {
        /// El id que el daemon espera de vuelta.
        approval_id: u64,
    },
    /// Crear un directorio dentro de este otro. El nombre lo teclea el
    /// usuario y se valida al confirmar, no al teclear: corregir un nombre a
    /// medias es peor que verlo rechazado al final.
    CrearDirectorio {
        /// Dónde se crea.
        dir: VPath,
    },
}

/// Una task viva en el tablero.
struct TaskViva {
    vista: TaskView,
    /// Cómo pedirle que pare. Cancelar dos veces no es un error.
    cancel: std::sync::Arc<dyn Fn() + Send + Sync>,
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
    /// El keymap efectivo del VISOR, para que la paleta pueda decir el
    /// atajo de un comando de esa pantalla.
    efectivo_visor: Effective,
    /// La paleta de comandos, si está abierta.
    ///
    /// Es un contexto de entrada más, como el buscador incremental y el
    /// visor: mientras esté abierta, las teclas de texto son suyas.
    paleta: Option<norte_frontend::palette_state::Palette>,
    /// La configuración con la que arrancó esta ventana, para enseñarla.
    config: norte_frontend::config::FrontendConfig,
    /// Dónde vive cada cosa.
    paths: crate::settings::HostPaths,
    /// Los ajustes, si están abiertos.
    ajustes: Option<crate::settings::Ajustes>,
    /// La ayuda, si está abierta. Tapa la pantalla y se queda las teclas,
    /// como el visor: sus teclas son FIJAS (no hay vocabulario `dialog.*`
    /// para «filtrar esta lista» ni para «seguir este enlace»), que es lo
    /// mismo que hacen el TUI y la paleta.
    ayuda: Option<crate::help::Ayuda>,
    /// Una COPIA del keymap efectivo del listado.
    ///
    /// El resolver se queda con el suyo, y construir el panel de
    /// continuaciones necesita el efectivo entero (qué sigue a un prefijo, y
    /// qué disponibilidad tiene cada continuación). `Effective` es `Clone` y
    /// el TUI hace exactamente esto por el mismo motivo.
    efectivo: Effective,
    /// El idioma negociado, para las etiquetas de las continuaciones.
    lang: norte_i18n::Lang,
    /// Las continuaciones del prefijo a medias, si lo hay.
    ///
    /// Se construye en la TRANSICIÓN —la tecla que abre la secuencia y cada
    /// una que la profundiza— y no al proyectar: `WhichKeyRows::build` cuesta
    /// varias cadenas y uno o dos formatos Fluent POR FILA, y su propio
    /// rustdoc avisa de lo que pasa si se llama desde el pintado.
    whichkey: Option<norte_frontend::whichkey::WhichKeyRows>,
    /// El resolver de la pantalla del visor. Mientras el visor esté abierto,
    /// las teclas pasan por AQUÍ.
    resolver_visor: Resolver,
    /// Si este frontend puede escribir.
    efectos: crate::commands::Efectos,
    /// Cuántas líneas caben en el visor, según el renderer.
    ///
    /// `None` mientras no lo diga: se cae al tamaño en celdas menos el cromo,
    /// que es una estimación y se comporta como tal.
    visor_filas: Option<usize>,
    /// El testigo de la lectura del visor en vuelo, si la hay.
    ///
    /// Sin él, una lectura lenta abría el visor DESPUÉS de que el usuario lo
    /// cerrara o se fuera a otro sitio — y como las teclas se enrutan por
    /// «hay visor», la siguiente tecla la interpretaba otro mapa sin que
    /// nadie hubiera pedido nada.
    visor_en_vuelo: Option<RequestToken>,
    /// El visor abierto, si lo hay. El modelo es el COMPARTIDO
    /// (`norte_frontend::viewer::Viewer`): decodificación, hexadecimal y
    /// desplazamiento son suyos.
    visor: Option<norte_frontend::viewer::Viewer>,
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
    /// El último tamaño repartido, en celdas. Viaja al renderer con el
    /// reparto: sin él no puede saber sobre qué rejilla están medidos los
    /// rectángulos que recibe.
    viewport: (u16, u16),
    /// Quién tiene el foco y quién es el destino.
    roles: Roles,
    /// La configuración de columnas, por esquema.
    columnas: norte_frontend::columns::ColumnsSettings,
    /// El catálogo de atributos de la localización de cada hueco, cacheado
    /// por ESQUEMA: es lo que dice si un `attr:` es un tamaño, una fecha o un
    /// modo, y sin él se pinta el número crudo.
    catalogos: std::collections::HashMap<String, norte_proto::AttrCatalog>,
    /// Los huecos con estado, por id.
    huecos: std::collections::BTreeMap<u32, Hueco>,
    /// Los diálogos abiertos, en orden de apertura. Cada uno con su id: un
    /// segundo `Confirm` con el mismo id no vuelve a lanzar nada, y uno con
    /// un id viejo no cierra el que hay ahora.
    dialogos: Vec<Dialogo>,
    /// El siguiente id de diálogo. Monótono: un id no se reutiliza jamás,
    /// que es lo que hace que «viejo» se pueda distinguir de «actual».
    siguiente_modal: u64,
    /// El tablero: lo que está en marcha, por id de task.
    tasks: std::collections::BTreeMap<u64, TaskViva>,
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
    /// Construye el estado a partir de las opciones de arranque.
    ///
    /// Toma las opciones ENTERAS y no ocho parámetros sueltos: son los
    /// mismos datos, y una lista de ocho posiciones es donde dos `Effective`
    /// del mismo tipo se intercambian sin que el compilador diga nada.
    fn nuevo(instance: InstanceId, options: UiHostOptions) -> (Self, Arc<dyn HostBackend>) {
        let UiHostOptions {
            backend,
            initial_dir,
            locale,
            keymap,
            keymap_viewer: keymap_visor,
            layout: arbol,
            viewport,
            columns: columnas,
            effects: efectos,
            settings,
            paths,
        } = options;
        let dir = &initial_dir;
        // El idioma negociado, para las etiquetas de las continuaciones.
        let lang = match locale.as_str() {
            "es" => norte_i18n::Lang::Es,
            _ => norte_i18n::Lang::En,
        };
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
                        drenando: None,
                        sondeando: false,
                        cancelar_sondeo: std::sync::Arc::default(),
                        estado: SlotState::Loading,
                        sondeados: std::collections::HashSet::new(),
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
        let estado = Self {
            instance,
            sequence: 0,
            token: 0,
            locale,
            paleta: None,
            ayuda: None,
            ajustes: None,
            config: settings,
            paths,
            efectivo_visor: keymap_visor.clone(),
            efectivo: keymap.clone(),
            lang,
            whichkey: None,
            resolver: Resolver::new(keymap),
            resolver_visor: Resolver::new(keymap_visor),
            efectos,
            visor_filas: None,
            visor_en_vuelo: None,
            visor: None,
            arbol,
            kinds,
            reparto,
            viewport,
            roles,
            columnas,
            catalogos: std::collections::HashMap::new(),
            huecos,
            dialogos: Vec::new(),
            siguiente_modal: 1,
            tasks: std::collections::BTreeMap::new(),
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
        };
        (estado, backend)
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

    /// Toma la primera página de un stream y deja el resto drenando hacia el
    /// actor.
    ///
    /// El resto llega por el MISMO buzón que todo lo demás, con el testigo de
    /// su petición: un lote de una navegación abandonada se descarta igual
    /// que su primera página.
    async fn primera_pagina(
        stream: Result<norte_client::EntryStream, Error>,
        slot: u32,
        token: RequestToken,
        buzon: mpsc::Sender<Mensaje>,
    ) -> Result<Vec<Entry>, Error> {
        use futures::StreamExt as _;
        let mut stream = stream?;
        let mut primera = Vec::with_capacity(FIRST_PAGE);
        while primera.len() < FIRST_PAGE {
            match stream.next().await {
                Some(Ok(e)) => primera.push(e),
                // Un error a mitad de página se cuenta como el error del
                // listado: media página no es un listado.
                Some(Err(e)) => return Err(e),
                None => return Ok(primera),
            }
        }
        tokio::spawn(async move {
            let mut lote = Vec::with_capacity(FILL_BATCH);
            while let Some(entrada) = stream.next().await {
                let Ok(entrada) = entrada else {
                    // El resto se cortó. Lo que ya se pintó sigue siendo
                    // válido; callarlo es mejor que tirar el listado entero.
                    break;
                };
                lote.push(entrada);
                if lote.len() >= FILL_BATCH {
                    let batch = std::mem::take(&mut lote);
                    if buzon
                        .send(Mensaje::MasEntradas(Box::new((token, slot, batch))))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    lote = Vec::with_capacity(FILL_BATCH);
                }
            }
            if !lote.is_empty() {
                let _ = buzon
                    .send(Mensaje::MasEntradas(Box::new((token, slot, lote))))
                    .await;
            }
        });
        Ok(primera)
    }

    /// El listado inicial de cada hueco VISIBLE, el único que se espera EN
    /// LÍNEA: hasta que exista no hay pantalla que enseñar, así que no hay
    /// nada que congelar.
    ///
    /// Un hueco oculto no se lista: lo que no se ve no se trae, y en cuanto
    /// el reparto lo saque a la luz se pedirá entonces.
    async fn listar_inicial(
        &mut self,
        backend_arc: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let backend = backend_arc.as_ref();
        let visibles: Vec<u32> = self
            .huecos
            .keys()
            .copied()
            .filter(|id| !self.oculto(*id))
            .collect();
        for id in visibles {
            let dir = self.huecos[&id].pane.dir().clone();
            self.token += 1;
            let token = RequestToken(self.token);
            if let Some(h) = self.huecos.get_mut(&id) {
                h.en_vuelo = Some(token);
                h.drenando = Some(token);
            }
            self.pedir_catalogo(&dir, backend_arc, buzon);
            let stream = backend.list(dir.clone(), self.attrs_de(&dir)).await;
            let res = Self::primera_pagina(stream, id, token, buzon.clone()).await;
            self.aterriza_en(id, dir, res);
        }
    }

    /// Aplica el resultado de un listado sobre SU hueco. El orden y el
    /// cursor los decide `PaneState`, que es quien sabe qué hacer con la
    /// memoria del cursor y con un foco pendiente.
    fn aterriza_en(&mut self, id: u32, dir: VPath, res: Result<Vec<Entry>, Error>) {
        let Some(hueco) = self.huecos.get_mut(&id) else {
            return;
        };
        hueco.en_vuelo = None;
        // El listado es OTRO: lo sondeado antes no dice nada de estas
        // entradas, que nacen perezosas otra vez. Sin este vaciado, volver a
        // un directorio ya visitado deja las columnas de tamaño y fecha en
        // blanco para el resto de la sesión — y de paso el conjunto crecía
        // con un `VPath` por fichero visto en toda la vida del proceso.
        hueco.sondeados.clear();
        // Y lo que esté volando ya no vale: se marca para que su respuesta se
        // descarte en vez de pegarse a otro directorio.
        hueco
            .cancelar_sondeo
            .store(true, std::sync::atomic::Ordering::SeqCst);
        hueco.cancelar_sondeo = std::sync::Arc::default();
        hueco.sondeando = false;
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
            UiAction::MoveCursor { slot_id, delta } => self.mover_cursor(*slot_id, *delta),
            UiAction::SelectRow {
                slot_id,
                key,
                generation,
            } => self.poner_cursor(*slot_id, *key, *generation),
            UiAction::ToggleMark {
                slot_id,
                key,
                generation,
            } => self.marcar(*slot_id, *key, *generation),
            UiAction::SetVisibleRange {
                slot_id,
                first,
                count,
            } => {
                let (slot_id, first, count) = (*slot_id, *first, *count);
                // NO se exige que sea el hueco activo: declarar qué filas se
                // ven no es actuar sobre el listado, es decir dónde está
                // mirando el usuario. La rueda sobre el panel de al lado
                // mueve ESE panel y no le roba el foco a nadie.
                if !self.huecos.contains_key(&slot_id) || self.oculto(slot_id) {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                let tope = count.min(u32::try_from(MAX_ROWS_PER_BATCH).unwrap_or(u32::MAX));
                if let Some(h) = self.huecos.get_mut(&slot_id) {
                    h.primera_visible = first;
                    h.visibles = tope;
                }
                // Scroll = filas nuevas a la vista, y puede que sin tamaño
                // todavía: el sondeo va con la ventana, no con el cursor.
                self.sondear(slot_id, backend, buzon);
                (self.aplicada(), vec![self.parche_filas_de(slot_id)])
            }
            UiAction::SortBy { slot_id, column } => self.ordenar_por(*slot_id, column),
            UiAction::FocusSlot { slot_id } => {
                let slot_id = *slot_id;
                // Enfocar algo que no existe o que no se ve es una carrera
                // con un reparto anterior, no una orden.
                if !self.huecos.contains_key(&slot_id) || self.oculto(slot_id) {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                self.roles.set(RoleId::Active, SlotId(slot_id));
                self.reconcilia_roles();
                // Un cambio de foco NO reenvía la pantalla: lo único que
                // cambia es quién lleva cada papel. Mandar la foto entera
                // costaba todas las filas de todos los listados por cada
                // tabulador — el mismo derroche que el bridge acota en el
                // cursor (decisión D7).
                let cambio = ViewChange::Layout(self.disposicion());
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            UiAction::MarkRange {
                slot_id,
                from,
                to,
                generation,
            } => {
                let (slot_id, from, to, generation) = (*slot_id, *from, *to, *generation);
                // Los DOS extremos tienen que existir en ESTA generación.
                // Medio rango válido significa marcar hasta un sitio que ya
                // no es el que el usuario señaló — y `PaneState::mark_range`
                // RECORTA por contrato, así que un extremo desbordado
                // marcaría el listado entero, incluidas filas que el renderer
                // nunca recibió. Lo marcado es la entrada de un borrado.
                let (Some(a), Some(b)) = (
                    self.fila_de(slot_id, from, generation),
                    self.fila_de(slot_id, to, generation),
                ) else {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                };
                self.hueco_mut().pane.mark_range(a, b);
                (self.aplicada(), vec![self.parche_filas()])
            }
            UiAction::Activate { .. } | UiAction::Parent { .. } | UiAction::History { .. } => {
                self.navegacion(accion, backend, buzon)
            }
            UiAction::SetViewport { width, height } => {
                self.viewport = (*width, *height);
                self.reparto = resolve(rect(self.viewport), &self.arbol, &self.kinds);
                // El destino no puede apuntar a algo que no se ve: una copia
                // que aterriza en un panel oculto es una copia que el usuario
                // no verá llegar.
                self.reconcilia_roles();
                let snap = self.snapshot();
                (
                    self.aplicada(),
                    vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
                )
            }
            UiAction::Key(k) => self.tecla(k, backend, buzon),
            UiAction::SetViewerRows { rows } => {
                self.visor_filas = Some(usize::try_from(*rows).unwrap_or(1).max(1));
                let cambio = ViewChange::Viewer {
                    viewer: self.vista_visor(),
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            UiAction::Resync => {
                let snap = self.snapshot();
                (
                    self.aplicada(),
                    vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
                )
            }
            UiAction::Dialog { id, choice } => self.responder_dialogo(*id, choice, backend, buzon),
            UiAction::CancelTask { task_id } => self.cancelar(*task_id),
            // Un diálogo con campo de texto llega con la tarea que lo traiga
            // (crear directorio, renombrar). Decirlo es más honesto que
            // aceptar texto que nadie va a leer.
            UiAction::DialogInput { id, text } => self.escribir_en_dialogo(*id, text),
            UiAction::HelpSelectTopic { row } => self.elegir_pagina(*row, backend, buzon),
            UiAction::SettingsSelectRow { row } => self.elegir_ajuste(*row),
            UiAction::HelpActivate { index } => self.activar_en_ayuda(*index, backend, buzon),
        }
    }

    /// La tecla, cuando hay un CONTEXTO DE ENTRADA abierto que se la queda.
    ///
    /// `None` = no había ninguno (o el que había no la quiso) y la tecla
    /// sigue su camino normal: el resolver del listado.
    ///
    /// El ORDEN es el de quien tapa a quién. El visor es otra pantalla
    /// entera; la ayuda tapa al listado y desde ella se puede abrir la
    /// paleta, así que va antes; la paleta es un editor de texto libre; y el
    /// buscador incremental solo se queda las teclas de TEXTO.
    fn tecla_de_un_overlay(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        // La AYUDA va primero, incluso antes que el visor, y no por gusto:
        // se abre ENCIMA de lo que hubiera —también encima del visor, que es
        // desde donde se pide la página del visor— y quien está arriba se
        // queda las teclas. Al revés, `F1` en el visor abría una ayuda que no
        // recibía ni una tecla y que ninguna podía cerrar.
        if self.ayuda.is_some() {
            return Some(self.tecla_en_ayuda(k, backend, buzon));
        }
        if self.ajustes.is_some() {
            return Some(self.tecla_en_ajustes(k));
        }
        if self.visor.is_some() {
            return Some(self.tecla_en_visor(k, backend, buzon));
        }
        // Cualquier tecla del LISTADO cancela una lectura de visor en vuelo.
        // El usuario pulsó F3, se cansó y siguió a lo suyo: abrirle el visor
        // medio segundo después es abrir una ventana que ya nadie pidió — y
        // cambiarle el teclado de mapa sin gesto suyo. (Un segundo F3 pide su
        // propia lectura y se queda con el testigo nuevo.)
        self.visor_en_vuelo = None;
        if self.paleta.is_some() {
            return Some(self.tecla_en_paleta(k, backend, buzon));
        }
        if self.hueco().pane.quick().is_some() {
            return self.tecla_en_quick(k);
        }
        None
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
        if let Some(salida) = self.tecla_de_un_overlay(k, backend, buzon) {
            return salida;
        }
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
                // La secuencia se cerró: el panel de continuaciones describe
                // teclas que ya no están vivas, y su propio contrato dice que
                // se tira en cuanto cambia el estado del resolver.
                self.whichkey = None;
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
                // El panel se construye AQUÍ, en la transición, y no al
                // proyectar: `build` cuesta varias cadenas y uno o dos
                // formatos Fluent por fila.
                self.whichkey = Some(norte_frontend::whichkey::WhichKeyRows::build(
                    &self.efectivo,
                    self.resolver.pending(),
                    self.resolver.count(),
                    self.lang,
                ));
                let cambios = vec![
                    ViewChange::Status(self.status.clone()),
                    ViewChange::WhichKey {
                        whichkey: self.vista_whichkey(),
                    },
                ];
                (self.aplicada(), vec![self.parche(cambios)])
            }
            Resolution::Unavailable { command, why } => {
                let frase = norte_frontend::keymap::unavailable_message(&command, why);
                self.status.message = Some(clamp_display(frase));
                self.status.pending = None;
                self.whichkey = None;
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
                let panel = self.whichkey.take().is_some();
                if habia || panel {
                    let cambios = vec![
                        ViewChange::Status(self.status.clone()),
                        ViewChange::WhichKey { whichkey: None },
                    ];
                    return (self.aplicada(), vec![self.parche(cambios)]);
                }
                (self.aplicada(), Vec::new())
            }
        }
    }

    /// Las teclas mientras la paleta está abierta.
    ///
    /// Fijas a propósito: `esc` cierra, `enter` corre lo seleccionado, las
    /// flechas mueven y lo demás teclea. Es lo mismo que hace el TUI, y por
    /// el mismo motivo — el catálogo no tiene comandos para esto.
    fn tecla_en_paleta(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(p) = self.paleta.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let alto = 10;
        match k.key.as_str() {
            "Escape" | "esc" => {
                self.paleta = None;
            }
            "Enter" | "enter" => {
                let elegido = p.selected();
                self.paleta = None;
                if let Some(cmd) = elegido {
                    // El cierre viaja en su PROPIO parche y antes que el
                    // efecto. Sin él, un renderer que aplica parches —que es
                    // lo que hace el de referencia— recibía el cambio del
                    // comando y ninguno de la paleta, y la seguía pintando
                    // encima del listado hasta la siguiente foto.
                    let cierre = self.parche(vec![ViewChange::Palette { palette: None }]);
                    // Se ejecuta por el MISMO camino que una tecla: la
                    // paleta es otra puerta al catálogo, no un segundo
                    // despachador.
                    let (ack, mut resto) = match efecto_de(&cmd, 1) {
                        Some(efecto) => self.aplicar_efecto(efecto, backend, buzon),
                        None => self.no_implementado(&cmd),
                    };
                    let mut envios = vec![cierre];
                    envios.append(&mut resto);
                    return (ack, envios);
                }
            }
            "ArrowDown" | "down" => p.down(),
            "ArrowUp" | "up" => p.up(),
            "PageDown" | "pgdn" => p.page_down(alto),
            "PageUp" | "pgup" => p.page_up(alto),
            "Backspace" | "backspace" => p.backspace(),
            otra => {
                // Una tecla de TEXTO es un punto de código, no una unidad
                // UTF-16 ni un nombre de tecla: `ArrowLeft` no se teclea.
                let mut chars = otra.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if !k.ctrl && !k.alt && !k.meta => p.push_char(c),
                    _ => return (self.aplicada(), Vec::new()),
                }
            }
        }
        let cambio = ViewChange::Palette {
            palette: self.vista_paleta(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Un comando del catálogo que este host no ejecuta, dicho con la misma
    /// frase que el TUI.
    fn no_implementado(&mut self, cmd: &str) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let frase = norte_frontend::keymap::unavailable_message(cmd, Availability::NotHere);
        self.status.message = Some(clamp_display(frase));
        let cambio = ViewChange::Status(self.status.clone());
        (
            ActionAck::Unavailable {
                reason_key: "cmd-not-here".to_owned(),
            },
            vec![self.parche(vec![cambio])],
        )
    }

    /// Abre los ajustes, en solo lectura.
    ///
    /// Las filas se construyen AQUÍ y se congelan, como las de la paleta y
    /// por el mismo motivo: `build_rows` resuelve el valor efectivo de cada
    /// entrada y formatea dos cadenas Fluent por fila. La configuración es la
    /// que el host recibió al arrancar, que es la que está usando.
    fn abrir_ajustes(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.ajustes = Some(crate::settings::Ajustes::abrir(
            &self.config,
            &self.paths,
            self.lang,
        ));
        let cambio = ViewChange::Settings {
            settings: self.vista_ajustes(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Un click en una fila de los ajustes: solo mueve el cursor.
    fn elegir_ajuste(&mut self, row: u32) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(a) = self.ajustes.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        a.senalar(row as usize);
        let cambio = ViewChange::Settings {
            settings: self.vista_ajustes(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// La proyección de los ajustes.
    fn vista_ajustes(&self) -> Option<crate::dto::SettingsView> {
        Some(self.ajustes.as_ref()?.vista(self.lang))
    }

    /// Las teclas mientras los ajustes están abiertos.
    ///
    /// FIJAS, como las de la paleta y la ayuda: el catálogo no tiene
    /// comandos para «bajar por esta lista». `enter` no edita —esta ventana
    /// no escribe ajustes todavía— y lo DICE, en vez de no hacer nada.
    fn tecla_en_ajustes(
        &mut self,
        k: &crate::keys::KeyInput,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        /// Cuántas filas mueve una página.
        const PAGINA: i64 = 10;
        let Some(a) = self.ajustes.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match k.key.as_str() {
            "Escape" | "esc" => self.ajustes = None,
            "ArrowDown" | "down" => a.mover(1),
            "ArrowUp" | "up" => a.mover(-1),
            "PageDown" | "pgdn" => a.mover(PAGINA),
            "PageUp" | "pgup" => a.mover(-PAGINA),
            "Home" | "home" => a.mover(i64::MIN / 2),
            "End" | "end" => a.mover(i64::MAX / 2),
            "Enter" | "enter" => {
                // No es un descarte silencioso: quien pulsa `enter` sobre un
                // ajuste espera editarlo, y esta ventana todavía no escribe.
                return (
                    ActionAck::Unavailable {
                        reason_key: "host-settings-read-only".to_owned(),
                    },
                    Vec::new(),
                );
            }
            _ => return (self.aplicada(), Vec::new()),
        }
        let cambio = ViewChange::Settings {
            settings: self.vista_ajustes(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Abre la ayuda sobre la página del CONTEXTO donde está el lector.
    ///
    /// Quien pulsa `F1` mirando una pregunta quiere la respuesta a ESA
    /// pregunta, no el índice. El contexto es una palabra cerrada
    /// (`dialog.confirm`, `viewer`, `browse`) y quién la reclama lo dice el
    /// propio corpus en su portada, así que añadir una página para un
    /// diálogo nuevo no toca este código.
    fn abrir_ayuda(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Sobre un diálogo que se está TECLEANDO, no. La ayuda se queda el
        // teclado mientras está abierta, así que abrirla encima de un campo
        // de texto convierte el `⌫` que corrige una errata en un paso atrás
        // de la ayuda y el `enter` que confirma en otra cosa. El TUI lo
        // prohíbe por su nombre desde H3c y con el mismo razonamiento.
        if self
            .dialogos
            .last()
            .is_some_and(|d| d.vista.input.is_some())
        {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-help-over-input".to_owned(),
                },
                Vec::new(),
            );
        }
        let contexto = self.contexto_de_ayuda();
        self.ayuda = Some(crate::help::Ayuda::abrir(
            self.lang,
            contexto,
            &self.efectivo,
            &self.efectivo_visor,
            self.hechos(),
        ));
        // El catálogo de extensiones se pide y NO se espera: la ayuda se
        // pinta ya. La documentación es cosmética, y una ventana en blanco
        // hasta que el daemon conteste es peor que una lateral que gana
        // filas medio segundo más tarde. Un fallo no se dice: se pinta la
        // ayuda sin páginas de extensión.
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(PLAZO_PLUGINS, backend.plugin_list()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon.send(Mensaje::Plugins(Box::new(res))).await;
        });
        let cambio = ViewChange::Help {
            help: self.vista_ayuda(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// La conexión con el daemon cambió de estado: se PINTA y se DICE.
    ///
    /// Las dos cosas, y por la misma cola: perder el daemon a mitad de una
    /// operación no puede notarse solo en un icono.
    fn cambio_de_conexion(&mut self, ev: norte_client::ConnEvent) -> Vec<BridgeEnvelope<UiUpdate>> {
        let (vista, clave) = match ev {
            norte_client::ConnEvent::Lost => (ConnectionView::Reconnecting, "msg-daemon-lost"),
            norte_client::ConnEvent::Restored => (ConnectionView::Connected, "msg-daemon-restored"),
        };
        self.conexion = vista.clone();
        let parche = self.parche(vec![ViewChange::Connection(vista)]);
        let aviso = self.sobre(UiUpdate::Notice(UiNotice::Message {
            key: clave.to_owned(),
            detail: None,
        }));
        vec![parche, aviso]
    }

    /// El parche de la ayuda, y de paso la página que haya que pedir.
    ///
    /// UN sitio para las dos cosas a propósito: cualquier gesto que cambie de
    /// página —una flecha, un click, un enlace— puede aterrizar en la de una
    /// extensión, y esa página no está en el corpus, hay que pedirla. Un
    /// segundo camino que solo pintara sería una página de extensión que se
    /// queda en blanco según cómo se llegue a ella.
    fn parche_de_ayuda(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.pedir_pagina_de_plugin(backend, buzon);
        let cambio = ViewChange::Help {
            help: self.vista_ayuda(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// El catálogo llegó: entra al modelo y, si el lector ya está sobre una
    /// página de extensión, se pide esa página.
    ///
    /// Si la ayuda se cerró mientras volaba, no hay nada que hacer: la foto
    /// era de una apertura que ya no existe, y guardarla para la siguiente
    /// sería enseñar un catálogo viejo.
    fn aplicar_catalogo_de_plugins(
        &mut self,
        res: Result<norte_proto::methods::PluginListResult, Error>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Ok(lista) = res else {
            return Vec::new();
        };
        let Some(a) = self.ayuda.as_mut() else {
            return Vec::new();
        };
        let antes = a.estado.rows().to_vec();
        a.set_plugins(&lista.plugins);
        if a.estado.rows() == antes {
            // Un catálogo que no añade ninguna fila —ninguna extensión, o
            // ninguna con página y con id válido— no cambia la pantalla, y
            // un parche que no cambia nada obliga a un renderer a repintar
            // la ayuda entera para nada.
            return Vec::new();
        }
        self.parche_de_ayuda(backend, buzon)
    }

    /// Pide la página de la extensión abierta, si hay una y no se ha pedido
    /// ya en esta apertura de la ayuda.
    fn pedir_pagina_de_plugin(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let Some(id) = self
            .ayuda
            .as_mut()
            .and_then(crate::help::Ayuda::reclamar_pagina)
        else {
            return;
        };
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res =
                match tokio::time::timeout(PLAZO_PLUGINS, backend.plugin_help(id.clone())).await {
                    Ok(r) => r,
                    Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
                };
            let _ = buzon
                .send(Mensaje::PaginaDePlugin(Box::new((id, res))))
                .await;
        });
    }

    /// La página de una extensión llegó. Un fallo deja la página VACÍA con su
    /// nombre, que es mejor respuesta que un error encima de la ayuda — y es
    /// también lo que ve un daemon N-1 sin el método. Se pide una vez por
    /// apertura: cerrar y volver a abrir la ayuda es el reintento.
    fn aplicar_pagina_de_plugin(
        &mut self,
        id: &str,
        res: Option<&norte_proto::methods::PluginHelpResult>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let a = self.ayuda.as_mut()?;
        a.instalar_pagina(id, res?);
        let cambio = ViewChange::Help {
            help: self.vista_ayuda(),
        };
        Some(self.parche(vec![cambio]))
    }

    /// Dónde está el lector, en el vocabulario del corpus.
    ///
    /// El diálogo de más arriba gana: es lo que tapa la pantalla y lo que el
    /// lector está mirando. Un diálogo que solo informa no tiene página
    /// propia y cae al listado, que es de lo que estaba hablando.
    fn contexto_de_ayuda(&self) -> &'static str {
        debug_assert!(
            CONTEXTOS.contains(&self.contexto_calculado()),
            "un contexto fuera del vocabulario declarado"
        );
        self.contexto_calculado()
    }

    /// El cálculo, sin el ancla.
    fn contexto_calculado(&self) -> &'static str {
        if let Some(d) = self.dialogos.last() {
            return match d.al_confirmar {
                Some(Pendiente::Borrar { .. }) => "dialog.confirm",
                Some(Pendiente::Decidir { .. }) => "dialog.approval",
                Some(Pendiente::CrearDirectorio { .. }) => "dialog.mkdir",
                None => "browse",
            };
        }
        if self.visor.is_some() {
            return "viewer";
        }
        "browse"
    }

    /// Los hechos con los que la ayuda atenúa una fila, congelados al abrir.
    ///
    /// Tres son de la entrada bajo el cursor y se saben. Los dos de solo
    /// lectura NO se saben todavía —el host no lleva cuenta de si el
    /// provider de un hueco rehúsa escribir— y se declaran permisivos, que
    /// es el valor por defecto de la propia tabla compartida: es una lista de
    /// IMPEDIMENTOS conocidos, y «no lo he mirado» no es uno. Atenuar por lo
    /// que no se ha comprobado engaña tanto como no atenuar.
    fn hechos(&self) -> norte_frontend::availability::Facts {
        let hueco = self.hueco();
        let entrada = hueco.pane.entries().get(hueco.pane.cursor());
        norte_frontend::availability::Facts {
            // `enterable` SÍ es exactamente `Dir`, porque eso es lo que
            // acepta la navegación. La asimetría con `viewable` de abajo es
            // deliberada: cada uno refleja lo que su camino rehúsa.
            enterable: entrada.is_some_and(|e| e.kind == EntryKind::Dir),
            // Lo MISMO que rehúsa `pedir_visor`, que solo rehúsa un
            // directorio: un symlink se abre en el visor sin problema, y
            // atenuar F3 sobre uno decía «esto no aplica» de una tecla que
            // funciona. Los dos sitios se mueven juntos.
            viewable: entrada.is_some_and(|e| e.kind != EntryKind::Dir),
            rename_single: hueco.pane.marks_len() <= 1,
            source_read_only: false,
            dest_read_only: false,
            // `degraded` en la tabla significa que la sesión va SIN CIFRAR,
            // que no es ninguno de los tres estados que este host proyecta
            // (conectado, reintentando, perdido). Mientras el wire de la
            // conexión no llegue hasta aquí, la respuesta honesta es que no
            // consta.
            degraded: false,
            // El host habla por el SDK contra el daemon, que es quien lleva
            // el journal (ADR 0066): lo que muta por aquí se registra y se
            // puede deshacer.
            journalled: true,
        }
    }

    /// La proyección de la ayuda.
    fn vista_ayuda(&self) -> Option<crate::dto::HelpView> {
        let a = self.ayuda.as_ref()?;
        Some(a.vista(self.lang, self.efectos, self.visor.is_some()))
    }

    /// Las teclas mientras la ayuda está abierta.
    ///
    /// FIJAS a propósito, como las de la paleta y por el mismo motivo: el
    /// catálogo no tiene comandos para «filtrar esta lista», «cambiar de
    /// mitad» o «seguir este enlace». Son las que la propia ayuda anuncia en
    /// su pie (`help-hint-gui`), y esa cadena y este `match` cambian juntos.
    fn tecla_en_ayuda(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // `Ctrl+P` sale de la ayuda a la paleta, que es lo que su pie
        // promete. Los DOS cambios viajan en el mismo parche: un renderer que
        // solo recibiera el de la paleta seguiría pintando la ayuda debajo.
        if k.ctrl && !k.alt && !k.meta && k.key.eq_ignore_ascii_case("p") {
            self.ayuda = None;
            self.paleta = Some(norte_frontend::palette_state::Palette::new(
                self.filas_de_paleta(),
            ));
            let cambios = vec![
                ViewChange::Help { help: None },
                ViewChange::Palette {
                    palette: self.vista_paleta(),
                },
            ];
            return (self.aplicada(), vec![self.parche(cambios)]);
        }
        let Some(a) = self.ayuda.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match k.key.as_str() {
            "Escape" | "esc" => {
                // Filtrando, `esc` deja de filtrar y no cierra: cerrar la
                // ayuda entera por abandonar una búsqueda es perder la
                // página que se estaba leyendo.
                if a.estado.filtering() {
                    a.estado.end_filter();
                } else {
                    self.ayuda = None;
                }
            }
            "Tab" | "tab" => a.estado.toggle_focus(),
            "ArrowDown" | "down" => a.estado.down(),
            "ArrowUp" | "up" => a.estado.up(),
            // La página la da el MODELO, que sabe lo que significa en cada
            // mitad: en la lateral camina y enseña UNA vez, y en el cuerpo
            // mueve el scroll y no el cursor, porque una página es un
            // movimiento sobre prosa. Repetir `down()` diez veces hacía diez
            // transiciones de página por tecla.
            "PageDown" | "pgdn" => a.estado.page_down(PAGINA_DE_AYUDA),
            "PageUp" | "pgup" => a.estado.page_up(PAGINA_DE_AYUDA),
            "Backspace" | "backspace" => {
                if a.estado.filtering() {
                    a.estado.backspace();
                } else if !a.estado.back() {
                    // En la raíz, «atrás» es cerrar: el lector no tiene a
                    // dónde volver y una tecla que no hace nada se lee como
                    // una ventana colgada.
                    self.ayuda = None;
                }
            }
            "Enter" | "enter" => return self.enter_en_ayuda(backend, buzon),
            "/" if !a.estado.filtering() => a.estado.start_filter(),
            otra => {
                // Una tecla de TEXTO es un punto de código, no un nombre de
                // tecla (`ArrowLeft` no se teclea), y solo cuenta con el
                // filtro abierto: teclear «d» leyendo una página no puede
                // ponerse a filtrar sola.
                let mut chars = otra.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if a.estado.filtering() && !k.ctrl && !k.alt && !k.meta => {
                        a.estado.push_char(c);
                    }
                    _ => return (self.aplicada(), Vec::new()),
                }
            }
        }
        (self.aplicada(), self.parche_de_ayuda(backend, buzon))
    }

    /// `enter` sobre la ayuda: abrir la página elegida, seguir un enlace o
    /// correr un comando.
    fn enter_en_ayuda(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(a) = self.ayuda.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        if a.estado.focus() == norte_frontend::help::Focus::Topics {
            a.estado.open_selected();
            return (self.aplicada(), self.parche_de_ayuda(backend, buzon));
        }
        let i = a.estado.action_cursor();
        self.activar_en_ayuda(u32::try_from(i).unwrap_or(u32::MAX), backend, buzon)
    }

    /// Actúa sobre la fila `i` del cuerpo, venga de `enter` o de un click.
    ///
    /// La comprobación de si se PUEDE vive aquí, en el host, y no en quien
    /// pinta: el renderer no le pone escuchador a una fila apagada, pero el
    /// teclado no pasa por ahí, así que delegarla dejaba que `enter` corriera
    /// una fila atenuada.
    fn activar_en_ayuda(
        &mut self,
        i: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let (lang, efectos, hay_visor) = (self.lang, self.efectos, self.visor.is_some());
        let Some(a) = self.ayuda.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let i = i as usize;
        let veredicto = a.accion_ejecutable(i, lang, efectos, hay_visor);
        // Señalar la fila es la mitad del click que NO ejecuta, y se hace
        // igual: tras seguir un enlace, la siguiente flecha se mueve por
        // donde el lector señaló.
        a.senalar(i);
        match veredicto {
            Ok(accion) => self.actuar_en_ayuda(Some(accion), backend, buzon),
            Err(clave) if clave.is_empty() => {
                // Una fila que ya no existe: el renderer iba un frame por
                // detrás, y eso no es un error.
                (Self::obsoleta(StaleAction::Modal), Vec::new())
            }
            Err(clave) => {
                // Atenuada: se dice por qué y la página SIGUE abierta. Cerrar
                // la ayuda para negarse sería quitarle al lector la página
                // donde está la explicación.
                self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, &clave)));
                let cambios = vec![
                    ViewChange::Status(self.status.clone()),
                    ViewChange::Help {
                        help: self.vista_ayuda(),
                    },
                ];
                (
                    ActionAck::Unavailable { reason_key: clave },
                    vec![self.parche(cambios)],
                )
            }
        }
    }

    /// Lo que hace una acción del cuerpo, venga de `enter` o de un click.
    fn actuar_en_ayuda(
        &mut self,
        accion: Option<norte_frontend::help::Action>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match accion {
            Some(norte_frontend::help::Action::Open(id)) => {
                if let Some(a) = self.ayuda.as_mut() {
                    a.estado.open(&id);
                }
                (self.aplicada(), self.parche_de_ayuda(backend, buzon))
            }
            Some(norte_frontend::help::Action::Run(cmd)) => {
                // Correr cierra la ayuda: el comando actúa sobre el listado
                // que la ayuda estaba tapando. El cierre viaja PRIMERO y en
                // su propio parche, de modo que el acuse que devuelve el
                // efecto sigue apuntando a la actualización que lo refleja.
                self.ayuda = None;
                let cierre = self.parche(vec![ViewChange::Help { help: None }]);
                let Some(efecto) = efecto_de(&cmd, 1) else {
                    let (ack, mut resto) = self.no_implementado(&cmd);
                    let mut envios = vec![cierre];
                    envios.append(&mut resto);
                    return (ack, envios);
                };
                let (ack, mut resto) = self.aplicar_efecto(efecto, backend, buzon);
                let mut envios = vec![cierre];
                envios.append(&mut resto);
                (ack, envios)
            }
            None => (self.aplicada(), Vec::new()),
        }
    }

    /// Un click en una fila de la lateral de la ayuda: ENSEÑA lo que haya,
    /// que es lo mismo que hace la flecha.
    fn elegir_pagina(
        &mut self,
        row: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(a) = self.ayuda.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        a.estado.click_row(row as usize);
        (self.aplicada(), self.parche_de_ayuda(backend, buzon))
    }

    /// Las teclas mientras el visor está abierto.
    ///
    /// Resuelven con el mapa de la pantalla `viewer`, y lo que no está ligado
    /// ahí NO cae al listado: un visor abierto que dejara pasar `F8` sería un
    /// borrado con la pantalla tapada.
    fn tecla_en_visor(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Ok(chord) = k.to_chord() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-key-unmapped".to_owned(),
                },
                Vec::new(),
            );
        };
        let Resolution::Run { command, count } = self.resolver_visor.push(chord) else {
            // Prefijo a medias, contador o nada: el visor no tiene barra de
            // estado propia todavía, así que no hay nada que pintar.
            return (self.aplicada(), Vec::new());
        };
        if command == "app.help" {
            // La ayuda es de la APLICACIÓN y no del visor, así que no está en
            // su lista de comandos —y no puede estarlo: las dos listas son
            // disjuntas a propósito—. Se atiende aquí para que `F1` con el
            // visor abierto abra la página del visor y no conteste «aquí no».
            return self.abrir_ayuda(backend, buzon);
        }
        let Some(efecto) = crate::commands::efecto_visor_de(&command, count.times()) else {
            // En el catálogo y ligado a esta pantalla, pero este host no lo
            // hace: se dice, con la misma frase que el TUI.
            let frase =
                norte_frontend::keymap::unavailable_message(&command, Availability::NotHere);
            self.status.message = Some(clamp_display(frase));
            let cambio = ViewChange::Status(self.status.clone());
            return (
                ActionAck::Unavailable {
                    reason_key: "cmd-not-here".to_owned(),
                },
                vec![self.parche(vec![cambio])],
            );
        };
        let alto = self.alto_del_visor();
        let Some(v) = self.visor.as_mut() else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        let pasos = |n: i64| usize::try_from(n.abs()).unwrap_or(usize::MAX);
        match efecto {
            crate::commands::EfectoVisor::Cerrar => {
                self.visor = None;
                self.visor_en_vuelo = None;
            }
            crate::commands::EfectoVisor::Linea(n) if n < 0 => v.scroll_up(pasos(n)),
            crate::commands::EfectoVisor::Linea(n) => v.scroll_down(pasos(n)),
            crate::commands::EfectoVisor::Pagina(n) if n < 0 => {
                v.scroll_up(pasos(n).saturating_mul(alto));
            }
            crate::commands::EfectoVisor::Pagina(n) => {
                v.scroll_down(pasos(n).saturating_mul(alto));
            }
            crate::commands::EfectoVisor::Extremo { al_final: false } => v.scroll_top(),
            crate::commands::EfectoVisor::Extremo { al_final: true } => v.scroll_bottom(),
            crate::commands::EfectoVisor::Hex => v.toggle_hex(),
            crate::commands::EfectoVisor::Encoding => v.cycle_encoding(),
            crate::commands::EfectoVisor::EncodingAuto => v.reset_encoding(),
        }
        // Un PARCHE del visor. La foto entera mandaba, por cada línea de
        // scroll, las filas visibles de todos los listados que hay debajo.
        let cambio = ViewChange::Viewer {
            viewer: self.vista_visor(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Guarda el catálogo de atributos de un esquema y repinta.
    ///
    /// Manda una FOTO y no un parche: el catálogo cambia cómo se leen celdas
    /// que ya viajaron —un modo que llegó como número y ahora es `rwx`—, y
    /// eso no es un cambio de filas, es otra lectura de todo lo que hay.
    fn aplicar_catalogo(
        &mut self,
        scheme: String,
        catalogo: norte_proto::AttrCatalog,
    ) -> BridgeEnvelope<UiUpdate> {
        self.catalogos.insert(scheme, catalogo);
        let snap = self.snapshot();
        self.sobre(UiUpdate::Snapshot(Box::new(snap)))
    }

    /// Pide el contenido de la entrada bajo el cursor para abrir el visor.
    fn pedir_visor(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(entrada) = self.hueco().pane.selected().cloned() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-nothing-to-view".to_owned(),
                },
                Vec::new(),
            );
        };
        if entrada.kind == EntryKind::Dir {
            // Ver un directorio es entrar en él, y eso ya tiene su tecla.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-cannot-view-dir".to_owned(),
                },
                Vec::new(),
            );
        }
        self.token += 1;
        let token = RequestToken(self.token);
        self.visor_en_vuelo = Some(token);
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let path = entrada.path.clone();
        tokio::spawn(async move {
            // Un byte de más que el presupuesto: es lo que delata que el
            // fichero seguía. El resto NO se lee.
            let lectura = backend.read(
                path.clone(),
                Some(norte_proto::ByteRange {
                    offset: 0,
                    len: Some(VISOR_CAP + 1),
                }),
            );
            // Con plazo: un montaje colgado no puede dejar la tecla F3 sin
            // desenlace para siempre.
            let leido = match tokio::time::timeout(PLAZO_VISOR, lectura).await {
                Ok(r) => r,
                // El wire no tiene «se acabó el tiempo»; lo que hubo es una
                // lectura que no llegó, y para el usuario es lo mismo que un
                // provider que no responde.
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Contenido(Box::new((token, path, leido))))
                .await;
        });
        (self.aplicada(), Vec::new())
    }

    /// Abre el visor con lo que se leyó.
    fn abrir_visor(
        &mut self,
        token: RequestToken,
        path: VPath,
        leido: Result<Vec<u8>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        if self.visor_en_vuelo != Some(token) {
            // El usuario cerró el visor, pidió otro fichero o se fue a otro
            // sitio mientras esto volaba. Abrirlo ahora sería abrir una
            // ventana que nadie ha pedido — y cambiarle el teclado de mapa.
            return None;
        }
        self.visor_en_vuelo = None;
        match leido {
            Ok(mut bytes) => {
                let cap = usize::try_from(VISOR_CAP).unwrap_or(usize::MAX);
                let truncado = bytes.len() > cap;
                if truncado {
                    bytes.truncate(cap);
                }
                self.visor = Some(norte_frontend::viewer::Viewer::new(path, bytes, truncado));
            }
            Err(e) => {
                // No se pudo leer: se DICE y no se abre un visor vacío que
                // parezca un fichero de cero bytes.
                // El texto de un error puede venir de un peer más nuevo
                // (`LimitExceeded` con un token desconocido, una huella de
                // host) y acaba en el DOM: se enmascara como cualquier otro
                // texto ajeno.
                let (pintable, _) = norte_frontend::display_name(format!("{e}").as_bytes());
                self.status.message = Some(clamp_display(pintable));
                let cambio = ViewChange::Status(self.status.clone());
                return Some(self.parche(vec![cambio]));
            }
        }
        let snap = self.snapshot();
        Some(self.sobre(UiUpdate::Snapshot(Box::new(snap))))
    }

    /// La tecla, cuando el buscador incremental está abierto.
    ///
    /// `None` = esta tecla no es suya y sigue su camino normal (una tecla de
    /// función, un atajo con modificador): abrir el buscador NO desconecta el
    /// resto del teclado, solo se queda el texto, el borrado y las tres
    /// teclas que lo gobiernan.
    fn tecla_en_quick(
        &mut self,
        k: &crate::keys::KeyInput,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        if k.ctrl || k.alt || k.meta {
            return None;
        }
        let pane = &mut self.hueco_mut().pane;
        match k.key.as_str() {
            "Escape" | "esc" => pane.quick_cancel(),
            "Enter" | "enter" => {
                pane.quick_confirm();
            }
            "Backspace" | "backspace" => pane.quick_backspace(),
            "ArrowDown" | "down" => pane.quick_down(),
            "ArrowUp" | "up" => pane.quick_up(),
            otro => {
                let mut chars = otro.chars();
                let (Some(c), None) = (chars.next(), chars.next()) else {
                    return None;
                };
                pane.quick_char(c);
            }
        }
        Some((self.aplicada(), vec![self.parche_filas()]))
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
                // Una tecla actúa sobre lo que hay AHORA bajo el cursor, así
                // que la generación es la de este mismo instante.
                let key = RowKey(self.hueco().pane.cursor() as u64);
                let generation = self.hueco().pane.listing_epoch();
                self.navegacion(
                    &UiAction::Activate {
                        slot_id: slot,
                        key,
                        generation,
                    },
                    backend,
                    buzon,
                )
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
                let generation = self.hueco().pane.listing_epoch();
                self.marcar(slot, key, generation)
            }
            Efecto::DesmarcarTodo => {
                self.hueco_mut().pane.clear_marks();
                (self.aplicada(), vec![self.parche_filas()])
            }
            Efecto::Foco { atras } => self.mover_foco(atras),
            Efecto::Destino => self.designar_destino(),
            Efecto::BuscarRapido => {
                // Filtrar es el modo por defecto: es el que no mueve el
                // listado bajo el cursor mientras se teclea.
                self.hueco_mut()
                    .pane
                    .quick_start(norte_frontend::nav::Mode::Filter);
                (self.aplicada(), vec![self.parche_filas()])
            }
            Efecto::CrearDirectorio | Efecto::Borrar { .. }
                if self.efectos == crate::commands::Efectos::SoloLectura =>
            {
                Self::no_muta()
            }
            Efecto::Paleta => {
                // Las filas se construyen AQUÍ, al abrir, y se congelan: es
                // lo que el modelo compartido espera (pliega el haystack de
                // cada fila una vez, no por tecla).
                self.paleta = Some(norte_frontend::palette_state::Palette::new(
                    self.filas_de_paleta(),
                ));
                let cambio = ViewChange::Palette {
                    palette: self.vista_paleta(),
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            Efecto::Ayuda => self.abrir_ayuda(backend, buzon),
            Efecto::Ajustes => self.abrir_ajustes(),
            Efecto::Ver => self.pedir_visor(backend, buzon),
            Efecto::CrearDirectorio => self.pedir_mkdir(),
            Efecto::Borrar { permanente } => self.pedir_borrado(permanente),
        }
    }

    /// Abre la confirmación de un borrado. NO borra.
    ///
    /// Todas las vías —tecla, menú, gesto— pasan por aquí. Una operación
    /// destructiva con dos puertas acaba teniendo una sin cerrojo, y la que
    /// se olvida es siempre la que no se usa a diario.
    fn pedir_borrado(&mut self, permanente: bool) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let hueco = self.hueco();
        let mut paths: Vec<VPath> = hueco.pane.marked_paths();
        if paths.is_empty() {
            // Sin marcas, lo que hay bajo el cursor. Sin cursor, nada que
            // borrar: y eso no abre un diálogo sobre un lote vacío.
            match hueco.pane.selected() {
                Some(e) => paths.push(e.path.clone()),
                None => {
                    return (
                        ActionAck::Unavailable {
                            reason_key: "msg-nothing-selected".to_owned(),
                        },
                        Vec::new(),
                    );
                }
            }
        }
        // Los nombres del cuerpo son de un atacante potencial: se pintan con
        // el saneado canónico y acotados, igual que en el listado.
        let cuerpo: Vec<String> = paths
            .iter()
            .take(16)
            .map(|p| {
                let (texto, _hostil) = norte_frontend::path_display(p);
                clamp_display(texto)
            })
            .collect();
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: if permanente {
                "modal-delete-permanent-title"
            } else {
                "modal-delete-title"
            }
            .to_owned(),
            body: cuerpo,
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    // Lo destructivo se DICE en el propio contrato del
                    // diálogo: el renderer no tiene que adivinar cuál de las
                    // respuestas borra.
                    destructive: true,
                },
                DialogChoice {
                    id: "cancel".to_owned(),
                    label_key: "dialog-cancel".to_owned(),
                    destructive: false,
                },
            ],
            input: None,
            input_hostile: false,
        };
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            input_crudo: String::new(),
            al_confirmar: Some(Pendiente::Borrar { paths, permanente }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Pide el catálogo de atributos de esta localización, si hace falta.
    ///
    /// Solo si hay columnas `attr:` configuradas y aún no se tiene el de su
    /// esquema: preguntar por un catálogo que nadie va a leer es un viaje de
    /// más en cada `cd`.
    fn pedir_catalogo(
        &self,
        dir: &VPath,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        if self.attrs_de(dir).is_empty() || self.catalogos.contains_key(dir.scheme()) {
            return;
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let dir = dir.clone();
        let scheme = dir.scheme().to_owned();
        tokio::spawn(async move {
            // Un catálogo que no llega no rompe nada: las celdas se pintan
            // opacas, que es exactamente lo que se sabe de ellas.
            if let Ok(catalogo) = backend.attr_catalog(dir).await {
                let _ = buzon
                    .send(Mensaje::Catalogo(Box::new((scheme, catalogo))))
                    .await;
            }
        });
    }

    /// Abre el diálogo de una op de agente que espera decisión.
    ///
    /// Las rutas vienen REDACTADAS del servidor y son solo display: jamás se
    /// reparsean a una operación —la op real va ligada al `approval_id`—, y
    /// se pintan con el saneado canónico porque las controla quien pidió la
    /// operación.
    fn abrir_aprobacion(
        &mut self,
        req: &norte_proto::methods::PolicyApprovalRequired,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let mut cuerpo: Vec<String> = Vec::new();
        cuerpo.push(clamp_display(norte_encoding::mask_terminal_hazards(
            &req.op,
        )));
        for p in req.paths.iter().take(16) {
            cuerpo.push(clamp_display(norte_encoding::mask_terminal_hazards(p)));
        }
        // Si la lista viene RECORTADA hay que decirlo: aprobar creyendo que
        // son tres rutas cuando son mil es aprobar otra cosa (0.36.0).
        let total = if req.paths_total == 0 {
            req.paths.len() as u64
        } else {
            req.paths_total
        };
        if total > req.paths.len() as u64 {
            cuerpo.push(clamp_display(norte_i18n::ta(
                "modal-approval-truncated",
                &[("total", &total.to_string())],
            )));
        }
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-approval-title".to_owned(),
            body: cuerpo,
            choices: vec![
                DialogChoice {
                    id: "approve".to_owned(),
                    label_key: "dialog-approve".to_owned(),
                    // Aprobar una mutación de un agente ES destructivo: el
                    // renderer la pinta como tal, y Enter no la dispara sola
                    // porque no hay respuesta por defecto.
                    destructive: true,
                },
                DialogChoice {
                    id: "deny".to_owned(),
                    label_key: "dialog-deny".to_owned(),
                    destructive: false,
                },
            ],
            input: None,
            input_hostile: false,
        };
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            input_crudo: String::new(),
            al_confirmar: Some(Pendiente::Decidir {
                approval_id: req.approval_id,
            }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// Abre el prompt de crear directorio, con su campo de texto vacío.
    fn pedir_mkdir(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let dir = self.hueco().pane.dir().clone();
        let (donde, _hostil) = norte_frontend::path_display(&dir);
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-mkdir-title".to_owned(),
            body: vec![clamp_display(donde)],
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    destructive: false,
                },
                DialogChoice {
                    id: "cancel".to_owned(),
                    label_key: "dialog-cancel".to_owned(),
                    destructive: false,
                },
            ],
            // Con campo de texto: es lo que hace que el renderer sepa que
            // aquí se teclea, sin que tenga que deducirlo del título.
            input: Some(String::new()),
            input_hostile: false,
        };
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            input_crudo: String::new(),
            al_confirmar: Some(Pendiente::CrearDirectorio { dir }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Teclea en el campo de un diálogo.
    ///
    /// El renderer manda el texto ENTERO tras la edición y no un delta: el
    /// caret es suyo, y reconstruirlo en Rust sería mantener dos ideas de
    /// dónde está el cursor.
    fn escribir_en_dialogo(
        &mut self,
        id: ModalId,
        texto: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(dialogo) = self.dialogos.iter_mut().find(|d| d.id == id) else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        if dialogo.vista.input.is_none() {
            // Un diálogo de decisión no tiene dónde escribir, y aceptar texto
            // que nadie va a leer sería peor que decirlo.
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        if texto.len() > MAX_NOMBRE {
            // Ni se recorta ni se acepta a medias: un nombre no es una
            // cadena de pantalla, y recortarlo es inventarse otro.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-name-too-long".to_owned(),
                },
                Vec::new(),
            );
        }
        texto.clone_into(&mut dialogo.input_crudo);
        // Lo que se PINTA es otra cosa: enmascarado (un `U+202E` en el
        // nombre que te van a pedir aprobar se ve) y acotado.
        let (pintable, hostil) = norte_frontend::display_name(texto.as_bytes());
        dialogo.vista.input = Some(clamp_display(pintable));
        dialogo.vista.input_hostile = hostil;
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Responde a un diálogo.
    ///
    /// Un id que no es el del diálogo abierto —porque ya se contestó, porque
    /// el renderer tardó— no hace nada y lo dice: confirmar dos veces NO
    /// borra dos veces.
    fn responder_dialogo(
        &mut self,
        id: ModalId,
        choice: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(pos) = self.dialogos.iter().position(|d| d.id == id) else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // Una respuesta que el diálogo no ofreció no se interpreta: no hay
        // respuestas implícitas en una superficie de decisión.
        if !self.dialogos[pos]
            .vista
            .choices
            .iter()
            .any(|c| c.id == choice)
        {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        let dialogo = self.dialogos.remove(pos);
        let mut salidas = Vec::new();
        // `confirm` es la respuesta afirmativa de los diálogos normales;
        // `approve`, la de una aprobación. Nombres distintos a propósito: en
        // una superficie de seguridad, «confirmar» y «aprobar» no deberían
        // poder confundirse en un renderer.
        if choice == "confirm" || choice == "approve" {
            match dialogo.al_confirmar {
                Some(Pendiente::Borrar { paths, permanente }) => {
                    Self::lanzar_borrado(paths, permanente, backend, buzon);
                }
                Some(Pendiente::CrearDirectorio { dir }) => {
                    let nombre = dialogo.input_crudo.clone();
                    // El nombre se valida AQUÍ, con la misma regla que
                    // cualquier otro segmento: ni vacío, ni `/`, ni NUL, ni
                    // `.`/`..`. Un nombre que no vale no encola nada y lo
                    // dice; el texto tecleado no se pierde porque el diálogo
                    // se vuelve a abrir con él.
                    let Ok(seg) = norte_proto::Segment::new(nombre.clone().into_bytes()) else {
                        self.status.message = Some(clamp_display(norte_i18n::t("err-bad-name")));
                        let cambio = ViewChange::Status(self.status.clone());
                        salidas.push(self.parche(vec![cambio]));
                        return (self.aplicada(), salidas);
                    };
                    let destino = dir.join(seg);
                    let backend = Arc::clone(backend);
                    let buzon = buzon.clone();
                    tokio::spawn(async move {
                        match backend.mkdir(destino).await {
                            Ok(task) => {
                                let _ = buzon.send(Mensaje::TaskNueva(Box::new(task))).await;
                            }
                            Err(e) => {
                                let _ = buzon.send(Mensaje::TaskFallida(Box::new(e))).await;
                            }
                        }
                    });
                }
                Some(Pendiente::Decidir { approval_id }) => {
                    // Solo `approve` aprueba. Cualquier otra respuesta —y el
                    // cierre del diálogo— DENIEGA: una decisión de seguridad
                    // no tiene respuesta por defecto que diga «sí».
                    let backend = Arc::clone(backend);
                    tokio::spawn(async move {
                        let _ = backend.policy_decide(approval_id, true).await;
                    });
                }
                None => {}
            }
        } else if let Some(Pendiente::Decidir { approval_id }) = dialogo.al_confirmar {
            // Denegar explícitamente, y también al cerrar: dejar al agente
            // esperando una respuesta que no llega es peor que decirle que no.
            let backend = Arc::clone(backend);
            tokio::spawn(async move {
                let _ = backend.policy_decide(approval_id, false).await;
            });
        }
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        salidas.push(self.parche(vec![cambio]));
        (self.aplicada(), salidas)
    }

    /// Encola una Task por entrada y engancha su progreso al actor.
    fn lanzar_borrado(
        paths: Vec<VPath>,
        permanente: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let mode = if permanente {
            norte_proto::DeleteMode::Permanent
        } else {
            norte_proto::DeleteMode::Trash
        };
        for path in paths {
            let backend = Arc::clone(backend);
            let buzon = buzon.clone();
            tokio::spawn(async move {
                match backend.delete(path, mode).await {
                    Ok(task) => {
                        let _ = buzon.send(Mensaje::TaskNueva(Box::new(task))).await;
                    }
                    Err(e) => {
                        let _ = buzon.send(Mensaje::TaskFallida(Box::new(e))).await;
                    }
                }
            });
        }
    }

    /// Mete una Task recién encolada en el tablero y deja su progreso
    /// bombeando hacia el actor.
    fn registrar_task(
        &mut self,
        task: crate::backend::HostTask,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let id = task.id.get();
        let ajena = task.foreign;
        if self.tasks.len() >= MAX_TASKS {
            // El tablero está acotado: lo más viejo TERMINADO se cae antes de
            // que la memoria del host dependa de cuántas operaciones lanzó
            // alguien.
            if let Some(viejo) = self
                .tasks
                .iter()
                .find(|(_, t)| Self::terminal(t.vista.state))
                .map(|(k, _)| *k)
            {
                self.tasks.remove(&viejo);
            }
        }
        let mut rx = task.progress.clone();
        let mut vista = Self::vista_de(&rx.borrow());
        vista.foreign = ajena;
        self.tasks.insert(
            id,
            TaskViva {
                vista,
                cancel: task.cancel,
            },
        );
        let buzon2 = buzon.clone();
        tokio::spawn(async move {
            // El estado de AHORA ya lo proyectó el registro; lo que bombea
            // esto son los CAMBIOS. El terminal va por la misma cola ordenada
            // que todo lo demás y se manda antes de soltar el canal: un
            // desenlace que se pierde deja al usuario mirando un progreso que
            // no avanza.
            while rx.changed().await.is_ok() {
                let snapshot = rx.borrow_and_update().clone();
                let terminal = matches!(
                    snapshot.state,
                    norte_proto::TaskState::Completed
                        | norte_proto::TaskState::Cancelled
                        | norte_proto::TaskState::Failed { .. }
                );
                if buzon2
                    .send(Mensaje::Progreso(Box::new(snapshot)))
                    .await
                    .is_err()
                    || terminal
                {
                    return;
                }
            }
        });
        let cambio = ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// Aplica un snapshot de progreso al tablero.
    fn progreso(&mut self, p: &norte_proto::TaskProgress) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(viva) = self.tasks.get_mut(&p.task_id.get()) else {
            return Vec::new();
        };
        let ajena = viva.vista.foreign;
        viva.vista = Self::vista_de(p);
        // De quién es la task no lo dice el progreso: lo dice de dónde vino.
        viva.vista.foreign = ajena;
        let cambio = ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// Pide la cancelación de una task. Idempotente por contrato: pedirla dos
    /// veces no es un error ni cambia nada.
    fn cancelar(&mut self, task_id: u64) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(viva) = self.tasks.get(&task_id) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        (viva.cancel)();
        (self.aplicada(), Vec::new())
    }

    fn vistas_de_dialogos(&self) -> Vec<DialogView> {
        self.dialogos.iter().map(|d| d.vista.clone()).collect()
    }

    fn vistas_de_tasks(&self) -> Vec<TaskView> {
        self.tasks.values().map(|t| t.vista.clone()).collect()
    }

    fn terminal(estado: TaskStateView) -> bool {
        matches!(
            estado,
            TaskStateView::Done | TaskStateView::Failed | TaskStateView::Cancelled
        )
    }

    /// Proyecta un snapshot del daemon a lo que el renderer pinta.
    fn vista_de(p: &norte_proto::TaskProgress) -> TaskView {
        let porcentaje = p.bytes_total.filter(|t| *t > 0).map(|total| {
            let hecho = p.bytes_done.min(total);
            u8::try_from(hecho.saturating_mul(100) / total).unwrap_or(100)
        });
        TaskView {
            task_id: p.task_id.get(),
            kind: format!("{:?}", p.kind).to_lowercase(),
            state: match p.state {
                norte_proto::TaskState::Completed => TaskStateView::Done,
                norte_proto::TaskState::Cancelled => TaskStateView::Cancelled,
                norte_proto::TaskState::Failed { .. } => TaskStateView::Failed,
                norte_proto::TaskState::Running | norte_proto::TaskState::Paused => {
                    TaskStateView::Running
                }
                // Un estado que este host todavía no conoce se pinta como
                // encolado: es lo único que no miente sobre algo que sigue
                // vivo (`TaskState` es no exhaustivo por contrato del wire).
                _ => TaskStateView::Queued,
            },
            percent: porcentaje,
            detail: p.current.as_ref().map(|path| {
                let (texto, _hostil) = norte_frontend::path_display(path);
                clamp_display(texto)
            }),
            foreign: false,
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
            UiAction::Activate {
                slot_id,
                key,
                generation,
            } => {
                let (slot_id, key, generation) = (*slot_id, *key, *generation);
                let Some(i) = self.fila_de(slot_id, key, generation) else {
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
        // El drenaje vive MÁS que la primera página: se marca aquí y solo lo
        // releva otra navegación del mismo hueco.
        self.hueco_mut().drenando = Some(token);

        self.pedir_catalogo(&destino, backend, buzon);
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let dir = destino.clone();
        let slot = self.activo();
        let attrs = self.attrs_de(&destino);
        tokio::spawn(async move {
            let stream = backend.list(dir.clone(), attrs).await;
            let res = Estado::primera_pagina(stream, slot, token, buzon.clone()).await;
            // Si el actor ya no está, la respuesta no le importa a nadie.
            let _ = buzon
                .send(Mensaje::Listado(Box::new((token, slot, dir, res))))
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
        // Una task viva al cerrar es trabajo sin terminar, lo diga la sesión
        // o no: cerrar a mitad de una copia y reportar «todo bien» es
        // exactamente lo que este informe existe para no hacer.
        let hay_tasks = self.tasks.values().any(|t| {
            matches!(
                t.vista.state,
                crate::dto::TaskStateView::Queued | crate::dto::TaskStateView::Running
            )
        });
        if !self.sesion.owner || self.sesion.futuro {
            // Una ventana suelta no escribe, y una sesión del futuro no se
            // machaca.
            return ShutdownReport {
                incomplete: hay_tasks,
            };
        }
        let mut body = self.capturar_sesion();
        let vivos: Vec<SlotId> = self.huecos.keys().map(|id| SlotId(*id)).collect();
        if self.sesion.policy.prepare(&mut body, &vivos, 0).is_none() {
            // Nada cambió desde lo último que se mandó.
            return ShutdownReport {
                incomplete: hay_tasks,
            };
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
        ShutdownReport {
            incomplete: hay_tasks,
        }
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
        self.filas_de(self.hueco())
    }

    /// Las filas visibles de un hueco cualquiera.
    fn filas_de(&self, hueco: &Hueco) -> Vec<RowView> {
        let primera = usize::try_from(hueco.primera_visible).unwrap_or(0);
        let cuantas = usize::try_from(hueco.visibles).unwrap_or(0);
        hueco
            .pane
            .entries()
            .iter()
            .enumerate()
            .skip(primera)
            .take(cuantas.min(MAX_ROWS_PER_BATCH))
            .map(|(i, e)| self.fila(hueco, i, e))
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

    /// Sondea lo que la ventana visible de un hueco todavía no sabe.
    ///
    /// El listado local es PEREZOSO a propósito (#52): `readdir` da el tipo
    /// pero no el tamaño, y statear medio millón de entradas para pintar
    /// cuarenta filas es justo lo que esa decisión evita. Quien enseña
    /// columnas de tamaño y fecha tiene que pedirlas para lo que se ve — el
    /// TUI lo hace desde su bucle, y esto es lo mismo con la ventana que el
    /// renderer declaró. La regla de QUÉ hace falta es la compartida
    /// (`needs_stat_at`), no una de aquí.
    fn sondear(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let attrs = self
            .huecos
            .get(&slot)
            .map(|h| self.attrs_de(h.pane.dir()))
            .unwrap_or_default();
        let Some(hueco) = self.huecos.get_mut(&slot) else {
            return;
        };
        // Una sonda por hueco, y se comprueba ANTES de elegir candidatos: si
        // se eligen y luego se abandona la tanda, esos paths quedan marcados
        // como sondeados sin haberlo sido, y no se piden nunca más. Sin este
        // orden, un scroll con debounce apilaba tandas de doscientos viajes
        // contra la misma conexión y además se comía filas por el camino.
        if hueco.sondeando {
            return;
        }
        let primera = usize::try_from(hueco.primera_visible).unwrap_or(0);
        let cuantas = usize::try_from(hueco.visibles).unwrap_or(0);
        let candidatos: Vec<VPath> = hueco
            .pane
            .needs_stat_at(primera..primera.saturating_add(cuantas))
            .into_iter()
            .filter(|p| !hueco.sondeados.contains(p))
            .take(MAX_SONDEOS)
            .collect();
        if candidatos.is_empty() {
            return;
        }
        for p in &candidatos {
            hueco.sondeados.insert(p.clone());
        }
        let dir = hueco.pane.dir().clone();
        hueco.sondeando = true;
        let cancelar = hueco.cancelar_sondeo.clone();
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            use futures::StreamExt as _;
            // En PARALELO acotado: una sesión remota no puede pagar N viajes
            // en serie (200 sondas a 80 ms de ida y vuelta son dieciséis
            // segundos), y con plazo, porque un provider colgado no puede
            // llevarse por delante las otras 199.
            let sondas: Vec<(VPath, Entry)> = futures::stream::iter(candidatos)
                .map(|p| {
                    let backend = Arc::clone(&backend);
                    let attrs = attrs.clone();
                    async move {
                        let stat = backend.stat(p.clone(), attrs);
                        match tokio::time::timeout(PLAZO_SONDEO, stat).await {
                            // Un sondeo que falla o que tarda no es un error
                            // de pantalla: esa celda se queda en blanco y no
                            // se vuelve a pedir.
                            Ok(Ok(e)) => Some((p, e)),
                            _ => None,
                        }
                    }
                })
                .buffer_unordered(SONDEOS_A_LA_VEZ)
                .filter_map(|x| async move { x })
                .collect()
                .await;
            if cancelar.load(std::sync::atomic::Ordering::SeqCst) || sondas.is_empty() {
                // El listado cambió mientras se sondeaba: lo que vuelve no
                // describe la pantalla que hay.
                let _ = buzon
                    .send(Mensaje::Hidratado(Box::new((dir, slot, Vec::new()))))
                    .await;
                return;
            }
            let _ = buzon
                .send(Mensaje::Hidratado(Box::new((dir, slot, sondas))))
                .await;
        });
    }

    /// Pega un lote del relleno al listado que lo pidió.
    ///
    /// `None` si el hueco desapareció o el lote es de una navegación ya
    /// relevada: pegarlo sería mezclar dos árboles en una pantalla.
    fn aplicar_lote(
        &mut self,
        slot: u32,
        token: RequestToken,
        batch: Vec<Entry>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let hueco = self.huecos.get_mut(&slot)?;
        if hueco.drenando != Some(token) {
            // Un lote de una navegación que ya fue relevada: pegarlo sería
            // mezclar dos árboles en una pantalla.
            return None;
        }
        hueco.pane.extend(batch);
        Some(self.parche_filas_de(slot))
    }

    /// Pega lo que un sondeo averiguó al listado que lo pidió.
    ///
    /// `None` si no hay nada que repintar: el hueco desapareció, o el listado
    /// que se sondeó ya fue relevado —pegarle tamaños a otro directorio sería
    /// mentir sobre lo que se ve—.
    fn aplicar_sondas(
        &mut self,
        slot: u32,
        dir: &VPath,
        sondas: &[(VPath, Entry)],
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let hueco = self.huecos.get_mut(&slot)?;
        hueco.sondeando = false;
        if hueco.pane.dir() != dir {
            // El hueco está en OTRO directorio: pegarle estos tamaños sería
            // mentir sobre lo que se ve. (Un lote de relleno, en cambio, no
            // invalida nada: sube la época y desplaza índices, y aquí se casa
            // por ruta.)
            return None;
        }
        for (pedido, e) in sondas {
            // Por la ruta que se PIDIÓ: la que devuelve el provider puede ser
            // otra ortografía del mismo nombre (NFD en HFS+, otra caja en
            // SMB, el destino de un enlace) y entonces no casa con nada — y
            // como ya está en `sondeados`, no se reintenta jamás.
            hueco.pane.hydrate(pedido, e.size, e.mtime_ms);
        }
        Some(self.parche_filas_de(slot))
    }

    /// Las filas visibles de UN hueco concreto, no del que tenga el foco.
    fn parche_filas_de(&mut self, slot: u32) -> BridgeEnvelope<UiUpdate> {
        let (generacion, primera, filas) = match self.huecos.get(&slot) {
            Some(h) => (h.pane.listing_epoch(), h.primera_visible, self.filas_de(h)),
            None => (0, 0, Vec::new()),
        };
        let cambio = ViewChange::Rows {
            slot_id: slot,
            generation: generacion,
            first_visible: primera,
            rows: filas,
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

    fn fila(&self, hueco: &Hueco, i: usize, e: &Entry) -> RowView {
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
            cells: self.celdas(hueco, e),
        }
    }

    /// El catálogo de la localización de un path, si ya llegó.
    fn catalogo_de(&self, path: &VPath) -> Option<&norte_proto::AttrCatalog> {
        self.catalogos.get(path.scheme())
    }

    /// Las celdas de una fila, una por columna configurada.
    ///
    /// Las construye `norte_frontend::columns::styled_cell`, que es la misma
    /// función que usa el TUI: el formato de un tamaño o de una fecha no
    /// puede depender de quién pinta. `None` es AUSENCIA —un directorio sin
    /// tamaño, un atributo que el provider no mandó— y viaja como tal: jamás
    /// un `0` fabricado.
    fn celdas(&self, hueco: &Hueco, e: &Entry) -> Vec<crate::dto::CellView> {
        use norte_frontend::columns::{ColumnId, ColumnStyle, styled_cell};
        let ahora = ahora_ms();
        self.columnas_de(hueco.pane.dir())
            .iter()
            .filter(|c| !matches!(c, ColumnId::Builtin(norte_frontend::columns::Builtin::Name)))
            .map(|col| {
                let texto = match col {
                    // Las de plugin no viven en la `Entry` sino en el
                    // side-map del pane: se resuelven por ese camino.
                    ColumnId::Plugin { plugin, column } => hueco.pane.plugin_cell(
                        &norte_frontend::columns::plugin_display_id(plugin, column),
                        &e.path,
                    ),
                    otra => styled_cell(
                        e,
                        otra,
                        ahora,
                        &ColumnStyle::default_for_id(otra, self.catalogo_de(&e.path)),
                    ),
                };
                crate::dto::CellView {
                    column: clamp_display(id_pintable(col)),
                    text: texto.map(clamp_display),
                }
            })
            .collect()
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
                slots.push(SlotView::Browser(Box::new(self.browser(id, hueco))));
            } else {
                let nombre = kind_de(&self.arbol, *slot)
                    .map_or_else(|| "unknown".to_owned(), |k| k.as_str().to_owned());
                // El kind sale de un fichero de disposición y `KindId` no
                // valida nada: es texto que puede traer controles, y acaba en
                // el DOM y en un `aria-label`.
                let (pintable, _) = norte_frontend::display_name(nombre.as_bytes());
                slots.push(SlotView::Unsupported {
                    slot_id: id,
                    kind_name: clamp_display(pintable),
                });
            }
        }
        ViewSnapshot {
            connection: self.conexion.clone(),
            layout: self.disposicion(),
            slots,
            focus: Some(self.activo()),
            status: self.status.clone(),
            // Una foto REEMPLAZA lo que el renderer tenga, así que va
            // entera: un resync que se dejara fuera el diálogo abierto
            // dejaría al usuario mirando una pantalla sin la pregunta que
            // está esperando respuesta, con la operación destructiva todavía
            // viva. Lo mismo con el tablero.
            dialogs: self.vistas_de_dialogos(),
            tasks: self.vistas_de_tasks(),
            palette: self.vista_paleta(),
            whichkey: self.vista_whichkey(),
            help: self.vista_ayuda(),
            settings: self.vista_ajustes(),
            viewer: self.vista_visor(),
            locale: self.locale.clone(),
        }
    }

    /// Cuántas líneas se le mandan al visor y cuánto avanza una página.
    ///
    /// Lo dice el renderer (`SetViewerRows`); mientras no lo haya dicho, se
    /// estima con las celdas de la ventana menos el cromo. Es UN número para
    /// las dos cosas a propósito: cuando la estimación y lo que se pinta no
    /// coinciden, una página salta en silencio las líneas recortadas.
    fn alto_del_visor(&self) -> usize {
        self.visor_filas
            .unwrap_or_else(|| usize::from(self.viewport.1.saturating_sub(2)))
            .max(1)
    }

    /// Las filas de la paleta: TODO lo que este host implementa.
    ///
    /// La descripción sale del catálogo Fluent compartido y el atajo del
    /// keymap efectivo, igual que en el TUI: una paleta construida de una
    /// lista a mano enseña atajos que el preset del usuario no tiene.
    fn filas_de_paleta(&self) -> Vec<norte_frontend::palette::Row> {
        use norte_frontend::palette::first_chord;
        crate::commands::todos()
            .into_iter()
            .map(|cmd| norte_frontend::palette::Row {
                key: cmd.to_owned(),
                text: cmd.to_owned(),
                desc: norte_i18n::t_in(self.lang, &format!("help-cmd-{}", cmd.replace('.', "-"))),
                chord: first_chord(cmd, &self.efectivo)
                    .or_else(|| first_chord(cmd, self.resolver_visor_efectivo()))
                    .unwrap_or_else(|| "—".to_owned()),
            })
            .collect()
    }

    /// El efectivo del visor, para buscar el atajo de un comando suyo.
    fn resolver_visor_efectivo(&self) -> &Effective {
        &self.efectivo_visor
    }

    /// La proyección de la paleta.
    fn vista_paleta(&self) -> Option<crate::dto::PaletteView> {
        let p = self.paleta.as_ref()?;
        let filas = p.rows();
        let visibles = p.visible();
        Some(crate::dto::PaletteView {
            query: clamp_display(p.query_display()),
            rows: visibles
                .iter()
                .filter_map(|i| filas.get(*i))
                .map(|r| crate::dto::PaletteRowView {
                    text: clamp_display(r.text.clone()),
                    desc: clamp_display(r.desc.clone()),
                    chord: clamp_display(r.chord.clone()),
                    // Todo lo que la paleta ofrece lo implementa este host:
                    // las filas salen de su propia lista.
                    enabled: true,
                })
                .collect(),
            cursor: (!visibles.is_empty()).then_some(p.cursor() as u64),
            total: filas.len() as u64,
        })
    }

    /// La proyección del panel de continuaciones.
    fn vista_whichkey(&self) -> Option<crate::dto::WhichKeyView> {
        let panel = self.whichkey.as_ref()?;
        Some(crate::dto::WhichKeyView {
            title: clamp_display(panel.title.clone()),
            rows: panel
                .rows
                .iter()
                .map(|r| crate::dto::WhichKeyRowView {
                    chord: clamp_display(r.chord.clone()),
                    label: clamp_display(r.label.clone()),
                    enabled: r.avail == Availability::Here,
                    opens_sequence: r.opens_sequence,
                    reason: clamp_display(r.reason.clone()),
                })
                .collect(),
        })
    }

    /// La proyección del visor, con la ventana de líneas que cabe.
    ///
    /// El alto sale del viewport en CELDAS —la misma rejilla que reparte la
    /// pantalla—, menos el cromo: el visor ocupa la ventana entera.
    fn vista_visor(&self) -> Option<crate::dto::ViewerView> {
        let v = self.visor.as_ref()?;
        let alto = self.alto_del_visor();
        let (path, hostil) = norte_frontend::path_display(&v.path);
        Some(crate::dto::ViewerView {
            path_display: clamp_display(path),
            path_hostile: hostil,
            encoding: v.encoding_name().to_owned(),
            eol: match v.eol() {
                norte_encoding::Eol::Lf => "lf",
                norte_encoding::Eol::CrLf => "crlf",
                norte_encoding::Eol::Cr => "cr",
                norte_encoding::Eol::Mixed => "mixed",
                norte_encoding::Eol::None => "none",
            }
            .to_owned(),
            hex: v.hex,
            forced: v.is_forced(),
            had_errors: v.had_errors(),
            truncated: v.truncated,
            total_rows: v.total_rows() as u64,
            first_line: v.scroll as u64,
            lines: v.rows(alto).into_iter().map(clamp_display).collect(),
        })
    }

    /// El reparto de ESTE tamaño, con los papeles puestos.
    ///
    /// Sale del mismo `resolve` que usa el TUI: el renderer recibe rectángulos
    /// en celdas y no una lista de huecos que tenga que colocar él, que sería
    /// una segunda regla de disposición escrita en otro lenguaje (decisión
    /// D14).
    fn disposicion(&self) -> LayoutView {
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
            placements,
        }
    }

    /// Mueve el cursor del hueco, topando en los extremos.
    fn mover_cursor(
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
    fn poner_cursor(
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
    fn marcar(
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
    fn fila_de(&self, slot_id: u32, key: RowKey, generation: u64) -> Option<usize> {
        if slot_id != self.activo() || self.hueco().pane.listing_epoch() != generation {
            return None;
        }
        self.fila_valida(key)
    }

    /// Este frontend todavía no muta, y lo DICE.
    ///
    /// Una tecla muda es peor que un «aquí no»: el usuario que pulsa F8 y no
    /// ve nada no sabe si borró.
    fn no_muta() -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        (
            ActionAck::Unavailable {
                reason_key: "host-read-only".to_owned(),
            },
            Vec::new(),
        )
    }

    /// Mueve el foco al siguiente hueco enfocable, o al anterior.
    fn mover_foco(&mut self, atras: bool) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // El recorrido es el COMPARTIDO: `focus_order` ya se salta lo
        // que no se ve y lo que no se enfoca (una barra de estado no
        // recibe el foco), así que aquí no hay una segunda regla que
        // pueda divergir de la del TUI.
        let actual = SlotId(self.activo());
        let siguiente = if atras {
            norte_frontend::layout::focus_prev(&self.reparto, actual)
        } else {
            norte_frontend::layout::focus_next(&self.reparto, actual)
        };
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

    /// Designa OTRO hueco visible como destino de la siguiente operación.
    fn designar_destino(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
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
    fn ordenar_por(
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
            .find(|c| id_pintable(c) == *column)
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
    fn columnas_de(&self, dir: &VPath) -> Vec<norte_frontend::columns::ColumnId> {
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
    fn attrs_de(&self, dir: &VPath) -> Vec<String> {
        self.columnas.attr_ids_for(dir.scheme())
    }

    /// Las cabeceras del listado, con la etiqueta ya traducida y la marca de
    /// orden puesta.
    ///
    /// `header_label` y `sort_column_id` son las MISMAS funciones que usa el
    /// TUI: cómo se llama una columna y si ordena no puede depender de quién
    /// pinta.
    fn cabeceras(&self, hueco: &Hueco) -> Vec<ColumnHeader> {
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
                    // El id se acota y se enmascara como todo lo demás: sale
                    // de la configuración (una columna `plugin:` de una capa
                    // de proyecto es texto ajeno) y viaja en CADA fila. Era
                    // la excepción que nadie había clavado al tope del
                    // bridge.
                    id: clamp_display(id_pintable(id)),
                    label: clamp_display(header_label(id, &estilo, catalogo)),
                    sort,
                    sortable: ordena.is_some(),
                }
            })
            .collect()
    }

    /// La proyección de UN listado.
    fn browser(&self, id: u32, hueco: &Hueco) -> BrowserSlotView {
        let (path, hostil) = norte_frontend::path_display(hueco.pane.dir());
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
