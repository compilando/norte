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
mod effects;
mod extensions;
mod fileops;
mod gestures;
mod help;
mod input;
mod layout;
mod lifecycle;
mod listing;
mod menu;
mod nav;
mod palette;
mod panel;
mod patches;
mod places;
mod profiles;
mod search;
mod selectors;
mod session;
mod settings;
mod sums;
mod sync;
mod tabs;
mod tasks;
mod transfer;
mod tree;
mod viewer;
mod views;

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
    /// A esta task TERMINADA se le acabó su rato en el tablero
    /// ([`TTL_TASK_TERMINAL`]). Lleva la ÉPOCA de conexión en la que se
    /// registró: tras un relevo del daemon los ids vuelven a empezar en 1, y
    /// caducar por número desalojaría a una task viva que solo comparte el
    /// número con la que se fue.
    TaskCaducada(u64, u64),
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
    /// Encolarla falló. El usuario tiene que enterarse: pidió un borrado.
    /// Cómo pliega nombres la ubicación de un hueco (#268).
    Pliegue(u32, VPath, norte_encoding::FoldMode),
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
    /// El secreto se entregó (o no), y con ello qué hacer con la navegación
    /// que `SecretNeeded` había suspendido (#327).
    SecretoEntregado(Box<(u32, VPath, Result<(), Error>)>),
    /// Un snapshot de progreso. Por la MISMA cola que todo lo demás, que es
    /// lo que garantiza que un estado terminal no se adelante ni se pierda.
    Progreso(Box<norte_proto::TaskProgress>),
    /// El informe de una Task que ya terminó y que TIENE informe.
    ///
    /// Lleva el `Result` entero y no un `Option`: «fue bien» y «el daemon no
    /// sabe informar» son dos cosas distintas, y colapsarlas es justo lo que
    /// estos informes existen para no hacer.
    Informe(Box<(u64, u64, Informe)>),
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
/// La respuesta de una tanda de decoración: qué hueco, qué directorio, las
/// insignias por ruta y las celdas de cada columna `plugin:`.
type Adornos = (
    u32,
    VPath,
    std::collections::HashMap<VPath, norte_frontend::Decoration>,
    std::collections::HashMap<String, std::collections::HashMap<VPath, String>>,
);

enum Fondo {
    /// El catálogo de plugins que pidió la AYUDA, para su lateral.
    PluginsDeAyuda(Result<norte_proto::methods::PluginListResult, Error>),
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
    /// La búsqueda de esta época ya tiene Task: este es su id.
    ///
    /// Llega por su cuenta y no dentro del primer lote porque puede no haber
    /// primer lote: el core no manda lotes vacíos.
    BusquedaViva(u64, norte_proto::TaskId),
    /// Los volúmenes, pedidos por la BARRA LATERAL.
    ///
    /// Aparte de los del selector por el mismo motivo que los dos catálogos
    /// de plugins: son dos superficies con dos vidas.
    SitiosVolumenes(Result<Vec<norte_proto::methods::Volume>, Error>),
    /// Los subdirectorios de una rama del ÁRBOL, ya filtrados y ordenados.
    ///
    /// Sin `Result`: una rama que no se deja leer llega VACÍA y se marca como
    /// leída, porque la alternativa es volver a pedirla en cada vuelta.
    RamasDeArbol(VPath, Vec<VPath>),
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
        // Los avisos de sesión en claro (#44) van por el mismo buzón, y se
        // toman SIEMPRE: no dependen de si esta ventana puede escribir. Que
        // un listado que se está LEYENDO viaje sin cifrar es un hecho para
        // quien lo mira, no un permiso.
        if let Some(mut degradadas) = backend.take_degraded() {
            let buzon = tx.clone();
            tokio::spawn(async move {
                while let Some(d) = degradadas.recv().await {
                    if buzon.send(Mensaje::Degradada(Box::new(d))).await.is_err() {
                        return;
                    }
                }
            });
        }
        // Y los fallos (#322), por el mismo buzón y con el mismo criterio: por
        // qué NO se pudo entrar en una máquina se le dice a quien lo intentó,
        // pueda esta ventana escribir o no.
        if let Some(mut fallidas) = backend.take_failed() {
            let buzon = tx.clone();
            tokio::spawn(async move {
                while let Some(f) = fallidas.recv().await {
                    if buzon.send(Mensaje::Fallida(Box::new(f))).await.is_err() {
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
                        .send(Mensaje::TaskNueva(Box::new((task, Vec::new(), None))))
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
#[allow(clippy::too_many_lines)]
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
                for u in estado.cambio_de_conexion(ev) {
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
            Mensaje::TaskNueva(task) => {
                let (task, afectados, reintento) = *task;
                for u in estado.registrar_task(task, afectados, reintento, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Pliegue(slot, dir, modo) => {
                estado.aplicar_pliegue(slot, &dir, modo);
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
                for u in estado.caducar_task(id, epoca) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::TemaPersistido(fallo) => {
                if let Some(clave) = fallo {
                    for u in estado.decir(clave) {
                        let _ = updates.send(u);
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
) -> std::collections::HashMap<String, std::collections::HashMap<VPath, String>> {
    let mut out = std::collections::HashMap::new();
    if pedidas.is_empty() {
        return out;
    }
    let Ok(lista) = backend.plugin_list().await else {
        return out;
    };
    for (plugin, columna) in
        norte_frontend::columns::validated_plugin_requests(pedidas, &lista.plugins)
    {
        if superado() {
            return out;
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
    out
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
) -> (String, bool) {
    use norte_frontend::columns::{ColumnId, header_label};
    let Ok(cid) = r.id.parse::<ColumnId>() else {
        // No parsea: el id crudo es lo único que se le puede enseñar, y es
        // texto de un fichero de configuración.
        return norte_frontend::display_name(r.id.as_bytes());
    };
    let estilo = columnas.style_for_id(esquema, &cid, None);
    norte_frontend::display_name(header_label(&cid, &estilo, None).as_bytes())
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
    pliegue: Option<norte_encoding::FoldMode>,
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
    /// Las rutas que ya se pidieron decorar (hayan contestado o no). Misma
    /// memoria que `sondeados` y por el mismo motivo: sin ella, un plugin
    /// que no decora nada se vuelve a preguntar en cada repintado.
    adornadas: std::collections::HashSet<VPath>,
}

/// Una búsqueda viva y lo que lleva encontrado.
struct Busqueda {
    /// Cuál de todas las búsquedas de esta ventana es.
    ///
    /// La identidad NO puede ser la Task: el id lo trae el daemon y llega
    /// tarde, así que hasta entonces no habría con qué distinguir un lote de
    /// la búsqueda anterior. La época se conoce al LANZAR, que es cuando hace
    /// falta.
    epoca: u64,
    /// La Task del daemon, en cuanto se sabe. Cero mientras no se sabe.
    task: norte_proto::TaskId,
    /// La vista se cerró y lo que quede de esta búsqueda sobra.
    ///
    /// La comparte con su reenviador, que es quien puede cancelar antes de
    /// que el id llegue al actor: `esc` justo tras lanzar es la ventana en la
    /// que nadie más tiene a quién cancelar.
    abandonada: Arc<std::sync::atomic::AtomicBool>,
    /// Lo que se buscó, para poder decirlo.
    query: String,
    /// Dónde se buscó.
    root: VPath,
    /// Lo encontrado, en el orden en que llegó.
    hits: Vec<Hallazgo>,
    /// Esta búsqueda es SEMÁNTICA: se preguntó por significado contra el
    /// índice, no por nombre contra el árbol.
    semantica: bool,
    /// Dónde está el cursor.
    cursor: usize,
    /// Sigue corriendo.
    viva: bool,
    /// El tope que se pidió: alcanzarlo significa que hay más.
    tope: u32,
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
            Self::Secreto => "",
        }
    }
}

/// Un fichero que se está creando para editarlo (#290).
#[derive(Debug)]
struct Creacion {
    /// La task que lo crea. `None` mientras se encola: el id no existe hasta
    /// que el daemon contesta, y el gesto ya ha vuelto.
    task: Option<u64>,
    /// Qué abrir cuando esa task termine BIEN.
    path: VPath,
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

/// El plan de renombrado que un modelo propuso, mientras se revisa.
///
/// Guarda las PAREJAS ya validadas y no el texto que contestó el daemon: la
/// validación es un cinturón fail-loud (`norte_frontend::validate_ai_plan`) y
/// una sola pareja que no sea un `Segment` legal tumba el lote entero, así
/// que lo que sobrevive hasta aquí ya es aplicable byte a byte.
struct RevisionIa {
    /// El directorio sobre el que se planeó.
    dir: VPath,
    /// Lo que el modelo propuso, tal como lo contestó.
    entradas: Vec<norte_proto::methods::AiRenameEntry>,
    /// Las mismas parejas en la forma que pide el core. Es lo que se manda a
    /// pedir el veredicto Y lo que se manda a ejecutar: los dos viajes llevan
    /// la MISMA intención, que es lo que hace que el `plan_hash` valga.
    parejas: Vec<norte_proto::methods::RenamePair>,
    /// El veredicto del core. Nace `Pending` —la revisión abre y se rellena—
    /// porque comprobarlo contra el directorio es otro viaje.
    plan: norte_frontend::BatchPlan,
    /// Primera pareja visible: la revisión es de todo el plan, por scroll.
    primera: usize,
    /// Hasta dónde ha LLEGADO el lector. Aprobar lo exige: la revisión es
    /// toda la defensa que hay contra un plan escrito a partir de nombres que
    /// controla quien escribe en el directorio, y con cinco parejas visibles
    /// de doscientas cincuenta y seis esa defensa cubría el 2 %.
    visto_hasta: usize,
    /// Esta revisión ya se ha ENSEÑADO al menos una vez, así que la siguiente
    /// tecla es una respuesta y no una tecla que iba a otro sitio.
    ///
    /// La pantalla se abre SOLA, del todo, decenas de segundos después del
    /// gesto que la pidió, y se queda el teclado. Sin esto, la `y` de quien
    /// estaba tecleando `yes.txt` en el filtro rápido aprobaba el renombrado
    /// del directorio entero.
    reconocida: bool,
    /// La época que la pidió.
    epoca: u64,
}

/// El informe de una Task terminada, por clase.
///
/// Dos clases lo tienen —un lote de renombrado y un undo— y las dos por el
/// mismo motivo: lo que quedó a medias no cabe en el desenlace de una Task.
enum Informe {
    /// El de un lote de renombrado (#272).
    Lote(Result<norte_proto::methods::FsRenameBatchReportResult, Error>),
    /// El de un undo de sesión.
    Undo(Result<norte_proto::methods::PolicyUndoReportResult, Error>),
    /// El de un empaquetado (#250). El único de los tres que cuenta algo de una
    /// Task que salió BIEN: el archivo se escribió entero y aun así puede
    /// llevar nombres que en otro sistema se colocan en otro sitio.
    Empaquetado(Result<norte_proto::methods::ArchivePackReportResult, Error>),
}

/// A qué task apunta un `task.cancel`.
///
/// Tres casos y no dos: «no hay ninguna» y «la señalada ya terminó» se leen
/// distinto, y colapsarlos haría que cancelar una task acabada dijera que no
/// hay tasks mientras el tablero enseña cuatro.
enum Objetivo {
    /// No hay ninguna a la que pedirle que pare.
    Ninguna,
    /// La señalada ya es terminal.
    Terminada,
    /// Esta.
    Viva(u64),
}

/// Un plan pedido cuya Task todavía no ha vuelto.
///
/// Existe por el id: el modelo compartido lo necesita AL NACER para poder
/// descartar lo que venga de otro plan.
struct SyncPedida {
    /// Cuál de todos los planes de esta ventana es.
    epoca: u64,
    /// Se abandonó antes de que la Task volviera.
    abandonada: Arc<std::sync::atomic::AtomicBool>,
    /// El modo pedido.
    modo: norte_proto::methods::SyncMode,
    /// Raíz origen.
    origen: VPath,
    /// Raíz destino.
    destino: VPath,
    /// Reinterpretación de nombres del ORIGEN, congelada al pedir.
    origen_encoding: Option<norte_encoding::NameEncoding>,
    /// La del DESTINO, que puede ser otra.
    destino_encoding: Option<norte_encoding::NameEncoding>,
}

/// Un plan de sincronización, con su modelo COMPARTIDO dentro.
///
/// El modelo es `norte_frontend::sync::SyncView`, el mismo que el TUI: qué
/// pasos hay, qué lo bloquea, si se puede aprobar y en qué estado va la Task.
/// Aquí no se decide ni un paso ni un veredicto; el plan lo produce el core y
/// solo él puede canjearlo.
/// Tope de lo que se lee de un fichero de sumas (#311): 1 MiB.
///
/// Por encima se RECHAZA en vez de comprobar media lista — el mismo criterio
/// que la terminal, y el mismo que el tope del otro extremo.
const SUMS_MAX_BYTES: u64 = 1024 * 1024;

/// Un lote de sumas en vuelo (#311).
///
/// La Task ya está en el tablero; lo que se espera aquí es su INFORME, que es
/// donde viajan los digests — no caben en el desenlace de una Task ni en su
/// progreso.
struct SumasEnVuelo {
    /// La Task cuyo informe se espera.
    task: norte_proto::TaskId,
    /// En qué época de CONEXIÓN vive esa Task: tras un relevo los ids del
    /// daemon vuelven a empezar en 1, y un informe de otra tarea con el mismo
    /// número contaría la comprobación de otra cosa.
    epoca_conexion: u64,
    /// Su informe ya se pidió: pedirlo es una RPC y una reconexión reanuncia
    /// el desenlace.
    informe_pedido: bool,
    /// Lo que el fichero de sumas publicaba, si esto es una COMPROBACIÓN.
    /// `None` = solo calcular.
    publicado: Option<Publicado>,
}

/// Un lote de sumas ENCOLADO y todavía sin id (#311).
///
/// Existe entre que el `checksum` se manda y el buzón devuelve la Task. Es un
/// tipo y no un `Option<Option<_>>` porque «no hay lote» y «hay uno que no
/// compara contra nada» son dos cosas distintas, y anidar dos opciones para
/// decirlo se lee mal en el sitio donde importa.
struct SumasEncoladas {
    /// Lo que el fichero de sumas publicaba, si esto es una comprobación.
    publicado: Option<Publicado>,
}

/// El fichero de sumas leído, tal como hace falta para juzgarlo (#311).
///
/// Gemelo del de la terminal, y con los mismos tres campos por el mismo
/// motivo: las líneas en su orden, dónde quedó cada una en la petición, y
/// cuántas no se entendieron —que es lo que prohíbe decir «todas correctas».
struct Publicado {
    lines: Vec<norte_frontend::checksums::SumLine>,
    asked: Vec<Option<usize>>,
    refused: usize,
}

struct Sincronizacion {
    /// Cuál de todos los planes de esta ventana es.
    epoca: u64,
    /// La Task del PLAN (la de aplicar es otra, y la guarda el modelo).
    task: norte_proto::TaskId,
    /// La vista se cerró y lo que quede sobra.
    abandonada: Arc<std::sync::atomic::AtomicBool>,
    /// El modelo compartido.
    vista: norte_frontend::sync::SyncView,
    /// La ventana que el renderer dice estar pintando.
    primera_visible: usize,
    /// Cuántos pasos caben en esa ventana.
    ventana: usize,
    /// Su informe ya se pidió: es una RPC, y una reconexión reanuncia el
    /// terminal.
    informe_pedido: bool,
    /// En qué época de CONEXIÓN vive su Task.
    ///
    /// Tras un relevo, el daemon nuevo reparte los ids desde 1: sin esto, una
    /// task ajena con el mismo número cerraba la historia de esta escritura
    /// con la prueba de otra.
    epoca_conexion: u64,
}

/// Una comparación de dos árboles, con su panel COMPARTIDO dentro.
///
/// El modelo —qué filas hay, qué categorías están escondidas, cuál está
/// seleccionada, de qué lado operan las teclas— es
/// `norte_frontend::compare::ComparePane`, el mismo que pinta el TUI. Aquí no
/// se vuelve a emparejar nada ni se decide ningún veredicto: eso lo hizo el
/// core, y reproducirlo en el host sería la tercera copia.
struct Comparacion {
    /// Cuál de todas las comparaciones de esta ventana es. Misma razón que la
    /// época de una búsqueda: el id de la Task llega tarde.
    epoca: u64,
    /// La Task del daemon, en cuanto se sabe. Cero mientras no se sabe.
    task: norte_proto::TaskId,
    /// La vista se cerró y lo que quede de esta comparación sobra.
    abandonada: Arc<std::sync::atomic::AtomicBool>,
    /// El MODELO compartido: raíces, filas, filtros, selección, lado activo
    /// y —lo que más importa— en qué estado quedó.
    ///
    /// Los cinco estados de `CompareState` son cómo un frontend dice si la
    /// respuesta está COMPLETA, y en una comparación eso ES la respuesta.
    /// Tener aquí un `bool viva` habría vuelto a perder el caso que ese enum
    /// existe para no perder: lotes que se cayeron por el camino.
    vista: norte_frontend::compare::CompareView,
    /// La ventana que el renderer dice estar pintando.
    primera_visible: usize,
    /// Cuántas filas caben en esa ventana.
    ventana: usize,
}

/// Un hallazgo de una búsqueda, venga de donde venga.
#[derive(Clone)]
struct Hallazgo {
    /// Dónde está.
    path: VPath,
    /// Qué es, si se sabe. `None` en un hallazgo SEMÁNTICO: el índice
    /// devuelve rutas y parecidos, no clases, y decir «fichero» porque suele
    /// serlo es inventarse la respuesta.
    kind: Option<EntryKind>,
    /// Cuánto se parece a lo que se preguntó, en `[-1, 1]`. `None` en una
    /// búsqueda por nombre: ahí no hay grados, o casa o no casa.
    score: Option<f64>,
}

/// Una task viva en el tablero.
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
}

/// El selector de tema abierto.
///
/// Mismo modelo que el del terminal: la lista, el cursor, y el que había
/// puesto al abrir — sin el último, `Escape` dejaría puesto lo que el cursor
/// rozó de paso, que es cambiar de tema sin querer.
struct SeleccionDeTema {
    /// Los presets, en el orden en que se declaran.
    nombres: Vec<String>,
    /// Cuál está señalado.
    cursor: usize,
    /// El que estaba puesto al abrir, ENTERO y no su nombre.
    ///
    /// Entero porque el que había puede no ser un preset —un fichero de tema
    /// del usuario lo es igual— y volver a resolverlo por nombre lo perdería.
    /// La lista solo ofrece presets; lo que se restaura es lo que había.
    previo: Box<crate::pickers::HostTheme>,
}

/// Qué de un perfil NO se puede aplicar sin reiniciar ESTA VENTANA.
///
/// Medido, no supuesto, y distinto de la lista del terminal — por eso no se
/// comparte. Aquí el tema SÍ se aplica (el catálogo vuelve a cruzar), y en
/// cambio las FUENTES no: viajan en el catálogo del arranque y la hoja de
/// estilos las lee una vez. `[ui] lang` tampoco: `norte_i18n::force` corre una
/// vez por proceso.
///
/// Un cambio que se callara esto sería un cambio que miente (ADR 0079, D8).
fn fuera_de_alcance_en_caliente(
    antes: &norte_config::CommonConfig,
    despues: &norte_config::CommonConfig,
) -> Vec<&'static str> {
    let mut fuera = Vec::new();
    if antes.ui_lang != despues.ui_lang {
        fuera.push("ui.lang");
    }
    if antes.ui_font != despues.ui_font
        || antes.ui_mono_font != despues.ui_mono_font
        || antes.ui_font_size != despues.ui_font_size
    {
        fuera.push("ui.font");
    }
    if antes.ui_reduce_motion != despues.ui_reduce_motion {
        fuera.push("ui.reduce_motion");
    }
    fuera
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

struct TaskViva {
    vista: TaskView,
    /// Cómo pedirle que pare. Cancelar dos veces no es un error.
    cancel: std::sync::Arc<dyn Fn() + Send + Sync>,
    /// Su informe ya se pidió. Lo llevan las clases que TIENEN informe —un
    /// lote de renombrado y un undo— y evita pedirlo dos veces si el daemon
    /// repite el último progreso (una reconexión reanuncia las tasks,
    /// terminales incluidas).
    informe_pedido: bool,
    /// En qué época de conexión se registró. Un id repetido de OTRA época es
    /// otra task, no la misma.
    epoca: u64,
    /// El progreso EN VIVO, para preguntarle si sigue corriendo.
    ///
    /// `vista` es una proyección que se actualiza cuando el `Mensaje::Progreso`
    /// sale del buzón, así que decidir sobre ella qué cancelar es decidir
    /// sobre una foto rancia: se decía «cancelando…» de algo ya terminado, y
    /// la elección de «la última viva» podía saltarse la que de verdad corre.
    /// El TUI pregunta al estado vivo por este mismo motivo.
    progreso: tokio::sync::watch::Receiver<norte_proto::TaskProgress>,
    /// Los directorios que esta task deja DISTINTOS.
    ///
    /// Se apuntan al encolar y no se deducen del progreso: el progreso dice
    /// qué fichero va por dentro, no qué pantallas mienten cuando termine.
    /// Vacío = nada que refrescar (una búsqueda, una task ajena de la que
    /// solo se conoce el id).
    afectados: Vec<VPath>,
    /// Con qué reintentar si CHOCA (#274). `None` en todo lo que no es una
    /// transferencia: un borrado o un undo no tienen otra política que ofrecer.
    reintento: Option<Reintento>,
}

/// La cuenta de UN lote de transferencias (#271).
///
/// Un lote grande contra un destino poblado produce muchas filas `Failed` —
/// `CollisionPolicy::Fail` es lo que se manda—, y el tablero las enseña una a
/// una hasta su tope. Lo que el lector necesita no es la fila 213: es «de
/// estas 500, 460 bien y 40 mal».
///
/// Y los rechazos al ENCOLAR tenían el problema gemelo: cada uno pintaba un
/// mensaje en la barra y el siguiente lo pisaba, así que de N rechazos
/// sobrevivía el último. Se cuentan en vez de decirse.
///
/// UNA sola frase, y al final: la mitad del lote no es una respuesta, es
/// ruido que se pisa a sí mismo. El lote se cierra cuando todo lo que se pidió
/// está resuelto — encolado o rechazado, y lo encolado, terminal.
#[derive(Debug, Default)]
struct Lote {
    /// Cuántas entradas se pidieron.
    total: usize,
    /// Cuántas llegaron a ser task.
    encoladas: usize,
    /// Cuántas rechazó el daemon al encolar.
    rechazadas: usize,
    /// Los ids de las que se encolaron, para reconocer su desenlace. Un id que
    /// no está aquí es de otra cosa (una búsqueda, un undo, otro cliente).
    ids: std::collections::BTreeSet<u64>,
    /// Desenlaces terminales BUENOS de las encoladas.
    hechas: usize,
    /// Desenlaces terminales malos: falló o se canceló.
    fallidas: usize,
}

impl Lote {
    /// Todo lo que se pidió está resuelto.
    fn cerrado(&self) -> bool {
        self.encoladas + self.rechazadas >= self.total
            && self.hechas + self.fallidas >= self.encoladas
    }
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
    /// Se está mirando el tema por dentro.
    /// El selector de tema, si está abierto.
    tema_elegido: Option<SeleccionDeTema>,
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
    /// La búsqueda abierta, si la hay.
    busqueda: Option<Busqueda>,
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
    abrir_al_crear: Option<Creacion>,
    /// El cursor del panel de procesos.
    ///
    /// Se acota al LEER y no al mover: las filas aparecen y desaparecen
    /// solas —una tarea termina y se barre—, así que un cursor guardado
    /// siempre puede haberse quedado fuera.
    cursor_procesos: usize,
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
    revision_ia: Option<RevisionIa>,
    /// La época de la revisión: sube en cada PETICIÓN y al abandonar una en
    /// vuelo. Una respuesta con otra época llegó tarde y se descarta en Rust.
    epoca_ia: u64,
    /// La petición de plan EN VUELO: su época y el DIRECTORIO para el que se
    /// pidió.
    ///
    /// El directorio viaja aquí y no se lee del hueco al aterrizar, porque
    /// entre pedir el plan y que llegue el lector puede haber navegado: un
    /// plan de `series/` abierto diciendo `descargas/` estaría prometiendo
    /// renombrar lo que se ve, y renombraría otra cosa.
    ia_en_vuelo: Option<(u64, VPath, Vec<Vec<u8>>)>,
    /// El tablero: lo que está en marcha, por id de task.
    tasks: std::collections::BTreeMap<u64, TaskViva>,
    /// El lote de transferencias en curso, si lo hay (#271).
    lote: Option<Lote>,
    /// La sesión de UI: qué revisión se leyó, si esta ventana es su dueña, y
    /// si el esquema que hay guardado es de una versión que este host no
    /// entiende (ADR 0059).
    sesion: Sesion,
    status: StatusView,
    conexion: ConnectionView,
    /// Las sesiones de provider que viajan sin cifrar (#44), acotadas por el
    /// módulo compartido.
    degradadas: norte_frontend::banners::DegradedSet,
    /// Lo que el daemon dijo de sí mismo antes de irse: relevo o parada.
    /// `None` = no ha dicho nada, o ya volvió.
    aviso_de_daemon: Option<&'static str>,
    /// La comparación abierta, si la hay.
    comparacion: Option<Comparacion>,
    /// El plan de sincronización abierto, si lo hay.
    sincronizacion: Option<Sincronizacion>,
    /// El lote de sumas en vuelo, si lo hay (#311). A lo sumo UNO: el diálogo
    /// de resultados es uno, y lanzar otro releva al anterior.
    sumas: Option<SumasEnVuelo>,
    /// El lote de sumas ENCOLADO y todavía sin id (#311). `None` = ninguno.
    sumas_pendientes: Option<SumasEncoladas>,
    /// Un plan PEDIDO cuya Task todavía no ha contestado.
    sync_pedida: Option<SyncPedida>,
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
            pliegue: None,
            esquema_del_orden: esquema,
            historial: History::default(),
            primera_visible: 0,
            visibles: 64,
            en_vuelo: None,
            dir_pedido: None,
            marcas_a_restaurar: Vec::new(),
            filas_por_publicar: false,
            drenando: None,
            sondeando: false,
            cancelar_sondeo: std::sync::Arc::default(),
            estado: SlotState::Loading,
            sondeados: std::collections::HashSet::new(),
            adornos: std::collections::HashMap::new(),
            celdas_plugin: std::collections::HashMap::new(),
            adornando: false,
            adornadas: std::collections::HashSet::new(),
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
                huecos.insert(id, Hueco::vacio(dir.clone(), ocultos, orden, subir));
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
    #[allow(clippy::too_many_lines)]
    fn nuevo(instance: InstanceId, options: UiHostOptions) -> (Self, Arc<dyn HostBackend>) {
        let UiHostOptions {
            backend,
            initial_dir,
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
            menu: None,
            ayuda: None,
            ajustes: None,
            extensiones: None,
            agencia: Agencia::default(),
            escritorio: Escritorio::default(),
            enfocada: true,
            destino_pendiente: None,
            tema: theme,
            tema_elegido: None,
            menu_ultimo: 0,
            // Lo que `--profile` nombró ya está APLICADO en `settings`; lo que
            // falta es que el host lo sepa (#307).
            perfil_activo: perfil_de_arranque,
            selector_perfil: None,
            gen_perfiles: 0,
            cursor_procesos: 0,
            sitios: None,
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
            visor_en_vuelo: None,
            visor_token: None,
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
            revision_ia: None,
            epoca_ia: 0,
            ia_en_vuelo: None,
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
            },
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
                !self.oculto(**id) && h.en_vuelo.is_none() && matches!(h.estado, SlotState::Loading)
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
                // Agrandar la ventana saca huecos de `hidden`, y un hueco que
                // aparece sin listado se queda cargando para siempre.
                self.despertar_visibles(backend, buzon);
                self.responde_con_foto()
            }
            UiAction::Key(k) => self.tecla(k, backend, buzon),
            UiAction::SetViewerRows { rows } => self.fijar_filas_del_visor(*rows),
            UiAction::AiRenameDecide { approve } => {
                self.decidir_revision_ia(*approve, backend, buzon)
            }
            UiAction::Resync => self.responde_con_foto(),
            UiAction::MenuOpen { menu } => self.desplegar_menu(*menu),
            UiAction::MenuPointRow { row } => self.apuntar_en_menu(*row),
            UiAction::MenuActivateRow { row } => self.activar_del_menu(*row, backend, buzon),
            UiAction::MenuClose => self.cerrar_menu(),
            UiAction::ResizeSlot { slot_id, cells } => {
                self.arrastrar_borde(*slot_id, *cells, backend, buzon)
            }
            UiAction::ProfileActivateRow { row, generation } => {
                self.activar_perfil_de_fila(*row, *generation, backend, buzon)
            }
            UiAction::Dialog { id, choice, secret } => {
                self.responder_dialogo(*id, choice, secret.as_deref(), backend, buzon)
            }
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
            UiAction::DirectoryPicked { path } => self.destino_elegido(path.clone()),
            UiAction::FilesDropped { paths } => self.soltados(paths),
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
            UiAction::ExtensionSelectRow { row } => self.elegir_extension(*row, backend, buzon),
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
