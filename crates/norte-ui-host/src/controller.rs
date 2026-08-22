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
    "dialog.ai-rename",
];

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
    AprobacionNoEntregada,
    /// Más entradas del listado que se está drenando por detrás.
    MasEntradas(Box<(RequestToken, u32, Vec<Entry>)>),
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
    TaskNueva(Box<(crate::backend::HostTask, Vec<VPath>)>),
    /// Encolarla falló. El usuario tiene que enterarse: pidió un borrado.
    TaskFallida(Box<Error>),
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
    Informe(Box<(u64, Informe)>),
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
    Catalogo(u64, Result<norte_proto::methods::PluginListResult, Error>),
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
                        .send(Mensaje::TaskNueva(Box::new((task, Vec::new()))))
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
            Mensaje::AprobacionNoEntregada => {
                for u in estado.decir("msg-approval-not-delivered") {
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
                if let Some(u) = estado.aterrizar_lote(*datos, &backend, &buzon) {
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
                let (task, afectados) = *task;
                for u in estado.registrar_task(task, afectados, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::TaskFallida(e) => {
                for u in estado.task_fallida(&e) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Progreso(p) => {
                for u in estado.progreso(&p, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Informe(informe) => {
                let (task_id, cual) = *informe;
                for u in estado.informe(task_id, &cual) {
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
    hits: Vec<Entry>,
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
    /// lector estuviera haciendo, y se quedan el teclado. Con esto, la
    /// primera tecla solo dice «ya lo veo» —la misma regla que la revisión de
    /// un plan, y por el mismo motivo—. `Escape` es la excepción: quitarse de
    /// encima algo que uno no ha pedido tiene que salir a la primera.
    ///
    /// `true` en un diálogo que abrió un gesto: ahí la tecla siguiente SÍ es
    /// una respuesta, porque la pregunta la hizo quien está tecleando.
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
    /// Decidir sobre una op de agente. La op real la tiene el daemon ligada
    /// al id: aquí solo viaja el sí o el no.
    Decidir {
        /// El id que el daemon espera de vuelta.
        approval_id: u64,
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

/// Una task viva en el tablero.
struct TaskViva {
    vista: TaskView,
    /// Cómo pedirle que pare. Cancelar dos veces no es un error.
    cancel: std::sync::Arc<dyn Fn() + Send + Sync>,
    /// Su informe ya se pidió. Solo lo llevan los lotes de renombrado, y
    /// evita pedirlo dos veces si el daemon repite el último progreso —una
    /// reconexión reanuncia las tasks, terminales incluidas—.
    informe_pedido: bool,
    /// Los directorios que esta task deja DISTINTOS.
    ///
    /// Se apuntan al encolar y no se deducen del progreso: el progreso dice
    /// qué fichero va por dentro, no qué pantallas mienten cuando termine.
    /// Vacío = nada que refrescar (una búsqueda, una task ajena de la que
    /// solo se conoce el id).
    afectados: Vec<VPath>,
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
    ia_en_vuelo: Option<(u64, VPath)>,
    /// El tablero: lo que está en marcha, por id de task.
    tasks: std::collections::BTreeMap<u64, TaskViva>,
    /// La sesión de UI: qué revisión se leyó, si esta ventana es su dueña, y
    /// si el esquema que hay guardado es de una versión que este host no
    /// entiende (ADR 0059).
    sesion: Sesion,
    status: StatusView,
    conexion: ConnectionView,
    /// Las sesiones de provider que viajan sin cifrar (#44), acotadas por el
    /// módulo compartido.
    degradadas: std::collections::VecDeque<norte_proto::methods::ConnectionDegraded>,
    /// Lo que el daemon dijo de sí mismo antes de irse: relevo o parada.
    /// `None` = no ha dicho nada, o ya volvió.
    aviso_de_daemon: Option<&'static str>,
    /// El daemon RECHAZÓ una mutación por no poder abrir su journal.
    ///
    /// Persistente y no un mensaje: la regla dura 4 dice que sin registro no
    /// se muta, así que esto describe lo que le va a pasar a TODA la sesión,
    /// no a la operación que se acaba de intentar.
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
    fn vacio(dir: VPath) -> Self {
        Self {
            pane: PaneState::new(dir, Vec::new()),
            historial: History::default(),
            primera_visible: 0,
            visibles: 64,
            en_vuelo: None,
            dir_pedido: None,
            marcas_a_restaurar: Vec::new(),
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
            theme,
            user_layouts,
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
                huecos.insert(id, Hueco::vacio(dir.clone()));
            }
        }
        let activo = huecos.keys().copied().next().unwrap_or(1);
        let mut roles = Roles::con_active(SlotId(activo));
        // El DESTINO lo resuelve la capa compartida, y NO se pone a mano.
        // Ponerlo con `Roles::set` lo marcaba como EXPLÍCITO —o sea, «lo
        // eligió una persona»— cuando no lo había elegido nadie, y entonces
        // sobrevivía a que aparecieran más candidatos: con tres listados, el
        // primero se quedaba el rol para siempre y copiar mandaba ahí sin
        // que nadie lo hubiera dicho (ADR 0058 D7).
        roles.reconcile(&arbol, &reparto, &kinds, SlotId(activo));
        let estado = Self {
            instance,
            sequence: 0,
            token: 0,
            locale,
            paleta: None,
            ayuda: None,
            ajustes: None,
            extensiones: None,
            tema: theme,
            mirando_tema: false,
            cursor_procesos: 0,
            sitios: None,
            gen_sitios: 0,
            gen_selector: 0,
            gen_extensiones: 0,
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
            degradadas: std::collections::VecDeque::new(),
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
        while primera.len() < FIRST_PAGE {
            match stream.next().await {
                Some(Ok(e)) => primera.push(e),
                // Un error a mitad de página se cuenta como el error del
                // listado: media página no es un listado.
                Some(Err(e)) => return Err(e),
                None => return Ok((primera, omitidas)),
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
        let Some(hueco) = self.huecos.get_mut(&id) else {
            return;
        };
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
            UiAction::SetViewerRows { rows } => {
                self.visor_filas = Some(usize::try_from(*rows).unwrap_or(1).max(1));
                let cambio = ViewChange::Viewer {
                    viewer: self.vista_visor(),
                };
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
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
            // Un diálogo con campo de texto llega con la tarea que lo traiga
            // (crear directorio, renombrar). Decirlo es más honesto que
            // aceptar texto que nadie va a leer.
            UiAction::DialogInput { id, text } => self.escribir_en_dialogo(*id, text),
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
        // La PRIMERA tecla de un diálogo que se abrió SOLO no lo contesta:
        // solo lo reconoce. `Escape` no lo necesita —descartar es seguro y
        // quien no quiere esto delante tiene que poder quitárselo a la
        // primera—, y un diálogo que abrió un gesto ya nace reconocido.
        if !d.reconocido && k.key != "Escape" && k.key != "esc" {
            if let Some(d) = self.dialogos.last_mut() {
                d.reconocido = true;
            }
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "host-dialog-acknowledge",
            )));
            return (
                self.aplicada(),
                vec![self.parche(vec![ViewChange::Status(self.status.clone())])],
            );
        }
        let elegido = match k.key.as_str() {
            "Enter" | "enter" => d
                .vista
                .choices
                .iter()
                .find(|c| !c.destructive)
                .map(|c| c.id.clone()),
            "Escape" | "esc" => d
                .vista
                .choices
                .iter()
                .find(|c| c.id == "cancel" || c.id == "deny")
                .or_else(|| d.vista.choices.iter().rfind(|c| !c.destructive))
                .map(|c| c.id.clone()),
            _ => None,
        };
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
        self.sondear(slot, backend, buzon);
        self.adornar(slot, backend, buzon);
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
        datos: (RequestToken, u32, Vec<Entry>),
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let (token, slot, batch) = datos;
        let u = self.aplicar_lote(slot, token, batch)?;
        self.sondear(slot, backend, buzon);
        self.adornar(slot, backend, buzon);
        Some(u)
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
        let filas = self.tasks.len();
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
                .send(Mensaje::TaskNueva(Box::new((task, Vec::new()))))
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
            b.hits.push(e.clone());
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
                        is_dir: e.kind == EntryKind::Dir,
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
        let (destino, foco) = if hit.kind == EntryKind::Dir {
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
            problem: actual
                .and_then(|r| r.problem.clone())
                .map_or_else(String::new, |p| {
                    // El diagnóstico del parser puede citar el fichero del
                    // usuario: entra por la misma puerta que el resto.
                    clamp_display(norte_frontend::display_name(p.as_bytes()).0)
                }),
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
        let dir = self.hueco().pane.dir().clone();
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
                hueco.insert(Hueco::vacio(dir.clone()));
            }
        }
        self.roles.clear(RoleId::Active);
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
    fn abrir_paleta(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.paleta = Some(norte_frontend::palette_state::Palette::new(
            self.filas_de_paleta(),
        ));
        let cambio = ViewChange::Palette {
            palette: self.vista_paleta(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
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
            _ => self.abrir_disposiciones(),
        }
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
        self.selector = Some(crate::pickers::Selector::volumenes());
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
        let Some(destino) = s.elegir() else {
            // Sin filas todavía (o la tabla llegó vacía): no hay a dónde ir.
            return (self.aplicada(), Vec::new());
        };
        self.selector = None;
        let cierre = self.parche(vec![ViewChange::Picker { picker: None }]);
        let mut envios = vec![cierre];
        envios.extend(self.navegar(&destino, Trail::Record, backend, buzon));
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
    fn abrir_extensiones(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.extensiones = Some(crate::extensions::Extensiones::abrir());
        self.gen_extensiones += 1;
        let apertura = self.gen_extensiones;
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(PLAZO_PLUGINS, backend.plugin_list()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::Catalogo(apertura, res))))
                .await;
        });
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
            Fondo::Catalogo(apertura, res) => {
                self.aplicar_catalogo_de_extensiones(apertura, res, backend, buzon)
            }
            Fondo::FichaDePlugin(id, res) => self
                .aplicar_ficha(&id, res.as_ref().ok())
                .into_iter()
                .collect(),
            Fondo::Volumenes(apertura, res) => {
                self.aplicar_volumenes(apertura, res).into_iter().collect()
            }
            Fondo::SitiosVolumenes(res) => self.aplicar_sitios(res).into_iter().collect(),
            Fondo::Resultados(epoca, lote) => {
                self.aplicar_resultados(epoca, &lote).into_iter().collect()
            }
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
        res: Result<norte_proto::methods::PluginListResult, Error>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // De ESTA apertura. «Sigue abierta» no es «es la misma».
        if apertura != self.gen_extensiones {
            return Vec::new();
        }
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
            "ArrowDown" | "down" => e.mover(1),
            "ArrowUp" | "up" => e.mover(-1),
            "PageDown" | "pgdn" => e.mover(PAGINA),
            "PageUp" | "pgup" => e.mover(-PAGINA),
            "Home" | "home" => e.mover(i64::MIN / 2),
            "End" | "end" => e.mover(i64::MAX / 2),
            "Enter" | "enter" => return self.pedir_ficha(backend, buzon),
            _ => return (self.aplicada(), Vec::new()),
        }
        let cambio = ViewChange::Extensions {
            extensions: self.vista_extensiones(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
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
            norte_client::ConnEvent::Lost => (ConnectionView::Reconnecting, "msg-daemon-lost"),
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
        };
        // Volver APAGA el aviso: uno que no sabe volverse «ya está» miente en
        // cuanto el daemon reaparece, y el relevo termina volviendo.
        if matches!(ev, norte_client::ConnEvent::Restored) {
            self.aviso_de_daemon = None;
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
        norte_frontend::banners::note_degraded(&mut self.degradadas, d);
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
        let mut banners = Vec::new();
        if self.journal_rehusado {
            banners.push(clamp_display(norte_i18n::t_in(
                self.lang,
                "status-journal-refused",
            )));
        }
        if let Some(clave) = self.aviso_de_daemon {
            banners.push(clamp_display(norte_i18n::t_in(self.lang, clave)));
        }
        if let Some(aviso) = norte_frontend::banners::connection_banner(self.lang, &self.degradadas)
        {
            banners.push(clamp_display(aviso));
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
                Some(Pendiente::Borrar { .. } | Pendiente::Transferir { .. }) => "dialog.confirm",
                Some(Pendiente::Decidir { .. }) => "dialog.approval",
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
                    | Pendiente::Renombrar { .. },
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
            | Efecto::DesmarcarTodo => self.efecto_de_listado(efecto, slot, backend, buzon),
            Efecto::Foco { atras } => self.mover_foco(atras),
            Efecto::Destino => self.designar_destino(),
            // Atendido arriba, antes del panel enfocado. El brazo existe
            // porque el `match` es exhaustivo a propósito: un efecto nuevo
            // sin sitio tiene que ser un error de compilación.
            Efecto::CancelarTask => self.cancelar_por_comando(),
            Efecto::Tamano(_) | Efecto::Igualar | Efecto::Disposiciones => {
                self.efecto_de_disposicion(efecto, backend, buzon)
            }
            Efecto::Columnas => self.abrir_columnas(),
            Efecto::Buscar => self.pedir_busqueda(),
            Efecto::BuscarRapido => {
                // Filtrar es el modo por defecto: es el que no mueve el
                // listado bajo el cursor mientras se teclea.
                self.hueco_mut()
                    .pane
                    .quick_start(norte_frontend::nav::Mode::Filter);
                (self.aplicada(), vec![self.parche_filas()])
            }
            Efecto::CrearDirectorio
            | Efecto::Borrar { .. }
            | Efecto::Transferir { .. }
            | Efecto::Renombrar
            | Efecto::RenameIa
                if self.efectos == crate::commands::Efectos::SoloLectura =>
            {
                Self::no_muta()
            }
            Efecto::Paleta
            | Efecto::Ayuda
            | Efecto::Ajustes
            | Efecto::Extensiones
            | Efecto::Tema
            | Efecto::Volumenes
            | Efecto::Ver => self.efecto_que_abre(efecto, backend, buzon),
            Efecto::CrearDirectorio
            | Efecto::Borrar { .. }
            | Efecto::Transferir { .. }
            | Efecto::Renombrar
            | Efecto::RenameIa => self.efecto_que_muta(efecto),
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
            // Los demás no llegan aquí: el `match` de arriba los reparte.
            _ => Self::no_muta(),
        }
    }

    /// Los efectos que abren una PANTALLA sobre el listado y no tocan nada.
    fn efecto_que_abre(
        &mut self,
        efecto: Efecto,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match efecto {
            Efecto::Paleta => self.abrir_paleta(),
            Efecto::Ayuda => self.abrir_ayuda(backend, buzon),
            Efecto::Ajustes => self.abrir_ajustes(),
            Efecto::Extensiones => self.abrir_extensiones(backend, buzon),
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
            // Los demás no llegan aquí: el `match` de arriba los reparte.
            _ => Self::no_muta(),
        }
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
        self.ia_en_vuelo = Some((epoca, dir.clone()));
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
        let Some((_, dir)) = self.ia_en_vuelo.take_if(|(e, _)| *e == epoca) else {
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
        let Some(parejas) = norte_frontend::rename_pairs(&plan.entries) else {
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
                .detail_lines(r.parejas.len())
                .into_iter()
                .map(|(text, hostile)| crate::dto::DialogLine {
                    text: clamp_display(text),
                    hostile,
                })
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
                Ok(task) => Mensaje::TaskNueva(Box::new((task, afectados))),
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
        // La siembra es lo que la FILA pinta, con el saneado canónico: editar
        // produce el texto que se ve. Para un nombre que no es UTF-8 eso
        // lleva un U+FFFD, y ese residuo es justo lo que el guard de la
        // confirmación no deja escribir.
        let (pintable, hostil) = norte_frontend::display_name(nombre.as_bytes());
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
        let activo = self.activo();
        let destino_id = self
            .roles
            .get(RoleId::Target)
            .map(|SlotId(id)| id)
            .filter(|id| *id != activo && self.huecos.contains_key(id) && !self.oculto(*id));
        if let Some(id) = destino_id {
            return Ok(self.huecos[&id].pane.dir().clone());
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
    fn pedir_transferencia(&mut self, mover: bool) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let activo = self.activo();
        let destino = match self.directorio_destino() {
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
        let hueco = self.hueco();
        let origen_dir = hueco.pane.dir().clone();
        if origen_dir == destino {
            // Los dos listados en el mismo sitio. El daemon lo rechazaría
            // igual, pero abrir un diálogo que promete algo imposible es
            // peor que decirlo antes.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-same-directory".to_owned(),
                },
                Vec::new(),
            );
        }
        // `marked_paths` ya cae al cursor cuando no hay marcas: es la fuente
        // única de «sobre qué opera esto», y duplicar aquí ese respaldo
        // sería un segundo sitio del que se pueden separar.
        let paths: Vec<VPath> = hueco.pane.marked_paths();
        if paths.is_empty() {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-nothing-selected".to_owned(),
                },
                Vec::new(),
            );
        }
        // Una entrada sin último segmento es una RAÍZ, y una raíz no tiene
        // nombre que componer en el destino. Se rechaza el lote entero en vez
        // de saltársela: transferir «casi todo lo que pediste» en silencio es
        // exactamente lo que no puede hacer una mutación.
        if paths.iter().any(|p| p.file_name().is_none()) {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-cannot-transfer-root".to_owned(),
                },
                Vec::new(),
            );
        }
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
                Some(Pendiente::Decidir { approval_id }) if approval_id == req.approval_id
            )
        }) {
            return Vec::new();
        }
        // Estas rutas vienen del daemon como TEXTO ya redactado, no como
        // `VPath`, así que el enmascarado es el de cadenas y la marca se
        // calcula comparando: si enmascarar cambió algo, lo que se lee no es
        // lo que hay, y quien aprueba tiene que verlo.
        let linea = |texto: &str| {
            let enmascarado = norte_encoding::mask_terminal_hazards(texto);
            let hostil = enmascarado != texto;
            crate::dto::DialogLine {
                text: clamp_display(enmascarado),
                hostile: hostil,
            }
        };
        let mut cuerpo: Vec<crate::dto::DialogLine> = Vec::new();
        cuerpo.push(linea(&req.op));
        for p in req.paths.iter().take(Self::MAX_LINEAS_DIALOGO) {
            cuerpo.push(linea(p));
        }
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
        // Cuánto le queda, DICHO. Una decisión con fecha de caducidad que no
        // la enseña se lee como una que espera para siempre, y el humano que
        // vuelve al rato pulsa aprobar sobre algo que el daemon ya denegó.
        // `0` = desconocido (una pendiente reconstruida por el resync no
        // transporta el TTL restante): entonces no se promete un plazo.
        if req.ttl_ms > 0 {
            let segundos = req.ttl_ms.div_ceil(1000);
            cuerpo.push(crate::dto::DialogLine {
                text: clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "modal-approval-ttl",
                    &[("s", &segundos.to_string())],
                )),
                hostile: false,
            });
        }
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-approval-title".to_owned(),
            destination: None,
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
        self.dialogos.push(Dialogo {
            id,
            vista: vista.clone(),
            input_crudo: String::new(),
            // Se abre SOLA: la trae una op de un agente, no una tecla.
            reconocido: false,
            al_confirmar: Some(Pendiente::Decidir {
                approval_id: req.approval_id,
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
        vec![self.parche(vec![cambio])]
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
                Some(Pendiente::Decidir { approval_id: id }) if id == approval_id
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
        salidas.extend(self.decir("msg-approval-expired"));
        salidas
    }

    /// Abre el prompt de crear directorio, con su campo de texto vacío.
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
                let nombre = dialogo.input_crudo.clone();
                // El nombre se valida AQUÍ, con la misma regla que
                // cualquier otro segmento: ni vacío, ni `/`, ni NUL, ni
                // `.`/`..`. Un nombre que no vale no encola nada y lo
                // dice; el texto tecleado no se pierde porque el diálogo
                // se vuelve a abrir con él.
                // El mismo cinturón que el rename: un nombre TOCADO que aún
                // lleva el carácter de sustitución no se escribe. La
                // asimetría de antes («crear no tiene siembra de la que
                // heredar residuos») era falsa del ROUND TRIP: el host pinta
                // su propia proyección enmascarada en el campo, y el renderer
                // vuelve a sembrarlo con ella si tuvo que reconstruir el nodo
                // — un diálogo de aprobación que se cuele por encima basta.
                let seg = match Self::segmento_tecleado(&nombre) {
                    Ok(seg) => seg,
                    Err(clave) => {
                        self.status.message =
                            Some(clamp_display(norte_i18n::t_in(self.lang, clave)));
                        let cambio = ViewChange::Status(self.status.clone());
                        salidas.push(self.parche(vec![cambio]));
                        return (Some(clave), salidas);
                    }
                };
                let destino = dir.join(seg);
                let backend = Arc::clone(backend);
                let buzon = buzon.clone();
                tokio::spawn(async move {
                    match backend.mkdir(destino).await {
                        Ok(task) => {
                            let _ = buzon
                                .send(Mensaje::TaskNueva(Box::new((task, vec![dir]))))
                                .await;
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
                //
                // Y si el sí NO llega, se dice. Un `policy.decide` que falla
                // —el daemon se cayó entre la pregunta y la respuesta— deja
                // la operación denegada por silencio mientras esta ventana da
                // por hecho que la autorizó: «lo dije» y «llegó» no son lo
                // mismo en una superficie de seguridad. Denegar es al revés:
                // si esa no llega, el desenlace es el mismo que se pidió.
                let backend = Arc::clone(backend);
                let buzon = buzon.clone();
                tokio::spawn(async move {
                    if backend.policy_decide(approval_id, true).await.is_err() {
                        let _ = buzon.send(Mensaje::AprobacionNoEntregada).await;
                    }
                });
            }
            None => {}
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
                            .send(Mensaje::TaskNueva(Box::new((task, afectados))))
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
                        // Pedir un plan no escribe en el disco, y aun así
                        // entra: manda el contenido de un directorio a un
                        // modelo, que no es algo que deba hacer una ventana
                        // que se declara de solo lectura.
                        | Pendiente::InstruccionIa { .. }
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
                Ok(task) => Mensaje::TaskNueva(Box::new((task, afectados))),
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
                    Ok(task) => Mensaje::TaskNueva(Box::new((task, afectados.clone()))),
                    Err(e) => Mensaje::TaskFallida(Box::new(e)),
                };
                if buzon.send(mensaje).await.is_err() {
                    // El actor ya no está: lo que quede del lote no le
                    // importa a nadie, y seguir pidiéndolo sí importaría.
                    return;
                }
            }
        });
    }

    /// Mete una Task recién encolada en el tablero y deja su progreso
    /// bombeando hacia el actor.
    fn registrar_task(
        &mut self,
        task: crate::backend::HostTask,
        afectados: Vec<VPath>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let id = task.id.get();
        let ajena = task.foreign;
        if self.tasks.len() >= MAX_TASKS {
            // El tablero está acotado: lo más viejo TERMINADO se cae antes de
            // que la memoria del host dependa de cuántas operaciones lanzó
            // alguien.
            // Se prefiere desalojar una TERMINADA BIEN: una fallida o una
            // cancelada es la única superficie que dice qué no llegó —un
            // fallo no deja entrada de journal—, y en un lote grande con
            // colisiones son justo las que se acumulan.
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
        let mut rx = task.progress.clone();
        let mut vista = Self::vista_de(&rx.borrow());
        vista.foreign = ajena;
        // Un REANUNCIO —el SDK vuelve a ofrecer las tasks al reconectar— trae
        // un progreso que no sabe nada del informe que ya se pidió por esta
        // task. Proyectarlo tal cual borraba del tablero la única señal de
        // que el directorio se quedó a medias, justo cuando la conexión se
        // recupera y el lector vuelve a mirarlo.
        if let Some(anterior) = self.tasks.get(&id)
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
        let informe_pedido = self.tasks.get(&id).is_some_and(|t| t.informe_pedido);
        self.tasks.insert(
            id,
            TaskViva {
                vista,
                cancel: task.cancel,
                afectados,
                informe_pedido,
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
        if self
            .tasks
            .get(&id)
            .is_some_and(|t| Self::terminal(t.vista.state))
        {
            cambios.extend(self.refrescar_afectados(id, backend, buzon));
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
        let mut cambios = vec![ViewChange::Tasks {
            tasks: self.vistas_de_tasks(),
        }];
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
        }
        vec![self.parche(cambios)]
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
        tokio::spawn(async move {
            let cual = if lote {
                Informe::Lote(backend.rename_batch_report(id).await)
            } else {
                Informe::Undo(backend.undo_report(id).await)
            };
            let _ = buzon
                .send(Mensaje::Informe(Box::new((id.get(), cual))))
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

    /// Un informe llegó: al tablero, y delante si dejó algo a medias.
    fn informe(&mut self, task_id: u64, cual: &Informe) -> Vec<BridgeEnvelope<UiUpdate>> {
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
        let Some(viva) = self.tasks.get(&task_id) else {
            return Vec::new();
        };
        let fallo_la_task = matches!(
            viva.vista.state,
            crate::dto::TaskStateView::Failed | crate::dto::TaskStateView::Cancelled
        );
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
        if hay_que_decirlo {
            cambios.push(self.abrir_informe("modal-undo-report-title".to_owned(), cuerpo));
        }
        vec![self.parche(cambios)]
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
        let Some(viva) = self.tasks.get(&task_id) else {
            // El tablero está acotado y la task pudo caerse mientras el
            // informe volaba. No se inventa una fila para colgarlo.
            return Vec::new();
        };
        let fallo_la_task = matches!(
            viva.vista.state,
            crate::dto::TaskStateView::Failed | crate::dto::TaskStateView::Cancelled
        );
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
        if hay_que_decirlo {
            cambios.push(self.abrir_informe("modal-batch-report-title".to_owned(), cuerpo));
        }
        vec![self.parche(cambios)]
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

    /// Abre el diálogo de un informe. Solo informa: no tiene nada que
    /// ejecutar, y su única respuesta lo cierra.
    fn abrir_informe(
        &mut self,
        title_key: String,
        cuerpo: Vec<crate::dto::DialogLine>,
    ) -> ViewChange {
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key,
            destination: None,
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
        self.dialogos.push(Dialogo {
            id,
            vista,
            input_crudo: String::new(),
            // Se abre SOLO, cuando el daemon contesta.
            reconocido: false,
            al_confirmar: None,
        });
        ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        }
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
                let (ack, mut fuera) = self.cancelar(id);
                fuera.extend(self.decir("msg-cancelling"));
                (ack, fuera)
            }
        }
    }

    /// A qué task le toca parar.
    fn task_a_cancelar(&self) -> Objetivo {
        if self.procesos_tienen_el_foco() {
            // La del cursor, sea cual sea su estado: la eligió un humano
            // mirándola. Si ya terminó se DICE, en vez de saltar a otra —
            // cancelar una task que no es la señalada es peor que no
            // cancelar nada.
            let Some((id, viva)) = self
                .tasks
                .iter()
                .nth(self.cursor_procesos.min(self.tasks.len().saturating_sub(1)))
            else {
                return Objetivo::Ninguna;
            };
            return if Self::task_viva(&viva.vista) {
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
            .find(|(_, t)| Self::task_viva(&t.vista))
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
    fn task_viva(v: &TaskView) -> bool {
        matches!(
            v.state,
            crate::dto::TaskStateView::Queued | crate::dto::TaskStateView::Running
        )
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
        crate::dto::DialogLine {
            text: clamp_display(texto),
            hostile: hostil,
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
        let sobran = self.tasks.len().saturating_sub(MAX_TASKS);
        self.tasks
            .values()
            .skip(sobran)
            .map(|t| t.vista.clone())
            .collect()
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

        self.pedir_listado(self.activo(), &destino, token, backend, buzon);

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
                    cursor: (!self.tasks.is_empty())
                        .then(|| self.cursor_procesos.min(self.tasks.len() - 1) as u64),
                }),
                _ => {
                    let nombre =
                        kind.map_or_else(|| "unknown".to_owned(), |k| k.as_str().to_owned());
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
        }
        ViewSnapshot {
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
        let imagen = Self::imagen_de(v);
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
