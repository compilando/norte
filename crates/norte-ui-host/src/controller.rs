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
    /// Un snapshot de progreso. Por la MISMA cola que todo lo demás, que es
    /// lo que garantiza que un estado terminal no se adelante ni se pierda.
    Progreso(Box<norte_proto::TaskProgress>),
    /// El informe de una Task que ya terminó y que TIENE informe.
    ///
    /// Lleva el `Result` entero y no un `Option`: «fue bien» y «el daemon no
    /// sabe informar» son dos cosas distintas, y colapsarlas es justo lo que
    /// estos informes existen para no hacer.
    Informe(Box<(u64, u64, Informe)>),
    Apagar(oneshot::Sender<ShutdownReport>),
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
                if let Some(u) = estado.aterrizar_listado(*datos, &backend, &buzon) {
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
            Mensaje::Informe(informe) => {
                let (epoca, task_id, cual) = *informe;
                for u in estado.informe(epoca, task_id, &cual) {
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
    input_crudo: String,
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

/// Lo que un diálogo tiene pendiente de hacer.
enum Pendiente {
    /// Borrar estas entradas, a la papelera o permanentemente.
    Borrar {
        /// Qué se borra, en orden de listado.
        paths: Vec<VPath>,
        /// Permanente (sin papelera): el diálogo lo AVISA.
        permanente: bool,
    },
    /// Preguntar al índice por SIGNIFICADO. Lo que se teclea es la consulta,
    /// y no lleva más operandos: el alcance es el índice entero.
    ConsultaSemantica,
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
    mirando_tema: bool,
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
    fn vacio(dir: VPath, ocultos: bool, orden: norte_frontend::SortSpec) -> Self {
        let esquema = dir.scheme().to_owned();
        let mut pane = PaneState::new(dir, Vec::new());
        pane.set_show_hidden(ocultos);
        pane.set_sort(orden);
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
        let orden = columnas.sort_for(dir.scheme());
        let mut huecos = std::collections::BTreeMap::new();
        for SlotId(id) in arbol.slot_ids() {
            if es_listado(arbol, SlotId(id), kinds) {
                huecos.insert(id, Hueco::vacio(dir.clone(), ocultos, orden));
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
            ayuda: None,
            ajustes: None,
            extensiones: None,
            agencia: Agencia::default(),
            escritorio: Escritorio::default(),
            destino_pendiente: None,
            tema: theme,
            mirando_tema: false,
            cursor_procesos: 0,
            sitios: None,
            gen_sitios: 0,
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
                let snap = self.snapshot();
                (
                    self.aplicada(),
                    vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
                )
            }
            UiAction::Key(k) => self.tecla(k, backend, buzon),
            UiAction::SetViewerRows { rows } => self.fijar_filas_del_visor(*rows),
            UiAction::AiRenameDecide { approve } => {
                self.decidir_revision_ia(*approve, backend, buzon)
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
            UiAction::LayoutActivateRow { row } => self.elegir_disposicion(*row, backend, buzon),
            UiAction::SearchActivateRow { row } => self.ir_al_resultado(*row, backend, buzon),
            UiAction::HelpActivate { index } => self.activar_en_ayuda(*index, backend, buzon),
            // El resto lo trató `aplicar`; llegar aquí sería un brazo que se
            // le olvidó, y contestar `Applied` a algo que no se hizo es peor
            // que decir que no se pudo.
            _ => (Self::obsoleta(StaleAction::Modal), Vec::new()),
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
    /// Las teclas de un diálogo: contestarlo o cancelarlo, y nada más.
    ///
    /// El TEXTO no pasa por aquí. Lo teclea el campo del renderer y llega por
    /// `dialog_input`, que es lo que permite que los bytes aprobados sean los
    /// tecleados y no una reconstrucción a partir de teclas sueltas.
    ///
    /// `Enter` elige la primera respuesta NO destructiva, así que en el
    /// diálogo de aprobación de un agente elige `deny`: aprobar una mutación
    /// que uno no pidió no puede ser lo que pasa por dejar el dedo en Enter.
    ///
    /// Una tecla que no es ninguna de las dos se COME igual: un modal que
    /// deja pasar la tecla que no entiende no es un modal.
    fn tecla_en_dialogo(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(d) = self.dialogos.last() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let id = d.id;
        let tecleando = d.vista.input.is_some();
        // DOS REGÍMENES, el mismo par que el TUI y que el resto de campos de
        // este host. Con un campo abierto las teclas son LETRAS: resolverlas
        // por el keymap convertiría escribir un nombre de fichero en
        // contestar la pregunta, porque no hay verbo `dialog.*` para «teclea
        // una letra». Sin campo, la tecla pasa por el resolutor COMPARTIDO,
        // que es lo que hace que un preset que reata `dialog.confirm` cambie
        // esta ventana y no solo el TUI.
        let verbo = if tecleando {
            match k.key.as_str() {
                "Enter" | "enter" => Some("dialog.confirm"),
                "Escape" | "esc" => Some("dialog.cancel"),
                _ => None,
            }
        } else {
            let Ok(chord) = k.to_chord() else {
                return (self.aplicada(), Vec::new());
            };
            match self.resolver_dialogo.push(chord) {
                Resolution::Run { command, .. } => match command.as_str() {
                    "dialog.confirm" => Some("dialog.confirm"),
                    "dialog.cancel" => Some("dialog.cancel"),
                    "dialog.approve" => Some("dialog.approve"),
                    "dialog.deny" => Some("dialog.deny"),
                    _ => None,
                },
                _ => None,
            }
        };
        let Some(d) = self.dialogos.last() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // El VERBO elige entre las respuestas que ESTE diálogo ofrece: una
        // que no ofrece no se interpreta —no hay respuestas implícitas en una
        // superficie de decisión— y por eso `dialog.confirm` sobre una
        // aprobación no aprueba: la afirmativa de una aprobación se llama
        // `approve` a propósito, para que un renderer no las confunda.
        let elegido = match verbo {
            Some("dialog.confirm") => d.vista.choices.iter().find(|c| c.id == "confirm"),
            Some("dialog.approve") => d.vista.choices.iter().find(|c| c.id == "approve"),
            Some("dialog.deny") => d.vista.choices.iter().find(|c| c.id == "deny"),
            Some("dialog.cancel") => d
                .vista
                .choices
                .iter()
                .find(|c| c.id == "cancel" || c.id == "deny")
                // Cerrar SIEMPRE se puede: si el diálogo no ofrece cancelar
                // ni denegar, la respuesta es la última que no destruye.
                .or_else(|| d.vista.choices.iter().rfind(|c| !c.destructive)),
            _ => None,
        }
        .map(|c| c.id.clone());
        let Some(choice) = elegido else {
            return (self.aplicada(), Vec::new());
        };
        self.responder_dialogo(id, &choice, backend, buzon)
    }

    fn tecla_de_un_overlay(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        // El DIÁLOGO va antes que todo lo demás: es la única superficie
        // modal de verdad —una pregunta que hay que contestar antes de
        // seguir— y las teclas que no atrapaba caían al listado de DEBAJO.
        // Con el prompt de un nombre abierto, `Backspace` navegaba al padre
        // mientras se tecleaba y `Enter` entraba en el directorio bajo el
        // cursor en vez de confirmar; con una confirmación de borrado
        // abierta, `Enter` navegaba la pantalla que la pregunta tapaba.
        if !self.dialogos.is_empty() {
            return Some(self.tecla_en_dialogo(k, backend, buzon));
        }
        // La SALIDA de un comando de extensión se queda TODAS las teclas
        // mientras está: pinta a pantalla completa, así que un modal que
        // dejara pasar la que no entiende no es un modal. `Enter` y `Escape`
        // la cierran —las dos, porque cerrar un panel de lectura con `Enter`
        // es el reflejo—; el resto no significan nada aquí y no caen a lo de
        // debajo, donde una confirmación de borrado podía estar esperando un
        // sí que el lector no ve. El momento lo elige el PLUGIN, que decide
        // cuándo contesta su comando.
        if self.escritorio.salida.is_some() {
            if matches!(k.key.as_str(), "Escape" | "esc" | "Enter" | "enter") {
                return Some(self.cerrar_salida());
            }
            return Some((self.aplicada(), Vec::new()));
        }
        // La AYUDA va primero, incluso antes que el visor, y no por gusto:
        // se abre ENCIMA de lo que hubiera —también encima del visor, que es
        // desde donde se pide la página del visor— y quien está arriba se
        // queda las teclas. Al revés, `F1` en el visor abría una ayuda que no
        // recibía ni una tecla y que ninguna podía cerrar.
        if self.ayuda.is_some() {
            return Some(self.tecla_en_ayuda(k, backend, buzon));
        }
        // La REVISIÓN de un plan de renombrado va antes que el resto de
        // overlays y solo por detrás del diálogo y de la ayuda: es una
        // pantalla que se lee entera antes de aprobar una mutación, y una
        // tecla que se le escapara al listado de debajo movería el cursor
        // bajo un plan que sigue esperando un sí.
        // El panel de sincronización, igual que el de diferencias: mientras
        // esté abierto se queda las teclas.
        if self.sincronizacion.is_some() {
            return Some(self.tecla_en_sincronizacion(k, backend, buzon));
        }
        // El panel de diferencias, cuando está abierto, se queda las teclas:
        // es una pantalla entera, y una flecha que se le escapara movería el
        // listado que hay debajo.
        if self.comparacion.is_some() {
            return Some(self.tecla_en_comparacion(k, backend, buzon));
        }
        if self.revision_ia.is_some() {
            return Some(self.tecla_en_revision_ia(k, backend, buzon));
        }
        if self.busqueda.is_some() {
            return Some(self.tecla_en_busqueda(k, backend, buzon));
        }
        if self.selector_disposicion.is_some() {
            return Some(self.tecla_en_disposiciones(k, backend, buzon));
        }
        if self.selector_columnas.is_some() {
            return Some(self.tecla_en_columnas(k, backend, buzon));
        }
        if self.selector.is_some() {
            return Some(self.tecla_en_selector(k, backend, buzon));
        }
        if self.mirando_tema {
            return Some(self.tecla_en_tema(k));
        }
        if self.extensiones.is_some() {
            return Some(self.tecla_en_extensiones(k, backend, buzon));
        }
        if self.agencia.panel {
            return Some(self.tecla_en_agentes(k, backend, buzon));
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
        // Y, cuando NADIE más la quería, `Escape` abandona un plan de
        // renombrado que siga pensando. Va la ÚLTIMA, que es la única
        // posición en la que «no la quería nadie» es verdad: por encima se
        // comía el `Escape` que cierra la paleta y el que cancela el filtro
        // rápido —una tecla haciendo dos cosas mal a la vez— y se saltaba el
        // corte del visor en vuelo.
        //
        // Y solo `Escape`, no cualquier tecla como el visor: el modelo tarda
        // de verdad y seguir navegando mientras piensa es lo normal. Lo que
        // no puede pasar es que el plan se abra encima de la pantalla medio
        // minuto después de que su dueño se haya ido a otra cosa.
        if self.ia_en_vuelo.is_some() && (k.key == "Escape" || k.key == "esc") {
            self.epoca_ia += 1;
            self.ia_en_vuelo = None;
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "host-plan-abandoned",
            )));
            let cambio = ViewChange::Status(self.status.clone());
            return Some((self.aplicada(), vec![self.parche(vec![cambio])]));
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
                        // Una fila de PLUGIN no está en el catálogo de
                        // comandos y no puede estarlo: la aporta un tercero
                        // en tiempo de ejecución.
                        None if cmd.starts_with("plugin:") => {
                            self.ejecutar_de_plugin(&cmd, backend, buzon)
                        }
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

    /// Pregunta cómo pliega nombres el directorio de un hueco (#268).
    ///
    /// Se pide al ATERRIZAR y no delante de cada diálogo: hacerlo al copiar
    /// metería un viaje al daemon en el camino de F5, que es la tecla que más
    /// se pulsa de un gestor ortodoxo. Aquí va detrás de un listado que ya
    /// costó una ronda, y la respuesta sirve para todas las copias que salgan
    /// de ese directorio.
    ///
    /// Un fallo no dice nada y no rompe nada: sin respuesta no se pliega, que
    /// es exactamente lo que se hacía antes de #268.
    fn pedir_pliegue(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let Some(hueco) = self.huecos.get_mut(&slot) else {
            return;
        };
        let dir = hueco.pane.dir().clone();
        // El de antes ya no vale: es de otro sitio.
        hueco.pliegue = None;
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let Ok(caps) = backend.capabilities(dir.clone()).await else {
                return;
            };
            let modo = norte_vfs::fold_mode_of(caps);
            let _ = buzon.send(Mensaje::Pliegue(slot, dir, modo)).await;
        });
    }

    /// Guarda el modo de plegado, si el hueco sigue donde estaba.
    ///
    /// La comprobación del directorio no es paranoia: entre pedir y contestar
    /// cabe una navegación entera, y guardar el pliegue de otro sitio haría
    /// que la comprobación del lote mintiera en la dirección permisiva.
    fn aplicar_pliegue(&mut self, slot: u32, dir: &VPath, modo: norte_encoding::FoldMode) {
        if let Some(h) = self.huecos.get_mut(&slot)
            && h.pane.dir() == dir
        {
            h.pliegue = Some(modo);
        }
    }

    /// Un listado que se pidió antes acaba de volver.
    ///
    /// `None` = llegó TARDE y otra navegación lo relevó. Se descarta aquí y
    /// no se esconde en el renderer.
    fn aterrizar_listado(
        &mut self,
        datos: RespuestaListado,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let (token, slot, dir, res) = datos;
        if self.huecos.get(&slot).and_then(|h| h.en_vuelo) != Some(token) {
            return None;
        }
        self.aterriza_en(slot, dir, res);
        self.pedir_pliegue(slot, backend, buzon);
        self.sondear(slot, backend, buzon);
        self.adornar(slot, backend, buzon);
        // Los hechos de la ayuda describen la entrada bajo el CURSOR, y este
        // listado es otro (#262). La foto de abajo la lleva ya re-congelada,
        // así que aquí no se fabrica parche: gastaría un número de secuencia
        // que nadie recibiría.
        self.recongelar_hechos_de_ayuda();
        // Un `cd` cambia la pantalla entera —directorio, filas, cursor,
        // marcas—, así que se manda una foto en vez de enumerar parches que
        // el renderer tendría que casar.
        let snap = self.snapshot();
        Some(self.sobre(UiUpdate::Snapshot(Box::new(snap))))
    }

    /// Lo que un sondeo averiguó, aplicado; y se pide la siguiente tanda.
    ///
    /// `MAX_SONDEOS` acota cada VUELTA, no la ventana: sin volver a pedir,
    /// una ventana más alta que una tanda se quedaba a medias en silencio.
    fn aterrizar_sondas(
        &mut self,
        datos: Sondas,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let (dir, slot, sondas) = datos;
        let u = self.aplicar_sondas(slot, &dir, &sondas)?;
        self.sondear(slot, backend, buzon);
        self.adornar(slot, backend, buzon);
        Some(u)
    }

    /// Lo que los plugins dijeron, pegado al hueco que lo pidió.
    ///
    /// `None` si el hueco desapareció o si el listado es OTRO: pegar unas
    /// insignias de un directorio a las filas de otro es exactamente el
    /// fallo que la clave por RUTA evita, y aun así se comprueba el
    /// directorio — las rutas de dos directorios distintos no casan, pero
    /// gastar un parche entero para no pintar nada sí se puede evitar.
    fn aplicar_adornos(&mut self, datos: Adornos) -> Option<BridgeEnvelope<UiUpdate>> {
        let (slot, dir, adornos, celdas) = datos;
        let hueco = self.huecos.get_mut(&slot)?;
        hueco.adornando = false;
        if *hueco.pane.dir() != dir {
            return None;
        }
        if adornos.is_empty() && celdas.is_empty() {
            // Ningún decorador consentido y ninguna columna de plugin. No es
            // un fallo y no repinta nada.
            return None;
        }
        hueco.adornos.extend(adornos);
        for (columna, valores) in celdas {
            hueco
                .celdas_plugin
                .entry(columna)
                .or_default()
                .extend(valores);
        }
        // Y al pane, que es quien las sirve: sus setters REEMPLAZAN, así que
        // se le pasa el acumulado entero y no el lote.
        hueco.pane.set_decorations(hueco.adornos.clone());
        hueco.pane.set_plugin_columns(hueco.celdas_plugin.clone());
        // Las FILAS, que es lo único que cambia: una insignia no mueve el
        // cursor ni el directorio.
        Some(self.parche_filas())
    }

    /// Un lote más del listado que se está drenando por detrás.
    fn aterrizar_lote(
        &mut self,
        datos: (RequestToken, u32, Vec<Entry>, bool),
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let (token, slot, batch, ultimo) = datos;
        let Some(u) = self.aplicar_lote(slot, token, batch, ultimo) else {
            return Vec::new();
        };
        self.sondear(slot, backend, buzon);
        self.adornar(slot, backend, buzon);
        // El listado creció por debajo: si la ayuda está delante, sus hechos
        // hablan de otra entrada (#262). Aquí no hay foto que lo arrastre,
        // así que va su propio parche.
        let mut salida = vec![u];
        salida.extend(self.recongelar_ayuda());
        salida
    }

    /// El movimiento, cuando el foco está en un panel que no es un listado.
    ///
    /// `None` = el foco está en un listado, o el efecto no es un movimiento y
    /// sigue su camino normal. Quién toma teclas lo dice el registro
    /// COMPARTIDO de kinds (`takes_keys`), no una lista aquí: la hoja de
    /// atributos se enfoca y NO toma teclas a propósito —sigue al cursor del
    /// listado, así que con el teclado dentro dejaría de seguir a nada—, y
    /// esa decisión ya está tomada en un sitio.
    fn efecto_en_panel_enfocado(
        &mut self,
        efecto: Efecto,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        let SlotId(id) = self.roles.get(RoleId::Active)?;
        if self.huecos.contains_key(&id) {
            return None;
        }
        let kind = kind_de(&self.arbol, SlotId(id))?;
        if !self.kinds.get(&kind).is_some_and(|d| d.takes_keys) {
            return None;
        }
        if kind.as_str() == "places" {
            return self.efecto_en_sitios(efecto);
        }
        if kind.as_str() != "processes" {
            // Otro panel que toma teclas y que este host todavía no proyecta:
            // se deja pasar, y el listado sigue respondiendo. Cuando se
            // proyecte, su brazo entra aquí.
            return None;
        }
        let filas = self.filas_de_tablero();
        if filas == 0 {
            return Some((self.aplicada(), Vec::new()));
        }
        let total = i64::try_from(filas).unwrap_or(i64::MAX);
        let paso = |n: i64| -> i64 { n.clamp(-total, total) };
        let actual = i64::try_from(self.cursor_procesos.min(filas - 1)).unwrap_or(0);
        let destino = match efecto {
            Efecto::Cursor(n) => actual.saturating_add(paso(n)),
            // Una página del panel de procesos son sus filas: no hay ventana
            // declarada para él, y saltar más de lo que hay no significa nada.
            Efecto::Pagina(n) => actual.saturating_add(paso(n).saturating_mul(total)),
            Efecto::Extremo { al_final: false } => 0,
            Efecto::Extremo { al_final: true } => total - 1,
            _ => return None,
        };
        self.cursor_procesos = usize::try_from(destino.max(0)).unwrap_or(0).min(filas - 1);
        // Va como FOTO y no como parche: no hay un `ViewChange` para un hueco
        // que no es un listado, y añadir uno por un cursor de tres dígitos es
        // contrato nuevo para nada. Es una tecla, no un scroll continuo.
        let snap = self.snapshot();
        Some((
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        ))
    }

    /// Pide el listado de `dir` para `slot`, con el testigo ya reservado.
    ///
    /// Extraído de la navegación para que otra cosa que estrena huecos —un
    /// cambio de disposición— pida por el MISMO camino: dos formas de pedir
    /// un listado son dos sitios donde olvidarse del catálogo de atributos o
    /// del testigo.
    fn pedir_listado(
        &mut self,
        slot: u32,
        dir: &VPath,
        token: RequestToken,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        self.pedir_catalogo(dir, backend, buzon);
        // Queda apuntado A DÓNDE va: lo que el hueco enseña no cambia hasta
        // que esto aterrice, y hasta entonces `pane.dir()` responde por el
        // directorio que se abandona.
        if let Some(h) = self.huecos.get_mut(&slot) {
            h.dir_pedido = Some(dir.clone());
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let dir = dir.clone();
        let attrs = self.attrs_de(&dir);
        tokio::spawn(async move {
            let stream = backend.list(dir.clone(), attrs).await;
            let res = Estado::primera_pagina(stream, slot, token, buzon.clone()).await;
            // Si el actor ya no está, la respuesta no le importa a nadie.
            let _ = buzon
                .send(Mensaje::Listado(Box::new((token, slot, dir, res))))
                .await;
        });
    }

    /// Tope de resultados de UNA búsqueda.
    ///
    /// Acota el mensaje y la memoria del host: un árbol grande con un patrón
    /// laxo devuelve todo lo que hay. Alcanzarlo NO es un fallo —la Task
    /// completa— y se DICE, porque «100 resultados» y «los primeros 100 de
    /// no se sabe cuántos» son dos respuestas distintas.
    const MAX_RESULTADOS: u32 = 2000;

    /// Abre el prompt de buscar. Lo que se teclea es el patrón.
    /// Abre el prompt de un GLOB para marcar —o desmarcar— por patrón.
    ///
    /// Un prompt y no una tecla: el operando es un patrón que se teclea, y
    /// eso ya tiene forma en este host. Lo que se marca lo decide el modelo
    /// COMPARTIDO (`mark_glob`), que pliega el nombre antes de casar y sabe
    /// que un `*` sobre nombres enmascarados no puede significar «todos los
    /// que se pintan raro».
    fn pedir_patron(&mut self, marcar: bool) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        self.dialogos.push(Dialogo {
            id,
            reconocido: true,
            vista: DialogView {
                id,
                title_key: if marcar {
                    "modal-mark-pattern-title"
                } else {
                    "modal-unmark-pattern-title"
                }
                .to_owned(),
                destination: None,
                subject: None,
                asker: None,
                deadline: None,
                deadline_at_ms: None,
                body: Vec::new(),
                overflow_note: String::new(),
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
                input: Some(String::new()),
                input_hostile: false,
            },
            input_crudo: String::new(),
            al_confirmar: Some(Pendiente::Patron { marcar }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Aplica el patrón tecleado.
    fn aplicar_patron(
        &mut self,
        marcar: bool,
        patron: &str,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        if patron.is_empty() {
            // Un glob vacío no casa nada, y decirlo es mejor que no hacer
            // nada: quien pulsó cree que marcó.
            return (Some("err-empty-pattern"), self.decir("err-empty-pattern"));
        }
        match self.hueco_mut().pane.mark_glob(patron, marcar) {
            Ok(n) => {
                // La clave del TUI, que ya existía y dice «N marcas
                // cambiadas»: sirve para las dos direcciones, y una segunda
                // definición de la misma clave la tira Fluent en silencio —
                // la trampa que este repo ya se ha comido dos veces.
                let mut fuera = self.decir_con("msg-marked-by-pattern", &[("n", &n.to_string())]);
                fuera.push(self.parche_filas());
                (None, fuera)
            }
            // Un glob que no compila se DICE: es lo que el lector acaba de
            // teclear, y callar deja una tecla que no hizo nada.
            Err(_) => (Some("err-bad-pattern"), self.decir("err-bad-pattern")),
        }
    }

    fn pedir_busqueda(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let root = self.hueco().pane.dir().clone();
        let donde = Self::linea_de_ruta(&root);
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        self.dialogos.push(Dialogo {
            id,
            reconocido: true,
            vista: DialogView {
                id,
                title_key: "modal-search-title".to_owned(),
                destination: None,
                subject: None,
                asker: None,
                deadline: None,
                deadline_at_ms: None,
                body: vec![donde],
                overflow_note: String::new(),
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
                input: Some(String::new()),
                input_hostile: false,
            },
            input_crudo: String::new(),
            al_confirmar: Some(Pendiente::Buscar { root }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Lanza la búsqueda y engancha el canal por el que llegan sus lotes.
    ///
    /// El patrón va como GLOB de nombre, que es lo que un usuario teclea
    /// cuando busca `*.rs`. La búsqueda por CONTENIDO es otra cosa —otro
    /// campo, otro coste— y llega con su propia rebanada.
    fn lanzar_busqueda(
        &mut self,
        root: VPath,
        patron: String,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let params = norte_proto::methods::FsSearchParams {
            root: root.clone(),
            name_glob: Some(patron.clone()),
            name_regex: None,
            content: None,
            content_regex: None,
            case_sensitive: false,
            max_hits: Some(Self::MAX_RESULTADOS),
        };
        let backend = Arc::clone(backend);
        let buzon2 = buzon.clone();
        self.epoca_busqueda += 1;
        let epoca = self.epoca_busqueda;
        let abandonada = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let abandonada2 = Arc::clone(&abandonada);
        // La búsqueda se lanza y CONTESTA por el buzón, como todo lo demás:
        // el actor sigue atendiendo teclas mientras el daemon camina el árbol.
        tokio::spawn(async move {
            let (task, mut rx) = match backend.search(params).await {
                Ok(par) => par,
                Err(e) => {
                    let _ = buzon2.send(Mensaje::TaskFallida(Box::new(e))).await;
                    return;
                }
            };
            let id = task.id;
            let cancel = Arc::clone(&task.cancel);
            let _ = buzon2
                .send(Mensaje::TaskNueva(Box::new((task, Vec::new(), None))))
                .await;
            // Bautizada AQUÍ y no con el primer lote: puede no haber primer
            // lote —el core no manda lotes vacíos— y entonces la búsqueda se
            // quedaba sin nombre, sin poder terminar y sin poder cancelarse.
            let _ = buzon2
                .send(Mensaje::Fondo(Box::new(Fondo::BusquedaViva(epoca, id))))
                .await;
            // La vista pudo cerrarse mientras el daemon aceptaba la Task: en
            // esa ventana el actor no tiene a quién cancelar, así que cancela
            // quien sí lo tiene.
            if abandonada2.load(std::sync::atomic::Ordering::SeqCst) {
                cancel();
                return;
            }
            // La bomba vive lo que el canal: cuando el daemon lo cierra, la
            // búsqueda terminó y el progreso ya lo dijo por su lado.
            while let Some(lote) = rx.recv().await {
                if abandonada2.load(std::sync::atomic::Ordering::SeqCst) {
                    cancel();
                    return;
                }
                if buzon2
                    .send(Mensaje::Fondo(Box::new(Fondo::Resultados(
                        epoca,
                        Box::new(lote),
                    ))))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        // La vista se abre YA, vacía y diciendo que corre: esperar al primer
        // lote es una ventana que no reacciona a una tecla que sí hizo algo.
        self.busqueda = Some(Busqueda {
            semantica: false,
            epoca,
            // Todavía no se sabe: `Fondo::BusquedaViva` la trae. Cero jamás
            // es una Task real.
            task: norte_proto::TaskId::new(0),
            abandonada,
            query: patron,
            root,
            hits: Vec::new(),
            cursor: 0,
            viva: true,
            tope: Self::MAX_RESULTADOS,
        });
        let cambio = ViewChange::Search {
            search: self.vista_busqueda(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// Un lote de resultados.
    ///
    /// Casa por ÉPOCA, que se conoce al lanzar. Con el id de la Task no
    /// bastaba: hasta que llegaba, `b.task` era cero y el primer lote que
    /// apareciese bautizaba la búsqueda —incluido uno rezagado de la
    /// ANTERIOR, cuyo reenviador sigue vivo—, así que los hallazgos de un
    /// patrón llenaban la lista rotulada con otro.
    fn aplicar_resultados(
        &mut self,
        epoca: u64,
        lote: &norte_proto::methods::SearchHits,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let b = self.busqueda.as_mut()?;
        if b.epoca != epoca {
            return None;
        }
        let sitio = usize::try_from(b.tope).unwrap_or(usize::MAX);
        for e in &lote.entries {
            if b.hits.len() >= sitio {
                break;
            }
            b.hits.push(Hallazgo {
                path: e.path.clone(),
                kind: Some(e.kind),
                score: None,
            });
        }
        let cambio = ViewChange::Search {
            search: self.vista_busqueda(),
        };
        Some(self.parche(vec![cambio]))
    }

    /// La proyección de la búsqueda.
    fn vista_busqueda(&self) -> Option<crate::dto::SearchView> {
        let b = self.busqueda.as_ref()?;
        let (donde, root_hostil) = norte_frontend::path_display(&b.root);
        Some(crate::dto::SearchView {
            semantic: b.semantica,
            query: clamp_display(norte_frontend::display_name(b.query.as_bytes()).0),
            root: clamp_display(donde),
            root_hostile: root_hostil,
            rows: b
                .hits
                .iter()
                .map(|e| {
                    let nombre = e
                        .path
                        .file_name()
                        .map_or_else(Vec::new, |s| s.as_bytes().to_vec());
                    let (pintable, hostil) = norte_frontend::display_name(&nombre);
                    let (padre, padre_hostil) = e.path.parent().map_or_else(
                        || (String::new(), false),
                        |p| norte_frontend::path_display(&p),
                    );
                    crate::dto::SearchRowView {
                        name: clamp_display(pintable),
                        hostile: hostil,
                        parent: clamp_display(padre),
                        parent_hostile: padre_hostil,
                        is_dir: e.kind == Some(EntryKind::Dir),
                        score: e.score,
                    }
                })
                .collect(),
            // `then` y no `then_some`: el argumento de `then_some` se evalúa
            // SIEMPRE, y con cero hallazgos el `len() - 1` se desbordaba.
            cursor: (!b.hits.is_empty()).then(|| b.cursor.min(b.hits.len() - 1) as u64),
            status: clamp_display(Self::estado_de_busqueda(b, self.lang)),
            running: b.viva,
        })
    }

    /// La frase de estado de una búsqueda.
    ///
    /// Reutiliza la familia del TUI (`search-status-*`) en vez de inventar
    /// otra: es la misma información y no hay dos maneras de decirla.
    fn estado_de_busqueda(b: &Busqueda, lang: norte_i18n::Lang) -> String {
        let n = b.hits.len().to_string();
        let clave = if b.hits.len() >= usize::try_from(b.tope).unwrap_or(usize::MAX) {
            "search-status-truncated"
        } else if b.viva {
            "search-status-running"
        } else {
            "search-status-done"
        };
        norte_i18n::ta_in(lang, clave, &[("n", &n)])
    }

    /// Las teclas mientras la búsqueda está abierta.
    fn tecla_en_busqueda(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        /// Cuántas filas mueve una página.
        const PAGINA: usize = 10;
        let Some(b) = self.busqueda.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let ultimo = b.hits.len().saturating_sub(1);
        match k.key.as_str() {
            "Escape" | "esc" => {
                // Cerrar la búsqueda CANCELA la Task: seguir caminando un
                // árbol para nadie es gastar el daemon en un resultado que ya
                // no tiene dónde aparecer.
                b.abandonada
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                let task = b.task;
                self.busqueda = None;
                if task.get() != 0 {
                    self.cancelar(task.get());
                }
                // Y si era una consulta SEMÁNTICA, se aborta: no tiene Task
                // que cancelar —es una llamada directa— y lo que la para es
                // soltarla, que hace que el SDK mande `rpc.cancel`.
                if let Some(vuelo) = self.semantica_en_vuelo.take() {
                    vuelo.abort();
                }
            }
            "ArrowDown" | "down" => b.cursor = (b.cursor + 1).min(ultimo),
            "ArrowUp" | "up" => b.cursor = b.cursor.saturating_sub(1),
            "PageDown" | "pgdn" => b.cursor = (b.cursor + PAGINA).min(ultimo),
            "PageUp" | "pgup" => b.cursor = b.cursor.saturating_sub(PAGINA),
            "Home" | "home" => b.cursor = 0,
            "End" | "end" => b.cursor = ultimo,
            "Enter" | "enter" => {
                let fila = u32::try_from(b.cursor).unwrap_or(u32::MAX);
                return self.ir_al_resultado(fila, backend, buzon);
            }
            _ => return (self.aplicada(), Vec::new()),
        }
        let cambio = ViewChange::Search {
            search: self.vista_busqueda(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Va al resultado `fila`: el panel navega a su directorio y el cursor
    /// queda ENCIMA de él.
    ///
    /// Sin reconstruir ninguna ruta: la del hallazgo es la que mandó el
    /// daemon, y se le pasa entera al panel para que la case byte a byte
    /// cuando aterrice el listado. Un nombre pintado no vuelve a ser un path
    /// nunca — por ahí es por donde se acaba abriendo otro fichero.
    fn ir_al_resultado(
        &mut self,
        fila: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(b) = self.busqueda.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // Fuera de rango no se recorta: recortar navegaba al ÚLTIMO hallazgo
        // en vez de no hacer nada. Los hallazgos solo se añaden por el final,
        // así que un índice válido nombra siempre el mismo y esta lista no
        // necesita generación; uno que se pasa es que la lista se vació.
        let Some(hit) = b.hits.get(fila as usize).cloned() else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        b.cursor = fila as usize;
        // Un directorio se abre por dentro; un fichero, en su carpeta con el
        // cursor encima.
        // Sin clase —un hallazgo semántico— se trata como fichero: se abre
        // su carpeta con el cursor encima. Es lo conservador; entrar EN algo
        // que resulta no ser un directorio no lleva a ninguna parte.
        let (destino, foco) = if hit.kind == Some(EntryKind::Dir) {
            (hit.path.clone(), None)
        } else {
            match hit.path.parent() {
                Some(p) => (p, Some(hit.path.clone())),
                None => (hit.path.clone(), None),
            }
        };
        let task = b.task;
        self.busqueda = None;
        if task.get() != 0 {
            self.cancelar(task.get());
        }
        if let Some(child) = foco {
            self.hueco_mut().pane.set_pending_focus(child);
        }
        let cierre = self.parche(vec![ViewChange::Search { search: None }]);
        let mut envios = vec![cierre];
        envios.extend(self.navegar(&destino, Trail::Record, backend, buzon));
        (self.aplicada(), envios)
    }

    /// Abre el selector de disposiciones.
    ///
    /// Las del usuario ya vienen leídas del arranque: el selector pinta la
    /// FORMA de cada una, y leerlas al mover el cursor sería I/O en el bucle
    /// de eventos.
    fn abrir_disposiciones(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.selector_disposicion = Some(norte_frontend::layout_picker::LayoutPicker::open(
            self.disposiciones.clone(),
        ));
        let cambio = ViewChange::Layouts {
            layouts: self.vista_disposiciones(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Abre el selector de COLUMNAS sobre el esquema del hueco enfocado.
    ///
    /// Sobre SU esquema y no sobre el conjunto por defecto: las columnas se
    /// configuran por esquema (`sftp` no enseña lo mismo que `file`), y
    /// abrirlo sobre otro sería editar una pantalla distinta de la que se
    /// está mirando.
    fn abrir_columnas(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let hueco = self.hueco();
        let esquema = hueco.pane.dir().scheme().to_owned();
        let orden = hueco.pane.sort();
        let catalogo = self.catalogos.get(esquema.as_str()).cloned();
        self.selector_columnas = Some(
            norte_frontend::columns_picker::ColumnsPicker::open_with_catalog(
                &self.columnas,
                &esquema,
                orden,
                catalogo.as_ref(),
                // Sin catálogo de plugins todavía: el selector ofrece lo
                // CONFIGURADO más los attrs que el provider anuncia, y una
                // columna de plugin que nadie ha configurado no aparece
                // aún. Ofrecerlas pide cachear `plugin.list` en el host, que
                // hoy se pide por tanda de decoración y se tira.
                &[],
            ),
        );
        let cambio = ViewChange::ColumnsPicker {
            columns: self.vista_columnas(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// La proyección del selector de columnas.
    fn vista_columnas(&self) -> Option<crate::dto::ColumnsPickerView> {
        let p = self.selector_columnas.as_ref()?;
        let esquema = p.scheme().to_owned();
        Some(crate::dto::ColumnsPickerView {
            // El título ya trae el ALCANCE, con la clave que la TUI usa y
            // su `$target`: inventarme una segunda colisionaba —Fluent se
            // queda con la PRIMERA definición— y la mía habría quedado
            // muerta con el catálogo diciendo que estaba.
            title: clamp_display(norte_i18n::ta_in(
                self.lang,
                "columns-picker-title",
                &[(
                    "target",
                    &if p.scheme_override() {
                        esquema.clone()
                    } else {
                        norte_i18n::t_in(self.lang, "columns-picker-target-default")
                    },
                )],
            )),
            rows: p
                .rows()
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    // La etiqueta de un `attr:` o un `plugin:` la da su
                    // catálogo, o sea texto de TERCERO. La de un builtin la
                    // da Fluent y es nuestra.
                    let (label, hostil) = etiqueta_de_columna(r, &esquema, &self.columnas);
                    crate::dto::ColumnsPickerRowView {
                        // Identidad: entera o vacía, jamás recortada — es lo
                        // que vuelve para encender, apagar y mover.
                        id: identidad_de_texto(&r.id),
                        label: clamp_display(label),
                        hostile: hostil,
                        enabled: r.enabled,
                        format: r.format.clone().unwrap_or_default(),
                        format_locked: r.format_locked,
                        // La primera fila es el NOMBRE, que por contrato del
                        // render va primero y no se apaga.
                        fixed: i == 0,
                    }
                })
                .collect(),
            cursor: p.cursor() as u64,
            // Esta ventana todavía NO escribe configuración: lo elegido vale
            // para ella y se pierde al cerrarla. Callarlo dejaría al usuario
            // creyendo que acaba de configurar norte.
            note: clamp_display(norte_i18n::t_in(self.lang, "columns-picker-session-only")),
        })
    }

    /// Las teclas del selector de columnas.
    ///
    /// Las mismas que el overlay del TUI, por el mismo modelo: encender y
    /// apagar, subir y bajar la fila, elegir por qué se ordena y ciclar el
    /// formato de la que lo admita.
    fn tecla_en_columnas(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(p) = self.selector_columnas.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // Las teclas son las que el PIE de la ventana anuncia
        // (`columns-picker-hint-gui`), no otras: un pie que dice `Shift+↑/↓`
        // sobre un código que escucha `J`/`K` es una mentira que solo se
        // descubre probando. Con `shift`, la flecha MUEVE la fila en vez de
        // mover el cursor, que es la misma tecla haciendo lo esperable.
        match (k.key.as_str(), k.shift) {
            ("Escape" | "esc", _) => self.selector_columnas = None,
            ("ArrowDown" | "down", false) => p.down(),
            ("ArrowUp" | "up", false) => p.up(),
            ("ArrowDown" | "down", true) => p.move_down(),
            ("ArrowUp" | "up", true) => p.move_up(),
            (" " | "space", _) => p.toggle(),
            ("s" | "S", _) => p.sort_current(),
            ("f" | "F", _) => p.cycle_format(),
            ("Enter" | "enter", _) => return self.aplicar_columnas(backend, buzon),
            _ => return (self.aplicada(), Vec::new()),
        }
        let cambio = ViewChange::ColumnsPicker {
            columns: self.vista_columnas(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Aplica lo elegido A ESTA VENTANA.
    ///
    /// No escribe `norte.toml`: esta fase no muta nada del disco, y el
    /// selector lo DICE en su propia nota. Es el mismo trato que el selector
    /// de disposiciones.
    ///
    /// Si cambia el conjunto de columnas `attr:`/`plugin:` hay que RE-LISTAR:
    /// los valores de un attr solo llegan pidiéndolos en `fs.list`, así que
    /// una columna nueva sobre el listado viejo se quedaría en blanco —
    /// indistinguible de «este fichero no tiene ese atributo»— hasta el
    /// siguiente `cd`. La huella que lo decide es la COMPARTIDA
    /// (`pane_fingerprint`), no una cuenta de aquí.
    fn aplicar_columnas(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(p) = self.selector_columnas.as_ref() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let elegido = p.finish();
        self.selector_columnas = None;
        let antes = self.huellas_de_columnas();
        self.columnas
            .apply_picked(elegido.scheme_target.as_deref(), &elegido.ids, elegido.sort);
        for (id, fmt) in &elegido.formats {
            self.columnas.apply_format(id, fmt);
        }
        for id in self.huecos.keys().copied().collect::<Vec<_>>() {
            if antes.get(&id) != self.huellas_de_columnas().get(&id) {
                self.re_listar(id, backend, buzon);
            }
        }
        let snap = self.snapshot();
        (
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// Vuelve a pedir el listado de un hueco, sin moverse de sitio.
    ///
    /// Lo pide el cambio de columnas: los valores de un `attr:` solo llegan
    /// si se piden en `fs.list`, así que una columna nueva sobre el listado
    /// viejo se quedaría en blanco — indistinguible de «este fichero no
    /// tiene ese atributo».
    fn re_listar(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        // Un solo camino de recarga. Este tenía su propia copia y le faltaban
        // las dos cosas que hacen que recargar no se note: no anclaba el
        // cursor ni conservaba las marcas, así que cambiar de columnas
        // mandaba el cursor a la primera fila y borraba la selección.
        let _ = self.refrescar(slot, backend, buzon);
    }

    /// La huella de columnas de cada hueco: qué `attr:`/`plugin:` pinta.
    fn huellas_de_columnas(&self) -> std::collections::BTreeMap<u32, Vec<String>> {
        self.huecos
            .iter()
            .map(|(id, h)| (*id, self.columnas.pane_fingerprint(h.pane.dir().scheme())))
            .collect()
    }

    /// La proyección del selector de disposiciones, con su vista previa.
    ///
    /// La miniatura la pinta el MISMO motor que reparte la pantalla de
    /// verdad, así que no puede mentir sobre lo que va a salir.
    fn vista_disposiciones(&self) -> Option<crate::dto::LayoutPickerView> {
        /// Tamaño de la miniatura, en caracteres.
        const MINIATURA: (u16, u16) = (32, 12);

        let p = self.selector_disposicion.as_ref()?;
        let actual = p.current();
        // El diagnóstico del parser puede CITAR el fichero del usuario: entra
        // por la misma puerta que el resto, y con su bandera (#266) — lo que
        // se enmascara se dice.
        let diagnostico = actual.and_then(|r| r.problem.clone()).map_or_else(
            || (String::new(), false),
            |p| norte_frontend::display_name(p.as_bytes()),
        );
        Some(crate::dto::LayoutPickerView {
            title: clamp_display(norte_i18n::t_in(self.lang, "layout-picker-title")),
            rows: p
                .rows()
                .iter()
                .map(|r| {
                    // El nombre es un nombre de FICHERO: bytes (regla 1). Se
                    // pinta por la puerta compartida y NO viaja como clave —
                    // para elegir una fila se manda su índice.
                    let (pintable, hostil) =
                        norte_frontend::display::display_os_name(r.name.as_os_str());
                    crate::dto::LayoutRowView {
                        name: clamp_display(pintable),
                        hostile: hostil,
                        factory: r.factory,
                        shares_keymap_name: r.shares_keymap_name,
                        broken: r.tree.is_none(),
                    }
                })
                .collect(),
            cursor: p.cursor() as u64,
            preview: actual
                .and_then(|r| r.tree.as_ref())
                .map_or_else(Vec::new, |t| {
                    norte_frontend::layout_picker::preview(t, MINIATURA.0, MINIATURA.1, &self.kinds)
                }),
            problem: clamp_display(diagnostico.0),
            problem_hostile: diagnostico.1,
        })
    }

    /// Las teclas mientras el selector de disposiciones está abierto.
    fn tecla_en_disposiciones(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(p) = self.selector_disposicion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match k.key.as_str() {
            "Escape" | "esc" => self.selector_disposicion = None,
            "ArrowDown" | "down" => p.down(),
            "ArrowUp" | "up" => p.up(),
            "Enter" | "enter" => return self.aplicar_disposicion_elegida(backend, buzon),
            _ => return (self.aplicada(), Vec::new()),
        }
        let cambio = ViewChange::Layouts {
            layouts: self.vista_disposiciones(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Un click en una fila del selector: la elige Y la aplica.
    fn elegir_disposicion(
        &mut self,
        row: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(p) = self.selector_disposicion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // Fuera de rango NO se recorta: el `while` de abajo para en la
        // última fila, así que un índice viejo aplicaba LA ÚLTIMA disposición
        // de la lista —la operación más invasiva del host— en vez de no hacer
        // nada. Las tres acciones hermanas que solo señalan ya lo hacen así.
        if row as usize >= p.rows().len() {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        // El selector compartido no tiene un `set_cursor`: se camina hasta
        // la fila, que para una lista de cinco a diez es lo mismo y no le
        // añade superficie a un modelo que ya está probado.
        while p.cursor() > row as usize {
            p.up();
        }
        while p.cursor() < row as usize && p.cursor() + 1 < p.rows().len() {
            p.down();
        }
        self.aplicar_disposicion_elegida(backend, buzon)
    }

    /// Aplica la disposición del cursor.
    ///
    /// Una que no parsea NO se aplica y lo dice: la fila ya lleva su motivo,
    /// y cambiar la pantalla por un fichero roto sería peor que no hacer
    /// nada. Se aplica para ESTA ventana y no se escribe en la
    /// configuración: escribir es mutar, y llega con la fase 5.
    fn aplicar_disposicion_elegida(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(p) = self.selector_disposicion.as_ref() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let Some(arbol) = p.current().and_then(|r| r.tree.clone()) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-layout-broken".to_owned(),
                },
                Vec::new(),
            );
        };
        self.selector_disposicion = None;
        self.aplicar_disposicion(arbol, backend, buzon)
    }

    /// Cambia el ÁRBOL entero: otra disposición.
    ///
    /// Manda una FOTO y no un parche: cambia el reparto, qué huecos hay y qué
    /// hay dentro de cada uno. Los listados que la disposición nueva coloca y
    /// no existían arrancan en el directorio del que ya estaba, que es lo
    /// menos sorprendente: cambiar de forma de pantalla no es irse a otro
    /// sitio.
    fn aplicar_disposicion(
        &mut self,
        arbol: Node,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.aplicar_disposicion_con(arbol, None, backend, buzon)
    }

    /// Como [`Self::aplicar_disposicion`], diciendo qué hueco QUEDA activo.
    ///
    /// `None` = lo decide la reconciliación, que es lo que hace falta cuando
    /// el árbol viene de fuera. `Some` es para quien acaba de crear un hueco
    /// y quiere el foco ahí: en dos pasos serían dos fotos, y la primera
    /// enseñaría el foco donde ya no está.
    fn aplicar_disposicion_con(
        &mut self,
        arbol: Node,
        activo: Option<SlotId>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let dir = self.hueco().pane.dir().clone();
        // Un hueco que se estrena nace como los del arranque: con la
        // ocultación de la configuración puesta.
        let ocultos = self.config.common.ui_show_hidden.unwrap_or(true);
        self.arbol = arbol;
        self.reparto = resolve(rect(self.viewport), &self.arbol, &self.kinds);
        // Desde el ÁRBOL, no desde el reparto. `placements` y `hidden`
        // PARTICIONAN el árbol, así que sembrar desde `placements` borra el
        // hueco de un listado que el reparto no coloca —un `Tabs` cuyo activo
        // es otro kind, o un split todo-ponderado que no cabe—, y con él
        // puede irse el ÚLTIMO: `huecos` queda vacío y la siguiente tecla
        // muere en el `expect` de `hueco()`, dentro de la task del actor.
        // `validate` garantiza que el árbol TENGA un listado, no que el
        // reparto lo coloque, así que la garantía hay que tomarla del árbol.
        let nuevos: Vec<u32> = self
            .arbol
            .slot_ids()
            .into_iter()
            .map(|SlotId(id)| id)
            .filter(|id| es_listado(&self.arbol, SlotId(*id), &self.kinds))
            .collect();
        self.huecos.retain(|id, _| nuevos.contains(id));
        for id in nuevos {
            if let std::collections::btree_map::Entry::Vacant(hueco) = self.huecos.entry(id) {
                hueco.insert(Hueco::vacio(
                    dir.clone(),
                    ocultos,
                    self.columnas.sort_for(dir.scheme()),
                ));
            }
        }
        match activo {
            Some(id) => self.roles.set(RoleId::Active, id),
            None => self.roles.clear(RoleId::Active),
        }
        self.reconcilia_roles();
        self.despertar_visibles(backend, buzon);
        if self.hueco_de_sitios().is_some() {
            self.sembrar_sitios();
            self.pedir_sitios(backend, buzon);
        }
        let snap = self.snapshot();
        (
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// Abre la paleta de comandos.
    ///
    /// Las filas se construyen AQUÍ, al abrir, y se congelan: es lo que el
    /// modelo compartido espera (pliega el haystack de cada fila una vez, no
    /// por tecla).
    fn abrir_paleta(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.paleta = Some(norte_frontend::palette_state::Palette::new(
            self.filas_de_paleta(),
        ));
        self.pedir_filas_de_plugin(backend, buzon);
        let cambio = ViewChange::Palette {
            palette: self.vista_paleta(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Pide el catálogo para las filas de PLUGIN de la paleta.
    ///
    /// No se espera: la paleta se pinta ya con los comandos propios y las de
    /// plugin se unen cuando el daemon conteste. Congelar la ventana hasta
    /// entonces sería pagar el viaje aunque no haya ninguna extensión.
    fn pedir_filas_de_plugin(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        // En SOLO LECTURA no se piden: lo que hace un comando de plugin lo
        // decide el plugin, y esta ventana no lo va a lanzar. Es la misma
        // regla que `filas_de_paleta` ya aplica a los comandos propios —
        // ofrecer lo que se va a rehusar es prometer algo que no se hará.
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return;
        }
        self.gen_paleta += 1;
        let apertura = self.gen_paleta;
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(PLAZO_PLUGINS, backend.plugin_list()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::PluginsDePaleta(
                    apertura, res,
                ))))
                .await;
        });
    }

    /// Las filas de plugin llegaron: se UNEN a la paleta abierta.
    ///
    /// Conservando lo tecleado (`extend_rows`): reconstruirla perdería la
    /// query, y perder lo que alguien acaba de escribir por unas filas que
    /// llegan tarde es peor que no tenerlas.
    ///
    /// Un fallo NO tumba la paleta ni se anuncia: los comandos propios
    /// siguen ahí, que es el mismo criterio que el TUI («un daemon caído
    /// degrada la paleta, no la tumba»).
    fn aplicar_filas_de_plugin(
        &mut self,
        apertura: u64,
        res: Result<norte_proto::methods::PluginListResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if apertura != self.gen_paleta {
            return Vec::new();
        }
        let Ok(lista) = res else {
            return Vec::new();
        };
        let Some(p) = self.paleta.as_mut() else {
            return Vec::new();
        };
        // El mismo filtro y el mismo tope que el GESTOR aplica al catálogo:
        // un daemon hostil puede anunciar los plugins que quiera, y por aquí
        // cada uno además aporta una fila por comando. Sin el `is_valid_
        // plugin_id`, un id con `:` dentro rompe la clave que `plugin_rows`
        // compone y `parse_plugin_key` deshace, que es justo el contrato que
        // las dos comparten.
        let catalogo: Vec<_> = lista
            .plugins
            .iter()
            .filter(|p| norte_proto::methods::is_valid_plugin_id(&p.id))
            .take(crate::extensions::MAX_EXTENSIONES)
            .cloned()
            .collect();
        // El modelo COMPARTIDO decide qué se ofrece: solo aprobadas y
        // encendidas —la misma puerta que `plugin.run_command` exige por su
        // cuenta—, en orden de manifiesto, y con el prefijo que impide que
        // un comando de tercero se disfrace de uno propio.
        let mut filas = norte_frontend::palette::plugin_rows(&catalogo);
        // Y un tope de FILAS: el manifiesto no acota cuántos comandos declara
        // un plugin, así que uno aprobado con doscientos mil convertía cada
        // `ctrl+p` en un mensaje de cientos de megas.
        filas.truncate(crate::bridge::MAX_ROWS_PER_BATCH);
        if filas.is_empty() {
            return Vec::new();
        }
        p.extend_rows(filas);
        self.rotulos_plugin = catalogo
            .iter()
            .map(|p| {
                let comandos = p
                    .commands
                    .iter()
                    .map(|c| (c.id.clone(), crate::extensions::texto_de_tercero(&c.title)))
                    .collect();
                (
                    p.id.clone(),
                    (crate::extensions::texto_de_tercero(&p.name), comandos),
                )
            })
            .collect();
        let cambio = ViewChange::Palette {
            palette: self.vista_paleta(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// Ejecuta un comando de extensión y enseña su salida.
    ///
    /// La autorización es del SERVIDOR: `plugin.run_command` resuelve el
    /// comando contra el catálogo y exige aprobada + encendida por su
    /// cuenta. Lo que la fila comprueba de este lado es coherencia con lo
    /// que el lector está mirando, jamás el permiso.
    fn ejecutar_de_plugin(
        &mut self,
        clave: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some((id, comando)) = norte_frontend::palette::parse_plugin_key(clave) else {
            return self.no_implementado(clave);
        };
        if self.efectos == crate::commands::Efectos::SoloLectura {
            // Lo que hace un comando de plugin lo decide el plugin: puede
            // escribir. Una ventana sin efectos no lo lanza.
            return Self::no_muta();
        }
        self.gen_salida += 1;
        let apertura = self.gen_salida;
        // Los dos rótulos se resuelven AHORA, con el catálogo que la paleta
        // usó: la respuesta puede tardar, y buscarlos al volver es buscarlos
        // en un catálogo que ya no es el mismo.
        let (rotulo, titulo) = self.rotulos_de_comando(id, comando);
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        let (id2, comando2) = (id.to_owned(), comando.to_owned());
        let id3 = id2.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(
                PLAZO_COMANDO,
                backend2.plugin_run_command(id2, comando2, String::new()),
            )
            .await
            {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon2
                .send(Mensaje::Fondo(Box::new(Fondo::SalidaDeComando(
                    apertura,
                    Box::new(SalidaPedida {
                        id: id3,
                        plugin: rotulo,
                        comando: titulo,
                        res,
                    }),
                ))))
                .await;
        });
        (self.aplicada(), self.decir("host-plugin-running"))
    }

    /// Cómo se llaman, para el panel de salida: el nombre de la extensión y
    /// el título del comando, ya enmascarados. Si el gestor no está abierto
    /// se cae al id, que es lo único que este proceso asigna.
    fn rotulos_de_comando(
        &self,
        id: &str,
        comando: &str,
    ) -> (crate::extensions::Texto, crate::extensions::Texto) {
        let del_catalogo = self.rotulos_plugin.get(id);
        let nombre = del_catalogo
            .map(|(n, _)| n.clone())
            .or_else(|| Some(self.extensiones.as_ref()?.concesion(id)?.nombre))
            // Sin rótulo conocido se cae al id —que el core SÍ valida— pero
            // por la misma puerta que todo lo demás: quien lo manda es el
            // daemon y no este proceso.
            .unwrap_or_else(|| crate::extensions::texto_de_tercero(id));
        let titulo = del_catalogo
            .and_then(|(_, cs)| cs.get(comando).cloned())
            .or_else(|| {
                self.extensiones
                    .as_ref()?
                    .comandos_de_id(id)
                    .iter()
                    .find(|c| c.id == comando)
                    .map(|c| (c.title.clone(), c.hostile))
            })
            // El id de un COMANDO no se pinta: el manifiesto no le valida
            // charset. Sin título conocido, la línea se queda sin él.
            .unwrap_or_default();
        (nombre, titulo)
    }

    /// Los tres efectos que tocan la DISPOSICIÓN, juntos.
    ///
    /// Agrupados aquí y no en `aplicar_efecto` porque ese método es un
    /// reparto y crece por familias: tres brazos que hacen lo mismo —cambiar
    /// la forma de la pantalla— son un brazo con tres casos.
    fn efecto_de_disposicion(
        &mut self,
        efecto: Efecto,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match efecto {
            Efecto::Tamano(delta) => self.redimensionar(delta, backend, buzon),
            Efecto::Igualar => self.igualar(backend, buzon),
            Efecto::Partir { vertical } => self.partir(vertical, backend, buzon),
            Efecto::CerrarHueco => self.cerrar_hueco(backend, buzon),
            Efecto::AlternarHueco { kind } => self.alternar_hueco(kind, backend, buzon),
            Efecto::PestanaNueva => self.pestana_nueva(backend, buzon),
            Efecto::CerrarPestana => self.cerrar_pestana(backend, buzon),
            Efecto::CiclarPestana { atras } => self.ciclar_pestana(atras, backend, buzon),
            Efecto::MoverPestana { derecha } => self.mover_pestana(derecha, backend, buzon),
            Efecto::IrAPestana { n } => self.ir_a_pestana(n, backend, buzon),
            _ => self.abrir_disposiciones(),
        }
    }

    /// Abre otra PESTAÑA junto al hueco enfocado.
    ///
    /// El listado nuevo arranca en el mismo directorio y se queda el foco,
    /// por lo mismo que al partir. `add_tab` envuelve el hueco en un grupo si
    /// todavía no lo estaba: no hay que decidirlo aquí.
    fn pestana_nueva(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use norte_frontend::layout::KindId;
        let id = self.nuevo_slot();
        let nuevo = self.arbol.add_tab(
            SlotId(self.enfocado()),
            &Node::slot(SlotId(id), KindId::browser()),
        );
        self.aplicar_disposicion_con(nuevo, Some(SlotId(id)), backend, buzon)
    }

    /// Cierra la pestaña enfocada.
    ///
    /// Sin grupo no hace nada y lo DICE: cerrar el hueco entero es otro
    /// comando, y hacerlo aquí «porque no había pestañas» sería cerrar lo que
    /// nadie pidió cerrar.
    fn cerrar_pestana(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(nuevo) = self.arbol.close_tab(SlotId(self.enfocado())) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-tabs".to_owned(),
                },
                self.decir("host-no-tabs"),
            );
        };
        self.aplicar_disposicion(nuevo, backend, buzon)
    }

    /// Pasa a la pestaña siguiente —o anterior—, CICLANDO.
    fn ciclar_pestana(
        &mut self,
        atras: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let foco = SlotId(self.enfocado());
        let Some((tabs, activo)) = self.arbol.tabs_of(foco) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-tabs".to_owned(),
                },
                self.decir("host-no-tabs"),
            );
        };
        if tabs.is_empty() {
            return (self.aplicada(), Vec::new());
        }
        let n = isize::try_from(tabs.len()).unwrap_or(1);
        let i = isize::try_from(activo).unwrap_or(0);
        let delta = if atras { -1 } else { 1 };
        let destino = usize::try_from((i + delta).rem_euclid(n)).unwrap_or(0);
        self.activar_pestana(foco, destino, &tabs, backend, buzon)
    }

    /// Va a la pestaña `n` (base 1).
    fn ir_a_pestana(
        &mut self,
        n: usize,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let foco = SlotId(self.enfocado());
        let Some((tabs, _)) = self.arbol.tabs_of(foco) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-tabs".to_owned(),
                },
                self.decir("host-no-tabs"),
            );
        };
        let i = n.saturating_sub(1);
        if i >= tabs.len() {
            // Pedir la séptima cuando hay tres no va a la última: no es lo
            // que se pidió, y adivinar aquí es cambiar de pestaña sola.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-such-tab".to_owned(),
                },
                Vec::new(),
            );
        }
        self.activar_pestana(foco, i, &tabs, backend, buzon)
    }

    /// Pone delante la pestaña `destino` del grupo de `foco`.
    ///
    /// Y le da el FOCO: la pestaña que está delante es con la que se trabaja,
    /// y dejarlo en la que se acaba de esconder deja las teclas apuntando a
    /// un listado que no se ve.
    fn activar_pestana(
        &mut self,
        foco: SlotId,
        destino: usize,
        tabs: &[SlotId],
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let nuevo = self.arbol.set_active_for(foco, destino);
        let activo = tabs.get(destino).copied();
        self.aplicar_disposicion_con(nuevo, activo, backend, buzon)
    }

    /// Mueve la pestaña enfocada dentro de su grupo.
    ///
    /// NO da la vuelta: una pestaña que salta del final al principio por una
    /// pulsación de más es justo lo que nadie quería (la regla es del modelo
    /// compartido, y aquí solo se usa).
    fn mover_pestana(
        &mut self,
        derecha: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let foco = SlotId(self.enfocado());
        if self.arbol.tabs_of(foco).is_none() {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-tabs".to_owned(),
                },
                self.decir("host-no-tabs"),
            );
        }
        let nuevo = self.arbol.move_tab(foco, if derecha { 1 } else { -1 });
        self.aplicar_disposicion_con(nuevo, Some(foco), backend, buzon)
    }

    /// Un clic en una pestaña: la pone delante.
    ///
    /// El hueco viene del propio grupo, así que un clic contra un árbol que
    /// ya cambió no acierta por casualidad: si ese id ya no está en un grupo,
    /// se rehúsa.
    fn elegir_pestana(
        &mut self,
        slot_id: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let quien = SlotId(slot_id);
        let Some((tabs, _)) = self.arbol.tabs_of(quien) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        let Some(i) = tabs.iter().position(|t| *t == quien) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        self.activar_pestana(quien, i, &tabs, backend, buzon)
    }

    /// El id de hueco más alto del árbol, más uno.
    ///
    /// Del ÁRBOL y no de `huecos`: los auxiliares —sitios, tablero, hoja de
    /// atributos— no están en ese mapa, y reusar el id de uno abierto sería
    /// meter dos cosas en el mismo hueco.
    fn nuevo_slot(&self) -> u32 {
        self.arbol
            .slot_ids()
            .into_iter()
            .map(|SlotId(id)| id)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    }

    /// Parte el hueco enfocado y pone otro LISTADO al lado.
    ///
    /// El nuevo arranca en el directorio del que se parte, que es lo menos
    /// sorprendente: pedir sitio para trabajar no es irse a otra parte. Y el
    /// foco va al recién nacido, por lo mismo.
    fn partir(
        &mut self,
        vertical: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use norte_frontend::layout::{Dir, KindId};
        let id = self.nuevo_slot();
        let dir = if vertical {
            Dir::Vertical
        } else {
            Dir::Horizontal
        };
        let nuevo = self.arbol.split_slot(
            SlotId(self.enfocado()),
            dir,
            &Node::slot(SlotId(id), KindId::browser()),
        );
        // El foco al recién nacido, y DENTRO de la misma aplicación: partir
        // es pedir sitio para trabajar en él. En dos pasos serían dos fotos,
        // y la primera enseñaría el foco donde ya no está.
        self.aplicar_disposicion_con(nuevo, Some(SlotId(id)), backend, buzon)
    }

    /// Cierra el hueco enfocado.
    ///
    /// Salvo si con eso la pantalla se queda sin LISTADO: una pantalla sin un
    /// listado usable no es una pantalla —es un cuelgue con bordes—, y esa es
    /// la misma regla que el reparto compartido ya aplica por su cuenta
    /// (#229). Aquí se dice, en vez de dejar una tecla que no hace nada.
    fn cerrar_hueco(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(nuevo) = self.arbol.close_slot(SlotId(self.enfocado())) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-layout-last-panel".to_owned(),
                },
                self.decir("msg-layout-last-panel"),
            );
        };
        let quedan = nuevo
            .slot_ids()
            .into_iter()
            .any(|s| es_listado(&nuevo, s, &self.kinds));
        if !quedan {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-layout-last-panel".to_owned(),
                },
                self.decir("msg-layout-last-panel"),
            );
        }
        self.aplicar_disposicion(nuevo, backend, buzon)
    }

    /// Abre —o cierra— el hueco auxiliar de este kind.
    ///
    /// Los tres que esta ventana sabe PINTAR. Uno que solo se pintaría en
    /// gris no se abre: `layout.preview` sigue sin construirse por eso, y lo
    /// dice el catálogo, no un hueco vacío.
    ///
    /// Los bordes y los tamaños son los MISMOS que el TUI usa, y no por
    /// simetría: son anchos medidos —dieciséis celdas es el mínimo del kind
    /// de sitios, ocho filas son las seis del tablero más el marco, treinta
    /// es la etiqueta más larga de la hoja con su valor al lado.
    fn alternar_hueco(
        &mut self,
        kind: &'static str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use norte_frontend::layout::{Bindings, Edge, Follow, KindId, Size};
        let abierto = self
            .arbol
            .slot_ids()
            .into_iter()
            .find(|s| kind_de(&self.arbol, *s).is_some_and(|k| k.as_str() == kind));
        if let Some(id) = abierto {
            let Some(nuevo) = self.arbol.close_slot(id) else {
                return (self.aplicada(), Vec::new());
            };
            return self.aplicar_disposicion(nuevo, backend, buzon);
        }
        let id = SlotId(self.nuevo_slot());
        let hoja = match kind {
            // La hoja de atributos SIGUE al rol activo: describe lo que el
            // cursor señala, y sin la atadura describiría el hueco donde
            // nació para siempre.
            "metadata" => Node::slot_bound(
                id,
                KindId::new(kind),
                Bindings {
                    follows: Some(Follow::Role(RoleId::Active)),
                },
            ),
            _ => Node::slot(id, KindId::new(kind)),
        };
        let (borde, tamano) = match kind {
            "places" => (Edge::Left, Size::Fixed(16)),
            "processes" => (Edge::Bottom, Size::Fixed(8)),
            _ => (Edge::Right, Size::Fixed(30)),
        };
        let nuevo = self
            .arbol
            .dock(SlotId(self.enfocado()), borde, tamano, &hoja);
        self.aplicar_disposicion(nuevo, backend, buzon)
    }

    /// Cambia el tamaño del hueco con el FOCO, no del listado activo.
    ///
    /// Del foco a propósito: la única forma de ensanchar la barra lateral es
    /// tenerla enfocada y crecer, y `activo()` —que se salta lo que no es un
    /// listado— habría redimensionado el panel de al lado.
    ///
    /// Redimensionar es una decisión sobre EL ÁRBOL, así que se guarda en él:
    /// el reparto se recalcula desde el árbol nuevo, y no al revés.
    fn redimensionar(
        &mut self,
        delta: i64,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let paso = i16::try_from(delta.clamp(i64::from(i16::MIN), i64::from(i16::MAX)))
            .unwrap_or(if delta < 0 { -1 } else { 1 });
        let nuevo = self.arbol.resize(SlotId(self.enfocado()), paso);
        self.aplicar_arbol(nuevo, backend, buzon)
    }

    /// Iguala el peso de los hermanos del hueco con el foco.
    fn igualar(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let nuevo = self.arbol.equalize(SlotId(self.enfocado()));
        self.aplicar_arbol(nuevo, backend, buzon)
    }

    /// Sustituye el árbol y vuelve a repartir.
    ///
    /// Si el reparto no cambia —el hueco estaba en su tope, o no tiene
    /// hermanos con los que repartir— NO se manda nada: un parche que no
    /// cambia nada obliga a repintar para nada, y la tecla ya dijo lo suyo
    /// sin moverse.
    fn aplicar_arbol(
        &mut self,
        nuevo: Node,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let antes = self.reparto.clone();
        self.arbol = nuevo;
        self.reparto = resolve(rect(self.viewport), &self.arbol, &self.kinds);
        if self.reparto.placements == antes.placements {
            return (self.aplicada(), Vec::new());
        }
        self.reconcilia_roles();
        // El reparto cambió: lo que acaba de salir de `hidden` no tiene
        // listado y nadie más se lo va a pedir.
        self.despertar_visibles(backend, buzon);
        let cambio = ViewChange::Layout(self.disposicion());
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// El foco está en la barra lateral de sitios.
    fn sitios_tienen_el_foco(&self) -> bool {
        self.sitios.is_some()
            && self
                .roles
                .get(RoleId::Active)
                .is_some_and(|s| self.hueco_de_sitios() == Some(s))
    }

    /// El movimiento y la activación, con el foco en la barra lateral.
    ///
    /// El vocabulario es el del LISTADO porque es el único mapa que esta
    /// ventana tiene —no hay pantalla `dialog` aquí—, y cada comando
    /// significa en la barra lo que significa en su superficie: bajar baja
    /// por ella, entrar va al sitio, y la tecla de marcar PLIEGA, porque una
    /// barra lateral no tiene nada que marcar y sí dos secciones que abrir y
    /// cerrar.
    ///
    /// La activación necesita el backend, así que se devuelve `None` para
    /// que la trate `aplicar_efecto` por su camino normal; aquí solo se
    /// mueve el cursor.
    fn efecto_en_sitios(
        &mut self,
        efecto: Efecto,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        let estado = self.sitios.as_mut()?;
        let filas = estado.rows().len();
        if filas == 0 {
            return Some((self.aplicada(), Vec::new()));
        }
        let total = i64::try_from(filas).unwrap_or(i64::MAX);
        let actual = i64::try_from(estado.cursor().min(filas - 1)).unwrap_or(0);
        let destino = match efecto {
            Efecto::Cursor(n) => actual.saturating_add(n.clamp(-total, total)),
            Efecto::Pagina(n) => {
                actual.saturating_add(n.clamp(-total, total).saturating_mul(total))
            }
            Efecto::Extremo { al_final: false } => 0,
            Efecto::Extremo { al_final: true } => total - 1,
            // Todo lo demás sigue su camino. Entrar y plegar, en concreto,
            // necesitan el backend —una navegación, o volver a pedir los
            // volúmenes—, así que los atiende quien sí lo tiene.
            _ => return None,
        };
        estado.set_cursor(usize::try_from(destino.max(0)).unwrap_or(0).min(filas - 1));
        let snap = self.snapshot();
        Some((
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        ))
    }

    /// Alimenta la barra lateral con los favoritos de la configuración.
    ///
    /// De la config con la que ARRANCÓ la ventana, que es la que está usando.
    /// Un favorito cuya ruta no parsea se conserva con su clave de error: la
    /// hotlist es data del usuario, no configuración estructural, y uno que
    /// desaparece en silencio es un fallo que nadie puede ver.
    fn sembrar_sitios(&mut self) {
        let items: Vec<(String, Result<VPath, String>)> = self
            .config
            .common
            .hotlist
            .iter()
            .map(|h| (h.name.clone(), h.target.clone()))
            .collect();
        let estado = self
            .sitios
            .get_or_insert_with(norte_frontend::places::PlacesState::new);
        estado.set_favorites(&items);
        self.gen_sitios += 1;
    }

    /// Pide los volúmenes para la barra lateral.
    ///
    /// Lo llaman el arranque y desplegar la sección de unidades. Y nadie más:
    /// una barra lateral con reloj rompería la regla de suspensión del ADR
    /// 0058 desde el primer frame, y `host.volumes` no es gratis — monta y
    /// consulta espacio en cada filesystem.
    fn pedir_sitios(&mut self, backend: &Arc<dyn HostBackend>, buzon: &mpsc::Sender<Mensaje>) {
        if self.hueco_de_sitios().is_none() {
            return;
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(PLAZO_PLUGINS, backend.volumes()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::SitiosVolumenes(res))))
                .await;
        });
    }

    /// Los volúmenes llegaron a la barra lateral.
    ///
    /// Un fallo NO vacía lo que hubiera: lo que se veía sigue siendo lo
    /// último que el host dijo.
    fn aplicar_sitios(
        &mut self,
        res: Result<Vec<norte_proto::methods::Volume>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let Ok(vols) = res else {
            return None;
        };
        // Solo si la barra EXISTE en esta disposición. `get_or_insert_with`
        // creaba un estado —sin favoritos, porque `sembrar_sitios` no corre—
        // para una respuesta rezagada de una disposición que ya no tiene
        // hueco `places`, y luego mandaba una foto entera para nada.
        // `pedir_sitios` ya se guarda igual.
        self.hueco_de_sitios()?;
        self.sitios
            .get_or_insert_with(norte_frontend::places::PlacesState::new)
            .set_drives(&vols);
        // Las unidades se insertan ANTES que los favoritos: todo indice
        // pintado hasta ahora nombra otra fila.
        self.gen_sitios += 1;
        let snap = self.snapshot();
        Some(self.sobre(UiUpdate::Snapshot(Box::new(snap))))
    }

    /// Un click en una fila de la barra lateral: la elige Y la activa.
    fn activar_sitio(
        &mut self,
        row: u32,
        generation: u64,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if generation != self.gen_sitios {
            // Lo pulsado y lo que hay ahora no son la misma lista: los
            // volúmenes aterrizan EN MEDIO. Rechazar es lo único correcto —
            // `set_cursor` recorta al último, así que seguir habría navegado
            // al último sitio de la barra.
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let Some(estado) = self.sitios.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        if row as usize >= estado.rows().len() {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        estado.set_cursor(row as usize);
        self.activar_sitio_del_cursor(backend, buzon)
    }

    /// Activa la fila del cursor de la barra lateral: navega a ella, o pliega
    /// su sección si es una cabecera.
    ///
    /// El `cd` va al LISTADO enfocado por el mismo camino que cualquier otro:
    /// es lo que hace que tener la barra abierta no cambie a dónde van las
    /// operaciones.
    fn activar_sitio_del_cursor(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(estado) = self.sitios.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        if let Some(destino) = estado.activate().cloned() {
            return (
                self.aplicada(),
                self.navegar(&destino, Trail::Record, backend, buzon),
            );
        }
        // Una cabecera: se pliega. Y desplegar las unidades ES el momento de
        // volver a pedirlas — un disco montado o desmontado desde que se
        // abrió la ventana se ve aquí, sin un reloj de por medio.
        estado.toggle_fold();
        self.gen_sitios += 1;
        let desplegadas = estado
            .rows()
            .iter()
            .any(|r| matches!(r, norte_frontend::places::PlaceRow::Drive { .. }));
        if desplegadas {
            self.pedir_sitios(backend, buzon);
        }
        let snap = self.snapshot();
        (
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// La barra lateral de sitios, proyectada.
    ///
    /// Si todavía no hay estado —la disposición la coloca pero nadie la ha
    /// alimentado— se proyecta VACÍA con sus dos cabeceras, que es lo que
    /// hace el modelo compartido: la lista no da un brinco cuando lleguen los
    /// volúmenes.
    fn barra_de_sitios(&self, id: u32) -> crate::dto::PlacesSlotView {
        use norte_frontend::places::{PlaceRow, PlacesState};
        let generation = self.gen_sitios;

        let vacia = PlacesState::new();
        let estado = self.sitios.as_ref().unwrap_or(&vacia);
        let rows = estado
            .rows()
            .iter()
            .map(|r| match r {
                PlaceRow::Header { section, folded } => crate::dto::PlaceRowView::Header {
                    label: clamp_display(norte_i18n::t_in(self.lang, section.label_key())),
                    folded: *folded,
                },
                PlaceRow::Drive {
                    label,
                    mount,
                    free,
                    total,
                    read_only,
                } => {
                    // La etiqueta son BYTES y el punto de montaje un `VPath`:
                    // los dos por la puerta compartida, nunca por
                    // `to_string_lossy`.
                    let (pintable, hostil) = if label.is_empty() {
                        norte_frontend::display::path_display(mount)
                    } else {
                        norte_frontend::display_name(label)
                    };
                    crate::dto::PlaceRowView::Drive {
                        label: clamp_display(pintable),
                        hostile: hostil,
                        detail: clamp_display(self.espacio_de(*free, *total, *read_only)),
                    }
                }
                PlaceRow::Favorite { name, target } => {
                    let (destino, hostil) = match target {
                        Ok(v) => norte_frontend::display::path_display(v),
                        Err(_) => (String::new(), false),
                    };
                    let (nombre, nombre_hostil) = norte_frontend::display_name(name.as_bytes());
                    crate::dto::PlaceRowView::Favorite {
                        // El nombre lo escribe el usuario, pero puede venir
                        // de la capa de PROYECTO: se enmascara igual.
                        name: clamp_display(nombre),
                        target: clamp_display(destino),
                        // El nombre O el destino. La bandera documentaba el
                        // destino y el nombre se enmascaraba tirando la suya,
                        // así que un favorito llamado con un override bidi
                        // llegaba sin marca ninguna.
                        hostile: hostil || nombre_hostil,
                        broken: target.as_ref().err().map_or_else(String::new, |clave| {
                            clamp_display(norte_i18n::t_in(self.lang, clave))
                        }),
                    }
                }
            })
            .collect();
        crate::dto::PlacesSlotView {
            slot_id: id,
            rows,
            cursor: estado.cursor() as u64,
            generation,
        }
    }

    /// El espacio de un volumen, dicho.
    ///
    /// Un tamaño que el sistema no contestó se DICE: un `0` se lee como
    /// «lleno», que es lo contrario de «no lo sé».
    fn espacio_de(&self, free: Option<u64>, total: Option<u64>, read_only: bool) -> String {
        let mut trozos = Vec::new();
        match (free, total) {
            (Some(f), Some(t)) => trozos.push(norte_i18n::ta_in(
                self.lang,
                "picker-volume-space",
                &[
                    ("free", &norte_frontend::human_bytes_short(f)),
                    ("total", &norte_frontend::human_bytes_short(t)),
                ],
            )),
            _ => trozos.push(norte_i18n::t_in(self.lang, "volumes-size-unknown")),
        }
        if read_only {
            trozos.push(norte_i18n::t_in(self.lang, "picker-volume-read-only"));
        }
        trozos.join(" · ")
    }

    /// El hueco que ocupa la barra lateral, si la disposición coloca una.
    fn hueco_de_sitios(&self) -> Option<SlotId> {
        self.reparto
            .placements
            .iter()
            .map(|(s, _)| *s)
            .find(|s| kind_de(&self.arbol, *s).is_some_and(|k| k.as_str() == "places"))
    }

    /// La hoja de atributos de un hueco `metadata`.
    ///
    /// Lo que enseña sale del panel al que este hueco SIGUE, resuelto con el
    /// motor compartido: un hueco que sigue a un rol que se ha quedado sin
    /// panel degrada al activo en vez de mirar al vacío en silencio.
    ///
    /// No pide nada: la `Entry` ya la trajo el listado.
    fn hoja_de_atributos(&self, slot: SlotId) -> crate::dto::MetadataSlotView {
        use norte_frontend::columns::{ColumnId, ColumnStyle, header_label, styled_cell};

        let SlotId(id) = slot;
        let mut diags = Vec::new();
        let seguido =
            norte_frontend::layout::resolve_follow(&self.arbol, slot, &self.roles, &mut diags)
                .or_else(|| self.roles.get(norte_frontend::layout::RoleId::Active));
        let entrada = seguido
            .and_then(|SlotId(s)| self.huecos.get(&s))
            .and_then(|h| h.pane.selected());
        let Some(e) = entrada else {
            return crate::dto::MetadataSlotView {
                slot_id: id,
                fields: Vec::new(),
                note: clamp_display(norte_i18n::t_in(self.lang, "metadata-empty")),
            };
        };
        let mut fields = Vec::new();
        let mut campo = |clave: &str, valor: String, hostile: bool| {
            fields.push(crate::dto::MetadataFieldView {
                label: clamp_display(norte_i18n::t_in(self.lang, clave)),
                value: clamp_display(valor),
                hostile,
            });
        };
        let nombre = e
            .path
            .file_name()
            .map_or_else(Vec::new, |s| s.as_bytes().to_vec());
        let (pintable, hostil) = norte_frontend::display_name(&nombre);
        campo("metadata-name", pintable, hostil);
        campo(
            "metadata-kind",
            norte_i18n::t_in(
                self.lang,
                match e.kind {
                    EntryKind::Dir => "metadata-kind-dir",
                    EntryKind::File => "metadata-kind-file",
                    EntryKind::Symlink => "metadata-kind-symlink",
                    EntryKind::Other => "metadata-kind-other",
                },
            ),
            false,
        );
        if let Some(n) = e.size {
            // El humano y el exacto, los dos: «1,2 MiB» no sirve para
            // comparar y `1258291` no sirve para leer.
            campo(
                "metadata-size",
                format!("{} ({n})", norte_frontend::human_bytes_short(n)),
                false,
            );
        }
        if let Some(ms) = e.mtime_ms {
            campo(
                "metadata-mtime",
                norte_frontend::columns::format_mtime(
                    ms,
                    norte_frontend::columns::TimeFormat::Iso,
                    ms,
                ),
                false,
            );
        }
        // Los atributos que el provider YA trajo. Van por la MISMA puerta que
        // su columna equivalente, para que la hoja y la columna no puedan
        // discrepar sobre lo que vale un atributo.
        let catalogo = self.catalogos.get(e.path.scheme());
        let ahora = e.mtime_ms.unwrap_or(0);
        for attr in e.attrs.keys() {
            let col = ColumnId::Attr(attr.clone());
            let style = ColumnStyle::default_for_id(&col, catalogo);
            if let Some(celda) = styled_cell(e, &col, ahora, &style) {
                // La marca se saca del valor CRUDO, no de la celda ya
                // formateada: `styled_cell` enmascara por dentro y no
                // devuelve la bandera, y volver a preguntársela a lo ya
                // enmascarado no contesta nada —U+FFFD no es un peligro de
                // terminal, así que un valor ya convertido se declara fiel—.
                // Aquí se ponía `false` a mano, o sea que la hoja de
                // atributos decía que todo era fiel mientras la COLUMNA
                // equivalente sí marcaba los mismos bytes.
                let hostil = match e.attrs.get(attr) {
                    Some(norte_proto::AttrValue::Text(t)) => {
                        norte_frontend::display_name(t.as_bytes()).1
                    }
                    Some(norte_proto::AttrValue::Bytes(b)) => norte_frontend::display_name(b).1,
                    // Los demás son números o marcas de tiempo que formatea
                    // norte: no hay texto de tercero que enmascarar.
                    _ => false,
                };
                fields.push(crate::dto::MetadataFieldView {
                    label: clamp_display(header_label(&col, &style, catalogo)),
                    value: clamp_display(celda),
                    hostile: hostil,
                });
            }
        }
        crate::dto::MetadataSlotView {
            slot_id: id,
            fields,
            note: String::new(),
        }
    }

    /// Enseña el tema activo por dentro.
    fn abrir_tema(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.mirando_tema = true;
        let cambio = ViewChange::Theme {
            theme: self.vista_tema(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// La proyección del tema.
    fn vista_tema(&self) -> Option<crate::dto::ThemeView> {
        self.mirando_tema.then(|| self.tema.vista())
    }

    /// Abre el selector de volúmenes y PIDE la tabla de montaje.
    ///
    /// Igual que el catálogo de extensiones: se abre diciendo que está
    /// preguntando, no esperando.
    fn abrir_volumenes(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.abrir_volumenes_en(self.activo(), backend, buzon)
    }

    /// Los volúmenes para el hueco de un LADO de la pantalla.
    ///
    /// `pane.select-drive-left`/`-right` nombran un lado y no el foco —es lo
    /// que hacen `Alt+F1`/`Alt+F2`—, y en un árbol de huecos el único
    /// significado honesto de «izquierda» es la GEOMETRÍA del reparto: el
    /// listado que se ve más a la izquierda. Sin ninguno de ese lado se dice,
    /// en vez de caer al del foco: montar un volumen en el panel equivocado
    /// es exactamente lo que este comando existe para evitar.
    fn abrir_volumenes_de_lado(
        &mut self,
        derecha: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(slot) = self.listado_del_lado(derecha) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-other-slot".to_owned(),
                },
                Vec::new(),
            );
        };
        self.abrir_volumenes_con(
            crate::pickers::Selector::volumenes_de_lado(slot, derecha),
            backend,
            buzon,
        )
    }

    /// El listado que se ve más a la izquierda —o más a la derecha— del
    /// reparto de ESTE tamaño.
    ///
    /// Solo entre los que se ven: una pestaña de atrás no está en ningún
    /// lado de la pantalla. Empata por `y` y luego por id, para que dos
    /// listados en la misma columna den siempre la misma respuesta.
    fn listado_del_lado(&self, derecha: bool) -> Option<u32> {
        let mut candidatos: Vec<(u16, u16, u32)> = self
            .reparto
            .placements
            .iter()
            .filter(|(s, _)| self.huecos.contains_key(&s.0))
            .map(|(s, r)| (r.x, r.y, s.0))
            .collect();
        candidatos.sort_unstable();
        if derecha {
            candidatos.last().map(|(_, _, id)| *id)
        } else {
            candidatos.first().map(|(_, _, id)| *id)
        }
    }

    /// Abre el selector de volúmenes para un hueco concreto y PIDE la tabla.
    fn abrir_volumenes_en(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.abrir_volumenes_con(crate::pickers::Selector::volumenes(slot), backend, buzon)
    }

    /// El cuerpo compartido: abre ESTE selector y pide la tabla de montaje.
    fn abrir_volumenes_con(
        &mut self,
        selector: crate::pickers::Selector,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.selector = Some(selector);
        self.gen_selector += 1;
        let apertura = self.gen_selector;
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(PLAZO_PLUGINS, backend.volumes()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::Volumenes(apertura, res))))
                .await;
        });
        let cambio = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// La tabla de montaje llegó.
    ///
    /// Un fallo se aplica igual: deja de estar preguntando con la lista
    /// vacía, que ya sabe decirse. Y si el selector se cerró mientras volaba,
    /// no hay nada que hacer.
    fn aplicar_volumenes(
        &mut self,
        apertura: u64,
        res: Result<Vec<norte_proto::methods::Volume>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        // De ESTA apertura: la generación sube al abrir, así que una
        // respuesta de la anterior no casa.
        if apertura != self.gen_selector {
            return None;
        }
        let lang = self.lang;
        let s = self.selector.as_mut()?;
        s.set_volumenes(&res.unwrap_or_default(), lang);
        self.gen_selector += 1;
        let cambio = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        Some(self.parche(vec![cambio]))
    }

    /// La proyección del selector.
    fn vista_selector(&self) -> Option<crate::dto::PickerView> {
        let mut v = self.selector.as_ref()?.vista(self.lang);
        v.generation = self.gen_selector;
        Some(v)
    }

    /// Las teclas mientras se mira el tema. Solo se cierra.
    fn tecla_en_tema(
        &mut self,
        k: &crate::keys::KeyInput,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if !matches!(k.key.as_str(), "Escape" | "esc") {
            return (self.aplicada(), Vec::new());
        }
        self.mirando_tema = false;
        let cambio = ViewChange::Theme { theme: None };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Las teclas mientras un selector está abierto.
    fn tecla_en_selector(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        /// Cuántas filas mueve una página.
        const PAGINA: i64 = 10;
        let Some(s) = self.selector.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match k.key.as_str() {
            "Escape" | "esc" => self.selector = None,
            "ArrowDown" | "down" => s.mover(1),
            "ArrowUp" | "up" => s.mover(-1),
            "PageDown" | "pgdn" => s.mover(PAGINA),
            "PageUp" | "pgup" => s.mover(-PAGINA),
            "Home" | "home" => s.mover(i64::MIN / 2),
            "End" | "end" => s.mover(i64::MAX / 2),
            "Enter" | "enter" => return self.elegir_del_selector(backend, buzon),
            _ => return (self.aplicada(), Vec::new()),
        }
        let cambio = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Elegir del selector: navegar al volumen.
    ///
    /// Navegar es LECTURA, así que el volumen sí se abre — al contrario que
    /// una conexión, que esta ventana ni enumera todavía.
    ///
    /// El cierre viaja en su PROPIO parche y antes de la navegación, como el
    /// de la paleta y por lo mismo: un renderer que aplica parches se
    /// quedaría el selector pintado encima del listado nuevo.
    fn elegir_del_selector(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(s) = self.selector.as_ref() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // El hueco lo dijo el selector al ABRIRSE: `pane.select-drive-left`
        // nombra un lado, y leer el foco aquí haría que moverlo con la lista
        // puesta montara el volumen en otro panel.
        let slot = s.slot();
        if !self.huecos.contains_key(&slot) || self.oculto(slot) {
            // El reparto cambió con la lista puesta: el hueco que el selector
            // capturó al abrirse ya no está, o dejó de verse. Navegar ahí
            // traería un listado que nadie va a mirar —contra «lo que no se
            // ve no se trae»— o no haría nada y cerraría el selector en
            // silencio.
            self.selector = None;
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-other-slot".to_owned(),
                },
                vec![self.parche(vec![ViewChange::Picker { picker: None }])],
            );
        }
        let Some(destino) = s.elegir() else {
            if s.hay_fila() {
                // Hay fila y no lleva a ninguna parte: un favorito cuya ruta
                // no parsea. Se DICE, que es lo que la fila ya avisaba.
                return (
                    ActionAck::Unavailable {
                        reason_key: "hotlist-invalid".to_owned(),
                    },
                    Vec::new(),
                );
            }
            // Sin filas todavía (o la tabla llegó vacía): no hay a dónde ir.
            return (self.aplicada(), Vec::new());
        };
        self.selector = None;
        let cierre = self.parche(vec![ViewChange::Picker { picker: None }]);
        let mut envios = vec![cierre];
        envios.extend(self.navegar_hueco(slot, &destino, Trail::Record, backend, buzon));
        (self.aplicada(), envios)
    }

    /// Un click en una fila del selector: la elige.
    fn elegir_fila_del_selector(
        &mut self,
        row: u32,
        generation: u64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if generation != self.gen_selector {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let Some(s) = self.selector.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        s.senalar(row as usize);
        let cambio = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Abre el gestor de extensiones y PIDE el catálogo.
    ///
    /// Se abre vacío y diciendo que está cargando, no esperando: una ventana
    /// congelada mientras el daemon contesta es peor que una lista que
    /// aparece medio segundo después. Y «cargando» no es lo mismo que
    /// «ninguna»: una lista vacía sin ese aviso se lee como que no hay nada
    /// instalado.
    /// Abre el panel de sesiones de AGENTE.
    ///
    /// No pide nada al daemon: no hay método que enumere sesiones vivas, así
    /// que lo que se enseña es lo que ESTA ventana ha visto pedir permiso —y
    /// el panel lo dice—. Eso es también lo que hace que el operando del
    /// deshacer se ELIJA en vez de teclearse, que es lo que la tarea 5.3
    /// rechazó: un id de sesión tecleado se puede equivocar, y deshacer la
    /// sesión equivocada es deshacer el trabajo de otro.
    fn abrir_agentes(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.agencia.panel = true;
        self.agencia.sesiones.al_abrir();
        let cambio = ViewChange::Agents {
            agents: self.vista_agentes(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// La proyección del panel de agentes.
    fn vista_agentes(&self) -> Option<crate::dto::AgentsView> {
        // Una ventana sin efectos NO se suscribe al canal de aprobaciones,
        // así que su lista está vacía por ESO y no porque nadie haya pedido
        // nada. La pantalla lo dice, en vez de afirmar lo que no sabe.
        let escucha = self.efectos == crate::commands::Efectos::Completo;
        self.agencia
            .panel
            .then(|| self.agencia.sesiones.vista_de(self.lang, escucha))
    }

    /// Las teclas mientras el panel de agentes está abierto.
    fn tecla_en_agentes(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        /// Cuántas filas mueve una página.
        const PAGINA: i64 = 10;
        match k.key.as_str() {
            "Escape" | "esc" => self.agencia.panel = false,
            "ArrowDown" | "down" => self.agencia.sesiones.mover(1),
            "ArrowUp" | "up" => self.agencia.sesiones.mover(-1),
            "PageDown" | "pgdn" => self.agencia.sesiones.mover(PAGINA),
            "PageUp" | "pgup" => self.agencia.sesiones.mover(-PAGINA),
            "Home" | "home" => self.agencia.sesiones.mover(i64::MIN / 2),
            "End" | "end" => self.agencia.sesiones.mover(i64::MAX / 2),
            // `u` DESHACE la sesión entera, y pregunta antes: es la operación
            // más grande que esta ventana puede lanzar de un tirón —revierte
            // todo lo que un agente hizo, en orden inverso— y no hay ninguna
            // otra que toque tantas cosas con una tecla.
            // Y exige la tecla PELADA, a diferencia del resto de letras de
            // este host: `ctrl+u` es memoria muscular de otra cosa, y esta es
            // la operación más grande que la ventana puede lanzar de un
            // tirón.
            "u" if !k.ctrl && !k.alt && !k.meta => return self.preguntar_por_deshacer(),
            _ => return (self.aplicada(), Vec::new()),
        }
        let _ = (backend, buzon);
        let cambio = ViewChange::Agents {
            agents: self.vista_agentes(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Un click sobre una fila del panel de agentes: la elige.
    fn elegir_agente(
        &mut self,
        row: u32,
        generation: u64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Con un diálogo encima, el panel no recibe: es modal para el teclado
        // y tiene que serlo también para el ratón.
        if !self.agencia.panel || !self.dialogos.is_empty() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        if !self.agencia.sesiones.senalar(row as usize, generation) {
            // La lista cambió entre el pintado y el clic: se rehúsa en vez de
            // recortar, porque recortar es elegir por el lector.
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let cambio = ViewChange::Agents {
            agents: self.vista_agentes(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Abre la pregunta de deshacer una sesión entera.
    fn preguntar_por_deshacer(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        let Some(sesion) = self.agencia.sesiones.elegida() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-session".to_owned(),
                },
                Vec::new(),
            );
        };
        // Uno cada vez: dos `policy.undo_session` de la misma sesión caminan
        // la MISMA lista de entradas —cada uno la fotografía antes de que el
        // otro registre sus compensaciones—, y el segundo devuelve un informe
        // lleno de bloqueos que no son de nadie.
        if self.agencia.sesiones.tiene_undo_vivo(&sesion) {
            let fuera = self.decir("host-undo-already-running");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-undo-already-running".to_owned(),
                },
                fuera,
            );
        }
        // El id, enmascarado, en su propio campo: es una clave opaca del
        // daemon que puede llevar cualquier byte, y una decisión sobre «esta
        // sesión» que no dice cuál no es una decisión.
        let (pintable, hostil) = norte_frontend::display_name(sesion.as_bytes());
        let modal = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id: modal,
            title_key: "modal-undo-session-title".to_owned(),
            destination: None,
            subject: Some(crate::dto::DialogLine {
                text: clamp_display(pintable),
                hostile: hostil,
            }),
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            // El cuerpo dice qué ALCANCE tiene, que es lo que no se ve en la
            // fila: deshacer una sesión revierte TODO lo que hizo, no lo
            // último, y lo que no se pueda revertir —algo irreversible, algo
            // que la policy deniegue ahora— se dirá en el informe.
            body: vec![crate::dto::DialogLine {
                text: clamp_display(norte_i18n::t_in(self.lang, "modal-undo-session-scope")),
                hostile: false,
            }],
            overflow_note: String::new(),
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    // Deshacer ESCRIBE: mueve ficheros de vuelta y borra los
                    // que la sesión creó.
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
            id: modal,
            vista: vista.clone(),
            input_crudo: String::new(),
            reconocido: true,
            al_confirmar: Some(Pendiente::DeshacerSesion { sesion }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Crea el directorio TECLEADO dentro de este otro.
    ///
    /// El nombre se valida AQUÍ, con la misma regla que cualquier otro
    /// segmento: ni vacío, ni `/`, ni NUL, ni `.`/`..`. Un nombre que no vale
    /// no encola nada y lo dice; el texto tecleado no se pierde porque el
    /// diálogo se vuelve a abrir con él.
    ///
    /// El mismo cinturón que el rename: un nombre TOCADO que aún lleva el
    /// carácter de sustitución no se escribe. La asimetría de antes («crear
    /// no tiene siembra de la que heredar residuos») era falsa del ROUND
    /// TRIP: el host pinta su propia proyección enmascarada en el campo, y el
    /// renderer vuelve a sembrarlo con ella si tuvo que reconstruir el nodo —
    /// un diálogo de aprobación que se cuele por encima basta.
    fn crear_directorio(
        &mut self,
        dir: &VPath,
        nombre: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let seg = match Self::segmento_tecleado(nombre) {
            Ok(seg) => seg,
            Err(clave) => {
                self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
                let cambio = ViewChange::Status(self.status.clone());
                return (Some(clave), vec![self.parche(vec![cambio])]);
            }
        };
        let destino = dir.join(seg);
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let dir = dir.clone();
        tokio::spawn(async move {
            match backend.mkdir(destino).await {
                Ok(task) => {
                    let _ = buzon
                        .send(Mensaje::TaskNueva(Box::new((task, vec![dir], None))))
                        .await;
                }
                Err(e) => {
                    let _ = buzon.send(Mensaje::TaskFallida(Box::new(e))).await;
                }
            }
        });
        (None, Vec::new())
    }

    /// Los dos pendientes que fabrican ficheros a partir de lo TECLEADO:
    /// partir por tamaño y empaquetar por nombre (#132, #290).
    fn ejecutar_de_archivo(
        &mut self,
        pendiente: Pendiente,
        tecleado: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        match pendiente {
            Pendiente::Partir { path, dest_dir } => {
                self.partir_fichero(path, dest_dir, tecleado, backend, buzon)
            }
            Pendiente::Empaquetar { dir, sources } => {
                self.empaquetar(&dir, sources, tecleado, backend, buzon)
            }
            // El llamante ya filtró; nombrarlos aquí hace que un tercero sea
            // un error de compilación.
            _ => (None, Vec::new()),
        }
    }

    /// Parte `path` en trozos del tamaño que se tecleó (#132, #290).
    ///
    /// El tamaño lo lee la misma función que el TUI: `10M` son 10 MiB y no
    /// diez millones, que es lo que significa en un gestor de ficheros. Un
    /// cero se rehúsa — trozos de cero bytes no terminan nunca.
    fn partir_fichero(
        &mut self,
        path: VPath,
        dest_dir: VPath,
        tamano: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(part_bytes) = norte_frontend::nav::parse_size(tamano) else {
            return (Some("msg-split-bad-size"), self.decir("msg-split-bad-size"));
        };
        let afectados = vec![dest_dir.clone()];
        let params = norte_proto::methods::FileSplitParams {
            path,
            part_bytes,
            dest_dir,
        };
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let mensaje = match backend.split_file(params).await {
                Ok(task) => Mensaje::TaskNueva(Box::new((task, afectados, None))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon.send(mensaje).await;
        });
        (None, Vec::new())
    }

    /// Empaqueta `sources` en el contenedor que se tecleó (#132, #290).
    ///
    /// El FORMATO sale del nombre y viaja explícito: un nombre sin extensión
    /// que sepamos ESCRIBIR se rehúsa aquí en vez de empaquetar en algo que
    /// nadie pidió — un `.rar` cae ahí, porque se delega y solo para leer.
    ///
    /// La base de los nombres guardados es el directorio del panel: quien
    /// desempaquete espera ver lo que se veía en pantalla, no rutas absolutas.
    fn empaquetar(
        &mut self,
        dir: &VPath,
        sources: Vec<VPath>,
        nombre: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let seg = match Self::segmento_tecleado(nombre) {
            Ok(seg) => seg,
            Err(clave) => return (Some(clave), self.decir(clave)),
        };
        let Some(format) = norte_frontend::nav::format_by_name(seg.as_bytes()) else {
            return (
                Some("msg-pack-unknown-format"),
                self.decir("msg-pack-unknown-format"),
            );
        };
        let params = norte_proto::methods::ArchivePackParams {
            sources,
            dest: dir.join(seg),
            format,
            level: None,
            base: dir.clone(),
        };
        let afectados = vec![dir.clone()];
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let mensaje = match backend.pack(params).await {
                Ok(task) => Mensaje::TaskNueva(Box::new((task, afectados, None))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon.send(mensaje).await;
        });
        (None, Vec::new())
    }

    /// Confirma el deshacer de UNA sesión: comprueba que no haya otro en
    /// marcha, lo lanza, y repinta la fila.
    ///
    /// La sesión es la que se LEYÓ en la pregunta, no la señalada ahora: la
    /// lista se reordena sola —una petición nueva sube a su sesión al primer
    /// puesto— y el diálogo se queda las teclas, no los mensajes de fondo.
    fn deshacer_sesion(
        &mut self,
        sesion: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.agencia.sesiones.tiene_undo_vivo(sesion) {
            return (
                Some("host-undo-already-running"),
                self.decir("host-undo-already-running"),
            );
        }
        self.agencia.sesiones.deshaciendo(sesion);
        // Sin ALCANCE conocido: un `undo_session` toca los directorios que la
        // sesión tocara, que esta ventana no sabe. Se relista lo que está EN
        // PANTALLA, que es donde el lector estaba mirando trabajar al agente.
        let visibles = self.dirs_visibles();
        Self::lanzar_deshacer(sesion.to_owned(), visibles, backend, buzon);
        let mut fuera = Vec::new();
        if self.agencia.panel {
            let cambio = ViewChange::Agents {
                agents: self.vista_agentes(),
            };
            fuera.push(self.parche(vec![cambio]));
        }
        (None, fuera)
    }

    /// Lanza el deshacer de una sesión entera.
    ///
    /// Como cualquier otra operación larga: es una Task, aparece en el
    /// tablero y su informe —lo que NO volvió— llega por el camino que la 5.3
    /// ya construyó.
    fn lanzar_deshacer(
        sesion: String,
        afectados: Vec<VPath>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            match backend.undo_session(sesion.clone()).await {
                Ok(task) => {
                    // El id de la task y la sesión, juntos: el desenlace
                    // llega por el progreso, que solo trae el id.
                    let _ = buzon
                        .send(Mensaje::Fondo(Box::new(Fondo::UndoDeSesion(
                            task.id.get(),
                            sesion,
                        ))))
                        .await;
                    let _ = buzon
                        .send(Mensaje::TaskNueva(Box::new((task, afectados, None))))
                        .await;
                }
                Err(e) => {
                    let _ = buzon.send(Mensaje::TaskFallida(Box::new(e))).await;
                }
            }
        });
    }

    /// Los directorios que hay AHORA en pantalla, sin repetir.
    fn dirs_visibles(&self) -> Vec<VPath> {
        let mut v: Vec<VPath> = Vec::new();
        for h in self.huecos.values() {
            let dir = h.pane.dir();
            if !v.contains(dir) {
                v.push(dir.clone());
            }
        }
        v
    }

    fn abrir_extensiones(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.extensiones = Some(crate::extensions::Extensiones::abrir());
        self.gen_extensiones += 1;
        self.pedir_catalogo_de_extensiones(backend, buzon);
        let cambio = ViewChange::Extensions {
            extensions: self.vista_extensiones(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Reparte una respuesta de fondo a la superficie que la pidió.
    ///
    /// UN sitio para las cinco: todas comprueban lo mismo —que su superficie
    /// siga abierta— y todas contestan lo mismo: los parches que haya que
    /// mandar, o ninguno.
    fn aplicar_de_fondo(
        &mut self,
        f: Fondo,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        match f {
            Fondo::PlanIa(epoca, res) => self.aplicar_plan_ia(epoca, *res, backend, buzon),
            Fondo::PlanDeLote(epoca, res) => self.aplicar_plan_de_lote(epoca, *res),
            Fondo::PluginsDeAyuda(res) => self.aplicar_catalogo_de_plugins(res, backend, buzon),
            Fondo::PaginaDePlugin(id, res) => self
                .aplicar_pagina_de_plugin(&id, res.as_ref().ok())
                .into_iter()
                .collect(),
            Fondo::Catalogo(apertura, peticion, res) => {
                self.aplicar_catalogo_de_extensiones(apertura, peticion, res, backend, buzon)
            }
            Fondo::FichaDePlugin(id, res) => self
                .aplicar_ficha(&id, res.as_ref().ok())
                .into_iter()
                .collect(),
            Fondo::UndoDeSesion(task_id, sesion) => {
                self.agencia.undos.insert(task_id, sesion);
                Vec::new()
            }
            Fondo::PluginsDePaleta(apertura, res) => self.aplicar_filas_de_plugin(apertura, res),
            Fondo::Gobernada(apertura, res) => {
                self.aplicar_gobierno(apertura, &res, backend, buzon)
            }
            Fondo::ConfigEscrita(apertura, id, res) => {
                self.aplicar_escritura(apertura, &id, res, backend, buzon)
            }
            Fondo::SalidaDeComando(apertura, datos) => self.aplicar_salida(apertura, *datos),
            Fondo::Volumenes(apertura, res) => {
                self.aplicar_volumenes(apertura, res).into_iter().collect()
            }
            Fondo::SitiosVolumenes(res) => self.aplicar_sitios(res).into_iter().collect(),
            Fondo::Resultados(epoca, lote) => {
                self.aplicar_resultados(epoca, &lote).into_iter().collect()
            }
            Fondo::Semanticos(epoca, hits) => self.aplicar_semanticos(epoca, hits),
            Fondo::ComparacionViva(epoca, id) => {
                if let Some(c) = self.comparacion.as_mut()
                    && c.epoca == epoca
                {
                    c.task = id;
                }
                Vec::new()
            }
            Fondo::FilasComparadas(epoca, lote) => self.aplicar_filas_comparadas(epoca, *lote),
            Fondo::PlanDeSyncVivo(epoca, id) => self.abrir_panel_de_sync(epoca, id),
            Fondo::SyncAplicando(epoca, id) => self.sync_aplicando(epoca, id, backend, buzon),
            Fondo::SyncNoAplicado(epoca, seguro) => {
                let mut fuera = Vec::new();
                if let Some(s) = self.sincronizacion.as_mut().filter(|s| s.epoca == epoca) {
                    if seguro {
                        s.vista.on_apply_abandoned();
                    } else {
                        // Ambiguo: el pestillo se QUEDA echado. La pantalla no
                        // puede decir «no se aplicó» de algo que quizá se está
                        // aplicando, ni ofrecer repetirlo.
                        fuera.extend(self.decir("msg-sync-apply-unknown"));
                    }
                }
                // Con su parche: `on_apply_abandoned` cambia lo que la
                // pantalla ofrece, y sin repintar, la `a` que acaba de
                // devolverse parece muerta.
                fuera.push(self.parche(vec![ViewChange::Sync {
                    sync: self.vista_sincronizacion(),
                }]));
                fuera
            }
            Fondo::InformeDeSync(epoca, estado, informe) => {
                self.informe_de_sync(epoca, &estado, *informe)
            }
            Fondo::PlanDeSyncFallido(epoca) => {
                if self.sync_pedida.as_ref().is_some_and(|p| p.epoca == epoca) {
                    self.sync_pedida = None;
                }
                Vec::new()
            }
            Fondo::EventoDeSync(epoca, ev) => self.aplicar_evento_de_sync(epoca, *ev),
            Fondo::Adornos(datos) => self.aplicar_adornos(*datos).into_iter().collect(),
            Fondo::Imagen(token, leido) => self.aplicar_imagen(token, leido).into_iter().collect(),
            Fondo::BusquedaViva(epoca, id) => {
                if let Some(b) = self.busqueda.as_mut()
                    && b.epoca == epoca
                {
                    b.task = id;
                }
                Vec::new()
            }
        }
    }

    /// El catálogo llegó al gestor.
    ///
    /// Un fallo también se aplica: deja de estar «cargando» y la lista queda
    /// vacía, que con el aviso apagado significa «no hay ninguna». Quedarse
    /// cargando para siempre sería la única respuesta peor.
    fn aplicar_catalogo_de_extensiones(
        &mut self,
        apertura: u64,
        peticion: u64,
        res: Result<norte_proto::methods::PluginListResult, Error>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // De ESTA apertura. «Sigue abierta» no es «es la misma».
        if apertura != self.gen_extensiones {
            return Vec::new();
        }
        // Y la más NUEVA de las que haya en vuelo: un catálogo viejo que
        // aterriza después del nuevo deja la columna «aprobada» diciendo lo
        // de antes, sobre un cambio que ya se hizo.
        if peticion <= self.catalogo_aplicado {
            return Vec::new();
        }
        self.catalogo_aplicado = peticion;
        let Some(e) = self.extensiones.as_mut() else {
            return Vec::new();
        };
        // Un fallo se aplica igual: deja de estar «cargando» con la lista
        // vacía, que ya sabe decirse. Quedarse cargando para siempre es la
        // única respuesta peor.
        e.set_catalogo(&res.unwrap_or(norte_proto::methods::PluginListResult {
            plugins: Vec::new(),
            errors: Vec::new(),
        }));
        let _ = (backend, buzon);
        let cambio = ViewChange::Extensions {
            extensions: self.vista_extensiones(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// Pide la ficha de la extensión elegida.
    fn pedir_ficha(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(e) = self.extensiones.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let Some(id) = e.reclamar_ficha() else {
            return (self.aplicada(), Vec::new());
        };
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(PLAZO_PLUGINS, backend.plugin_config(id.clone()))
                .await
            {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::FichaDePlugin(id, res))))
                .await;
        });
        (self.aplicada(), Vec::new())
    }

    /// La ficha llegó.
    fn aplicar_ficha(
        &mut self,
        id: &str,
        res: Option<&norte_proto::methods::PluginGetConfigResult>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let lang = self.lang;
        let e = self.extensiones.as_mut()?;
        match res {
            Some(r) => e.set_ficha(id, r, lang),
            // Un fallo también se APLICA: solo salir dejaba `pedida` puesta,
            // así que `reclamar_ficha` devolvía `None` para siempre y esa
            // fila no se podía volver a abrir —`enter` no hacía nada y no
            // decía nada— salvo moviendo el cursor a otra y volviendo. Es el
            // mismo criterio que este fichero ya aplica dos veces al
            // catálogo: quedarse cargando para siempre es la única respuesta
            // peor que un error.
            None => e.cerrar_ficha(),
        }
        let cambio = ViewChange::Extensions {
            extensions: self.vista_extensiones(),
        };
        Some(self.parche(vec![cambio]))
    }

    /// La proyección del gestor.
    fn vista_extensiones(&self) -> Option<crate::dto::ExtensionsView> {
        Some(self.extensiones.as_ref()?.vista())
    }

    /// Las teclas mientras el gestor está abierto.
    fn tecla_en_extensiones(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        /// Cuántas filas mueve una página.
        const PAGINA: i64 = 10;
        let Some(e) = self.extensiones.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // TRES REGÍMENES, y el orden importa. Mientras se TECLEA un valor,
        // las letras son letras: resolver `a` como «aprobar» ahí convierte
        // escribir la palabra «casa» en dos concesiones de capabilities.
        if e.editando() {
            return self.tecla_editando_config(k, backend, buzon);
        }
        match k.key.as_str() {
            "Escape" | "esc" => {
                // El primer `esc` cierra la FICHA, no el gestor: dejar la
                // lista por cerrar un detalle pierde dónde estaba el lector.
                if e.tiene_ficha() {
                    e.cerrar_ficha();
                } else {
                    self.extensiones = None;
                }
            }
            // Con la ficha abierta, las flechas recorren SUS claves: mover el
            // catálogo por debajo tiraría la ficha que se está leyendo.
            "ArrowDown" | "down" => {
                if !e.mover_en_ficha(1) {
                    e.mover(1);
                }
            }
            "ArrowUp" | "up" => {
                if !e.mover_en_ficha(-1) {
                    e.mover(-1);
                }
            }
            // Las de página y los extremos, por la misma puerta que las
            // flechas: con la ficha abierta recorren SUS claves, y solo
            // cuando no hay nada que andar caen al catálogo.
            "PageDown" | "pgdn" => {
                if !e.mover_en_ficha(PAGINA) {
                    e.mover(PAGINA);
                }
            }
            "PageUp" | "pgup" => {
                if !e.mover_en_ficha(-PAGINA) {
                    e.mover(-PAGINA);
                }
            }
            "Home" | "home" => {
                if !e.mover_en_ficha(i64::MIN / 2) {
                    e.mover(i64::MIN / 2);
                }
            }
            "End" | "end" => {
                if !e.mover_en_ficha(i64::MAX / 2) {
                    e.mover(i64::MAX / 2);
                }
            }
            "Enter" | "enter" => {
                if e.tiene_ficha() {
                    return self.activar_clave(backend, buzon);
                }
                return self.pedir_ficha(backend, buzon);
            }
            "a" => return self.gobernar_elegida(Cambio::Aprobacion, backend, buzon),
            "e" => return self.gobernar_elegida(Cambio::Encendido, backend, buzon),
            _ => return (self.aplicada(), Vec::new()),
        }
        let cambio = ViewChange::Extensions {
            extensions: self.vista_extensiones(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Las teclas mientras se TECLEA el valor de una clave.
    ///
    /// Régimen FIJO, como el de cualquier campo de este host: aquí una letra
    /// es una letra. `Enter` confirma —y entonces se escribe—, `Escape`
    /// cancela sin escribir, y el resto de teclas no significan nada.
    fn tecla_editando_config(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(e) = self.extensiones.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match k.key.as_str() {
            "Escape" | "esc" => e.cancelar_edicion(),
            "Backspace" | "backspace" => e.borrar(),
            "Enter" | "enter" => return self.confirmar_config(backend, buzon),
            otra => {
                // Una tecla imprimible es su carácter; cualquier otra —y
                // cualquier combinación con modificador— no es texto.
                let mut cs = otra.chars();
                match (cs.next(), cs.next()) {
                    (Some(c), None) if !k.ctrl && !k.alt && !k.meta => e.escribir(c),
                    _ => return (self.aplicada(), Vec::new()),
                }
            }
        }
        let cambio = ViewChange::Extensions {
            extensions: self.vista_extensiones(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// `Enter` sobre una clave: cicla, o abre el buffer para teclearla.
    fn activar_clave(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        let Some(e) = self.extensiones.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let escritura = e.activar_clave();
        let cambio = ViewChange::Extensions {
            extensions: self.vista_extensiones(),
        };
        let mut fuera = vec![self.parche(vec![cambio])];
        // Un `bool` o un `enum` YA cambiaron de valor en el modelo: lo que
        // queda es contárselo al daemon. Un `string`/`int` solo abrió el
        // buffer y todavía no hay nada que escribir.
        if let Some((id, escritura)) = escritura {
            fuera.extend(Self::escribir_config(
                self.gen_extensiones,
                &id,
                escritura,
                backend,
                buzon,
            ));
        }
        (self.aplicada(), fuera)
    }

    /// `Enter` con el buffer abierto: valida y escribe, o dice por qué no.
    fn confirmar_config(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // El buffer solo lo abre `activar_clave`, que ya comprueba esto, así
        // que hoy es inalcanzable — igual que `rechaza_por_solo_lectura`, que
        // existe de todas formas. Una puerta que escribe se comprueba en la
        // puerta.
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        let Some(e) = self.extensiones.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let Some(resultado) = e.confirmar_edicion() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match resultado {
            Ok((id, escritura)) => {
                let cambio = ViewChange::Extensions {
                    extensions: self.vista_extensiones(),
                };
                let mut fuera = vec![self.parche(vec![cambio])];
                fuera.extend(Self::escribir_config(
                    self.gen_extensiones,
                    &id,
                    escritura,
                    backend,
                    buzon,
                ));
                (self.aplicada(), fuera)
            }
            // La validación de ESTE lado no es la que permite —el daemon
            // vuelve a validar contra el esquema— pero decirlo aquí ahorra un
            // viaje y, sobre todo, dice CUÁL era la cota.
            Err(norte_frontend::settings::SettingsEditError::NotAnInt) => (
                ActionAck::Unavailable {
                    reason_key: "host-not-an-int".to_owned(),
                },
                self.decir("host-not-an-int"),
            ),
            Err(norte_frontend::settings::SettingsEditError::OutOfRange { min, max }) => {
                // El aviso lleva las cotas; el ACUSE no puede: nadie
                // sustituye variables en esa clave, así que un `{ $min }` en
                // el acuse se registra literalmente. Dos claves, y la que
                // lleva números es la que sí se traduce con ellos.
                let fuera = self.decir_con(
                    "host-out-of-range",
                    &[("min", &min.to_string()), ("max", &max.to_string())],
                );
                (
                    ActionAck::Unavailable {
                        reason_key: "host-value-rejected".to_owned(),
                    },
                    fuera,
                )
            }
        }
    }

    /// Manda UNA clave al daemon.
    ///
    /// El valor ya está puesto en el modelo (optimismo): lo que corrige un
    /// fallo es REPEDIR la ficha, no adivinar qué había antes.
    fn escribir_config(
        apertura: u64,
        id: &str,
        escritura: norte_frontend::plugin_config::PendingConfigWrite,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        let (id2, key, value) = (id.to_owned(), escritura.key, escritura.value);
        tokio::spawn(async move {
            let res = match tokio::time::timeout(
                PLAZO_PLUGINS,
                backend2.plugin_set_config(id2.clone(), key, value),
            )
            .await
            {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon2
                .send(Mensaje::Fondo(Box::new(Fondo::ConfigEscrita(
                    apertura, id2, res,
                ))))
                .await;
        });
        Vec::new()
    }

    /// La escritura contestó.
    fn aplicar_escritura(
        &mut self,
        apertura: u64,
        id: &str,
        res: Result<(), Error>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Err(e) = res else {
            return Vec::new();
        };
        let mut fuera = self.decir(norte_frontend::error::error_key(&e));
        if apertura != self.gen_extensiones {
            return fuera;
        }
        // Y se REPIDE la ficha: el valor optimista de la pantalla es ahora
        // mismo una mentira sobre lo que el plugin tiene configurado, y
        // adivinar el anterior es inventarse un tercer estado.
        //
        // Salvo si se está TECLEANDO: repedirla tira el `PluginConfigState`
        // entero, y con él lo que el lector lleva escrito de otra clave. Un
        // valor viejo en pantalla es malo; comerse lo que alguien acaba de
        // teclear, peor — y la corrección llega igual en cuanto cierre el
        // campo.
        if let Some(ext) = self.extensiones.as_mut()
            && ext.es_ficha_de(id)
            && !ext.editando()
        {
            ext.cerrar_ficha();
            // El cierre viaja SIEMPRE en su parche: `pedir_ficha` no manda
            // ninguno por su camino bueno, así que sin esto el renderer
            // seguía pintando una ficha que el host ya no tiene —y las
            // flechas, que ya no la encuentran, movían el catálogo por
            // debajo—.
            let cambio = ViewChange::Extensions {
                extensions: self.vista_extensiones(),
            };
            fuera.push(self.parche(vec![cambio]));
            let (_, partes) = self.pedir_ficha(backend, buzon);
            fuera.extend(partes);
        }
        fuera
    }

    /// `a`/`e` sobre la extensión elegida.
    fn gobernar_elegida(
        &mut self,
        cambio: Cambio,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        let Some(e) = self.extensiones.as_ref() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let Some(fila) = e.fila_elegida() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-extension".to_owned(),
                },
                Vec::new(),
            );
        };
        let (id, aprobada, encendida) = (fila.id.clone(), fila.approved, fila.enabled);
        match cambio {
            // Conceder PREGUNTA; retirar, no.
            Cambio::Aprobacion if !aprobada => self.preguntar_por_aprobacion(&id),
            Cambio::Aprobacion => {
                let fuera = self.gobernar(&id, Gobierno::Aprobar(false, None), backend, buzon);
                (self.aplicada(), fuera)
            }
            // ENCENDER un plugin sin aprobar no es una decisión que esta
            // pantalla pueda tomar por su cuenta: sin capabilities aprobadas
            // el core no lo va a cargar, y decir «encendido» sobre algo que
            // no corre es la pantalla que miente. APAGARLO sí, siempre: va en
            // la dirección segura, y negarlo dejaba sin poder apagar a una
            // extensión encendida a la que se le acababan de revocar las
            // capabilities —o sea, prohibía justo lo que hay que poder hacer.
            Cambio::Encendido if !aprobada && !encendida => (
                ActionAck::Unavailable {
                    reason_key: "host-extension-not-approved".to_owned(),
                },
                self.decir("host-extension-not-approved"),
            ),
            Cambio::Encendido => {
                let fuera = self.gobernar(&id, Gobierno::Encender(!encendida), backend, buzon);
                (self.aplicada(), fuera)
            }
        }
    }

    /// Abre la pregunta de conceder capabilities, con las capabilities
    /// dentro.
    fn preguntar_por_aprobacion(&mut self, id: &str) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(concesion) = self.extensiones.as_ref().and_then(|e| e.concesion(id)) else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let (nombre, capabilities, ancla) =
            (concesion.nombre, concesion.capabilities, concesion.digest);
        // Una capability por LÍNEA, y el nombre de la extensión aparte: son
        // los operandos de la decisión, y meterlos en la frase es lo que
        // deja a un nombre de tercero imitando el texto de la ventana. Cada
        // una con SU bandera: la que se pinta distinta de lo que dice es
        // justo la que un manifiesto hostil escribe para colarse.
        // Y NINGUNA se recorta. El tope de líneas de un diálogo existe para
        // una lista de rutas de la que sobra ver una parte; aquí la lista ES
        // la concesión, y enseñar dieciséis de cuarenta mientras el sí
        // concede las cuarenta es exactamente el hueco por el que se cuela la
        // capability que nadie leyó. Si son tantas que no caben, no se
        // pregunta: se rehúsa.
        if capabilities.len() > MAX_CAPABILIDADES {
            let fuera = self.decir("host-extension-too-many-caps");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-extension-too-many-caps".to_owned(),
                },
                fuera,
            );
        }
        let mut cuerpo = vec![crate::dto::DialogLine {
            text: nombre.0,
            hostile: nombre.1,
        }];
        cuerpo.extend(
            capabilities
                .iter()
                .cloned()
                .map(|(text, hostile)| crate::dto::DialogLine { text, hostile }),
        );
        let nota = String::new();
        let modal = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id: modal,
            title_key: "modal-extension-approve-title".to_owned(),
            destination: None,
            // El id reverse-DNS, que es lo ÚNICO que el core valida: dos
            // extensiones pueden llamarse igual, y el nombre que el diálogo
            // enseña lo escribe el manifiesto. Sin esto, la pantalla donde se
            // conceden permisos no dice a quién.
            subject: Some(crate::dto::DialogLine {
                text: clamp_display(id.to_owned()),
                hostile: false,
            }),
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: cuerpo,
            overflow_note: nota,
            choices: vec![
                DialogChoice {
                    id: "approve".to_owned(),
                    label_key: "dialog-approve".to_owned(),
                    // Conceder permisos no borra nada, pero tampoco es la
                    // respuesta inocua de un diálogo cualquiera: se marca
                    // para que el renderer no la pinte como el «Aceptar» de
                    // un aviso.
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
            id: modal,
            vista: vista.clone(),
            input_crudo: String::new(),
            reconocido: true,
            al_confirmar: Some(Pendiente::AprobarExtension {
                id: id.to_owned(),
                capabilities: capabilities.into_iter().map(|(t, _)| t).collect(),
                digest: ancla,
            }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Concede las capabilities LEÍDAS, o vuelve a preguntar si han cambiado.
    ///
    /// El diálogo se queda las TECLAS, no los mensajes de fondo: un catálogo
    /// que aterrice entre la pregunta y el sí puede traer otras capabilities
    /// para esa extensión, y entonces el sí concedería algo que nadie leyó.
    fn conceder(
        &mut self,
        id: &str,
        leidas: &[String],
        ancla_leida: Option<String>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let ahora = self
            .extensiones
            .as_ref()
            .and_then(|e| e.concesion(id))
            .map(|c| {
                c.capabilities
                    .into_iter()
                    .map(|(t, _)| t)
                    .collect::<Vec<_>>()
            });
        if ahora.as_deref() == Some(leidas) {
            // El ancla que viaja es la de LA PREGUNTA, jamás la del catálogo
            // de ahora (#282): releerla aquí certificaría al core «esto es lo
            // que el humano leyó» sobre lo que el humano no leyó, que es
            // exactamente el agujero que el campo cierra. Y la comparación de
            // capabilities de arriba no lo tapa: `category` y `contributions`
            // entran en el ancla y no en la lista pintada.
            return (
                None,
                self.gobernar(id, Gobierno::Aprobar(true, ancla_leida), backend, buzon),
            );
        }
        let mut fuera = self.decir("host-extension-changed");
        let (_, partes) = self.preguntar_por_aprobacion(id);
        fuera.extend(partes);
        (Some("host-extension-changed"), fuera)
    }

    /// Manda el cambio al daemon. La verdad la dirá el catálogo repedido.
    fn gobernar(
        &mut self,
        id: &str,
        que: Gobierno,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let apertura = self.gen_extensiones;
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        let id2 = id.to_owned();
        tokio::spawn(async move {
            let llamada = match que {
                Gobierno::Aprobar(v, digest) => backend2.plugin_set_approval(id2, v, digest),
                Gobierno::Encender(v) => backend2.plugin_set_enabled(id2, v),
            };
            let res = match tokio::time::timeout(PLAZO_PLUGINS, llamada).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon2
                .send(Mensaje::Fondo(Box::new(Fondo::Gobernada(apertura, res))))
                .await;
        });
        Vec::new()
    }

    /// El cambio de gobierno contestó.
    ///
    /// Con un OK NO se toca el `bool` local: se REPIDE el catálogo. Un
    /// optimismo que el daemon no confirmó es, en esta pantalla, una
    /// afirmación sobre quién puede leer tus ficheros.
    fn aplicar_gobierno(
        &mut self,
        apertura: u64,
        res: &Result<(), Error>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if apertura != self.gen_extensiones {
            return Vec::new();
        }
        // El desenlace se DICE aunque el gestor ya esté cerrado: una
        // concesión que falló y nadie contó es la ventana callándose sobre
        // quién puede leer tus ficheros.
        let fuera = match res {
            Ok(()) => self.decir("host-extension-updated"),
            Err(e) => self.decir(norte_frontend::error::error_key(e)),
        };
        // Y el catálogo se repide EN LOS DOS CASOS. El fallo incluye el plazo
        // de ESTE lado, que no es «no pasó» sino «no se sabe»: el daemon pudo
        // conceder las capabilities y tardar en contestar, y entonces dejar
        // la fila diciendo «sin aprobar» es la misma mentira que el optimismo
        // local, en pesimista. Lo único que resuelve un desconocido es ir a
        // preguntar.
        if self.extensiones.is_some() {
            self.repedir_catalogo(backend, buzon);
        }
        fuera
    }

    /// Vuelve a pedir el catálogo para la apertura VIVA.
    fn repedir_catalogo(&mut self, backend: &Arc<dyn HostBackend>, buzon: &mpsc::Sender<Mensaje>) {
        self.pedir_catalogo_de_extensiones(backend, buzon);
    }

    /// Pide el catálogo para el gestor, numerando la PETICIÓN.
    ///
    /// Dos números y no uno: la APERTURA dice si el gestor sigue siendo el
    /// mismo, y la PETICIÓN cuál de varias en vuelo es la más nueva. Dos
    /// gobiernos seguidos piden dos catálogos dentro de la misma apertura, y
    /// pueden contestar en cualquier orden — sin el segundo número, el viejo
    /// pisaba al nuevo y la columna «aprobada» se quedaba atrás para siempre.
    fn pedir_catalogo_de_extensiones(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let apertura = self.gen_extensiones;
        self.gen_catalogo += 1;
        let peticion = self.gen_catalogo;
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(PLAZO_PLUGINS, backend.plugin_list()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::Catalogo(
                    apertura, peticion, res,
                ))))
                .await;
        });
    }

    /// La salida de un comando llegó.
    ///
    /// Solo la del ÚLTIMO que se lanzó: dos comandos en vuelo y el lento
    /// aterrizando después pintaría la salida de uno bajo el título del
    /// otro, que en un panel que dice quién imprimió qué es mentir.
    fn aplicar_salida(
        &mut self,
        apertura: u64,
        datos: SalidaPedida,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let SalidaPedida {
            id,
            plugin,
            comando,
            res,
        } = datos;
        if apertura != self.gen_salida {
            return Vec::new();
        }
        match res {
            Ok(texto) => {
                // Texto de TERCERO: se ACOTA primero —enmascarar un megabyte
                // para quedarse con cuatro mil caracteres es hacer el trabajo
                // entero por nada—, se parte en líneas, y cada una se
                // enmascara por su cuenta. Que se haya cortado se DICE: el
                // receptor no puede deducirlo, porque lo que le llega ya
                // viene corto.
                let recortado: String = texto.chars().take(MAX_SALIDA).collect();
                let mut truncado = texto.chars().nth(MAX_SALIDA).is_some();
                let mut lineas = Vec::new();
                let mut hostil = false;
                for linea in recortado.lines().take(MAX_SALIDA_LINEAS) {
                    let (pintable, marcada) = norte_frontend::display_name(linea.as_bytes());
                    hostil |= marcada;
                    lineas.push(clamp_display(pintable));
                }
                truncado |= recortado.lines().nth(MAX_SALIDA_LINEAS).is_some();
                self.escritorio.salida = Some(crate::dto::ExtensionOutputView {
                    plugin: crate::dto::MaskedTextView {
                        text: plugin.0,
                        hostile: plugin.1,
                    },
                    plugin_id: id,
                    command: crate::dto::MaskedTextView {
                        text: comando.0,
                        hostile: comando.1,
                    },
                    lines: lineas,
                    text_hostile: hostil,
                    truncated: truncado,
                });
                let cambio = ViewChange::PluginOutput {
                    output: self.escritorio.salida.clone(),
                };
                vec![self.parche(vec![cambio])]
            }
            Err(e) => self.decir(norte_frontend::error::error_key(&e)),
        }
    }

    /// Cierra el panel de salida.
    fn cerrar_salida(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.escritorio.salida = None;
        (
            self.aplicada(),
            vec![self.parche(vec![ViewChange::PluginOutput { output: None }])],
        )
    }

    /// Un click en una fila del gestor: la elige.
    fn elegir_extension(
        &mut self,
        row: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(e) = self.extensiones.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        e.senalar(row as usize);
        let (_, mut envios) = self.pedir_ficha(backend, buzon);
        let cambio = ViewChange::Extensions {
            extensions: self.vista_extensiones(),
        };
        envios.push(self.parche(vec![cambio]));
        (self.aplicada(), envios)
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
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::PluginsDeAyuda(res))))
                .await;
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
            norte_client::ConnEvent::Restored => (ConnectionView::Connected, "msg-daemon-restored"),
            // El daemon avisa ANTES de cerrar, y esto es lo único que
            // distingue un relevo de una parada: en cuanto la conexión caiga,
            // las dos se ven igual. Se queda como aviso PERSISTENTE porque
            // sigue siendo verdad mientras dure, y la vista de conexión no se
            // toca todavía — la conexión, ahora mismo, sigue en pie.
            norte_client::ConnEvent::GoingAway { reconnect } => {
                self.aviso_de_daemon = Some(if reconnect {
                    "msg-daemon-handover"
                } else {
                    "msg-daemon-stopping"
                });
                let clave = self.aviso_de_daemon.unwrap_or("msg-daemon-stopping");
                let banners = self.cambio_de_banners();
                let parche = self.parche(vec![banners]);
                let aviso = self.sobre(UiUpdate::Notice(UiNotice::Message {
                    key: clave.to_owned(),
                    detail: None,
                }));
                return vec![parche, aviso];
            }
            // `Lost` y el comodín juntos: `ConnEvent` es NO EXHAUSTIVO, y un
            // evento de un SDK más nuevo se lee como una pérdida, que es lo
            // conservador — se pinta reconectando en vez de fingir que todo
            // sigue igual.
            norte_client::ConnEvent::Lost | _ => (ConnectionView::Reconnecting, "msg-daemon-lost"),
        };
        // Volver APAGA el aviso: uno que no sabe volverse «ya está» miente en
        // cuanto el daemon reaparece, y el relevo termina volviendo.
        // Y estrena ÉPOCA: al otro lado puede haber un daemon NUEVO, con su
        // contador de ids desde 1. Lo que quede en el tablero con esos
        // números es de antes, y a partir de aquí no se hereda nada suyo.
        if matches!(ev, norte_client::ConnEvent::Restored) {
            self.aviso_de_daemon = None;
            self.epoca_conexion = self.epoca_conexion.saturating_add(1);
            // Un plan pedido al daemon ANTERIOR no lo va a contestar el
            // nuevo: su id empieza otra vez en 1, y dejar la petición colgada
            // haría que el panel se abriera con la Task de otro.
            if let Some(pedida) = self.sync_pedida.take() {
                pedida
                    .abandonada
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        self.conexion = vista.clone();
        let banners = self.cambio_de_banners();
        let parche = self.parche(vec![ViewChange::Connection(vista), banners]);
        let aviso = self.sobre(UiUpdate::Notice(UiNotice::Message {
            key: clave.to_owned(),
            detail: None,
        }));
        vec![parche, aviso]
    }

    /// Una sesión de provider viaja sin cifrar (#44): se apunta y se dice.
    ///
    /// Persistente y no efímero: un mensaje que borra la siguiente tecla no
    /// puede describir cómo viaja lo que se está mirando. La frase y el techo
    /// los pone `norte_frontend::banners`, que es el MISMO código que compone
    /// el aviso del TUI.
    fn sesion_degradada(
        &mut self,
        d: norte_proto::methods::ConnectionDegraded,
    ) -> BridgeEnvelope<UiUpdate> {
        self.degradadas.note(d);
        let cambio = self.cambio_de_banners();
        self.parche(vec![cambio])
    }

    /// Recompone los avisos persistentes de la barra y devuelve su cambio.
    ///
    /// UN sitio para los tres, y en este orden: el journal habla de TODA la
    /// sesión y de si algo se puede deshacer, el daemon de si esta ventana
    /// va a seguir sirviendo, y la degradación de cómo viaja una conexión.
    /// Elegir uno solo escondería los otros para siempre, que es justo lo
    /// que el TUI ya decidió no hacer.
    fn cambio_de_banners(&mut self) -> ViewChange {
        let frase = |clave: &str| crate::dto::BannerView {
            text: clamp_display(norte_i18n::t_in(self.lang, clave)),
            subject: None,
        };
        let mut banners = Vec::new();
        if self.journal_rehusado {
            banners.push(frase("status-journal-refused"));
        }
        if let Some(clave) = self.aviso_de_daemon {
            banners.push(frase(clave));
        }
        if let Some(aviso) = self.degradadas.banner(self.lang) {
            // La conexión va en su propio campo, jamás dentro de la frase:
            // ver el rustdoc de `connection_banner`.
            banners.push(crate::dto::BannerView {
                text: clamp_display(aviso.text),
                subject: Some(crate::dto::BannerSubjectView {
                    scheme: clamp_display(aviso.scheme),
                    host: clamp_display(aviso.host),
                    reason: clamp_display(aviso.reason),
                    detail: aviso.detail.map(clamp_display),
                    hostile: aviso.hostile,
                }),
            });
        }
        self.status.banners = banners;
        ViewChange::Status(self.status.clone())
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
                .send(Mensaje::Fondo(Box::new(Fondo::PaginaDePlugin(id, res))))
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
                // Una transferencia comparte página con el borrado: las dos
                // son «la pregunta que hay que responder antes de que algo
                // cambie», y el corpus tiene UNA que habla de eso.
                // La colisión tiene su PROPIA página en el corpus: sus teclas
                // son otras (`dialog.overwrite`, `dialog.skip`…), y mandar al
                // lector a la de confirmar le enseñaría las que no valen.
                Some(Pendiente::Reintentar { .. }) => "dialog.collision",
                Some(Pendiente::Borrar { .. } | Pendiente::Transferir { .. }) => "dialog.confirm",
                Some(
                    Pendiente::Decidir { .. }
                    | Pendiente::AprobarExtension { .. }
                    | Pendiente::DeshacerSesion { .. },
                ) => "dialog.approval",
                // Buscar comparte página con crear: los dos son el diálogo
                // que pide que teclees un nombre, y el corpus tiene UNA que
                // habla de eso.
                // Renombrar comparte página con crear y con buscar: los tres
                // son el diálogo que pide que teclees algo, y el corpus tiene
                // UNA que habla de eso.
                Some(Pendiente::InstruccionIa { .. }) => "dialog.ai-rename",
                Some(
                    Pendiente::CrearDirectorio { .. }
                    | Pendiente::Buscar { .. }
                    | Pendiente::Renombrar { .. }
                    // La consulta semántica es otro diálogo que pide que
                    // teclees algo, y el corpus tiene UNA página que habla
                    // de eso.
                    | Pendiente::ConsultaSemantica
                    // Marcar por patrón, igual: un diálogo que pide que
                    // teclees algo.
                    | Pendiente::Patron { .. }
                    // Y empaquetar: lo que se teclea es el nombre del
                    // contenedor, de donde sale el formato.
                    | Pendiente::Empaquetar { .. }
                    // Partir pide un TAMAÑO, pero es el mismo diálogo de un
                    // campo de texto y una confirmación.
                    | Pendiente::Partir { .. },
                ) => "dialog.mkdir",
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

    /// Re-congela los hechos de la ayuda si está abierta, y devuelve su
    /// parche (#262).
    ///
    /// El congelado de `abrir_ayuda` es contra que se mueva el LECTOR, no
    /// contra que se mueva el mundo. Dos de los hechos —`enterable` y
    /// `viewable`— describen la entrada bajo el cursor, y una copia o un
    /// borrado que terminan con la ayuda delante re-listan el panel por
    /// debajo: la frase de motivo se quedaba explicando por qué no aplica a
    /// una selección que ya no existe. No había despacho incorrecto
    /// —`activar_en_ayuda` vuelve a preguntar antes de correr—, pero una
    /// pantalla que explica algo falso es una pantalla que miente.
    fn recongelar_ayuda(&mut self) -> Option<BridgeEnvelope<UiUpdate>> {
        if !self.recongelar_hechos_de_ayuda() {
            return None;
        }
        let cambio = ViewChange::Help {
            help: self.vista_ayuda(),
        };
        Some(self.parche(vec![cambio]))
    }

    /// Re-congela y NO fabrica parche. Para los caminos que ya mandan una
    /// foto: `parche` gasta un número de secuencia, y tirar el sobre después
    /// de gastarlo deja un HUECO en la secuencia — que es exactamente la
    /// condición que obliga al renderer a pedir una foto entera.
    ///
    /// Devuelve si la ayuda estaba abierta Y sus hechos han CAMBIADO.
    fn recongelar_hechos_de_ayuda(&mut self) -> bool {
        if self.ayuda.is_none() {
            return false;
        }
        let hechos = self.hechos();
        self.ayuda.as_mut().is_some_and(|a| a.recongelar(hechos))
    }

    /// Una parte del detalle de un veredicto, en LÍNEAS separadas (#273).
    ///
    /// La causa y el nombre van en líneas distintas, que es la forma que
    /// tiene esta superficie de separarlos FUERA de banda: componerlos en
    /// una sola dejaba que un fichero llamado `✗ 4. ya existe: otro.txt`
    /// fabricara una entrada de la lista que no existe. El nombre viaja solo
    /// y con su marca, que es lo único que un tercero controla.
    fn lineas_de_detalle(&self, parte: &norte_frontend::DetailPart) -> Vec<crate::dto::DialogLine> {
        use norte_frontend::DetailPart;
        let plana = |texto: String| crate::dto::DialogLine {
            text: clamp_display(texto),
            hostile: false,
        };
        match parte {
            DetailPart::Temp { count } => vec![plana(norte_i18n::ta_in(
                self.lang,
                "modal-rename-batch-temp",
                &[("n", &count.to_string())],
            ))],
            DetailPart::Collision {
                index,
                kind_key,
                name,
                hostile,
            } => {
                let kind = norte_i18n::t_in(self.lang, kind_key);
                let causa = match index {
                    Some(n) => norte_i18n::ta_in(
                        self.lang,
                        "modal-rename-batch-collision-prefix",
                        &[("n", &n.to_string()), ("kind", &kind)],
                    ),
                    None => norte_i18n::ta_in(
                        self.lang,
                        "modal-rename-batch-collision-prefix-unindexed",
                        &[("kind", &kind)],
                    ),
                };
                vec![
                    plana(causa),
                    crate::dto::DialogLine {
                        text: clamp_display(name.clone()),
                        hostile: *hostile,
                    },
                ]
            }
            DetailPart::More {
                shown,
                total,
                hostile,
            } => vec![crate::dto::DialogLine {
                text: clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "modal-rename-batch-collision-more",
                    &[("shown", &shown.to_string()), ("total", &total.to_string())],
                )),
                // El resumen no lleva nombre, pero SÍ la marca de que alguna
                // de las ocultas lo tiene hostil: lo escondido no se cuela
                // limpio.
                hostile: *hostile,
            }],
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
            self.pedir_filas_de_plugin(backend, buzon);
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
            // La página la da el MODELO, que sabe lo que significa en la
            // LATERAL: camina por los temas y enseña uno, en vez de diez
            // transiciones de página por tecla.
            //
            // En el CUERPO no. El cuerpo de una página cruza el puente
            // entero y quien lo desplaza es el DOM, que es lo que un
            // renderer con scroll nativo hace bien y sin preguntar; el
            // renderer ni siquiera manda estas teclas cuando el cuerpo tiene
            // el foco. Moverlo aquí crearía una SEGUNDA verdad sobre por
            // dónde va la ayuda —el `scrollTop` del DOM y el `body_scroll`
            // del modelo— y solo una de las dos se pinta (#267). El modelo
            // conserva su paginación de cuerpo porque el TUI la usa: ahí no
            // hay scroll nativo que delegar.
            "PageDown" | "pgdn" if a.estado.focus() == norte_frontend::help::Focus::Topics => {
                a.estado.page_down(PAGINA_DE_AYUDA);
            }
            "PageUp" | "pgup" if a.estado.focus() == norte_frontend::help::Focus::Topics => {
                a.estado.page_up(PAGINA_DE_AYUDA);
            }
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
                self.visor_token = None;
                // La imagen se SUELTA al cerrar: son megas, y un visor
                // cerrado no tiene nada que enseñar.
                self.imagen = None;
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
            // Y se le pregunta a los plugins. Un previewer que falla, que
            // tarda o que no aplica NO es un error: el visor cae a la vista
            // cruda, que es lo que el TUI ya hace. Un plugin no puede dejar
            // un fichero sin poder mirarse.
            let preview = match tokio::time::timeout(
                PLAZO_PLUGINS,
                backend.plugin_preview_styled(path.clone()),
            )
            .await
            {
                Ok(Ok(p)) => p,
                _ => None,
            };
            let _ = buzon
                .send(Mensaje::Contenido(Box::new((token, path, leido, preview))))
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
        preview: Option<norte_proto::methods::PluginPreviewStyled>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        if self.visor_en_vuelo != Some(token) {
            // El usuario cerró el visor, pidió otro fichero o se fue a otro
            // sitio mientras esto volaba. Abrirlo ahora sería abrir una
            // ventana que nadie ha pedido — y cambiarle el teclado de mapa.
            return None;
        }
        self.visor_en_vuelo = None;
        self.visor_token = Some(token);
        // Un visor nuevo: la imagen del anterior sobra. Y hay que soltarla,
        // no solo dejar de pintarla: son megas.
        self.imagen = None;
        match leido {
            Ok(mut bytes) => {
                let cap = usize::try_from(VISOR_CAP).unwrap_or(usize::MAX);
                let truncado = bytes.len() > cap;
                if truncado {
                    bytes.truncate(cap);
                }
                let ruta = path.clone();
                self.visor = Some(match preview {
                    // Un previewer aplicó: se enseña LO SUYO. Los bytes ya
                    // leídos no se tiran —hicieron falta para saber que el
                    // fichero se puede leer— pero no se pintan: pintar las
                    // dos cosas sería enseñar el mismo fichero dos veces.
                    Some(p) => norte_frontend::viewer::Viewer::with_plugin_preview_styled(
                        path,
                        p.plugin_name,
                        &p.lines,
                        p.lossy,
                    ),
                    None => norte_frontend::viewer::Viewer::new(path, bytes, truncado),
                });
                self.pedir_imagen(&ruta, token, backend, buzon);
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

    /// Trae los bytes ENTEROS de la imagen, si el visor tiene una aceptada.
    ///
    /// La cabecera ya se leyó con el visor y ya dijo que sí; esto trae el
    /// resto. Si el fichero cabía en lo que se leyó no hay segundo viaje: los
    /// bytes ya están.
    ///
    /// El tope es una NEGATIVA, no un recorte. Media imagen decodificada es
    /// una imagen de otra cosa, así que un fichero por encima de
    /// [`IMAGEN_CAP`] no se pinta y se dice.
    fn pedir_imagen(
        &mut self,
        path: &VPath,
        token: RequestToken,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let Some(v) = self.visor.as_ref() else {
            return;
        };
        if !matches!(Self::imagen_de(v), Ok(Some(_))) {
            return;
        }
        if !v.truncated {
            // Cabía entera en la lectura del visor: no hay nada que pedir.
            self.imagen = v.image_bytes().map(|b| std::sync::Arc::new(b.to_vec()));
            return;
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let path = path.clone();
        tokio::spawn(async move {
            // Un byte de más que el tope: es lo que delata que no cabe.
            let lectura = backend.read(
                path,
                Some(norte_proto::ByteRange {
                    offset: 0,
                    len: Some(IMAGEN_CAP + 1),
                }),
            );
            let leido = match tokio::time::timeout(PLAZO_VISOR, lectura).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::Imagen(token, leido))))
                .await;
        });
    }

    /// Los bytes de la imagen, llegados.
    ///
    /// Se descartan si el visor ya es otro: pintar la foto anterior sobre el
    /// fichero de ahora es la misma clase de error que abrir un visor que
    /// nadie pidió.
    fn aplicar_imagen(
        &mut self,
        token: RequestToken,
        leido: Result<Vec<u8>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        if self.visor_token != Some(token) {
            return None;
        }
        let Ok(bytes) = leido else {
            return None;
        };
        if bytes.len() as u64 > IMAGEN_CAP {
            // No cabe. Se dice y se enseña la vista cruda: enseñarla a medias
            // sería enseñar otra imagen.
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "viewer-image-too-large",
            )));
            let cambio = ViewChange::Status(self.status.clone());
            return Some(self.parche(vec![cambio]));
        }
        self.imagen = Some(std::sync::Arc::new(bytes));
        let cambio = ViewChange::Viewer {
            viewer: self.vista_visor(),
        };
        Some(self.parche(vec![cambio]))
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
        // El foco puede estar en un panel que NO es un listado y que SÍ toma
        // teclas —hoy, el de procesos—. Entonces «bajar» es bajar por ÉL:
        // hasta ahora el rol `active` lo pintaba enfocado y las flechas movían
        // el listado de al lado, que es media función y la mitad que no se ve.
        //
        // Se decide por EFECTO y no por tecla, así que `j`, `↓` y `g g`
        // funcionan igual: el keymap dice qué comando es, y la superficie con
        // el foco dice qué significa ahí.
        // Cancelar se decide ANTES que nada: con el panel de procesos
        // enfocado y el tablero vacío, `efecto_en_panel_enfocado` responde
        // «aplicado» a cualquier efecto, y eso convertiría un «no hay nada
        // que parar» en un silencio.
        if matches!(efecto, Efecto::CancelarTask) {
            return self.cancelar_por_comando();
        }
        // Recorrer y descartar el tablero, por el mismo motivo y antes del
        // foco: son comandos del TABLERO, no del panel que lo pinta, y con el
        // panel de procesos cerrado tienen que seguir significando lo mismo.
        if let Efecto::TaskVecina { atras } = efecto {
            return self.mover_en_tablero(atras);
        }
        if matches!(efecto, Efecto::DescartarTask) {
            return self.descartar_task();
        }
        if let Some(salida) = self.efecto_en_panel_enfocado(efecto) {
            return salida;
        }
        if self.sitios_tienen_el_foco() && matches!(efecto, Efecto::Entrar | Efecto::Marcar) {
            // Entrar y plegar los atiende la barra lateral, y el `cd` que
            // salga va al LISTADO por el mismo camino que cualquier otro: es
            // lo que hace que tenerla abierta no cambie a dónde van las
            // operaciones.
            return self.activar_sitio_del_cursor(backend, buzon);
        }
        let slot = self.activo();
        match efecto {
            Efecto::Cursor(_)
            | Efecto::Pagina(_)
            | Efecto::Extremo { .. }
            | Efecto::Entrar
            | Efecto::Subir
            | Efecto::Rastro { .. }
            | Efecto::Marcar
            | Efecto::MarcarTodo
            | Efecto::InvertirMarcas
            | Efecto::DesmarcarTodo => self.efecto_de_listado(efecto, slot, backend, buzon),
            Efecto::Foco { atras } => self.mover_foco(atras),
            Efecto::Destino => self.designar_destino(),
            // Atendido arriba, antes del panel enfocado. El brazo existe
            // porque el `match` es exhaustivo a propósito: un efecto nuevo
            // sin sitio tiene que ser un error de compilación.
            // Los tres del TABLERO se atienden antes de llegar aquí: no
            // dependen del panel que tenga el foco.
            Efecto::CancelarTask | Efecto::TaskVecina { .. } | Efecto::DescartarTask => {
                self.cancelar_por_comando()
            }
            Efecto::Tamano(_)
            | Efecto::Igualar
            | Efecto::Disposiciones
            | Efecto::Partir { .. }
            | Efecto::CerrarHueco
            | Efecto::AlternarHueco { .. }
            | Efecto::PestanaNueva
            | Efecto::CerrarPestana
            | Efecto::CiclarPestana { .. }
            | Efecto::MoverPestana { .. }
            | Efecto::IrAPestana { .. } => {
                self.efecto_de_disposicion(efecto, backend, buzon)
            }
            Efecto::Ordenar(col) => self.ordenar_por_columna(slot, col),
            Efecto::Refrescar => self.refrescar_visibles(backend, buzon),
            Efecto::AlternarOcultos => self.alternar_ocultos(),
            Efecto::CiclarEncoding => self.ciclar_encoding(),
            Efecto::Espejo | Efecto::Traer | Efecto::Intercambiar => {
                self.gesto_de_panel(efecto, backend, buzon)
            }
            Efecto::Historial => self.abrir_historial(),
            Efecto::Hotlist => self.abrir_hotlist(),
            Efecto::VolumenesDeLado { derecha } => {
                self.abrir_volumenes_de_lado(derecha, backend, buzon)
            }
            Efecto::Columnas => self.abrir_columnas(),
            Efecto::Buscar => self.pedir_busqueda(),
            Efecto::BuscarRapido => self.buscar_rapido(),
            Efecto::CrearDirectorio
            | Efecto::Borrar { .. }
            | Efecto::Transferir { .. }
            | Efecto::Renombrar
            | Efecto::RenameIa
            | Efecto::BuscarSemantica
            | Efecto::Sincronizar
            // Los dos que LANZAN un proceso: lo que ese proceso haga con los
            // ficheros no lo decide esta ventana.
            | Efecto::AbrirExterno
            | Efecto::Terminal
                if self.efectos == crate::commands::Efectos::SoloLectura =>
            {
                Self::no_muta()
            }
            // Copiar la ruta no toca nada y va en los dos modos: poner texto
            // en el portapapeles es tan de solo mirar como leer un nombre.
            Efecto::CopiarRuta => self.copiar_rutas(),
            Efecto::MarcarPatron { marcar } => self.pedir_patron(marcar),
            Efecto::AbrirExterno => self.abrir_externo(),
            Efecto::Terminal => self.abrir_terminal(),
            Efecto::Comparar => self.pedir_comparacion(backend, buzon),
            Efecto::TamanoDeDirectorio
            | Efecto::Empaquetar
            | Efecto::Desempaquetar
            | Efecto::ComprobarArchivo
            | Efecto::PartirFichero
            | Efecto::Juntar => self.efecto_sobre_entradas(efecto, backend, buzon),
            // Como comparar: necesita el backend porque sale a preguntar en
            // cuanto se abre, y el panel nace diciendo que planifica.
            Efecto::Sincronizar => self.pedir_sincronizacion(backend, buzon),
            Efecto::Paleta
            | Efecto::Ayuda
            | Efecto::Ajustes
            | Efecto::Extensiones
            | Efecto::Agentes
            | Efecto::Tema
            | Efecto::Volumenes
            | Efecto::Ver => self.efecto_que_abre(efecto, backend, buzon),
            Efecto::CrearDirectorio
            | Efecto::Borrar { .. }
            | Efecto::Transferir { .. }
            | Efecto::Renombrar
            | Efecto::RenameIa
            | Efecto::BuscarSemantica => self.efecto_que_muta(efecto),
        }
    }

    /// Los efectos que mueven el CURSOR o el listado: recorrer, entrar,
    /// subir, volver y marcar. Nada de esto escribe.
    fn efecto_de_listado(
        &mut self,
        efecto: Efecto,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
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
            Efecto::MarcarTodo => {
                self.hueco_mut().pane.mark_all();
                (self.aplicada(), vec![self.parche_filas()])
            }
            Efecto::InvertirMarcas => {
                self.hueco_mut().pane.invert_marks();
                (self.aplicada(), vec![self.parche_filas()])
            }
            // Los demás no llegan aquí: el `match` de arriba los reparte.
            _ => Self::no_muta(),
        }
    }

    /// Manda un efecto NATIVO al proceso que hospeda, si hay alguien.
    ///
    /// `false` = nadie escucha. No es un error del host: un frontend que no
    /// sabe hacer estas cosas no se suscribe, y entonces lo honesto es
    /// decirle a quien pulsó que aquí eso no pasa, en vez de acusar recibo de
    /// algo que no va a ocurrir.
    fn nativo(&self, efecto: crate::dto::NativeEffect) -> bool {
        self.escritorio
            .nativos
            .as_ref()
            .is_some_and(|tx| tx.send(efecto).is_ok())
    }

    /// Las rutas de lo MARCADO —o de lo señalado, si no hay marcas— al
    /// portapapeles.
    ///
    /// Marcado primero y cursor como respaldo: es la misma regla que copiar y
    /// mover, y tener dos respuestas a «sobre qué actúa esto» según el
    /// comando es lo que hace que un gesto se aplique a otra cosa.
    ///
    /// En BYTES y en forma nativa cuando la hay: lo que se pega tiene que
    /// abrir el mismo fichero, y una ruta decodificada con pérdida abre otro.
    fn copiar_rutas(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let hueco = self.hueco();
        // `marked_paths` ya cae al cursor cuando no hay marcas: es la misma
        // regla que copiar y mover, y tener dos respuestas a «sobre qué
        // actúa esto» según el comando es lo que aplica un gesto a otra cosa.
        let paths: Vec<VPath> = hueco.pane.marked_paths();
        if paths.is_empty() {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-nothing-selected".to_owned(),
                },
                Vec::new(),
            );
        }
        let count = paths.len();
        let bytes = norte_frontend::shell::clipboard_bytes(&paths);
        if !self.nativo(crate::dto::NativeEffect::CopyBytes { bytes, count }) {
            return Self::sin_escritorio();
        }
        let fuera = self.decir_con("msg-paths-copied", &[("n", &count.to_string())]);
        (self.aplicada(), fuera)
    }

    /// Abre lo señalado con la aplicación que el escritorio elija.
    fn abrir_externo(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(path) = self.hueco().pane.selected().map(|e| e.path.clone()) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-nothing-selected".to_owned(),
                },
                Vec::new(),
            );
        };
        // Solo lo que está en ESTE disco: a `xdg-open` no se le puede dar un
        // `sftp://`, y fingir que sí abriría otra cosa —o nada— sin decirlo.
        if !norte_frontend::shell::is_local(&path) {
            let fuera = self.decir("host-not-local");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-not-local".to_owned(),
                },
                fuera,
            );
        }
        if !self.nativo(crate::dto::NativeEffect::OpenPath { path }) {
            return Self::sin_escritorio();
        }
        (self.aplicada(), self.decir("msg-opening-external"))
    }

    /// Abre un terminal sentado en el directorio del panel activo.
    fn abrir_terminal(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let dir = self.hueco().pane.dir().clone();
        if !norte_frontend::shell::is_local(&dir) {
            // Un terminal se sienta en un directorio del sistema de ficheros:
            // en un `sftp://` no hay dónde sentarlo, y abrirlo en el `$HOME`
            // sin decir nada sería abrirlo en otro sitio.
            let fuera = self.decir("host-not-local");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-not-local".to_owned(),
                },
                fuera,
            );
        }
        if !self.nativo(crate::dto::NativeEffect::OpenTerminal { dir }) {
            return Self::sin_escritorio();
        }
        (self.aplicada(), self.decir("msg-opening-terminal"))
    }

    /// Nadie escucha los efectos nativos: se DICE.
    fn sin_escritorio() -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        (
            ActionAck::Unavailable {
                reason_key: "host-no-desktop".to_owned(),
            },
            Vec::new(),
        )
    }

    /// Los efectos que abren una PANTALLA sobre el listado y no tocan nada.
    fn efecto_que_abre(
        &mut self,
        efecto: Efecto,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match efecto {
            Efecto::Paleta => self.abrir_paleta(backend, buzon),
            Efecto::Ayuda => self.abrir_ayuda(backend, buzon),
            Efecto::Ajustes => self.abrir_ajustes(),
            Efecto::Extensiones => self.abrir_extensiones(backend, buzon),
            Efecto::Agentes => self.abrir_agentes(),
            Efecto::Tema => self.abrir_tema(),
            Efecto::Volumenes => self.abrir_volumenes(backend, buzon),
            Efecto::Ver => self.pedir_visor(backend, buzon),
            // Los demás no llegan aquí: el `match` de arriba los reparte.
            _ => Self::no_muta(),
        }
    }

    /// Los efectos que ESCRIBEN. Ninguno muta aquí: los cinco abren la
    /// pregunta por la que pasa la mutación, que es la única puerta.
    fn efecto_que_muta(&mut self, efecto: Efecto) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match efecto {
            Efecto::CrearDirectorio => self.pedir_mkdir(),
            Efecto::Borrar { permanente } => self.pedir_borrado(permanente),
            Efecto::Transferir { mover } => self.pedir_transferencia(mover),
            Efecto::Renombrar => self.pedir_rename(),
            Efecto::RenameIa => self.pedir_instruccion_ia(),
            Efecto::BuscarSemantica => self.pedir_consulta_semantica(),
            // Los demás no llegan aquí: el `match` de arriba los reparte.
            _ => Self::no_muta(),
        }
    }

    /// El desenlace de la Task de una comparación entra en el modelo.
    ///
    /// Lo traduce `finish_from_task`, que es donde vive la diferencia que
    /// importa: «terminó» no es lo mismo que «terminó y llegó todo». Una
    /// comparación a la que se le perdieron lotes se lee INCOMPLETA, y una
    /// cuyo canal se cerró sin desenlace observado se lee DESCONOCIDA — dos
    /// estados que el CLI y el MCP ya perdieron cada uno por su cuenta.
    /// La Task del APPLY terminó: se pide su informe.
    ///
    /// El desenlace de la Task dice si corrió; lo que se hizo y lo que NO lo
    /// cuenta el informe, y sin él «terminó» se lee como «salió bien» sobre
    /// un destino que puede haber quedado a medias.
    fn pedir_informe_de_sync(
        &mut self,
        p: &norte_proto::TaskProgress,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let Some(sinc) = self.sincronizacion.as_ref() else {
            return;
        };
        // Los MISMOS tres guards que el informe de un lote, y por los mismos
        // motivos: la CLASE (un `fs.copy` cualquiera puede llevar el mismo id
        // tras un relevo), la ÉPOCA de conexión (los ids del daemon nuevo
        // empiezan otra vez en 1) y la idempotencia (una reconexión reanuncia
        // el terminal, y esto es una RPC).
        if sinc.task != p.task_id
            || sinc.epoca_conexion != self.epoca_conexion
            || !matches!(p.kind, norte_proto::TaskKind::Sync)
            || sinc.informe_pedido
            || !matches!(
                sinc.vista.state,
                norte_frontend::sync::SyncState::Applying(_)
            )
        {
            return;
        }
        let epoca = sinc.epoca;
        let estado = p.state.clone();
        let id = p.task_id;
        if let Some(s) = self.sincronizacion.as_mut() {
            s.informe_pedido = true;
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let informe = backend.sync_report(id).await;
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::InformeDeSync(
                    epoca,
                    estado,
                    Box::new(informe),
                ))))
                .await;
        });
    }

    /// El informe llegó: entra en el modelo, que decide qué frase sale.
    fn informe_de_sync(
        &mut self,
        epoca: u64,
        estado: &norte_proto::TaskState,
        informe: Result<norte_proto::methods::SyncReportResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let lang = self.lang;
        let Some(sinc) = self.sincronizacion.as_mut().filter(|s| s.epoca == epoca) else {
            return Vec::new();
        };
        // `on_apply_ended` es quien sabe leer el par (desenlace, informe): un
        // apply cancelado CON informe dice las dos mitades —«cancelado tras
        // aplicar N»— y uno sin informe deja que mande el error, porque no
        // hay recuento que lo pueda sustituir.
        // La categoría del error vuelve YA localizada en el idioma de esta
        // ventana, porque el modelo lo recibe como parámetro: leerlo del
        // global habría puesto el desenlace de una escritura en el idioma de
        // otra ventana.
        if let Some(categoria) = sinc.vista.on_apply_ended(estado, informe, lang) {
            sinc.vista.error = Some(clamp_display(categoria));
        }
        let cambio = ViewChange::Sync {
            sync: self.vista_sincronizacion(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// El desenlace de la Task de un PLAN entra en el modelo.
    ///
    /// Sin esto, `run` se quedaba en `Running` para siempre y con él moría la
    /// cláusula que el modelo compartido documenta como su motivo de existir:
    /// un plan CANCELADO o FALLIDO no se aprueba aunque haya cerrado. El
    /// `sync.plan_done` puede ir ya en el canal cuando el lector pulsa
    /// `Escape`, así que sin el desenlace la pantalla ofrecía aprobar un plan
    /// que acababan de mandar parar — y la fase B cuelga de ese campo el
    /// botón que escribe.
    ///
    /// Y por el progreso, no por el cierre del canal: una Task que muere sin
    /// cerrar su stream dejaba el panel en «planificando…» para siempre.
    fn cerrar_sincronizacion(&mut self, p: &norte_proto::TaskProgress) -> Vec<ViewChange> {
        let lang = self.lang;
        let Some(sinc) = self.sincronizacion.as_mut() else {
            return Vec::new();
        };
        if sinc.task != p.task_id {
            return Vec::new();
        }
        sinc.vista.run = norte_frontend::sync::SyncRunState::from_task_state(&p.state);
        if let norte_proto::TaskState::Failed { error } = &p.state {
            // La CATEGORÍA localizada, jamás el `Display` inglés: esto se
            // pinta de forma persistente y varias variantes interpolan datos
            // del otro extremo.
            sinc.vista.error = Some(clamp_display(norte_frontend::error::error_category_in(
                lang, error,
            )));
        }
        vec![ViewChange::Sync {
            sync: self.vista_sincronizacion(),
        }]
    }

    fn cerrar_comparacion(&mut self, p: &norte_proto::TaskProgress) -> Vec<ViewChange> {
        let lang = self.lang;
        let Some(c) = self.comparacion.as_mut() else {
            return Vec::new();
        };
        if c.task != p.task_id {
            return Vec::new();
        }
        let recibidas = c.vista.pane.len() as u64;
        c.vista
            .finish_from_task(&p.state, p.entries_done, recibidas, lang);
        vec![ViewChange::Compare {
            compare: self.vista_comparacion(),
        }]
    }

    /// Las teclas mientras el panel de diferencias está abierto.
    /// Las teclas mientras el panel de diferencias está abierto.
    ///
    /// `Escape` DOS veces y no una: la primera pide cancelar la Task, la
    /// segunda cierra pase lo que pase. Sin la segunda, cerrar dependía de
    /// que el canal de filas se cerrara de verdad, y hay formas de que no lo
    /// haga —un daemon muerto, un provider colgado de un NFS— que dejaban al
    /// lector atrapado en la única pantalla de norte sin salida.
    /// Las teclas mientras el panel de sincronización está abierto.
    ///
    /// `Escape` DOS veces, por lo mismo que en el panel de diferencias: la
    /// primera pide cancelar la Task viva —la del plan, o la del apply si ya
    /// está escribiendo—, la segunda cierra pase lo que pase. `a` aprueba, y
    /// cuando el plan borra o deja algo sin vuelta atrás, `y` contesta la
    /// segunda pregunta: es la última pantalla donde todavía se puede decir
    /// que no.
    fn tecla_en_sincronizacion(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(sinc) = self.sincronizacion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // Con la SEGUNDA pregunta delante, las teclas son suyas: solo `y`
        // contesta que sí, y cualquier otra cosa la retira. Una pregunta que
        // se puede contestar con cualquier tecla no es una pregunta.
        if sinc.vista.confirming.is_some() {
            let si = matches!(k.key.as_str(), "y" | "Y");
            sinc.vista.confirming = None;
            if si {
                return self.aplicar_plan(backend, buzon);
            }
            let cambio = ViewChange::Sync {
                sync: self.vista_sincronizacion(),
            };
            return (self.aplicada(), vec![self.parche(vec![cambio])]);
        }
        match k.key.as_str() {
            "a" | "A" => self.pedir_aprobacion(backend, buzon),
            "Escape" | "esc" => {
                // Mientras el daemon ESCRIBE, `Escape` pide cancelar y no
                // cierra: cerrar pierde el informe —y con él el recuento, los
                // fallos y el asa del deshacer— sobre un destino que se
                // reescribió a medias.
                let escribiendo = sinc.vista.is_submitted()
                    || matches!(
                        sinc.vista.state,
                        norte_frontend::sync::SyncState::Applying(_)
                    );
                if escribiendo {
                    // La PRIMERA vez pide parar y no cierra: cerrar pierde el
                    // informe sobre un destino a medio reescribir.
                    //
                    // La segunda SÍ cierra, y eso no contradice lo anterior:
                    // «espera al informe» vale mientras el informe pueda
                    // llegar, y hay formas de que no llegue nunca —un daemon
                    // muerto, un `sync.report` que falla, una Task cuyo canal
                    // se cae sin desenlace—. Sin esta salida, esta pantalla
                    // —la que ESCRIBE— era la única de norte sin salida.
                    if !sinc.vista.cancel_requested {
                        sinc.vista.cancel_requested = true;
                        let task = sinc.task;
                        if task.get() != 0 {
                            self.cancelar(task.get());
                        }
                        let cambio = ViewChange::Sync {
                            sync: self.vista_sincronizacion(),
                        };
                        return (self.aplicada(), vec![self.parche(vec![cambio])]);
                    }
                    let task = sinc.task;
                    self.sincronizacion = None;
                    if task.get() != 0 {
                        self.cancelar(task.get());
                    }
                    let mut fuera = vec![self.parche(vec![ViewChange::Sync { sync: None }])];
                    // Y se DICE lo que se pierde al cerrar: el destino puede
                    // haber quedado a medias y su informe ya no se va a ver.
                    fuera.extend(self.decir("msg-sync-closed-midway"));
                    return (self.aplicada(), fuera);
                }
                if sinc.vista.cancel_requested {
                    let task = sinc.task;
                    sinc.abandonada
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    self.sincronizacion = None;
                    if task.get() != 0 {
                        self.cancelar(task.get());
                    }
                    return (
                        self.aplicada(),
                        vec![self.parche(vec![ViewChange::Sync { sync: None }])],
                    );
                }
                sinc.vista.cancel_requested = true;
                // Y el modelo se entera YA: si el `plan_done` viene de camino,
                // sin esto el panel pasaría a «listo para aprobar» un plan que
                // el lector acaba de mandar parar.
                //
                // Solo mientras algo CORRE. Sobre un plan ya aplicado, marcar
                // «cancelado» reescribía el desenlace a «cancelado tras
                // aplicar N; el resto no se aplicó» sobre una sincronización
                // que terminó entera: dos frases falsas sobre lo que hay en
                // disco, en la única pantalla que lo describe.
                if matches!(sinc.vista.run, norte_frontend::sync::SyncRunState::Running) {
                    sinc.vista.run = norte_frontend::sync::SyncRunState::Cancelled;
                }
                let task = sinc.task;
                if task.get() != 0 {
                    self.cancelar(task.get());
                }
                let cambio = ViewChange::Sync {
                    sync: self.vista_sincronizacion(),
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            "ArrowDown" | "Down" | "ArrowUp" | "Up" | "PageDown" | "pgdn" | "PageUp" | "pgup"
            | "Home" | "home" | "End" | "end" => {
                let total = sinc.vista.steps().len();
                if total == 0 {
                    return (self.aplicada(), Vec::new());
                }
                // El TOPE del desplazamiento es «lo que hay menos lo que
                // cabe», no «lo que hay menos uno»: con lo segundo, una sola
                // flecha sobre un plan de dos pasos y una ventana de
                // doscientos dejaba de mandar el primer paso.
                let tope = total.saturating_sub(sinc.ventana.max(1));
                let pagina = sinc.ventana.max(1);
                sinc.primera_visible = match k.key.as_str() {
                    "ArrowDown" | "Down" => sinc.primera_visible.saturating_add(1),
                    "ArrowUp" | "Up" => sinc.primera_visible.saturating_sub(1),
                    "PageDown" | "pgdn" => sinc.primera_visible.saturating_add(pagina),
                    "PageUp" | "pgup" => sinc.primera_visible.saturating_sub(pagina),
                    "Home" | "home" => 0,
                    _ => tope,
                }
                .min(tope);
                let cambio = ViewChange::Sync {
                    sync: self.vista_sincronizacion(),
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            // Lo que no entiende se COME: un panel que deja pasar teclas no
            // es una pantalla.
            _ => (self.aplicada(), Vec::new()),
        }
    }

    /// `a`: pide aprobar el plan. Puede que haya una SEGUNDA pregunta.
    ///
    /// La segunda no es ceremonia: la compone el modelo compartido con una
    /// rama por perspectiva de deshacer, y solo aparece cuando el plan borra
    /// árboles o deja algo sin vuelta atrás. Un plan que se deshace entero y
    /// no borra nada no la tiene — preguntar siempre es lo que enseña a
    /// contestar sin leer.
    fn pedir_aprobacion(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let lang = self.lang;
        let Some(sinc) = self.sincronizacion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        if !sinc.vista.can_approve() {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-sync-cannot-approve".to_owned(),
                },
                Vec::new(),
            );
        }
        let pregunta = sinc.vista.state.plan().and_then(|p| p.confirmation(lang));
        match pregunta {
            Some(c) => {
                sinc.vista.confirming = Some(c);
                let cambio = ViewChange::Sync {
                    sync: self.vista_sincronizacion(),
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            None => self.aplicar_plan(backend, buzon),
        }
    }

    /// Manda `sync.apply` con el hash que el CORE devolvió.
    ///
    /// Por `SyncView::submit`, que es la ÚNICA puerta: mira `can_approve` y
    /// echa el pestillo del apply en vuelo en el mismo gesto. Separarlos deja
    /// la ventana en la que un segundo `a` —o un `Escape`— entra entre que la
    /// petición sale y el daemon contesta, y esta ventana lee eventos entre
    /// teclas, así que es alcanzable de verdad.
    fn aplicar_plan(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let epoca = self.sincronizacion.as_ref().map_or(0, |s| s.epoca);
        let Some(hash) = self.sincronizacion.as_mut().and_then(|s| s.vista.submit()) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-sync-cannot-approve".to_owned(),
                },
                Vec::new(),
            );
        };
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        tokio::spawn(async move {
            let resultado = backend2.sync_apply(hash).await;
            match resultado {
                Ok(task) => {
                    let id = task.id;
                    let _ = buzon2
                        .send(Mensaje::TaskNueva(Box::new((task, Vec::new(), None))))
                        .await;
                    let _ = buzon2
                        .send(Mensaje::Fondo(Box::new(Fondo::SyncAplicando(epoca, id))))
                        .await;
                }
                Err(e) => {
                    // ¿Se SABE que no escribió? Solo si el daemon contestó que
                    // no. Un transporte muerto deja la petición en el aire.
                    let seguro = matches!(
                        e,
                        Error::PolicyDenied { .. }
                            | Error::Conflict { .. }
                            | Error::NotFound
                            | Error::PermissionDenied
                            | Error::InvalidPath
                            | Error::Unsupported
                            | Error::EncodingLoss
                    );
                    let _ = buzon2.send(Mensaje::TaskFallida(Box::new(e))).await;
                    // Y se suelta el pestillo —cuando toca—: sin esto la `a`
                    // queda muerta para siempre sobre un plan que nadie aplicó.
                    let _ = buzon2
                        .send(Mensaje::Fondo(Box::new(Fondo::SyncNoAplicado(
                            epoca, seguro,
                        ))))
                        .await;
                }
            }
        });
        let cambio = ViewChange::Sync {
            sync: self.vista_sincronizacion(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// El daemon aceptó el apply: el modelo pasa a APLICANDO.
    fn sync_aplicando(
        &mut self,
        epoca: u64,
        task: norte_proto::TaskId,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(sinc) = self.sincronizacion.as_mut().filter(|s| s.epoca == epoca) else {
            return Vec::new();
        };
        // La Task que se sigue pasa a ser la del APPLY: es a la que apunta
        // ahora el `Escape`, y de la que hay que pedir el informe.
        sinc.task = task;
        sinc.epoca_conexion = self.epoca_conexion;
        if !sinc.vista.on_apply_started(task) {
            // El modelo la NIEGA —el lector pidió parar en la ventana en la
            // que el apply todavía no tenía id— y entonces cancelarla es
            // NUESTRO trabajo: nadie más conoce ese id, y el contrato del
            // modelo lo dice con todas las letras. Sin esto, el daemon seguía
            // reescribiendo el destino de un plan que el humano canceló.
            sinc.vista.on_apply_abandoned();
            let (_, mut fuera) = self.cancelar(task.get());
            fuera.extend(self.decir("msg-sync-cancelled-late"));
            fuera.push(self.parche(vec![ViewChange::Sync {
                sync: self.vista_sincronizacion(),
            }]));
            return fuera;
        }
        // Pudo nacer TERMINAL: el daemon la completó antes de contestar y su
        // progreso no dispara nunca. Es la misma carrera que el tablero ya
        // documenta, y aquí se traduce en un panel aplicando para siempre.
        let nacio = self
            .tasks
            .get(&task.get())
            .map(|t| t.progreso.borrow().clone());
        let mut fuera = vec![self.parche(vec![ViewChange::Sync {
            sync: self.vista_sincronizacion(),
        }])];
        if let Some(p) = nacio.filter(|p| p.state.is_terminal()) {
            self.pedir_informe_de_sync(&p, backend, buzon);
        }
        fuera.extend(Vec::new());
        fuera
    }

    fn tecla_en_comparacion(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(c) = self.comparacion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match k.key.as_str() {
            "Escape" | "esc" => {
                if c.vista.cancel_requested {
                    let task = c.task;
                    c.abandonada
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    self.comparacion = None;
                    if task.get() != 0 {
                        self.cancelar(task.get());
                    }
                    return (
                        self.aplicada(),
                        vec![self.parche(vec![ViewChange::Compare { compare: None }])],
                    );
                }
                c.vista.cancel_requested = true;
                let task = c.task;
                if task.get() != 0 {
                    self.cancelar(task.get());
                }
                let cambio = ViewChange::Compare {
                    compare: self.vista_comparacion(),
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            "Tab" | "tab" => {
                // Cambiar de lado cambia a qué panel navega `Enter` y sobre
                // qué lado operan las teclas de fichero.
                c.vista.pane.swap_active_side();
                let cambio = ViewChange::Compare {
                    compare: self.vista_comparacion(),
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            "Enter" | "enter" => {
                let Some(id) = c.vista.pane.selected_id() else {
                    return (self.aplicada(), Vec::new());
                };
                self.comparacion_activa(id, backend, buzon)
            }
            "ArrowUp" | "Up" | "ArrowDown" | "Down" => {
                let abajo = k.key.ends_with("Down");
                let visibles = c.vista.pane.visible_ids();
                if visibles.is_empty() {
                    return (self.aplicada(), Vec::new());
                }
                let actual = c
                    .vista
                    .pane
                    .selected_id()
                    .and_then(|id| visibles.iter().position(|v| *v == id))
                    .unwrap_or(0);
                let destino = if abajo {
                    (actual + 1).min(visibles.len() - 1)
                } else {
                    actual.saturating_sub(1)
                };
                let id = visibles[destino];
                c.vista.pane.select(id);
                let cambio = ViewChange::Compare {
                    compare: self.vista_comparacion(),
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            // 1..5: los filtros, en el orden fijo de las categorías, igual
            // que en el TUI.
            d if d.len() == 1 && d.chars().all(|c| ('1'..='5').contains(&c)) => {
                let i = d.chars().next().and_then(|c| c.to_digit(10)).unwrap_or(1) as usize - 1;
                let Some(cat) = norte_frontend::compare::CATEGORIES.get(i).copied() else {
                    return (self.aplicada(), Vec::new());
                };
                c.vista.pane.toggle_filter(cat);
                let cambio = ViewChange::Compare {
                    compare: self.vista_comparacion(),
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            // Una tecla que no entiende se COME igual: un panel que deja
            // pasar lo que no entiende no es una pantalla, es un adorno.
            _ => (self.aplicada(), Vec::new()),
        }
    }

    /// Elige una fila del panel de diferencias.
    /// Las cuatro acciones del panel de diferencias, en un brazo.
    ///
    /// Juntas y no cuatro brazos del reparto general: son la misma superficie
    /// y ninguna significa nada sin ella.
    fn accion_de_comparacion(
        &mut self,
        accion: &UiAction,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match accion {
            UiAction::CompareSelectRow { id } => self.comparacion_selecciona(*id),
            UiAction::CompareActivateRow { id } => self.comparacion_activa(*id, backend, buzon),
            UiAction::CompareToggleFilter { category } => self.comparacion_filtra(category),
            UiAction::CompareSetVisibleRange { first, count } => {
                self.comparacion_ventana(*first, *count)
            }
            // El reparto general solo manda aquí esas cuatro.
            _ => (Self::obsoleta(StaleAction::Modal), Vec::new()),
        }
    }

    /// Elige una fila del panel de diferencias.
    fn comparacion_selecciona(&mut self, id: u64) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(c) = self.comparacion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // `select` IGNORA un id que no llegó, que es lo correcto: la
        // alternativa es una selección que nombra una fila inexistente.
        c.vista.pane.select(id);
        if c.vista.pane.selected_id() != Some(id) {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let cambio = ViewChange::Compare {
            compare: self.vista_comparacion(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Enseña o esconde una categoría entera.
    fn comparacion_filtra(
        &mut self,
        categoria: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(c) = self.comparacion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let Some(cat) = norte_frontend::compare::CATEGORIES
            .iter()
            .find(|c| c.id() == categoria)
        else {
            // Una categoría que no existe es un renderer de otra versión, no
            // una orden.
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        c.vista.pane.toggle_filter(*cat);
        let cambio = ViewChange::Compare {
            compare: self.vista_comparacion(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// El renderer dice qué ventana pinta.
    fn comparacion_ventana(
        &mut self,
        primera: u64,
        cuantas: u32,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(c) = self.comparacion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        c.primera_visible = usize::try_from(primera).unwrap_or(0);
        // Acotada: lo que el renderer diga que le cabe no puede hacer que un
        // parche lleve medio millón de filas.
        c.ventana = usize::try_from(cuantas)
            .unwrap_or(Self::VENTANA_COMPARACION)
            .clamp(1, MAX_ROWS_PER_BATCH);
        let cambio = ViewChange::Compare {
            compare: self.vista_comparacion(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Abre la fila elegida: navega al directorio del lado ACTIVO.
    ///
    /// A dónde ir lo decide el modelo COMPARTIDO (`navigation_target`): la
    /// fila si es un directorio, su padre si es un fichero, y `None` cuando
    /// ese lado está vacío —un huérfano mirado desde el lado que no lo
    /// tiene—, que NO cae al otro lado.
    fn comparacion_activa(
        &mut self,
        id: u64,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(c) = self.comparacion.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        c.vista.pane.select(id);
        let Some(destino) = c.vista.pane.navigation_target() else {
            let lado = norte_frontend::compare::side_label(c.vista.pane.active_side(), self.lang);
            return (
                ActionAck::Unavailable {
                    reason_key: "compare-no-target".to_owned(),
                },
                self.decir_con("compare-no-target", &[("side", &lado)]),
            );
        };
        // El panel que navega es el del lado ACTIVO, no el que tenga el foco:
        // quien mira la derecha no puede perder su directorio de la izquierda
        // por pulsar `Enter`. Se ENFOCA ese hueco y se navega por el camino
        // de siempre, que es el que registra el rastro y pide el listado.
        if let Some(slot) = self.hueco_del_lado() {
            self.roles.set(RoleId::Active, SlotId(slot));
            self.reconcilia_roles();
        }
        let mut salidas = self.navegar(&destino, Trail::Record, backend, buzon);
        let cambio = ViewChange::Compare {
            compare: self.vista_comparacion(),
        };
        salidas.push(self.parche(vec![cambio]));
        (self.aplicada(), salidas)
    }

    /// El hueco que corresponde al lado ACTIVO de la comparación.
    fn hueco_del_lado(&self) -> Option<u32> {
        let c = self.comparacion.as_ref()?;
        let izquierdo = u32::try_from(c.vista.left_pane).ok()?;
        match c.vista.pane.active_side() {
            norte_proto::methods::Side::Right => self.hueco_destino().ok(),
            _ => Some(izquierdo),
        }
    }

    /// Pide el PLAN de sincronizar el panel activo sobre el destino.
    ///
    /// El plan no escribe un byte: dice qué haría. Lo que escribe es
    /// `sync.apply`, y solo contra el hash que este plan cierre.
    fn pedir_sincronizacion(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // UNA a la vez. Relanzar dejaba el panel anterior sin abandonar y su
        // Task sin cancelar —el daemon seguía caminando un árbol para un plan
        // que ya no se puede ver— y, con una petición en vuelo, la segunda
        // pulsación mataba el panel de las dos.
        if self.sincronizacion.is_some() || self.sync_pedida.is_some() {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-sync-already".to_owned(),
                },
                Vec::new(),
            );
        }
        let destino_slot = match self.hueco_destino() {
            Ok(d) => d,
            Err(reason_key) => {
                return (
                    ActionAck::Unavailable {
                        reason_key: reason_key.to_owned(),
                    },
                    Vec::new(),
                );
            }
        };
        // Qué árbol se sobrescribe lo decide la regla COMPARTIDA, no una
        // copia local: dos respuestas a «cuál de los dos se reescribe» es el
        // bug más barato de escribir y el más caro de encontrar, porque las
        // dos producen un plan perfectamente plausible.
        let enfocado = self.hueco().pane.dir().clone();
        let otro = self.huecos[&destino_slot].pane.dir().clone();
        // Con el panel de diferencias abierto manda el LADO ACTIVO; sin él,
        // el pane con foco es el origen. Las dos ramas viven en la regla
        // compartida, y aquí solo se le pasan los datos.
        let raices = norte_frontend::sync::sync_roots(
            self.comparacion.as_ref().map(|c| &c.vista),
            &norte_frontend::sync::Panes {
                focused_root: &enfocado,
                focused_encoding: None,
                other_root: &otro,
                other_encoding: None,
            },
        );
        let (origen, destino) = (raices.source.clone(), raices.dest.clone());
        if origen == destino {
            // Raíces solapadas: el daemon lo rechaza con `OverlappingRoots` y
            // no crea Task. Este atajo local es cortesía —la autoridad es el
            // core, que también caza el solape ANIDADO— pero abrir un panel
            // que va a morir es peor que decirlo antes.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-same-directory".to_owned(),
                },
                Vec::new(),
            );
        }
        (
            self.aplicada(),
            self.lanzar_plan_de_sync(raices, backend, buzon),
        )
    }

    /// Encola `sync.plan` y engancha su canal de eventos al actor.
    fn lanzar_plan_de_sync(
        &mut self,
        raices: norte_frontend::sync::SyncRoots,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let norte_frontend::sync::SyncRoots {
            source: origen,
            dest: destino,
            source_encoding: origen_encoding,
            dest_encoding: destino_encoding,
        } = raices;
        self.epoca_busqueda += 1;
        let epoca = self.epoca_busqueda;
        let abandonada = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // `Update` y no `Mirror`: el modo que NO borra es el que puede ser el
        // de por defecto. Elegir espejo es una decisión que se toma a
        // propósito, y hasta que haya dónde tomarla no se ofrece.
        let modo = norte_proto::methods::SyncMode::Update;
        let params = norte_proto::methods::SyncPlanParams {
            source: origen.clone(),
            dest: destino.clone(),
            mode: modo,
            compare: norte_proto::methods::SyncCompareOptions::default(),
            on_unknown: norte_proto::methods::OnUnknown::default(),
            // Sin `include`: el árbol entero. Acotar el plan a una selección
            // es lo que hace el panel de diferencias con sus marcas, y eso
            // llega cuando esta ventana tenga esa vía.
            include: None,
        };
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        let abandonada2 = Arc::clone(&abandonada);
        tokio::spawn(async move {
            let (task, mut rx) = match backend2.sync_plan(params).await {
                Ok(par) => par,
                Err(e) => {
                    // El fallo se DICE y además SUELTA la petición: sin lo
                    // segundo, un daemon que no sabe planificar —o unas
                    // raíces solapadas— dejaban `sync_pedida` puesta para
                    // siempre y el siguiente intento se rehusaba solo.
                    let _ = buzon2.send(Mensaje::TaskFallida(Box::new(e))).await;
                    let _ = buzon2
                        .send(Mensaje::Fondo(Box::new(Fondo::PlanDeSyncFallido(epoca))))
                        .await;
                    return;
                }
            };
            let id = task.id;
            let cancel = Arc::clone(&task.cancel);
            let _ = buzon2
                .send(Mensaje::TaskNueva(Box::new((task, Vec::new(), None))))
                .await;
            let _ = buzon2
                .send(Mensaje::Fondo(Box::new(Fondo::PlanDeSyncVivo(epoca, id))))
                .await;
            if abandonada2.load(std::sync::atomic::Ordering::SeqCst) {
                cancel();
                return;
            }
            while let Some(ev) = rx.recv().await {
                if abandonada2.load(std::sync::atomic::Ordering::SeqCst) {
                    cancel();
                    return;
                }
                if buzon2
                    .send(Mensaje::Fondo(Box::new(Fondo::EventoDeSync(
                        epoca,
                        Box::new(ev),
                    ))))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        // El panel se abre cuando se SABE el id de la Task, y no antes: el
        // modelo compartido lo usa para descartar lo que venga de otro plan,
        // y con un id de relleno descartaba también los suyos —el panel se
        // quedaba en cero pasos y el plan cerraba «no se puede aprobar»—.
        self.sincronizacion = None;
        self.sync_pedida = Some(SyncPedida {
            epoca,
            abandonada,
            modo,
            origen,
            destino,
            origen_encoding,
            destino_encoding,
        });
        Vec::new()
    }

    /// Un evento del plan: un lote de pasos, o su cierre.
    /// El daemon aceptó el plan y dijo su Task: ahora se abre el panel.
    ///
    /// El modelo compartido nace CON el id porque es lo que usa para
    /// descartar lo que venga de otro plan; construirlo antes, con un id de
    /// relleno, hacía que descartara también sus propios lotes y el panel se
    /// quedaba en cero pasos y cerraba «no se puede aprobar».
    fn abrir_panel_de_sync(
        &mut self,
        epoca: u64,
        task: norte_proto::TaskId,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // FILTRAR antes de TOMAR: `take()` incondicional se llevaba por
        // delante una petición nueva cuando contestaba la Task de una vieja,
        // y entonces no se abría panel ninguno mientras dos recorridos
        // seguían caminando dos árboles en el daemon.
        if self.sync_pedida.as_ref().is_none_or(|p| p.epoca != epoca) {
            return Vec::new();
        }
        let Some(pedida) = self.sync_pedida.take() else {
            return Vec::new();
        };
        self.sincronizacion = Some(Sincronizacion {
            epoca,
            task,
            abandonada: pedida.abandonada,
            vista: norte_frontend::sync::SyncView::new(
                task,
                pedida.modo,
                pedida.origen,
                pedida.destino,
                // Las reinterpretaciones de cada lado, tal como las decidió
                // la regla compartida: son DOS porque los dos panes son dos
                // ubicaciones, y cruzarlas nombraría con otros bytes el
                // fichero sobre el que cae la escritura.
                pedida.origen_encoding,
                pedida.destino_encoding,
            ),
            primera_visible: 0,
            ventana: Self::VENTANA_COMPARACION,
            epoca_conexion: self.epoca_conexion,
            informe_pedido: false,
        });
        let cambio = ViewChange::Sync {
            sync: self.vista_sincronizacion(),
        };
        vec![self.parche(vec![cambio])]
    }

    fn aplicar_evento_de_sync(
        &mut self,
        epoca: u64,
        ev: norte_client::SyncPlanEvent,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(sinc) = self.sincronizacion.as_mut() else {
            return Vec::new();
        };
        if sinc.epoca != epoca {
            return Vec::new();
        }
        // El modelo COMPARTIDO decide qué entra: descarta lo que venga de
        // otro plan por su `task_id`, y es quien sabe cuándo el plan cierra.
        let cambio = match ev {
            norte_client::SyncPlanEvent::Steps(lote) => sinc.vista.state.on_steps(lote),
            norte_client::SyncPlanEvent::Done(done) => sinc.vista.state.on_plan_done(done),
        };
        if !cambio {
            // Que se descartó, DICHO: un lote rechazado después del cierre es
            // una violación del contrato del daemon, y callarla la esconde.
            tracing::warn!(epoca, "un evento del plan se descartó");
            return Vec::new();
        }
        let cambio = ViewChange::Sync {
            sync: self.vista_sincronizacion(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// Los pasos de la ventana, proyectados por el modelo COMPARTIDO.
    fn pasos_proyectados(
        pasos: &[norte_proto::methods::SyncStep],
        papelera: norte_proto::methods::DestTrash,
        enc: norte_frontend::sync::SyncEncodings,
        lang: norte_i18n::Lang,
    ) -> Vec<crate::dto::SyncStepView> {
        pasos
            .iter()
            .map(|paso| {
                // Las celdas las compone el modelo COMPARTIDO: qué hace el
                // paso, por qué, si el deshacer lo devuelve —que NUNCA sale
                // de `reversal` a secas, porque esa es media respuesta— y las
                // dos ortografías cuando las hay.
                let c = norte_frontend::sync::render_step(paso, papelera, enc);
                crate::dto::SyncStepView {
                    id: c.id,
                    kind: clamp_display(norte_frontend::sync::step_label(paso.kind, lang)),
                    // El porqué solo lo tienen los pasos que lo tienen: un
                    // `Skip`, o uno que no se puede deshacer. Vacío es
                    // AUSENCIA, no una frase inventada.
                    reason: c.reason.map_or_else(String::new, |r| {
                        clamp_display(norte_frontend::sync::reason_label(r, lang))
                    }),
                    undo: clamp_display(norte_frontend::sync::undo_label(c.undo, lang)),
                    anchor: Self::nombre_de_ancla(c.anchor),
                    anchor_label: Self::etiqueta_de_ancla(c.anchor, lang),
                    path: clamp_display(c.rel.text.clone()),
                    path_hostile: c.rel.hostile,
                    dest_path: c.dest_rel.as_ref().map(|d| clamp_display(d.text.clone())),
                    dest_path_hostile: c.dest_rel.as_ref().is_some_and(|d| d.hostile),
                    twins: c.dest_rel_twin,
                }
            })
            .collect()
    }

    /// Los fallos del informe, cuando ya hay informe.
    fn fallos_proyectados(
        estado: &norte_frontend::sync::SyncState,
        enc: norte_frontend::sync::SyncEncodings,
        lang: norte_i18n::Lang,
    ) -> Vec<crate::dto::SyncFailureView> {
        let norte_frontend::sync::SyncState::Applied(a) = estado else {
            return Vec::new();
        };
        a.report()
            .failures
            .iter()
            .map(|f| {
                let c = norte_frontend::sync::render_failure(f, enc);
                crate::dto::SyncFailureView {
                    cause: clamp_display(norte_frontend::sync::failure_cause_label(f.cause, lang)),
                    path: clamp_display(c.rel.text.clone()),
                    path_hostile: c.rel.hostile,
                    anchor: Self::nombre_de_ancla(c.anchor),
                    anchor_label: Self::etiqueta_de_ancla(c.anchor, lang),
                }
            })
            .collect()
    }

    /// De qué raíz cuelga una ruta, por su id estable.
    ///
    /// `either` se dice: en un panel donde una ruta sin calificar significa
    /// «del origen», callarlo es afirmar el origen.
    fn nombre_de_ancla(anchor: norte_frontend::sync::RelAnchor) -> String {
        match anchor {
            norte_frontend::sync::RelAnchor::Dest => "dest".to_owned(),
            norte_frontend::sync::RelAnchor::Source => "source".to_owned(),
            norte_frontend::sync::RelAnchor::Either => "either".to_owned(),
        }
    }

    /// La etiqueta del ancla, ya traducida, o vacía cuando no hay nada que
    /// decir.
    ///
    /// La etiqueta y no solo el id: el DTO promete que esto se pinta, y un
    /// `data-` que ningún estilo lee no lo pinta — el `either` seguía
    /// callado, que en un panel donde una ruta sin calificar significa «del
    /// origen» es afirmar el origen.
    fn etiqueta_de_ancla(
        anchor: norte_frontend::sync::RelAnchor,
        lang: norte_i18n::Lang,
    ) -> String {
        norte_frontend::sync::anchor_label(anchor, lang).map_or_else(String::new, clamp_display)
    }

    /// La proyección del panel de sincronización, acotada a su ventana.
    fn vista_sincronizacion(&self) -> Option<crate::dto::SyncView> {
        let sinc = self.sincronizacion.as_ref()?;
        let v = &sinc.vista;
        let (origen, origen_hostil) = norte_frontend::path_display(&v.source_root);
        let (destino, destino_hostil) = norte_frontend::path_display(&v.dest_root);
        let pasos = v.steps();
        let primera = sinc.primera_visible.min(pasos.len());
        let hasta = primera.saturating_add(sinc.ventana).min(pasos.len());
        let papelera = v.dest_trash();
        let enc = v.encodings();
        let filas = Self::pasos_proyectados(
            pasos.get(primera..hasta).unwrap_or_default(),
            papelera,
            enc,
            self.lang,
        );
        let fallos = Self::fallos_proyectados(&v.state, enc, self.lang);
        Some(crate::dto::SyncView {
            source: crate::dto::DialogLine {
                text: clamp_display(origen),
                hostile: origen_hostil,
            },
            dest: crate::dto::DialogLine {
                text: clamp_display(destino),
                hostile: destino_hostil,
            },
            // El modo, por la etiqueta COMPARTIDA. Caer en «actualizar» ante
            // un modo que esta build no sabe nombrar afirmaría la mitad
            // SEGURA de lo que se está aprobando —«esto no borra»— sobre algo
            // desconocido, y el propio catálogo lo prohíbe por escrito.
            mode: clamp_display(norte_frontend::sync::mode_label(v.mode, self.lang)),
            steps: filas,
            first_visible: primera as u64,
            // Los RETENIDOS más los que el modelo tiró: sin sumarlos, este
            // número y el de la línea de estado se contradicen en un plan
            // grande, y los dos cruzan en el mismo mensaje.
            total: (pasos.len() as u64).saturating_add(
                v.state
                    .plan()
                    .map_or(0, norte_frontend::sync::SyncPlan::dropped),
            ),
            // El RESUMEN, que es lo que un humano lee antes de aprobar:
            // irreversibles, bytes (con los que no se pudieron medir aparte),
            // lo que no se pudo leer, y si la lista esconde pasos. No cabe en
            // la línea de estado y no puede quedarse dentro del modelo.
            summary: v
                .state
                .plan()
                .map(|p| {
                    p.summary_lines(self.lang)
                        .into_iter()
                        .map(clamp_display)
                        .collect()
                })
                .unwrap_or_default(),
            // Lo que IMPIDE aplicar, con SU RUTA: «el destino es de solo
            // lectura» sin decir cuál manda a buscar el problema a ciegas, y
            // un bloqueo de la raíz se dice «todo el árbol», no vacío.
            blockers: v
                .state
                .plan()
                .map(|p| {
                    p.done()
                        .blockers
                        .iter()
                        .map(|b| {
                            // Sin reinterpretación: de qué raíz cuelga el
                            // `rel` de un BLOQUEO no lo decide ninguna regla
                            // compartida todavía —la que existe es para
                            // pasos—, y elegirla aquí sería inventar una
                            // segunda respuesta. Hoy no cambia nada porque
                            // esta ventana no tiene override de codificación
                            // de nombres; el día que lo tenga, la regla va
                            // arriba y no aquí.
                            let ruta =
                                norte_frontend::sync::rel_display_or_root(&b.rel, None, self.lang);
                            crate::dto::SyncBlockerView {
                                label: clamp_display(norte_frontend::sync::blocker_label(
                                    b.kind, self.lang,
                                )),
                                path: clamp_display(ruta.text),
                                path_hostile: ruta.hostile,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default(),
            // Cuántos hay DE VERDAD: el wire recorta la lista a 256 y el
            // total viaja aparte justo para que 40 000 no se lean como 256.
            blockers_total: v.state.plan().map_or(0, |p| p.done().blockers_total),
            status: clamp_display(norte_frontend::sync::status_line(v, self.lang)),
            // La línea de teclas del modelo ofrece `a aprobar` en cuanto el
            // plan se puede aprobar, y esta fase NO tiene esa tecla: decir lo
            // que no se puede hacer entrena a pulsarla justo en la pantalla
            // donde la fase siguiente pone la escritura. Mientras aprobar no
            // exista, esta pantalla dice que solo lee.
            hint: clamp_display(if v.can_approve() {
                norte_i18n::t_in(self.lang, "host-sync-read-only")
            } else {
                norte_i18n::t_in(self.lang, norte_frontend::sync::hint_id(v))
            }),
            confirming: v.confirming.as_ref().map(|c| clamp_display(c.text.clone())),
            // Los fallos del informe, uno a uno. El recuento va en la línea
            // de estado, que lo compone el modelo compartido; esto es el
            // detalle, y sin él «3 fallaron» no dice cuáles.
            failures: fallos,
            cancel_requested: v.cancel_requested,
            can_approve: v.can_approve(),
            running: matches!(v.run, norte_frontend::sync::SyncRunState::Running),
        })
    }

    /// Cuántas filas de la comparación —o pasos de un plan— cruzan si el
    /// renderer no ha dicho su ventana todavía.
    const VENTANA_COMPARACION: usize = 200;

    /// Lanza la comparación de los dos paneles y abre el panel de
    /// diferencias.
    ///
    /// La raíz derecha sale del hueco con el rol `Target`, por el MISMO
    /// camino que una transferencia: dos formas de decidir «el otro panel»
    /// son dos sitios donde pueden divergir, y con varios candidatos y
    /// ninguno designado se pide elegir en vez de romper el empate.
    fn pedir_comparacion(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let derecha = match self.directorio_destino() {
            Ok(d) => d,
            Err(reason_key) => {
                return (
                    ActionAck::Unavailable {
                        reason_key: reason_key.to_owned(),
                    },
                    Vec::new(),
                );
            }
        };
        let izquierda = self.hueco().pane.dir().clone();
        if izquierda == derecha {
            // El daemon lo rechazaría igual (`-32602`), y abrir un panel que
            // promete una respuesta imposible es peor que decirlo antes.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-same-directory".to_owned(),
                },
                Vec::new(),
            );
        }
        (
            self.aplicada(),
            self.lanzar_comparacion(izquierda, derecha, backend, buzon),
        )
    }

    /// `pane.dir-size` (#139, #290): cuenta lo que ocupa lo MARCADO —o lo que
    /// hay bajo el cursor— y lo deja en el tablero.
    ///
    /// UNA Task para el lote entero, al revés que copiar o borrar: el método
    /// del wire toma una lista, y contar por separado obligaría a quien
    /// pregunta a sumar los bytes **y** los ilegibles, que no se suman igual
    /// —un total redondo compuesto de dos cuentas parciales es una respuesta
    /// equivocada, no una incompleta—.
    ///
    /// No hay directorios afectados que refrescar: esto no escribe nada. Su
    /// resultado ES su progreso terminal, que el tablero ya sabe leer.
    fn contar_tamano(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // `marked_paths` cae al cursor cuando no hay marcas: la misma fuente
        // de «sobre qué opera esto» que usa una transferencia.
        let paths: Vec<VPath> = self.hueco().pane.marked_paths();
        if paths.is_empty() {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-nothing-selected".to_owned(),
                },
                self.decir("msg-nothing-selected"),
            );
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let mensaje = match backend.dir_size(paths).await {
                Ok(task) => Mensaje::TaskNueva(Box::new((task, Vec::new(), None))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon.send(mensaje).await;
        });
        (self.aplicada(), Vec::new())
    }

    /// Encola `fs.compare` y engancha su canal de filas al actor.
    fn lanzar_comparacion(
        &mut self,
        izquierda: VPath,
        derecha: VPath,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.epoca_busqueda += 1;
        let epoca = self.epoca_busqueda;
        let abandonada = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let params = norte_proto::methods::FsCompareParams {
            left: izquierda.clone(),
            right: derecha.clone(),
            criteria: norte_proto::methods::CompareCriteria::default(),
            // Sin tope de profundidad, como el TUI: una comparación que se
            // para a mitad no ha contestado a lo que se le preguntó.
            max_depth: None,
            // La regla FAT, que es el default del wire.
            mtime_tolerance_ms: 2000,
            // Sin seguir enlaces, como el default del core: los destinos se
            // comparan como BYTES, y seguirlos podría salirse del árbol que
            // se preguntó.
            follow_symlinks: false,
            // Un huérfano se emite como UNA fila y no se recorre, que es lo
            // que sabe hacer el default. Descender un lado es una decisión
            // del plan de sincronización, no de una comparación que solo
            // mira.
            descend_orphans: None,
        };
        self.comparacion = Some(Comparacion {
            epoca,
            task: norte_proto::TaskId::new(0),
            abandonada: Arc::clone(&abandonada),
            vista: norte_frontend::compare::CompareView::new(
                izquierda,
                derecha,
                // El hueco que lanzó la comparación ES el lado izquierdo, y
                // eso decide a qué panel navega un `Enter`. Sin ello, quien
                // mira el lado derecho perdía su directorio de la izquierda
                // para ir a ver el de la derecha.
                self.activo() as usize,
                None,
                None,
            ),
            primera_visible: 0,
            ventana: Self::VENTANA_COMPARACION,
        });
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        tokio::spawn(async move {
            let (task, mut rx) = match backend2.compare(params).await {
                Ok(par) => par,
                Err(e) => {
                    let _ = buzon2.send(Mensaje::TaskFallida(Box::new(e))).await;
                    return;
                }
            };
            let id = task.id;
            let cancel = Arc::clone(&task.cancel);
            let _ = buzon2
                .send(Mensaje::TaskNueva(Box::new((task, Vec::new(), None))))
                .await;
            let _ = buzon2
                .send(Mensaje::Fondo(Box::new(Fondo::ComparacionViva(epoca, id))))
                .await;
            // La vista pudo cerrarse mientras el daemon aceptaba la Task: en
            // esa ventana el actor no tiene a quién cancelar, así que cancela
            // quien sí lo tiene.
            if abandonada.load(std::sync::atomic::Ordering::SeqCst) {
                cancel();
                return;
            }
            while let Some(lote) = rx.recv().await {
                if abandonada.load(std::sync::atomic::Ordering::SeqCst) {
                    cancel();
                    return;
                }
                if buzon2
                    .send(Mensaje::Fondo(Box::new(Fondo::FilasComparadas(
                        epoca,
                        Box::new(lote),
                    ))))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        let cambio = ViewChange::Compare {
            compare: self.vista_comparacion(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// Un lote de filas comparadas. Casa por ÉPOCA, como los hallazgos.
    fn aplicar_filas_comparadas(
        &mut self,
        epoca: u64,
        lote: norte_proto::methods::CompareRowsBatch,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(c) = self.comparacion.as_mut() else {
            return Vec::new();
        };
        if c.epoca != epoca {
            return Vec::new();
        }
        // El panel COMPARTIDO es quien cuenta, filtra y selecciona: aquí solo
        // se le dan las filas.
        c.vista.pane.extend(lote.rows);
        let cambio = ViewChange::Compare {
            compare: self.vista_comparacion(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// La proyección del panel de diferencias, acotada a su ventana.
    fn vista_comparacion(&self) -> Option<crate::dto::CompareView> {
        use norte_frontend::compare::{Category, cells_for};

        let c = self.comparacion.as_ref()?;
        let ahora = ahora_ms();
        let (izq, izq_hostil) = norte_frontend::path_display(&c.vista.left_root);
        let (der, der_hostil) = norte_frontend::path_display(&c.vista.right_root);
        let visibles: Vec<&norte_proto::methods::CompareRow> = c.vista.pane.visible().collect();
        let primera = c.primera_visible.min(visibles.len());
        let hasta = primera.saturating_add(c.ventana).min(visibles.len());
        let filas = visibles
            .get(primera..hasta)
            .unwrap_or_default()
            .iter()
            .map(|r| {
                // Las celdas las compone el modelo COMPARTIDO: los nombres
                // enmascarados con su bandera, y los dos glifos del medio.
                // Ni el emparejado ni el veredicto se recalculan aquí.
                let celdas = cells_for(r, None, None);
                let cara = |f: Option<&norte_frontend::compare::RowFace>| {
                    f.map(|f| crate::dto::CompareFaceView {
                        name: clamp_display(f.name.clone()),
                        hostile: f.hostile,
                        // Formateados con las MISMAS funciones que una
                        // columna del listado: un tamaño o una fecha no
                        // pueden leerse distinto según qué panel los pinte.
                        size: f.size.map(norte_frontend::human_bytes).unwrap_or_default(),
                        mtime: f
                            .mtime_ms
                            .map(|ms| {
                                norte_frontend::columns::format_mtime(
                                    ms,
                                    norte_frontend::columns::TimeFormat::Iso,
                                    ahora,
                                )
                            })
                            .unwrap_or_default(),
                        is_dir: f.kind == EntryKind::Dir,
                    })
                };
                crate::dto::CompareRowView {
                    id: r.id,
                    verdict: clamp_display(norte_frontend::compare::verdict_label(
                        r.verdict, self.lang,
                    )),
                    category: Category::of(r.verdict).id().to_owned(),
                    confidence: clamp_display(norte_frontend::compare::confidence_label(
                        r.confidence,
                        self.lang,
                    )),
                    criterion: clamp_display(norte_frontend::compare::criterion_label(
                        r.criterion,
                        self.lang,
                    )),
                    reason: r.reason.map(|x| {
                        clamp_display(norte_frontend::compare::reason_label(x, self.lang))
                    }),
                    left: cara(celdas.left.as_ref()),
                    right: cara(celdas.right.as_ref()),
                    paired_under: norte_frontend::compare::paired_under_label(
                        r.paired_under,
                        self.lang,
                    )
                    .map(clamp_display),
                }
            })
            .collect();
        let filtros = norte_frontend::compare::CATEGORIES
            .iter()
            .map(|cat| crate::dto::CompareFilterView {
                id: cat.id().to_owned(),
                label: clamp_display(cat.label(self.lang)),
                count: c.vista.pane.count_of(*cat) as u64,
                hidden: c.vista.pane.is_hidden(*cat),
            })
            .collect();
        Some(crate::dto::CompareView {
            left: clamp_display(izq),
            left_hostile: izq_hostil,
            right: clamp_display(der),
            right_hostile: der_hostil,
            rows: filas,
            first_visible: primera as u64,
            total: visibles.len() as u64,
            selected: c.vista.pane.selected_id(),
            filters: filtros,
            // La frase la compone el modelo COMPARTIDO, y no es un detalle:
            // sus cinco estados son cómo se dice si la respuesta está
            // completa, y una comparación que perdió lotes tiene que leerse
            // distinto de una que terminó. El TUI y el CLI ya se equivocaron
            // aquí cada uno por su cuenta.
            status: clamp_display(norte_frontend::compare::status_line(
                &c.vista,
                c.vista.pane.marked_len(),
                self.lang,
            )),
            running: c.vista.state == norte_frontend::compare::CompareState::Running,
        })
    }

    /// Abre el prompt de una consulta SEMÁNTICA.
    /// Abre el prompt de una consulta SEMÁNTICA.
    ///
    /// No lleva raíz, y eso es lo que dice el diálogo: el índice se construye
    /// por raíces y no por lo que se esté mirando, así que acotar la búsqueda
    /// al directorio del panel prometería un alcance que el índice puede no
    /// tener. Se pregunta al índice ENTERO, igual que el TUI.
    fn pedir_consulta_semantica(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-semantic-title".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![crate::dto::DialogLine {
                text: clamp_display(norte_i18n::t_in(self.lang, "modal-semantic-scope")),
                hostile: false,
            }],
            overflow_note: String::new(),
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
            input: Some(String::new()),
            input_hostile: false,
        };
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            input_crudo: String::new(),
            reconocido: true,
            al_confirmar: Some(Pendiente::ConsultaSemantica),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Lanza la consulta contra el índice. La respuesta vuelve al actor.
    ///
    /// Época nueva por consulta: la respuesta tarda —hay un embed de por
    /// medio— y quien pregunta dos veces no puede acabar mirando los
    /// resultados de la primera.
    fn lanzar_semantica(
        &mut self,
        consulta: String,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if consulta.trim().is_empty() {
            // Una consulta vacía no sale del proceso: no significa nada, y
            // lo que sale va a un proveedor externo.
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "err-empty-pattern",
            )));
            let cambio = ViewChange::Status(self.status.clone());
            return vec![self.parche(vec![cambio])];
        }
        self.epoca_busqueda += 1;
        let epoca = self.epoca_busqueda;
        self.busqueda = Some(Busqueda {
            semantica: true,
            epoca,
            task: norte_proto::TaskId::new(0),
            abandonada: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            query: consulta.clone(),
            // El alcance es el índice entero: no hay raíz que enseñar, y la
            // vista lo dice por `semantic`.
            root: self.hueco().pane.dir().clone(),
            hits: Vec::new(),
            cursor: 0,
            viva: true,
            tope: norte_proto::methods::INDEX_SEMANTIC_MAX_K,
        });
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let handle = tokio::spawn(async move {
            let hits = backend
                .semantic_search(consulta, norte_frontend::SEMANTIC_K)
                .await;
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::Semanticos(epoca, hits))))
                .await;
        });
        // Relanzar ABORTA la anterior, y abortar la cancela de verdad: el SDK
        // manda `rpc.cancel` al soltar la llamada. Dejarla correr sería pagar
        // un embed y un barrido del índice por una respuesta que la época ya
        // condena a descartarse.
        if let Some(vieja) = self.semantica_en_vuelo.replace(handle) {
            vieja.abort();
        }
        let cambio = ViewChange::Search {
            search: self.vista_busqueda(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// La respuesta del índice: entra si sigue siendo la consulta de ahora.
    fn aplicar_semanticos(
        &mut self,
        epoca: u64,
        hits: Result<Vec<norte_proto::methods::SemanticHit>, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // La vista pudo cerrarse o relevarse mientras el embed corría.
        if self.busqueda.as_ref().is_none_or(|b| b.epoca != epoca) {
            return Vec::new();
        }
        let hits = match hits {
            // El barrido del wire es el COMPARTIDO: acota la `k` y rehúsa un
            // score no finito, que serializado como `null` envenenaría el
            // orden.
            Ok(h) => {
                let Some(h) = norte_frontend::validate_semantic_hits(h) else {
                    self.busqueda = None;
                    let mut fuera = vec![self.parche(vec![ViewChange::Search { search: None }])];
                    fuera.extend(self.decir("msg-semantic-bad-hits"));
                    return fuera;
                };
                h
            }
            Err(e) => {
                self.busqueda = None;
                let clave = match e {
                    // `NotFound` aquí NO es «no hay resultados»: es que ese
                    // root no tiene filas en el índice. Leerlo como una
                    // búsqueda vacía deja al lector creyendo que no hay nada
                    // parecido a lo que preguntó.
                    Error::NotFound => "msg-semantic-no-index",
                    Error::Unsupported => "msg-semantic-unsupported",
                    _ => norte_frontend::error::error_key(&e),
                };
                let mut fuera = vec![self.parche(vec![ViewChange::Search { search: None }])];
                fuera.extend(self.decir(clave));
                return fuera;
            }
        };
        self.semantica_en_vuelo = None;
        if let Some(b) = self.busqueda.as_mut() {
            b.hits = hits
                .into_iter()
                .map(|h| Hallazgo {
                    path: h.path,
                    // El índice devuelve rutas y parecidos, no clases.
                    kind: None,
                    score: Some(h.score),
                })
                .collect();
            b.viva = false;
        }
        let cambio = ViewChange::Search {
            search: self.vista_busqueda(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// Abre el prompt de la instrucción para un plan de renombrado.
    ///
    /// Lo que se teclea NO es un nombre: es lo que se le pide a un modelo.
    /// Nada muta aquí, y por eso el prompt no lleva la disciplina de bytes
    /// que lleva el de renombrar — el texto es para el daemon, no para el
    /// disco.
    fn pedir_instruccion_ia(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let dir = self.hueco().pane.dir().clone();
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-ai-rename".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![Self::linea_de_ruta(&dir)],
            overflow_note: String::new(),
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
            input: Some(String::new()),
            input_hostile: false,
        };
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            input_crudo: String::new(),
            reconocido: true,
            al_confirmar: Some(Pendiente::InstruccionIa { dir }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Le pide el plan al modelo. La respuesta vuelve al actor.
    ///
    /// Una época nueva por petición: entre pedirlo y que llegue, el lector
    /// puede haber descartado la revisión o haber pedido otra, y un plan
    /// viejo no se abre encima del que hay.
    fn lanzar_plan_ia(
        &mut self,
        dir: VPath,
        instruccion: String,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if instruccion.trim().is_empty() {
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "modal-ai-rename-empty-instruction",
            )));
            let cambio = ViewChange::Status(self.status.clone());
            return vec![self.parche(vec![cambio])];
        }
        self.epoca_ia += 1;
        let epoca = self.epoca_ia;
        // Los nombres del directorio que se PLANEA, guardados con la
        // petición: el cinturón de #275 exige que cada `from` exista donde se
        // va a aplicar, y para cuando el modelo conteste el lector puede
        // estar en otro sitio. Preguntarle al panel entonces validaría el
        // plan contra un directorio que no es el suyo.
        let nombres: Vec<Vec<u8>> = self
            .hueco()
            .pane
            .entries()
            .iter()
            .filter_map(|e| e.path.file_name().map(|s| s.as_bytes().to_vec()))
            .collect();
        self.ia_en_vuelo = Some((epoca, dir.clone(), nombres));
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = (tokio::time::timeout(PLAZO_IA, backend.ai_rename_plan(dir, instruccion))
                .await)
                .unwrap_or(Err(Error::ProviderUnavailable { retryable: true }));
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::PlanIa(
                    epoca,
                    Box::new(res),
                ))))
                .await;
        });
        // Y se DICE que se está pidiendo. Sin esto la tecla no producía nada
        // visible, así que el lector la volvía a pulsar — que es justo lo que
        // destapaba la carrera de las dos peticiones.
        self.status.message = Some(clamp_display(norte_i18n::t_in(
            self.lang,
            "host-plan-asking",
        )));
        let cambio = ViewChange::Status(self.status.clone());
        vec![self.parche(vec![cambio])]
    }

    /// Lo que contestó el modelo, revisado antes de enseñarlo.
    ///
    /// Dos cinturones, y los dos son de INGESTIÓN —no de presentación—, así
    /// que rechazan EN BLOQUE y ni abren la revisión:
    ///
    /// - un plan con más parejas de las que un directorio puede tener delata
    ///   a un daemon hostil inflando la respuesta;
    /// - una pareja que no es un `Segment` legal delata a uno roto o
    ///   adulterado, y aplicar «lo que valga» de un plan adulterado es
    ///   exactamente lo que no se puede hacer.
    fn aplicar_plan_ia(
        &mut self,
        epoca: u64,
        res: Result<norte_proto::methods::AiRenamePlanResult, Error>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // La petición EN VUELO tiene que ser esta. Un plan de otra época es
        // uno que el lector abandonó, y abrirlo es la aplicación moviéndose
        // sola.
        // `take_if` y NO `take().filter(...)`: `take` vacía el hueco ANTES de
        // que el filtro mire, así que una respuesta VIEJA se llevaba por
        // delante la petición VIVA. La secuencia era normal —pedir, no ver
        // nada, volver a pedir— y se quedaban las dos sin abrir, sin decir
        // nada y sin poder distinguirse de un daemon muerto.
        let Some((_, dir, nombres)) = self.ia_en_vuelo.take_if(|(e, _, _)| *e == epoca) else {
            return Vec::new();
        };
        let plan = match res {
            Ok(p) => p,
            Err(e) => return self.decir_de_ia(epoca, norte_frontend::error::error_key(&e)),
        };
        if plan.entries.is_empty() {
            return self.decir_de_ia(epoca, "msg-ai-rename-empty");
        }
        if plan.entries.len() > norte_frontend::MAX_AI_PLAN_ENTRIES {
            return self.decir_de_ia(epoca, "msg-ai-rename-invalid-plan");
        }
        // CONTRA el directorio que se PLANEÓ (#275), no contra lo que el
        // panel enseñe ahora: un plan adulterado no puede renombrar algo que
        // no estaba ahí, y el lector puede haberse ido a otro sitio mientras
        // el modelo pensaba.
        let Some(parejas) = norte_frontend::rename_pairs_in(&plan.entries, Some(&nombres)) else {
            return self.decir_de_ia(epoca, "msg-ai-rename-invalid-plan");
        };
        // El veredicto se pide EN EL MISMO viaje: la revisión necesita el
        // `plan_hash` para que aprobar haga algo, y un plan que se quedara
        // esperando a que alguien se lo pidiera después no tendría quién.
        // Va spawneado porque contra un directorio enorme es un `fs.list`
        // entero, y esperarlo aquí congelaría el actor.
        let b = Arc::clone(backend);
        let buz = buzon.clone();
        let d = dir.clone();
        let p = parejas.clone();
        tokio::spawn(async move {
            let res = b.rename_batch_plan(d, p).await;
            let _ = buz
                .send(Mensaje::Fondo(Box::new(Fondo::PlanDeLote(
                    epoca,
                    Box::new(res),
                ))))
                .await;
        });
        self.revision_ia = Some(RevisionIa {
            dir,
            entradas: plan.entries,
            parejas,
            plan: norte_frontend::BatchPlan::Pending,
            primera: 0,
            visto_hasta: norte_frontend::AI_RENAME_PAIR_LIMIT,
            reconocida: false,
            epoca,
        });
        let cambio = ViewChange::AiRename {
            ai_rename: self.vista_ia(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// El veredicto del core sobre el plan que hay en revisión.
    fn aplicar_plan_de_lote(
        &mut self,
        epoca: u64,
        res: Result<norte_proto::methods::FsRenameBatchPlanResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(r) = self.revision_ia.as_mut().filter(|r| r.epoca == epoca) else {
            return Vec::new();
        };
        let fallo = res.as_ref().err().map(norte_frontend::error::error_key);
        r.plan = match res {
            Ok(p) => norte_frontend::BatchPlan::Ready(Box::new(p)),
            // `Failed` no es «no aplicable»: es «no hay plan», o sea que no
            // hay `plan_hash` aprobado que mandar. El motivo concreto va a la
            // barra; aquí solo se sabe que aprobar no puede hacer nada.
            Err(_) => norte_frontend::BatchPlan::Failed,
        };
        let mut cambios = vec![ViewChange::AiRename {
            ai_rename: self.vista_ia(),
        }];
        if let Some(clave) = fallo {
            self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
            cambios.push(ViewChange::Status(self.status.clone()));
        }
        vec![self.parche(cambios)]
    }

    /// La proyección de la revisión, o `None` si no hay ninguna.
    ///
    /// Los nombres los propone un MODELO sobre nombres que escribió cualquiera:
    /// van los dos por el saneado canónico y cada uno dice si lo pintado
    /// difiere de lo real. Y van ENTEROS y por separado, jamás concatenados
    /// con una flecha — el mismo motivo que el destino de una transferencia.
    fn vista_ia(&self) -> Option<crate::dto::AiRenameView> {
        let r = self.revision_ia.as_ref()?;
        let linea = |texto: &str| {
            let (pintable, hostil) = norte_frontend::display_name(texto.as_bytes());
            crate::dto::DialogLine {
                text: clamp_display(pintable),
                hostile: hostil,
            }
        };
        let pairs = r
            .entradas
            .iter()
            .skip(r.primera)
            .take(norte_frontend::AI_RENAME_PAIR_LIMIT)
            .map(|e| crate::dto::AiRenamePairView {
                from: linea(&e.from),
                to: linea(&e.to),
            })
            .collect();
        let total = r.entradas.len();
        let hasta = (r.primera + norte_frontend::AI_RENAME_PAIR_LIMIT).min(total);
        Some(crate::dto::AiRenameView {
            dir: Self::linea_de_ruta(&r.dir),
            pairs,
            first_visible: r.primera as u64,
            total: total as u64,
            more_note: if hasta >= total {
                String::new()
            } else {
                clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "modal-ai-rename-more",
                    &[("shown", &hasta.to_string()), ("total", &total.to_string())],
                ))
            },
            // Lo que NO se ve también se dice: la marca de una línea solo
            // existe para la línea, y la pareja alterada puede estar en la
            // posición doce.
            hidden_hostile: r
                .entradas
                .iter()
                .enumerate()
                .filter(|(i, _)| *i < r.primera || *i >= hasta)
                .any(|(_, e)| {
                    norte_frontend::display_name(e.from.as_bytes()).1
                        || norte_frontend::display_name(e.to.as_bytes()).1
                }),
            // Traducido AQUÍ: un renderer no traduce, y de todo el cuerpo
            // esta es la línea que no se puede perder.
            status: clamp_display(norte_i18n::t_in(self.lang, r.plan.status_key())),
            // El detalle sale ENTERO de la capa compartida, marcas incluidas:
            // cada superficie pinta nombres que un atacante controla, y una
            // que lo derive por su cuenta es donde se pierde el saneado.
            detail: r
                .plan
                .detail_parts(r.parejas.len(), self.lang)
                .into_iter()
                .flat_map(|parte| self.lineas_de_detalle(&parte))
                .collect(),
            // Aprobar exige las DOS cosas: que el core lo acepte y que el
            // lector haya llegado al final. Lo segundo no lo puede saber el
            // core y lo primero no lo puede saber el lector.
            confirmable: r.plan.confirmable() && r.visto_hasta >= total,
            real_steps_note: if r.plan.ready().is_none() {
                String::new()
            } else {
                clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "modal-ai-rename-real-steps",
                    &[("n", &r.plan.real_steps().to_string())],
                ))
            },
            seen_all: r.visto_hasta >= total,
        })
    }

    /// Las teclas mientras la revisión está abierta.
    ///
    /// FIJAS, como las de la paleta y la ayuda, y por el mismo motivo: el
    /// catálogo no tiene comandos para «recorrer este plan» ni «aprobarlo».
    /// Son las que la propia pantalla anuncia en su pie.
    fn tecla_en_revision_ia(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Un acorde CON modificador no es una respuesta a esta pantalla: es
        // una tecla que iba a otro sitio. `tecla_en_quick` los rehúsa por lo
        // mismo, y aquí importa más — `ctrl+y` aprobaba un lote.
        if k.ctrl || k.alt || k.meta {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-key-unmapped".to_owned(),
                },
                Vec::new(),
            );
        }
        // La PRIMERA tecla solo reconoce la pantalla. Esta se abre sola,
        // decenas de segundos después del gesto que la pidió, y se queda el
        // teclado: sin este paso, la tecla que el lector iba a mandar a otra
        // cosa contestaba una pregunta que aún no sabía que tenía delante.
        // `Escape` es la excepción y no necesita reconocimiento: descartar es
        // seguro en los dos estados, y quien no quiere esto tiene que poder
        // quitárselo de encima a la primera.
        let reconocida = self.revision_ia.as_ref().is_some_and(|r| r.reconocida);
        if !reconocida && k.key != "Escape" && k.key != "esc" {
            if let Some(r) = self.revision_ia.as_mut() {
                r.reconocida = true;
            }
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "host-plan-acknowledge",
            )));
            let cambios = vec![
                ViewChange::AiRename {
                    ai_rename: self.vista_ia(),
                },
                ViewChange::Status(self.status.clone()),
            ];
            return (self.aplicada(), vec![self.parche(cambios)]);
        }
        let total = self.revision_ia.as_ref().map_or(0, |r| r.entradas.len());
        let ventana = norte_frontend::AI_RENAME_PAIR_LIMIT;
        let tope = total.saturating_sub(ventana);
        let pagina = i64::try_from(ventana).unwrap_or(1);
        let mover = |r: &mut RevisionIa, delta: i64| {
            let destino = i64::try_from(r.primera).unwrap_or(0).saturating_add(delta);
            r.primera = usize::try_from(destino.max(0)).unwrap_or(0).min(tope);
            // La marca de agua solo SUBE: recorrer hacia atrás no deshace lo
            // que ya se leyó.
            r.visto_hasta = r.visto_hasta.max((r.primera + ventana).min(total));
        };
        match k.key.as_str() {
            "ArrowDown" | "j" => {
                if let Some(r) = self.revision_ia.as_mut() {
                    mover(r, 1);
                }
            }
            "ArrowUp" | "k" => {
                if let Some(r) = self.revision_ia.as_mut() {
                    mover(r, -1);
                }
            }
            "PageDown" => {
                if let Some(r) = self.revision_ia.as_mut() {
                    mover(r, pagina);
                }
            }
            "PageUp" => {
                if let Some(r) = self.revision_ia.as_mut() {
                    mover(r, -pagina);
                }
            }
            "Escape" | "n" | "N" => return self.cerrar_revision_ia(),
            // `Enter` NO aprueba, y esto rompe la paridad con el TUI a
            // propósito. Allí el plan lo abre una tecla del lector y la
            // siguiente tecla es una respuesta; aquí la pantalla se abre sola
            // decenas de segundos después, y `Enter` es justo la tecla con la
            // que se estaba recorriendo el árbol mientras el modelo pensaba.
            // Dos `Enter` seguidos entrando en directorios anidados son
            // normales; que el segundo apruebe un renombrado de lote, no.
            // Queda `y` —que el reconocimiento protege— y el botón, que es un
            // gesto que no se puede confundir con otra cosa.
            "y" | "Y" => return self.aprobar_revision_ia(backend, buzon),
            _ => {
                return (
                    ActionAck::Unavailable {
                        reason_key: "host-key-unmapped".to_owned(),
                    },
                    Vec::new(),
                );
            }
        }
        let cambio = ViewChange::AiRename {
            ai_rename: self.vista_ia(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Contesta a la revisión con un gesto DIRIGIDO a ella (un botón).
    ///
    /// No necesita el reconocimiento que sí necesita una tecla: un clic en
    /// un botón de esta pantalla no puede ser un gesto que iba a otro sitio.
    fn decidir_revision_ia(
        &mut self,
        approve: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.revision_ia.is_none() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        if let Some(r) = self.revision_ia.as_mut() {
            r.reconocida = true;
        }
        if approve {
            self.aprobar_revision_ia(backend, buzon)
        } else {
            self.cerrar_revision_ia()
        }
    }

    /// Descarta el plan sin aplicar nada.
    fn cerrar_revision_ia(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // No se sube la época, y no se toca `ia_en_vuelo`. Las dos cosas
        // parecen prudencia y una de ellas era un bug:
        //
        // - La época no hace falta. El veredicto tardío ya no encuentra
        //   revisión que actualizar, y una petición NUEVA sube la época ella
        //   misma.
        // - `ia_en_vuelo` NO puede ser la petición de esta revisión: se la
        //   llevó `aplicar_plan_ia` al abrirla. Si hay algo ahí es una
        //   petición POSTERIOR, y soltarla aquí la mataba en silencio —
        //   descartar un plan que se está leyendo no es abandonar el que se
        //   acaba de pedir.
        self.revision_ia = None;
        let cambio = ViewChange::AiRename { ai_rename: None };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Aprueba el plan: UNA Task para el lote entero, un solo deshacer.
    ///
    /// Solo si el CORE lo marcó aplicable, y con el `plan_hash` que él mismo
    /// devolvió: lo que se ejecuta es exactamente lo que se enseñó. Un plan
    /// sin veredicto, o con uno que dice que no, no se aprueba y se dice.
    fn aprobar_revision_ia(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(r) = self.revision_ia.as_ref() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // La segunda cerradura, también aquí. `rechaza_por_solo_lectura` mira
        // los DIÁLOGOS, y esta es una pantalla propia: hoy es inalcanzable en
        // solo lectura porque las dos vías que la abren están cerradas, pero
        // esa es exactamente la condición que deja de valer en cuanto alguien
        // añade la tercera. Aprobar un plan ejecuta N movimientos.
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return Self::no_muta();
        }
        if r.visto_hasta < r.entradas.len() {
            // Y se dice CUÁL de las dos cosas falta: «el core no lo acepta» y
            // «todavía no lo has leído entero» se arreglan de formas
            // distintas.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-plan-unseen".to_owned(),
                },
                Vec::new(),
            );
        }
        let Some(plan) = r.plan.ready().filter(|p| p.executable) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-plan-not-applicable".to_owned(),
                },
                Vec::new(),
            );
        };
        let (dir, parejas, hash) = (r.dir.clone(), r.parejas.clone(), plan.plan_hash.clone());
        let afectados = vec![dir.clone()];
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        tokio::spawn(async move {
            let mensaje = match backend2.rename_batch(dir, parejas, hash).await {
                Ok(task) => Mensaje::TaskNueva(Box::new((task, afectados, None))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon2.send(mensaje).await;
        });
        self.cerrar_revision_ia()
    }

    /// Lo dice en la barra y no abre nada. Cierra la revisión si la había:
    /// un plan que no se pudo pedir no deja media pantalla abierta.
    fn decir_de_ia(&mut self, epoca: u64, clave: &str) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
        let mut cambios = vec![ViewChange::Status(self.status.clone())];
        // Solo se cierra la revisión de ESTA época. Cerrar la que hubiera
        // tiraba un plan bueno, ya con veredicto y a punto de aprobarse,
        // porque OTRA petición posterior había fallado.
        if self.revision_ia.take_if(|r| r.epoca == epoca).is_some() {
            cambios.push(ViewChange::AiRename { ai_rename: None });
        }
        vec![self.parche(cambios)]
    }

    /// Abre el nombre de la entrada bajo el cursor, para editarlo. NO
    /// renombra.
    ///
    /// Con VARIAS marcas se niega, y eso NO es lo mismo que hace el TUI: el
    /// TUI renombra la del cursor e ignora las marcas. La tabla compartida
    /// documenta la asimetría en `Facts::rename_single` y deja que cada
    /// frontend conteste; este host ya contestaba «una sola» en `hechos()`,
    /// así que atenuar la fila y luego renombrar de todas formas habría sido
    /// la ayuda mintiendo sobre la tecla.
    fn pedir_rename(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let hueco = self.hueco();
        if hueco.pane.marks_len() > 1 {
            return (
                ActionAck::Unavailable {
                    reason_key: norte_frontend::availability::reason_key(
                        norte_help::Reason::WrongTarget,
                    )
                    .to_owned(),
                },
                Vec::new(),
            );
        }
        let Some(from) = hueco.pane.selected().map(|e| e.path.clone()) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-nothing-selected".to_owned(),
                },
                Vec::new(),
            );
        };
        let Some(nombre) = from.file_name() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-cannot-transfer-root".to_owned(),
                },
                Vec::new(),
            );
        };
        // La siembra es lo que la FILA pinta, con el saneado canónico Y con
        // la reinterpretación que el panel tenga puesta: editar produce el
        // texto que se ve, y desde #57 la fila puede estar transcodificada.
        // Sembrar sin ella dejaba `CAF<FFFD>.TXT` bajo una fila que decía
        // `CAFÉ.TXT`. Para un nombre que sigue sin ser representable eso
        // lleva un U+FFFD, y ese residuo es justo lo que el guard de la
        // confirmación no deja escribir.
        let (pintable, hostil) =
            norte_frontend::display_name_with(nombre.as_bytes(), hueco.pane.name_encoding());
        let siembra = clamp_display(pintable.clone());
        if siembra != pintable {
            // El recorte le pega una elipsis al final, y `…` es un carácter
            // LEGAL en un nombre: ni se enmascara ni se marca. Editar ese
            // campo y confirmar escribiría el recorte en el disco como parte
            // del nombre, sin que nada lo dijera — y el guard del U+FFFD no
            // lo ve, porque el recorte pasa DESPUÉS de que `display_name`
            // haya dado su veredicto. Se rehúsa abrirlo, que es lo único
            // honesto: el nombre no cabe, así que aquí no se puede editar.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-name-not-editable".to_owned(),
                },
                Vec::new(),
            );
        }
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-rename-title".to_owned(),
            // Un rename no va a ninguna parte: se queda donde está.
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![Self::linea_de_ruta(&from)],
            overflow_note: String::new(),
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
            input: Some(siembra.clone()),
            input_hostile: hostil,
        };
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            // El crudo arranca IGUAL que la siembra: es lo que permite
            // reconocer «no lo ha tocado» sin llevar una bandera aparte.
            input_crudo: siembra.clone(),
            reconocido: true,
            al_confirmar: Some(Pendiente::Renombrar { from, siembra }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Los bytes que un rename confirmado va a escribir, o la clave del
    /// motivo por el que no hay ninguno.
    ///
    /// Tres reglas, y las tres son de la regla 1:
    ///
    /// - **Sin tocar**, se reconstruyen los BYTES ORIGINALES — y entonces el
    ///   destino es el origen, así que el resultado es siempre «mismo nombre,
    ///   mismo sitio». La rama no renombra NADA, y está para que la siembra
    ///   no pueda convertirse en el operando: la proyección de pantalla no es
    ///   reversible para un nombre que no es UTF-8.
    ///
    ///   La consecuencia hay que decirla porque no es evidente: un nombre que
    ///   no es UTF-8 válido **no se puede renombrar desde esta ventana**. Sin
    ///   tocar da «mismo nombre»; tocado lleva el U+FFFD que la pantalla puso
    ///   y no se puede teclear alrededor de él. Es fail-closed y deliberado
    ///   —lo contrario sería escribir mojibake— pero es una limitación, no una
    ///   protección que funcione.
    /// - **Tocado y con un U+FFFD dentro**, se rehúsa: ese carácter lo puso
    ///   la pantalla, y confirmarlo escribiría mojibake de verdad. El guard
    ///   no distingue residuo de intención, así que también rehúsa un U+FFFD
    ///   TECLEADO — asimetría deliberada con crear un directorio, que no
    ///   tiene siembra de la que heredar residuos.
    /// - **El mismo nombre en el mismo sitio** no es una operación.
    fn bytes_del_rename(from: &VPath, siembra: &str, escrito: &str) -> Result<VPath, &'static str> {
        let bytes = if escrito == siembra {
            from.file_name()
                .map(|n| n.as_bytes().to_vec())
                .unwrap_or_default()
        } else {
            if escrito.contains('\u{FFFD}') {
                return Err("msg-transfer-name-fffd");
            }
            escrito.as_bytes().to_vec()
        };
        let seg = norte_proto::Segment::new(bytes).map_err(|_| "err-bad-name")?;
        let destino = from.parent().ok_or("host-cannot-transfer-root")?.join(seg);
        if destino == *from {
            return Err("msg-transfer-name-same");
        }
        Ok(destino)
    }

    /// El DIRECTORIO al que va una transferencia, o por qué no hay uno.
    ///
    /// El destino declarado tiene que seguir sirviendo: existir, verse, y no
    /// ser uno mismo —copiarse encima no es una operación—.
    ///
    /// Sin destino hay dos situaciones DISTINTAS, y decir la misma frase en
    /// las dos manda a buscar otro panel a quien tiene tres. La capa
    /// compartida deja el rol SIN FIJAR cuando hay varios candidatos y
    /// ninguno elegido (ADR 0058 D7): eso no es «no hay otro», es «elige
    /// cuál».
    fn directorio_destino(&self) -> Result<VPath, &'static str> {
        self.hueco_destino()
            .map(|id| self.huecos[&id].pane.dir().clone())
    }

    /// El HUECO destino, con la misma regla que [`Self::directorio_destino`].
    ///
    /// Los dos por el mismo camino: una comparación necesita el hueco (para
    /// navegar el lado derecho) y una transferencia necesita su directorio,
    /// y dos formas de decidir «el otro panel» son dos sitios donde divergir.
    fn hueco_destino(&self) -> Result<u32, &'static str> {
        let activo = self.activo();
        let destino_id = self
            .roles
            .get(RoleId::Target)
            .map(|SlotId(id)| id)
            .filter(|id| *id != activo && self.huecos.contains_key(id) && !self.oculto(*id));
        if let Some(id) = destino_id {
            return Ok(id);
        }
        let candidatos = self
            .huecos
            .keys()
            .filter(|id| **id != activo && !self.oculto(**id))
            .count();
        Err(if candidatos > 1 {
            "host-no-target-designated"
        } else {
            "host-no-other-slot"
        })
    }

    /// Abre la confirmación de una copia o un movimiento. NO transfiere.
    ///
    /// El origen son las marcas del hueco activo (o el cursor si no hay
    /// ninguna) y el destino es el DIRECTORIO del hueco con el rol `Target`.
    /// Ni una ni otro los nombra el renderer: manda `pane.copy` y punto. Es
    /// la misma regla que dejó sin parámetro al comando de los bytes de una
    /// imagen (ADR 0069), y por el mismo motivo — un nombre que viene de la
    /// webview es un nombre que la webview puede elegir.
    /// ¿Hay dos entradas del lote cuyos NOMBRES son uno solo en el destino?
    ///
    /// Se pliega con la clave compartida bajo el modo del DESTINO —la trampa
    /// del dominio de siempre: la caja y la normalización las decide el sitio
    /// al que van, no el del que salen—. Sin modo todavía (el hueco acaba de
    /// aterrizar, o el daemon no contestó) no se pliega: esto es una cortesía
    /// del cliente y la autoridad es el core.
    fn dos_marcas_pliegan_igual(&self, paths: &[VPath]) -> bool {
        let Some(modo) = self
            .hueco_destino()
            .ok()
            .and_then(|id| self.huecos.get(&id))
            .and_then(|h| h.pliegue)
        else {
            return false;
        };
        if modo == norte_encoding::FoldMode::None {
            return false;
        }
        let mut vistas = std::collections::HashSet::new();
        paths
            .iter()
            .filter_map(|p| p.file_name())
            .any(|n| !vistas.insert(norte_encoding::name_key(n.as_bytes(), modo)))
    }

    /// Los dos topes de un lote (#271), o `None` si cabe.
    ///
    /// Se preguntan antes de abrir diálogo alguno: preguntar por algo que no
    /// se va a poder hacer es peor que decirlo de entrada.
    fn lote_no_cabe(&self, cuantas: usize) -> Option<&'static str> {
        if cuantas > MAX_TRANSFER_BATCH {
            return Some("host-batch-too-large");
        }
        // Y que quepa en lo que el host RETIENE: el desalojo solo puede tirar
        // tasks terminales, así que un lote sobre un tablero ya lleno de vivas
        // no tendría dónde caer.
        if self.tasks.len().saturating_add(cuantas) > MAX_TASKS_RETAINED {
            return Some("host-task-board-full");
        }
        None
    }

    /// Sobre QUÉ opera una transferencia hacia `destino`, o el motivo por el
    /// que no se puede preguntar siquiera. Devuelve `(origen_dir, paths)`.
    ///
    /// El destino llega por PARÁMETRO desde #284: casi siempre sale del rol
    /// compartido, pero con un solo listado en pantalla lo elige el lector en
    /// el selector del escritorio, y las dos formas tienen que pasar por las
    /// mismas comprobaciones.
    fn operandos_de_transferencia(
        &self,
        destino: &VPath,
    ) -> Result<(VPath, Vec<VPath>), &'static str> {
        let destino = destino.clone();
        let origen_dir = self.hueco().pane.dir().clone();
        if origen_dir == destino {
            // Los dos listados en el mismo sitio. El daemon lo rechazaría
            // igual, pero abrir un diálogo que promete algo imposible es
            // peor que decirlo antes.
            //
            // BYTE A BYTE a propósito (#269): en un volumen que pliega,
            // `/casa/docs` y `/casa/DOCS` son el mismo sitio y este atajo NO
            // los ve. Saberlo cuesta un `fs.capabilities` —o sea un viaje al
            // daemon delante de CADA diálogo de copia—, y el error de este
            // lado solo puede ser por PERMISIVO: la autoridad es
            // `norte_core::ops`, que sí pliega (#215) y devuelve
            // `InvalidPath`. Ser más estricto aquí sí rompería algo: negaría
            // una operación legítima en un volumen sensible a la caja.
            return Err("host-same-directory");
        }
        // `marked_paths` ya cae al cursor cuando no hay marcas: es la fuente
        // única de «sobre qué opera esto», y duplicar aquí ese respaldo
        // sería un segundo sitio del que se pueden separar.
        let paths: Vec<VPath> = self.hueco().pane.marked_paths();
        if paths.is_empty() {
            return Err("msg-nothing-selected");
        }
        if let Some(motivo) = self.lote_no_cabe(paths.len()) {
            return Err(motivo);
        }
        // Dos marcas que PLIEGAN al mismo nombre en el destino (#268): en un
        // ext4 `README.txt` y `readme.txt` son dos ficheros, y en NTFS o APFS
        // son uno. Encolar las dos deja que una gane —cuál, no es
        // determinista— y que la otra falle sin explicación sobre un miembro
        // arbitrario de la pareja. Con `CollisionPolicy::Fail` el resultado es
        // al menos un error visible; el día que la ventana ofrezca elegir
        // sobrescribir, el mismo lote pierde un fichero en silencio.
        if self.dos_marcas_pliegan_igual(&paths) {
            return Err("host-batch-folds-to-one");
        }
        // Una entrada sin último segmento es una RAÍZ, y una raíz no tiene
        // nombre que componer en el destino. Se rechaza el lote entero en vez
        // de saltársela: transferir «casi todo lo que pediste» en silencio es
        // exactamente lo que no puede hacer una mutación.
        if paths.iter().any(|p| p.file_name().is_none()) {
            return Err("host-cannot-transfer-root");
        }
        let _ = destino;
        Ok((origen_dir, paths))
    }

    /// Abre la confirmación de copiar o mover, resolviendo el destino.
    ///
    /// Con un solo listado en pantalla no hay panel destino, y hasta #284 eso
    /// era el final del camino: la operación se rehusaba y quien no había
    /// partido la ventana no podía copiar. Ahora se le pregunta al escritorio.
    fn pedir_transferencia(&mut self, mover: bool) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match self.directorio_destino() {
            Ok(destino) => self.confirmar_transferencia(&destino, mover),
            // Sin OTRO hueco al que apuntar: lo elige el lector fuera.
            Err("host-no-other-slot") => self.pedir_destino_al_escritorio(mover),
            Err(reason_key) => (
                ActionAck::Unavailable {
                    reason_key: reason_key.to_owned(),
                },
                Vec::new(),
            ),
        }
    }

    /// Cuántas filas caben en el visor, según lo mide el renderer.
    ///
    /// Al menos una: un visor de cero filas no pinta nada y su paginación
    /// dividiría por cero.
    fn fijar_filas_del_visor(&mut self, rows: u32) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.visor_filas = Some(usize::try_from(rows).unwrap_or(1).max(1));
        let cambio = ViewChange::Viewer {
            viewer: self.vista_visor(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Le pide al ESCRITORIO que el lector elija el destino (#284).
    ///
    /// Se recuerda solo el VERBO —copiar o mover—, no los operandos: cuando la
    /// respuesta vuelva se recalculan del estado de entonces. Congelar aquí
    /// las marcas sería prometer una operación sobre un listado que el lector
    /// pudo cambiar mientras el selector estaba abierto.
    fn pedir_destino_al_escritorio(
        &mut self,
        mover: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // El directorio del panel es solo la SUGERENCIA de dónde abrir el
        // selector, y por eso no se exige que sea local: lo que el selector
        // devuelve es siempre una carpeta de esta máquina, y copiar de un
        // `sftp://` a una carpeta local es una operación legítima que el core
        // hace desde siempre. Con un panel remoto, quien ejecuta abre donde
        // pueda — la sugerencia se pierde, la operación no.
        let desde = self.hueco().pane.dir().clone();
        if !self.nativo(crate::dto::NativeEffect::PickDirectory { desde }) {
            return Self::sin_escritorio();
        }
        self.destino_pendiente = Some(mover);
        (self.aplicada(), self.decir("host-pick-destination"))
    }

    /// Volvió el selector del escritorio (#284).
    ///
    /// `None` = se cerró sin elegir, y entonces no pasa nada: cancelar es una
    /// respuesta. Con una ruta, se confirma como cualquier otra transferencia
    /// — y eso significa que el destino se ENSEÑA antes de mover un byte, que
    /// es lo que acota que la ruta haya pasado por el renderer.
    fn destino_elegido(
        &mut self,
        path: Option<String>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(mover) = self.destino_pendiente.take() else {
            // Nadie pidió un destino: una respuesta que no contesta a ninguna
            // pregunta no se interpreta.
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let Some(nativa) = path else {
            return (self.aplicada(), Vec::new());
        };
        let Some(destino) = norte_frontend::shell::vpath_de_ruta_nativa(&nativa) else {
            let fuera = self.decir("host-bad-destination");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-bad-destination".to_owned(),
                },
                fuera,
            );
        };
        self.confirmar_transferencia(&destino, mover)
    }

    /// La confirmación propiamente dicha, con el destino ya resuelto.
    fn confirmar_transferencia(
        &mut self,
        destino: &VPath,
        mover: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let activo = self.activo();
        let destino = destino.clone();
        let (origen_dir, paths) = match self.operandos_de_transferencia(&destino) {
            Ok(t) => t,
            Err(reason_key) => {
                return (
                    ActionAck::Unavailable {
                        reason_key: reason_key.to_owned(),
                    },
                    Vec::new(),
                );
            }
        };
        // El destino va en SU CAMPO, no como una línea con una flecha: un
        // directorio puede llamarse `docs → /casa/BORRAR` y esa flecha es
        // legítima, no se enmascara y no se marca, así que la línea se leería
        // como dos rutas y quien confirma creería estar mandando sus ficheros
        // a la segunda (fixture `arrow_join_spoof` del corpus canónico).
        let destino_linea = Self::linea_de_ruta(&destino);
        // Y los orígenes, enmascarados y acotados igual que el listado: estos
        // nombres los controla quien haya escrito en el directorio.
        let cuerpo: Vec<crate::dto::DialogLine> = paths
            .iter()
            .take(Self::MAX_LINEAS_DIALOGO)
            .map(Self::linea_de_ruta)
            .collect();
        let nota = self.nota_de_recorte(cuerpo.len(), paths.len());
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: if mover {
                "modal-move-title"
            } else {
                "modal-copy-title"
            }
            .to_owned(),
            destination: Some(destino_linea),
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: cuerpo,
            overflow_note: nota,
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    // Ni copiar ni mover se marcan destructivos, y es una
                    // decisión: `destructive` es lo que hace que `Enter`
                    // elija cancelar, y F5/F6 son las dos teclas que más se
                    // pulsan de un gestor ortodoxo. Lo que destruye es el
                    // borrado, y ese sí lo lleva.
                    destructive: false,
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
            reconocido: true,
            al_confirmar: Some(Pendiente::Transferir {
                origen: activo,
                origen_dir,
                paths,
                destino,
                mover,
            }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
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
        let cuerpo: Vec<crate::dto::DialogLine> = paths
            .iter()
            .take(Self::MAX_LINEAS_DIALOGO)
            .map(Self::linea_de_ruta)
            .collect();
        let nota = self.nota_de_recorte(cuerpo.len(), paths.len());
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
            // Un borrado no va a ninguna parte.
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: cuerpo,
            overflow_note: nota,
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
            reconocido: true,
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
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // La MISMA aprobación puede llegar dos veces: el SDK resincroniza
        // `policy.pending` en cada reconexión, y lo que sigue vivo vuelve por
        // el canal. Dos diálogos son dos respuestas, y la segunda cae sobre
        // un id que el daemon ya cerró.
        if self.dialogos.iter().any(|d| {
            matches!(
                d.al_confirmar,
                Some(Pendiente::Decidir { approval_id, .. }) if approval_id == req.approval_id
            )
        }) {
            return Vec::new();
        }
        // Se APUNTA la sesión que pidió, aunque el diálogo no llegue a
        // abrirse por lo que sea: es lo ÚNICO que nombra a un agente en todo
        // el protocolo, y sin ese apunte no hay forma de ofrecer deshacer lo
        // que hizo salvo tecleando su id a mano (#276).
        let mut fuera = Vec::new();
        if let Some(sesion) = req.session.as_deref() {
            self.agencia.sesiones.vista(sesion, &req.op);
            // Y se REPINTA si el panel está abierto. La lista cambia SIN
            // gesto —esta petición la reordena— y un renderer al que no se
            // le dice se queda pintando el orden de antes: la fila que el
            // lector ve resaltada deja de ser la que el host tiene elegida, y
            // `u` deshace el trabajo de otra sesión.
            if self.agencia.panel {
                fuera.push(self.parche(vec![ViewChange::Agents {
                    agents: self.vista_agentes(),
                }]));
            }
        }
        // Estas rutas vienen del daemon como TEXTO ya redactado, no como
        // `VPath`, así que el enmascarado es el de cadenas y la marca se
        // calcula comparando: si enmascarar cambió algo, lo que se lee no es
        // lo que hay, y quien aprueba tiene que verlo.
        let linea = |texto: &str| {
            let enmascarado = norte_encoding::mask_terminal_hazards(texto);
            // DOS motivos para marcar, y el segundo es el que faltaba: estas
            // rutas llegan REDACTADAS del daemon, que ya pasó los bytes por
            // `display_lossy` —controles, overrides bidi y bytes inválidos ya
            // son U+FFFD—, así que comparar contra el original no detecta
            // nada de eso y la marca no saltaba justo en la clase más
            // peligrosa. Encima era inconsistente: un `zwsp` sí la encendía,
            // porque el lossy del daemon no lo toca.
            //
            // El carácter de sustitución ES la señal de que lo que se lee no
            // es lo que hay. No se puede recuperar qué había —por eso el
            // daemon manda texto y no `VPath`— pero sí decir que no es fiel.
            let hostil = enmascarado != texto || texto.contains('\u{FFFD}');
            crate::dto::DialogLine {
                text: clamp_display(enmascarado),
                hostile: hostil,
            }
        };
        // El cuerpo son SOLO las rutas: el renderer las numera por posición,
        // que es una etiqueta que ningún nombre de fichero puede escribir. Lo
        // demás —qué se pide, quién lo pide, cuándo caduca— va en campos
        // propios, por el mismo motivo que el destino de una transferencia:
        // entre líneas de rutas, una ruta suplanta a cualquier otra línea.
        let cuerpo: Vec<crate::dto::DialogLine> = req
            .paths
            .iter()
            .take(Self::MAX_LINEAS_DIALOGO)
            .map(|p| linea(p))
            .collect();
        let sujeto = linea(&req.op);
        // Quién pide es lo PRIMERO que hace falta para decidir, y se
        // descartaba: el título dice «aprobación de agente» y sin esto no se
        // sabe de qué agente.
        let quien = req.session.as_deref().map(linea);
        // Si la lista viene RECORTADA hay que decirlo: aprobar creyendo que
        // son tres rutas cuando son mil es aprobar otra cosa (0.36.0). Y son
        // DOS recortes: el del daemon (`paths_total`) y el nuestro. El
        // recuento honesto es el mayor de los dos.
        //
        // La frase va en `overflow_note` y no como una línea más del cuerpo,
        // por el mismo motivo que el destino de una transferencia tiene campo
        // propio: entre líneas de rutas, una ruta la puede suplantar. Antes
        // era una línea Y encima citaba `modal-approval-truncated`, una clave
        // Fluent que no existe en ningún idioma — o sea que un lote recortado
        // pintaba el identificador crudo.
        let total = std::cmp::max(req.paths_total, req.paths.len() as u64);
        let mostrados = req.paths.len().min(Self::MAX_LINEAS_DIALOGO);
        let nota = self.nota_de_recorte(mostrados, usize::try_from(total).unwrap_or(usize::MAX));
        // Cuánto le queda, DICHO y en su propio campo. Una decisión con fecha
        // de caducidad que no la enseña se lee como una que espera para
        // siempre, y quien vuelve al rato pulsa aprobar sobre algo que el
        // daemon ya denegó.
        //
        // Con `ttl_ms == 0` —DESCONOCIDO: una pendiente reconstruida por el
        // resync de `policy.pending` no transporta el TTL restante— se dice
        // que no se sabe, en vez de callar: callar deja el diálogo delante
        // invitando a aprobar sobre un id que el daemon puede haber reapado
        // hace rato. Y sin línea de plazo, un fichero llamado «caduca en
        // 3600 s» sería la única que lo pareciera.
        let plazo = Some(if req.ttl_ms > 0 {
            clamp_display(norte_i18n::ta_in(
                self.lang,
                "modal-approval-ttl",
                &[("s", &req.ttl_ms.div_ceil(1000).to_string())],
            ))
        } else {
            clamp_display(norte_i18n::t_in(self.lang, "modal-approval-ttl-unknown"))
        });
        // Y CUÁNDO vence, para que el renderer cuente en vez de repetir una
        // frase congelada (#279). Solo con un TTL conocido: contar hacia atrás
        // desde un plazo inventado sería peor que no contar.
        let vence_en = (req.ttl_ms > 0)
            .then(|| i64::try_from(req.ttl_ms).ok().map(|ms| ahora_ms() + ms))
            .flatten();
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-approval-title".to_owned(),
            destination: None,
            subject: Some(sujeto),
            asker: quien,
            deadline: plazo,
            deadline_at_ms: vence_en,
            body: cuerpo,
            overflow_note: nota,
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
        let caidos = self.apilar_dialogo(Dialogo {
            id,
            vista: vista.clone(),
            input_crudo: String::new(),
            // Se abre SOLA: la trae una op de un agente, no una tecla.
            reconocido: false,
            al_confirmar: Some(Pendiente::Decidir {
                approval_id: req.approval_id,
                session: req.session.clone(),
            }),
        });
        // Y se programa su caducidad. El daemon deja de aceptar el id cuando
        // el TTL se acaba: un diálogo que siguiera delante invitaría a
        // aprobar en el vacío, y quien lo hiciera se quedaría creyendo que
        // autorizó lo que en realidad quedó denegado por silencio.
        if req.ttl_ms > 0 {
            let buzon = buzon.clone();
            let approval_id = req.approval_id;
            let plazo = std::time::Duration::from_millis(req.ttl_ms);
            tokio::spawn(async move {
                tokio::time::sleep(plazo).await;
                let _ = buzon.send(Mensaje::AprobacionCaducada(approval_id)).await;
            });
        }
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        let mut salidas = vec![self.parche(vec![cambio])];
        salidas.extend(caidos);
        salidas
    }

    /// El TTL de una aprobación se acabó: su diálogo se cierra y se dice.
    ///
    /// No se manda `policy.decide`: el daemon ya la resolvió por su cuenta
    /// —un TTL vencido es una denegación—, y contestar sobre un id cerrado
    /// solo produce un error que no significa nada para quien lo lee.
    fn caduca_aprobacion(&mut self, approval_id: u64) -> Vec<BridgeEnvelope<UiUpdate>> {
        let antes = self.dialogos.len();
        self.dialogos.retain(|d| {
            !matches!(
                d.al_confirmar,
                Some(Pendiente::Decidir { approval_id: id, .. }) if id == approval_id
            )
        });
        if self.dialogos.len() == antes {
            // Ya se había contestado: la caducidad llega y no hay nada que
            // cerrar. No es un error, y no se dice nada.
            return Vec::new();
        }
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        let mut salidas = vec![self.parche(vec![cambio])];
        // NOMBRA la que caducó (#279). Con dos apiladas, «la aprobación
        // caducó» no dice cuál se cerró sola ni cuál sigue esperando.
        salidas.extend(self.decir_con("msg-approval-expired", &[("id", &approval_id.to_string())]));
        salidas
    }

    /// Abre el prompt de crear directorio, con su campo de texto vacío.
    /// Arranca el buscador incremental del listado.
    ///
    /// Filtrar es el modo por defecto: es el que no mueve el listado bajo el
    /// cursor mientras se teclea.
    fn buscar_rapido(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.hueco_mut()
            .pane
            .quick_start(norte_frontend::nav::Mode::Filter);
        (self.aplicada(), vec![self.parche_filas()])
    }

    /// Los gestos que operan sobre lo MARCADO —o lo que hay bajo el cursor— y
    /// lanzan una task: contar, empaquetar, desempaquetar y comprobar (#132,
    /// #139, #290).
    ///
    /// Juntos por la misma razón que los de disposición: `aplicar_efecto` es
    /// un despachador y no puede crecer un brazo por cada gesto nuevo.
    fn efecto_sobre_entradas(
        &mut self,
        efecto: Efecto,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match efecto {
            Efecto::TamanoDeDirectorio => self.contar_tamano(backend, buzon),
            Efecto::Empaquetar => self.pedir_empaquetado(),
            Efecto::Desempaquetar => self.desempaquetar(backend, buzon),
            Efecto::ComprobarArchivo => self.comprobar_archivo(backend, buzon),
            Efecto::PartirFichero => self.pedir_partido(),
            Efecto::Juntar => self.juntar_trozos(backend, buzon),
            // El llamante ya filtró: nombrarlos aquí es lo que hace que
            // añadir uno más sea un error de compilación.
            _ => (self.aplicada(), Vec::new()),
        }
    }

    /// `pane.pack` (#132, #290): pide el NOMBRE del contenedor.
    ///
    /// El nombre se teclea porque de él sale el formato. Aquí no se valida
    /// nada más que haya algo que empaquetar: la extensión se resuelve al
    /// confirmar, que es cuando hay nombre.
    fn pedir_empaquetado(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // `marked_paths` cae al cursor sin marcas, igual que en una
        // transferencia: una sola fuente de «sobre qué opera esto».
        let sources: Vec<VPath> = self.hueco().pane.marked_paths();
        if sources.is_empty() {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-nothing-selected".to_owned(),
                },
                self.decir("msg-nothing-selected"),
            );
        }
        let dir = self.hueco().pane.dir().clone();
        let donde = Self::linea_de_ruta(&dir);
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-pack-title".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![donde],
            overflow_note: String::new(),
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
            input: Some(String::new()),
            input_hostile: false,
        };
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            input_crudo: String::new(),
            reconocido: true,
            al_confirmar: Some(Pendiente::Empaquetar { dir, sources }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// `pane.split-file` (#132, #290): pide el TAMAÑO de los trozos.
    ///
    /// Los trozos van al panel destino, como una copia y por lo mismo: partir
    /// un fichero de un giga en el sitio donde ya está suele no caber.
    fn pedir_partido(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(entrada) = self.hueco().pane.selected().cloned() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-nothing-selected".to_owned(),
                },
                self.decir("msg-nothing-selected"),
            );
        };
        let dest_dir = match self.directorio_destino() {
            Ok(d) => d,
            Err(reason_key) => {
                return (
                    ActionAck::Unavailable {
                        reason_key: reason_key.to_owned(),
                    },
                    self.decir(reason_key),
                );
            }
        };
        let donde = Self::linea_de_ruta(&dest_dir);
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-split-title".to_owned(),
            destination: Some(donde),
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![crate::dto::DialogLine {
                text: clamp_display(norte_i18n::t_in(self.lang, "modal-split-hint")),
                hostile: false,
            }],
            overflow_note: String::new(),
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
            input: Some(String::new()),
            input_hostile: false,
        };
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            input_crudo: String::new(),
            reconocido: true,
            al_confirmar: Some(Pendiente::Partir {
                path: entrada.path.clone(),
                dest_dir,
            }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// `pane.combine-files` (#132, #290): junta los trozos desde el `.001`
    /// bajo el cursor.
    ///
    /// Solo desde el PRIMERO, y la regla vive en el crate compartido: empezar
    /// por el `.007` uniría media cosa, y el core solo busca hacia delante.
    fn juntar_trozos(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(entrada) = self.hueco().pane.selected().cloned() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-nothing-selected".to_owned(),
                },
                self.decir("msg-nothing-selected"),
            );
        };
        let nombre = entrada
            .path
            .file_name()
            .map(|s| s.as_bytes().to_vec())
            .unwrap_or_default();
        let Some(base) = norte_frontend::nav::base_de_trozos(&nombre) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-combine-needs-first".to_owned(),
                },
                self.decir("msg-combine-needs-first"),
            );
        };
        let dir = self.hueco().pane.dir().clone();
        let params = norte_proto::methods::FileCombineParams {
            first: entrada.path.clone(),
            dest: dir.join(base),
        };
        let afectados = vec![dir];
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let mensaje = match backend.combine_files(params).await {
                Ok(task) => Mensaje::TaskNueva(Box::new((task, afectados, None))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon.send(mensaje).await;
        });
        (self.aplicada(), self.decir("msg-combine-started"))
    }

    /// `pane.unpack` (#132, #290): copia el INTERIOR del contenedor bajo el
    /// cursor al panel destino.
    ///
    /// Sin método propio y sin hacerle falta: el motor de copia acepta el
    /// interior de un archivo como origen, así que esto es la copia que el
    /// lector podría haber hecho a mano — con su journal, su undo y su
    /// cancelación.
    fn desempaquetar(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(entrada) = self.hueco().pane.selected().cloned() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-nothing-selected".to_owned(),
                },
                self.decir("msg-nothing-selected"),
            );
        };
        // La MISMA función que decide si `Enter` entra en un contenedor
        // (`norte_frontend::nav`): dos tablas de extensiones serían dos sitios
        // donde una se olvida, y entonces la misma entrada se navega en una
        // superficie y no se desempaqueta en la otra.
        let Some(raiz) = norte_frontend::nav::archive_root_for(&entrada) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-unpack-not-archive".to_owned(),
                },
                self.decir("msg-unpack-not-archive"),
            );
        };
        let destino = match self.directorio_destino() {
            Ok(d) => d,
            Err(reason_key) => {
                return (
                    ActionAck::Unavailable {
                        reason_key: reason_key.to_owned(),
                    },
                    self.decir(reason_key),
                );
            }
        };
        // Una copia como cualquier otra, con `Fail` y su reintento: si el
        // destino ya tiene lo que va dentro, el lector decide igual que en una
        // transferencia (#274).
        Self::lanzar_reintento(
            Reintento {
                from: raiz,
                to: destino,
                mover: false,
            },
            norte_proto::CollisionPolicy::Fail,
            backend,
            buzon,
        );
        (self.aplicada(), self.decir("msg-unpack-started"))
    }

    /// `pane.test-archive` (#132, #290): comprueba el contenedor bajo el
    /// cursor. No escribe nada; su resultado es el desenlace de la Task.
    fn comprobar_archivo(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(entrada) = self.hueco().pane.selected().cloned() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-nothing-selected".to_owned(),
                },
                self.decir("msg-nothing-selected"),
            );
        };
        if norte_frontend::nav::archive_root_for(&entrada).is_none() {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-unpack-not-archive".to_owned(),
                },
                self.decir("msg-unpack-not-archive"),
            );
        }
        let params = norte_proto::methods::ArchiveTestParams {
            path: entrada.path.clone(),
        };
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let mensaje = match backend.test_archive(params).await {
                // Comprobar no cambia nada: no hay directorios que refrescar.
                Ok(task) => Mensaje::TaskNueva(Box::new((task, Vec::new(), None))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon.send(mensaje).await;
        });
        (self.aplicada(), self.decir("msg-test-archive-started"))
    }

    fn pedir_mkdir(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let dir = self.hueco().pane.dir().clone();
        let donde = Self::linea_de_ruta(&dir);
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-mkdir-title".to_owned(),
            // El directorio en el que se crea NO es un destino: es el
            // contexto. Un destino es a dónde se MUEVE algo que ya existe.
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![donde],
            overflow_note: String::new(),
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
            reconocido: true,
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
    /// Lo que la respuesta AFIRMATIVA de un diálogo pone en marcha.
    ///
    /// Separado de [`Self::responder_dialogo`], que se queda con lo que es
    /// igual para todos: que el id sea el del diálogo abierto, que la
    /// respuesta esté entre las que se ofrecieron, la cerradura de solo
    /// lectura y el cierre. Aquí solo vive lo que cada pendiente hace.
    fn ejecutar_pendiente(
        &mut self,
        dialogo: Dialogo,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let mut salidas = Vec::new();
        // El motivo por el que la respuesta NO hizo nada, si lo hubo: viaja al
        // acuse en vez de quedarse solo en la barra.
        let mut rehusado: Option<&'static str> = None;
        match dialogo.al_confirmar {
            Some(Pendiente::Borrar { paths, permanente }) => {
                Self::lanzar_borrado(paths, permanente, backend, buzon);
            }
            Some(Pendiente::InstruccionIa { dir }) => {
                let instruccion = dialogo.input_crudo.clone();
                salidas.extend(self.lanzar_plan_ia(dir, instruccion, backend, buzon));
            }
            Some(Pendiente::ConsultaSemantica) => {
                let consulta = dialogo.input_crudo.clone();
                salidas.extend(self.lanzar_semantica(consulta, backend, buzon));
            }
            Some(Pendiente::Renombrar { from, siembra }) => {
                let (motivo, partes) =
                    self.confirmar_rename(&from, &siembra, &dialogo.input_crudo, backend, buzon);
                rehusado = motivo;
                salidas.extend(partes);
            }
            Some(Pendiente::Transferir {
                origen,
                origen_dir,
                paths,
                destino,
                mover,
            }) => {
                // El lote se abre AQUÍ, con el número que se va a pedir: la
                // cuenta tiene que existir antes de que llegue el primer
                // desenlace, que con una task que nace terminal puede ser
                // antes de que el bucle de envío haya pedido la segunda.
                // Uno solo no es un lote: su desenlace ya se dice en su fila
                // y su rechazo en la barra, con la frase tipada del error.
                self.lote = (paths.len() > 1).then(|| Lote {
                    total: paths.len(),
                    ..Lote::default()
                });
                Self::lanzar_transferencia(&paths, &origen_dir, &destino, mover, backend, buzon);
                // Las marcas las CONSUME el envío, no el desenlace (mismo
                // criterio que el TUI y que mc): una selección a medio
                // consumir significaría cosas distintas según qué task de
                // las N terminó.
                //
                // Y las del hueco de ORIGEN, no las del que tenga el foco
                // ahora: `FocusSlot` no está vedada mientras hay un
                // diálogo abierto, así que un clic en el otro panel entre
                // la pregunta y la respuesta borraba las marcas del panel
                // equivocado y dejaba intactas las que se acababan de
                // enviar — y el lector volvía a pulsar F5 sobre lo mismo.
                if let Some(h) = self.huecos.get_mut(&origen) {
                    h.pane.clear_marks();
                }
                salidas.push(self.parche_filas());
            }
            Some(Pendiente::Buscar { root }) => {
                let patron = dialogo.input_crudo.clone();
                if patron.is_empty() {
                    // Un patrón vacío casaría el árbol entero: no es una
                    // búsqueda, es un listado recursivo, y se dice en vez
                    // de lanzarlo.
                    self.status.message = Some(clamp_display(norte_i18n::t_in(
                        self.lang,
                        "err-empty-pattern",
                    )));
                    let cambio = ViewChange::Status(self.status.clone());
                    salidas.push(self.parche(vec![cambio]));
                } else {
                    salidas.extend(self.lanzar_busqueda(root, patron, backend, buzon));
                }
            }
            Some(Pendiente::CrearDirectorio { dir }) => {
                let (motivo, partes) =
                    self.crear_directorio(&dir, &dialogo.input_crudo, backend, buzon);
                rehusado = motivo;
                salidas.extend(partes);
            }
            // Los dos que fabrican ficheros a partir de lo tecleado, juntos:
            // este `match` es un despachador y ya roza su tope.
            Some(p @ (Pendiente::Partir { .. } | Pendiente::Empaquetar { .. })) => {
                let (motivo, partes) =
                    self.ejecutar_de_archivo(p, &dialogo.input_crudo, backend, buzon);
                rehusado = motivo;
                salidas.extend(partes);
            }
            Some(Pendiente::Patron { marcar }) => {
                let patron = dialogo.input_crudo.clone();
                let (motivo, partes) = self.aplicar_patron(marcar, &patron);
                rehusado = motivo;
                salidas.extend(partes);
            }
            Some(Pendiente::DeshacerSesion { sesion }) => {
                let (motivo, partes) = self.deshacer_sesion(&sesion, backend, buzon);
                rehusado = motivo;
                salidas.extend(partes);
            }
            Some(Pendiente::AprobarExtension {
                id,
                capabilities,
                digest,
            }) => {
                let (motivo, partes) = self.conceder(&id, &capabilities, digest, backend, buzon);
                rehusado = motivo;
                salidas.extend(partes);
            }
            Some(Pendiente::Decidir {
                approval_id,
                session,
            }) => {
                // Y se apunta a QUIÉN se le dijo que sí desde aquí: la fila
                // del panel de agentes distingue «pidió N veces» de «se le
                // aprobaron M», que no son lo mismo cuando contestó otra
                // ventana, cuando se denegó, o cuando caducó.
                if let Some(sesion) = &session {
                    self.agencia.sesiones.aprobada(sesion);
                    if self.agencia.panel {
                        let cambio = ViewChange::Agents {
                            agents: self.vista_agentes(),
                        };
                        salidas.push(self.parche(vec![cambio]));
                    }
                }
                // Solo `approve` aprueba. Cualquier otra respuesta —y el
                // cierre del diálogo— DENIEGA: una decisión de seguridad
                // no tiene respuesta por defecto que diga «sí».
                //
                // Y si el sí NO llega, se dice. Un `policy.decide` que falla
                // —el daemon se cayó entre la pregunta y la respuesta— deja
                // la operación denegada por silencio mientras esta ventana da
                // por hecho que la autorizó: «lo dije» y «llegó» no son lo
                // mismo en una superficie de seguridad. Denegar es al revés:
                // si esa no llega, el desenlace es el mismo que se pidió.
                lanzar_aprobacion(approval_id, backend, buzon);
            }
            // Una colisión no se contesta con «confirmar»: cada salida ES una
            // política, y quien las traduce es `responder_dialogo`, que sabe
            // cuál se pulsó. Llegar aquí sería una respuesta que este diálogo
            // no ofreció, y esas no se interpretan.
            Some(Pendiente::Reintentar { .. }) | None => {}
        }
        (rehusado, salidas)
    }

    /// Un nombre TECLEADO, como `Segment`, o la clave del motivo por el que
    /// no vale.
    ///
    /// El guard del carácter de sustitución vive aquí y no solo en el rename
    /// porque el camino de VUELTA lo comparten: `escribir_en_dialogo` proyecta
    /// `clamp_display(display_name(texto))` en cada tecla, y el renderer
    /// vuelve a sembrar el campo con esa proyección si tuvo que reconstruir el
    /// nodo. Sin el guard, crear un directorio escribía en el disco el U+FFFD
    /// que había puesto la pantalla.
    fn segmento_tecleado(nombre: &str) -> Result<norte_proto::Segment, &'static str> {
        if nombre.contains('\u{FFFD}') {
            return Err("msg-transfer-name-fffd");
        }
        norte_proto::Segment::new(nombre.as_bytes().to_vec()).map_err(|_| "err-bad-name")
    }

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
        // La PRIMERA respuesta a un diálogo que se abrió SOLO no lo contesta:
        // solo lo reconoce. Vive AQUÍ y no en el camino de teclas porque el
        // ratón es la entrada primaria de esta superficie: el diálogo se
        // pinta en el mismo sitio que el anterior y con la misma primera
        // opción, así que un clic ya en marcha sobre «Confirmar» aterrizaba
        // sobre el «Aprobar» de una aprobación de agente recién llegada.
        //
        // Las respuestas que DENIEGAN están exentas por el mismo motivo que
        // `Escape`: quitarse de encima algo que uno no ha pedido tiene que
        // salir a la primera, y denegar es el desenlace seguro.
        if !self.dialogos[pos].reconocido && choice != "deny" && choice != "cancel" {
            self.dialogos[pos].reconocido = true;
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "host-dialog-acknowledge",
            )));
            return (
                self.aplicada(),
                vec![self.parche(vec![ViewChange::Status(self.status.clone())])],
            );
        }
        if let Some(rechazo) = self.rechaza_por_solo_lectura(pos) {
            return rechazo;
        }
        let dialogo = self.dialogos.remove(pos);
        let mut salidas = Vec::new();
        // `confirm` es la respuesta afirmativa de los diálogos normales;
        // `approve`, la de una aprobación. Nombres distintos a propósito: en
        // una superficie de seguridad, «confirmar» y «aprobar» no deberían
        // poder confundirse en un renderer.
        let mut rehusado = None;
        if choice == "confirm" || choice == "approve" {
            let (motivo, partes) = self.ejecutar_pendiente(dialogo, backend, buzon);
            rehusado = motivo;
            salidas.extend(partes);
        } else if let Some(Pendiente::Decidir { approval_id, .. }) = dialogo.al_confirmar {
            // Denegar explícitamente, y también al cerrar: dejar al agente
            // esperando una respuesta que no llega es peor que decirle que no.
            let backend = Arc::clone(backend);
            tokio::spawn(async move {
                let _ = backend.policy_decide(approval_id, false).await;
            });
        } else if let Some(Pendiente::Reintentar { con }) = &dialogo.al_confirmar {
            // Las cuatro salidas de una colisión no son «confirmar» (#274):
            // cada una ES una política distinta, y cuál se pulsó es la
            // respuesta entera. `cancel` no traduce a ninguna y entonces no se
            // relanza nada — la task fallida se queda como estaba.
            if let Some(politica) = politica_de_colision(choice) {
                Self::lanzar_reintento(con.clone(), politica, backend, buzon);
            }
        }
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        salidas.push(self.parche(vec![cambio]));
        // Un rechazo se ACUSA como tal. Contestar `Applied` a un nombre que
        // no se escribió le dice al renderer que la operación salió, y la
        // misma superficie ya contestaba `Unavailable` cuando el rechazo era
        // por tener varias marcas: dos respuestas para la misma cosa.
        match rehusado {
            Some(reason_key) => (
                ActionAck::Unavailable {
                    reason_key: reason_key.to_owned(),
                },
                salidas,
            ),
            None => (self.aplicada(), salidas),
        }
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
            let afectados: Vec<VPath> = path.parent().into_iter().collect();
            tokio::spawn(async move {
                match backend.delete(path, mode).await {
                    Ok(task) => {
                        let _ = buzon
                            .send(Mensaje::TaskNueva(Box::new((task, afectados, None))))
                            .await;
                    }
                    Err(e) => {
                        let _ = buzon.send(Mensaje::TaskFallida(Box::new(e))).await;
                    }
                }
            });
        }
    }

    /// La SEGUNDA cerradura del modo solo lectura, sobre el punto ÚNICO donde
    /// se lanzan todas las mutaciones.
    ///
    /// Barata, y hoy inalcanzable: en solo lectura ningún `Pendiente` que
    /// mute llega a nacer y el canal de aprobaciones ni se toma. «Inalcanzable
    /// hoy» es exactamente lo que deja de ser verdad cuando alguien añada el
    /// siguiente diálogo, y esta es la puerta por la que pasaría.
    ///
    /// Cierra el diálogo al rechazarlo: dejarlo abierto invitaría a pulsar
    /// otra vez lo que no va a ocurrir.
    fn rechaza_por_solo_lectura(
        &mut self,
        pos: usize,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        if self.efectos != crate::commands::Efectos::SoloLectura {
            return None;
        }
        let muta = self
            .dialogos
            .get(pos)?
            .al_confirmar
            .as_ref()
            .is_some_and(|p| {
                matches!(
                    p,
                    Pendiente::Borrar { .. }
                        | Pendiente::Transferir { .. }
                        | Pendiente::CrearDirectorio { .. }
                        | Pendiente::Decidir { .. }
                        | Pendiente::Renombrar { .. }
                        // Conceder capabilities es la decisión de seguridad
                        // del sistema de extensiones: una ventana que se
                        // declara de solo lectura no la toma.
                        | Pendiente::AprobarExtension { .. }
                        // Deshacer una sesión ESCRIBE: mueve ficheros de
                        // vuelta y borra lo que el agente creó.
                        | Pendiente::DeshacerSesion { .. }
                        // Pedir un plan no escribe en el disco, y aun así
                        // entra: manda el contenido de un directorio a un
                        // modelo, que no es algo que deba hacer una ventana
                        // que se declara de solo lectura.
                        | Pendiente::InstruccionIa { .. }
                        // Tampoco: la consulta sale del proceso.
                        | Pendiente::ConsultaSemantica
                )
            });
        if !muta {
            return None;
        }
        self.dialogos.remove(pos);
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        Some((
            ActionAck::Unavailable {
                reason_key: "host-read-only".to_owned(),
            },
            vec![self.parche(vec![cambio])],
        ))
    }

    /// Resuelve el nombre confirmado y encola el rename, o dice por qué no.
    fn confirmar_rename(
        &mut self,
        from: &VPath,
        siembra: &str,
        escrito: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        match Self::bytes_del_rename(from, siembra, escrito) {
            Ok(destino) => {
                Self::lanzar_rename(from.clone(), destino, backend, buzon);
                (None, Vec::new())
            }
            Err(clave) => {
                // El motivo vuelve para que el ACUSE lo diga, no solo la
                // barra: un renderer que recibe `Applied` cree que la
                // operación salió, y la misma superficie contestaba
                // `Unavailable` cuando el rechazo era por las marcas.
                self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
                let cambio = ViewChange::Status(self.status.clone());
                (Some(clave), vec![self.parche(vec![cambio])])
            }
        }
    }

    /// Encola el `fs.move` de UN rename, con el destino ya compuesto.
    ///
    /// Aparte de [`Self::lanzar_transferencia`] porque el destino de un
    /// rename es una RUTA COMPLETA y el de una transferencia es un
    /// DIRECTORIO sobre el que se compone el nombre del origen. Pasar el uno
    /// por el otro renombraría a `nuevo/nombre-viejo`, que es exactamente el
    /// tipo de error que un parámetro con dos significados produce.
    ///
    /// Mismo verbo del wire, misma entrada de journal y mismo camino de
    /// deshacer que mover: lo que cambia es la pregunta, no el efecto.
    fn lanzar_rename(
        from: VPath,
        to: VPath,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        // El directorio del que sale y al que llega es el MISMO, así que una
        // sola entrada: relistarlo dos veces sería pedir el mismo listado dos
        // veces.
        let afectados: Vec<VPath> = from.parent().into_iter().collect();
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let mensaje = match backend
                .move_(from, to, norte_proto::CollisionPolicy::Fail)
                .await
            {
                Ok(task) => Mensaje::TaskNueva(Box::new((task, afectados, None))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon.send(mensaje).await;
        });
    }

    /// Encola las Tasks del lote y engancha su progreso al actor.
    ///
    /// El nombre del destino se compone AQUÍ, con el último segmento del
    /// origen tal cual: bytes, sin normalizar y sin pasar por pantalla. Un
    /// nombre que ha ido a la webview y ha vuelto es otro nombre (ADR 0061).
    /// Lo que la composición byte a byte NO puede resolver es un nombre legal
    /// en el origen e ilegal en el destino (`CON`, un punto final, un `:`
    /// yendo de ext4 a NTFS): eso es cosa del provider de destino, y está en
    /// la issue #217.
    ///
    /// La política de colisión es `Fail`, el default seguro del wire: si el
    /// destino existe, la Task falla y el tablero lo dice. Sobrescribir o
    /// renombrar son decisiones del lector, y esta ventana todavía no tiene
    /// dónde tomarlas — elegirlas por él sería la clase de silencio que borra
    /// ficheros.
    fn lanzar_transferencia(
        paths: &[VPath],
        origen_dir: &VPath,
        destino: &VPath,
        mover: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        // Los directorios que el desenlace deja desactualizados. En una copia
        // solo el destino; en un movimiento, también de donde sale — y el de
        // origen se toma del HUECO, no del padre de cada entrada: el padre lo
        // escribe el provider y el hueco puede venir de la config o de la
        // sesión, así que en NFD contra NFC, o contra un servidor sin
        // distinción de caja, son dos cadenas para el mismo sitio y la
        // comparación byte a byte del refresco no encontraría el panel
        // (ADR 0061). Se apuntan los dos: uno de ellos casa.
        let mut afectados = vec![destino.clone()];
        if mover {
            afectados.push(origen_dir.clone());
        }
        let mut trabajos: Vec<(VPath, VPath)> = Vec::with_capacity(paths.len());
        for path in paths {
            let Some(nombre) = path.file_name() else {
                // Imposible aquí: `pedir_transferencia` rechaza el lote
                // entero si alguna entrada es una raíz. Se comprueba igual
                // porque la alternativa es un `unwrap` en el camino de una
                // mutación.
                continue;
            };
            if mover
                && let Some(padre) = path.parent()
                && !afectados.contains(&padre)
            {
                afectados.push(padre);
            }
            trabajos.push((path.clone(), destino.join(nombre.clone())));
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        // UNA task de envío para el lote entero, y las llamadas EN SERIE. Un
        // `spawn` por entrada abría tantas RPC simultáneas como marcas
        // hubiera: marcar unos miles de ficheros y pulsar F5 es el flujo
        // normal de un gestor ortodoxo, y contra SFTP eso no es una copia,
        // es una denegación de servicio contra el propio daemon. En serie el
        // daemon sigue haciendo el trabajo en paralelo si quiere; lo que se
        // acota es cuántas peticiones hay volando a la vez.
        tokio::spawn(async move {
            for (from, to) in trabajos {
                // El par ORIGINAL viaja con la task (#274): si esto choca, es
                // lo único con lo que se puede volver a intentar con otra
                // política. Recomponerlo desde el progreso no vale — dice qué
                // fichero va por dentro, no qué se pidió.
                let reintento = Reintento {
                    from: from.clone(),
                    to: to.clone(),
                    mover,
                };
                let encolada = if mover {
                    backend
                        .move_(from, to, norte_proto::CollisionPolicy::Fail)
                        .await
                } else {
                    backend
                        .copy(from, to, norte_proto::CollisionPolicy::Fail)
                        .await
                };
                let mensaje = match encolada {
                    Ok(task) => {
                        Mensaje::TaskNueva(Box::new((task, afectados.clone(), Some(reintento))))
                    }
                    // A la CUENTA del lote, no a la barra: N rechazos eran N
                    // mensajes de los que solo sobrevivía el último (#271).
                    Err(e) => Mensaje::TaskDeLoteRechazada(Box::new(e)),
                };
                if buzon.send(mensaje).await.is_err() {
                    // El actor ya no está: lo que quede del lote no le
                    // importa a nadie, y seguir pidiéndolo sí importaría.
                    return;
                }
            }
        });
    }

    /// Hace sitio en el tablero tirando lo más viejo TERMINADO.
    ///
    /// Se prefiere desalojar una TERMINADA BIEN: una fallida o una cancelada
    /// es la única superficie que dice qué no llegó —un fallo no deja entrada
    /// de journal—, y en un lote grande con colisiones son justo las que se
    /// acumulan. Una VIVA no se toca: tiene progreso que bombear y, quizá, un
    /// directorio que relistar.
    fn desalojar_del_tablero(&mut self) {
        if self.tasks.len() < MAX_TASKS {
            return;
        }
        let viejo = self
            .tasks
            .iter()
            .find(|(_, t)| t.vista.state == crate::dto::TaskStateView::Done)
            .or_else(|| {
                self.tasks
                    .iter()
                    .find(|(_, t)| Self::terminal(t.vista.state))
            })
            .map(|(k, _)| *k);
        if let Some(viejo) = viejo {
            debug_assert!(
                self.tasks[&viejo].afectados.is_empty(),
                "se desaloja una task con un refresco pendiente"
            );
            self.tasks.remove(&viejo);
        }
    }

    /// Mete una Task recién encolada en el tablero y deja su progreso
    /// bombeando hacia el actor.
    /// Relanza la transferencia que chocó, con la política elegida (#274).
    ///
    /// Repite el MISMO verbo: un «sobrescribir» sobre una copia que se
    /// convirtiera en un movimiento borraría el origen que nadie mandó tocar.
    /// Y vuelve a viajar con su `Reintento`, porque el segundo intento puede
    /// chocar otra vez —`Skip` y `RenameAuto` no, pero `Newer` sí— y entonces
    /// hay que poder volver a preguntar.
    fn lanzar_reintento(
        con: Reintento,
        politica: norte_proto::CollisionPolicy,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        // El destino cambia; el origen también deja de estar si es un
        // movimiento. Se apuntan los dos padres, como en la transferencia
        // original.
        let mut afectados: Vec<VPath> = con.to.parent().into_iter().collect();
        if con.mover
            && let Some(padre) = con.from.parent()
            && !afectados.contains(&padre)
        {
            afectados.push(padre);
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let encolada = if con.mover {
                backend
                    .move_(con.from.clone(), con.to.clone(), politica)
                    .await
            } else {
                backend
                    .copy(con.from.clone(), con.to.clone(), politica)
                    .await
            };
            let mensaje = match encolada {
                Ok(task) => Mensaje::TaskNueva(Box::new((task, afectados, Some(con)))),
                Err(e) => Mensaje::TaskFallida(Box::new(e)),
            };
            let _ = buzon.send(mensaje).await;
        });
    }

    /// El reintento que ya tenía esta task, si la hay y es de esta época.
    ///
    /// Un reanuncio de la reconexión no sabe con qué se pidió la task, así que
    /// sustituirlo por `None` dejaría sin salida justo a la colisión que el
    /// lector encuentra al volver. La época importa: tras un relevo del daemon
    /// los ids vuelven a empezar, y lo que había con ese número era otra cosa.
    fn reintento_heredado(&self, id: u64) -> Option<Reintento> {
        self.tasks
            .get(&id)
            .filter(|t| t.epoca == self.epoca_conexion)
            .and_then(|t| t.reintento.clone())
    }

    fn registrar_task(
        &mut self,
        task: crate::backend::HostTask,
        afectados: Vec<VPath>,
        reintento: Option<Reintento>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let id = task.id.get();
        let ajena = task.foreign;
        // Alta en la cuenta del lote (#271), antes de cualquier desalojo: lo
        // que se encoló se encoló aunque su fila no llegue a caber.
        if let Some(lote) = self.lote.as_mut()
            && !ajena
            && lote.encoladas + lote.rechazadas < lote.total
            && lote.ids.insert(id)
        {
            lote.encoladas += 1;
        }
        self.desalojar_del_tablero();
        // Y el techo DURO de lo retenido (#271). Solo se llega aquí con el
        // tablero lleno de tasks VIVAS, y solo desde el canal de ajenas: las
        // propias no pasan de `pedir_transferencia`, que rehúsa el lote entero
        // si no cabe. Una fila ajena que se cae no pierde nada —viene con
        // `afectados` vacío, o sea sin refresco que deber— salvo una fila que
        // esta ventana nunca prometió enseñar.
        if self.tasks.len() >= MAX_TASKS_RETAINED && !self.tasks.contains_key(&id) {
            tracing::debug!(task = id, "tablero lleno: no se retiene una task ajena");
            return Vec::new();
        }
        let mut rx = task.progress.clone();
        let nacio = rx.borrow().clone();
        let mut vista = Self::vista_de(&nacio);
        vista.foreign = ajena;
        // Un REANUNCIO —el SDK vuelve a ofrecer las tasks al reconectar— trae
        // un progreso que no sabe nada del informe que ya se pidió por esta
        // task. Proyectarlo tal cual borraba del tablero la única señal de
        // que el directorio se quedó a medias, justo cuando la conexión se
        // recupera y el lector vuelve a mirarlo.
        // Solo se hereda de la MISMA época: tras un relevo del daemon el id
        // vuelve a empezar en 1, y lo que había con ese número era otra task.
        let anterior_de_esta_epoca = self
            .tasks
            .get(&id)
            .filter(|t| t.epoca == self.epoca_conexion);
        if let Some(anterior) = anterior_de_esta_epoca
            && anterior.informe_pedido
            && Self::terminal(vista.state)
            && anterior.vista.detail.is_some()
        {
            vista.detail.clone_from(&anterior.vista.detail);
            vista.detail_hostile = anterior.vista.detail_hostile;
        }
        // Si esta task YA estaba en el tablero —una reconexión la reanuncia
        // por el canal de ajenas— lo que llega no sabe qué directorios tocaba,
        // así que se conserva lo apuntado: sustituirlo por una lista vacía
        // perdía el relistado justo en el camino donde la pantalla es más
        // probable que esté rancia.
        let afectados = if afectados.is_empty() {
            self.tasks
                .get(&id)
                .filter(|t| t.epoca == self.epoca_conexion)
                .map(|t| t.afectados.clone())
                .unwrap_or_default()
        } else {
            afectados
        };
        // Una mutación ACEPTADA es la prueba de que el journal volvió: el
        // daemon rehúsa mutar sin él (regla dura 4), así que si esta entró,
        // el aviso de «no se registra» dejó de ser verdad. No hay
        // notificación de recuperación —el TUI la tiene porque su journal es
        // embebido—, y un aviso que no sabe apagarse miente sobre lo único
        // que describe de toda la sesión.
        let apaga_el_aviso = self.journal_rehusado && !ajena && Self::muta(vista.kind.as_str());
        if apaga_el_aviso {
            self.journal_rehusado = false;
        }
        // Igual que con los afectados: si ya estaba, se conserva que su
        // informe se pidió. Una reconexión que reanuncia un lote terminado no
        // puede volver a abrir el mismo informe.
        let informe_pedido = self
            .tasks
            .get(&id)
            .is_some_and(|t| t.informe_pedido && t.epoca == self.epoca_conexion);
        let reintento = reintento.or_else(|| self.reintento_heredado(id));
        self.tasks.insert(
            id,
            TaskViva {
                vista,
                cancel: task.cancel,
                afectados,
                reintento,
                informe_pedido,
                epoca: self.epoca_conexion,
                progreso: task.progress.clone(),
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
        // Puede nacer TERMINAL: el daemon la completó antes de que esta
        // llamada volviera, y entonces `rx.changed()` no dispara nunca y
        // `progreso` no se llama ni una vez. Sin esto, una copia rapidísima
        // dejaba el destino sin relistar para siempre — la carrera que la
        // tarea 5.1 nombra literalmente.
        let mut cambios = vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
        }];
        if apaga_el_aviso {
            cambios.push(self.cambio_de_banners());
        }
        if let Some(estado) = self
            .tasks
            .get(&id)
            .map(|t| t.vista.state)
            .filter(|e| Self::terminal(*e))
        {
            // Nace TERMINAL: su desenlace entra en la cuenta del lote aquí,
            // porque `progreso` no se llamará nunca para ella.
            if self.anota_desenlace_de_lote(id, estado) {
                cambios.push(self.cambio_de_banners());
            }
            cambios.extend(self.refrescar_afectados(id, backend, buzon));
            // Y su informe, por el mismo motivo que el relistado: si nació
            // terminal, `progreso` no se llama NUNCA, y el informe es la
            // única señal de que el directorio se quedó a medias. Un lote
            // rapidísimo se quedaba sin ella justo cuando el desenlace de la
            // Task más parece que todo fue bien.
            self.pedir_informe_de_lote(&nacio, backend, buzon);
        }
        vec![self.parche(cambios)]
    }

    /// Vuelve a listar los huecos que esta task dejó desactualizados, y
    /// OLVIDA lo que afectaba: un desenlace se aplica una vez.
    ///
    /// Por directorio y no por hueco: quien encoló la task sabía qué
    /// directorios tocaba, no qué paneles estarán mirándolos cuando termine
    /// —el lector puede haber navegado, o haber cambiado de disposición—.
    ///
    /// Un hueco cuenta como afectado por a dónde VA si tiene algo en vuelo, y
    /// por lo que enseña si no: los dos son «el directorio de este panel», y
    /// mirar solo el segundo dejaba sin refrescar al panel que estaba
    /// entrando justo en el sitio que la mutación cambió.
    fn refrescar_afectados(
        &mut self,
        task_id: u64,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<ViewChange> {
        let afectados = match self.tasks.get(&task_id) {
            Some(t) if !t.afectados.is_empty() => t.afectados.clone(),
            _ => return Vec::new(),
        };
        // Mientras quede OTRA task viva sobre el mismo directorio, no se
        // relista: un lote de doscientas copias produciría doscientos
        // listados del mismo panel, cada uno invalidando el anterior y
        // volviendo a pagar el sondeo y las decoraciones de plugin (medidas
        // en 167 ms por página de veinte). Se refresca cuando termina la
        // ÚLTIMA, que es cuando el directorio deja de moverse.
        let queda_trabajo = self.tasks.iter().any(|(id, t)| {
            *id != task_id
                && !Self::terminal(t.vista.state)
                && t.afectados.iter().any(|d| afectados.contains(d))
        });
        if queda_trabajo {
            return Vec::new();
        }
        // Consumido: ni esta ni las hermanas ya terminadas vuelven a pedirlo.
        for t in self.tasks.values_mut() {
            if t.afectados.iter().any(|d| afectados.contains(d)) {
                t.afectados.clear();
            }
        }
        let huecos: Vec<(u32, bool)> = self
            .huecos
            .iter()
            .filter(|(_, h)| {
                afectados.contains(h.dir_pedido.as_ref().unwrap_or_else(|| h.pane.dir()))
            })
            .map(|(id, _)| (*id, self.oculto(*id)))
            .collect();
        let mut cambios = Vec::new();
        for (slot, oculto) in huecos {
            if oculto {
                // Un hueco que no se ve no pide listados —lo que no se ve no
                // se trae—, pero tampoco puede quedarse creyendo que su
                // listado sigue siendo verdad: se marca CARGANDO, que es lo
                // que `despertar_visibles` recoge en cuanto vuelva a la
                // pantalla. Sin esto, una pestaña de atrás sobre el
                // directorio de destino enseñaba un listado anterior a la
                // copia hasta que alguien navegara a mano.
                if let Some(h) = self.huecos.get_mut(&slot) {
                    h.estado = SlotState::Loading;
                }
                cambios.push(ViewChange::SlotState {
                    slot_id: slot,
                    state: SlotState::Loading,
                });
                continue;
            }
            cambios.extend(self.refrescar(slot, backend, buzon));
        }
        cambios
    }

    /// Vuelve a pedir el listado de UN hueco, en su MISMO directorio.
    ///
    /// No es una navegación: no toca el rastro ni el foco. Lo que sí hace es
    /// conservar lo que el lector tenía puesto, y las dos cosas son por
    /// IDENTIDAD y no por índice:
    ///
    /// - el CURSOR se ancla con `set_pending_focus`, o sea por ruta. La
    ///   memoria por directorio guarda un índice, y un índice no sobrevive a
    ///   que la operación quite o añada una entrada: quien miraba `e` se
    ///   encontraba el cursor en otro fichero, sin haber tocado una tecla, y
    ///   la siguiente tecla podía ser F8.
    /// - las MARCAS se vuelven a poner por ruta con `restore_marks`
    ///   (`set_listing` las limpia, que es lo correcto para un `cd`). Lo que
    ///   la operación se llevó no se vuelve a marcar y no se inventa nada.
    ///
    /// Con algo EN VUELO no hace nada. Reservaría un testigo nuevo, así que
    /// la respuesta de esa navegación llegaría con uno viejo y se tiraría: el
    /// panel se quedaría en el directorio del que el lector acababa de salir,
    /// sin decir nada. Perder un refresco es una pantalla un poco vieja;
    /// perder una navegación es la aplicación moviéndose sola. Y no hay nada
    /// que perder: el listado que va a aterrizar es más nuevo que la
    /// mutación, o va a otro sitio.
    fn refrescar(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<ViewChange> {
        self.token += 1;
        let token = RequestToken(self.token);
        let Some(hueco) = self.huecos.get_mut(&slot) else {
            return Vec::new();
        };
        if hueco.en_vuelo.is_some() {
            return Vec::new();
        }
        if let Some(sel) = hueco.pane.selected().map(|e| e.path.clone()) {
            hueco.pane.set_pending_focus(sel);
        }
        hueco.pane.remember_cursor();
        // `marked_paths` cae al cursor cuando no hay marcas, y restaurar ESO
        // convertiría un refresco en una marca que el lector no hizo.
        hueco.marcas_a_restaurar = if hueco.pane.marks_len() > 0 {
            hueco.pane.marked_paths()
        } else {
            Vec::new()
        };
        let dir = hueco.pane.dir().clone();
        hueco.estado = SlotState::Loading;
        hueco.en_vuelo = Some(token);
        hueco.drenando = Some(token);
        self.pedir_listado(slot, &dir, token, backend, buzon);
        vec![ViewChange::SlotState {
            slot_id: slot,
            state: SlotState::Loading,
        }]
    }

    /// Vuelve a pedir el listado de TODOS los huecos que se ven.
    ///
    /// De todos y no solo del enfocado, que es lo que hace el TUI y por el
    /// mismo motivo: lo que cambia un listado por debajo es un cambio EN EL
    /// DISCO, y un cambio en el disco no respeta el foco. Los ocultos se
    /// quedan fuera —lo que no se ve no se trae—; ya los despierta
    /// `despertar_visibles` cuando el reparto los saca a la luz.
    fn refrescar_visibles(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slots: Vec<u32> = self
            .huecos
            .keys()
            .copied()
            .filter(|id| !self.oculto(*id))
            .collect();
        let mut cambios = Vec::new();
        for slot in slots {
            cambios.extend(self.refrescar(slot, backend, buzon));
        }
        if cambios.is_empty() {
            // Todos tenían algo en vuelo: lo que va a aterrizar es más nuevo
            // que esta tecla, así que no hay nada que decir ni que pintar.
            return (self.aplicada(), Vec::new());
        }
        (self.aplicada(), vec![self.parche(cambios)])
    }

    /// Aparta —o devuelve— las entradas ocultas del panel activo (#107).
    ///
    /// Presentación-solo: el provider no vuelve a listar, las entradas
    /// apartadas siguen en el modelo. Y se ANUNCIA, porque un listado que
    /// encoge sin decir por qué se lee como un fallo del panel.
    fn alternar_ocultos(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let (visibles, podadas) = {
            let hueco = self.hueco_mut();
            let visibles = hueco.pane.toggle_hidden();
            (visibles, hueco.pane.pruned_marks())
        };
        let clave = if visibles {
            "msg-hidden-shown"
        } else {
            "msg-hidden-hidden"
        };
        let mut frase = norte_i18n::t_in(self.lang, clave);
        if podadas > 0 {
            // Apartar las ocultas PODA las marcas de las que se van. El
            // contrato de `PaneState::pruned_marks` es que eso jamás es
            // silencioso: callarlo mandaría la siguiente op en masa sobre
            // menos ficheros de los que el lector marcó, creyendo él que van
            // todos.
            frase.push_str(", ");
            frase.push_str(&norte_i18n::ta_in(
                self.lang,
                "status-marks-pruned",
                &[("n", &podadas.to_string())],
            ));
        }
        self.status.message = Some(clamp_display(frase));
        let filas = self.parche_filas();
        let cambio = ViewChange::Status(self.status.clone());
        (self.aplicada(), vec![filas, self.parche(vec![cambio])])
    }

    /// Cicla la reinterpretación de los nombres que no son UTF-8 (#57).
    ///
    /// Display-only (regla 1): lo que cambia es cómo se PINTAN los bytes, no
    /// los bytes. Por eso las claves de fila siguen valiendo y solo viajan
    /// las filas visibles.
    fn ciclar_encoding(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let etiqueta = self.hueco_mut().pane.cycle_name_encoding();
        let frase = match etiqueta {
            Some(enc) => norte_i18n::ta_in(self.lang, "msg-names-encoding", &[("enc", enc)]),
            None => norte_i18n::t_in(self.lang, "msg-names-encoding-off"),
        };
        self.status.message = Some(clamp_display(frase));
        let filas = self.parche_filas();
        let cambio = ViewChange::Status(self.status.clone());
        (self.aplicada(), vec![filas, self.parche(vec![cambio])])
    }

    /// Los tres gestos de panel de la ADR 0058: espejo, traer e intercambiar.
    ///
    /// Los tres necesitan el OTRO hueco, y el otro hueco lo dice el rol
    /// compartido —el mismo del que sale el destino de una copia—, nunca «el
    /// de al lado»: con tres listados, adivinar es mandar el panel de alguien
    /// a un sitio que no eligió.
    fn gesto_de_panel(
        &mut self,
        efecto: Efecto,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let otro = match self.hueco_destino() {
            Ok(id) => id,
            Err(clave) => {
                return (
                    ActionAck::Unavailable {
                        reason_key: clave.to_owned(),
                    },
                    Vec::new(),
                );
            }
        };
        let activo = self.activo();
        match efecto {
            // Lo que viaja es A DÓNDE VA el panel de origen, no lo que
            // enseña: durante una navegación `pane.dir()` responde todavía
            // por el directorio que se abandona, y espejar eso mandaría al
            // otro panel al sitio del que el lector acaba de salir.
            Efecto::Espejo | Efecto::Traer => {
                let (origen, llega) = if matches!(efecto, Efecto::Espejo) {
                    (activo, otro)
                } else {
                    (otro, activo)
                };
                let Some(destino) = self.dir_en_curso(origen) else {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                };
                if self.dir_en_curso(llega).as_ref() == Some(&destino) {
                    // Los dos ya están ahí. Un cd redundante RE-LISTA el
                    // panel que llega: `set_listing` le borra las marcas y
                    // una navegación —a diferencia de un refresco— no las
                    // restaura, además de deslizarle el listado bajo el
                    // cursor. Todo eso a cambio de nada, porque ya enseña lo
                    // que se le pide. El TUI lo rehúsa por lo mismo
                    // (`gestures::mirror_plan`).
                    return (self.aplicada(), Vec::new());
                }
                (
                    self.aplicada(),
                    self.navegar_hueco(llega, &destino, Trail::Record, backend, buzon),
                )
            }
            Efecto::Intercambiar => self.intercambiar_huecos(activo, otro, backend, buzon),
            // El `match` de arriba no manda aquí nada más.
            _ => Self::no_muta(),
        }
    }

    /// A dónde va un hueco: el directorio pedido si hay una navegación en
    /// vuelo, y si no el que enseña.
    ///
    /// `None` solo si el hueco no existe, que para quien llama es una
    /// pantalla que cambió por debajo.
    fn dir_en_curso(&self, slot: u32) -> Option<VPath> {
        let h = self.huecos.get(&slot)?;
        Some(h.dir_pedido.clone().unwrap_or_else(|| h.pane.dir().clone()))
    }

    /// Los dos listados cambian de sitio. NO toca disco.
    ///
    /// Lo que se intercambia es el CONTENIDO del hueco —listado, cursor,
    /// marcas, rastro y orden—, porque partirlo más sería inventar reglas
    /// sobre qué se queda dónde. El foco no se mueve: quien lo tenía sigue
    /// teniéndolo, y ahora enseña lo otro, que es lo que el gesto significa.
    ///
    /// Lo único que NO viaja es la ventana de pintado (`primera_visible` y
    /// `visibles`): esa es geometría del SLOT, no del listado.
    ///
    /// Lo que estaba EN VUELO es la parte que no se ve. Una respuesta viaja
    /// etiquetada con su hueco, así que tras el intercambio llegaría al hueco
    /// equivocado y se descartaría por testigo: el panel se quedaría cargando
    /// para siempre. Se vuelve a pedir, apuntando a donde iba. Lo mismo con
    /// el sondeo y la decoración, que casan por RUTA y por eso no pintarían
    /// nada raro, pero dejarían la memoria de «ya se pidió» sobre un listado
    /// que ya no está ahí — o sea columnas de tamaño en blanco para siempre.
    fn intercambiar_huecos(
        &mut self,
        a: u32,
        b: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if !self.huecos.contains_key(&a) || !self.huecos.contains_key(&b) {
            // Uno de los dos desapareció entre el rol y aquí. Se comprueba
            // ANTES de sacar ninguno: los dos `remove` de una tupla se
            // evalúan los dos antes de casar el patrón, así que salir por el
            // camino de error con uno ya extraído lo DROPEA — «se deshace lo
            // hecho» no deshacía nada.
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let (Some(mut ha), Some(mut hb)) = (self.huecos.remove(&a), self.huecos.remove(&b)) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        // La ventana de pintado se queda en SU slot. `primera_visible` y
        // `visibles` no describen el listado: los pone el renderer con
        // `set_visible_range`, y su `scrollTop` es suyo — un intercambio no
        // lo mueve ni dispara un evento de scroll que lo recalcule. Si
        // viajaran con el hueco, cada panel pintaría filas de una banda que
        // el lector no tiene delante y los DOS se verían vacíos, sin nada
        // que lo corrigiera salvo arrastrar la barra a mano.
        std::mem::swap(&mut ha.primera_visible, &mut hb.primera_visible);
        std::mem::swap(&mut ha.visibles, &mut hb.visibles);
        self.huecos.insert(a, hb);
        self.huecos.insert(b, ha);
        for slot in [a, b] {
            if let Some(h) = self.huecos.get_mut(&slot) {
                h.cancelar_sondeo
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                h.cancelar_sondeo = std::sync::Arc::default();
                h.sondeando = false;
                h.sondeados.clear();
                h.olvidar_adornos();
                h.adornando = false;
            }
            self.reanudar_peticion(slot, backend, buzon);
        }
        // Cambian las dos mitades de la pantalla a la vez —filas, cabeceras,
        // ruta, cursor y estado—, así que viaja una FOTO y no seis parches.
        let snap = self.snapshot();
        (
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// Vuelve a pedir lo que este hueco tenía en vuelo, con testigo nuevo.
    ///
    /// «En vuelo» son DOS cosas, y mirar solo la primera dejaba pasar el caso
    /// común. `en_vuelo` se limpia en cuanto aterriza la primera página,
    /// mientras `drenando` sigue trayendo el resto del stream: en un
    /// directorio de más de `FIRST_PAGE` entradas —o sea casi cualquiera— hay
    /// una ventana en la que solo vive el drenaje. Los lotes que siguieran
    /// llegando se descartarían por testigo (no se cruzan de hueco, eso está
    /// bien), y el listado se quedaría congelado en las cien primeras
    /// entradas, en `Ready`, sin decir nada: marcar todo actuaría sobre ese
    /// trozo.
    ///
    /// Los dos casos se repiden distinto:
    ///
    /// - **Navegación**: conserva el DESTINO de la petición vieja, no el
    ///   directorio del que salía.
    /// - **Solo drenaje**: la primera página ya está en pantalla, así que
    ///   esto es un REFRESCO de lo que el lector mira — cursor y marcas
    ///   vuelven, con la misma disciplina que [`Self::refrescar`].
    ///
    /// Sin ninguna de las dos no hace nada, y no gasta testigo.
    fn reanudar_peticion(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let Some(h) = self.huecos.get(&slot) else {
            return;
        };
        let navegando = h.en_vuelo.is_some();
        if !navegando && h.drenando.is_none() {
            return;
        }
        self.token += 1;
        let token = RequestToken(self.token);
        let Some(h) = self.huecos.get_mut(&slot) else {
            return;
        };
        let dir = if navegando {
            h.dir_pedido.clone().unwrap_or_else(|| h.pane.dir().clone())
        } else {
            // El hueco YA está en su directorio: lo que faltaba era el resto.
            h.pane.dir().clone()
        };
        if !navegando {
            if let Some(sel) = h.pane.selected().map(|e| e.path.clone()) {
                h.pane.set_pending_focus(sel);
            }
            h.pane.remember_cursor();
            // `marked_paths` cae al cursor sin marcas, y restaurar ESO sería
            // una marca que nadie hizo.
            h.marcas_a_restaurar = if h.pane.marks_len() > 0 {
                h.pane.marked_paths()
            } else {
                Vec::new()
            };
        }
        h.en_vuelo = Some(token);
        h.drenando = Some(token);
        h.estado = SlotState::Loading;
        self.pedir_listado(slot, &dir, token, backend, buzon);
    }

    /// Abre el rastro de navegación del hueco activo.
    ///
    /// Las filas son el MRU compartido (`History::entries`), más reciente
    /// primero: qué recuerda un panel no puede depender de quién lo pinta.
    fn abrir_historial(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot = self.activo();
        let rastro = self.hueco().historial.entries().clone();
        self.selector = Some(crate::pickers::Selector::historial(slot, &rastro));
        self.gen_selector += 1;
        let cambio = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Abre los favoritos de la configuración con la que arrancó la ventana.
    ///
    /// Los mismos que alimentan la barra lateral, y de la misma fuente: dos
    /// listas de favoritos que se leen distinto serían dos configuraciones.
    fn abrir_hotlist(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot = self.activo();
        let favoritos: Vec<(String, Result<VPath, String>)> = self
            .config
            .common
            .hotlist
            .iter()
            .map(|h| (h.name.clone(), h.target.clone()))
            .collect();
        self.selector = Some(crate::pickers::Selector::hotlist(
            slot, &favoritos, self.lang,
        ));
        self.gen_selector += 1;
        let cambio = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Aplica un snapshot de progreso al tablero.
    fn progreso(
        &mut self,
        p: &norte_proto::TaskProgress,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(viva) = self.tasks.get_mut(&p.task_id.get()) else {
            return Vec::new();
        };
        let ajena = viva.vista.foreign;
        viva.vista = Self::vista_de(p);
        // De quién es la task no lo dice el progreso: lo dice de dónde vino.
        viva.vista.foreign = ajena;
        // Leído de la vista que se acaba de proyectar: volver a construirla
        // solo para mirar su estado cuesta dos `String` y un `path_display`
        // en cada tick de progreso de cada task del lote.
        let acabo = Self::terminal(viva.vista.state);
        let estado_final = viva.vista.state;
        let mut cambios = vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
        }];
        // El desenlace entra en la cuenta del lote (#271). Solo cuando el lote
        // queda RESUELTO viaja algo: doscientas frases de «una más» no dicen
        // nada que la fila no diga ya.
        if acabo && self.anota_desenlace_de_lote(p.task_id.get(), estado_final) {
            cambios.push(self.cambio_de_banners());
        }
        // Un deshacer que TERMINA suelta su sesión: mientras corre, la fila
        // lo dice y `u` sobre ella se rehúsa —dos undos de la misma sesión
        // caminan la misma lista de entradas— y eso no puede quedarse pegado
        // para siempre.
        if acabo && let Some(sesion) = self.agencia.undos.remove(&p.task_id.get()) {
            self.agencia.sesiones.deshecha(&sesion);
            if self.agencia.panel {
                cambios.push(ViewChange::Agents {
                    agents: self.vista_agentes(),
                });
            }
        }
        // Si la que acaba de terminar es LA búsqueda, su vista deja de decir
        // «buscando…»: una lista que ya no crece y una que sigue creciendo se
        // leen igual si nadie las distingue.
        let termino = !matches!(p.state, norte_proto::TaskState::Running);
        if termino
            && let Some(b) = self.busqueda.as_mut()
            && b.task == p.task_id
        {
            b.viva = false;
            cambios.push(ViewChange::Search {
                search: self.vista_busqueda(),
            });
        }
        // Una mutación que terminó deja pantallas desactualizadas: la entrada
        // nueva está en el disco y no en el listado. Solo con un desenlace de
        // VERDAD —`Running` no lo es—, y una sola vez.
        if acabo {
            cambios.extend(self.refrescar_afectados(p.task_id.get(), backend, buzon));
            self.pedir_informe_de_lote(p, backend, buzon);
            cambios.extend(self.cerrar_comparacion(p));
            cambios.extend(self.cerrar_sincronizacion(p));
            self.pedir_informe_de_sync(p, backend, buzon);
            cambios.extend(self.decir_el_recuento(p));
            cambios.extend(self.ofrecer_reintento(p));
        }
        vec![self.parche(cambios)]
    }

    /// El TOTAL de un recuento, que es lo único que ese recuento produce
    /// (#139, #290).
    ///
    /// `fs.dir_size` no publica nada ni muta nada: su resultado **es** su
    /// progreso terminal. Sin esto, la ventana lanzaría la cuenta, la vería
    /// terminar en el tablero y no diría nunca cuánto ocupaba.
    ///
    /// **Un total con algo ilegible dentro se dice DISTINTO**: un recuento
    /// sirve para decidir si algo CABE en el destino, así que darlo redondo
    /// sin haberlo podido contar entero es una respuesta equivocada, no una
    /// incompleta. Con ilegibles se dice «al menos», que es lo que se sabe.
    ///
    /// `unreadable: None` —un daemon 0.52, que no los contaba— se lee como
    /// cero, igual que en el TUI (`refresh.rs`): callar el total porque el
    /// otro extremo es viejo sería peor que darlo. Las dos superficies tienen
    /// que decir lo mismo ante el mismo progreso.
    ///
    /// Solo con `Completed`: una cuenta cancelada o fallida no tiene total que
    /// dar, y pintar el parcial de una cancelación como si fuera la respuesta
    /// es el mismo error de arriba con otro nombre.
    /// Una transferencia que CHOCÓ abre la pregunta que faltaba (#274).
    ///
    /// La ventana manda siempre `CollisionPolicy::Fail`, que es el default
    /// seguro —sobrescribir o renombrar son decisiones del lector—, pero no
    /// tenía dónde tomarlas: quedaba una task fallida en el tablero y ningún
    /// camino hacia delante, mientras el TUI sí ofrece las cuatro salidas.
    ///
    /// Solo con un `Conflict` y solo si la task trae con qué reintentar: un
    /// borrado o un undo no tienen otra política que ofrecer, y una task ajena
    /// no es de esta ventana.
    fn ofrecer_reintento(&mut self, p: &norte_proto::TaskProgress) -> Vec<ViewChange> {
        if !matches!(
            p.state,
            norte_proto::TaskState::Failed {
                error: norte_proto::Error::Conflict { .. }
            }
        ) {
            return Vec::new();
        }
        let Some(con) = self
            .tasks
            .get(&p.task_id.get())
            .and_then(|t| t.reintento.clone())
        else {
            return Vec::new();
        };
        // El destino, en su propio campo y enmascarado: es un nombre de
        // fichero del otro extremo, y es LO que el lector tiene que mirar para
        // decidir si sobrescribe.
        let (destino, destino_hostil) =
            norte_frontend::display_name(con.to.display_lossy().as_bytes());
        let modal = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id: modal,
            title_key: "modal-collision-title".to_owned(),
            destination: Some(crate::dto::DialogLine {
                text: clamp_display(destino),
                hostile: destino_hostil,
            }),
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![crate::dto::DialogLine {
                text: clamp_display(norte_i18n::t_in(self.lang, "modal-collision-body")),
                hostile: false,
            }],
            overflow_note: String::new(),
            // Las MISMAS cuatro que el TUI, y en el mismo orden: es la tabla
            // de `dialog.*` del catálogo compartido, no una lista inventada
            // aquí.
            choices: vec![
                DialogChoice {
                    id: "overwrite".to_owned(),
                    label_key: "dialog-overwrite".to_owned(),
                    // Sobrescribir DESTRUYE lo que hay en el destino.
                    destructive: true,
                },
                DialogChoice {
                    id: "newer".to_owned(),
                    label_key: "dialog-newer".to_owned(),
                    // También sobrescribe, solo que condicionado a la fecha.
                    destructive: true,
                },
                DialogChoice {
                    id: "rename".to_owned(),
                    label_key: "dialog-rename".to_owned(),
                    destructive: false,
                },
                DialogChoice {
                    id: "skip".to_owned(),
                    label_key: "dialog-skip".to_owned(),
                    destructive: false,
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
            id: modal,
            vista,
            input_crudo: String::new(),
            // Se abrió SOLO —llega cuando la task termina, encima de lo que el
            // lector estuviera haciendo—, así que la primera respuesta solo lo
            // reconoce. Es la misma regla que una aprobación de agente, y aquí
            // importa igual: la primera opción es «sobrescribir».
            reconocido: false,
            al_confirmar: Some(Pendiente::Reintentar { con }),
        });
        vec![ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        }]
    }

    fn decir_el_recuento(&mut self, p: &norte_proto::TaskProgress) -> Vec<ViewChange> {
        if p.kind != norte_proto::TaskKind::DirSize
            || !matches!(p.state, norte_proto::TaskState::Completed)
        {
            return Vec::new();
        }
        let tamano = norte_frontend::human_bytes(p.bytes_done);
        let cuantas = p.entries_done.to_string();
        let saltados = p.unreadable.unwrap_or(0);
        let mensaje = if saltados > 0 {
            norte_i18n::ta_in(
                self.lang,
                "msg-dir-size-partial",
                &[
                    ("size", &tamano),
                    ("count", &cuantas),
                    ("skipped", &saltados.to_string()),
                ],
            )
        } else {
            norte_i18n::ta_in(
                self.lang,
                "msg-dir-size",
                &[("size", &tamano), ("count", &cuantas)],
            )
        };
        self.status.message = Some(clamp_display(mensaje));
        vec![ViewChange::Status(self.status.clone())]
    }

    /// Un lote de renombrado que acaba de terminar: se le pide su informe.
    ///
    /// Es la ÚNICA señal de que el directorio se quedó A MEDIAS, y hay que
    /// pedirla AUNQUE la Task diga `Completed`: el desenlace de la Task
    /// habla del lote, y el informe habla de lo que quedó en el disco.
    ///
    /// Se pide también para un lote AJENO —otro cliente de esta sesión— por
    /// el mismo motivo: el directorio medio renombrado es el mismo mire quien
    /// lo mire, y quien tiene esta ventana delante es quien lo va a ver.
    fn pedir_informe_de_lote(
        &mut self,
        p: &norte_proto::TaskProgress,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        // Dos clases tienen informe, y las dos por el mismo motivo: lo que
        // quedó a medias no cabe en el desenlace de una Task.
        let lote = match p.kind {
            norte_proto::TaskKind::RenameBatch => true,
            norte_proto::TaskKind::Undo => false,
            _ => return,
        };
        let id = p.task_id;
        match self.tasks.get_mut(&id.get()) {
            Some(t) if !t.informe_pedido => t.informe_pedido = true,
            _ => return,
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let epoca = self.epoca_conexion;
        tokio::spawn(async move {
            let cual = if lote {
                Informe::Lote(backend.rename_batch_report(id).await)
            } else {
                Informe::Undo(backend.undo_report(id).await)
            };
            let _ = buzon
                .send(Mensaje::Informe(Box::new((epoca, id.get(), cual))))
                .await;
        });
    }

    /// Encolar una mutación falló: se dice, y si fue por el journal se
    /// queda dicho.
    ///
    /// `error_key` devuelve una CLAVE Fluent, y el contrato de
    /// `StatusView.message` dice «ya traducido por el host»: sin traducir, el
    /// usuario leía `err-not-found` en la barra.
    fn task_fallida(&mut self, e: &Error) -> Vec<BridgeEnvelope<UiUpdate>> {
        let clave = norte_frontend::error::error_key(e);
        self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
        // Un rechazo por journal no es una mutación que salió mal: es que
        // ESTA SESIÓN no muta hasta que el fichero se arregle (regla dura 4).
        // Eso dura más que un mensaje.
        self.journal_rehusado |= matches!(e, Error::JournalUnavailable);
        let cambio = self.cambio_de_banners();
        let parche = self.parche(vec![cambio]);
        let aviso = self.sobre(UiUpdate::Notice(UiNotice::Message {
            key: clave.to_owned(),
            detail: None,
        }));
        vec![parche, aviso]
    }

    /// Una entrada del lote la rechazó el daemon al encolar (#271).
    ///
    /// No pinta nada: cuenta. Con `CollisionPolicy::Fail` contra un destino
    /// poblado los rechazos son la norma, y N mensajes de los que sobrevive el
    /// último no dicen ni cuántos hubo.
    ///
    /// Sin lote abierto —no debería pasar, el bucle solo manda esto dentro de
    /// uno— cae a la barra, que es lo que hacía antes: perder el aviso entero
    /// es peor que pintarlo donde ya se pintaba.
    fn rechazo_de_lote(&mut self, e: &Error) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(lote) = self.lote.as_mut() else {
            return self.task_fallida(e);
        };
        lote.rechazadas += 1;
        // Un rechazo por journal sigue significando lo mismo aunque venga de
        // un lote: esta sesión NO muta hasta que el fichero se arregle (regla
        // dura 4), y eso dura más que cualquier resumen — y es lo único de un
        // rechazo suelto que SÍ viaja antes del final.
        let antes = self.journal_rehusado;
        self.journal_rehusado |= matches!(e, Error::JournalUnavailable);
        let banner_nuevo = self.journal_rehusado != antes;
        if !self.resumen_de_lote_si_cerrado() && !banner_nuevo {
            // Un parche por rechazo es la tormenta que esto existe para
            // apagar: mientras el lote siga abierto, nada viaja.
            return Vec::new();
        }
        let cambio = self.cambio_de_banners();
        vec![self.parche(vec![cambio])]
    }

    /// Anota el desenlace de UNA task del lote (#271). `true` si con ella el
    /// lote quedó resuelto y `status.message` ya lleva el resumen.
    ///
    /// El id se saca de la cuenta al anotarlo: un progreso terminal puede
    /// llegar más de una vez —un reanuncio tras reconectar trae el estado
    /// final otra vez— y la segunda no es un segundo desenlace.
    fn anota_desenlace_de_lote(&mut self, id: u64, estado: TaskStateView) -> bool {
        let Some(lote) = self.lote.as_mut() else {
            return false;
        };
        if !lote.ids.remove(&id) {
            return false;
        }
        if estado == TaskStateView::Done {
            lote.hechas += 1;
        } else {
            lote.fallidas += 1;
        }
        self.resumen_de_lote_si_cerrado()
    }

    /// Si el lote está resuelto, pone el resumen en la barra y lo cierra.
    fn resumen_de_lote_si_cerrado(&mut self) -> bool {
        let Some(lote) = self.lote.as_ref() else {
            return false;
        };
        if !lote.cerrado() {
            return false;
        }
        // Rechazada al encolar y terminada mal son el mismo desenlace para
        // quien mira: no llegó. Distinguirlas pediría dos números más en una
        // frase que tiene que caber en la barra.
        let total = lote.total.to_string();
        let bien = lote.hechas.to_string();
        let mal = (lote.rechazadas + lote.fallidas).to_string();
        self.lote = None;
        self.status.message = Some(clamp_display(norte_i18n::ta_in(
            self.lang,
            "msg-batch-summary",
            &[("total", &total), ("ok", &bien), ("fail", &mal)],
        )));
        true
    }

    /// Un informe llegó: al tablero, y delante si dejó algo a medias.
    fn informe(
        &mut self,
        epoca: u64,
        task_id: u64,
        cual: &Informe,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // Un informe que salió ANTES de la reconexión habla de una task de
        // otro daemon, y el id puede estar reutilizado: colgarlo de la fila
        // que hoy lleva ese número abriría un «se quedó a medias» sobre un
        // directorio que no es.
        if epoca != self.epoca_conexion {
            return Vec::new();
        }
        match cual {
            Informe::Lote(r) => self.informe_de_lote(task_id, r),
            Informe::Undo(r) => self.informe_de_undo(task_id, r),
        }
    }

    /// El informe de un undo llegó: al tablero, y delante si algo no volvió.
    ///
    /// Misma forma que [`Self::informe_de_lote`] porque es la misma pregunta
    /// —qué quedó sin deshacer— hecha sobre otra clase de Task.
    fn informe_de_undo(
        &mut self,
        task_id: u64,
        resultado: &Result<norte_proto::methods::PolicyUndoReportResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // Una fila que ya no está NO tira el informe: el tablero está acotado
        // y la task pudo caerse mientras el informe volaba, pero lo que se
        // perdía así era justo el «se quedó a medias», que jamás se doblega
        // dentro de «fue bien». Sin fila se salta el detalle y se enseña
        // igual lo que haya que decir.
        let fallo_la_task = self.tasks.get(&task_id).is_some_and(|viva| {
            matches!(
                viva.vista.state,
                crate::dto::TaskStateView::Failed | crate::dto::TaskStateView::Cancelled
            )
        });
        let (detalle, cuerpo) = match resultado {
            Ok(r) => (
                norte_i18n::ta_in(self.lang, "task-undo-done", &[("n", &r.undone.to_string())]),
                self.cuerpo_de_undo(r),
            ),
            Err(e) => {
                let clave = if matches!(e, Error::Unsupported) {
                    "modal-undo-unsupported"
                } else {
                    "modal-undo-report-failed"
                };
                (
                    norte_i18n::t_in(self.lang, "task-undo-unverified"),
                    vec![crate::dto::DialogLine {
                        text: clamp_display(norte_i18n::t_in(self.lang, clave)),
                        hostile: false,
                    }],
                )
            }
        };
        if let Some(t) = self.tasks.get_mut(&task_id) {
            t.vista.detail = Some(clamp_display(detalle));
            t.vista.detail_hostile = false;
        }
        let mut cambios = vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
        }];
        let hay_que_decirlo = match resultado {
            Ok(r) => !Self::undo_limpio(r),
            Err(_) => fallo_la_task,
        };
        let mut caidos = Vec::new();
        if hay_que_decirlo {
            let (cambio, cayeron) =
                self.abrir_informe("modal-undo-report-title".to_owned(), cuerpo);
            cambios.push(cambio);
            caidos = cayeron;
        }
        let mut salidas = vec![self.parche(cambios)];
        salidas.extend(caidos);
        salidas
    }

    /// `true` si el undo devolvió TODO lo que tocaba.
    ///
    /// Lo saltado cuenta como no-limpio: una entrada irreversible o una
    /// creación que se queda porque el destino no tiene papelera son cosas
    /// que NO volvieron, y un informe que las callara diría que el árbol
    /// está como estaba.
    fn undo_limpio(r: &norte_proto::methods::PolicyUndoReportResult) -> bool {
        r.blocked.is_none()
            && r.batch_stuck.is_none()
            && r.compensations_lost == 0
            && r.denied_total == 0
            && r.skipped_irreversible == 0
            && r.skipped_created_no_trash == 0
    }

    /// El cuerpo del informe de un undo: qué volvió y qué no.
    fn cuerpo_de_undo(
        &self,
        r: &norte_proto::methods::PolicyUndoReportResult,
    ) -> Vec<crate::dto::DialogLine> {
        let linea = |texto: String| crate::dto::DialogLine {
            text: clamp_display(texto),
            hostile: false,
        };
        let mut cuerpo = vec![linea(norte_i18n::ta_in(
            self.lang,
            "modal-undo-summary",
            &[
                ("undone", &r.undone.to_string()),
                ("skipped", &r.skipped_irreversible.to_string()),
            ],
        ))];
        if r.skipped_created_no_trash > 0 {
            cuerpo.push(linea(norte_i18n::ta_in(
                self.lang,
                "modal-undo-left-in-place",
                &[("n", &r.skipped_created_no_trash.to_string())],
            )));
        }
        if let Some(b) = &r.blocked {
            // El `seq` es una referencia OPACA: sirve para CITAR la entrada
            // contra el journal del server, no para interpretarla aquí.
            cuerpo.push(linea(norte_i18n::ta_in(
                self.lang,
                "modal-undo-blocked",
                &[
                    ("seq", &b.seq.to_string()),
                    (
                        "error",
                        &norte_i18n::t_in(self.lang, norte_frontend::error::error_key(&b.error)),
                    ),
                ],
            )));
        }
        if let Some(paso) = &r.batch_stuck {
            cuerpo.push(linea(norte_i18n::t_in(self.lang, "modal-undo-batch-stuck")));
            cuerpo.push(Self::linea_de_ruta(&paso.to));
        }
        if r.compensations_lost > 0 {
            cuerpo.push(linea(norte_i18n::ta_in(
                self.lang,
                "modal-batch-compensations-lost",
                &[("n", &r.compensations_lost.to_string())],
            )));
        }
        if r.denied_total > 0 {
            cuerpo.push(linea(norte_i18n::ta_in(
                self.lang,
                "modal-undo-denied",
                &[("n", &r.denied_total.to_string())],
            )));
        }
        cuerpo
    }

    /// El informe llegó: se apunta en el tablero y, si el lote dejó algo a
    /// medias, se dice DELANTE.
    ///
    /// Dos superficies y no una: la fila del tablero se queda con el resumen
    /// —sobrevive a que alguien cierre lo que sea—, y el diálogo es lo que
    /// hace que un directorio medio renombrado no pase inadvertido. Un lote
    /// limpio no abre nada: no hay nada que buscar.
    fn informe_de_lote(
        &mut self,
        task_id: u64,
        resultado: &Result<norte_proto::methods::FsRenameBatchReportResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // Ver [`Self::informe_de_undo`]: sin fila, el informe se enseña
        // igual. Lo que no se hace es inventarse una fila para colgarlo.
        let fallo_la_task = self.tasks.get(&task_id).is_some_and(|viva| {
            matches!(
                viva.vista.state,
                crate::dto::TaskStateView::Failed | crate::dto::TaskStateView::Cancelled
            )
        });
        let (detalle, cuerpo) = match resultado {
            Ok(r) => (Self::detalle_de_lote(self.lang, r), self.cuerpo_de_lote(r)),
            Err(e) => {
                let clave = if matches!(e, Error::Unsupported) {
                    "modal-batch-unsupported"
                } else {
                    "modal-batch-report-failed"
                };
                (
                    norte_i18n::t_in(self.lang, "task-batch-unverified"),
                    vec![crate::dto::DialogLine {
                        text: clamp_display(norte_i18n::t_in(self.lang, clave)),
                        hostile: false,
                    }],
                )
            }
        };
        // El detalle de la fila es «qué va por dentro» mientras corre; ya
        // terminada, lo que importa es en qué quedó. No hay más progreso
        // detrás que lo pise: el estado es terminal.
        if let Some(t) = self.tasks.get_mut(&task_id) {
            t.vista.detail = Some(clamp_display(detalle));
            t.vista.detail_hostile = false;
        }
        let mut cambios = vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
        }];
        // Se abre por lo que el INFORME dice, no por cómo terminó la Task:
        // un lote `Completed` con un paso atascado es exactamente el caso
        // que el desenlace de la Task no cuenta.
        let hay_que_decirlo = match resultado {
            Ok(r) => !Self::lote_limpio(r),
            // Un informe que no se pudo pedir sobre un lote que además falló
            // deja el directorio sin explicación: eso se dice delante. Si el
            // lote terminó bien, la fila del tablero basta.
            Err(_) => fallo_la_task,
        };
        let mut caidos = Vec::new();
        if hay_que_decirlo {
            let (cambio, cayeron) =
                self.abrir_informe("modal-batch-report-title".to_owned(), cuerpo);
            cambios.push(cambio);
            caidos = cayeron;
        }
        let mut salidas = vec![self.parche(cambios)];
        salidas.extend(caidos);
        salidas
    }

    /// `true` si el lote no dejó nada que buscar ni que rematar.
    fn lote_limpio(r: &norte_proto::methods::FsRenameBatchReportResult) -> bool {
        r.stuck.is_none()
            && r.uncertain.is_none()
            && r.failed_pair.is_none()
            && r.compensations_lost == 0
            && r.rolled_back == 0
    }

    /// El resumen de una línea que se queda en la fila del tablero.
    fn detalle_de_lote(
        lang: norte_i18n::Lang,
        r: &norte_proto::methods::FsRenameBatchReportResult,
    ) -> String {
        if Self::lote_limpio(r) {
            return norte_i18n::ta_in(lang, "task-batch-applied", &[("n", &r.applied.to_string())]);
        }
        norte_i18n::ta_in(
            lang,
            "task-batch-half",
            &[
                ("applied", &r.applied.to_string()),
                ("back", &r.rolled_back.to_string()),
            ],
        )
    }

    /// El cuerpo del informe: qué se aplicó, qué no se pudo devolver, y CÓMO
    /// SE LLAMA AHORA lo que se quedó a medias.
    ///
    /// El nombre de ahora es lo único accionable que hay aquí, así que va
    /// como línea de ruta —enmascarada y marcada— y no dentro de una frase:
    /// una ruta metida en una frase la puede suplantar otra ruta.
    fn cuerpo_de_lote(
        &self,
        r: &norte_proto::methods::FsRenameBatchReportResult,
    ) -> Vec<crate::dto::DialogLine> {
        let frase = |clave: &str| crate::dto::DialogLine {
            text: clamp_display(norte_i18n::t_in(self.lang, clave)),
            hostile: false,
        };
        let mut cuerpo = vec![crate::dto::DialogLine {
            text: clamp_display(norte_i18n::ta_in(
                self.lang,
                "modal-batch-summary",
                &[
                    ("applied", &r.applied.to_string()),
                    ("back", &r.rolled_back.to_string()),
                ],
            )),
            hostile: false,
        }];
        if let Some(paso) = &r.stuck {
            cuerpo.push(frase("modal-batch-stuck"));
            cuerpo.push(Self::linea_de_ruta(&paso.to));
            cuerpo.push(frase(if paso.journalled {
                "modal-batch-stuck-journalled"
            } else {
                "modal-batch-stuck-unjournalled"
            }));
        }
        if let Some(paso) = &r.uncertain {
            cuerpo.push(frase("modal-batch-uncertain"));
            cuerpo.push(Self::linea_de_ruta(&paso.to));
        }
        if r.compensations_lost > 0 {
            cuerpo.push(crate::dto::DialogLine {
                text: clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "modal-batch-compensations-lost",
                    &[("n", &r.compensations_lost.to_string())],
                )),
                hostile: false,
            });
        }
        cuerpo
    }

    /// Apila un diálogo, con techo.
    ///
    /// El techo existe porque la pila la alimenta el WIRE desde la tarea 5.3
    /// (aprobaciones e informes, también de tasks ajenas). Se cae el más
    /// viejo SIN reconocer —lo que nadie ha llegado a mirar— y nunca el de
    /// arriba, que es el que se está contestando; si todos están reconocidos,
    /// el más viejo. Que se cayó alguno se DICE: una pregunta que desaparece
    /// en silencio es peor que una pila larga.
    fn apilar_dialogo(&mut self, dialogo: Dialogo) -> Vec<BridgeEnvelope<UiUpdate>> {
        let mut fuera = Vec::new();
        if self.dialogos.len() >= MAX_DIALOGS {
            // Se sacrifica un INFORME antes que una decisión: el informe
            // también vive en la fila del tablero, y una aprobación que
            // desaparece deja a un agente esperando. Si solo quedan
            // decisiones, cae la más vieja — a esa el daemon le acabará
            // aplicando su TTL, que es una denegación.
            let victima = self
                .dialogos
                .iter()
                .position(|d| d.al_confirmar.is_none())
                .or_else(|| self.dialogos.iter().position(|d| !d.reconocido))
                .unwrap_or(0);
            self.dialogos.remove(victima);
            fuera.extend(self.decir("msg-dialog-dropped"));
        }
        self.dialogos.push(dialogo);
        fuera
    }

    /// Abre el diálogo de un informe. Solo informa: no tiene nada que
    /// ejecutar, y su única respuesta lo cierra.
    fn abrir_informe(
        &mut self,
        title_key: String,
        cuerpo: Vec<crate::dto::DialogLine>,
    ) -> (ViewChange, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key,
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: cuerpo,
            overflow_note: String::new(),
            choices: vec![DialogChoice {
                id: "ok".to_owned(),
                label_key: "dialog-ok".to_owned(),
                destructive: false,
            }],
            input: None,
            input_hostile: false,
        };
        let caidos = self.apilar_dialogo(Dialogo {
            id,
            vista,
            input_crudo: String::new(),
            // Se abre SOLO, cuando el daemon contesta.
            reconocido: false,
            al_confirmar: None,
        });
        (
            ViewChange::Dialogs {
                dialogs: self.vistas_de_dialogos(),
            },
            caidos,
        )
    }

    /// `task.cancel`: le pide parar a UNA task, y dice a cuál o que no hay.
    ///
    /// Qué task es depende de dónde está el foco, y no por gusto: con el
    /// panel de procesos delante, el tablero pinta un cursor, y una tecla que
    /// cancelara otra cosa dejaría ese cursor pintando una selección que no
    /// manda. Sin ese panel enfocado se cancela la ÚLTIMA viva, que es lo que
    /// hace el TUI con la misma tecla.
    fn cancelar_por_comando(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match self.task_a_cancelar() {
            Objetivo::Ninguna => (self.aplicada(), self.decir("msg-no-tasks")),
            Objetivo::Terminada => (self.aplicada(), self.decir("msg-task-finished")),
            Objetivo::Viva(id) => {
                // Una ventana sin efectos no aborta la task de OTRO cliente:
                // cancelar una copia deja el destino limpio o un
                // `.norte-partial`, o sea que toca el disco. Las propias sí,
                // que para lanzarlas ya hacía falta el interruptor.
                if self.efectos == crate::commands::Efectos::SoloLectura
                    && self.tasks.get(&id).is_some_and(|t| t.vista.foreign)
                {
                    return Self::no_muta();
                }
                let (ack, mut fuera) = self.cancelar(id);
                fuera.extend(self.decir("msg-cancelling"));
                (ack, fuera)
            }
        }
    }

    /// Mueve la fila elegida del tablero.
    ///
    /// Sin necesitar el foco del panel de procesos: el tablero se pinta
    /// también cuando ese hueco no existe —las tasks salen en el sobre— y un
    /// comando que solo funcionara con un hueco concreto abierto sería una
    /// tecla que depende de la disposición.
    fn mover_en_tablero(&mut self, atras: bool) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let filas = self.filas_de_tablero();
        if filas == 0 {
            return (self.aplicada(), self.decir("msg-no-tasks"));
        }
        let actual = self.cursor_procesos.min(filas - 1);
        self.cursor_procesos = if atras {
            actual.saturating_sub(1)
        } else {
            (actual + 1).min(filas - 1)
        };
        // Foto y no parche, por lo mismo que el cursor del panel de procesos:
        // no hay `ViewChange` para un hueco que no es un listado, y añadir
        // contrato por un índice es contrato para nada.
        let snap = self.snapshot();
        (
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// Quita del tablero la fila elegida, si YA terminó.
    ///
    /// Una viva no se descarta: pararla es `task.cancel`, y quitar de la
    /// vista algo que sigue escribiendo en el disco es perder de vista
    /// justo lo que hay que mirar.
    fn descartar_task(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let filas = self.filas_de_tablero();
        if filas == 0 {
            return (self.aplicada(), self.decir("msg-no-tasks"));
        }
        let i = self.cursor_procesos.min(filas - 1);
        let Some((&id, viva)) = self.tasks_visibles().nth(i) else {
            return (self.aplicada(), self.decir("msg-no-tasks"));
        };
        if !Self::terminal(viva.vista.state) {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-task-running".to_owned(),
                },
                self.decir("host-task-running"),
            );
        }
        self.tasks.remove(&id);
        self.undos_sin_task(id);
        // El cursor se queda donde estaba, clampado: descartar la última deja
        // la selección en la que ahora es la última, no en la primera.
        let filas = self.filas_de_tablero();
        self.cursor_procesos = self.cursor_procesos.min(filas.saturating_sub(1));
        let mut fuera = vec![self.parche(vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
        }])];
        let snap = self.snapshot();
        fuera.push(self.sobre(UiUpdate::Snapshot(Box::new(snap))));
        (self.aplicada(), fuera)
    }

    /// Suelta el «deshaciendo» de una sesión cuya task se descarta.
    ///
    /// Descartar la fila de un undo que terminó es lo mismo que verlo
    /// terminar: si no se soltara aquí, esa sesión se quedaría marcada como
    /// «deshaciendo» para siempre y `u` sobre ella se rehusaría sin motivo.
    fn undos_sin_task(&mut self, task_id: u64) {
        if let Some(sesion) = self.agencia.undos.remove(&task_id) {
            self.agencia.sesiones.deshecha(&sesion);
        }
    }

    /// A qué task le toca parar.
    fn task_a_cancelar(&self) -> Objetivo {
        if self.procesos_tienen_el_foco() {
            // La del cursor, sea cual sea su estado: la eligió un humano
            // mirándola. Si ya terminó se DICE, en vez de saltar a otra —
            // cancelar una task que no es la señalada es peor que no
            // cancelar nada.
            let Some((id, viva)) = self.tasks_visibles().nth(
                self.cursor_procesos
                    .min(self.filas_de_tablero().saturating_sub(1)),
            ) else {
                return Objetivo::Ninguna;
            };
            return if Self::sigue_viva(viva) {
                Objetivo::Viva(*id)
            } else {
                Objetivo::Terminada
            };
        }
        // El tablero va por id, y el daemon los reparte crecientes: la última
        // viva es la de id mayor.
        self.tasks
            .iter()
            .rev()
            .find(|(_, t)| Self::sigue_viva(t))
            .map_or(Objetivo::Ninguna, |(id, _)| Objetivo::Viva(*id))
    }

    /// `true` si esta clase de task ESCRIBE.
    ///
    /// Por la clave del catálogo y no por `TaskKind`, que es no exhaustivo:
    /// una clase de un daemon más nuevo cae en `unknown` y NO cuenta como
    /// mutación, que es el lado seguro — apagar el aviso del journal por algo
    /// que este host no sabe qué hace sería apagarlo por si acaso.
    fn muta(clase: &str) -> bool {
        matches!(
            clase,
            "copy" | "move" | "delete" | "mkdir" | "rename-batch" | "undo" | "pack" | "sync"
        )
    }

    /// `true` si a esta task todavía se le puede pedir que pare.
    ///
    /// Pregunta al progreso EN VIVO y no a la vista proyectada: entre que el
    /// daemon marca el desenlace y el `Mensaje::Progreso` sale del buzón, la
    /// vista dice que sigue corriendo. Sobre esa foto se contestaba
    /// «cancelando…» a algo ya terminado y se elegía como «última viva» a una
    /// que ya no lo era, dejando corriendo la que de verdad quedaba.
    fn sigue_viva(t: &TaskViva) -> bool {
        !t.progreso.borrow().state.is_terminal()
    }

    /// `true` si el foco está en el panel de procesos.
    fn procesos_tienen_el_foco(&self) -> bool {
        self.roles
            .get(RoleId::Active)
            .and_then(|s| kind_de(&self.arbol, s))
            .is_some_and(|k| k.as_str() == "processes")
    }

    /// Dice una frase: en la barra de estado Y como aviso.
    ///
    /// Las dos cosas y por la misma llamada, que es lo que ya hace una task
    /// fallida: la barra es donde se lee al mirar, y el aviso es lo que el
    /// renderer puede anunciar a un lector de pantalla.
    fn decir_con(&mut self, clave: &str, args: &[(&str, &str)]) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.status.message = Some(clamp_display(norte_i18n::ta_in(self.lang, clave, args)));
        let parche = self.parche(vec![ViewChange::Status(self.status.clone())]);
        let aviso = self.sobre(UiUpdate::Notice(UiNotice::Message {
            key: clave.to_owned(),
            detail: None,
        }));
        vec![parche, aviso]
    }

    /// Como [`Self::decir_con`], sin argumentos.
    fn decir(&mut self, clave: &str) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
        let parche = self.parche(vec![ViewChange::Status(self.status.clone())]);
        let aviso = self.sobre(UiUpdate::Notice(UiNotice::Message {
            key: clave.to_owned(),
            detail: None,
        }));
        vec![parche, aviso]
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

    /// Cuántos elementos como mucho enseña el cuerpo de un diálogo.
    ///
    /// El cuerpo no puede crecer con la selección —un lote de mil ficheros no
    /// cabe en una pregunta— así que se acota. Que se acotó lo dice
    /// [`Self::nota_de_recorte`]: una lista recortada en silencio describe
    /// una operación más pequeña que la que se va a ejecutar, y esta es la
    /// última pantalla donde todavía se puede decir que no.
    const MAX_LINEAS_DIALOGO: usize = 16;

    /// La frase que dice que el cuerpo enseña menos de lo que hay. Vacía si
    /// los enseña todos.
    fn nota_de_recorte(&self, mostrados: usize, total: usize) -> String {
        if mostrados >= total {
            return String::new();
        }
        clamp_display(norte_i18n::ta_in(
            self.lang,
            "dialog-body-truncated",
            &[
                ("shown", &mostrados.to_string()),
                ("total", &total.to_string()),
            ],
        ))
    }

    /// Una ruta como LÍNEA de diálogo: enmascarada, acotada, y diciendo si
    /// lo pintado difiere de lo real.
    ///
    /// Una sola función porque los cinco diálogos que enseñan rutas —crear,
    /// buscar, borrar, transferir y aprobar— tienen que decirlo igual, y el
    /// sitio donde uno de ellos se olvida del `bool` es exactamente donde
    /// alguien aprueba otra cosa.
    fn linea_de_ruta(p: &VPath) -> crate::dto::DialogLine {
        let (texto, hostil) = norte_frontend::path_display(p);
        // El RECORTE también altera lo pintado, y ocurre DESPUÉS del
        // veredicto de `path_display`: una ruta UTF-8 limpia y larga —doce
        // segmentos de 255 bytes bastan— se pintaba con `…` al final y se
        // declaraba fiel. La elipsis es un carácter legal en un nombre, así
        // que quien lee no puede distinguir «se llama así» de «esto está
        // cortado», y en el informe de un lote ese nombre es lo único
        // accionable que hay: se va a teclear a mano.
        let recortado = texto.len() > crate::bridge::MAX_STRING_BYTES;
        crate::dto::DialogLine {
            text: clamp_display(texto),
            hostile: hostil || recortado,
        }
    }

    fn vistas_de_dialogos(&self) -> Vec<DialogView> {
        self.dialogos.iter().map(|d| d.vista.clone()).collect()
    }

    /// El tablero que cruza el puente, acotado a [`MAX_TASKS`].
    ///
    /// El desalojo de `registrar_task` solo puede tirar tasks TERMINADAS, así
    /// que un lote más grande que el tope —marcar tres mil ficheros y pulsar
    /// F5 es el flujo normal— no tiene nada que desalojar y el mapa crece por
    /// encima del tope que el contrato del puente promete. Se acota aquí, que
    /// es donde el número significa algo: cuántas filas viajan.
    ///
    /// Se quedan las MÁS NUEVAS (el mapa está ordenado por id, que es
    /// monótono): lo que interesa de un lote en marcha es su frente, no las
    /// primeras que se encolaron.
    fn vistas_de_tasks(&self) -> Vec<TaskView> {
        self.tasks_visibles()
            .map(|(_, t)| t.vista.clone())
            .collect()
    }

    /// Las tasks que CRUZAN el puente, en el orden en que se pintan.
    ///
    /// UNA sola definición de «las visibles», y no por gusto: el tablero se
    /// recorta a [`MAX_TASKS`] y el cursor es un ÍNDICE. Mientras el recorte
    /// vivía solo aquí y el cursor contaba sobre el mapa entero, con más de
    /// 256 tasks —marcar tres mil ficheros y pulsar F5 es el flujo normal, y
    /// el desalojo solo se lleva las TERMINADAS— la fila resaltada y la task
    /// que se cancelaba eran dos tasks distintas. Es literalmente lo que el
    /// rustdoc del panel prohíbe: «dos listas de tareas se separan, y la que
    /// se ve deja de ser la que se cancela».
    fn tasks_visibles(&self) -> impl Iterator<Item = (&u64, &TaskViva)> {
        let sobran = self.tasks.len().saturating_sub(MAX_TASKS);
        self.tasks.iter().skip(sobran)
    }

    /// Cuántas filas tiene el tablero PINTADO.
    fn filas_de_tablero(&self) -> usize {
        self.tasks.len().min(MAX_TASKS)
    }

    fn terminal(estado: TaskStateView) -> bool {
        matches!(
            estado,
            TaskStateView::Done | TaskStateView::Failed | TaskStateView::Cancelled
        )
    }

    /// Proyecta un snapshot del daemon a lo que el renderer pinta.
    fn vista_de(p: &norte_proto::TaskProgress) -> TaskView {
        // Por la regla COMPARTIDA, que cae a las entradas cuando no hay
        // bytes totales: esto solo miraba bytes, así que un borrado —que no
        // cuenta bytes— cruzaba el puente sin porcentaje de principio a fin.
        let porcentaje = norte_frontend::tasks::progress_pct(p);
        TaskView {
            task_id: p.task_id.get(),
            kind: clase_de_task(p.kind).to_owned(),
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
            detail_hostile: p
                .current
                .as_ref()
                .is_some_and(|path| norte_frontend::path_display(path).1),
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
        self.navegar_hueco(self.activo(), destino, trail, backend, buzon)
    }

    /// Lo mismo, sobre un hueco que NO tiene por qué ser el activo.
    ///
    /// Existe porque hay gestos que mueven OTRO panel: el espejo manda la
    /// ubicación del activo al destino, y un selector de volúmenes abierto
    /// para un lado de la pantalla monta ahí. Antes esto se hacía leyendo
    /// `activo()` tres veces por dentro, así que no había forma de decirlo.
    fn navegar_hueco(
        &mut self,
        slot: u32,
        destino: &VPath,
        trail: Trail,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.token += 1;
        let token = RequestToken(self.token);
        let destino = destino.clone();
        let Some(hueco) = self.huecos.get_mut(&slot) else {
            return Vec::new();
        };
        let anterior = hueco.pane.dir().clone();
        // Un `Replay` es el rastro reproduciéndose: registrar ahí haría que
        // `back` se alimentara de sí mismo y el lector oscilara entre dos
        // directorios.
        if anterior != destino && trail == Trail::Record {
            hueco.historial.record(anterior);
        }
        // La memoria del cursor se toma con el dir que se ABANDONA todavía
        // puesto (contrato de `remember_cursor`).
        hueco.pane.remember_cursor();
        hueco.estado = SlotState::Loading;
        hueco.en_vuelo = Some(token);
        // El drenaje vive MÁS que la primera página: se marca aquí y solo lo
        // releva otra navegación del mismo hueco.
        hueco.drenando = Some(token);

        self.pedir_listado(slot, &destino, token, backend, buzon);

        let cambio = ViewChange::SlotState {
            slot_id: slot,
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
        let ahora = u64::try_from(ahora_ms()).unwrap_or(0);
        let mut body = self.capturar_sesion(ahora);
        let vivos: Vec<SlotId> = self.huecos.keys().map(|id| SlotId(*id)).collect();
        if self
            .sesion
            .policy
            .prepare(&mut body, &vivos, ahora)
            .is_none()
        {
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
    /// Pide a los plugins lo que quieran decir de la VENTANA VISIBLE.
    ///
    /// Dos cosas en un viaje —insignias y valores de columna `plugin:`—
    /// porque son la misma pregunta sobre las mismas rutas, y el TUI ya lo
    /// hace así.
    ///
    /// De la ventana y NO del listado, que es donde esto se separa del TUI:
    /// el terminal decora «todas las entradas cargadas» porque su pane no
    /// declara una ventana, y aquí el renderer sí la declara. Cada llamada
    /// levanta una instancia de wasm por plugin y #224 midió **167 ms por
    /// página de 20 sobre 2000 entradas**: pedirlo para lo que no se ve es
    /// pagar ese precio por nada, multiplicado por el tamaño del directorio.
    ///
    /// Un hueco OCULTO no pregunta. Lo que no se ve no se trae, igual que su
    /// listado.
    ///
    /// Todo fail-soft: sin decoradores consentidos, con el catálogo caído o
    /// con la RPC rota, el listado se pinta igual y sin insignias. Una
    /// decoración es cosmética por contrato (ADR 0037).
    fn adornar(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        if self.oculto(slot) {
            return;
        }
        let columnas = self
            .huecos
            .get(&slot)
            .map(|h| self.columnas.plugin_ids_for(h.pane.dir().scheme()))
            .unwrap_or_default();
        let Some(hueco) = self.huecos.get_mut(&slot) else {
            return;
        };
        // Una tanda por hueco, comprobada ANTES de elegir candidatos: al
        // revés, los elegidos quedarían marcados como pedidos sin haberlo
        // sido y no se pedirían nunca más. Es la misma trampa que `sondear`
        // documenta, y se cae en ella igual de fácil.
        if hueco.adornando {
            return;
        }
        let primera = usize::try_from(hueco.primera_visible).unwrap_or(0);
        let cuantas = usize::try_from(hueco.visibles).unwrap_or(0);
        let candidatos: Vec<VPath> = hueco
            .pane
            .entries()
            .iter()
            .skip(primera)
            .take(cuantas)
            .map(|e| e.path.clone())
            .filter(|p| !hueco.adornadas.contains(p))
            .collect();
        if candidatos.is_empty() {
            return;
        }
        for p in &candidatos {
            hueco.adornadas.insert(p.clone());
        }
        let dir = hueco.pane.dir().clone();
        hueco.adornando = true;
        let cancelar = hueco.cancelar_sondeo.clone();
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let crudas = backend
                .plugin_decorate(candidatos.clone())
                .await
                .unwrap_or_default();
            let adornos = norte_frontend::merge_decorations(&candidatos, &crudas);
            let celdas = celdas_de_plugin(&backend, &columnas, &candidatos, || {
                cancelar.load(std::sync::atomic::Ordering::SeqCst)
            })
            .await;
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::Adornos(Box::new((
                    slot, dir, adornos, celdas,
                ))))))
                .await;
        });
    }

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
        ultimo: bool,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let hueco = self.huecos.get_mut(&slot)?;
        if hueco.drenando != Some(token) {
            // Un lote de una navegación que ya fue relevada: pegarlo sería
            // mezclar dos árboles en una pantalla.
            return None;
        }
        if ultimo {
            // Se acabó el stream: este hueco ya no está creciendo.
            hueco.drenando = None;
        }
        if batch.is_empty() && !hueco.filas_por_publicar {
            // Nada que pegar y nada pendiente: el stream cerró sin resto.
            return None;
        }
        // Lo que se ve AHORA, para compararlo con lo que se verá. Un
        // directorio de cien mil entradas se drena en lotes de 500 y cada
        // lote publicaba su parche: doscientos parches en ráfaga contra un
        // canal de 64, o sea que cualquier suscriptor que no drene a esa
        // velocidad recibe `Lagged` y tiene que pedir una foto entera. Y casi
        // todos esos parches llevaban las MISMAS filas: lo que se estaba
        // mezclando caía muy por debajo de la ventana visible (#252).
        let antes = self
            .huecos
            .get(&slot)
            .map(|h| self.filas_de(h))
            .unwrap_or_default();
        if !batch.is_empty()
            && let Some(hueco) = self.huecos.get_mut(&slot)
        {
            hueco.pane.extend(batch);
        }
        let despues = self
            .huecos
            .get(&slot)
            .map(|h| self.filas_de(h))
            .unwrap_or_default();
        // Callar un parche no es gratis: `extend` sube la ÉPOCA del listado y
        // el renderer nombra cada fila con la época en la que la vio, así que
        // un renderer al que se le callan todos los parches se queda con una
        // época vieja y cada clic suyo se rechaza por rancio. Por eso lo que
        // se calla se APUNTA, y el último lote —aunque venga vacío, que pasa
        // cuando el resto es múltiplo exacto del lote— salda la deuda.
        let calla = !ultimo && antes == despues;
        if let Some(h) = self.huecos.get_mut(&slot) {
            // Se calla: queda deuda. Se publica: la deuda se salda, porque el
            // parche lleva la época de AHORA.
            h.filas_por_publicar = calla;
        }
        if calla {
            return None;
        }
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
        // La bandera se baja SOLO si lo que llega describe este listado. Una
        // tanda cancelada que aterriza tarde bajaba la de la tanda NUEVA, y
        // entonces `sondear` dejaba lanzar una segunda sobre el mismo hueco.
        if hueco.pane.dir() != dir {
            // El hueco está en OTRO directorio: pegarle estos tamaños sería
            // mentir sobre lo que se ve. (Un lote de relleno, en cambio, no
            // invalida nada: sube la época y desplaza índices, y aquí se casa
            // por ruta.)
            return None;
        }
        hueco.sondeando = false;
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
        // CON la reinterpretación que el panel tenga puesta (#57): sin ella
        // `pane.names-encoding` ciclaba por dentro y la pantalla no cambiaba,
        // que es un comando que solo se puede leer como roto. Lo que se
        // reinterpreta es el PINTADO; los bytes no se tocan, y la fila sigue
        // marcada como hostil (regla 1).
        let (texto, hostil) = norte_frontend::display_name_with(bytes, hueco.pane.name_encoding());
        // Lo sirve el PANE, que re-enmascara al servir: el host acumula pero
        // no es quien decide qué se pinta.
        let adorno = hueco.pane.decoration_for(&e.path);
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
            badge: adorno
                .and_then(|d| d.badge.clone())
                .map(clamp_display)
                .unwrap_or_default(),
            badge_hostile: adorno.is_some_and(|d| d.badge_hostile),
            badge_role: adorno
                .and_then(|d| d.role)
                .map_or_else(String::new, |r| r.as_kebab().to_owned()),
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
                    // Identidad: entera o vacía, jamás recortada.
                    column: identidad_de_columna(col),
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
        self.sesion.leida = body;
    }

    /// Coloca cada hueco donde la sesión dice que estaba.
    fn aplicar_sesion(&mut self, body: &norte_frontend::session::SessionBody) {
        for (id, hueco) in &mut self.huecos {
            let Some(estado) = body.slots.get(id) else {
                continue;
            };
            hueco.pane.begin_loading(estado.path.clone());
            // El orden y los ocultos se ESCRIBÍAN en la sesión y no los leía
            // nadie: la ventana se acordaba de dónde estabas y olvidaba cómo
            // lo estabas mirando, así que ordenar por tamaño o apartar los
            // dotfiles duraba hasta cerrar.
            hueco.pane.set_sort(estado.sort);
            hueco.pane.set_show_hidden(estado.show_hidden);
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
    fn capturar_sesion(&self, ahora: u64) -> norte_frontend::session::SessionBody {
        // Se parte de lo LEÍDO y se pisa solo lo propio: los huecos de otro
        // frontend y las disposiciones guardadas siguen ahí.
        //
        // Y `layouts` NO se toca. Hasta esta fase `self.arbol` era constante,
        // así que escribirlo era escribir lo que se había leído; ahora cambia
        // con `layout.pick` y con cada `Ctrl+→`, y el TUI adopta
        // `layouts["default"]` al arrancar. Curiosear un minuto en el selector
        // le cambiaba el arranque al TUI, que es lo que el rustdoc del campo
        // prohíbe por su nombre (ADR 0058 D5) y lo que
        // `aplicar_disposicion_elegida` promete no hacer: «se aplica para ESTA
        // ventana». Era verdad para la configuración y falso para la sesión,
        // que es la que lee el otro frontend.
        let mut body = self.sesion.leida.clone();
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
                    // El sello de edad, con el reloj de quien escribe. Un
                    // cero sellaba los huecos VIVOS con la época: para la
                    // barrida propia era inocuo —`0.saturating_sub(x)` nunca
                    // pasa de `MAX_AGE_MS`— pero el siguiente escritor con
                    // reloj de verdad los veía con treinta días y se los
                    // llevaba en su primer volcado.
                    touched_ms: ahora,
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
                continue;
            }
            let kind = kind_de(&self.arbol, *slot);
            match kind.as_ref().map(norte_frontend::layout::KindId::as_str) {
                Some("metadata") => {
                    slots.push(SlotView::Metadata(Box::new(self.hoja_de_atributos(*slot))));
                }
                Some("places") => slots.push(SlotView::Places(Box::new(self.barra_de_sitios(id)))),
                Some("processes") => slots.push(SlotView::Processes {
                    slot_id: id,
                    // Índice sobre las filas PINTADAS, que es lo que el
                    // renderer resalta. Sobre el mapa entero, con el tablero
                    // recortado, señalaba a otra.
                    cursor: (self.filas_de_tablero() > 0)
                        .then(|| self.cursor_procesos.min(self.filas_de_tablero() - 1) as u64),
                }),
                _ => {
                    let nombre =
                        kind.map_or_else(|| "unknown".to_owned(), |k| k.as_str().to_owned());
                    // El kind sale de un fichero de disposición y `KindId` no
                    // valida nada: es texto que puede traer controles, y acaba en
                    // el DOM y en un `aria-label`.
                    let (pintable, hostil) = norte_frontend::display_name(nombre.as_bytes());
                    slots.push(SlotView::Unsupported {
                        slot_id: id,
                        kind_name: clamp_display(pintable),
                        kind_name_hostile: hostil,
                    });
                }
            }
        }
        ViewSnapshot {
            compare: self.vista_comparacion(),
            sync: self.vista_sincronizacion(),
            connection: self.conexion.clone(),
            layout: self.disposicion(),
            slots,
            focus: Some(self.enfocado()),
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
            extensions: self.vista_extensiones(),
            agents: self.vista_agentes(),
            plugin_output: self.escritorio.salida.clone(),
            theme: self.vista_tema(),
            search: self.vista_busqueda(),
            layouts: self.vista_disposiciones(),
            columns: self.vista_columnas(),
            picker: self.vista_selector(),
            viewer: self.vista_visor(),
            ai_rename: self.vista_ia(),
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
        // Con los EFECTOS de esta ventana, no con todos: la paleta era la
        // única puerta que no pasaba por el keymap efectivo, así que una
        // ventana de solo lectura ofrecía copiar, mover y borrar. La guarda
        // de `aplicar_efecto` los rechazaba, pero ofrecer lo que se va a
        // rehusar es prometer algo que no se va a hacer.
        crate::commands::todos_con(self.efectos)
            .into_iter()
            .map(|cmd| norte_frontend::palette::Row {
                key: cmd.to_owned(),
                text: cmd.to_owned(),
                desc: norte_i18n::t_in(self.lang, &format!("help-cmd-{}", cmd.replace('.', "-"))),
                chord: first_chord(cmd, &self.efectivo)
                    .or_else(|| first_chord(cmd, self.resolver_visor_efectivo()))
                    .unwrap_or_else(|| "—".to_owned()),
                // Un comando propio es vocabulario de este proyecto.
                hostile: false,
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
                // Un tope, como cualquier otra lista que cruza: con la
                // consulta vacía TODAS las filas son visibles, y las de
                // plugin las pone un tercero.
                .take(crate::bridge::MAX_ROWS_PER_BATCH)
                .filter_map(|i| filas.get(*i))
                .map(|r| crate::dto::PaletteRowView {
                    text: clamp_display(r.text.clone()),
                    desc: clamp_display(r.desc.clone()),
                    chord: clamp_display(r.chord.clone()),
                    // Lo que se pinta DIFIERE de lo que el manifiesto dice.
                    // Una fila de plugin es texto de tercero en la pantalla
                    // donde se elige qué código correr: sin esto se pintaba
                    // enmascarada y sin decirlo.
                    hostile: r.hostile,
                    // Los comandos propios los implementa este host —salen de
                    // su propia lista— y los de PLUGIN los resuelve el
                    // daemon, que exige aprobada + encendida por su cuenta.
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
        let imagen = Self::imagen_de(v);
        let alto = self.alto_del_visor();
        // El TUI pinta la ruta del visor con el encoding del panel ENFOCADO
        // (`ui::panels`), y por lo mismo: es el fichero que se abrió desde
        // ahí.
        let (path, hostil) =
            norte_frontend::path_display_with(&v.path, self.hueco().pane.name_encoding());
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
            // El nombre ya viene enmascarado del modelo compartido; se acota
            // aquí como todo lo que cruza.
            preview_by: v.preview_plugin().map_or_else(String::new, |n| {
                // La MISMA clave que el TUI: el indicador «via …» no puede
                // decirse de dos maneras según quién pinte. El nombre ya
                // viene enmascarado del modelo compartido.
                clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "viewer-plugin-preview",
                    &[("plugin", n)],
                ))
            }),
            preview_lossy: v.preview_lossy(),
            image: imagen.clone().ok().flatten(),
            image_refused: match &imagen {
                Err(clave) => clamp_display(norte_i18n::t_in(self.lang, clave)),
                Ok(_) => String::new(),
            },
        })
    }

    /// Si lo que hay en el visor es una imagen PINTABLE, y si no, por qué no.
    ///
    /// `Ok(None)` = no es una imagen. `Ok(Some(_))` = lo es y se acepta.
    /// `Err(clave)` = lo es y se RECHAZA, con la clave que lo explica.
    ///
    /// Los tres topes del ADR 0069, y los tres son negativas y no recortes:
    ///
    /// - El **formato** sale de los bytes mágicos, nunca de la extensión: una
    ///   extensión es una afirmación de quien nombró el fichero.
    /// - Las **dimensiones declaradas** se comparan con el presupuesto ANTES
    ///   de que nadie decodifique. Un PNG de 64 KB puede declarar 60000×60000
    ///   y costar gigabytes; leerle la cabecera es la única defensa barata.
    ///   Una cabecera que no se entiende también se rechaza: «no sé» tratado
    ///   como «adelante» es la puerta que esto existe para cerrar.
    /// - Los **bytes** los acota quien los sirve, y un fichero que no cabe no
    ///   se pinta A MEDIAS: media imagen decodificada es una imagen de otra
    ///   cosa.
    fn imagen_de(
        v: &norte_frontend::viewer::Viewer,
    ) -> Result<Option<crate::dto::ImageView>, &'static str> {
        let Some(fmt) = v.image_kind() else {
            return Ok(None);
        };
        // La cabecera SIEMPRE cabe en lo que el visor ya leyó, así que
        // rechazar aquí no cuesta un viaje.
        let bytes = v.image_bytes().unwrap_or_default();
        let Some((w, h)) = norte_frontend::viewer::image_dimensions(bytes) else {
            return Err("viewer-image-unreadable");
        };
        if u64::from(w) * u64::from(h) > norte_frontend::viewer::PIXEL_BUDGET {
            return Err("viewer-image-too-large");
        }
        Ok(Some(crate::dto::ImageView {
            format: fmt.label().to_owned(),
            width: w,
            height: h,
        }))
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
            tabs: self.grupos_de_pestanas(),
            placements,
        }
    }

    /// Los grupos de PESTAÑAS que hay en pantalla.
    ///
    /// Uno por hueco colocado que viva dentro de una `Tabs`: las inactivas no
    /// se colocan —el repartidor compartido no las pinta— y sin esto la
    /// ventana enseñaría la de delante sin decir que hay otras dos abiertas.
    fn grupos_de_pestanas(&self) -> Vec<crate::dto::TabGroupView> {
        let mut fuera = Vec::new();
        for (slot, _) in &self.reparto.placements {
            let Some((huecos, activo)) = self.arbol.tabs_of(*slot) else {
                continue;
            };
            if huecos.len() < 2 {
                // Un grupo de UNA no es un grupo: pintarle una barra de
                // pestañas es cromo que no dice nada y que roba una fila.
                continue;
            }
            let SlotId(id) = *slot;
            fuera.push(crate::dto::TabGroupView {
                slot_id: id,
                tabs: huecos.iter().map(|t| self.pestana(*t)).collect(),
                active: activo as u64,
            });
        }
        fuera
    }

    /// Una pestaña: qué hueco lleva dentro y cómo se llama.
    ///
    /// El rótulo es el nombre del DIRECTORIO de su listado —no la ruta
    /// entera, que no cabe— enmascarado como cualquier otro nombre: uno
    /// hostil dentro de una pestaña es tan hostil como dentro de un listado.
    /// Un directorio raíz no tiene nombre: se cae al esquema, que es lo único
    /// que lo distingue de otro.
    fn pestana(&self, slot: SlotId) -> crate::dto::TabView {
        let SlotId(id) = slot;
        let (titulo, hostil) = if let Some(h) = self.huecos.get(&id) {
            let dir = h.pane.dir();
            match dir.file_name() {
                Some(seg) => norte_frontend::display_name(seg.as_bytes()),
                // Una raíz no tiene nombre: se cae al esquema, que es lo
                // único que la distingue de otra.
                None => (dir.scheme().to_owned(), false),
            }
        } else {
            // Lo que no es un listado se nombra por su KIND, que sale de un
            // fichero de disposición y entra por la misma puerta.
            let kind = kind_de(&self.arbol, slot)
                .map_or_else(|| "unknown".to_owned(), |k| k.as_str().to_owned());
            norte_frontend::display_name(kind.as_bytes())
        };
        crate::dto::TabView {
            slot_id: id,
            title: clamp_display(titulo),
            title_hostile: hostil,
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
        let actual = SlotId(self.enfocado());
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
            .find(|c| identidad_de_columna(c) == *column)
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
        self.ordenar_por_columna(slot_id, col)
    }

    /// Ordena un listado por una columna ya resuelta.
    ///
    /// Las dos puertas —el click en la cabecera y los `pane.sort-*` del
    /// catálogo— acaban AQUÍ, y por eso ordenan igual: la columna activa
    /// invierte y una nueva empieza ascendente, porque quien lo decide es
    /// `SortSpec::after_click` y no una tabla por superficie.
    fn ordenar_por_columna(
        &mut self,
        slot_id: u32,
        col: norte_frontend::SortColumn,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
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
                    id: identidad_de_columna(id),
                    label: clamp_display(header_label(id, &estilo, catalogo)),
                    sort,
                    sortable: ordena.is_some(),
                }
            })
            .collect()
    }

    /// La proyección de UN listado.
    fn browser(&self, id: u32, hueco: &Hueco) -> BrowserSlotView {
        // Con la MISMA reinterpretación que las filas: pintar la cabecera con
        // los bytes crudos mientras las filas van transcodificadas deja
        // `pane.names-encoding` a medias — el mojibake se queda arriba y el
        // lector no puede saber si el comando hizo algo (#57, #293).
        let (path, hostil) =
            norte_frontend::path_display_with(hueco.pane.dir(), hueco.pane.name_encoding());
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
            hidden_note: match hueco.pane.hidden_count() {
                0 => String::new(),
                n => clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "status-hidden",
                    &[("n", &n.to_string())],
                )),
            },
            skipped_note: hueco.pane.skipped().map_or_else(String::new, |n| {
                clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "listing-skipped",
                    &[("n", &n.to_string())],
                ))
            }),
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
