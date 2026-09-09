//! Estado puro del TUI (panes, cursor, presentación de nombres) y la
//! presentación de ERRORES para la barra (#73): categorías Fluent
//! ([`error_key`]/[`error_category`] y compañía) + saneado de detalle
//! ([`detail_for_bar`]). Máquina testeable sin terminal — el render (`ui`)
//! y el I/O (`main`) viven aparte; los scripts Lua (M4) consumen de aquí la
//! clave ESTABLE de [`error_key`].

use norte_i18n::Lang;
use norte_proto::VPath;

#[cfg(test)]
pub(crate) mod testutil;

mod banners;
mod caps;
mod compare;
mod dialogs;
mod errors;
mod focus;
mod help_view;
mod layout;
mod modal;
mod nav;
mod nav_popup;
mod ops;
mod palette;
mod pane;
mod pickers;
mod plugins;
pub mod profile;
mod prompts;
mod session;
mod trail;

pub use dialogs::*;
pub use errors::*;
pub use help_view::*;
pub use modal::*;
pub use nav_popup::*;
pub use palette::*;
pub use pane::*;
pub use plugins::*;
pub use trail::*;

// Privados en `app` antes del reparto: el glob de arriba solo reexporta
// lo `pub`, asi que estos tres se nombran uno a uno.
use help_view::default_help_chords;

/// El formato que sugiere un nombre vive en el crate COMPARTIDO: el TUI y
/// la ventana ofrecen el mismo diálogo (D14).
pub use norte_frontend::nav::format_by_name;

/// El tamaño con sufijo lo lee el crate COMPARTIDO: el mismo diálogo lo pide
/// en las dos superficies (D14).
pub use norte_frontend::nav::parse_size;

/// El estado del run (`CompareState`) y el panel abierto (`CompareView`)
/// viven en [`norte_frontend::compare`] (#158): la GUI necesita exactamente
/// esta máquina y no una reimplementada, que es como el CLI (fase A) y la
/// tool MCP (fase B) se equivocaron cada uno por su lado — ambos dieron por
/// completa una respuesta a la que le faltaban lotes. Ver
/// [`CompareState::Incomplete`] para la razón de que el cierre del canal no
/// baste.
pub use norte_frontend::compare::{CompareState, CompareView};

/// El estado del run (`SyncRunState`) y el panel abierto (`SyncView`) viven en
/// [`norte_frontend::sync`] (#161, el mismo argumento que ya llevó
/// [`CompareView`] allí): la GUI necesita exactamente este envoltorio del run
/// y no uno reimplementado. C1 aprendió, a costa de una revisión de rama, que
/// mover el TIPO y dejar sus decisiones a mano en cada frontend es peor que no
/// moverlo — así que lo que viaja con él es el mapeo `TaskState` →
/// [`SyncRunState`] ([`SyncRunState::from_task_state`]) y el paquete de
/// actualizaciones de «se aprobó y arrancó `sync.apply`»
/// ([`SyncView::on_apply_started`]), no solo la struct.
pub use norte_frontend::sync::{SyncRunState, SyncView};

// El saneado de nombres ([`display_name`]/[`path_display`]/`must_mask`) y el
// orden del listado ([`sort_entries`] + `nfc_key`/`name_bytes`) viven ahora en
// `norte-frontend` (lógica de presentación PURA compartida con la GUI). Se
// re-exportan aquí para que los call-sites `crate::app::…`/`app::…` (main, ui,
// viewer) sigan resolviendo sin cambios.
pub use norte_frontend::{display_name, path_display, sort_entries};

/// Comando externo que `pane.open` (F4) dejó resuelto y el run loop lanzará
/// (#28). Se separa la resolución del lanzamiento porque el dueño de la
/// terminal es el run loop, no el despacho.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOpen {
    /// Binario a sondear en el `PATH` antes de lanzar nada.
    pub program: String,
    /// argv completo, con el binario en `[0]` y las rutas byte-exactas.
    pub argv: Vec<std::ffi::OsString>,
    /// `true` cuando es el lanzador del escritorio (`xdg-open`/`open`/
    /// `explorer.exe`): entrega el fichero al programa asociado y vuelve
    /// enseguida, así que la TUI **no** se suspende — hacerlo pintaría un
    /// parpadeo de pantalla completa para nada. `false` es un opener
    /// declarado en `ns.toml`, que puede ser `bat` o un editor y necesita la
    /// terminal entera para sí.
    pub detached: bool,
    /// El directorio del pane con el foco, que el hijo recibe como cwd
    /// (#144).
    ///
    /// Los tres comandos de shell (#135) ya lo pasaban y los openers no, así
    /// que un editor abierto sobre un fichero del pane heredaba el cwd de
    /// norte y guardaba donde no se estaba mirando. Se dejó así a propósito
    /// en la ola de shell —cambiarlo cambia comportamiento— y se decidió el
    /// 2026-08-14: pasa el del pane. `None` solo si la ruta no convierte a
    /// nativa, donde no hay nada mejor que heredar.
    pub cwd: Option<std::path::PathBuf>,
}

/// Una SUSPENSIÓN que el despacho resolvió y el run loop ejecutará (#135).
///
/// Mismo reparto que [`PendingOpen`] y por la misma razón: quien es dueño de
/// la terminal es el run loop, no el despacho. Lo que cambia es que aquí no
/// hay sonda PREVIA que decir en la barra — el argv sale de `$SHELL`, de
/// `$EDITOR` o de una línea que el usuario escribió, y que el programa no
/// exista se dice con el error del lanzamiento, no con una sonda aparte que
/// adivinaría lo mismo.
///
/// Lo que sí pasa en el lanzamiento es la RESOLUCIÓN del programa a ruta
/// absoluta (#302): el hijo se lanza con [`Self::cwd`] puesto, y en unix
/// `current_dir` se aplica antes de resolver el programa. Ver
/// [`crate::suspend::run_suspended`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingShell {
    /// argv completo, con el binario en `[0]`. VACÍO es legítimo y significa
    /// «no lances nada»: es `app.toggle-panels`, que solo enseña la terminal
    /// anfitriona.
    pub argv: Vec<std::ffi::OsString>,
    /// Directorio de trabajo del hijo. `None` = el de norte (que es lo que
    /// hacen hoy los openers de #28).
    pub cwd: Option<std::path::PathBuf>,
    /// Esperar a una tecla ANTES de repintar los paneles. Es lo que hace
    /// legible la salida de un comando: sin esto, el listado vuelve encima de
    /// lo que acaba de escribirse.
    pub wait_for_key: bool,
    /// La ruta que tiene que seguir siendo un fichero REGULAR en el instante
    /// del lanzamiento, o no se lanza nada (#303).
    ///
    /// La pone `pane.edit-new` y solo él: es el gesto en el que norte ANUNCIA
    /// un nombre creándolo y después se lo entrega a otro programa. Viaja en la
    /// suspensión —y no se comprueba donde se resuelve el gesto— porque entre
    /// una cosa y otra corre el re-listado de los dos paneles: la comprobación
    /// vale lo que vale el hueco que deja detrás, y aquí el hueco es el `exec`.
    ///
    /// `None` = nada que comprobar, que es lo que llevan el shell, la línea de
    /// comandos y `app.toggle-panels`.
    pub check_regular: Option<norte_proto::VPath>,
}

/// The key of the capability cache: the DIRECTORY, in wire form.
///
/// It used to be `(scheme, authority)` — one backend — and that stopped being
/// the right question when ADR 0054 made the daemon answer per LOCATION
/// (#215). Under one `file://` there are mounts: an exFAT stick that folds
/// case, an ext4 subtree in `+F`, a read-only bind. An answer cached for
/// `/home` was served for every one of them.
///
/// Owned because the map owns its keys, and the lookups are per help open and
/// per cd — not per frame.
type CapsKey = String;

/// The location `at` belongs to, as a cache key: the directory itself.
fn caps_key(at: &VPath) -> CapsKey {
    at.to_wire()
}

/// How many locations the capability cache keeps.
///
/// It was unbounded when the key was one per backend — there are seven schemes
/// — and a key per DIRECTORY is not: a session that walks a big tree would
/// grow it without end. Sixty-four is far more than the directories a reader
/// keeps coming back to, and the eviction is by insertion order, which for a
/// cache whose entries cost one round trip each is the honest cheap answer:
/// the oldest location is the one least likely to be the next cd.
const CAPS_CACHE_MAX: usize = 64;

/// Lo que el run loop tiene que preguntarle al DESTINO antes de que el humano
/// diga que sí: si cabe (#149) y si sabe sujetar sus escrituras (#164).
///
/// Van juntas porque son la misma pregunta hecha al mismo sitio en el mismo
/// momento, y separarlas costaría dos rondas de I/O por diálogo para pintar dos
/// líneas contiguas.
#[derive(Debug, Clone)]
pub struct DestCheck {
    /// El directorio DESTINO, que es de quien se pregunta todo esto.
    pub to: VPath,
    /// Bytes que la transferencia va a escribir, si se saben.
    ///
    /// `None` = alguno de los ítems no dice cuánto ocupa (un directorio no lo
    /// trae en el listado), y entonces NO hay pregunta de espacio: sumar solo
    /// lo conocido avisaría con un número menor que el real (lo calcula
    /// `App::transfer_total`, privado). La de confinamiento se hace igual — no
    /// depende del tamaño, y es justo el caso recursivo el que más la necesita.
    pub total: Option<u64>,
}

/// Lo que la barra dice del journal de ESTA sesión.
///
/// Un enum y no un `Option<NoJournal>` más un bool: son estados excluyentes de
/// una misma cosa —qué frase toca— y dos campos podrían contradecirse.
#[derive(Debug, Clone)]
enum JournalIndicator {
    /// No se está registrando, por este motivo (#177/#178).
    NotRecorded(norte_core::embedded::NoJournal),
    /// Y además lleva minutos así sin daemon que lo explique (#203).
    Squatted,
}

/// Quién se queda el teclado del cuerpo de la pantalla.
///
/// NO es el foco. [`App::focus`] sigue apuntando al LISTADO en el que estabas,
/// y toda operación —una copia, un borrado, un `cd`— sigue yendo ahí: lo que
/// esto decide es solo a quién se le entregan las teclas mientras un panel
/// auxiliar está delante, igual que hacen la ayuda o la palette.
///
/// Existe porque `App::focus` es un índice sobre los listados VISIBLES, así
/// que un sidebar no puede tenerlo sin el refactor a `SlotId` que P6 aplazó.
/// El día que ese refactor llegue, esto se pliega dentro de él.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyOwner {
    /// Los listados, que es lo de siempre.
    #[default]
    Panes,
    /// El sidebar de sitios.
    Places,
    /// El visor acoplado.
    Preview,
    /// El panel de procesos.
    Processes,
    /// El árbol de directorios (#136).
    Tree,
    /// El panel de registro (#323).
    Log,
}

/// Lo que hace un click sobre una fila del sidebar de sitios (#226).
///
/// Lo que el modelo podía hacer ya está hecho al volver; esto es lo que
/// necesita al backend, que [`App`] no tiene.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacesClick {
    /// Se movió el cursor y el teclado se vino al sidebar. Nada más que hacer.
    Focused,
    /// Se plegó o desplegó una sección: desplegar las unidades es el momento
    /// de volver a pedirlas, igual que por teclado.
    Folded,
    /// Hay que llevar el listado a donde diga [`App::places_activate`].
    Activate,
}

/// El editor que la configuración nombra (`[ui] editor`).
///
/// La plantilla TAL CUAL, con sus códigos de campo sin expandir: expandirlos
/// necesita el fichero y el directorio, que solo se saben en el momento del
/// gesto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorSpec {
    /// El argv plantilla (`["zed", "%f"]`). El primer token es el binario.
    pub command: Vec<String>,
    /// Abre ventana propia: no se suspende la terminal esperándolo.
    pub detached: bool,
}

/// Dónde cayó un click dentro de una fila del árbol (#136).
///
/// La MARCA y el resto de la fila no hacen lo mismo, y el nombre lo dice: un
/// `bool` en la llamada se lee «true» en el sitio donde importa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeSpot {
    /// Sobre el `▾`/`▸`/`·`: pliega o despliega esa rama.
    Mark,
    /// En cualquier otra celda de la fila.
    Row,
}

/// Lo que hace un click sobre una fila del árbol (#136).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeClick {
    /// Se movió el cursor (o se plegó la rama) y el teclado se vino al árbol.
    /// Nada más que hacer.
    Focused,
    /// Hay que llevar el listado a donde diga [`App::tree_activate`].
    Activate,
}

/// Lo que este proceso sabe de la sesión guardada (L2).
///
/// Junto y no cinco campos sueltos en [`App`]: son una sola cosa —la pantalla
/// que se guarda— y los tres privados solo tienen sentido entre ellos.
#[derive(Debug, Default)]
pub struct SessionUi {
    /// Esta ventana NO es la dueña: otra la tiene, así que ésta arranca con la
    /// misma pantalla y a partir de ahí va por su cuenta sin escribir nada. Se
    /// dice al abrir con un mensaje y, mientras dure, con una marca permanente
    /// en la barra de estado ([`App::session_banner`], #232).
    ///
    /// También se pone suelta la ventana que encuentra un cuerpo de una
    /// versión más nueva: no se lee, y sobre todo no se pisa.
    pub detached: bool,
    /// La revisión que este proceso tiene por vigente, SOLO para arrancar el
    /// escritor de la sesión.
    ///
    /// A partir de ahí la de verdad la lleva el escritor, que es quien ve las
    /// respuestas del core; ésta solo se refresca cuando avisa de un relevo. No
    /// se compara con nada: leerla para decidir algo sería leer un número
    /// viejo.
    pub revision: u64,
    /// Estado por hueco que vino en la sesión y que este layout NO tiene.
    ///
    /// Se conserva y se vuelve a escribir tal cual: cambiar de disposición no
    /// puede costarte el historial de un panel al que vas a volver. Lo recorta
    /// [`norte_frontend::session::SessionBody::prune`], que es quien sabe
    /// cuántos huérfanos caben.
    orphans: std::collections::BTreeMap<u32, norte_frontend::session::SlotState>,
    /// Las disposiciones de los OTROS perfiles, tal y como vinieron.
    ///
    /// Mismo trato que los huérfanos y por el mismo motivo: esta sesión es la
    /// pantalla de VARIOS perfiles y este proceso solo mira uno, así que lo de
    /// los demás viaja de vuelta intacto. Escribir solo el activo borraría del
    /// documento el sitio donde los otros habían dejado sus paneles (ADR
    /// 0079, D5).
    other_layouts: std::collections::BTreeMap<String, norte_frontend::layout::Node>,
    /// Cuándo se tocó cada hueco por última vez (epoch ms), para la barrida
    /// por edad. Se guarda en vez de sellarse al capturar porque capturar no
    /// es tocar: dos capturas seguidas de la misma pantalla tienen que dar el
    /// mismo documento.
    touched: std::collections::HashMap<u32, u64>,
    /// El cursor que traía la sesión, hasta que llegue el listado que lo puede
    /// colocar: sobre un pane vacío, poner el cursor en la fila 12 es ponerlo
    /// en la 0.
    cursors: std::collections::HashMap<u32, u64>,
    /// De qué huecos SABÍA la sesión guardada, tal y como se leyó del disco.
    ///
    /// La necesita `[profile.start]`, que solo siembra el hueco del que la
    /// sesión no sabe nada (ADR 0098). Y tiene que ser lo LEÍDO y no
    /// `App::session_body()`, que es la pantalla de AHORA: aquélla nombra todos
    /// los huecos vivos, así que preguntándole el perfil no sembraba nunca.
    read: std::collections::BTreeSet<u32>,
    /// Los huecos que este proceso ya sembró desde `[profile.start]`.
    ///
    /// Sembrar es de la PRIMERA vez, y sin esta cuenta un lector sin sesión
    /// guardada —una instalación nueva— volvía al directorio de arranque del
    /// perfil cada vez que entraba y salía de él, que es la decisión 2 de la
    /// ADR 0098 puesta del revés.
    seeded: std::collections::BTreeSet<u32>,
}

/// Filas que salta `cursor.page-up/down` (fijo hasta que el alto real del
/// pane viaje con el comando).
///
/// Vive aquí y no en el binario porque lo pagina TODO lo que tiene lista: los
/// panes, la ayuda, los ajustes y el editor de atajos — y ese último salió del
/// binario antes que el resto, que es cuando una constante compartida deja de
/// poder vivir en el que se va.
pub const PAGE: usize = 10;

/// Estado completo del TUI: los paneles y el foco.
// `struct_excessive_bools`: el lint busca APIs cuyos parámetros booleanos se
// confunden entre sí en la llamada. Esto no es una API: es el estado completo
// de la TUI, y sus banderas son independientes entre sí, se leen por nombre y
// jamás viajan juntas como argumentos. Agruparlas en sub-structs por contar
// bools escondería qué mira cada pintor a cambio de nada.
#[expect(clippy::struct_excessive_bools, reason = "estado del TUI, no una API")]
pub struct App {
    /// Los dos paneles (izquierda, derecha), guardados por hueco.
    pub panes: crate::panel::PaneSlots,
    /// El árbol de huecos vigente. En L1a es siempre `orthodox`.
    pub layout: norte_frontend::layout::Node,
    /// Los kinds que este binario sabe pintar.
    pub kinds: norte_frontend::layout::KindRegistry,
    /// Los roles, reconciliados tras cada reparto.
    pub roles: norte_frontend::layout::Roles,
    /// Quién tiene el teclado del cuerpo (L3). Ver [`KeyOwner`].
    key_owner: KeyOwner,
    /// La barra de menús, si está abierta. Overlay: se queda TODAS las teclas
    /// mientras está, como el resto.
    pub menu: Option<norte_frontend::menu::MenuState>,
    /// Por qué menú se abrió la última vez.
    ///
    /// Se reabre por ahí ([`norte_frontend::menu::MenuState::reopen_at`]):
    /// empezar siempre por el primero obliga a recorrer la barra entera cada
    /// vez, y quien usa dos entradas del mismo menú lo paga en cada gesto.
    pub menu_ultimo: usize,
    /// El siguiente `SlotId` a acuñar. Nunca decrece y nunca se reutiliza:
    /// un id reciclado haría que el estado huérfano de un hueco cerrado
    /// resucitara dentro de otro que no tiene nada que ver.
    next_slot: u32,
    /// Config de columnas resuelta (#108 bloque 4): set por scheme + sort.
    /// La siembra el arranque desde `[ui.columns]`; el render y los hooks
    /// de cd la consultan.
    pub columns: norte_frontend::columns::ColumnsSettings,
    /// `now` para las celdas de tiempo RELATIVO (#108 L5): `None` = reloj
    /// real; los tests de snapshot fijan `Some(ms)` para render estable.
    pub render_now_ms: Option<i64>,
    /// Catálogo de attrs por SCHEME (#117): una llamada a
    /// `fs.capabilities` por scheme nuevo y por sesión; alimenta hints y
    /// cabeceras del render y las filas del picker (tarea 4). Privado:
    /// lectura por [`Self::attr_catalog`], escritura por
    /// [`Self::insert_attr_catalog`].
    attr_catalogs: std::collections::HashMap<String, norte_proto::AttrCatalog>,
    /// Capability flags per LOCATION, the other half of the same response.
    ///
    /// `fs.capabilities` answers with `capabilities` AND `attrs` in one
    /// message, and this crate was keeping only the attrs — so the honest
    /// answer to "does this location refuse writes" was already in the
    /// process, thrown away, and asking for it again meant a second round trip
    /// over a link that had just carried it. Filled from the SAME call as
    /// [`Self::attr_catalogs`] (`main::first_page`), so caching it costs
    /// nothing.
    ///
    /// Keyed by the DIRECTORY (`caps_key`), which the attr catalogue beside it
    /// is not, and the asymmetry is the point.
    ///
    /// It was keyed by `(scheme, authority)` — one backend — and that was the
    /// right shape until ADR 0054 made the daemon answer per LOCATION (#215).
    /// Under one `file://` there are mounts: an exFAT stick that folds case,
    /// an ext4 subtree in `+F`, a read-only bind. The answer cached for
    /// `/home` was served for all of them, and the only reason nothing had
    /// broken yet is that `pane_read_only` was the sole reader and no built-in
    /// provider varies `READ_ONLY` below its scheme. [`Self::caps`] is a
    /// general accessor to every flag, and the next flag read this way must
    /// not be the one that finds out.
    ///
    /// Bounded by [`CAPS_CACHE_MAX`]: a key per backend was bounded by the
    /// seven schemes that exist; a key per directory is not.
    ///
    /// Private: read through [`Self::caps`], written through
    /// [`Self::insert_caps`].
    caps: std::collections::HashMap<CapsKey, norte_proto::Capabilities>,
    /// Orden de llegada de las claves de [`Self::caps`], para desalojar la más
    /// vieja cuando se llena ([`CAPS_CACHE_MAX`]).
    caps_order: std::collections::VecDeque<CapsKey>,
    /// Índice del pane con foco (invariante 0|1: privado, ver [`Self::focus`]).
    focus: usize,
    /// `true` cuando el usuario pidió salir.
    pub quit: bool,
    /// Secuencia de teclas pendiente, ya formateada (status bar). Se escribe
    /// SOLO por [`App::show_pending`]/[`App::clear_pending`], que la mantienen
    /// de acuerdo con [`App::which_key`].
    pub pending: String,
    /// The which-key panel, open exactly while a chord sequence is pending
    /// (K3a). `None` = closed.
    ///
    /// It takes NO keys of its own — the pane (or viewer) resolver keeps the
    /// keyboard while it is up, which is the whole point: the reader carries
    /// on typing the sequence and watches the panel narrow. It is the one
    /// overlay of this crate for which that is true, so `keyboard_owner`
    /// counts it only so a dispatch that opens or closes it is noticed, never
    /// to route a key to it.
    ///
    /// Written ONLY by [`App::show_pending`]/[`App::clear_pending`], next to
    /// [`App::pending`]: the panel and the status-bar segment describe the
    /// same resolver state, and two fields that can be updated separately are
    /// two fields that will eventually disagree — a panel left open over a
    /// keymap that was hot-reloaded under it would teach keys nobody has.
    pub which_key: Option<norte_frontend::whichkey::WhichKeyRows>,
    /// Diálogo modal activo (bloquea el keymap hasta resolverse).
    pub modal: Option<Modal>,
    /// Último mensaje para la barra (error por categoría o resultado).
    pub message: Option<String>,
    /// Todo lo que este proceso sabe de la sesión guardada (L2).
    pub session: SessionUi,
    /// Panel de tasks vivo.
    pub board: crate::tasks::TaskBoard,
    /// Viewer abierto (F3); None = navegando.
    pub viewer: Option<crate::viewer::Viewer>,
    /// Help overlay open (F1, H3b): the navigable view over the `norte-help`
    /// corpus — sidebar, body, filter and history — plus the generated
    /// keyboard page, which is still built from the EFFECTIVE keymap (preset
    /// and the user's layers included, never a hand-kept list).
    pub help: Option<HelpView>,
    /// Colisiones a la espera de diálogo: JAMÁS se pisa un modal abierto
    /// (una tecla en vuelo respondería a la pregunta equivocada); se
    /// atienden en orden al cerrarse el modal actual.
    pub pending_collisions: std::collections::VecDeque<crate::tasks::RetrySpec>,
    /// Aprobaciones de policy a la espera de diálogo (M3-3b T5): misma
    /// disciplina que las colisiones (jamás pisar un modal abierto), pero con
    /// PRIORIDAD sobre ellas — una aprobación tiene TTL en el daemon y una
    /// colisión espera lo que haga falta.
    pub pending_approvals: std::collections::VecDeque<norte_proto::methods::PolicyApprovalRequired>,
    /// Tema resuelto + profundidad de color (ADR 0020). El render lee de aquí;
    /// el hot-reload lo reemplaza. Default = preset `default`.
    pub theme: crate::theme::TuiTheme,
    /// Selector de tema abierto (popup): None = cerrado.
    pub theme_picker: Option<ThemePicker>,
    /// Selector de disposición abierto (F9 → `layout.pick`): None = cerrado.
    /// El modelo vive en norte-frontend (regla 7); aquí solo se guarda.
    pub layout_picker: Option<norte_frontend::layout_picker::LayoutPicker>,
    /// Selector de PERFIL abierto (`profile.pick`): None = cerrado. Mismo
    /// patrón que el de disposición, y por el mismo motivo: el modelo vive en
    /// norte-frontend (regla 7) y aquí solo se guarda.
    pub profile_picker: Option<norte_frontend::profile_picker::ProfilePicker>,
    /// Si la barra de menú está FIJADA en la fila de arriba (`[ui] menu_bar`).
    ///
    /// Fijada le quita una fila al cuerpo, y esa resta se hace en el reparto
    /// del frame —el único sitio por el que pasan el pintado, el mapeo de
    /// clics y las decisiones de «qué hueco se colocó» del bucle—, así que
    /// las tres cosas cuadran solas.
    ///
    /// Suelta, el menú sigue abriéndose con su tecla y pintándose ENCIMA de
    /// la primera fila, como siempre.
    pub menu_bar: bool,
    /// El comando que dejó pedido un clic en la barra de paneles (#324).
    ///
    /// Se despacha por el MISMO camino que su atajo, y no por uno propio: dos
    /// caminos para abrir el mismo panel divergen en cuanto uno de los dos
    /// crece un detalle.
    pub pending_panel_command: Option<String>,
    /// La barra de paneles está fijada (`[ui] panel_bar`, #324).
    ///
    /// Los paneles laterales se abrían por atajo, por el menú o por la paleta,
    /// y los tres exigen SABER que el panel existe: no había ninguna superficie
    /// que los enseñara. Una fila permanente cuesta una celda de alto, así que
    /// se elige — pero por defecto va puesta, porque el que no sabe que el
    /// panel existe tampoco sabe que existe la opción de enseñarlo.
    pub panel_bar: bool,
    /// La fila `..` está encendida (`[ui] parent_entry`).
    ///
    /// Se guarda aquí además de en cada pane porque un pane NUEVO —una
    /// pestaña, un hueco de una disposición— tiene que nacer con la misma
    /// respuesta que los demás.
    pub parent_row: bool,
    /// El perfil activo, o `None` si no hay ninguno.
    ///
    /// Espejo en memoria de `SessionBody.active`. Se guarda como `OsString`
    /// —no como el `String` del cuerpo— porque es lo que se le pasa al
    /// resolutor de capas, y ahí es un nombre de directorio (D4).
    pub active_profile: Option<std::ffi::OsString>,
    /// Un cambio de perfil pedido y todavía sin hacer.
    ///
    /// Lo pone `dispatch` y lo drena el run loop, como el resto de lo que un
    /// comando pide y no puede ejecutar él mismo: cambiar de perfil recarga
    /// capas y relista paneles, que es I/O, y `dispatch` ya devuelve un
    /// [`crate::navigate::Cd`] — meter un segundo canal de salida en su firma
    /// tocaría a todos sus llamantes para servir a tres brazos.
    pub pending_profile: Option<std::ffi::OsString>,
    /// El sidebar necesita que le vuelvan a pedir las unidades.
    ///
    /// Mismo patrón que [`Self::pending_profile`] y por lo mismo: `host.volumes`
    /// es I/O y `App` no tiene backend. Lo enciende TODO lo que hace aparecer
    /// la sección —abrir el sidebar, desplegarla, montar una disposición que
    /// ya lo trae— y lo drena el run loop una vez por vuelta.
    ///
    /// Antes cada sitio pedía los volúmenes por su cuenta, y por eso faltaban
    /// justo en los que nadie recordó: arrancar con `full`, cambiar de perfil,
    /// abrir el sidebar desde dentro de otro panel. Una bandera y un drenaje
    /// es un sitio donde equivocarse en vez de cinco.
    pub places_wants_drives: bool,
    /// El selector de conexiones (#140), si está abierto.
    pub connections_picker: Option<norte_frontend::connections_picker::ConnectionsPicker>,
    /// El estado del panel de registro: qué nivel se enseña y qué se filtra.
    pub log_panel: norte_frontend::logpanel::LogPanel,
    /// El filtro de texto del registro MIENTRAS se teclea.
    ///
    /// Aparte del filtro ya aplicado (`log_panel.filter()`) porque son dos
    /// cosas: lo que se está escribiendo y lo que está filtrando. Sin la
    /// separación, cada letra re-filtraría la lista y el lector vería la
    /// pantalla saltar bajo el cursor mientras escribe.
    pub log_filter_input: Option<String>,
    /// El anillo del que lee ese panel.
    ///
    /// `Option` porque el subscriber lo instala `main`, y los tests construyen
    /// `App` sin él: un panel sin anillo se pinta vacío diciendo que no hay
    /// registro instalado, que es la verdad, y no se cae.
    pub log_ring: Option<norte_config::logring::LogRing>,
    /// La mitad REMOTA de ese panel: lo que el daemon lleva contado (#328).
    ///
    /// Con `--socket` el anillo de arriba solo tiene las líneas de esta
    /// terminal, y los providers, el journal, la política y el motivo por el
    /// que una conexión falló están en el otro proceso. La petición en vuelo
    /// no vive aquí sino en `InFlight`, que es quien habla con el backend.
    pub log_remote: crate::logview::RegistroRemoto,
    /// Lo que el lector está esperando ahora mismo, si algo (#323).
    ///
    /// Lo pone y lo quita quien espera, y solo dura la espera: un `Busy` que
    /// sobrevive a su trabajo es exactamente el spinner que no avanza nunca.
    /// No se pinta hasta cruzar el umbral de [`norte_frontend::busy`], así que
    /// una navegación local —la inmensa mayoría— no llega a enseñar nada.
    pub busy: Option<norte_frontend::busy::Busy>,
    /// Overlay del picker de columnas (#108 7a): mismo patrón que
    /// `theme_picker` — un Option en App, NO una variante de Modal (Modal es
    /// confirmación; esto es lista con cursor). El modelo vive en
    /// norte-frontend (`ColumnsPicker`, regla 7).
    pub columns_picker: Option<norte_frontend::columns_picker::ColumnsPicker>,
    /// Gestor de extensiones abierto (overlay del catálogo, M4-P3): None =
    /// cerrado.
    pub extensions: Option<ExtensionManager>,
    /// TOFU Lua pendiente (M4, [`Modal::TrustLuaInit`]): path CANÓNICO del
    /// `init.lua` de proyecto + los BYTES leídos una sola vez. Al resolver
    /// el modal se registra la decisión y, si se aprueba, se evalúan ESTOS
    /// bytes — jamás se relee el disco entre el check y el eval
    /// (anti-TOCTOU).
    pub lua_pending_trust: Option<(std::path::PathBuf, Vec<u8>)>,
    /// Salida del hook `norte.ui.statusbar` del `init.lua` activo (M4 Lua),
    /// YA saneada por el host (`detail_for_bar`). `Some` sustituye la línea
    /// default de la barra del pane con foco; `None` = barra normal.
    pub lua_status: Option<String>,
    /// #44: remote sessions degraded to plaintext, BY SCHEME.
    ///
    /// Was a single pre-formatted `Option<String>`: `main` formatted the scheme
    /// and the host into a sentence and dropped the structured
    /// `ConnectionDegraded`, so "which connection degraded" had no answer, a
    /// second degradation silently overwrote the first, and the help had no
    /// fact to read. The value is kept whole and the banner
    /// ([`Self::connection_banner`]) is built from it on demand.
    ///
    /// One entry per scheme, so `sftp` and `ftp` coexist. Two HOSTS on one
    /// scheme still collapse into one entry, and the banner does not claim
    /// otherwise.
    ///
    /// A `VecDeque` in arrival order rather than a map, for two reasons that
    /// the map could not give: it is CAPPED at `DEGRADED_MAX` (a map keyed on
    /// a wire-supplied string grows as far as the sender wants), and the newest
    /// report is `back()`, which is the one the banner names when there are
    /// several. Lookup is a scan of at most 32 short strings, on the path that
    /// assembles the help's facts — not a per-frame one.
    ///
    /// NEVER CLEARED, deliberately. A degradation is not known to be resolved
    /// without a successful reconnect that reports the session encrypted, and
    /// the wire has no such notification: `connection.degraded` is only ever
    /// sent, never withdrawn. Any clearing rule this side could invent — a
    /// timeout, the next successful listing, leaving the pane — would say "the
    /// session is encrypted again" on evidence that does not support it, which
    /// is the one wrong answer for a security indicator. Follow-up work is a
    /// wire notification for the recovered case, not a heuristic here.
    ///
    /// Private: read through [`Self::degraded_for`] /
    /// [`Self::connection_banner`], written through [`Self::note_degraded`].
    degraded: norte_frontend::banners::DegradedSet,
    /// #177: esta sesión NO está registrando sus mutaciones en el journal.
    ///
    /// El brazo embebido abre el journal del directorio de estado en su primera
    /// mutación, y si lo tiene otro proceso (un daemon vivo, otra sesión que ya
    /// mutó) esta sigue adelante SIN registro: nada de lo que se copie, mueva o
    /// borre a partir de ahí se podrá deshacer ni auditar.
    ///
    /// Persistente, y por el mismo motivo que `degraded`: llega UNA vez, en
    /// mitad de una operación que el usuario acaba de lanzar con el teclado, y
    /// `app.message` lo borra la siguiente tecla — hay 136 sitios que escriben
    /// ese campo. Un aviso que dura hasta el siguiente `↓` no es un indicador
    /// de seguridad.
    ///
    /// No se limpia nunca: la decisión de esta sesión se toma una vez y no se
    /// revisa (ver `norte_core::embedded`), así que mientras la sesión viva la
    /// frase sigue siendo cierta. Si algún día se reintenta la apertura, esto
    /// necesita el evento de recuperación ANTES que el reintento.
    ///
    /// Se retiene el valor ESTRUCTURADO y no un `bool`, por el mismo criterio
    /// que `degraded` (H3d): la frase se compone al pintarla, y el motivo sigue
    /// disponible para quien lo necesite (una página de ayuda, un futuro
    /// detalle en la barra).
    ///
    /// Privado: se lee por [`Self::journal_banner`] y se escribe por
    /// [`Self::note_no_journal`].
    no_journal: Option<JournalIndicator>,
    /// Historial de directorios por pane (spec 2026-07-18, `Alt+↓`): mismo
    /// índice que `panes`. Vive en `App` y no en `Pane` (el historial no es
    /// estado de render): cada cd EXITOSO empuja el dir anterior (main.rs).
    pub history: crate::panel::Histories,
    /// Copia de la hotlist de `LoadedConfig` (clonada en arranque y en cada
    /// hot-reload OK): la fuente para el popup `Ctrl+D`. Los adds/removes
    /// SOLO la tocan tras persistir con éxito (consistencia con disco).
    pub hotlist: Vec<crate::config::HotlistItem>,
    /// Popup de navegación abierto (historial/hotlist): None = cerrado.
    pub nav_popup: Option<NavPopup>,
    /// Diálogo de búsqueda viva abierto (`Alt+F7`, liveSearch T6): None =
    /// cerrado. Captura imprimibles como el `name_input` del popup de nav.
    pub search_dialog: Option<SearchDialog>,
    /// Panel de diferencias abierto (`Shift+F2`,
    /// 2026-08-11-directory-comparison.md): `None` = cerrado.
    ///
    /// Un overlay (`Option` en `App`) y NO un modo del pane, a diferencia de
    /// la búsqueda viva: una fila de comparación tiene DOS caras y un
    /// veredicto entre ellas, así que no cabe en la columna de un pane ni es
    /// una `Entry` que `extend_listing` pueda tragar. Ocupa el sitio de los
    /// dos panes mientras está abierto, que es lo que un diff es.
    pub compare: Option<CompareView>,
    /// Tamaños hidratados bajo demanda para el panel de diferencias, por
    /// `VPath` (#157).
    ///
    /// Solo la fila SELECCIONADA se sondea, nunca una ventana: a diferencia
    /// del pane normal (radio de filas, #52), `list_offset` ya mete la fila
    /// seleccionada dentro del área pintada en cuanto el panel de
    /// diferencias es lo que se está pintando, así que "en pantalla" es casi
    /// una tautología aquí — sondear solo esa fila cubre exactamente el caso
    /// que el issue señala: un huérfano `OnlyLeft`/`OnlyRight` sin tamaño es
    /// la fila que más lo pide, porque es la que decide si se copia.
    ///
    /// Vive en la TUI y no en `ComparePane` (`norte-frontend`) A PROPÓSITO:
    /// es una caché de PRESENTACIÓN, nunca viaja por el wire y ningún otro
    /// frontend la necesita, y `ComparePane` no tiene hoy ninguna vía de
    /// mutar una fila ya llegada — sus filas no cambian nunca tras `extend`
    /// (ver su rustdoc: "Rows only ever grow"). Guardarlo aquí y pintarlo
    /// como una superposición en `ui::draw_compare` evita necesitar esa vía.
    pub compare_size_hints: std::collections::HashMap<VPath, u64>,
    /// Paths YA sondeados para [`Self::compare_size_hints`], acierto o
    /// fallo, para no reintentar un stat que falló en cada frame — mismo
    /// criterio que `last_probed` para el pane normal. Se vacía cuando
    /// `launch_compare` abre una comparación nueva, nunca durante una: las
    /// filas de una comparación en curso no cambian bajo los pies (ver la
    /// nota de [`Self::compare_size_hints`]).
    pub compare_size_probed: std::collections::HashSet<VPath>,
    /// Qué comparación es la de esas dos ([`Self::begin_compare_generation`],
    /// #198). La sonda vive en el run loop y `launch_compare` no la recibe,
    /// así que un resultado en vuelo cuando empieza otra comparación llegaría
    /// a las tablas recién vaciadas de la SIGUIENTE. Lo que impide eso es que
    /// el resultado traiga la generación con la que se pidió.
    compare_generation: u64,
    /// Lo que hay que preguntarle al destino y el run loop aún no ha
    /// preguntado (#149, #164): `open_transfer` sabe QUÉ se va a mover, y
    /// preguntar por los volúmenes y las capacidades es I/O, que es del run
    /// loop. Mismo reparto que `pending_compare`.
    pub pending_dest_check: Option<DestCheck>,
    /// Params de `fs.compare` que el despacho resolvió y el run loop aún no
    /// ha lanzado (`Shift+F2`). Mismo reparto que [`Self::pending_open`] y
    /// [`Self::pending_shell`]: `dispatch` decide QUÉ, el run loop —dueño del
    /// canal y de la Task— lo hace.
    pub pending_compare: Option<norte_proto::methods::FsCompareParams>,
    /// Lote de sumas que el despacho resolvió y el run loop aún no ha lanzado
    /// (#311). Mismo reparto que [`Self::pending_compare`]: leer el fichero de
    /// sumas y esperar el informe es I/O, y eso es del run loop.
    pub pending_checksum: Option<ChecksumRequest>,
    /// Panel de sincronización abierto (`Ctrl+Y`, o `s`/`m` dentro del panel
    /// de diferencias): `None` = cerrado. Se pinta POR ENCIMA del de
    /// diferencias, que sigue vivo detrás con sus marcas.
    pub sync: Option<SyncView>,
    /// Params de `sync.plan` resueltos y aún sin lanzar. Mismo reparto que
    /// [`Self::pending_compare`].
    pub pending_sync: Option<norte_proto::methods::SyncPlanParams>,
    /// `plan_hash` que el lector aprobó y el run loop aún no ha aplicado.
    ///
    /// Es lo ÚNICO que viaja: `sync.apply` no lleva rutas ni modo, así que no
    /// hay forma de que se ejecute algo distinto de lo que se enseñó (ADR
    /// 0049). Un `Box` porque es el mayor de los `pending_*` con diferencia y
    /// clippy mide el `App` entero.
    pub pending_sync_apply: Option<Box<norte_proto::methods::PlanHash>>,
    /// A dónde llevar el panel que acaba de desconectar (#140).
    ///
    /// La RUTA y no una bandera, por dos razones: el bucle no tiene que
    /// adivinar a dónde —lo decide quien desconectó— y `App` no engorda su
    /// cuenta de `bool`s, que es un lint de este repo y una señal de que el
    /// estado se estaba volviendo una bolsa de banderitas.
    pub pending_disconnect_dest: Option<VPath>,
    /// El fichero que `pane.edit-new` mandó crear y el editor abrirá CUANDO
    /// exista (#290), con el id de la task que lo está creando.
    ///
    /// Abrirlo al encolar sería abrir algo que todavía no está en el disco —y
    /// que puede no llegar a estarlo: si la política deniega la creación, el
    /// editor lo crearía él, que es justo lo que esta tecla dejó de hacer—.
    /// El id es lo que distingue ESTA creación de cualquier otra task que
    /// termine mientras tanto.
    pub pending_edit_open: Option<(norte_proto::TaskId, VPath)>,
    /// Reinterpretación de nombres (#57) del lado ORIGEN, congelada junto con
    /// [`Self::pending_sync`] y no cuando el run loop abre el panel: entre una
    /// cosa y la otra el lector puede haber pulsado `Alt+E`, y un plan que se
    /// pintase con otra reinterpretación de la que se pidió enseñaría
    /// `????.txt` donde el origen tenía un nombre CP1251.
    pub pending_sync_encoding: (
        Option<norte_encoding::NameEncoding>,
        Option<norte_encoding::NameEncoding>,
    ),
    /// Este backend registra sus mutaciones en un journal y por tanto puede
    /// sincronizar (`--daemon`).
    ///
    /// Lo fija el arranque, una vez, porque el `Backend` no cambia de brazo en
    /// vida del proceso. Es lo que alimenta
    /// [`norte_frontend::availability::Facts::journalled`]: sin él la hoja de
    /// referencia ofrecería `Ctrl+Y` y el core lo rechazaría en cerrado —una
    /// tecla muerta documentada, que es lo que #159 acaba de costar una vez.
    pub backend_journalled: bool,
    /// Openers declarativos fusionados (#28): clonados en arranque y en cada
    /// hot-reload OK. Fuente de `pane.open` (F4). Vacío = sin openers.
    pub openers: norte_frontend::openers::OpenersConfig,
    /// El editor de `[ui] editor`, si la configuración nombra uno.
    ///
    /// `None` = el de siempre: `$VISUAL`, `$EDITOR`, y el fallback POSIX. Se
    /// copia aquí al arrancar y en cada recarga, igual que [`Self::openers`]:
    /// un gesto no vuelve a leer configuración del disco.
    pub editor: Option<EditorSpec>,
    /// El comparador de `[ui] diff` (#312), si lo hay.
    ///
    /// `None` = `diff -u`, que POSIX garantiza. Misma forma que
    /// [`Self::editor`] —un argv plantilla y si abre ventana— porque es el
    /// mismo trato: norte elige el operando, el programa elige el formato.
    pub diff: Option<EditorSpec>,
    /// Comando externo resuelto por `pane.open` y pendiente de lanzar (#28).
    /// `dispatch` lo fija tras validar; el run loop —dueño de la terminal—
    /// lo ejecuta.
    pub pending_open: Option<PendingOpen>,
    /// Suspensión resuelta por el despacho y pendiente de ejecutar (#135):
    /// `app.terminal`, `app.toggle-panels` y el Enter de
    /// [`Modal::CommandLine`]. Mismo reparto que [`Self::pending_open`], y
    /// drenado en UN solo sitio del run loop (arriba del todo de la vuelta,
    /// antes del draw) para que ningún `continue` de los que responde teclas
    /// pueda dejarla encallada.
    pub pending_shell: Option<PendingShell>,
    /// `app.toggle-panels` pidió el SUBSHELL (#142).
    ///
    /// Mismo reparto que [`Self::pending_shell`] y por lo mismo: el dueño de
    /// la terminal —y del shell de larga vida— es el run loop, no el despacho.
    /// Va aparte porque no es una suspensión: no se lanza nada, se le cede la
    /// terminal a un proceso que YA existe y que sigue vivo al volver.
    pub pending_subshell: bool,
    /// El acorde que RECUPERA los paneles del subshell, PRECOMPUTADO del
    /// keymap efectivo — mismo criterio que [`Self::dialog_hints`] y
    /// `palette_rows`, y por lo mismo: el efectivo se muda al `Resolver`
    /// compartido, así que lo que se derive de él se saca antes.
    ///
    /// `None` = el preset no ata `app.toggle-panels` a un acorde suelto, y
    /// entonces no se cede la terminal: ver
    /// [`norte_frontend::subshell::detach_chord`].
    pub subshell_chord: Option<norte_frontend::keymap::Chord>,
    /// Bytes que hay que escribirle al EMULADOR de terminal, si los hay.
    ///
    /// Mismo reparto que [`Self::pending_shell`]: `dispatch` decide QUÉ y el
    /// bucle —dueño de la salida— lo escribe. Hoy solo lo usa OSC 52, que es
    /// la única forma de copiar al portapapeles por SSH: quien recibe la
    /// secuencia es el terminal que el humano mira, no la máquina donde
    /// corre norte (#286).
    pub pending_osc52: Option<Vec<u8>>,
    /// Hints de pie de página de los overlays de diálogo (H1 T3, #24),
    /// PRECOMPUTADOS del efectivo `dialog` vigente — igual que `help_lines`
    /// en `main.rs`, se reconstruyen en el arranque y en cada hot-reload OK
    /// (`main::build_keymaps` + `DialogHints::build`), ANTES de que el
    /// efectivo se mueva al `Resolver` compartido. `ui::draw_*` los lee en
    /// vez de una clave Fluent estática.
    pub dialog_hints: crate::hints::DialogHints,
    /// Versión y revisión del binario (`norte_frontend::version::VERSION_LINE`),
    /// pintadas en el marco de la ayuda. Vacía = no se pinta: es lo que
    /// reciben los tests, cuyos snapshots no pueden depender del commit.
    pub version_line: &'static str,
    /// Resolver of the help's live marks (H3b): rebuilt with the effective
    /// keymaps on every hot reload, exactly like `dialog_hints` and
    /// `help_lines` — a rebind must change the prose, and it does because the
    /// page is drawn through this.
    pub help_chords: std::sync::Arc<crate::help::TuiChords>,
    /// Command palette abierta (`Ctrl+P`/vim `:`, H1 T4): `None` = cerrada.
    pub palette: Option<Palette>,
    /// Filas de la palette PRECOMPUTADAS del keymap vigente
    /// ([`crate::palette::build_rows`]) — igual criterio que `help_lines`/
    /// `dialog_hints`: se reconstruyen en el arranque y en cada hot-reload
    /// OK, ANTES de que los efectivos se muevan al `Resolver`. Abrir la
    /// palette (`dispatch`, brazo `app.palette`) solo clona esta snapshot.
    pub palette_rows: Vec<crate::palette::Row>,
    /// Estado del ratón (captura aparte, que es de la terminal): la
    /// geometría PINTADA del último frame, el gesto armado y el último
    /// click. La geometría la devuelve el run loop tras cada `draw`
    /// (#124): sin ella no se resuelve ningún click.
    pub mouse: crate::mouse::MouseState,
    /// Overlay de ajustes abierto (`app.settings`, S3): `None` = cerrado.
    /// Sus filas se reconstruyen del `cfg` VIGENTE en cada hot-reload OK
    /// (`main::reload_config`, `Settings::refresh`) — a diferencia de
    /// `palette`/`help`, que se CIERRAN, este overlay se queda abierto y se
    /// refresca en su sitio (ver el doc de `Settings::refresh`).
    pub settings: Option<Settings>,
    /// Shortcut editor open (K3c, `Ctrl+K` from the settings overlay):
    /// `None` = closed.
    ///
    /// Sits IN FRONT of `settings`, which stays open behind it — the editor is
    /// a screen of Settings, not a replacement for it, and closing it returns
    /// the reader where they were.
    ///
    /// Like `settings` it is REFRESHED and not closed on a hot reload
    /// (`main::reload_config`), because that reload is usually its own write
    /// coming back through the watcher: an editor that closed on the write it
    /// just made would be unusable for a second rebind. Its capture, unlike a
    /// settings edit buffer, does NOT survive the refresh — see
    /// `ShortcutsState::refresh`, whose verdict belongs to the map that was
    /// just replaced.
    pub shortcuts: Option<Shortcuts>,
    /// How many times the two panes have been exchanged (`pane.swap`).
    ///
    /// It exists because nothing else in the model records that a swap
    /// happened: everything indexed by pane TRAVELS with the pane, so a swap
    /// merely exchanges two values and anything comparing them per side sees
    /// nothing move. Read through [`Self::swap_seq`] by the mouse, whose
    /// armed gesture carries pane indices that the swap has just
    /// re-attributed to the other side's content.
    ///
    /// Only ever compared for EQUALITY, so it wraps rather than saturating —
    /// saturating would eventually stop changing, which is the one thing it
    /// must never do.
    swap_seq: u64,
    /// `--pick` (S2): true for the lifetime of the process once the flag was
    /// passed. Read by the run loop's Enter/Ctrl+Enter override (design §B)
    /// and by `Command::AppPickAccept`'s dispatch arm, which is a no-op
    /// without it — the command exists in the catalogue unconditionally
    /// (help, palette, rebind checks), but only ever FIRES under `--pick`.
    /// Set once in `main`, right after construction; never toggled at
    /// runtime.
    pub pick: bool,
    /// The picker's answer, written by `Command::AppPickAccept` and read by
    /// `main` after the run loop returns (`app.quit` is set alongside it, so
    /// this is always read exactly once). `None` after a normal quit means
    /// the pick was CANCELLED, not that nothing happened — `main` tells the
    /// two apart with `Self::pick`, per the exit-code table in the design
    /// (0 accepted, 1 cancelled, 2 error).
    pub picked: Option<Vec<VPath>>,
}

// `PendingWrite`/`SettingsEditError`/`Settings`/`cycle` (S3 overlay editor)
// hoisted to `norte_frontend::settings` in S4 (GUI settings view): the code
// had ZERO TUI-specific coupling (no ratatui/crossterm, pure state +
// `norte_frontend::nav::fold`) — re-exported here under their historical
// names so the rest of this crate (and integration tests referencing
// `norte_tui::app::{Settings, PendingWrite, SettingsEditError}`) keep
// resolving unchanged. See `norte_frontend::settings` module doc.
pub use norte_frontend::settings::{PendingWrite, SettingsEditError, SettingsState as Settings};

// K3c: the shortcut editor's state machine is shared with the GUI for the same
// reason as the settings one — it is pure (rows, a filter, a cursor and a
// capture with its verdict), and two frontends deciding separately what a
// collision is would be two answers to one question.
pub use norte_frontend::shortcuts::ShortcutsState as Shortcuts;

impl App {
    /// App con foco en el pane izquierdo.
    #[must_use]
    pub fn new(left: Pane, right: Pane) -> Self {
        Self {
            panes: crate::panel::PaneSlots::new(left, right),
            layout: crate::panel::orthodox(),
            kinds: norte_frontend::layout::KindRegistry::builtin(),
            roles: norte_frontend::layout::Roles::con_active(crate::panel::SLOT_LEFT),
            key_owner: KeyOwner::Panes,
            log_panel: norte_frontend::logpanel::LogPanel::default(),
            log_filter_input: None,
            log_ring: None,
            log_remote: crate::logview::RegistroRemoto::default(),
            busy: None,
            menu: None,
            menu_ultimo: 0,
            // Los cuatro primeros son los del preset `orthodox`.
            next_slot: 5,
            render_now_ms: None,
            attr_catalogs: std::collections::HashMap::new(),
            caps: std::collections::HashMap::new(),
            caps_order: std::collections::VecDeque::new(),
            columns: norte_frontend::columns::ColumnsSettings::default(),
            focus: 0,
            quit: false,
            pending: String::new(),
            which_key: None,
            modal: None,
            message: None,
            session: SessionUi::default(),
            board: crate::tasks::TaskBoard::default(),
            viewer: None,
            help: None,
            pending_collisions: std::collections::VecDeque::new(),
            pending_approvals: std::collections::VecDeque::new(),
            theme: crate::theme::TuiTheme::default(),
            theme_picker: None,
            layout_picker: None,
            profile_picker: None,
            menu_bar: true,
            panel_bar: true,
            pending_panel_command: None,
            // Apagada hasta que el arranque diga: un `App` de test no lee
            // configuración, y una fila que aparece sola cambiaría los
            // índices de ochenta tests que no van de esto.
            parent_row: false,
            active_profile: None,
            pending_profile: None,
            places_wants_drives: false,
            connections_picker: None,
            columns_picker: None,
            extensions: None,
            lua_pending_trust: None,
            lua_status: None,
            degraded: norte_frontend::banners::DegradedSet::default(),
            no_journal: None,
            history: crate::panel::Histories::new(),
            hotlist: Vec::new(),
            nav_popup: None,
            search_dialog: None,
            compare: None,
            compare_size_hints: std::collections::HashMap::new(),
            compare_size_probed: std::collections::HashSet::new(),
            compare_generation: 0,
            pending_compare: None,
            pending_checksum: None,
            pending_dest_check: None,
            sync: None,
            pending_sync: None,
            pending_sync_apply: None,
            pending_disconnect_dest: None,
            pending_edit_open: None,
            pending_sync_encoding: (None, None),
            // Fail-CLOSED: el `App` de un test no tiene backend, y ofrecer
            // sincronizar por defecto convertiría cada test en un permiso.
            // `main` lo enciende cuando el backend es remoto.
            backend_journalled: false,
            openers: norte_frontend::openers::OpenersConfig::empty(),
            editor: None,
            diff: None,
            pending_open: None,
            pending_shell: None,
            pending_subshell: false,
            subshell_chord: None,
            pending_osc52: None,
            dialog_hints: crate::hints::DialogHints::default(),
            version_line: "",
            help_chords: default_help_chords(),
            palette: None,
            palette_rows: Vec::new(),
            mouse: crate::mouse::MouseState::default(),
            settings: None,
            shortcuts: None,
            swap_seq: 0,
            pick: false,
            picked: None,
        }
    }

    /// Un listado nuevo, ya con la configuración de esta sesión puesta.
    ///
    /// Los huecos nacen en cuatro sitios —una pestaña, una partición, un
    /// hueco de una disposición, una sesión restaurada— y el que se olvidara
    /// de la fila `..` sería una mitad de la pantalla comportándose distinto
    /// de la otra.
    #[must_use]
    pub fn nuevo_pane(&self, dir: VPath, entradas: Vec<norte_proto::Entry>) -> Pane {
        let mut pane = Pane::new(dir, entradas);
        pane.set_parent_row(self.parent_row);
        pane
    }

    /// Un pane nuevo con el listado del pane `i`: lo que necesitan partir un
    /// panel y abrir una pestaña.
    ///
    /// Hereda las entradas ya listadas en vez de pedir un listado —es el MISMO
    /// directorio, así que el panel nuevo aparece lleno en el acto y no
    /// parpadea vacío mientras alguien vuelve a leer lo mismo—, y hereda las de
    /// VERDAD: [`norte_frontend::PaneState::real_entries`] deja fuera la fila
    /// `..`, que el pane nuevo se pone él. Copiando `entries()` la heredada se
    /// quedaba de entrada normal en medio del listado, con el nombre del
    /// directorio padre y marcable — una más por cada partición.
    ///
    /// UNA puerta para los dos, y no tres líneas repetidas en cada uno: el
    /// tercero que apareciera las repetiría mal.
    #[must_use]
    pub fn fork_pane(&self, i: usize) -> Pane {
        let p = &self.panes[i];
        self.nuevo_pane(p.dir().clone(), p.real_entries().to_vec())
    }

    /// Mete en `id` un listado que nació FUERA de [`Self::nuevo_pane`] y le
    /// pone la configuración de esta sesión.
    ///
    /// Los tres que nacen fuera son de la SESIÓN: el que `apply_session`
    /// levanta sobre la ruta guardada, el que el arranque lista para él, y el
    /// que `pin_start_dir` pone cuando la línea de órdenes nombra un
    /// directorio. Los tres reponían el orden y los ocultos y ninguno la fila
    /// `..`, así que `[ui] parent_entry = true` se apagaba solo a partir de
    /// la primera sesión guardada — y el lector lo veía como que el TUI no
    /// tiene fila de subir y la ventana sí.
    ///
    /// Estampa lo de la CONFIG (la fila `..`) y repone lo de la SESIÓN (el
    /// orden y los ocultos), en ese orden y en un solo sitio.
    ///
    /// Los tres llamantes lo hacían por su cuenta y en órdenes distintos, que
    /// es cómo uno se dejó la fila; el campo que se añada mañana se dejarían
    /// dos. `None` en `sort`/`hidden` es «este llamante no tiene nada que
    /// reponer», no «pon el de fábrica».
    pub fn adoptar_pane(
        &mut self,
        id: norte_frontend::layout::SlotId,
        mut pane: Pane,
        sort: Option<norte_frontend::SortSpec>,
        hidden: Option<bool>,
    ) {
        pane.set_parent_row(self.parent_row);
        if let Some(s) = sort {
            pane.set_sort(s);
        }
        if let Some(h) = hidden {
            pane.set_show_hidden(h);
        }
        self.panes.insert_browser(id, pane);
    }

    /// Enciende o apaga la fila `..` en TODOS los panes (`[ui] parent_entry`).
    ///
    /// En todos y no solo en los visibles: un pane detrás de una pestaña
    /// vuelve a pintarse tal y como se dejó, y una mitad de la pantalla con la
    /// fila y otra sin ella sería la misma configuración diciendo dos cosas.
    pub fn set_parent_row(&mut self, on: bool) {
        self.parent_row = on;
        for pane in self.panes.browsers_mut() {
            pane.set_parent_row(on);
        }
    }

    /// Cierra la barra de menús, apuntando por dónde iba.
    ///
    /// UNA puerta, y no por gusto: el menú se cierra desde cinco sitios —la
    /// tecla, `Esc`, elegir una entrada, pulsar fuera y pulsar en la barra— y
    /// el que se olvidara de apuntar sería el que hace que la próxima
    /// apertura empiece por el primero sin motivo aparente.
    pub fn close_menu(&mut self) {
        if let Some(m) = &self.menu {
            self.menu_ultimo = m.menu();
        }
        self.menu = None;
    }

    /// Abre la barra de menús, o la cierra si ya estaba: la misma tecla hace
    /// las dos cosas, como el resto de los overlays.
    ///
    /// Se reabre por donde iba —empezar siempre por el primero obliga a
    /// recorrer la barra entera en cada gesto— y por eso pasa por
    /// [`Self::close_menu`], que es quien lo apunta.
    pub fn toggle_menu(&mut self) {
        if self.menu.is_some() {
            self.close_menu();
        } else {
            self.menu = Some(norte_frontend::menu::MenuState::reopen_at(self.menu_ultimo));
        }
    }

    /// Lo que un panel lateral con teclado NO decide: el cromo de la
    /// aplicación. `true` = atendido aquí y el panel no tiene que mirarlo.
    ///
    /// La barra de menús no es de los listados, es de la aplicación entera, y
    /// estando dentro del árbol, del sidebar o del panel de procesos su tecla
    /// se moría: no figuraba en el allowlist de ninguno, así que el panel se la
    /// comía y la pantalla se quedaba igual. Es la misma lección que ya trajo
    /// `layout.places` a esos allowlists, y por eso vive en UN sitio: tres
    /// paneles con su propia copia son tres sitios donde olvidarse del cuarto.
    pub fn panel_chrome_command(&mut self, cmd: &str) -> bool {
        if cmd == "app.menu" {
            self.toggle_menu();
            return true;
        }
        false
    }

    /// El reloj de la interfaz, en milisegundos de época.
    ///
    /// UNA sola fuente: [`Self::render_now_ms`] cuando está fijada (los tests
    /// la fijan para que un snapshot no dependa de la hora), y el reloj real
    /// si no. Lo usan las celdas de tiempo relativo y la caducidad de las
    /// filas terminales del tablero de tasks — dos sitios que tienen que
    /// coincidir, porque un test que fija el reloj para el primero y no para
    /// el segundo tendría un panel que cambia solo.
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        self.render_now_ms.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        })
    }

    /// Publishes a resolver's in-flight state to the screen: the status-bar
    /// segment ([`Self::pending`]) and the which-key panel
    /// ([`Self::which_key`]), which must never disagree about it.
    ///
    /// The ONE place that decides whether the panel is open, and it decides it
    /// from the resolver rather than from the [`Resolution`] variant that got
    /// us here:
    ///
    /// - a pending SEQUENCE opens it — immediately, with no delay of any kind.
    ///   ADR 0006's resolution is timing-free and a panel that waited 400 ms
    ///   would put timing back into what the reader sees;
    /// - a bare COUNT does not. Its pending sequence is empty, so there are no
    ///   rows: the continuation of a count is any key at all, and the panel
    ///   would be the whole keymap. K2a already paints the count in the bar,
    ///   and a count typed BEHIND a prefix still shows — in the panel's title.
    ///
    /// [`Resolution`]: norte_frontend::keymap::Resolution
    pub fn show_pending(&mut self, resolver: &norte_frontend::keymap::Resolver, lang: Lang) {
        self.pending = crate::keymap::pending_display(resolver);
        self.which_key = (!resolver.pending().is_empty()).then(|| {
            norte_frontend::whichkey::WhichKeyRows::build(
                resolver.effective(),
                resolver.pending(),
                resolver.count(),
                lang,
            )
        });
    }

    /// Nothing is pending any more: the bar segment and the panel go together.
    ///
    /// Called by every arm that ENDS a pending state — a command ran, the key
    /// was unavailable, `Esc` reset it, a key the frontend does not model
    /// arrived — and by the hot reload, which replaces the resolvers whole
    /// (ADR 0007): a panel built from the old effective map would survive its
    /// keymap and list keys the new one does not have.
    pub fn clear_pending(&mut self) {
        self.pending.clear();
        self.which_key = None;
    }

    /// The reader is no longer typing a sequence, and it was not a key of that
    /// sequence that ended it: a gesture (a double click), or a key SWALLOWED
    /// before the resolver ever saw it (the `Esc` that cancels a Lua command,
    /// an AI rename or a semantic search in flight).
    ///
    /// Resets the resolver too, which [`Self::clear_pending`] alone does not:
    /// the pending chords live in the resolver, and clearing only the screen
    /// would leave `g` armed inside it, so the NEXT key would complete a
    /// sequence the reader had already abandoned. It is the same treatment the
    /// run loop gives a key the frontend does not model.
    ///
    /// The visible symptom this exists for: with a Lua command running and `g`
    /// pending, `Esc` is consumed by the cancellation and the panel used to
    /// stay on screen — the one key that means "never mind" appearing to do
    /// nothing at all.
    pub fn abandon_pending(&mut self, resolver: &mut norte_frontend::keymap::Resolver) {
        resolver.reset();
        self.clear_pending();
    }

    /// Lays the open help overlay out for a body of `width`×`height` cells,
    /// through the resolver and theme in force. No-op with the overlay closed.
    ///
    /// The split-borrow wrapper exists because [`HelpView::refresh`] needs
    /// three fields of `App` at once ([`Self::help`] mutably,
    /// [`Self::help_chords`] and [`Self::theme`] shared) and a caller outside
    /// this module cannot name them disjointly. The run loop calls it with
    /// [`crate::ui::help_body_size`] of the frame it is about to paint, so
    /// the geometry the model clamps against is the geometry on screen.
    pub fn refresh_help(&mut self, width: usize, height: usize) {
        if let Some(help) = &mut self.help {
            help.refresh(&self.help_chords, width, height, &self.theme);
        }
    }

    /// Abre el diálogo de búsqueda viva (`Alt+F7`, liveSearch T6) vacío. La
    /// raíz del walk se resuelve al lanzar (cwd del pane con foco).
    pub fn open_search_dialog(&mut self) {
        self.search_dialog = Some(SearchDialog::new());
    }
}

/// Hits semánticos visibles a la vez en [`Modal::SemanticHits`] (ventana de
/// scroll) — la constante vive en `norte-frontend` (compartida con la GUI,
/// mismo criterio que [`AI_RENAME_PAIR_LIMIT`]); re-export para el render
/// (`ui`), el alto del modal y el clamp de [`App::semantic_cursor`].
pub use norte_frontend::SEMANTIC_HIT_LIMIT;

/// Parejas del plan IA visibles a la vez en [`Modal::AiRenamePlan`] (ventana
/// de scroll, audit MAJOR-3) — la constante vive en `norte-frontend`
/// (compartida con la GUI, quality review 78eb243 MAJOR-1); re-export para
/// el render (`ui`), el alto del modal y el clamp de [`App::ai_plan_scroll`].
pub use norte_frontend::AI_RENAME_PAIR_LIMIT;

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    /// Diálogo de búsqueda (liveSearch T6): Tab alterna el campo activo y los
    /// imprimibles/backspace caen en el campo con foco.
    #[test]
    fn search_dialog_tab_y_edicion_por_campo() {
        let mut d = SearchDialog::new();
        assert_eq!(d.field, SearchField::Name);
        d.push_char('*');
        d.push_char('x');
        assert_eq!(d.name, "*x");
        assert_eq!(d.content, "");
        d.toggle_field();
        assert_eq!(d.field, SearchField::Content);
        d.push_char('a');
        d.push_char('b');
        d.backspace();
        assert_eq!(d.content, "a");
        assert_eq!(d.name, "*x", "backspace solo tocó el campo activo");
        d.toggle_field();
        assert_eq!(d.field, SearchField::Name);
    }

    /// Los toggles (F2 regex / F3 case) alternan sus flags de forma
    /// independiente.
    #[test]
    fn search_dialog_toggles_regex_y_case() {
        let mut d = SearchDialog::new();
        assert!(!d.regex && !d.case);
        d.toggle_regex();
        assert!(d.regex && !d.case);
        d.toggle_case();
        assert!(d.regex && d.case);
        d.toggle_regex();
        assert!(!d.regex && d.case);
    }

    /// Validación del criterio: sin ningún campo no hay búsqueda; basta con
    /// uno (nombre O contenido) para que la haya.
    #[test]
    fn search_dialog_criterio_no_vacio() {
        let mut d = SearchDialog::new();
        assert!(!d.has_criteria(), "ambos vacíos: no lanza");
        d.push_char('*');
        assert!(d.has_criteria(), "solo nombre basta");
        let mut d = SearchDialog::new();
        d.toggle_field();
        d.push_char('a');
        assert!(d.has_criteria(), "solo contenido basta");
    }

    /// `begin_search` marca el pane como virtual, vacía las entries y resetea
    /// el estado a `Running`; `extend_listing` alimenta los hits SIN apagar el
    /// modo virtual (los hits siguen siendo de una búsqueda).
    #[test]
    fn begin_search_marca_virtual_y_extend_conserva() {
        let mut p = pane_con(&["basura"]);
        p.begin_search(root());
        assert!(p.virtual_search);
        assert_eq!(p.search_state, SearchState::Running);
        assert!(p.entries().is_empty(), "los hits empiezan vacíos");
        p.extend_listing(vec![file("hit1"), file("hit2")]);
        assert!(p.virtual_search, "extend no apaga el modo virtual");
        assert_eq!(names(&p), vec!["hit1", "hit2"]);
    }

    /// Un listado NORMAL (cd/refresh) apaga el modo virtual de búsqueda.
    #[test]
    fn listados_normales_apagan_el_modo_virtual() {
        let mut p = pane_con(&[]);
        p.begin_search(root());
        assert!(p.virtual_search);
        p.begin_listing(root(), vec![file("a")], false, None);
        assert!(!p.virtual_search, "begin_listing apaga virtual");

        p.begin_search(root());
        p.set_listing(root(), vec![file("a")]);
        assert!(!p.virtual_search, "set_listing apaga virtual");

        p.begin_search(root());
        p.refresh_listing(vec![file("a")]);
        assert!(!p.virtual_search, "refresh_listing apaga virtual");
    }
}
