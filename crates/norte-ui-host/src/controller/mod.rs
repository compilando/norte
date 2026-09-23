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
    ActionAck, BridgeEnvelope, InstanceId, MAX_DIALOGS, MAX_ROWS_PER_BATCH, MAX_TASKS,
    MAX_TASKS_RETAINED, MAX_TRANSFER_BATCH, ModalId, RequestToken, RowKey, StaleAction,
    clamp_display,
};
use crate::commands::{Efecto, efecto_de};
use crate::dto::{
    BrowserSlotView, ColumnHeader, ConnectionView, DialogChoice, DialogView, LayoutView,
    PendingView, RowKind, RowView, SlotPlacement, SlotRole, SlotState, SlotView, StatusView,
    TaskStateView, TaskView, UiNotice, UiUpdate, ViewChange, ViewPatch, ViewSnapshot,
};

// Los `impl Estado` repartidos por tema (ADR 0086). El actor, el buzón,
// el `Estado` y el reparto de acciones se quedan aquí; cada módulo hijo
// ve lo privado de este porque es su descendiente, así que el movimiento
// no ha necesitado abrir la visibilidad de nada.
mod agents;
mod ai;
mod approvals;
mod dialogs;
mod diskmap;
mod effects;
mod extensions;
mod fileops;
mod gestures;
mod goto;
mod help;
mod input;
mod layout;
mod lifecycle;
mod listing;
mod logpanel;
mod menu;
mod nav;
mod organize;
mod palette;
mod panel;
mod panelplugin;
mod patches;
mod places;
mod preview;
mod profiles;
mod search;
mod selectors;
mod session;
mod settings;
mod sums;
mod sync;
mod tabs;
mod tasks;
mod termpanel;
mod timeline;
mod transfer;
mod tree;
mod viewer;
mod views;
mod wizard;

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
    "dialog.ai-rename",
];

/// Cómo se llaman las extensiones y sus comandos, ya enmascarados:
/// `id → (nombre, comando → título)`.
type Rotulos = std::collections::HashMap<
    String,
    (
        crate::extensions::Texto,
        std::collections::HashMap<String, crate::extensions::Texto>,
    ),
>;

/// Lo que hace falta para pintar la salida de un comando: quién, qué, y qué
/// contestó.
struct SalidaPedida {
    /// El id de la extensión, reverse-DNS validado.
    id: String,
    /// Su nombre, ya enmascarado, con su bandera.
    plugin: crate::extensions::Texto,
    /// El título del comando, ya enmascarado, con su bandera.
    comando: crate::extensions::Texto,
    /// Lo que imprimió, o por qué no.
    res: Result<String, Error>,
}

/// Lo que esta ventana tiene del ESCRITORIO.
///
/// Juntos porque son la misma cosa vista dos veces: por dónde se le pide algo
/// al proceso que hospeda, y lo que ese proceso contestó y sigue en pantalla.
#[derive(Debug, Default)]
struct Escritorio {
    /// Por dónde salen los efectos NATIVOS, cuando hay alguien escuchando.
    ///
    /// `Option` porque el estado se construye antes que el canal —la primera
    /// foto sale de él— y porque un host sin nadie suscrito tiene que poder
    /// seguir: un efecto que nadie recoge es un gesto que no pasa nada, no un
    /// error.
    nativos: Option<broadcast::Sender<crate::dto::NativeEffect>>,
    /// La salida del último comando de extensión, si sigue en pantalla.
    ///
    /// Aquí y no en el gestor de extensiones: un comando se lanza desde la
    /// PALETA, que no necesita tener el gestor abierto —ni lo abre—, y una
    /// salida guardada dentro de una pantalla cerrada no la ve nadie.
    salida: Option<crate::dto::ExtensionOutputView>,
    /// La salida del último PROGRAMA que se corrió esperándolo (#312), si
    /// sigue en pantalla.
    programa: Option<crate::dto::ProgramOutputView>,
}

/// Lo que esta ventana sabe de las sesiones de AGENTE.
///
/// Juntas y no sueltas en el estado: las tres describen lo mismo —quién ha
/// pedido permiso, si se está mirando, y qué se está deshaciendo— y separarlas
/// era tener que acordarse de las tres cada vez que una cambia.
#[derive(Debug, Default)]
struct Agencia {
    /// Lo visto. SIEMPRE presente: una petición llega cuando llega, y el
    /// panel solo decide si se pinta.
    sesiones: crate::agents::Agentes,
    /// El panel está abierto.
    panel: bool,
    /// Qué sesión deshace cada task de undo en marcha, por id de task.
    ///
    /// El desenlace llega por el progreso, que solo trae el id: sin este mapa
    /// no hay forma de saber a qué sesión soltarle el «deshaciendo».
    undos: std::collections::HashMap<u64, String>,
}

/// Qué se cambia de una extensión.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cambio {
    /// Conceder o retirar sus capabilities.
    Aprobacion,
    /// Encenderla o apagarla.
    Encendido,
    /// Desinstalarla (ADR 0104).
    Desinstalacion,
}

impl From<crate::action::ExtensionChange> for Cambio {
    fn from(c: crate::action::ExtensionChange) -> Self {
        use crate::action::ExtensionChange as E;
        match c {
            E::Approval => Self::Aprobacion,
            E::Enabled => Self::Encendido,
            E::Uninstall => Self::Desinstalacion,
        }
    }
}

/// El cambio ya resuelto a un valor concreto.
///
/// Aparte de [`Cambio`] a propósito: `a` sobre una fila significa cosas
/// distintas según cómo esté, y la que viaja al daemon es la resuelta.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Gobierno {
    /// `plugin.set_approval`, con el ancla que se ENSEÑÓ (#282). `None` al
    /// revocar: quitar un permiso no concede nada, y rehusarlo por un digest
    /// rancio dejaría vivo justo lo que alguien intenta quitar.
    Aprobar(bool, Option<String>),
    /// `plugin.set_enabled`.
    Encender(bool),
    /// `plugin.uninstall` (ADR 0104). Ya confirmado por un humano.
    Desinstalar,
}

/// Tope de capabilities que una pregunta de concesión puede enseñar.
///
/// No es un recorte: por encima de esto NO se pregunta. Un manifiesto que
/// declara más capabilities de las que caben en una pantalla no produce una
/// decisión informada, y conceder lo que no se leyó es lo que esta pregunta
/// existe para evitar.
const MAX_CAPABILIDADES: usize = 32;

/// Tope de caracteres de la salida de un comando de extensión.
///
/// Lo que imprime un plugin no tiene tope por su lado: un comando puede
/// devolver un megabyte y dejarlo cruzar es regalarle la ventana. Se aplica
/// ANTES de enmascarar: si no, una salida de 100 MB se enmascara entera —y se
/// materializa entera en la task del escritor— para que sobrevivan cuatro mil
/// caracteres.
const MAX_SALIDA: usize = 4_000;

/// Tope de LÍNEAS de esa salida.
///
/// Las líneas cruzan sueltas para que un salto de línea no marque como
/// hostil una salida honesta, y una lista también necesita su tope.
const MAX_SALIDA_LINEAS: usize = 200;

/// Plazo de EJECUTAR un comando de extensión.
///
/// Aparte del de leer el catálogo, y mucho más largo: al otro lado corre
/// código de tercero que puede estar indexando o hablando por red, y cortarlo
/// a los cinco segundos no lo para —sigue corriendo en el daemon, con sus
/// efectos— sino que solo deja a esta ventana sin saber cómo acabó.
const PLAZO_COMANDO: std::time::Duration = std::time::Duration::from_mins(1);

/// Plazo de una llamada de extensiones (catálogo o página).
///
/// La ayuda se pinta sin esperarla, así que este plazo no gobierna una
/// pantalla: gobierna una tarea que si no volviera jamás dejaría un `id`
/// reclamado y una página en blanco para siempre.
const PLAZO_PLUGINS: std::time::Duration = std::time::Duration::from_secs(5);

/// Cuánto sigue en el tablero una task ya TERMINADA.
///
/// Sin esto una terminal se quedaba hasta que otra la empujaba fuera por el
/// tope de filas, así que lo que el panel enseñaba de un vistazo era el
/// historial de la sesión y no lo que está pasando. Diez segundos bastan para
/// leer el `✓` o el error, y por debajo el tablero vuelve a hablar del
/// presente. El mismo número que el TUI, a propósito: dos frontends que
/// caducan distinto son dos respuestas a «¿sigue esto en marcha?».
const TTL_TASK_TERMINAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Cada cuánto se mira si la sesión cambió y, si cambió, se escribe.
///
/// El mismo segundo que el terminal (`session_tick` en su bucle), a
/// propósito: dos frontends que guardan a ritmos distintos son dos respuestas
/// a «¿dónde me quedé?» tras un cierre que no llegó a tiempo. Y un segundo es
/// barato: comparar el cuerpo con lo último mandado es lo único que hace un
/// tic sin cambios.
const SESION_TIC: std::time::Duration = std::time::Duration::from_secs(1);

/// Plazo de una petición de plan a un modelo.
///
/// Generoso: pensar es lo que hace. Es el tope de ARRIBA, para que una
/// llamada que no vuelva jamás no deje la petición en vuelo para siempre —
/// con `Escape` como única salida y sin nada en pantalla que diga que sigue
/// viva.
const PLAZO_IA: std::time::Duration = std::time::Duration::from_mins(2);

/// Tope de un nombre tecleado, en bytes. Ni `NAME_MAX` (que es del sistema
/// de ficheros y no lo sabemos aquí) ni el de pantalla: un tope generoso que
/// impide que un renderer mande un megabyte, y que RECHAZA en vez de
/// recortar — recortar un nombre es inventarse otro.
const MAX_NOMBRE: usize = 4096;

/// Lo que el visor lee de un fichero: una cabecera de 256 KiB. El resto NO
/// se lee — el mismo presupuesto que el TUI, y por el mismo motivo (ADR
/// 0005): un visor no es una excusa para traerse un fichero de un giga.
const VISOR_CAP: u64 = 256 * 1024;

/// Lo más que se lee de una IMAGEN para previsualizarla: 8 MiB.
///
/// Aparte del tope del visor de texto, que es una CABECERA a propósito —una
/// imagen no se puede enseñar a medias—. Una foto de móvil cabe de sobra;
/// un TIFF de escáner no, y entonces no se pinta y se dice (ADR 0069).
const IMAGEN_CAP: u64 = 8 * 1024 * 1024;

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
    /// [`Self::initial_dir`] lo ESCRIBIÓ un humano en la línea de órdenes.
    ///
    /// Con `false` es el directorio actual del proceso, o sea un valor por
    /// defecto que la sesión tiene todo el derecho a pisar. Con `true` es una
    /// intención, y gana: `norte-gui /usr/bin` con una sesión guardada abría
    /// donde estuvieras ayer y se comía el argumento sin decir nada.
    ///
    /// Solo el panel ACTIVO. El otro se queda donde la sesión lo dejó: media
    /// pantalla de memoria que nadie pidió tirar. Es la misma regla que
    /// `App::pin_start_dir` en el terminal.
    pub initial_dir_pedido: bool,
    /// Esta ventana es el otro extremo de un RELEVO (`--attach`, fase 9), así
    /// que además de la pantalla reclama lo MARCADO que el otro frontend dejó.
    ///
    /// La misma naturaleza que [`Self::initial_dir_pedido`] —cómo se lanzó el
    /// proceso—, y por eso vive a su lado. Sin él, un arranque es un arranque:
    /// unas marcas de un relevo que se quedó a medias no resucitan al día
    /// siguiente.
    pub attach: bool,
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
    /// El keymap efectivo de un DIÁLOGO, con las mismas capas.
    ///
    /// Tercera pantalla, por el mismo motivo que la del visor: con una
    /// pregunta delante las teclas son suyas. Sin esto, esta ventana atendía
    /// sus diálogos con teclas fijas y un preset que reataba
    /// `dialog.confirm` cambiaba el TUI y no la ventana (#287).
    pub keymap_dialog: Effective,
    /// La disposición: el árbol de huecos. De un preset de fábrica
    /// (`norte_frontend::layout::presets::tree`) o de la configuración del
    /// usuario; el host no lee ficheros.
    pub layout: Node,
    /// El tamaño INICIAL de la ventana, en celdas de layout. El renderer lo
    /// corrige en cuanto sepa el suyo ([`UiAction::SetViewport`]).
    pub viewport: (u16, u16),
    /// Hasta dónde llega este frontend: solo mirar, o también escribir.
    ///
    /// No es una amputación del host —sabe mutar y sus tests lo prueban—
    /// sino una decisión de quien lo monta. La ventana gráfica arrancó en
    /// [`crate::commands::Efectos::SoloLectura`] hasta que la revisión de
    /// seguridad de la tarea 5.4 levantó su interruptor; hoy los tres
    /// montajes (ventana, TUI y tests) usan
    /// [`crate::commands::Efectos::Completo`], y `SoloLectura` sigue siendo
    /// la posición que puede elegir un montaje que no quiera autoridad
    /// destructiva ni de policy.
    pub effects: crate::commands::Efectos,
    /// La configuración YA cargada, para los ajustes en solo lectura.
    ///
    /// La lee quien arranca el host, UNA vez, como todo lo demás: un
    /// frontend que la relee por su cuenta acaba enseñando unos ajustes que
    /// no son los que está usando.
    pub settings: norte_frontend::config::FrontendConfig,
    /// Dónde vive cada cosa, ya resuelto. Ver [`crate::settings::HostPaths`].
    pub paths: crate::settings::HostPaths,
    /// El tema activo, ya resuelto a pares rol → color por quien arranca.
    pub theme: crate::pickers::HostTheme,
    /// Las disposiciones que el usuario tiene en `layouts/*.toml`, YA leídas.
    ///
    /// Leídas y no por nombre: el selector PINTA la forma de cada una, y
    /// leerlas al mover el cursor sería I/O en el bucle de eventos. Quien
    /// tiene el disco delante es quien arranca la ventana, no el host.
    pub user_layouts: Vec<norte_frontend::layout_picker::UserLayout>,
    /// La configuración de columnas ENTERA, no una lista ya resuelta.
    ///
    /// Las columnas se configuran POR ESQUEMA (`[ui.columns.schemes.sftp]`),
    /// y resolverlas una vez al arrancar dejaba muerta esa mitad de la
    /// configuración: una columna `attr:` que solo existe en `sftp` no se
    /// pedía nunca y no se pintaba nunca, porque los atributos que viajan en
    /// cada listado se habían congelado con los del esquema de arranque.
    pub columns: norte_frontend::columns::ColumnsSettings,
    /// El perfil con el que se ARRANCÓ, si lo hubo (`--profile`, #307).
    ///
    /// Ya aplicado: sus capas entraron en la configuración que llega en
    /// [`Self::settings`], porque un perfil nombrado en la línea de órdenes se
    /// conoce antes de conectar con nada y así alcanza hasta `[ui] lang`. Lo
    /// que el host necesita es SABERLO, para marcarlo activo en el selector y
    /// para que la sesión lo recuerde; sin esto, arrancar con `--profile` daba
    /// una ventana correcta cuyo selector decía que no había ninguno puesto.
    ///
    /// `OsString` porque es un nombre de directorio (#245).
    pub profile: Option<std::ffi::OsString>,
    /// El anillo de registro del que lee el panel `log` (#326).
    ///
    /// Lo monta el PROCESO al arrancar —el subscriber de `tracing` se instala
    /// una vez— y llega aquí porque el host no monta subscribers: los lee.
    /// `None` = este binario no lo montó, y entonces el panel lo dice en vez
    /// de enseñarse vacío, que sería indistinguible de «no ha pasado nada».
    ///
    /// Va por opción y no por un `set_*` posterior porque el panel puede
    /// existir en la disposición de arranque: montarlo después dejaría la
    /// primera foto con un registro que no es el que hay.
    pub log_ring: Option<norte_config::logring::LogRing>,
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

/// El tic de la sesión, cada segundo, como el del terminal: la pantalla se
/// guarda mientras se usa, no solo al cerrar. Un cierre que no llega —el
/// proceso muerto, el socket atascado más allá del plazo— perdía hasta aquí
/// todo lo andado desde el arranque. Los cambios del ÁRBOL además se escriben
/// al momento, sin esperar al tic (`aplicar_disposicion_con`,
/// `aplicar_arbol`). Muere con el buzón, como las demás bombas.
fn bombear_tic_de_sesion(buzon: mpsc::Sender<Mensaje>) {
    tokio::spawn(async move {
        let mut tic = tokio::time::interval(SESION_TIC);
        // El primero de `interval` es inmediato, y no hay nada que escribir
        // un instante después de arrancar.
        tic.tick().await;
        loop {
            tic.tick().await;
            if buzon.send(Mensaje::SesionTic).await.is_err() {
                return;
            }
        }
    });
}

/// Reenvía los `plugin.notice` del backend al buzón del host (ADR 0100). Una
/// función aparte de `start` porque la lista de bombas ya llenaba el límite
/// de líneas, y la forma es la de las demás: una task que muere con el canal
/// que la alimenta.
fn bombear_avisos_de_plugin(backend: &dyn HostBackend, buzon: mpsc::Sender<Mensaje>) {
    let Some(mut avisos) = backend.take_plugin_notices() else {
        return;
    };
    tokio::spawn(async move {
        while let Some(n) = avisos.recv().await {
            if buzon.send(Mensaje::AvisoPlugin(Box::new(n))).await.is_err() {
                return;
            }
        }
    });
}

/// Reenvía al buzón del host TODO lo que el backend empuja por su cuenta:
/// conexión, sesión en claro, fallos de entrada, avisos de plugin,
/// aprobaciones y tareas ajenas.
///
/// Juntas porque son la misma decisión seis veces —una task que muere con el
/// canal que la alimenta— y porque entran por el MISMO buzón: un aviso de
/// conexión perdida tiene que ordenarse con lo que estaba pasando cuando se
/// perdió. Fuera de `start` por el límite de líneas.
fn bombear_canales_del_backend(
    backend: &dyn HostBackend,
    buzon: &mpsc::Sender<Mensaje>,
    efectos: crate::commands::Efectos,
) {
    // Los dos canales de la conexión son del PRIMER dueño, así que se toman
    // una vez, aquí.
    if let Some(mut eventos) = backend.take_conn_events() {
        let buzon = buzon.clone();
        tokio::spawn(async move {
            while let Some(ev) = eventos.recv().await {
                if buzon.send(Mensaje::Conexion(ev)).await.is_err() {
                    return;
                }
            }
        });
    }
    // Los avisos de sesión en claro (#44) se toman SIEMPRE: no dependen de si
    // esta ventana puede escribir. Que un listado que se está LEYENDO viaje
    // sin cifrar es un hecho para quien lo mira, no un permiso.
    if let Some(mut degradadas) = backend.take_degraded() {
        let buzon = buzon.clone();
        tokio::spawn(async move {
            while let Some(d) = degradadas.recv().await {
                if buzon.send(Mensaje::Degradada(Box::new(d))).await.is_err() {
                    return;
                }
            }
        });
    }
    // Y los fallos (#322), con el mismo criterio: por qué NO se pudo entrar en
    // una máquina se le dice a quien lo intentó, pueda esta ventana escribir o
    // no.
    if let Some(mut fallidas) = backend.take_failed() {
        let buzon = buzon.clone();
        tokio::spawn(async move {
            while let Some(f) = fallidas.recv().await {
                if buzon.send(Mensaje::Fallida(Box::new(f))).await.is_err() {
                    return;
                }
            }
        });
    }
    // Los avisos de los hooks (ADR 0100) hablan de ficheros que ya cambiaron,
    // así que se leen pueda escribir o no.
    bombear_avisos_de_plugin(backend, buzon.clone());
    // Las aprobaciones de policy son una MUTACIÓN por delegación: decir que sí
    // a la operación de un agente. Un frontend que todavía no puede escribir
    // tampoco puede autorizar que escriba otro, así que en solo lectura el
    // canal ni se toma (y el diálogo no existe, que es más honesto que uno que
    // no responde).
    if efectos == crate::commands::Efectos::Completo
        && let Some(mut aprobaciones) = backend.take_approvals()
    {
        let buzon = buzon.clone();
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
        let buzon = buzon.clone();
        tokio::spawn(async move {
            while let Some(task) = ajenas.recv().await {
                if buzon
                    .send(Mensaje::TaskNueva(Box::new((task, Vec::new(), None))))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
    }
}

/// El host: un asa barata de clonar sobre el único escritor.
#[derive(Clone)]
pub struct UiHost {
    inbox: mpsc::Sender<Mensaje>,
    updates: broadcast::Sender<BridgeEnvelope<UiUpdate>>,
    nativos: broadcast::Sender<crate::dto::NativeEffect>,
    instance: InstanceId,
}

/// Lo que vuelve de un listado: el testigo que lo pidió, el hueco al que va,
/// el directorio y el resultado.
/// Lo que el visor pidió: testigo, ruta, la cabecera leída y la preview de
/// plugin si alguna aplicó.
type Contenido = (
    RequestToken,
    VPath,
    Result<Vec<u8>, Error>,
    Option<norte_proto::methods::PluginPreviewStyled>,
);

/// Lo mismo, para el visor ACOPLADO (#291): el hueco que lo pidió va
/// delante, y no se resuelve al llegar — el hueco puede haberse cerrado, y
/// entonces la respuesta se tira.
type PreviewContenido = (u32, Contenido);

/// Lo que vuelve de un listado: su testigo, el hueco, el directorio, y las
/// entradas de la primera página con CUÁNTAS se saltó el provider.
type RespuestaListado = (
    RequestToken,
    u32,
    VPath,
    Result<(Vec<Entry>, Option<u64>), Error>,
);

/// Lo que vuelve de una tanda de sondeo: el directorio que se sondeaba, el
/// hueco, y las parejas `(lo que se pidió, lo que contestó el provider)`.
type Sondas = (VPath, u32, Vec<(VPath, Entry)>);

/// Lo que vuelve de medir un mapa de disco (fase 4).
///
/// El hueco que lo pidió, el TESTIGO de esa petición —uno que no sea el vivo es
/// de un directorio que ya se dejó atrás— y el resultado, con el ESTADO de la
/// Task pegado al informe: uno de una Task cancelada está a medias, y pintarlo
/// como completo convierte un directorio enorme en uno pequeño.
type MedidaDeMapa = (
    u32,
    RequestToken,
    Result<
        (
            norte_proto::TaskState,
            norte_proto::methods::FsDirUsageReportResult,
        ),
        Error,
    >,
);

enum Mensaje {
    Accion(Box<UiAction>, oneshot::Sender<ActionAck>),
    /// Los BYTES de la imagen que el visor tiene abierta, si los hay.
    ///
    /// Una consulta y no una acción: no cambia nada y no produce parche. Va
    /// por el buzón igual porque el estado es del actor, y contestarla desde
    /// fuera sería leer lo que otro está escribiendo.
    BytesDeImagen(oneshot::Sender<Option<std::sync::Arc<Vec<u8>>>>),
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
    /// A esta aprobación se le acabó el TTL: el daemon ya no la acepta.
    AprobacionCaducada(u64),
    /// El tema elegido ya está (o no) en el `norte.toml`. `Some(clave)` es el
    /// motivo por el que no se pudo guardar; `None` es que se guardó.
    ///
    /// Solo el fallo se DICE. Un «guardado» por cada tema elegido sería un
    /// mensaje por cada Enter en una pantalla cuyo resultado ya se ve: los
    /// colores cambiaron.
    TemaPersistido(Option<&'static str>),
    /// El ancho de una columna ya está (o no) en el `norte.toml` (puente
    /// 64). Mismo trato que el tema: solo el fallo se dice.
    AnchoPersistido(Option<&'static str>),
    /// Un `[ui] theme` que era una RUTA, ya leído fuera del actor.
    ///
    /// Lleva el spec para poder nombrarlo en el efecto nativo —quien hospeda
    /// vuelve a resolverlo por su cuenta, porque los colores cruzan a la
    /// webview convertidos en variables CSS y esa conversión no es del host—
    /// y, en caja, porque un `Theme` es grande al lado del resto del enum.
    ///
    /// El error es una CLAVE, no el error: el suyo lleva la ruta dentro (#73).
    TemaResuelto(Box<(String, Result<norte_theme::Theme, &'static str>)>),
    /// El tic de la sesión: cada segundo, como el terminal. Si la pantalla
    /// cambió desde lo último escrito, se escribe; si no, nada.
    SesionTic,
    /// Un `session.put` contestó: qué dijo el daemon y el cuerpo que se
    /// mandó, para darlo por escrito solo si de verdad entró.
    SesionPuesta(
        Box<(
            Result<u64, Error>,
            std::sync::Arc<norte_frontend::session::SessionBody>,
        )>,
    ),
    /// La sesión releída tras un conflicto: otra ventana escribió en medio y
    /// la revisión sobre la que se escribe ya no vale.
    SesionReleida(Result<(norte_proto::methods::Session, bool), Error>),
    /// El RELEVO a la terminal terminó (fase 9): la pantalla está escrita y
    /// la sesión, soltada — o no se pudo, y entonces no pasa nada y se dice.
    Relevado {
        /// Esta ventana era la dueña y ha dejado de serlo.
        soltada: bool,
    },
    /// A esta task TERMINADA se le acabó su rato en el tablero
    /// ([`TTL_TASK_TERMINAL`]). Lleva la ÉPOCA de conexión en la que se
    /// registró: tras un relevo del daemon los ids vuelven a empezar en 1, y
    /// caducar por número desalojaría a una task viva que solo comparte el
    /// número con la que se fue.
    TaskCaducada(u64, u64),
    /// Algo que se pidió fuera del actor terminó y hay que DECIRLO: la clave
    /// del mensaje (hoy, una pausa que el daemon no sabe hacer).
    Decir(&'static str),
    /// La barra de progreso ligera cambia sin que llegue progreso: pasó su
    /// umbral, el del panel, o se acabó el rato del «✓» (ADR 0146).
    Tira,
    /// El `policy.decide` que APROBABA no llegó al daemon.
    /// Un `policy.decide` que no salió bien: qué aprobación y con qué clave
    /// se cuenta (#279).
    AprobacionNoEntregada(u64, &'static str),
    /// Más entradas del listado que se está drenando por detrás.
    ///
    /// El `bool` dice si es el ÚLTIMO lote. Sin él, `drenando` se levantaba
    /// al pedir el listado y no lo bajaba nadie nunca —ni siquiera cuando el
    /// stream se agotaba dentro de la primera página—, así que el campo no
    /// significaba «sigue llegando» sino «alguna vez se pidió esto», y
    /// cualquiera que lo consultara para decidir se equivocaba.
    MasEntradas(Box<(RequestToken, u32, Vec<Entry>, bool)>),
    /// Lo que contestó una petición que se lanzó para un OVERLAY.
    ///
    /// Las cinco viajan juntas porque son la misma historia: una superficie
    /// se abrió SIN esperar —la documentación y la tabla de montaje son
    /// cosméticas, y una ventana en blanco hasta que el daemon conteste es
    /// peor que una lista que gana filas medio segundo después—, y esto es
    /// la respuesta llegando tarde. Cada una comprueba que su superficie
    /// siga abierta antes de tocar nada.
    Fondo(Box<Fondo>),
    /// El contenido que el visor pidió.
    /// Lo que el visor pidió: la cabecera del fichero y, si algún plugin
    /// `previewer` aplicó, su preview con estilo.
    ///
    /// Las dos en el MISMO mensaje porque son una sola respuesta a una sola
    /// tecla: mandarlas por separado abriría el visor crudo y lo cambiaría
    /// por la preview un instante después, que es un parpadeo que nadie pidió.
    Contenido(Box<Contenido>),
    /// Lo que un hueco de preview pidió (#291): igual que [`Self::Contenido`]
    /// pero para el visor acoplado, y con el hueco delante.
    PreviewContenido(Box<PreviewContenido>),
    /// El marco que pintó un panel de plugin (fase 3), con el testigo de la
    /// petición que lo pidió: uno que no sea el vivo es de un cursor que ya se
    /// movió.
    PanelContenido(
        Box<(
            u32,
            RequestToken,
            Result<Option<norte_proto::methods::PanelFrame>, Error>,
        )>,
    ),
    /// Lo que midió un mapa de disco (fase 4), con el testigo de la petición
    /// que lo pidió: uno que no sea el vivo es de un directorio que ya se dejó
    /// atrás. El ESTADO viaja con el informe porque uno de una Task cancelada
    /// está a medias, y pintarlo como completo convierte un directorio enorme
    /// en uno pequeño.
    MapaContenido(Box<MedidaDeMapa>),
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
    /// Una Task recién encolada, con su progreso, su cancelación y los
    /// directorios que dejará distintos.
    TaskNueva(Box<(crate::backend::HostTask, Vec<VPath>, Option<Reintento>)>),
    /// Qué acepta la ubicación de un hueco: cómo pliega nombres (#268) y si
    /// rehúsa escribir.
    Capacidades(u32, VPath, norte_proto::Capabilities),
    /// Encolarla falló. El usuario tiene que enterarse: pidió un borrado.
    TaskFallida(Box<Error>),
    /// Un rechazo al encolar UNA entrada de un lote (#271). Separado de
    /// [`Self::TaskFallida`] a propósito: aquél lo manda todo el que encola
    /// algo —una búsqueda, un plan, un undo— y su sitio es la barra; éste solo
    /// lo manda el bucle de un lote, y su sitio es la CUENTA del lote.
    TaskDeLoteRechazada(Box<Error>),
    /// La conexión con el daemon cambió de estado.
    Conexion(norte_client::ConnEvent),
    /// Una sesión de un provider viaja SIN cifrar (#44).
    Degradada(Box<norte_proto::methods::ConnectionDegraded>),
    /// Una conexión NO se pudo abrir, y por qué (#322).
    Fallida(Box<norte_proto::methods::ConnectionFailed>),
    /// Un plugin `hook` dijo algo sobre una mutación ya registrada, o el
    /// daemon apagó sus hooks (0.69.0, ADR 0100).
    AvisoPlugin(Box<norte_proto::methods::PluginNotice>),
    /// El secreto se entregó (o no), y con ello qué hacer con la navegación
    /// que `SecretNeeded` había suspendido (#327).
    SecretoEntregado(Box<(u32, VPath, Result<(), Error>)>),
    /// Toca mirar si el registro tiene algo nuevo (#326). Lleva la ÉPOCA de la
    /// apertura que lo programó: uno de una apertura anterior se deja morir en
    /// vez de rearmarse para siempre.
    RegistroTic(u64),
    /// Lo que el daemon contestó a `log.tail` (#328), con la ÉPOCA de la
    /// apertura que lo pidió.
    ///
    /// La época no es adorno: entre pedir y contestar caben un cierre y una
    /// apertura, y unas líneas de la sesión anterior aterrizando en el panel
    /// nuevo serían historia que nadie pidió por delante de la que sí.
    RegistroRemoto(u64, Box<Result<norte_proto::methods::LogTailResult, Error>>),
    /// Lo que el daemon contestó a `log.level` (#328): el nivel que de verdad
    /// quedó puesto, que puede no ser el que se pidió.
    RegistroNivel(u64, Box<Result<String, Error>>),
    /// Un snapshot de progreso. Por la MISMA cola que todo lo demás, que es
    /// lo que garantiza que un estado terminal no se adelante ni se pierda.
    Progreso(Box<norte_proto::TaskProgress>),
    /// El informe de una Task que ya terminó y que TIENE informe.
    ///
    /// Lleva el `Result` entero y no un `Option`: «fue bien» y «el daemon no
    /// sabe informar» son dos cosas distintas, y colapsarlas es justo lo que
    /// estos informes existen para no hacer.
    Informe(Box<(u64, u64, tasks::Informe)>),
    /// El `fs.stat` que se hace entre crear un fichero y abrirlo (#303): la
    /// ruta que se creó, y si lo que hay ahí sigue siendo un fichero regular.
    ///
    /// Sin época: lo que se decide con esto es abrir una ruta ABSOLUTA en el
    /// escritorio de esta máquina, que no significa una cosa distinta según
    /// qué daemon conteste — al revés que los informes, cuyos ids de task
    /// vuelven a empezar en 1 tras un relevo.
    CreadoComprobado(Box<(norte_proto::VPath, Veredicto)>),
    /// Un favorito se guardó (#309): su nombre, a dónde apunta y, si falló, la
    /// clave del motivo. La copia en memoria no se toca hasta que el disco
    /// contesta.
    ///
    /// El destino viaja en el mensaje y no se relee al llegar: entre pedir el
    /// nombre y guardar, el panel puede haber navegado, y reflejar «donde
    /// estoy ahora» pondría en la lista un favorito distinto del que se acaba
    /// de escribir en el fichero.
    FavoritoPersistido(Box<(String, norte_proto::VPath, Option<&'static str>)>),
    /// El perfil se escribió (o no): nombre y la clave del fallo (#318).
    PerfilGuardado(Box<(String, Option<&'static str>)>),
    /// Un favorito se quitó, con la misma forma.
    FavoritoQuitado(Box<(String, Option<&'static str>)>),
    Apagar(oneshot::Sender<ShutdownReport>),
}

/// Qué contestó el `fs.stat` que se hace entre crear un fichero y abrirlo
/// (#303).
///
/// Tres valores y no un `bool` porque «no es el fichero» y «no se pudo
/// preguntar» son cosas distintas y se dicen distinto: llamar manipulación a un
/// daemon relevado es una acusación falsa, y enseñar al lector a ignorar ese
/// mensaje es lo que lo inutiliza el día que sea verdad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Veredicto {
    /// Sigue siendo un fichero regular: adelante.
    EsElFichero,
    /// Un enlace, una carpeta o nada. Las tres se dicen igual: nombrar cuál
    /// sería confirmarle el enlace a quien lo puso.
    YaNoEsElFichero,
    /// El `stat` falló. No se abre nada, y se dice que no se pudo comprobar.
    NoSeSabe,
}

impl From<bool> for Veredicto {
    fn from(regular: bool) -> Self {
        if regular {
            Self::EsElFichero
        } else {
            Self::YaNoEsElFichero
        }
    }
}

/// La respuesta de una petición de fondo, por superficie.
///
/// Un enum aparte y no cinco variantes de [`Mensaje`]: el actor es un
/// reparto, y cinco brazos que hacen lo mismo —comprobar que su superficie
/// siga abierta y devolver parches— son un brazo con cinco casos.
/// La respuesta de una tanda de decoración: de qué GENERACIÓN de adornos
/// (una tanda pedida antes de que el gestor apagara un plugin no describe
/// lo que hay), qué hueco, qué directorio, las insignias por ruta y las
/// celdas de cada columna `plugin:`.
type Adornos = (
    u64,
    u32,
    VPath,
    std::collections::HashMap<VPath, norte_frontend::Decoration>,
    std::collections::HashMap<String, std::collections::HashMap<VPath, String>>,
    // El rótulo que el manifiesto le puso a cada columna de plugin, ya
    // saneado: la cabecera lo prefiere a su id.
    std::collections::BTreeMap<String, String>,
);

enum Fondo {
    /// El catálogo de plugins que pidió la AYUDA, para su lateral.
    PluginsDeAyuda(Result<norte_proto::methods::PluginListResult, Error>),
    /// El catálogo pedido al ARRANCAR, para declarar qué PANELES aportan los
    /// plugins (fase 3).
    ///
    /// Aparte del de la ayuda y del gestor, y no por gusto: esos dos salen
    /// pronto si su superficie está cerrada, y un panel de plugin tiene que
    /// poder pintarse sin que nadie haya abierto ni la ayuda ni el gestor.
    /// Lo que trae es la DECLARACIÓN de qué huecos existen, no el contenido
    /// de ninguno.
    PanelesDePlugin(Result<norte_proto::methods::PluginListResult, Error>),
    /// La página de un plugin, pedida al abrirla en la ayuda.
    PaginaDePlugin(
        String,
        Result<norte_proto::methods::PluginHelpResult, Error>,
    ),
    /// El catálogo que pidió el GESTOR de extensiones, con la APERTURA que
    /// lo pidió.
    ///
    /// Aparte del de la ayuda: son dos superficies con dos vidas, y
    /// compartir la respuesta obligaría a cada una a comprobar si la otra
    /// sigue abierta.
    ///
    /// La apertura hace falta porque «sigue abierta» no es «es la misma».
    /// Abrir (petición A, lenta), `esc`, reabrir: A vencía por plazo y su
    /// `unwrap_or(vacío)` apagaba el «cargando» de B y decía «ninguna
    /// instalada» hasta que llegara B.
    Catalogo(
        u64,
        u64,
        Result<norte_proto::methods::PluginListResult, Error>,
    ),
    /// Lo que hay que saber del DESTINO de una transferencia antes de que el
    /// humano diga que sí: si cabe (#149) y si sabe sujetar sus escrituras
    /// (#164). Lleva el modal al que pertenece, porque llega tarde.
    ///
    /// Las dos van juntas porque son la misma pregunta hecha al mismo sitio
    /// en el mismo momento, y separarlas costaría dos rondas de I/O por
    /// diálogo para pintar dos líneas contiguas — el mismo reparto que hace
    /// el terminal en `DestCheck`.
    AvisosDeDestino(ModalId, Vec<String>),
    /// La task de un `policy.undo_session` ya tiene id: se ata a su sesión.
    UndoDeSesion(u64, String),
    /// Los perfiles que hay en `profiles/`, ya leídos, y qué se hace con
    /// ellos: `None` = abrir el selector, `Some(hacia_delante)` = saltar al
    /// vecino sin abrir nada.
    ///
    /// Leerlos es disco —un directorio y un `norte.toml` por perfil— así que
    /// va por aquí como todo lo que no puede correr en el actor.
    Perfiles(
        Vec<norte_frontend::profile_picker::UserProfile>,
        Option<bool>,
    ),
    /// La configuración de un perfil, ya cargada, con su nombre.
    ///
    /// `Err` es la clave del motivo: un perfil que no carga NO cambia nada —
    /// se sigue en el que estabas, que es lo que la ADR 0079 D7 pide para un
    /// cambio.
    PerfilCargado(
        std::ffi::OsString,
        Box<Result<norte_frontend::config::FrontendConfig, &'static str>>,
    ),
    /// Un ajuste de F11 ya está (o no) en el `norte.toml`, y la configuración
    /// releída con él. En caja porque una `FrontendConfig` es grande al lado
    /// del resto del enum.
    AjusteEscrito(Box<settings::AjusteEscrito>),
    /// Una clave de F11 ya no está en el `norte.toml` — restablecida.
    AjusteRestablecido(Box<settings::AjusteRestablecido>),
    /// El catálogo que pidió la PALETA, para sus filas de plugin.
    PluginsDePaleta(u64, Result<norte_proto::methods::PluginListResult, Error>),
    /// Un cambio de gobierno (aprobar/revocar, encender/apagar) contestó.
    ///
    /// Lleva la APERTURA por lo mismo que el catálogo: la respuesta puede
    /// llegar sobre un gestor que ya se cerró y se volvió a abrir.
    Gobernada(u64, Result<(), Error>),
    /// Una escritura de `[config.<key>]` contestó: la apertura del gestor
    /// que la pidió, a qué extensión, y qué dijo el daemon.
    ///
    /// La apertura hace falta por lo mismo que en el catálogo: cerrar el
    /// gestor y reabrirlo mientras una escritura vuela dejaba que el fallo de
    /// la primera cerrara la ficha de la segunda.
    ConfigEscrita(u64, String, Result<(), Error>),
    /// La salida de un comando de extensión: la apertura que lo pidió, el id
    /// de la extensión, sus dos rótulos CON su bandera, y lo que contestó.
    ///
    /// Los rótulos viajan con su bandera y no solo enmascarados porque de una
    /// máscara no se vuelve: una bandera calculada después, sobre el texto ya
    /// enmascarado, sale siempre `false` y el panel afirma ser fiel.
    SalidaDeComando(u64, Box<SalidaPedida>),
    /// El esquema `[config]` de una extensión, pedido al abrir su ficha.
    FichaDePlugin(
        String,
        Result<norte_proto::methods::PluginGetConfigResult, Error>,
    ),
    /// Los volúmenes del host, con la APERTURA del selector que los pidió.
    /// Ver [`Fondo::Catalogo`].
    Volumenes(u64, Result<Vec<norte_proto::methods::Volume>, Error>),
    /// Una página de la línea de tiempo (#359): el hueco, el testigo de la
    /// petición y desde dónde se pidió (`None` = la primera).
    PaginaDeLinea(
        u32,
        RequestToken,
        Option<i64>,
        Result<norte_proto::methods::JournalListResult, Error>,
    ),
    /// Las conexiones para «ir a» (#357), con la APERTURA que las pidió: una
    /// respuesta de una apertura anterior no rellena la de ahora.
    ConexionesDeIrA(
        u64,
        Result<Vec<norte_proto::methods::ConnectionEntry>, Error>,
    ),
    /// Lo que contestó el índice a una consulta de «ir a» (#357): la apertura
    /// y la consulta que se preguntaron, para tirar la respuesta si ya no es
    /// lo que hay escrito.
    IndiceDeIrA(
        u64,
        String,
        Result<Vec<norte_proto::methods::SemanticHit>, Error>,
    ),
    /// Las conexiones configuradas, con la APERTURA que las pidió (#264).
    Conexiones(
        u64,
        Result<Vec<norte_proto::methods::ConnectionEntry>, Error>,
    ),
    /// Un lote de resultados, con la época de la búsqueda que lo pidió.
    Resultados(u64, Box<norte_proto::methods::SearchHits>),
    /// La respuesta de una consulta SEMÁNTICA: entera, de una vez.
    Semanticos(u64, Result<Vec<norte_proto::methods::SemanticHit>, Error>),
    /// La comparación tiene Task: se bautiza para poder cancelarla.
    ComparacionViva(u64, norte_proto::TaskId),
    /// El plan de sincronización tiene Task.
    PlanDeSyncVivo(u64, norte_proto::TaskId),
    /// El `sync.apply` fue aceptado y esta es su Task.
    SyncAplicando(u64, norte_proto::TaskId),
    /// El `sync.apply` falló. El `bool` dice si se SABE que no escribió nada:
    /// un rechazo (política, conflicto, ruta inválida) lo sabe, porque el
    /// daemon contestó; un transporte caído NO, porque la petición pudo
    /// llegar y estar corriendo ahora mismo. Soltar el pestillo en el segundo
    /// caso invita a aplicar el mismo plan dos veces sobre el mismo destino.
    SyncNoAplicado(u64, bool),
    /// El informe de una sincronización terminada.
    InformeDeSync(
        u64,
        norte_proto::TaskState,
        Box<Result<norte_proto::methods::SyncReportResult, Error>>,
    ),
    /// El daemon rechazó el plan: no habrá Task ni panel.
    PlanDeSyncFallido(u64),
    /// Los bytes del fichero de sumas que se va a comprobar (#311).
    FicheroDeSumas(Box<VPath>, Box<Result<Vec<u8>, Error>>),
    /// Los digests que calculó esa Task, con el ESTADO con el que terminó
    /// (#311): un informe de una Task cancelada está a medias, y compararlo
    /// acusaría a ficheros que nadie llegó a leer.
    InformeDeSumas(
        norte_proto::TaskId,
        norte_proto::TaskState,
        Box<Result<norte_proto::methods::FsChecksumReportResult, Error>>,
    ),
    /// Un evento del plan: un lote de pasos, o su cierre.
    EventoDeSync(u64, Box<norte_client::SyncPlanEvent>),
    /// Un lote de filas comparadas.
    FilasComparadas(u64, Box<norte_proto::methods::CompareRowsBatch>),
    /// Lo que el modelo propuso, con la época de la petición que lo pidió.
    PlanIa(
        u64,
        Box<Result<norte_proto::methods::AiRenamePlanResult, Error>>,
    ),
    /// El plan de ORGANIZAR que propuso un productor (fase 8), con la época
    /// de la petición que lo pidió. Uno solo para el modelo y para un plugin:
    /// los dos producen el mismo plan y la misma revisión.
    PlanOrganizar(
        u64,
        Box<Result<norte_proto::methods::AiOrganizePlanResult, Error>>,
    ),
    /// El veredicto del core sobre ese plan, con la MISMA época: entre pedir
    /// el uno y el otro el lector puede haber descartado la revisión, y un
    /// veredicto sobre un plan que ya no está en pantalla no se aplica.
    PlanDeLote(
        u64,
        Box<Result<norte_proto::methods::FsRenameBatchPlanResult, Error>>,
    ),
    /// Lo que los plugins dijeron de la ventana visible de un hueco.
    ///
    /// Insignias y valores de columna juntos: son la misma pregunta sobre las
    /// mismas rutas y viajan en la misma respuesta.
    Adornos(Box<Adornos>),
    /// Los bytes enteros de una imagen que el visor aceptó.
    Imagen(RequestToken, Result<Vec<u8>, Error>),
    /// La vista CON ESTILO que un previewer dio del fichero del visor (ADR
    /// 0141), o `None` si ninguno casó. Llega DESPUÉS de abrir: el visor se
    /// abre con la vista cruda en cuanto se lee y esto la sustituye.
    Estilo(
        RequestToken,
        Option<norte_proto::methods::PluginPreviewStyled>,
    ),
    /// La MINIATURA que un plugin dio del fichero del visor (ADR 0107), o
    /// `None` si ninguno casó o el que casó no supo.
    Miniatura(RequestToken, Option<norte_proto::methods::PluginThumbnail>),
    /// La búsqueda de esta época ya tiene Task: este es su id.
    ///
    /// Llega por su cuenta y no dentro del primer lote porque puede no haber
    /// primer lote: el core no manda lotes vacíos.
    BusquedaViva(u64, norte_proto::TaskId),
    /// La búsqueda de esta época NO llegó a encolarse, y con qué error.
    ///
    /// Ahí no hay Task, así que el desenlace no puede llegar por el progreso:
    /// sin esto la vista se quedaba diciendo «buscando…» para siempre sobre
    /// una búsqueda que no existe, mientras el error pasaba por la barra y se
    /// lo llevaba la siguiente tecla.
    BusquedaRota(u64, Box<Error>),
    /// Los volúmenes, pedidos por la BARRA LATERAL.
    ///
    /// Aparte de los del selector por el mismo motivo que los dos catálogos
    /// de plugins: son dos superficies con dos vidas.
    SitiosVolumenes(Result<Vec<norte_proto::methods::Volume>, Error>),
    /// Los volúmenes para el PIE de los listados (spec 2026-09-10). Aparte
    /// de los de sitios y del selector por lo mismo: otra vida, y llega sin
    /// que nadie haya abierto nada.
    VolumenesDePie(Result<Vec<norte_proto::methods::Volume>, Error>),
    /// Los subdirectorios de una rama del ÁRBOL, ya filtrados y ordenados.
    ///
    /// `None` = la rama no se dejó leer; lo decide
    /// `Tree::branch_unreadable` (vacía, o re-anclar si era la raíz).
    RamasDeArbol(VPath, Option<Vec<VPath>>),
    /// La sesión de un panel se cerró (#140): el hueco, cómo fue, y a dónde va
    /// ese panel ahora.
    ///
    /// El destino viaja DENTRO del mensaje porque se decidió antes de soltar
    /// la sesión: después, la ruta del panel ya no sirve para elegirlo.
    Desconectada(u32, Result<bool, Error>, VPath),
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
        // Los efectos NATIVOS van por su propio canal: llevan rutas y van al
        // proceso que hospeda, no a la webview. Un buffer pequeño porque son
        // gestos de una persona —copiar una ruta, abrir un fichero— y no un
        // flujo: si alguna vez se llenara, lo que se pierde es un gesto que
        // se puede repetir, y no un trozo de la pantalla.
        let (nativos, _) = broadcast::channel(16);
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
        // Lo que la sesión dijo de esta ventana —dueña o suelta— va a la
        // barra desde el primer frame: el cambio se descarta porque la foto
        // del arranque lleva la barra entera.
        let _ = estado.cambio_de_banners();
        // Y después `[profile.start]`, FUERA de `leer_sesion` a propósito: esa
        // vuelve pronto por cuatro caminos —sin sesión, de una versión futura,
        // revisión 0, cuerpo ilegible— y tres de ellos son justo el caso para
        // el que la clave existe: una instalación nueva, o un perfil copiado de
        // otra máquina (ADR 0098). Dentro no se sembraba nunca.
        //
        // El orden ES la precedencia: la sesión, después lo que el perfil dice
        // de los huecos que ella no conoce, y encima el directorio que un
        // humano acaba de teclear.
        for (id, destino) in estado.siembra_de_perfil() {
            if let Some(hueco) = estado.huecos.get_mut(&id) {
                hueco.pane.begin_loading(destino);
            }
        }
        estado.fijar_dir_pedido();
        // El primer listado se pide ANTES de publicar nada: el snapshot 0
        // describe una pantalla que ya existe, no una promesa.
        estado.listar_inicial(&backend, &tx2).await;
        // La barra lateral, si la disposición coloca una: los favoritos salen
        // de la configuración que ya está cargada, y los volúmenes se PIDEN y
        // llegan después — preguntarlos monta y consulta espacio en cada
        // filesystem, y la ventana no espera a eso para pintar.
        if estado.hueco_de_sitios().is_some() {
            estado.sembrar_sitios();
            estado.pedir_sitios(&backend, &tx2);
        }
        // Y qué PANELES aportan los plugins (fase 3). Sin preguntar si hay
        // hueco para uno: la disposición guardada puede traerlo y ese hueco no
        // se coloca hasta que su kind está declarado. La única puerta la pone
        // `pedir_paneles`, y es la de los efectos.
        //
        // Llega después de la primera foto, como los volúmenes: declarar un
        // kind repinta, y esperar a una RPC para enseñar la pantalla sería
        // pagar por lo que casi nunca hay.
        Estado::pedir_paneles(&backend, &tx2);
        // Y se sondea lo que ya se ve: el listado local no trae tamaño ni
        // fecha (#52), así que sin esto la primera pantalla nace con dos
        // columnas en blanco y no se llenan hasta que algo la mueva.
        let visibles: Vec<u32> = estado.huecos.keys().copied().collect();
        for slot in visibles {
            estado.sondear(slot, &backend, &tx2);
        }
        let primero = estado.snapshot();

        bombear_tic_de_sesion(tx.clone());

        bombear_canales_del_backend(backend.as_ref(), &tx, estado.efectos);
        let host = Self {
            inbox: tx,
            updates: updates.clone(),
            nativos: nativos.clone(),
            instance,
        };
        estado.escritorio.nativos = Some(nativos);
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

    /// Los bytes de la imagen que el visor tiene abierta, si ya llegaron.
    ///
    /// Aparte de la foto A PROPÓSITO: ocho megas en el flujo de parches es un
    /// mensaje que se reenvía entero en cada `Resync`. El renderer los pide
    /// por aquí, hace un `blob:` y lo revoca al cerrar (ADR 0069).
    ///
    /// No lleva RUTA. El renderer no nombra ficheros —ni aquí ni en ningún
    /// otro sitio— así que lo que se sirve es la imagen que el host mismo
    /// decidió abrir, y no la que alguien pida.
    ///
    /// # Errors
    /// [`UiError::Down`] si el actor ya no está.
    pub async fn image_bytes(&self) -> Result<Option<std::sync::Arc<Vec<u8>>>, UiError> {
        let (tx, rx) = oneshot::channel();
        self.inbox
            .send(Mensaje::BytesDeImagen(tx))
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

    /// Los efectos NATIVOS que el host pide: portapapeles, abrir con el
    /// escritorio, terminal.
    ///
    /// Canal aparte del de la vista a propósito: esto lleva rutas y va al
    /// PROCESO que hospeda, no a la webview, que no tiene permiso para
    /// ejecutar nada (ADR 0066 D11). Un frontend que no se suscriba
    /// sencillamente no hace ninguna, que es lo que se quiere de un frontend
    /// que no sepa hacerlas.
    #[must_use]
    pub fn native_effects(&self) -> broadcast::Receiver<crate::dto::NativeEffect> {
        self.nativos.subscribe()
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
// El REPARTO de mensajes del actor: un brazo por variante, y cada brazo
// delega. Largo por número de variantes, no por lógica — partirlo en dos
// mitades arbitrarias solo escondería dónde se atiende cada mensaje.
#[expect(
    clippy::too_many_lines,
    reason = "reparto de mensajes del actor: largo por variantes, no por lógica"
)]
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
            Mensaje::BytesDeImagen(responde) => {
                let _ = responde.send(estado.imagen.clone());
            }
            Mensaje::Catalogo(datos) => {
                let (scheme, catalogo) = *datos;
                let _ = updates.send(estado.aplicar_catalogo(scheme, catalogo));
            }
            Mensaje::Aprobacion(req) => {
                // Y por el ESCRITORIO si la ventana no está delante (#285).
                // Es el aviso que justifica el mecanismo: una aprobación
                // caduca sola si nadie contesta, así que no enterarse cambia
                // el desenlace — al revés que una copia, que sigue terminada
                // cuando vuelves.
                estado.avisar_de_aprobacion(&req);
                for u in estado.abrir_aprobacion(&req, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::AprobacionCaducada(approval_id) => {
                for u in estado.caduca_aprobacion(approval_id) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::AprobacionNoEntregada(approval_id, clave) => {
                // NOMBRA la aprobación (#279): con dos apiladas, «la
                // aprobación no llegó» no dice cuál de las dos, y son
                // decisiones de seguridad sobre operandos distintos.
                for u in estado.decir_con(clave, &[("id", &approval_id.to_string())]) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Listado(datos) => {
                for u in estado.aterrizar_listado(*datos, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::RegistroTic(epoca) => {
                for u in estado.tic_de_registro(epoca, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::RegistroRemoto(epoca, res) => {
                for u in estado.aterrizar_registro_remoto(epoca, *res) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::RegistroNivel(epoca, res) => {
                for u in estado.aterrizar_nivel_remoto(epoca, *res) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::SecretoEntregado(datos) => {
                let (slot, dir, res) = *datos;
                for u in estado.secreto_entregado(slot, &dir, res, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Contenido(datos) => {
                let (token, path, leido, preview) = *datos;
                if let Some(u) = estado.abrir_visor(token, path, leido, preview, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::PreviewContenido(datos) => {
                let (slot, (token, path, leido, preview)) = *datos;
                if let Some(u) = estado.aterrizar_preview(slot, token, path, leido, preview) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::PanelContenido(datos) => {
                let (slot, token, res) = *datos;
                if let Some(u) = estado.aterrizar_panel(slot, token, res) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::MapaContenido(datos) => {
                let (slot, token, res) = *datos;
                if let Some(u) = estado.aterrizar_mapa(slot, token, res) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Fondo(f) => {
                for u in estado.aplicar_de_fondo(*f, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Hidratado(datos) => {
                if let Some(u) = estado.aterrizar_sondas(*datos, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::MasEntradas(datos) => {
                for u in estado.aterrizar_lote(*datos, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Conexion(ev) => {
                for u in estado.cambio_de_conexion(ev, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Degradada(d) => {
                let _ = updates.send(estado.sesion_degradada(*d));
            }
            Mensaje::Fallida(f) => {
                for u in estado.conexion_fallida(&f) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::AvisoPlugin(n) => {
                for u in estado.aviso_de_plugin(&n) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::TaskNueva(task) => {
                let (task, afectados, reintento) = *task;
                for u in estado.registrar_task(task, afectados, reintento, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Capacidades(slot, dir, caps) => {
                if let Some(u) = estado.aplicar_capacidades(slot, &dir, caps) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::TaskFallida(e) => {
                for u in estado.task_fallida(&e) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::TaskDeLoteRechazada(e) => {
                for u in estado.rechazo_de_lote(&e) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Progreso(p) => {
                for u in estado.progreso(&p, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::TaskCaducada(id, epoca) => {
                for u in estado.caducar_task(id, epoca, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Decir(clave) => {
                for u in estado.decir(clave) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Tira => {
                for u in estado.despertar_tira(&backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::TemaPersistido(fallo) | Mensaje::AnchoPersistido(fallo) => {
                if let Some(clave) = fallo {
                    for u in estado.decir(clave) {
                        let _ = updates.send(u);
                    }
                }
            }
            Mensaje::SesionTic => {
                estado.empujar_sesion(&backend, &buzon);
                // Y un segundo más para el aviso de la barra (spec 2026-09-10).
                if let Some(u) = estado.caducar_aviso() {
                    let _ = updates.send(u);
                }
            }
            Mensaje::SesionPuesta(datos) => {
                let (res, cuerpo) = *datos;
                for u in estado.sesion_puesta(res, cuerpo, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Relevado { soltada } => {
                for u in estado.relevo_terminado(soltada) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::SesionReleida(res) => {
                for u in estado.sesion_releida(res) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::TemaResuelto(datos) => {
                let (spec, resultado) = *datos;
                match resultado {
                    Ok(tema) => {
                        estado.tema_puesto(&spec, &tema);
                        // Foto y no parche: cambiar de tema mueve los colores
                        // de TODA la pantalla, y el renderer los reenchufa
                        // desde el catálogo, no desde un campo de la vista.
                        let snap = estado.snapshot();
                        let _ = updates.send(estado.sobre(UiUpdate::Snapshot(Box::new(snap))));
                    }
                    // Un tema que no se puede leer NO deja la ventana sin
                    // colores: se queda el que había y se dice por qué.
                    Err(clave) => {
                        for u in estado.decir(clave) {
                            let _ = updates.send(u);
                        }
                    }
                }
            }
            Mensaje::Informe(informe) => {
                let (epoca, task_id, cual) = *informe;
                for u in estado.informe(epoca, task_id, &cual) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::CreadoComprobado(comprobado) => {
                let (path, veredicto) = *comprobado;
                for u in estado.abrir_lo_comprobado(path, veredicto) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::FavoritoPersistido(hecho) => {
                let (nombre, destino, fallo) = *hecho;
                for u in estado.favorito_persistido(&nombre, Some(destino), fallo) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::FavoritoQuitado(hecho) => {
                let (nombre, fallo) = *hecho;
                for u in estado.favorito_persistido(&nombre, None, fallo) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::PerfilGuardado(hecho) => {
                let (nombre, fallo) = *hecho;
                for u in estado.perfil_guardado(&nombre, fallo) {
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
        // El visor acoplado sigue al cursor (#291), y el cursor lo mueve
        // cualquier mensaje: una tecla, un listado que aterriza, un panel
        // que se abre. Se pregunta DESPUÉS de cada uno, como la TUI lo
        // pregunta en cada frame: qué debería estar enseñando cada hueco de
        // preview colocado, y si no es lo que enseña, se pide.
        for u in estado.sondear_previews(&backend, &buzon) {
            let _ = updates.send(u);
        }
        // Y el panel de un plugin (fase 3), por lo mismo y en el mismo sitio:
        // su guest recibe el directorio y la fila bajo el cursor, así que
        // cualquier mensaje puede cambiar lo que debería estar enseñando.
        for u in estado.sondear_paneles(&backend, &buzon) {
            let _ = updates.send(u);
        }
        // Y el mapa de disco (fase 4), en el mismo sitio y por lo mismo: sigue
        // al DIRECTORIO del listado al que está atado, así que un `cd` —venga
        // de donde venga— cambia lo que debería estar enseñando. No sigue al
        // cursor: mover una fila no cambia de qué está hecho el directorio, y
        // sondear por cursor sería medir un `$HOME` en cada flecha.
        for u in estado.sondear_mapas(&backend, &buzon) {
            let _ = updates.send(u);
        }
        // Y la línea de tiempo (#359): la primera página cuando aparece su
        // hueco, y la siguiente cuando el cursor llega abajo.
        estado.sondear_lineas(&backend, &buzon);
        // Y la hoja de atributos, por lo MISMO y en el mismo sitio: también
        // sigue al cursor y tampoco tiene otro camino hasta el renderer. Va
        // después del visor para que, cuando los dos cambian a la vez, la
        // foto que se manda ya lleve los dos al día.
        for u in estado.sondear_hojas() {
            let _ = updates.send(u);
        }
    }
}

/// El área que el renderer dice tener, en celdas de layout.
///
/// El árbol se reparte en una rejilla y no en píxeles a propósito: los
/// mínimos de cada kind están declarados así y los comparte con el TUI, que
/// es lo que hace que «este panel no cabe» signifique lo mismo en las dos
/// superficies.
/// El id de una columna: una IDENTIDAD, entera o vacía.
///
/// NO se enmascara y NO se recorta, al contrario que todo lo demás. Es lo que
/// vuelve para ordenar y lo que nombra la columna de cada celda, así que las
/// dos transformaciones lo rompen, y de formas distintas:
///
/// - Enmascarar no es inyectivo. Un `norte.toml` con dos columnas `attr:`
///   que solo se diferencien en un carácter invisible daba DOS cabeceras con
///   el MISMO id enmascarado, y la resolución hace `find`: pulsar la segunda
///   ordenaba por la primera. Es la regla del ADR 0061 —«recortar no es
///   inyectivo, y esto es una clave»— aplicada al enmascarado, en una
///   superficie que el ADR no cubría.
/// - Recortar era además ASIMÉTRICO: el id salía con `clamp_display` y se
///   comparaba sin él, así que uno largo no casaba nunca con su propia
///   columna y caía a un `parse` sobre una cadena acabada en `…`.
///
/// Un id que no cabe en el tope del bridge se manda VACÍO: una clave que no
/// casa con nada es un fallo visible; una que casa con la equivocada, no. Lo
/// que se PINTA es `label`, que sí va enmascarado, y el renderer solo usa el
/// id en un `data-` y para mandarlo de vuelta.
fn identidad_de_columna(id: &norte_frontend::columns::ColumnId) -> String {
    let s = id.to_string();
    if s.len() > crate::bridge::MAX_STRING_BYTES {
        return String::new();
    }
    s
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
/// La política que corresponde a cada salida del diálogo de colisión (#274).
///
/// `None` para `cancel` y para cualquier otra cosa: no elegir es una respuesta
/// válida —la task fallida se queda como está— y una opción que el diálogo no
/// ofreció no se interpreta.
///
/// Los ids son los del catálogo compartido de `dialog.*`, los mismos que ata
/// el TUI: dos vocabularios para la misma pregunta serían dos sitios donde una
/// tecla acaba haciendo otra cosa.
fn politica_de_colision(choice: &str) -> Option<norte_proto::CollisionPolicy> {
    use norte_proto::CollisionPolicy as P;
    match choice {
        "overwrite" => Some(P::Overwrite),
        "newer" => Some(P::Newer),
        "rename" => Some(P::RenameAuto),
        "skip" => Some(P::Skip),
        _ => None,
    }
}

/// Manda el «sí» al daemon, y si no sale bien lo CUENTA con la frase que toca.
///
/// Aparte porque el desenlace tiene tres formas distintas de salir mal y
/// ninguna de ellas es asunto de la función que decide qué hacer con un
/// diálogo (#279).
fn lanzar_aprobacion(
    approval_id: u64,
    backend: &Arc<dyn HostBackend>,
    buzon: &mpsc::Sender<Mensaje>,
) {
    let backend = Arc::clone(backend);
    let buzon = buzon.clone();
    tokio::spawn(async move {
        if let Err(e) = backend.policy_decide(approval_id, true).await {
            let _ = buzon
                .send(Mensaje::AprobacionNoEntregada(
                    approval_id,
                    clave_de_aprobacion_perdida(&e),
                ))
                .await;
        }
    });
}

/// Qué frase toca cuando un `policy.decide` no sale bien (#279).
///
/// Las tres formas de «esa aprobación ya no está» piden consejos distintos, y
/// antes las tres se pintaban como la primera: «tu clic no llegó» sobre una
/// aprobación que sí llegó y venció es una frase que manda al lector a
/// reintentar algo que ya se decidió sin él.
///
/// Un `reason` que este binario no conozca cae en «desconocida» y nunca en una
/// de las otras: el vocabulario del wire puede crecer, y adivinar en una
/// superficie de seguridad es peor que decir que no se sabe.
fn clave_de_aprobacion_perdida(e: &norte_proto::Error) -> &'static str {
    match e {
        norte_proto::Error::ApprovalGone { reason } => match reason.as_str() {
            "expired" => "msg-approval-expired",
            "already-decided" => "msg-approval-already-decided",
            _ => "msg-approval-unknown",
        },
        // Cualquier otro error es que la petición NO llegó (el daemon se cayó
        // entre la pregunta y la respuesta), que es el caso original.
        _ => "msg-approval-not-delivered",
    }
}

/// La CLASE de una task, en el vocabulario del bridge.
///
/// Un `match` y no `format!("{:?}").to_lowercase()`. El `Debug` daba
/// `renamebatch` y `dirsize` para variantes cuya clave en el catálogo es
/// `rename-batch` y `dir-size`, así que esas dos se pintaban como su propio
/// identificador.
///
/// `TaskKind` es `#[non_exhaustive]`, así que el comodín es obligatorio y
/// esto NO deja de compilar al aparecer una variante: lo que hace es que caiga
/// en `unknown`, que es una clave que EXISTE en el catálogo. Una task de un
/// daemon más nuevo se lee «tarea» en vez de leerse `gui-task-kind-frobnicate`.
fn clase_de_task(kind: norte_proto::TaskKind) -> &'static str {
    use norte_proto::TaskKind as K;
    match kind {
        K::Copy => "copy",
        K::Move => "move",
        K::Delete => "delete",
        K::Undo => "undo",
        K::Search => "search",
        K::Mkdir => "mkdir",
        K::Create => "create",
        K::Index => "index",
        K::Embed => "embed",
        K::RenameBatch => "rename-batch",
        K::Compare => "compare",
        K::DirSize => "dir-size",
        K::Pack => "pack",
        K::TestArchive => "test-archive",
        K::Split => "split",
        K::Combine => "combine",
        K::SyncPlan => "sync-plan",
        K::Sync => "sync",
        // #311 y #314: caían en `unknown`, o sea que una comprobación de sumas
        // y un cambio de permisos se leían «tarea» en la franja.
        K::Checksum => "checksum",
        K::SetMode => "set-mode",
        // `Unknown` y lo que traiga un daemon más nuevo, juntos: ver el doc
        // de arriba. `unknown` es una clave de verdad, no un identificador
        // pintado crudo.
        K::Unknown | _ => "unknown",
    }
}

/// Los valores de las columnas `plugin:` CONFIGURADAS del esquema.
///
/// La validación de pertenencia y el dedupe de colisiones viven en el modelo
/// COMPARTIDO (`validated_plugin_requests`): una sola definición para los dos
/// frontends, y una colisión de id bare se queda en blanco antes que atribuir
/// una columna al plugin equivocado.
///
/// Fail-soft POR COLUMNA: una que falla deja celdas vacías, jamás convierte
/// el listado en un error. Y `superado` corta entre RPCs, porque una tanda a
/// la que el listado ya relevó no tiene por qué gastar las que le quedan.
async fn celdas_de_plugin(
    backend: &Arc<dyn HostBackend>,
    pedidas: &[(String, String)],
    paths: &[VPath],
    superado: impl Fn() -> bool,
) -> (
    std::collections::HashMap<String, std::collections::HashMap<VPath, String>>,
    std::collections::BTreeMap<String, String>,
) {
    let mut out = std::collections::HashMap::new();
    let mut rotulos = std::collections::BTreeMap::new();
    if pedidas.is_empty() {
        return (out, rotulos);
    }
    let Ok(lista) = backend.plugin_list().await else {
        return (out, rotulos);
    };
    for (plugin, columna) in
        norte_frontend::columns::validated_plugin_requests(pedidas, &lista.plugins)
    {
        // El RÓTULO que el manifiesto le puso, para que la cabecera no diga
        // el id (`acme.git/status`). Texto de un plugin: se enmascara y se
        // acota como cualquier cabecera. Un manifiesto que lo deja vacío se
        // queda sin rótulo y la cabecera cae al id, como antes.
        if let Some(h) = lista
            .plugins
            .iter()
            .find(|p| p.id == plugin)
            .and_then(|p| p.columns.iter().find(|c| c.id == columna))
        {
            let sano: String = norte_frontend::columns::sanitize_header(&h.header)
                .chars()
                .take(norte_frontend::columns::HEADER_MAX_CHARS)
                .collect();
            if !sano.is_empty() {
                rotulos.insert(
                    norte_frontend::columns::plugin_display_id(&plugin, &columna),
                    sano,
                );
            }
        }
        if superado() {
            return (out, rotulos);
        }
        let crudos = backend
            .plugin_column_values(plugin.clone(), columna.clone(), paths.to_vec())
            .await
            .unwrap_or_default();
        let sanos = norte_frontend::columns::sanitize_column_values(paths, &crudos);
        out.insert(
            norte_frontend::columns::plugin_display_id(&plugin, &columna),
            sanos,
        );
    }
    (out, rotulos)
}

/// Cómo se llama una columna en el selector, y si eso difiere de lo real.
///
/// Por `header_label`, que es la MISMA función que pinta la cabecera del
/// listado: cómo se llama una columna no puede depender de dónde se lea. Un
/// id que no parsea se enseña tal cual —es intención de configuración del
/// usuario y el selector jamás la limpia— y por eso también se enmascara.
fn etiqueta_de_columna(
    r: &norte_frontend::columns_picker::PickerRow,
    esquema: &str,
    columnas: &norte_frontend::columns::ColumnsSettings,
    lang: norte_i18n::Lang,
) -> (String, bool) {
    use norte_frontend::columns::{ColumnId, header_label_in};
    let Ok(cid) = r.id.parse::<ColumnId>() else {
        // No parsea: el id crudo es lo único que se le puede enseñar, y es
        // texto de un fichero de configuración.
        return norte_frontend::display_name(r.id.as_bytes());
    };
    let estilo = columnas.style_for_id(esquema, &cid, None);
    norte_frontend::display_name(header_label_in(&cid, &estilo, None, lang).as_bytes())
}

/// Una IDENTIDAD de texto que cruza el bridge: entera, o vacía.
///
/// Misma regla que [`identidad_de_columna`] y por el mismo motivo (ADR 0061):
/// recortar no es inyectivo, y una clave recortada casa con la equivocada.
fn identidad_de_texto(id: &str) -> String {
    if id.len() > crate::bridge::MAX_STRING_BYTES {
        return String::new();
    }
    id.to_owned()
}

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
    /// Cómo PLIEGA nombres el directorio en el que este hueco está (#268).
    ///
    /// Por UBICACIÓN y no por esquema (#215): un pincho FAT montado bajo el
    /// mismo `file://` que un `/home` sensible a la caja da otra respuesta, y
    /// contestar por el provider sería contestar por el sitio equivocado.
    ///
    /// Se pide al ATERRIZAR y no delante de cada diálogo: preguntarlo en el
    /// momento de copiar metería un viaje al daemon en el camino de F5, que es
    /// la tecla que más se pulsa. `None` = todavía no ha llegado, y entonces no
    /// se pliega nada — la comprobación es una cortesía y el core es la
    /// autoridad.
    ///
    /// Se guardan ENTERAS y no ya destiladas a un `FoldMode`. La respuesta
    /// costó una ronda al daemon y trae más de una cosa que este frontend
    /// necesita: el plegado del destino y, desde la nivelación de la ayuda,
    /// el `READ_ONLY` con el que se atenúa lo que esta ubicación no va a
    /// aceptar. Quedarse solo con lo primero es lo que dejó la ayuda de la
    /// ventana declarando que se puede escribir en cualquier sitio.
    ///
    /// Y van ATADAS a la ruta de la que se preguntaron, en vez de borrarse
    /// cada vez que se piden otras. Lo que las invalida es cambiar de
    /// DIRECTORIO, no volver a listar el mismo: borrarlas al pedir dejaba una
    /// ventana determinista —el aterrizaje re-congela los hechos de la ayuda
    /// tres líneas después de pedirlas— en la que el hueco decía «no consta»
    /// de un sitio que ya había contestado. Casar por ruta también hace
    /// inofensiva una respuesta que llega tarde: si es de otro directorio, no
    /// se lee.
    ///
    /// `None`, o una ruta que no casa, significa «todavía no consta», y
    /// entonces el plegado no se aplica y el solo-lectura lo contesta el
    /// esquema (`norte_frontend::availability::read_only`).
    caps: Option<(VPath, norte_proto::Capabilities)>,
    /// El esquema cuyo orden lleva puesto `pane` ahora mismo (#108).
    ///
    /// `[ui.columns] sort` puede dar un orden POR ESQUEMA, y se reaplica
    /// cuando el hueco aterriza en un esquema distinto — no en cada `cd`,
    /// que es lo que hace el TUI: aquí la sesión RESTAURA el orden, y
    /// reaplicarlo en el primer aterrizaje lo borraría antes de que se vea.
    esquema_del_orden: String,
    /// De dónde vengo y a dónde vuelvo. También compartido.
    historial: History,
    primera_visible: u64,
    visibles: u32,
    /// La petición de listado EN VUELO, si la hay. Una respuesta con otro
    /// testigo llegó tarde: se descarta aquí, en Rust, no se esconde en el
    /// renderer.
    en_vuelo: Option<RequestToken>,
    /// A DÓNDE va la petición en vuelo, si la hay.
    ///
    /// No es lo mismo que `pane.dir()`, y confundirlos era un bug: `dir()`
    /// solo cambia cuando el listado ATERRIZA, así que durante una
    /// navegación el hueco «está» todavía en el directorio que abandona. Un
    /// refresco que se guiara por `dir()` relistaría el viejo y pisaría el
    /// testigo de la navegación, que se descartaría en silencio; y un hueco
    /// que va ENTRANDO en el directorio que una mutación acaba de cambiar no
    /// se reconocería como afectado, y aterrizaría sobre un listado anterior
    /// a la mutación sin que nada lo corrigiera.
    dir_pedido: Option<VPath>,
    /// Las marcas que hay que volver a poner cuando aterrice un REFRESCO.
    ///
    /// Vacío siempre que lo que vuela es una navegación: ahí las filas son de
    /// otro directorio y una marca no significa nada. Se consume al aterrizar.
    marcas_a_restaurar: Vec<VPath>,
    /// La fila que la SESIÓN dejó bajo el cursor, hasta que llegue su listado.
    ///
    /// Espera por lo mismo que las marcas: sobre un pane vacío, poner el
    /// cursor en la fila 12 es ponerlo en la 0. Se consume en el primer
    /// aterrizaje —bueno o malo—, así que nunca cae sobre un listado posterior
    /// de otro sitio. Es un ÍNDICE, el mismo que guarda y restaura la
    /// terminal: un relevo tiene que caer en la misma fila en los dos
    /// sentidos.
    cursor_a_restaurar: Option<usize>,
    /// Hay filas mezcladas que no se han publicado todavía.
    ///
    /// El relleno se calla cuando el lote que mezcla no cambia la ventana
    /// visible (#252), pero cada mezcla sube la ÉPOCA del listado y el
    /// renderer nombra las filas por época: si se callan TODOS los parches,
    /// se queda con una época vieja y sus clics se rechazan por rancios. Esta
    /// bandera es la deuda, y el último lote la salda.
    filas_por_publicar: bool,
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
    /// La decoración que los plugins pusieron sobre cada ruta.
    ///
    /// Por RUTA y no por índice: las decoraciones llegan asíncronas y el
    /// listado se reordena por debajo, así que un índice nombraría otra fila
    /// para cuando aterrizan.
    adornos: std::collections::HashMap<VPath, norte_frontend::Decoration>,
    /// Los valores de cada columna `plugin:`, por id de columna y ruta.
    celdas_plugin: std::collections::HashMap<String, std::collections::HashMap<VPath, String>>,
    /// Hay una tanda de decoración en vuelo para este hueco.
    adornando: bool,
    /// El directorio al que va una navegación que CUENTA como paso, hasta que
    /// su listado llegue (spec 2026-09-15 D6): entonces se suma a los
    /// populares, y si falla se olvida. Lo relevan la siguiente navegación del
    /// hueco y el propio aterrizaje.
    visita_pendiente: Option<VPath>,
    /// Las rutas que ya se pidieron decorar (hayan contestado o no). Misma
    /// memoria que `sondeados` y por el mismo motivo: sin ella, un plugin
    /// que no decora nada se vuelve a preguntar en cada repintado.
    adornadas: std::collections::HashSet<VPath>,
    /// La generación de los adornos: sube cada vez que se OLVIDAN. Una
    /// tanda en vuelo lleva la suya, y si aterriza con otra se tira y se
    /// vuelve a pedir: apagar un decorador desde el gestor con una tanda a
    /// medio camino dejaba sus insignias pegadas a las filas.
    gen_adornos: u64,
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
    ///
    /// Un enum y no un `String` desde #327: hay un diálogo que pide una
    /// CONTRASEÑA, y guardarla aquí como texto normal la mandaría a un
    /// `Debug`, al heap sin pisar, y —lo peor— a la proyección de pintado por
    /// el mismo camino que un nombre de fichero. Con dos formas, quien escriba
    /// tiene que decir cuál es.
    tecleado: Tecleado,
    /// Este diálogo se ABRIÓ SOLO, y todavía no se le ha reconocido.
    ///
    /// Una aprobación y el informe de un lote aparecen sin que nadie acabe de
    /// pulsar nada: llegan cuando el daemon contesta, encima de lo que el
    /// lector estuviera haciendo, y se quedan la entrada. Con esto, la
    /// primera RESPUESTA solo dice «ya lo veo» —la misma regla que la
    /// revisión de un plan, y por el mismo motivo—.
    ///
    /// Respuesta, no tecla: la comprobación vive en `responder_dialogo`, que
    /// es por donde pasan las dos entradas. Cuando solo cubría el teclado, un
    /// clic ya en marcha sobre el «Confirmar» de una confirmación aterrizaba
    /// sobre el «Aprobar» de una aprobación de agente que acababa de
    /// pintarse en el mismo sitio.
    ///
    /// Denegar y cancelar están exentos, igual que `Escape`: quitarse de
    /// encima algo que uno no ha pedido tiene que salir a la primera.
    ///
    /// `true` en un diálogo que abrió un gesto: ahí la respuesta siguiente SÍ
    /// es una respuesta, porque la pregunta la hizo quien está delante.
    reconocido: bool,
}
/// Lo tecleado en el campo de un diálogo, según lo que sea.
///
/// Dos formas y no un `String` porque el trato es DISTINTO y la diferencia no
/// puede quedar a criterio de quien llama: un nombre de fichero se pinta
/// enmascarado, una contraseña se pinta como puntos y no se pinta nunca. Con
/// un solo tipo, la única barrera era acordarse — y el modo de fallo era una
/// contraseña saliendo por el mismo `display_name` que un nombre, o dentro de
/// un `Debug` del estado entero.
#[derive(Debug)]
enum Tecleado {
    /// Un nombre, una instrucción, una plantilla: texto que se enseña.
    Texto(String),
    /// Una contraseña, y el host NO la tiene mientras se escribe.
    ///
    /// Sin datos dentro, y eso es la decisión: el campo lo enmascara el
    /// `input type=password` del renderer, así que aquí no hay nada que contar
    /// ni que pintar, y la contraseña cruza una sola vez —con la respuesta,
    /// en `UiAction::Dialog::secret`— en el instante en que el lector decide
    /// entregarla. Lo que el host no tiene no se le puede escapar por un
    /// `Debug`, por una foto ni por un log.
    ///
    /// La variante existe igual porque es la barrera de TIPO: `texto()`
    /// devuelve nada sobre ella, así que una pendiente de texto que aterrizara
    /// por error sobre este diálogo no puede leer un secreto — no hay ninguno.
    Secreto,
    /// Un FORMULARIO: varios campos a la vez (puente 91).
    ///
    /// El modelo es el COMPARTIDO (`norte_frontend::search::SearchForm`), no
    /// uno de esta ventana: el terminal pregunta la misma búsqueda, y dos
    /// modelos divergen en silencio — que es lo que ya pasó con el desenlace
    /// de una búsqueda (ADR 0077).
    ///
    /// `Box` porque es el doble de grande que las otras dos variantes juntas
    /// y hay un `Tecleado` por diálogo abierto.
    Formulario(Box<norte_frontend::search::SearchForm>),
}

/// El tope de una contraseña, del crate COMPARTIDO: lo que se rechaza aquí es
/// exactamente lo que aquel puede guardar sin reasignar.
use norte_frontend::secret::SECRET_MAX_CHARS;

impl Tecleado {
    /// El texto, para las pendientes que trabajan con texto.
    ///
    /// Vacío para un secreto, a propósito: si alguna vez una pendiente de
    /// texto acabara sobre un diálogo de contraseña, lo que recibe es nada.
    /// Un `panic!` sería peor —tumbar la ventana por un error de cableado— y
    /// aquí no hay nada más que devolver.
    fn texto(&self) -> &str {
        match self {
            Self::Texto(s) => s,
            // Un formulario no tiene «el» texto: tiene siete campos, y una
            // pendiente de texto que aterrizara aquí por error no puede
            // llevarse uno cualquiera haciéndolo pasar por el que pidió.
            Self::Secreto | Self::Formulario(_) => "",
        }
    }
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
    /// Copiar al panel lo que se SOLTÓ desde el escritorio (#283).
    ///
    /// Separada de [`Pendiente::Transferir`] por dos razones, y ninguna es
    /// cosmética: aquí los orígenes no salen de ningún panel —así que no hay
    /// marcas que consumir, y consumirlas borraría una selección que el lector
    /// hizo para otra cosa—, y el verbo es siempre COPIAR: mover lo que
    /// arrastró otra aplicación significaría borrarlo de donde ese proceso lo
    /// tenga, y esta ventana no ha preguntado eso.
    Soltar {
        /// Qué llegó, ya convertido y filtrado.
        paths: Vec<VPath>,
        /// Dónde cae, que es el directorio del panel activo cuando se soltó.
        destino: VPath,
    },
    /// Preguntar al índice por SIGNIFICADO. Lo que se teclea es la consulta,
    /// y no lleva más operandos: el alcance es el índice entero.
    ConsultaSemantica,
    /// CERRAR la ventana, ya confirmado (`[ui] confirm_quit`).
    Salir,
    /// Entregar el secreto de una conexión y REINTENTAR la navegación que
    /// `Error::SecretNeeded` interrumpió (#325/#327).
    ///
    /// Lleva a dónde iba el panel porque esta pendiente es el único sitio de
    /// esta ventana donde una navegación sobrevive a la respuesta que la
    /// interrumpió: el listado ya volvió con error, y el hueco se quedó
    /// enseñando el directorio que abandonaba. Sin el destino aquí, entregar
    /// el secreto dejaría al lector con la contraseña dada y el panel donde
    /// estaba.
    EntregarSecreto {
        /// Nombre de la entrada de `connections.toml` que lo pide — la MISMA
        /// cadena que va en `connection.provide_secret`. Sale del error del
        /// core, no del servidor remoto.
        conn: String,
        /// El hueco que estaba navegando.
        slot: u32,
        /// A dónde reintentar.
        dir: VPath,
    },
    /// Marcar —o desmarcar— por patrón. Lo que se teclea es el glob.
    Patron {
        /// `true` añade marcas, `false` las quita.
        marcar: bool,
    },
    /// Volver a intentar una transferencia que CHOCÓ, con otra política
    /// (#274).
    ///
    /// La pregunta no es si seguir: es CUÁL de las cuatro salidas, así que la
    /// política sale del `choice` que el lector pulsó y no de aquí. Cancelar
    /// es no elegir ninguna, y entonces la task fallida se queda como estaba —
    /// que es lo que pasaba siempre antes de esto.
    Reintentar {
        /// Con qué se relanza.
        con: Reintento,
    },
    /// Partir el fichero bajo el cursor en trozos del tamaño que se teclea
    /// (#132).
    Partir {
        /// Qué se parte.
        path: VPath,
        /// Dónde caen los trozos. El panel DESTINO, como una copia: partir un
        /// fichero de un giga donde ya está suele no caber.
        dest_dir: VPath,
    },
    /// Empaquetar lo MARCADO en el contenedor que se teclea (#132).
    ///
    /// Lleva el directorio y no el nombre: el nombre es lo que el lector
    /// escribe, y de él sale el formato. Se resuelve al confirmar, no al
    /// abrir, porque hasta entonces no hay nada que resolver.
    Empaquetar {
        /// Dónde cae el contenedor y, además, la BASE de los nombres que se
        /// guardan dentro: quien desempaquete espera ver lo que veía en
        /// pantalla, no rutas absolutas.
        dir: VPath,
        /// Qué se mete, en orden de listado.
        sources: Vec<VPath>,
    },
    /// Copiar al portapapeles la lista de sumas que el diálogo enseña (#311).
    ///
    /// Los BYTES ya montados —con el escapado de coreutils— y no las filas:
    /// lo que se pinta va saneado, y copiar eso daría un `SHA256SUMS` que no
    /// comprueba los ficheros que nombra.
    CopiarSumas {
        /// Lo que va al portapapeles, tal cual.
        bytes: Vec<u8>,
    },
    /// Cambiar los PERMISOS de estas entradas al modo que se teclee (#314).
    ///
    /// Las rutas se congelan al ABRIR el diálogo, como en el resto de los que
    /// llevan operando: entre la pregunta y el sí el listado puede refrescarse,
    /// y entonces «lo marcado» sería otra cosa.
    Permisos {
        /// Sobre qué, en orden de listado.
        targets: Vec<VPath>,
    },
    /// Deshacer TODO lo que hizo una sesión de agente (#276).
    DeshacerSesion {
        /// La clave OPACA con la que el core la resuelve, cruda.
        sesion: String,
    },
    /// Deshacer lo del humano POSTERIOR a un punto de la línea de tiempo
    /// (#359, `journal.undo_after`). La fila señalada se queda.
    DeshacerHasta {
        /// El corte: el `seq` más nuevo de la fila señalada.
        seq: i64,
        /// El techo: lo más nuevo que el recuento contó (`upto_seq`). Se
        /// congela al PREGUNTAR, como los operandos de cualquier diálogo.
        techo: Option<i64>,
    },
    /// Conceder las capabilities de una extensión.
    ///
    /// Es la única de las cuatro operaciones del gestor que PREGUNTA:
    /// revocar, encender y apagar van en la dirección segura y no hacen
    /// falta dos gestos. La pregunta enumera las capabilities una por línea
    /// —fuera de la frase, como cualquier operando de este host— porque
    /// «aprobar org.ejemplo.foo» sin decir qué concede no es una decisión.
    AprobarExtension {
        /// A quién se le conceden.
        id: String,
        /// QUÉ se enseñó al preguntar, en el orden en que se enseñó.
        ///
        /// Se guarda para volver a comprobarlo al confirmar: el diálogo se
        /// queda las TECLAS, no los mensajes de fondo, así que un catálogo
        /// que aterrice entre la pregunta y el sí puede haber cambiado las
        /// capabilities de esa extensión — y entonces el sí concedería algo
        /// que nadie leyó. Si han cambiado, se vuelve a preguntar.
        capabilities: Vec<String>,
        /// El ancla del manifiesto TAL COMO ESTABA AL PREGUNTAR (#282).
        ///
        /// Aquí, y no releída al confirmar, por la misma razón que las
        /// capabilities de arriba: leerla en el momento del sí devolvería el
        /// ancla del catálogo que haya aterrizado mientras tanto, o sea que el
        /// host certificaría al core «esto es lo que el humano leyó» sobre
        /// algo que el humano no leyó. Y la comparación de capabilities no lo
        /// tapa: `category` y `contributions` —cuándo y cómo se dispara—
        /// entran en el ancla y NO en la lista que se pinta.
        digest: Option<String>,
    },
    /// Desinstalar una extensión (ADR 0104): borrar sus ficheros y retirar
    /// su consentimiento. Pregunta porque no tiene vuelta —no hay
    /// `plugin.install` por el wire— y porque un plugin instalado después
    /// bajo el mismo id nace sin la aprobación que este tenía.
    DesinstalarExtension {
        /// Cuál.
        id: String,
    },
    /// Decidir sobre una op de agente. La op real la tiene el daemon ligada
    /// al id: aquí solo viaja el sí o el no.
    Decidir {
        /// El id que el daemon espera de vuelta.
        approval_id: u64,
        /// La sesión de agente que la pidió, CRUDA, si la petición la traía.
        ///
        /// Cruda y no la del diálogo: lo que el diálogo pinta está
        /// enmascarado, y enmascarar no es inyectivo — usar eso como clave
        /// apuntaría el sí en la fila de otra sesión, o en ninguna.
        session: Option<String>,
    },
    /// Buscar por el subárbol de este directorio. Lo que se teclea es el
    /// patrón.
    Buscar {
        /// Dónde empieza el walk.
        root: VPath,
    },
    /// Crear un fichero VACÍO y abrirlo con el escritorio (#290).
    CrearFichero {
        /// Dónde se crea. El nombre es lo que se teclea.
        dir: VPath,
    },
    /// Guardar un FAVORITO que apunta aquí (#309). El nombre es lo que se
    /// teclea, y viene prellenado con la sugerencia compartida.
    ///
    /// Lleva el destino y no lo lee al confirmar: entre abrir el diálogo y
    /// aceptar, el panel puede haber navegado, y guardar «donde estoy ahora»
    /// haría un favorito que apunta a otro sitio que el que se estaba mirando
    /// cuando se pidió.
    GuardarFavorito {
        /// A dónde apunta el favorito.
        destino: VPath,
    },
    /// Guardar el espacio de trabajo como un perfil (#318, ADR 0079).
    ///
    /// No lleva nada: lo que se guarda es lo que se VE, y eso se lee al
    /// confirmar. La diferencia con el favorito de arriba no es un descuido —
    /// allí el destino es una respuesta a «¿qué estabas mirando?», y aquí la
    /// pregunta es «¿cómo está la pantalla?», que solo tiene sentido AHORA.
    GuardarPerfil,
    /// El valor de una entrada de TEXTO de los ajustes (F11). Lo que se
    /// teclea es el valor; `id` es sobre qué entrada se preguntó.
    ///
    /// Un ID y no una fila: el cursor puede moverse con el diálogo delante,
    /// y el buscador de detrás puede cambiar qué filas hay — una posición
    /// deja de nombrar la misma entrada, y confirmar escribiría el valor en
    /// otra.
    EditarAjuste {
        /// El id del catálogo sobre el que se preguntó.
        id: &'static str,
    },
    /// Crear un directorio dentro de este otro. El nombre lo teclea el
    /// usuario y se valida al confirmar, no al teclear: corregir un nombre a
    /// medias es peor que verlo rechazado al final.
    CrearDirectorio {
        /// Dónde se crea.
        dir: VPath,
    },
    /// Pedirle a un modelo un plan de renombrado para este directorio. Lo
    /// que se teclea es la INSTRUCCIÓN, no un nombre: no muta nada todavía.
    InstruccionIa {
        /// El directorio sobre el que planear.
        dir: VPath,
    },
    /// La PLANTILLA del renombrado en lote (#310). Lo que se teclea es una
    /// plantilla, no un nombre: el plan se genera aquí y se revisa antes de
    /// nada, como el de la IA.
    PlantillaLote {
        /// El directorio sobre el que planear.
        dir: VPath,
        /// Los nombres sobre los que actúa el lote: lo marcado, o el del
        /// cursor. Se fijan al abrir el prompt, como el operando de
        /// cualquier otra operación.
        nombres: Vec<String>,
    },
    /// Renombrar UNA entrada dentro de su propio directorio.
    ///
    /// Lleva la SIEMBRA del campo, no solo la ruta, y esa es la pieza que
    /// hace que la regla 1 se sostenga aquí: si lo que se confirma es
    /// EXACTAMENTE lo que se sembró, no se ha tocado nada y lo que viaja son
    /// los bytes de siempre. Comparar contra la siembra en vez de llevar un
    /// `bool` de «tocado» es lo que sobrevive a que el renderer devuelva el
    /// texto entero en cada evento en vez de un delta.
    Renombrar {
        /// La entrada que se renombra.
        from: VPath,
        /// Lo que se puso en el campo, TAL CUAL (la proyección pintable del
        /// nombre, que para un nombre que no es UTF-8 lleva un U+FFFD).
        siembra: String,
    },
    /// Copiar o mover estas entradas AL directorio de otro hueco.
    ///
    /// El destino viaja ya resuelto —el directorio del hueco con el rol
    /// `Target` en el momento de abrir el diálogo— y no como un id de hueco:
    /// entre abrir la pregunta y responderla el lector puede haber navegado
    /// ese panel, y entonces «el otro» sería otro sitio del que se enseñó.
    Transferir {
        /// El hueco del que salieron las marcas.
        ///
        /// Viaja por el mismo motivo que `destino`, y su ausencia era un
        /// bug: `UiAction::FocusSlot` NO está vedada mientras hay un diálogo
        /// abierto —solo lo están las teclas—, así que un clic en el otro
        /// panel entre la pregunta y la respuesta hacía que las marcas que se
        /// consumían fueran las de OTRO hueco. El de verdad se quedaba
        /// marcado, y el lector volvía a pulsar F5 sobre lo mismo.
        origen: u32,
        /// El DIRECTORIO del que salieron, tal como lo escribe el hueco.
        ///
        /// No se deriva del padre de cada entrada: el padre lo escribe el
        /// PROVIDER y el directorio del hueco puede venir de la config o de
        /// la sesión, así que en macOS (NFD contra NFC) o en un servidor sin
        /// distinción de caja son dos cadenas distintas para el mismo sitio
        /// — y el refresco por comparación byte a byte no encontraría el
        /// panel de origen (ADR 0061).
        origen_dir: VPath,
        /// Qué se transfiere, en orden de listado.
        paths: Vec<VPath>,
        /// El DIRECTORIO al que van. El nombre final se compone aquí, jamás
        /// en el renderer.
        destino: VPath,
        /// `true` = mover.
        mover: bool,
    },
}

/// Tope de lo que se lee de un fichero de sumas (#311): 1 MiB.
///
/// Por encima se RECHAZA en vez de comprobar media lista — el mismo criterio
/// que la terminal, y el mismo que el tope del otro extremo.
const SUMS_MAX_BYTES: u64 = 1024 * 1024;

/// Con qué se puede volver a intentar una transferencia que CHOCÓ (#274).
///
/// La ventana manda siempre `CollisionPolicy::Fail`, que es el default
/// seguro: sobrescribir o renombrar son decisiones del lector. Lo que faltaba
/// era dónde tomarlas — una task fallida y ningún camino hacia delante— y para
/// ofrecerlas hay que recordar QUÉ se pidió: el progreso de la task dice qué
/// fichero va por dentro, no cuál era el origen ni el destino.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reintento {
    /// El origen, tal cual se pidió.
    from: VPath,
    /// El destino EXACTO, con su nombre ya compuesto.
    to: VPath,
    /// Mover en vez de copiar: el reintento tiene que repetir el mismo verbo,
    /// o un «sobrescribir» sobre una copia se convertiría en un movimiento.
    mover: bool,
    /// La reinterpretación de nombres que había AL LANZAR.
    ///
    /// Se captura aquí y no se lee al llegar, y ese es el punto: la colisión
    /// llega ASÍNCRONA, encima de lo que el lector esté haciendo, y entre el
    /// envío y la pregunta cabe cambiar de hueco o ciclar la codificación. El
    /// diálogo tiene que pintar el MISMO texto por el que se navegó, o se
    /// está aprobando un nombre distinto del que se vio. El terminal lo lleva
    /// en su `RetrySpec` desde #98 y lo dice ahí con estas palabras.
    enc: Option<norte_encoding::NameEncoding>,
}

/// La clave Fluent de un error de io LOCAL.
///
/// La CLAVE y no el texto: el host localiza con SU idioma
/// (`norte_i18n::t_in`), no con el del proceso. Y jamás el `Display` del
/// sistema, que el SO traduce a su antojo — «Permission denied (os error
/// 13)» no es un mensaje de norte (#73).
fn clave_de_io(e: &std::io::Error) -> &'static str {
    match e.kind() {
        std::io::ErrorKind::NotFound => "err-not-found",
        std::io::ErrorKind::PermissionDenied => "err-permission-denied",
        std::io::ErrorKind::StorageFull => "err-no-space",
        _ => "err-io",
    }
}

/// El estado semántico. Solo el actor lo toca.
// Cuatro banderas INDEPENDIENTES entre sí: si la ventana tiene el foco, si el
// escritorio pide oscuro, si el journal se rehusó… Son estados de cosas
// distintas que coexisten, no los valores de una sola máquina, que es lo que
// el lint propone y aquí sería falso — plegarlas en enums de dos variantes
// daría cuatro enums, no uno.
#[expect(
    clippy::struct_excessive_bools,
    reason = "estado del controlador: banderas de cosas distintas, no una máquina"
)]
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
    /// «Ir a cualquier sitio», si está abierto (#357). Otro contexto de
    /// entrada con texto libre, como la paleta.
    ir_a: Option<norte_frontend::goto::Goto>,
    /// Cuántas veces se ha abierto «ir a»: las conexiones y el índice que
    /// contesten a una apertura anterior se tiran.
    gen_ir_a: u64,
    /// La pregunta al índice en vuelo, si la hay. Cada tecla la ABORTA y
    /// lanza otra: escribir deprisa no deja tres preguntas vivas contra un
    /// proveedor que cuesta tiempo y puede costar dinero.
    ir_a_indice: Option<tokio::task::JoinHandle<()>>,
    /// El asistente de primer arranque (spec 2026-09-10), mientras está
    /// abierto. Un overlay más: se queda las teclas.
    asistente: Option<norte_frontend::wizard::Wizard>,
    /// La pantalla de arranque (spec 2026-09-15, ADR 0115), mientras está
    /// puesta. Una CAPA y no un overlay con teclas propias: cualquier tecla o
    /// clic la quita, y el asistente le gana.
    splash: Option<norte_frontend::splash::SplashView>,
    /// Cuándo deja de tapar el `brief`, en milisegundos de época. `None` = no
    /// caduca solo (`home`), o no hay pantalla puesta.
    splash_hasta_ms: Option<i64>,
    /// La pantalla de arranque ya se enseñó en ESTA sesión del host.
    ///
    /// El host sobrevive al webview —una recarga, un renderer que se
    /// reinicia—, y el renderer manda `splash_open` cada vez que arranca. Sin
    /// esta marca, recargar a media sesión tapaba lo que estabas mirando con
    /// una pantalla de bienvenida que en modo `home` se queda hasta que la
    /// toques.
    splash_visto: bool,
    /// El panel de procesos lo abrió el AUTOMÁTICO (`[ui] processes_panel`),
    /// así que el automático puede cerrarlo. Uno que abrió el lector se queda.
    procesos_auto: bool,
    /// La barra de progreso ligera del item `tasks` (ADR 0146).
    tira: norte_frontend::task_strip::TaskStrip,
    /// Las transferencias que se lancen van a la COLA (ADR 0149). De la
    /// sesión, no de la configuración: se enciende para un rato de mover
    /// cosas y se apaga después.
    encolar: bool,
    /// El origen del reloj de [`Self::tira`]: el de tokio, que los tests
    /// pueden pausar y adelantar.
    tira_base: tokio::time::Instant,
    /// Para cuándo hay ya un despertar programado, para no apilar uno por
    /// cada progreso.
    tira_despertar: Option<i64>,
    /// Las últimas claves lanzadas desde la paleta, la más reciente primero
    /// (spec 2026-09-10). Viven en la sesión de UI, como en el terminal.
    paleta_recientes: Vec<String>,
    /// Los directorios populares de la sesión (spec 2026-09-15 D6). Viven en
    /// la sesión de UI, como en el terminal.
    popular: norte_frontend::history::Popular,
    /// Los volúmenes del host, cacheados para el pie de cada listado (spec
    /// 2026-09-10). Se piden cuando un listado aterriza, nunca por foto:
    /// `host.volumes` monta y consulta espacio en cada filesystem.
    volumenes_pie: Vec<norte_proto::methods::Volume>,
    /// Hay una petición de [`Self::volumenes_pie`] en vuelo: no se apila otra.
    pie_en_vuelo: bool,
    /// Por qué menú se desplegó la última vez. Se reabre por ahí: empezar
    /// siempre por el primero obliga a recorrer la barra entera en cada
    /// gesto, y quien usa dos entradas del mismo menú lo paga cada vez.
    menu_ultimo: usize,
    /// El menú DESPLEGADO, si hay alguno.
    ///
    /// La barra se pinta siempre (o nunca, según `[ui] menu_bar`); esto es
    /// solo el desplegable. Mientras esté, las teclas son suyas — igual que
    /// la paleta, y por lo mismo: una flecha que se escapara movería el
    /// listado de debajo.
    menu: Option<norte_frontend::menu::MenuState>,
    /// La configuración con la que arrancó esta ventana, para enseñarla.
    config: norte_frontend::config::FrontendConfig,
    /// Dónde vive cada cosa.
    paths: crate::settings::HostPaths,
    /// Los ajustes, si están abiertos.
    ajustes: Option<crate::settings::Ajustes>,
    /// El gestor de extensiones, si está abierto.
    extensiones: Option<crate::extensions::Extensiones>,
    /// Todo lo de las sesiones de AGENTE: lo visto, si el panel está
    /// abierto, y qué deshacer corre por quién.
    agencia: Agencia,

    /// Cómo se llaman las extensiones y sus comandos, ya enmascarados, para
    /// el panel de salida: `id → (nombre, comando → título)`.
    ///
    /// Se compone con el catálogo que la PALETA pidió, y no con el del
    /// gestor: un comando se lanza desde la paleta con el gestor cerrado, y
    /// entonces no hay de dónde sacar un rótulo. Un panel que dice quién
    /// imprimió qué sin poder nombrar a ninguno de los dos no dice nada.
    rotulos_plugin: Rotulos,
    /// Lo que esta ventana tiene del ESCRITORIO: por dónde salen los efectos
    /// nativos y la salida del último comando de extensión.
    escritorio: Escritorio,
    /// La ventana tiene el foco del escritorio (#285).
    ///
    /// Arranca en `true` y no en `false`: un renderer que no mande
    /// `WindowFocus` se comporta como antes —avisa siempre— en vez de
    /// callarse. Perder un aviso es peor que repetirlo.
    enfocada: bool,
    /// Hay un selector de carpeta abierto, y esto dice si lo que se pidió era
    /// MOVER (#284). `None` = no se pidió ninguno.
    ///
    /// Solo el verbo: los operandos se recalculan cuando la respuesta vuelve.
    /// Congelarlos aquí prometería una operación sobre un listado que el
    /// lector pudo cambiar mientras el selector estaba delante.
    destino_pendiente: Option<bool>,

    /// El tema, tal como lo resolvió el arranque.
    tema: crate::pickers::HostTheme,
    /// El escritorio pide esquema OSCURO (`prefers-color-scheme`).
    ///
    /// Lo dice el renderer con [`UiAction::SetColorScheme`], al arrancar y
    /// cada vez que cambia. Elige contra qué variante de tema se resuelve el
    /// color de una entrada (puente 66): sin este dato, la ventana pintaba
    /// el cromo con la variante correcta y los NOMBRES con la otra.
    ///
    /// Arranca en `false` y no en «lo que diga el sistema» porque el host no
    /// tiene escritorio al que preguntar: lo corrige el primer mensaje del
    /// renderer, que llega antes de que se pinte nada.
    esquema_oscuro: bool,
    /// Se está mirando el tema por dentro.
    /// El selector de tema, si está abierto.
    tema_elegido: Option<profiles::SeleccionDeTema>,
    /// El PERFIL activo (ADR 0079), o ninguno.
    ///
    /// `OsString` porque es un nombre de directorio: pasarlo por texto cambia
    /// cuál se abre (#245).
    perfil_activo: Option<std::ffi::OsString>,
    /// El selector de perfiles, si está abierto.
    selector_perfil: Option<norte_frontend::profile_picker::ProfilePicker>,
    /// La generación de la lista de perfiles: sube cada vez que se relee.
    gen_perfiles: u64,
    /// Sube cada vez que cambia el conjunto de filas de la barra lateral.
    gen_sitios: u64,
    /// Sube cada vez que cambia el conjunto de filas del selector. Sirve
    /// también de id de APERTURA: el selector se abre vacío.
    gen_selector: u64,
    /// Cuántas veces se ha abierto el gestor de extensiones.
    gen_extensiones: u64,
    /// Cuántos catálogos se han PEDIDO, y cuál fue el último APLICADO.
    ///
    /// Aparte de la apertura: dos gobiernos seguidos piden dos catálogos con
    /// la misma apertura, y pueden contestar en cualquier orden. Sin esto, el
    /// viejo pisaba al nuevo y la columna «aprobada» se quedaba atrás sin que
    /// nada volviera a moverla.
    gen_catalogo: u64,
    /// El último catálogo aplicado, para descartar los que llegan tarde.
    catalogo_aplicado: u64,
    /// Cuántas veces se ha abierto la paleta.
    gen_paleta: u64,
    /// Cuántos comandos de extensión se han lanzado.
    gen_salida: u64,
    /// Los bytes de la imagen que el visor enseña, si ya llegaron.
    ///
    /// NO viajan en la foto: una imagen de ocho megas en el flujo de parches
    /// es un mensaje que se reenvía entero en cada `Resync` y que rompe la
    /// garantía de tamaño que `payload.rs` vigila. El renderer los pide
    /// aparte y hace un `blob:` con ellos (ADR 0069).
    imagen: Option<std::sync::Arc<Vec<u8>>>,
    /// La miniatura que un plugin dio del fichero del visor (ADR 0107): lo
    /// que la proyección anuncia como imagen y de quién es, mientras los
    /// bytes van en `imagen`. `None` = el visor pinta lo suyo.
    miniatura: Option<(crate::dto::ImageView, String)>,
    /// La búsqueda abierta, si la hay.
    busqueda: Option<search::Busqueda>,
    /// Cuántas búsquedas ha lanzado esta ventana. Es la identidad de la
    /// búsqueda mientras el daemon no ha dicho la suya.
    epoca_busqueda: u64,
    /// Las disposiciones del usuario, ya leídas por quien arrancó el host.
    disposiciones: Vec<norte_frontend::layout_picker::UserLayout>,
    /// El selector de disposiciones, si está abierto.
    selector_disposicion: Option<norte_frontend::layout_picker::LayoutPicker>,
    /// El selector de COLUMNAS, si está abierto.
    selector_columnas: Option<norte_frontend::columns_picker::ColumnsPicker>,
    /// La barra lateral de sitios, si la disposición coloca una. Hay UNA
    /// como mucho: dos listas idénticas de discos no son una disposición,
    /// son un fallo (lo dice el registro compartido, `multi: false`).
    sitios: Option<norte_frontend::places::PlacesState>,
    /// Lo que cada hueco de preview enseña (#291), por hueco: qué ruta, el
    /// visor con lo leído o la nota que lo sustituye, y lo que está en
    /// vuelo. Por hueco y no uno solo: el registro permite varios.
    previews: std::collections::BTreeMap<u32, preview::EstadoPreview>,
    /// Lo que cada panel de PLUGIN tiene vivo (fase 3), por hueco: su último
    /// marco, el estado opaco de su guest y lo que está en vuelo.
    ///
    /// El estado opaco es lo ÚNICO que sobrevive entre repintados —el permiso
    /// de leer se acuña por llamada—, así que se poda con el árbol: un
    /// `SlotId` se reutiliza, y sin podar el panel de otro plugin heredaría lo
    /// que guardó el primero.
    paneles: std::collections::BTreeMap<u32, panelplugin::EstadoPanel>,
    /// El mapa de disco de cada hueco que lo enseñe (fase 4).
    ///
    /// El estado es el COMPARTIDO (`norte_frontend::diskmap`), el mismo que
    /// usa el terminal: qué directorio describe, lo medido y cuál es el hijo
    /// elegido. Una decisión escrita dos veces diverge en silencio (ADR 0077).
    mapas: std::collections::BTreeMap<u32, diskmap::EstadoMapa>,
    /// La línea de tiempo de cada hueco que la tiene (#359).
    lineas: std::collections::BTreeMap<u32, timeline::EstadoLinea>,
    /// Lo ÚLTIMO que se mandó de cada hoja de atributos, por hueco.
    ///
    /// La hoja no pide nada y se calcula entera del listado, así que no tiene
    /// estado propio que guardar — pero sí hace falta saber qué vio el
    /// renderer, porque el cursor lo mueve cualquier mensaje y la hoja solo
    /// viaja en la foto entera. Sin esto, viajaba de gorra en la foto que
    /// provocaba el VISOR al cambiar de nota, y en una disposición con hoja y
    /// sin visor se quedaba congelada (lo que se veía: pinchar una fila no
    /// movía «Detalles»).
    hojas: std::collections::BTreeMap<u32, crate::dto::MetadataSlotView>,
    /// El árbol de directorios, si la disposición coloca uno. Hay UNO como
    /// mucho, por lo mismo que la barra de sitios.
    ramas: Option<norte_frontend::tree::Tree>,
    /// Sube cada vez que cambia el conjunto de ramas visibles.
    gen_ramas: u64,
    /// El fichero que hay que abrir en cuanto exista (#290), con la task que
    /// lo está creando.
    ///
    /// Uno como mucho: el gesto pide un nombre, y hasta que ese diálogo se
    /// contesta no hay otro.
    abrir_al_crear: Option<fileops::Creacion>,
    /// El cursor del panel de procesos.
    ///
    /// El MISMO tipo que usa la TUI, con su regla dentro: se acota al LEER y
    /// no al mover, porque las filas aparecen y desaparecen solas —una tarea
    /// termina y se barre a los diez segundos—, así que un cursor guardado
    /// siempre puede haberse quedado fuera. Aquí estaba escrito a mano en
    /// cinco sitios, que es la misma decisión duplicada que la ADR 0077
    /// existe para no tener.
    cursor_procesos: norte_frontend::processes::Processes,
    /// El estado del panel de registro: nivel, filtro y seguimiento (#326).
    ///
    /// El MISMO tipo que usa la TUI, con su regla de los dos niveles dentro
    /// (el que se captura y el que se enseña) y la de que bajar el segundo no
    /// deja de capturar. Duplicarlo aquí habría sido duplicar esas dos.
    log_panel: norte_frontend::logpanel::LogPanel,
    /// El anillo del que salen las líneas. `None` = este proceso no lo montó,
    /// y entonces el panel lo DICE en vez de enseñarse vacío como si no
    /// hubiera pasado nada.
    log_ring: Option<norte_config::logring::LogRing>,
    /// Cuántas filas de registro cabían en el último frame.
    ///
    /// La pone el renderer (`LogSetVisibleRange`), como la ventana del
    /// listado: adivinarla aquí es lo que en la TUI hizo que cada página se
    /// saltara dos líneas y la primera cuatro, y lo que ninguna de las dos
    /// ventanas enseñaba no se podía leer de ninguna manera.
    log_filas: usize,
    /// Sube en cada APERTURA del panel. Distingue el temporizador de esta
    /// apertura del de la anterior: abrir, cerrar y volver a abrir dejaría dos
    /// vivos sobre el mismo panel, y el viejo se rearmaría para siempre.
    log_epoca: u64,
    /// El contador de entradas del anillo la última vez que se pintó, para no
    /// mandar una foto por sondeo cuando no ha pasado nada.
    log_visto: u64,
    /// La mitad REMOTA del panel de registro: lo que el daemon lleva
    /// entregado de su anillo y lo que se sabe de él (#328).
    ///
    /// Junta y no seis campos sueltos: son un solo asunto —una fuente de
    /// líneas con su cursor, su estado y su petición en vuelo— y sueltos
    /// convertían a `Estado` en la clase de estructura que se describe con
    /// una lista de banderas.
    log_remoto: logpanel::RegistroRemoto,
    /// El selector abierto, si lo hay.
    selector: Option<crate::pickers::Selector>,
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
    /// El resolutor de la pantalla de DIÁLOGO.
    resolver_dialogo: Resolver,
    /// Si este frontend puede escribir.
    efectos: crate::commands::Efectos,
    /// Cuántas líneas caben en el visor, según el renderer.
    ///
    /// `None` mientras no lo diga: se cae al tamaño en celdas menos el cromo,
    /// que es una estimación y se comporta como tal.
    visor_filas: Option<usize>,
    /// Celdas de ancho del CUERPO del visor, medidas por el renderer la
    /// última vez que lo pintó. `None` hasta entonces: la primera apertura
    /// usa el viewport, que se pasa por el cromo.
    visor_columnas: Option<u32>,
    /// El testigo de la lectura del visor en vuelo, si la hay.
    ///
    /// Sin él, una lectura lenta abría el visor DESPUÉS de que el usuario lo
    /// cerrara o se fuera a otro sitio — y como las teclas se enrutan por
    /// «hay visor», la siguiente tecla la interpretaba otro mapa sin que
    /// nadie hubiera pedido nada.
    visor_en_vuelo: Option<RequestToken>,
    /// El testigo del visor que está ABIERTO, no del que se está pidiendo.
    ///
    /// Separado de `visor_en_vuelo`, que se limpia al abrirse: los bytes de
    /// la imagen llegan DESPUÉS, y sin esto no habría con qué comprobar que
    /// son de este visor y no del anterior.
    visor_token: Option<RequestToken>,
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
    /// La última barra de paneles que cruzó el puente. `parche` la compara
    /// con la de ahora y manda la nueva si difiere: es lo que hace que la
    /// barra se actualice por cualquier camino sin que cada camino lo sepa.
    ultima_barra: Option<crate::dto::PanelBarView>,
    /// Los últimos elementos de la barra de estado que cruzaron (ADR 0132),
    /// por lo mismo que la barra de paneles.
    ultimos_elementos: Option<Vec<crate::dto::StatusItemView>>,
    /// La última línea fina que cruzó por hueco (ADR 0148), para mandar solo
    /// lo que cambia.
    ultima_linea: std::collections::HashMap<u32, Option<u8>>,
    /// El último ajuste de columnas que cruzó, por hueco
    /// (`norte_frontend::columns::fitted_columns`). Depende del ancho del
    /// hueco y de los nombres de su listado, y los dos cambian por caminos
    /// que no mandan cabecera; `parche` lo compara y, si difiere, manda la
    /// cabecera Y las filas juntas — una fila con una celda que su cabecera
    /// ya no tiene se pintaría sin ancho.
    ultimo_ajuste: std::collections::HashMap<u32, Vec<norte_frontend::columns::Fitted>>,
    /// Cuántos tics de un segundo lleva `status.message` en la barra (spec
    /// 2026-09-10): en TICS para que un test lo haga avanzar sin dormir.
    mensaje_ticks: u32,
    /// El texto que se estaba contando: si cambia, la cuenta vuelve a cero.
    mensaje_contado: Option<String>,
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
    /// El plan de renombrado en revisión, si lo hay.
    revision_ia: Option<ai::RevisionIa>,
    /// La época de la revisión: sube en cada PETICIÓN y al abandonar una en
    /// vuelo. Una respuesta con otra época llegó tarde y se descarta en Rust.
    epoca_ia: u64,
    /// Navegación SINCRONIZADA (`pane.sync-nav`): mientras está puesta, cada
    /// navegación del hueco activo la repite el hueco destino.
    ///
    /// Estado de ejecución y no configuración ni sesión: es un modo que se
    /// enciende para hacer una cosa y se apaga después, como en Krusader.
    espejo_permanente: bool,
    /// La petición de plan EN VUELO: su época y el DIRECTORIO para el que se
    /// pidió.
    ///
    /// El directorio viaja aquí y no se lee del hueco al aterrizar, porque
    /// entre pedir el plan y que llegue el lector puede haber navegado: un
    /// plan de `series/` abierto diciendo `descargas/` estaría prometiendo
    /// renombrar lo que se ve, y renombraría otra cosa.
    ia_en_vuelo: Option<(u64, VPath, Vec<Vec<u8>>)>,
    /// El plan de ORGANIZAR en revisión (fase 8), si lo hay.
    revision_organizar: Option<organize::RevisionOrganizar>,
    /// Su época: sube en cada petición, y una respuesta con otra llegó tarde.
    epoca_organizar: u64,
    /// La petición de plan de organizar EN VUELO: época, directorio y los
    /// nombres que había en él al pedir.
    ///
    /// Los nombres viajan aquí por lo mismo que el directorio: entre pedir el
    /// plan y que llegue, el lector puede haber navegado, y preguntarle al
    /// hueco entonces pintaría el árbol contra un directorio que no es el
    /// suyo — diciendo «nueva» de una carpeta que sí existía, o al revés.
    organizar_en_vuelo: Option<(u64, VPath, Vec<String>)>,
    /// El tablero: lo que está en marcha, por id de task.
    tasks: std::collections::BTreeMap<u64, tasks::TaskViva>,
    /// El lote de transferencias en curso, si lo hay (#271).
    lote: Option<tasks::Lote>,
    /// La sesión de UI: qué revisión se leyó, si esta ventana es su dueña, y
    /// si el esquema que hay guardado es de una versión que este host no
    /// entiende (ADR 0059).
    sesion: Sesion,
    /// El directorio que un humano ESCRIBIÓ al arrancar, si escribió alguno.
    ///
    /// Se guarda porque la sesión se lee después de montar los huecos y pisa
    /// el sitio de todos: sin esto, `norte-gui /usr/bin` acababa donde
    /// estuvieras ayer. Lo consume [`Self::leer_sesion`] y no vuelve a hacer
    /// falta — una intención de arranque vale una vez.
    dir_pedido: Option<VPath>,
    /// Viene de un RELEVO (`--attach`, fase 9): las marcas de la sesión se
    /// reclaman. Sin él se ignoran — un arranque no es un relevo.
    attach: bool,
    /// Esta ventana ha entregado la pantalla y espera a saber si la terminal
    /// se abrió (fase 9). Es lo único que autoriza un `HandoffFailed`: la
    /// acción la puede mandar cualquiera, y sin un relevo en curso no hay
    /// nada que recuperar ni que decir.
    relevo_en_curso: bool,
    /// El último LISTADO que tuvo el foco. Cuando el foco está en un panel
    /// que no es un listado —el árbol, los sitios—, es sobre él sobre el que
    /// actúan los comandos y a él navega el árbol ([`Self::activo`]). Sin
    /// esto, `activo` caía al listado de id más bajo, que puede ser el de la
    /// DERECHA: elegir una rama movía el panel que no tenía el foco.
    ultimo_listado: Option<u32>,
    status: StatusView,
    conexion: ConnectionView,
    /// Las sesiones de provider que viajan sin cifrar (#44), acotadas por el
    /// módulo compartido.
    degradadas: norte_frontend::banners::DegradedSet,
    /// Lo que el daemon dijo de sí mismo antes de irse: relevo o parada.
    /// `None` = no ha dicho nada, o ya volvió.
    aviso_de_daemon: Option<&'static str>,
    /// La comparación abierta, si la hay.
    comparacion: Option<sync::Comparacion>,
    /// El plan de sincronización abierto, si lo hay.
    sincronizacion: Option<sync::Sincronizacion>,
    /// El lote de sumas en vuelo, si lo hay (#311). A lo sumo UNO: el diálogo
    /// de resultados es uno, y lanzar otro releva al anterior.
    sumas: Option<tasks::SumasEnVuelo>,
    /// El lote de sumas ENCOLADO y todavía sin id (#311). `None` = ninguno.
    sumas_pendientes: Option<sums::SumasEncoladas>,
    /// Un plan PEDIDO cuya Task todavía no ha contestado.
    sync_pedida: Option<sync::SyncPedida>,
    /// La consulta semántica en vuelo, para poder ABORTARLA.
    ///
    /// Abortar no es solo dejar de escuchar: el SDK manda `rpc.cancel` al
    /// soltar la llamada, y al otro lado hay un embed y un barrido del índice
    /// que cuestan. Relanzar o cerrar la vista los para.
    semantica_en_vuelo: Option<tokio::task::JoinHandle<()>>,
    /// Cuántas veces se ha (re)establecido la conexión con el daemon.
    ///
    /// Los ids de task los reparte el SCHEDULER de un proceso y empiezan en 1
    /// en cada arranque, así que tras un relevo —que esta ventana ahora sabe
    /// que viene, `ConnEvent::GoingAway`— el daemon nuevo reparte los MISMOS
    /// ids. Sin distinguir la época, la task 3 nueva heredaba de la vieja
    /// que su informe ya se pidió (y no se pedía nunca), sus directorios
    /// afectados, y hasta su detalle. Las aprobaciones no tienen este
    /// problema porque el daemon siembra SUS ids con el reloj a propósito.
    epoca_conexion: u64,
    /// El motor RECHAZÓ una mutación por no poder abrir su journal.
    ///
    /// Persistente y no un mensaje: la regla dura 4 dice que sin registro no
    /// se muta, así que esto describe lo que le va a pasar a TODA la sesión,
    /// no a la operación que se acaba de intentar.
    ///
    /// **Quién lo enciende, exactamente**: `Error::JournalUnavailable` solo
    /// lo produce el motor EMBEBIDO (el journal perezoso del TUI y el CLI).
    /// Un daemon con ese problema no llega a arrancar, así que una ventana
    /// montada sobre un socket —el caso de hoy— no puede ver este aviso. Se
    /// proyecta igualmente porque el host no elige quién lo monta, y quien lo
    /// montara sobre el motor embebido tendría el mismo derecho a saberlo.
    ///
    /// Y lo que este aviso NO dice: un journal simplemente OCUPADO deja pasar
    /// la mutación sin registrarla, y eso no produce este error ni enciende
    /// esto. El aviso habla de un motor que REHÚSA, no de uno que no anota.
    journal_rehusado: bool,
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
    /// El cuerpo tal como se LEYÓ, para conservar lo ajeno.
    ///
    /// Escribir desde un `SessionBody::default()` tiraba todo lo que esta
    /// ventana no entiende —los huecos de otro frontend, y las disposiciones
    /// guardadas— en vez de conservarlo. La ola #229–#234 puso «conserva lo
    /// ajeno en un relevo» exactamente por esto, y esta ventana no lo hacía.
    leida: norte_frontend::session::SessionBody,
    /// De qué huecos SABÍA lo leído del disco.
    ///
    /// El veto de `[profile.start]` (ADR 0098). Aparte de [`Self::leida`] y no
    /// derivado de ella al vuelo porque son dos preguntas: aquélla es lo que
    /// hay que volver a escribir, y esto es lo que la sesión ya conocía —y no
    /// puede moverse cuando el proceso empieza a guardar lo suyo.
    conocidos: std::collections::BTreeSet<u32>,
    /// El sello de edad de cada hueco, tal como se ESCRIBIÓ la última vez.
    ///
    /// La política compartida sella los huecos que cambiaron al preparar el
    /// cuerpo, y el llamante tiene que recordar ese sello para la siguiente
    /// captura: sellar cada captura con «ahora» hacía que ningún cuerpo fuera
    /// igual al anterior, y el tic escribía cada segundo sin que nada hubiera
    /// cambiado. Es el mismo mapa que lleva el terminal (`session.touched`).
    touched: std::collections::BTreeMap<u32, u64>,
    /// El cuerpo de un `session.put` en vuelo, si lo hay: el tic siguiente
    /// no manda otro encima —dos escrituras cruzadas con la misma revisión
    /// son un conflicto seguro— y el apagado sabe qué se estaba escribiendo
    /// para decir si lo suyo llegó o no.
    en_vuelo: Option<std::sync::Arc<norte_frontend::session::SessionBody>>,
    /// El daemon rehusó el cuerpo por tamaño (#316): desde entonces se manda
    /// sin historial, que es lo que se degrada. Lo que había que salvar es
    /// dónde está el lector, y eso cabe.
    sin_historial: bool,
    /// Los huecos que este proceso ya sembró desde `[profile.start]`.
    ///
    /// Sembrar es de la PRIMERA vez. Sin esta cuenta, un lector sin sesión
    /// guardada volvía al directorio de arranque del perfil cada vez que
    /// entraba y salía de él: para él [`Self::conocidos`] está siempre vacío.
    sembrados: std::collections::BTreeSet<u32>,
}

impl Hueco {
    /// Un hueco recién nacido: sin listado, sin historial y CARGANDO.
    ///
    /// Uno solo, porque los tres sitios que lo construían —el arranque,
    /// estrenar un hueco al cambiar de disposición y el de prueba— tenían que
    /// coincidir campo a campo, y un campo nuevo que se olvide en uno de
    /// ellos es un hueco que se comporta distinto según por dónde naciera.
    ///
    /// `ocultos` es el estado INICIAL de `[ui] show_hidden` (#107). Lo trae
    /// quien construye porque es configuración, y el pane nace enseñándolo
    /// todo: sin esto, una ventana con `show_hidden = false` en su config
    /// arrancaba enseñando los dotfiles igual, y `pane.toggle-hidden` los
    /// apartaba «por primera vez» en cada arranque.
    fn vacio(
        dir: VPath,
        ocultos: bool,
        orden: norte_frontend::SortSpec,
        fila_de_subir: bool,
    ) -> Self {
        let esquema = dir.scheme().to_owned();
        let mut pane = PaneState::new(dir, Vec::new());
        pane.set_show_hidden(ocultos);
        pane.set_sort(orden);
        // `[ui] parent_entry`: la fila `..` nace con el hueco y no se le pone
        // después — un hueco que se estrena sin ella y la gana en el siguiente
        // listado enseñaría dos pantallas distintas para la misma config.
        pane.set_parent_row(fila_de_subir);
        Self {
            pane,
            caps: None,
            esquema_del_orden: esquema,
            historial: History::default(),
            primera_visible: 0,
            visibles: 64,
            en_vuelo: None,
            dir_pedido: None,
            marcas_a_restaurar: Vec::new(),
            cursor_a_restaurar: None,
            filas_por_publicar: false,
            drenando: None,
            sondeando: false,
            cancelar_sondeo: std::sync::Arc::default(),
            // Sin destino: un hueco recién nacido no va a ninguna parte, ya
            // está donde va a estar.
            estado: Estado::cargando_hacia(None, None),
            sondeados: std::collections::HashSet::new(),
            adornos: std::collections::HashMap::new(),
            celdas_plugin: std::collections::HashMap::new(),
            adornando: false,
            visita_pendiente: None,
            adornadas: std::collections::HashSet::new(),
            gen_adornos: 0,
        }
    }

    /// Olvida lo que los plugins dijeron: el listado es OTRO.
    ///
    /// Una insignia de `git status` de un directorio no puede sobrevivir a un
    /// `cd`: la ruta sería otra y no casaría, pero la MEMORIA de «ya se pidió»
    /// sí sobreviviría y dejaría el listado nuevo sin decorar para siempre.
    fn olvidar_adornos(&mut self) {
        self.adornos.clear();
        self.celdas_plugin.clear();
        self.adornadas.clear();
        self.gen_adornos += 1;
    }
}

impl Estado {
    /// Construye el estado a partir de las opciones de arranque.
    ///
    /// Toma las opciones ENTERAS y no ocho parámetros sueltos: son los
    /// mismos datos, y una lista de ocho posiciones es donde dos `Effective`
    /// del mismo tipo se intercambian sin que el compilador diga nada.
    /// Un hueco de listado por cada `browser` del árbol, todos en el mismo
    /// directorio: de dónde arranca cada uno es cosa de la sesión (y hasta
    /// que exista, arrancar los dos donde arrancó el host es lo honesto).
    ///
    /// `[ui] show_hidden` (#107) siembra el estado inicial de cada uno, igual
    /// que en el TUI: ausente = enseñarlo todo.
    fn huecos_iniciales(
        arbol: &Node,
        kinds: &KindRegistry,
        dir: &VPath,
        settings: &norte_frontend::config::FrontendConfig,
        columnas: &norte_frontend::columns::ColumnsSettings,
    ) -> std::collections::BTreeMap<u32, Hueco> {
        let ocultos = settings.common.ui_show_hidden.unwrap_or(true);
        let subir = settings.common.ui_parent_entry.unwrap_or(true);
        let orden = columnas.sort_for(dir.scheme());
        let mut huecos = std::collections::BTreeMap::new();
        for SlotId(id) in arbol.slot_ids() {
            if es_listado(arbol, SlotId(id), kinds) {
                huecos.insert(id, Hueco::vacio(dir.clone(), ocultos, orden.clone(), subir));
            }
        }
        huecos
    }

    /// El idioma negociado, para las etiquetas de las continuaciones.
    fn lang_de(locale: &str) -> norte_i18n::Lang {
        match locale {
            "es" => norte_i18n::Lang::Es,
            _ => norte_i18n::Lang::En,
        }
    }

    /// Los roles de arranque.
    ///
    /// El DESTINO lo resuelve la capa compartida, y NO se pone a mano.
    /// Ponerlo con `Roles::set` lo marcaba como EXPLÍCITO —o sea, «lo eligió
    /// una persona»— cuando no lo había elegido nadie, y entonces sobrevivía
    /// a que aparecieran más candidatos: con tres listados, el primero se
    /// quedaba el rol para siempre y copiar mandaba ahí sin que nadie lo
    /// hubiera dicho (ADR 0058 D7).
    fn roles_iniciales(
        arbol: &Node,
        reparto: &norte_frontend::layout::Resolved,
        kinds: &KindRegistry,
        activo: u32,
    ) -> Roles {
        let mut roles = Roles::con_active(SlotId(activo));
        roles.reconcile(arbol, reparto, kinds, SlotId(activo));
        roles
    }

    /// Largo porque es un LITERAL de estructura: un campo por línea, con el
    /// porqué de los que no son obvios. No hay nada que extraer que no sea
    /// mover campos a una función que los devuelva de uno en uno.
    #[expect(
        clippy::too_many_lines,
        reason = "constructor: un campo por línea con su porqué, nada que extraer"
    )]
    fn nuevo(instance: InstanceId, options: UiHostOptions) -> (Self, Arc<dyn HostBackend>) {
        let UiHostOptions {
            backend,
            initial_dir,
            initial_dir_pedido,
            attach,
            locale,
            keymap,
            keymap_viewer: keymap_visor,
            keymap_dialog,
            layout: arbol,
            viewport,
            columns: columnas,
            effects: efectos,
            settings,
            paths,
            theme,
            user_layouts,
            profile: perfil_de_arranque,
            log_ring,
        } = options;
        let dir = &initial_dir;
        let lang = Self::lang_de(&locale);
        let kinds = KindRegistry::builtin();
        let reparto = resolve(rect(viewport), &arbol, &kinds);
        let huecos = Self::huecos_iniciales(&arbol, &kinds, dir, &settings, &columnas);
        let activo = huecos.keys().copied().next().unwrap_or(1);
        let roles = Self::roles_iniciales(&arbol, &reparto, &kinds, activo);
        let estado = Self {
            rotulos_plugin: Rotulos::new(),
            instance,
            sequence: 0,
            token: 0,
            locale,
            paleta: None,
            ir_a: None,
            gen_ir_a: 0,
            ir_a_indice: None,
            asistente: None,
            splash: None,
            splash_hasta_ms: None,
            splash_visto: false,
            procesos_auto: false,
            tira: norte_frontend::task_strip::TaskStrip::default(),
            encolar: false,
            tira_base: tokio::time::Instant::now(),
            tira_despertar: None,
            paleta_recientes: Vec::new(),
            popular: norte_frontend::history::Popular::default(),
            volumenes_pie: Vec::new(),
            pie_en_vuelo: false,
            menu: None,
            ayuda: None,
            ajustes: None,
            extensiones: None,
            agencia: Agencia::default(),
            escritorio: Escritorio::default(),
            enfocada: true,
            destino_pendiente: None,
            tema: theme,
            esquema_oscuro: false,
            tema_elegido: None,
            menu_ultimo: 0,
            // Lo que `--profile` nombró ya está APLICADO en `settings`; lo que
            // falta es que el host lo sepa (#307).
            perfil_activo: perfil_de_arranque,
            selector_perfil: None,
            gen_perfiles: 0,
            cursor_procesos: norte_frontend::processes::Processes::default(),
            log_panel: norte_frontend::logpanel::LogPanel::default(),
            log_ring,
            // Uno hasta que el primer frame diga la verdad: nunca cero, para
            // que una página antes de pintar mueva algo en vez de nada.
            log_filas: 1,
            log_epoca: 0,
            log_visto: 0,
            log_remoto: logpanel::RegistroRemoto::default(),
            sitios: None,
            previews: std::collections::BTreeMap::new(),
            paneles: std::collections::BTreeMap::new(),
            mapas: std::collections::BTreeMap::new(),
            lineas: std::collections::BTreeMap::new(),
            hojas: std::collections::BTreeMap::new(),
            gen_sitios: 0,
            ramas: None,
            gen_ramas: 0,
            abrir_al_crear: None,
            gen_selector: 0,
            gen_extensiones: 0,
            gen_catalogo: 0,
            catalogo_aplicado: 0,
            gen_paleta: 0,
            gen_salida: 0,
            imagen: None,
            miniatura: None,
            busqueda: None,
            epoca_busqueda: 0,
            disposiciones: user_layouts,
            selector_disposicion: None,
            selector_columnas: None,
            selector: None,
            config: settings,
            paths,
            efectivo_visor: keymap_visor.clone(),
            efectivo: keymap.clone(),
            lang,
            whichkey: None,
            resolver: Resolver::new(keymap),
            resolver_visor: Resolver::new(keymap_visor),
            resolver_dialogo: Resolver::new(keymap_dialog),
            efectos,
            visor_filas: None,
            visor_columnas: None,
            visor_en_vuelo: None,
            visor_token: None,
            visor: None,
            arbol,
            kinds,
            ultima_barra: None,
            ultimos_elementos: None,
            ultima_linea: std::collections::HashMap::new(),
            ultimo_ajuste: std::collections::HashMap::new(),
            mensaje_ticks: 0,
            mensaje_contado: None,
            reparto,
            viewport,
            roles,
            columnas,
            catalogos: std::collections::HashMap::new(),
            huecos,
            dialogos: Vec::new(),
            siguiente_modal: 1,
            revision_ia: None,
            epoca_ia: 0,
            espejo_permanente: false,
            ia_en_vuelo: None,
            revision_organizar: None,
            epoca_organizar: 0,
            organizar_en_vuelo: None,
            tasks: std::collections::BTreeMap::new(),
            lote: None,
            sesion: Sesion {
                revision: 0,
                owner: false,
                futuro: false,
                // Una ventana suelta vuelve a preguntar por la propiedad
                // cada treinta ticks: la dueña puede cerrarse en cualquier
                // momento y entonces alguien tiene que recogerla.
                policy: norte_frontend::session::PushPolicy::new(30),
                leida: norte_frontend::session::SessionBody::default(),
                conocidos: std::collections::BTreeSet::new(),
                touched: std::collections::BTreeMap::new(),
                en_vuelo: None,
                sin_historial: false,
                sembrados: std::collections::BTreeSet::new(),
            },
            dir_pedido: initial_dir_pedido.then(|| initial_dir.clone()),
            attach,
            relevo_en_curso: false,
            ultimo_listado: None,
            status: StatusView::default(),
            conexion: ConnectionView::Connected,
            degradadas: norte_frontend::banners::DegradedSet::default(),
            epoca_conexion: 0,
            semantica_en_vuelo: None,
            comparacion: None,
            sincronizacion: None,
            sumas: None,
            sumas_pendientes: None,
            sync_pedida: None,
            aviso_de_daemon: None,
            journal_rehusado: false,
        };
        (estado, backend)
    }

    /// El hueco con el foco. Siempre hay uno: si el rol apunta a un hueco
    /// que ya no existe, cae al primero que haya.
    fn activo(&self) -> u32 {
        let preferido = self.roles.get(RoleId::Active).map(|SlotId(id)| id);
        preferido
            .filter(|id| self.huecos.contains_key(id))
            .or_else(|| {
                self.ultimo_listado
                    .filter(|id| self.huecos.contains_key(id))
            })
            .or_else(|| self.huecos.keys().copied().next())
            .unwrap_or(1)
    }

    /// El hueco con el FOCO, sea del tipo que sea.
    ///
    /// No es lo mismo que [`Self::activo`], y confundirlos fue un bug: aquel
    /// contesta «el LISTADO sobre el que actúan los comandos» y se salta lo
    /// que no es un listado, que es justo lo que hace falta para que `F5`
    /// copie algo con la barra lateral enfocada. Este contesta dónde está el
    /// teclado, que es lo que decide quién recibe una tecla y qué hueco se
    /// pinta enfocado.
    fn enfocado(&self) -> u32 {
        self.roles
            .get(RoleId::Active)
            .map_or_else(|| self.activo(), |SlotId(id)| id)
    }

    /// El hueco ACTIVO, que siempre existe.
    ///
    /// El invariante (regla 6): `huecos` se siembra desde `arbol.slot_ids()`
    /// —en `Estado::nuevo` y en `aplicar_disposicion`, los dos únicos sitios
    /// que lo tocan— y `validate` rechaza un árbol sin `browser`, así que hay
    /// al menos uno. `activo()` sale de `Roles`, y `reconcilia_roles` corre
    /// tras cada cambio de reparto dejándolos apuntando a huecos que existen.
    ///
    /// El invariante fue FALSO hasta esta ola: se sembraba desde
    /// `reparto.placements`, que no incluye lo oculto, así que elegir una
    /// disposición que no coloca ningún listado vaciaba el mapa y la
    /// siguiente tecla panicaba dentro de la task del actor. Está clavado en
    /// `una_disposicion_que_esconde_el_listado_deja_el_hueco_vivo`.
    fn hueco(&self) -> &Hueco {
        let id = self.activo();
        self.huecos.get(&id).expect("el hueco activo existe")
    }

    /// El hueco activo, mutable. Mismo invariante que [`Self::hueco`].
    fn hueco_mut(&mut self) -> &mut Hueco {
        let id = self.activo();
        self.huecos.get_mut(&id).expect("el hueco activo existe")
    }

    /// Deja los roles apuntando a huecos que EXISTEN y se VEN.
    ///
    /// El foco se decide AQUÍ (solo se mueve si el hueco que lo tenía ya no
    /// vale) y el DESTINO lo decide la capa compartida
    /// ([`norte_frontend::layout::roles::Roles::reconcile`]), que es la que
    /// implementa la ADR 0058 D7. Este método tenía su propia regla —«el
    /// primer otro hueco visible»— y esa regla estaba mal por dos motivos
    /// que no se ven con dos paneles: con TRES adivinaba el de id más bajo, y
    /// pisaba un destino que una persona había designado a mano en cada
    /// cambio de foco. Desde que copiar y mover leen ese rol, adivinar es
    /// mandar ficheros a un sitio que nadie eligió. La regla compartida
    /// conserva lo explícito y, con varios candidatos y ninguno elegido,
    /// deja el rol SIN FIJAR: entonces la transferencia pide que se designe
    /// uno en vez de desempatar sola.
    fn reconcilia_roles(&mut self) {
        // El foco solo se MUEVE cuando el hueco que lo tenía ya no vale: se
        // ocultó, desapareció del reparto, o dejó de poder enfocarse. Pisarlo
        // siempre con «el primer listado» —que es lo que hacía— convertía el
        // tabulador en un interruptor entre dos paneles: caía en la barra
        // lateral o en el de procesos y volvía sola antes de que nadie lo
        // viera.
        let foco = self.roles.get(RoleId::Active).map(|SlotId(id)| id);
        let sirve = foco.is_some_and(|id| {
            !self.oculto(id)
                && self.reparto.placements.iter().any(|(s, _)| s.0 == id)
                && kind_de(&self.arbol, SlotId(id))
                    .and_then(|k| self.kinds.get(&k).map(|d| d.focusable))
                    .unwrap_or(false)
        });
        if !sirve {
            self.roles.set(RoleId::Active, SlotId(self.activo()));
        }
        let foco = SlotId(self.enfocado());
        self.roles
            .reconcile(&self.arbol, &self.reparto, &self.kinds, foco);
        // El foco en un LISTADO se recuerda: es a donde vuelven los comandos
        // y el árbol mientras el foco está en otro panel.
        if let Some(SlotId(id)) = self.roles.get(RoleId::Active)
            && self.huecos.contains_key(&id)
        {
            self.ultimo_listado = Some(id);
        }
    }

    /// ¿Está este hueco fuera del reparto de ESTE tamaño?
    ///
    /// Un hueco oculto —una pestaña de atrás, un panel que no cabe— no pide
    /// listados ni proyecta filas: lo que no se ve no se trae.
    fn oculto(&self, id: u32) -> bool {
        self.reparto.hidden.contains(&SlotId(id))
    }

    /// Pide el listado de los huecos que se VEN y todavía no lo han pedido.
    ///
    /// Un hueco oculto no se lista —lo que no se ve no se trae—, así que
    /// cuando el reparto lo saca a la luz hay que pedirlo ENTONCES. Nadie lo
    /// hacía: un `browser` oculto al arrancar que aparecía al agrandar la
    /// ventana se quedaba en `Loading` para siempre, con cero filas, y tras
    /// cambiar de disposición ni siquiera existía su `Hueco`, así que
    /// `snapshot()` caía al brazo por defecto y lo pintaba como
    /// `Unsupported { kind_name: "browser" }`.
    ///
    /// Idempotente por diseño: solo despierta lo que está en `Loading` SIN
    /// petición en vuelo, así que llamarlo en cada reparto no duplica nada.
    fn despertar_visibles(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let dormidos: Vec<u32> = self
            .huecos
            .iter()
            .filter(|(id, h)| {
                !self.oculto(**id)
                    && h.en_vuelo.is_none()
                    && matches!(h.estado, SlotState::Loading { .. })
            })
            .map(|(id, _)| *id)
            .collect();
        for id in dormidos {
            let dir = self.huecos[&id].pane.dir().clone();
            self.token += 1;
            let token = RequestToken(self.token);
            if let Some(h) = self.huecos.get_mut(&id) {
                h.en_vuelo = Some(token);
                h.drenando = Some(token);
            }
            self.pedir_listado(id, &dir, token, backend, buzon);
        }
    }

    /// Toma la primera página de un stream y deja el resto drenando hacia el
    /// actor.
    ///
    /// El resto llega por el MISMO buzón que todo lo demás, con el testigo de
    /// su petición: un lote de una navegación abandonada se descarta igual
    /// que su primera página.
    async fn primera_pagina(
        listado: Result<(norte_client::EntryStream, Option<u64>), Error>,
        slot: u32,
        token: RequestToken,
        buzon: mpsc::Sender<Mensaje>,
    ) -> Result<(Vec<Entry>, Option<u64>), Error> {
        use futures::StreamExt as _;
        let (mut stream, omitidas) = listado?;
        let mut primera = Vec::with_capacity(FIRST_PAGE);
        let mut agotado = false;
        while primera.len() < FIRST_PAGE {
            match stream.next().await {
                Some(Ok(e)) => primera.push(e),
                // Un error a mitad de página se cuenta como el error del
                // listado: media página no es un listado.
                Some(Err(e)) => return Err(e),
                None => {
                    agotado = true;
                    break;
                }
            }
        }
        // La tarea se lanza SIEMPRE, aunque el stream ya se haya agotado: su
        // último mensaje es lo que baja `drenando`, y sin él un listado que
        // cabe en una página dejaría el hueco marcado como «sigue llegando»
        // para el resto de la sesión. Y se lanza APARTE en vez de mandarlo
        // aquí porque este futuro lo espera el actor: mandar al buzón desde
        // dentro se bloquearía contra el único que lo vacía.
        tokio::spawn(async move {
            let mut lote = Vec::with_capacity(FILL_BATCH);
            if !agotado {
                while let Some(entrada) = stream.next().await {
                    let Ok(entrada) = entrada else {
                        // El resto se cortó. Lo que ya se pintó sigue siendo
                        // válido; callarlo es mejor que tirar el listado
                        // entero.
                        break;
                    };
                    lote.push(entrada);
                    if lote.len() >= FILL_BATCH {
                        let batch = std::mem::take(&mut lote);
                        if buzon
                            .send(Mensaje::MasEntradas(Box::new((token, slot, batch, false))))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        lote = Vec::with_capacity(FILL_BATCH);
                    }
                }
            }
            let _ = buzon
                .send(Mensaje::MasEntradas(Box::new((token, slot, lote, true))))
                .await;
        });
        Ok((primera, omitidas))
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
            // El espacio libre del pie (spec 2026-09-10): también en el
            // arranque, que no pasa por `aterrizar_listado`. Sin esto la
            // ventana abría sin «libres» hasta la primera navegación.
            self.pedir_volumenes_de_pie(backend_arc, buzon);
            // Lo mismo que hace el aterrizaje de una navegación, y que este
            // camino no hacía: el PRIMER directorio de un hueco se quedaba sin
            // capacidades hasta que el lector navegara a otro sitio. O sea que
            // la ventana que se acaba de abrir dentro de un contenedor
            // ofrecía escrituras que ese contenedor no acepta —y el plegado
            // del destino tampoco constaba (#268) mientras nadie se moviera.
            self.pedir_capacidades(id, backend_arc, buzon);
        }
    }

    /// Aplica el resultado de un listado sobre SU hueco. El orden y el
    /// cursor los decide `PaneState`, que es quien sabe qué hacer con la
    /// memoria del cursor y con un foco pendiente.
    fn aterriza_en(&mut self, id: u32, dir: VPath, res: Result<(Vec<Entry>, Option<u64>), Error>) {
        // #108: el orden de `[ui.columns]` es POR ESQUEMA, así que se
        // reaplica cuando el hueco cambia de esquema — no en cada `cd`, que
        // es lo que hace el TUI. Aquí la SESIÓN restaura el orden, y
        // reaplicarlo en el primer aterrizaje lo borraría antes de verse.
        let cambia_esquema = self
            .huecos
            .get(&id)
            .is_some_and(|h| h.esquema_del_orden != dir.scheme());
        let orden = cambia_esquema.then(|| self.columnas.sort_for(dir.scheme()));
        let Some(hueco) = self.huecos.get_mut(&id) else {
            return;
        };
        if cambia_esquema {
            dir.scheme().clone_into(&mut hueco.esquema_del_orden);
        }
        hueco.en_vuelo = None;
        hueco.dir_pedido = None;
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
        // Y lo mismo con lo que dijeron los plugins: la ruta de otro
        // directorio no casaría, pero la memoria de «ya se pidió» sí, y
        // dejaría el listado nuevo sin decorar para siempre.
        hueco.olvidar_adornos();
        hueco.adornando = false;
        match res {
            Ok((entradas, omitidas)) => {
                if let Some(spec) = orden {
                    hueco.pane.set_sort(spec);
                }
                hueco.pane.set_listing(dir, entradas);
                // Un refresco conserva la selección; un `cd` no tiene ninguna
                // que conservar y llega con la lista vacía. Lo que la
                // operación se llevó no se vuelve a marcar.
                let marcas = std::mem::take(&mut hueco.marcas_a_restaurar);
                hueco.pane.restore_marks(&marcas);
                // Y el cursor que dejó la sesión, también TRAS `set_listing`:
                // antes no hay filas y la fila 12 sería la 0. `set_cursor` lo
                // acota si el directorio tiene hoy menos entradas que entonces.
                if let Some(fila) = hueco.cursor_a_restaurar.take() {
                    hueco.pane.set_cursor(fila);
                }
                // TRAS `set_listing`, que la limpia: es un dato de ESTE
                // listado y arrastrar el del anterior sería decir que faltan
                // entradas de un directorio en el que faltaban de otro.
                hueco.pane.set_skipped(omitidas);
                hueco.primera_visible = 0;
                hueco.estado = SlotState::Ready;
            }
            Err(e) => {
                // Sin stream no hay drenaje que vaya a contestar, así que la
                // bandera la baja quien la levantó.
                hueco.drenando = None;
                hueco.pane.set_listing(dir, Vec::new());
                hueco.marcas_a_restaurar.clear();
                // Un listado fallido CONSUME el cursor guardado: si quedara
                // pendiente, caería sobre el siguiente listado que llegue, que
                // puede ser de otro sitio.
                hueco.cursor_a_restaurar = None;
                hueco.estado = SlotState::Error {
                    reason_key: norte_frontend::error::error_key(&e).to_owned(),
                    // CUÁL pide la contraseña. Sin esto, un arranque con dos
                    // paneles remotos decía «hace falta un secreto» dos veces
                    // y no había forma de saber a cuál contestar. El nombre
                    // sale de `connections.toml` —un fichero, no algo de
                    // fiar— así que se enmascara y se acota como todo lo que
                    // se pinta.
                    detail: match &e {
                        Error::SecretNeeded { conn, .. } => Some(clamp_display(
                            norte_frontend::display_name(conn.as_bytes()).0,
                        )),
                        _ => None,
                    },
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
    #[expect(
        clippy::too_many_lines,
        reason = "despachador exhaustivo: un brazo por acción y sin lógica dentro, \
                  como `ejecutar_pendiente`. Partirlo por la mitad solo movería \
                  la frontera a un sitio arbitrario"
    )]
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
                self.adornar(slot_id, backend, buzon);
                (self.aplicada(), vec![self.parche_filas_de(slot_id)])
            }
            UiAction::SortBy { slot_id, column } => self.ordenar_por(*slot_id, column),
            UiAction::ResizeColumn {
                slot_id,
                column,
                cells,
            } => self.redimensionar_columna(*slot_id, column, *cells, buzon),
            UiAction::FocusSlot { slot_id } => {
                let slot_id = *slot_id;
                // Enfocar algo que no se ve, o que no recibe foco, es una
                // carrera con un reparto anterior, no una orden.
                //
                // El criterio es el recorrido COMPARTIDO (`focus_order`), el
                // mismo que usa el tabulador: mientras esto exigía un
                // `browser`, un CLIC sobre el panel de procesos o sobre la
                // barra de sitios no los enfocaba —solo el tabulador podía—,
                // y el renderer manda exactamente esta acción al pulsar.
                if !self.reparto.focus_order.contains(&SlotId(slot_id)) || self.oculto(slot_id) {
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
            UiAction::Activate { .. }
            | UiAction::Parent { .. }
            | UiAction::BreadcrumbActivate { .. }
            | UiAction::History { .. } => self.navegacion(accion, backend, buzon),
            UiAction::SetViewport { width, height } => {
                self.viewport = (*width, *height);
                self.reparto = resolve(rect(self.viewport), &self.arbol, &self.kinds);
                // El destino no puede apuntar a algo que no se ve: una copia
                // que aterriza en un panel oculto es una copia que el usuario
                // no verá llegar.
                self.reconcilia_roles();
                // Agrandar la ventana saca huecos de `hidden`, y un hueco que
                // aparece sin listado se queda cargando para siempre.
                self.despertar_visibles(backend, buzon);
                self.responde_con_foto()
            }
            UiAction::SetColorScheme { dark } => {
                // Lo mismo, no es nada: el renderer la manda al arrancar y en
                // cada cambio, y repintar todas las filas por un mensaje que
                // no cambia nada es trabajo por nada.
                if self.esquema_oscuro == *dark {
                    return (self.aplicada(), Vec::new());
                }
                self.esquema_oscuro = *dark;
                // Solo las FILAS: las variables CSS de la variante las
                // enchufa el renderer por su cuenta, síncronamente, para no
                // parpadear. Lo que el host tiene que rehacer es lo que va
                // cocido en la fila (puente 66).
                (self.aplicada(), self.parches_de_filas_de_todos())
            }
            UiAction::Key(k) => self.tecla(k, backend, buzon),
            UiAction::SetViewerRows { rows } => self.fijar_filas_del_visor(*rows),
            UiAction::SetViewerCols { cols } => {
                self.visor_columnas = Some((*cols).clamp(1, u32::from(u16::MAX)));
                (self.aplicada(), Vec::new())
            }
            UiAction::AiRenameDecide { approve } => {
                self.decidir_revision_ia(*approve, backend, buzon)
            }
            UiAction::OrganizeDecide { approve } => {
                self.decidir_revision_organizar(*approve, backend, buzon)
            }
            UiAction::OrganizeScroll { down } => self.recorrer_organizar(*down),
            UiAction::HandoffFailed { no_terminal } => self.relevo_fallido(*no_terminal),
            UiAction::Resync => self.responde_con_foto(),
            UiAction::RequestQuit => self.pedir_salir(),
            UiAction::MenuOpen { menu } => self.desplegar_menu(*menu),
            UiAction::MenuPointRow { row } => self.apuntar_en_menu(*row),
            UiAction::MenuActivateRow { row } => self.activar_del_menu(*row, backend, buzon),
            UiAction::MenuClose => self.cerrar_menu(),
            UiAction::MenuToggle => self.alternar_menu(),
            UiAction::WizardOpen => self.abrir_asistente(),
            UiAction::SplashOpen => self.abrir_splash(),
            UiAction::SplashClose => (self.aplicada(), self.cerrar_splash()),
            UiAction::SplashActivateRow { number } => {
                self.activar_fila_de_splash(*number, backend, buzon)
            }
            UiAction::WizardActivateRow { row } => {
                self.activar_fila_de_asistente(*row, backend, buzon)
            }
            UiAction::PanelBarActivate { button } => {
                self.pulsar_barra_de_paneles(*button, backend, buzon)
            }
            UiAction::StatusItemActivate { id } => {
                self.pulsar_elemento_de_estado(id, backend, buzon)
            }
            UiAction::LayoutButtonActivate { id } => {
                self.pulsar_boton_de_disposicion(id, backend, buzon)
            }
            UiAction::TabAction { slot_id, verb } => {
                self.boton_de_pestana(*slot_id, *verb, backend, buzon)
            }
            UiAction::MoveSlot {
                slot_id,
                target,
                zone,
            } => self.mover_hueco(*slot_id, *target, *zone, backend, buzon),
            UiAction::ResizeSlot { slot_id, cells } => {
                self.arrastrar_borde(*slot_id, *cells, backend, buzon)
            }
            UiAction::ProfileActivateRow { row, generation } => {
                self.activar_perfil_de_fila(*row, *generation, backend, buzon)
            }
            UiAction::Dialog { id, choice, secret } => {
                self.responder_dialogo(*id, choice, secret.as_deref(), backend, buzon)
            }
            UiAction::RefreshSlot { slot_id } => {
                let cambios = self.refrescar(*slot_id, backend, buzon);
                if cambios.is_empty() {
                    // Ya tenía algo en vuelo: lo que va a aterrizar es más
                    // nuevo que este clic.
                    (self.aplicada(), Vec::new())
                } else {
                    (self.aplicada(), vec![self.parche(cambios)])
                }
            }
            UiAction::LogSetLevel { level } => self.nivel_de_registro(level, backend, buzon),
            UiAction::LogSetFilter { filter } => self.filtro_de_registro(filter),
            UiAction::LogScroll { delta } => self.desplazar_registro(*delta),
            UiAction::PanelClick { slot_id, row, col } => {
                // La MISMA acción para los dos, y se bifurca por el kind del
                // hueco: el renderer manda una celda y no sabe —ni tiene por
                // qué— si detrás hay un guest o un treemap. Lo que cambia es
                // quién resuelve y contra qué marco.
                if kind_de(&self.arbol, SlotId(*slot_id))
                    .is_some_and(|k| k.as_str() == diskmap::KIND)
                {
                    self.clic_en_mapa(*slot_id, *row, *col, backend, buzon)
                } else {
                    self.clic_en_panel(*slot_id, *row, *col, backend, buzon)
                }
            }
            UiAction::PreviewScroll { slot_id, delta } => self.desplazar_preview(*slot_id, *delta),
            UiAction::ViewerScroll { lines, cols } => self.desplazar_visor(*lines, *cols),
            UiAction::LogFollow => self.seguir_registro(),
            UiAction::LogCycleSource => self.fuente_de_registro(),
            UiAction::LogSetVisibleRange { rows } => self.filas_de_registro(*rows),
            UiAction::CancelTask { task_id } => self.cancelar(*task_id),
            UiAction::CompareSelectRow { .. }
            | UiAction::CompareActivateRow { .. }
            | UiAction::CompareToggleFilter { .. }
            | UiAction::CompareSetVisibleRange { .. } => {
                self.accion_de_comparacion(accion, backend, buzon)
            }
            // Un diálogo con campo de texto llega con la tarea que lo traiga
            // (crear directorio, renombrar). Decirlo es más honesto que
            // aceptar texto que nadie va a leer.
            UiAction::DialogInput { id, text } => self.escribir_en_dialogo(*id, text),
            // Y un diálogo-FORMULARIO (puente 91): dice CUÁL de sus campos se
            // tocó, que es lo que el de un solo campo no necesita decir.
            UiAction::DialogField { id, field, value } => {
                self.tocar_campo_de_dialogo(*id, field, value)
            }
            UiAction::DirectoryPicked { path } => {
                self.destino_elegido(path.clone(), backend, buzon)
            }
            UiAction::ProgramFinished {
                title_key,
                command,
                output,
                truncated,
                failed,
            } => self.programa_terminado(title_key, command, output, *truncated, *failed),
            UiAction::FilesDropped { paths } => self.soltados(paths, backend, buzon),
            UiAction::WindowFocus { focused } => {
                self.enfocada = *focused;
                (self.aplicada(), Vec::new())
            }
            otra => self.fila_por_indice(otra, backend, buzon),
        }
    }

    /// Las acciones que nombran una fila de un OVERLAY por su índice.
    ///
    /// Juntas y aparte porque comparten el mismo riesgo: el renderer pinta
    /// una lista y el usuario pulsa sobre la lista que TENÍA delante, no
    /// sobre la que el host tiene ahora. Las dos cuyo conjunto de filas puede
    /// cambiar solo —la barra lateral y el selector, que se llenan desde una
    /// tarea de fondo— llevan generación; las demás no pueden cambiar sin un
    /// gesto del usuario, y lo que sí hacen todas es RECHAZAR un índice fuera
    /// de rango en vez de recortarlo.
    fn fila_por_indice(
        &mut self,
        accion: &UiAction,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match accion {
            UiAction::HelpSelectTopic { row } => self.elegir_pagina(*row, backend, buzon),
            UiAction::SettingsSelectRow { row } => self.elegir_ajuste(*row),
            UiAction::SettingsActivate { row } => self.activar_ajuste_por_raton(*row, buzon),
            UiAction::SettingsQuery { text } => self.buscar_ajuste(text),
            UiAction::SettingsJumpSection { section } => self.saltar_a_seccion(section),
            UiAction::SettingsReset { row } => self.restablecer_ajuste(*row, buzon),
            UiAction::SettingsSet { id, value } => self.poner_ajuste(id, value, buzon),
            UiAction::ExtensionSelectRow { row } => self.elegir_extension(*row, backend, buzon),
            UiAction::ExtensionGovern { row, id, change } => {
                self.gobernar_por_raton(*row, id, (*change).into(), backend, buzon)
            }
            UiAction::ExtensionHelp { row, id } => {
                self.ayuda_de_extension(*row, id, backend, buzon)
            }
            UiAction::SelectTab { slot_id } => self.elegir_pestana(*slot_id, backend, buzon),
            UiAction::AgentSelectRow { row, generation } => self.elegir_agente(*row, *generation),
            UiAction::PickerSelectRow { row, generation } => {
                self.elegir_fila_del_selector(*row, *generation)
            }
            UiAction::PlaceActivateRow { row, generation } => {
                self.activar_sitio(*row, *generation, backend, buzon)
            }
            UiAction::TreeActivateRow { row, generation } => {
                self.tocar_rama(*row, *generation, true, backend, buzon)
            }
            UiAction::TreeToggleRow { row, generation } => {
                self.tocar_rama(*row, *generation, false, backend, buzon)
            }
            UiAction::LayoutActivateRow { row } => self.elegir_disposicion(*row, backend, buzon),
            UiAction::SearchActivateRow { row } => self.ir_al_resultado(*row, backend, buzon),
            UiAction::HelpActivate { index } => self.activar_en_ayuda(*index, backend, buzon),
            // El resto lo trató `aplicar`; llegar aquí sería un brazo que se
            // le olvidó, y contestar `Applied` a algo que no se hizo es peor
            // que decir que no se pudo.
            _ => (Self::obsoleta(StaleAction::Modal), Vec::new()),
        }
    }
}
